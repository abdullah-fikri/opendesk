//! Remote screens in OpenGL textures of our own (V2).
//!
//! A frame shown as `Image::from_rgba8` has no texture-cache key in Slint's femtovg renderer, so the whole picture
//! (8 MB at 1080p) is uploaded again on every redraw of the window, also when only a cursor blinked or the mouse
//! moved over the toolbar. Here every session gets one texture: a new frame uploads only the rows the decoder
//! changed (`glTexSubImage2D`), and the session shows it as a borrowed texture that costs nothing to draw again.
//!
//! Everything runs on the UI thread: frames are stored by the presenter's closures and uploaded from the
//! rendering notifier right before Slint draws (the GL context is current there). Without OpenGL (software
//! renderer), before the first render, or with `OPENDESK_GL_FRAMES=0`, sessions keep `Image::from_rgba8`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::num::NonZeroU32;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::infra::logger;

/// Rows `start..end` that changed, or None for the whole frame.
pub type Rows = Option<(usize, usize)>;

/// The smallest band holding both; None (whole frame) wins.
pub fn union(a: Rows, b: Rows) -> Rows {
    match (a, b) {
        (Some((a0, a1)), Some((b0, b1))) => Some((a0.min(b0), a1.max(b1))),
        _ => None,
    }
}

const TEXTURE_2D: u32 = 0x0DE1;
const TEXTURE_BINDING_2D: u32 = 0x8069;
const RGBA: u32 = 0x1908;
const UNSIGNED_BYTE: u32 = 0x1401;
const TEXTURE_MIN_FILTER: u32 = 0x2801;
const TEXTURE_MAG_FILTER: u32 = 0x2800;
const LINEAR: i32 = 0x2601;
const TEXTURE_WRAP_S: u32 = 0x2802;
const TEXTURE_WRAP_T: u32 = 0x2803;
const CLAMP_TO_EDGE: i32 = 0x812F;
const UNPACK_ALIGNMENT: u32 = 0x0CF5;
const UNPACK_ROW_LENGTH: u32 = 0x0CF2;
const UNPACK_SKIP_ROWS: u32 = 0x0CF3;
const UNPACK_SKIP_PIXELS: u32 = 0x0CF4;
const PIXEL_UNPACK_BUFFER_BINDING: u32 = 0x88EF;
const PIXEL_UNPACK_BUFFER: u32 = 0x88EC;

/// The few OpenGL entry points this needs, loaded from the renderer's context.
#[allow(clippy::type_complexity)]
struct Gl {
    gen_textures: unsafe extern "system" fn(i32, *mut u32),
    delete_textures: unsafe extern "system" fn(i32, *const u32),
    bind_texture: unsafe extern "system" fn(u32, u32),
    tex_parameteri: unsafe extern "system" fn(u32, u32, i32),
    tex_image_2d: unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
    tex_sub_image_2d: unsafe extern "system" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
    get_integerv: unsafe extern "system" fn(u32, *mut i32),
    pixel_storei: unsafe extern "system" fn(u32, i32),
    get_error: unsafe extern "system" fn() -> u32,
    /// Absent before OpenGL 2.1 / ES 3.0; then no pixel buffer can be bound either.
    bind_buffer: Option<unsafe extern "system" fn(u32, u32)>,
}

/// A loader pointer as the function type it names.
///
/// # Safety
/// `p` must point to a function of type `F`.
unsafe fn cast<F: Copy>(p: *const c_void) -> F {
    unsafe { std::mem::transmute_copy(&p) }
}

impl Gl {
    fn load(get: &dyn Fn(&CStr) -> *const c_void) -> Option<Self> {
        let f = |name: &CStr| {
            let p = get(name);
            (!p.is_null()).then_some(p)
        };
        // SAFETY: each pointer comes from the context's loader for exactly the named function, whose C
        // signature the field types repeat.
        unsafe {
            Some(Self {
                gen_textures: cast(f(c"glGenTextures")?),
                delete_textures: cast(f(c"glDeleteTextures")?),
                bind_texture: cast(f(c"glBindTexture")?),
                tex_parameteri: cast(f(c"glTexParameteri")?),
                tex_image_2d: cast(f(c"glTexImage2D")?),
                tex_sub_image_2d: cast(f(c"glTexSubImage2D")?),
                get_integerv: cast(f(c"glGetIntegerv")?),
                pixel_storei: cast(f(c"glPixelStorei")?),
                get_error: cast(f(c"glGetError")?),
                bind_buffer: f(c"glBindBuffer").map(|p| cast(p)),
            })
        }
    }

    /// Reads an integer state; None when this context does not know `name` (OpenGL ES 2).
    fn integer(&self, name: u32) -> Option<i32> {
        let mut v = 0;
        unsafe {
            (self.get_integerv)(name, &mut v);
            ((self.get_error)() == 0).then_some(v)
        }
    }
}

/// One session's texture.
struct Tex {
    id: NonZeroU32,
    w: u32,
    h: u32,
    image: Image,
    /// The picture in the texture, shown as a plain image if the context goes away.
    last: SharedPixelBuffer<Rgba8Pixel>,
}

/// A frame waiting for the next render.
struct Pending {
    pixels: SharedPixelBuffer<Rgba8Pixel>,
    rows: Rows,
}

#[derive(Default)]
struct State {
    gl: Option<Gl>,
    disabled: bool,
    textures: HashMap<String, Tex>,
    pending: HashMap<String, Pending>,
    /// Textures no session shows any more, deleted once the frame that stopped showing them was drawn.
    retired: Vec<NonZeroU32>,
    /// Sessions whose new texture is ready, to be put into their row (outside rendering).
    ready: Vec<(String, Image)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        disabled: std::env::var("OPENDESK_GL_FRAMES").is_ok_and(|v| v == "0"),
        ..State::default()
    });
}

/// Hands session `id`'s new frame (`rows` changed since its previous one) to its texture. Returns the image the
/// session shows: its texture, or None to show `pixels` as `Image::from_rgba8` (no OpenGL, or the texture is
/// made at the next render; [`take_ready`] then hands it over). UI thread only.
pub fn offer(id: &str, pixels: &SharedPixelBuffer<Rgba8Pixel>, rows: Rows) -> Option<Image> {
    STATE.with_borrow_mut(|s| {
        if s.gl.is_none() || s.disabled {
            return None;
        }
        let rows = match s.pending.remove(id) {
            Some(older) => union(older.rows, rows),
            None => rows,
        };
        s.pending.insert(id.to_string(), Pending { pixels: pixels.clone(), rows });
        s.textures.get(id).filter(|t| (t.w, t.h) == (pixels.width(), pixels.height())).map(|t| t.image.clone())
    })
}

/// Sessions whose texture became ready at the last render, with the image to show from now on.
pub fn take_ready() -> Vec<(String, Image)> {
    STATE.with_borrow_mut(|s| std::mem::take(&mut s.ready))
}

/// Session `id` closed: its texture goes after the next render.
pub fn forget(id: &str) {
    STATE.with_borrow_mut(|s| {
        s.pending.remove(id);
        if let Some(t) = s.textures.remove(id) {
            s.retired.push(t.id);
        }
    });
}

/// Rendering notifier hook: loads OpenGL at setup, uploads pending frames before rendering, drops everything at
/// teardown. Returns true when a texture became ready (the caller then schedules [`take_ready`]).
pub fn on_rendering(state: &slint::RenderingState, api: &slint::GraphicsAPI<'_>) -> bool {
    STATE.with_borrow_mut(|s| match state {
        slint::RenderingState::RenderingSetup => {
            if let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api
                && !s.disabled
            {
                s.gl = Gl::load(*get_proc_address);
                logger::info(
                    "ui",
                    if s.gl.is_some() {
                        "Remote screens use their own OpenGL textures (changed rows only)."
                    } else {
                        "OpenGL entry points missing; remote screens use full image uploads."
                    },
                );
            }
            false
        }
        slint::RenderingState::BeforeRendering => s.upload(),
        slint::RenderingState::AfterRendering => {
            s.free_retired();
            false
        }
        slint::RenderingState::RenderingTeardown => {
            // The context and every texture in it are gone: sessions show their last picture as a plain image
            // until a new setup.
            let ready = s.textures.drain().map(|(id, t)| (id, Image::from_rgba8(t.last))).collect();
            *s = State { disabled: s.disabled, ready, ..State::default() };
            true
        }
        _ => false,
    })
}

impl State {
    fn upload(&mut self) -> bool {
        let Some(gl) = &self.gl else { return false };
        if self.pending.is_empty() {
            return false;
        }
        let mut made = false;
        unsafe {
            // Leave the state femtovg relies on as it was.
            let bound = gl.integer(TEXTURE_BINDING_2D).unwrap_or(0);
            let unpack =
                [UNPACK_ALIGNMENT, UNPACK_ROW_LENGTH, UNPACK_SKIP_ROWS, UNPACK_SKIP_PIXELS].map(|n| (n, gl.integer(n)));
            let pbo = gl.bind_buffer.and_then(|b| Some((b, gl.integer(PIXEL_UNPACK_BUFFER_BINDING)?)));
            for (n, v) in unpack {
                if v.is_some() {
                    (gl.pixel_storei)(n, if n == UNPACK_ALIGNMENT { 4 } else { 0 });
                }
            }
            if let Some((bind, buf)) = pbo
                && buf != 0
            {
                bind(PIXEL_UNPACK_BUFFER, 0);
            }
            for (id, p) in self.pending.drain() {
                let (w, h) = (p.pixels.width(), p.pixels.height());
                let bytes = p.pixels.as_bytes();
                let fresh = self.textures.get(&id).is_none_or(|t| (t.w, t.h) != (w, h));
                if fresh {
                    let mut tex = 0;
                    (gl.gen_textures)(1, &mut tex);
                    let Some(tex) = NonZeroU32::new(tex) else { continue };
                    (gl.bind_texture)(TEXTURE_2D, tex.get());
                    for (k, v) in [
                        (TEXTURE_MIN_FILTER, LINEAR),
                        (TEXTURE_MAG_FILTER, LINEAR),
                        (TEXTURE_WRAP_S, CLAMP_TO_EDGE),
                        (TEXTURE_WRAP_T, CLAMP_TO_EDGE),
                    ] {
                        (gl.tex_parameteri)(TEXTURE_2D, k, v);
                    }
                    let data = bytes.as_ptr().cast();
                    (gl.tex_image_2d)(TEXTURE_2D, 0, RGBA as i32, w as i32, h as i32, 0, RGBA, UNSIGNED_BYTE, data);
                    let image = slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(tex, [w, h].into()).build();
                    let t = Tex { id: tex, w, h, image: image.clone(), last: p.pixels.clone() };
                    if let Some(old) = self.textures.insert(id.clone(), t) {
                        self.retired.push(old.id);
                    }
                    self.ready.push((id, image));
                    made = true;
                    continue;
                }
                let Some(t) = self.textures.get_mut(&id) else { continue };
                t.last = p.pixels.clone();
                let (y0, y1) = p.rows.unwrap_or((0, h as usize));
                let (y0, y1) = (y0.min(h as usize), y1.min(h as usize));
                if y1 > y0 {
                    let row = w as usize * 4;
                    (gl.bind_texture)(TEXTURE_2D, t.id.get());
                    let data = bytes[y0 * row..].as_ptr().cast();
                    let (y, n) = (y0 as i32, (y1 - y0) as i32);
                    (gl.tex_sub_image_2d)(TEXTURE_2D, 0, 0, y, w as i32, n, RGBA, UNSIGNED_BYTE, data);
                }
            }
            (gl.bind_texture)(TEXTURE_2D, bound as u32);
            for (n, v) in unpack {
                if let Some(v) = v {
                    (gl.pixel_storei)(n, v);
                }
            }
            if let Some((bind, buf)) = pbo
                && buf != 0
            {
                bind(PIXEL_UNPACK_BUFFER, buf as u32);
            }
        }
        made
    }

    fn free_retired(&mut self) {
        let Some(gl) = &self.gl else { return };
        if self.retired.is_empty() || !self.ready.is_empty() {
            return; // a session still shows the old texture until its new one is in its row
        }
        let ids: Vec<u32> = self.retired.drain(..).map(NonZeroU32::get).collect();
        unsafe { (gl.delete_textures)(ids.len() as i32, ids.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_bands_merge_and_whole_frames_win() {
        assert_eq!(union(Some((10, 20)), Some((5, 12))), Some((5, 20)));
        assert_eq!(union(Some((10, 20)), None), None);
        assert_eq!(union(None, Some((1, 2))), None);
    }

    #[test]
    fn without_opengl_frames_stay_plain_images() {
        let px = SharedPixelBuffer::<Rgba8Pixel>::new(4, 2);
        assert!(offer("s1", &px, Some((0, 1))).is_none());
        assert!(take_ready().is_empty());
        forget("s1");
    }
}
