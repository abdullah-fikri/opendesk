//! On-GPU check of `src/ui/gl_frames.rs` (V2) in a real Slint/femtovg window: a picture goes into a texture of
//! our own, then only its top rows change, and the rendered window is read back (`glReadPixels`) to prove the
//! new rows show and the untouched ones stayed. Run: `cargo run --example gl_frames_check`

use std::cell::Cell;
use std::ffi::c_void;
use std::rc::Rc;

use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};

#[path = "../src/ui/gl_frames.rs"]
#[allow(dead_code)]
mod gl_frames;

mod infra {
    pub mod logger {
        pub fn info(_: &str, msg: &str) {
            println!("{msg}");
        }
    }
}

slint::slint! {
    export component Check inherits Window {
        width: 400px;
        height: 200px;
        in property <image> img;
        Image { source: img; width: 100%; height: 100%; image-fit: fill; }
    }
}

const W: u32 = 400;
const H: u32 = 200;
const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];

fn picture(top: [u8; 4], bottom: [u8; 4]) -> SharedPixelBuffer<Rgba8Pixel> {
    let mut px = SharedPixelBuffer::<Rgba8Pixel>::new(W, H);
    for (i, p) in px.make_mut_bytes().chunks_mut(4).enumerate() {
        p.copy_from_slice(if (i as u32 / W) < H / 2 { &top } else { &bottom });
    }
    px
}

type ReadPixels = unsafe extern "system" fn(i32, i32, i32, i32, u32, u32, *mut c_void);
type GetIntegerv = unsafe extern "system" fn(u32, *mut i32);

fn main() {
    let ui = Check::new().unwrap();
    // 0: plain image shown, 1: texture made, 2: top rows changed, 3: done.
    let step = Rc::new(Cell::new(0));
    let fns: Rc<Cell<Option<(ReadPixels, GetIntegerv)>>> = Rc::new(Cell::new(None));
    let weak = ui.as_weak();
    let (s, f) = (Rc::clone(&step), Rc::clone(&fns));
    ui.window()
        .set_rendering_notifier(move |state, api| {
            if let slint::RenderingState::RenderingSetup = state
                && let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api
            {
                let rp = get_proc_address(c"glReadPixels");
                let gi = get_proc_address(c"glGetIntegerv");
                // SAFETY: loader pointers for exactly these functions.
                f.set(Some(unsafe {
                    (
                        std::mem::transmute::<*const c_void, ReadPixels>(rp),
                        std::mem::transmute::<*const c_void, GetIntegerv>(gi),
                    )
                }));
            }
            if gl_frames::on_rendering(&state, api) {
                for (_, image) in gl_frames::take_ready() {
                    weak.unwrap().set_img(image);
                }
                s.set(1);
                let w = weak.clone();
                // Next: only the top half changes to green.
                let _ = slint::invoke_from_event_loop(move || {
                    let ui = w.unwrap();
                    let img = gl_frames::offer("s", &picture(GREEN, BLUE), Some((0, H as usize / 2)));
                    ui.set_img(img.expect("texture in use"));
                    ui.window().request_redraw();
                });
                return;
            }
            if let slint::RenderingState::AfterRendering = state
                && s.get() == 1
                && let Some((read, get)) = f.get()
            {
                let mut vp = [0i32; 4];
                unsafe { get(0x0BA2, vp.as_mut_ptr()) }; // GL_VIEWPORT
                let px = |fx: f32, fy_top: f32| {
                    let (x, y) = ((vp[2] as f32 * fx) as i32, (vp[3] as f32 * (1.0 - fy_top)) as i32);
                    let mut out = [0u8; 4];
                    unsafe { read(x, y, 1, 1, 0x1908, 0x1401, out.as_mut_ptr().cast()) };
                    out
                };
                let (top, bottom) = (px(0.5, 0.25), px(0.5, 0.75));
                if top == GREEN && bottom == BLUE {
                    println!("PASS: changed rows uploaded (top {top:?}), untouched rows kept (bottom {bottom:?})");
                    s.set(3);
                    let _ = slint::quit_event_loop();
                } else if top == RED {
                    // The changed rows are uploaded at the next render.
                    weak.unwrap().window().request_redraw();
                } else {
                    println!("FAIL: top {top:?} (want {GREEN:?}), bottom {bottom:?} (want {BLUE:?})");
                    s.set(3);
                    let _ = slint::quit_event_loop();
                }
            }
        })
        .unwrap();
    // First frame: no texture yet, shown as a plain image; the next render makes the texture.
    let first = picture(RED, BLUE);
    let shown = gl_frames::offer("s", &first, None);
    ui.set_img(shown.unwrap_or_else(|| Image::from_rgba8(first.clone())));
    let w = ui.as_weak();
    // OpenGL loads at the first render: offer again until it did, then leave the texture to the steps above.
    let timer = slint::Timer::default();
    let s = Rc::clone(&step);
    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(50), move || {
        if s.get() == 0 && gl_frames::offer("s", &first, None).is_none() {
            w.unwrap().window().request_redraw();
        }
    });
    let watchdog = slint::Timer::default();
    watchdog.start(slint::TimerMode::SingleShot, std::time::Duration::from_secs(5), || {
        println!("FAIL: timed out");
        let _ = slint::quit_event_loop();
    });
    ui.run().unwrap();
    std::process::exit(if step.get() == 3 { 0 } else { 1 });
}
