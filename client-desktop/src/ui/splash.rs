//! Start-up splash: a short video, stored as JPEG frames (`assets/splash.bin`, packed by
//! `scripts/pack_splash.py`), played over the window until it ends or the user clicks or presses a key.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode};

use super::MainWindow;

static PACK: &[u8] = include_bytes!("../../assets/splash.bin");

/// One frame's JPEG bytes by index, or `None` past the end or on a damaged pack.
struct Pack {
    fps: u32,
    frames: Vec<&'static [u8]>,
}

fn parse(data: &'static [u8]) -> Option<Pack> {
    let fps = u16::from_le_bytes(data.get(0..2)?.try_into().ok()?) as u32;
    let count = u32::from_le_bytes(data.get(2..6)?.try_into().ok()?) as usize;
    let mut at = 6;
    let mut frames = Vec::with_capacity(count);
    for _ in 0..count {
        let len = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
        at += 4;
        frames.push(data.get(at..at + len)?);
        at += len;
    }
    (fps > 0 && !frames.is_empty()).then_some(Pack { fps, frames })
}

fn decode(jpeg: &[u8]) -> Option<Image> {
    let rgba = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(rgba.as_raw(), w, h)))
}

/// Plays the splash over `win`. Returns the timer, which must stay alive while the window runs. A damaged pack
/// shows no splash.
pub fn start(win: &MainWindow) -> Option<Timer> {
    let pack = parse(PACK)?;
    let first = decode(pack.frames[0])?;
    win.set_splash_frame(first);
    win.set_show_splash(true);

    let timer = Timer::default();
    let next = Rc::new(Cell::new(1usize));
    let weak = win.as_weak();
    let step = Duration::from_millis((1000 / pack.fps) as u64);
    let frames = pack.frames;
    timer.start(TimerMode::Repeated, step, move || {
        let Some(win) = weak.upgrade() else { return };
        // Skipped by click or key: the overlay is already gone.
        if !win.get_show_splash() {
            return;
        }
        let i = next.get();
        match frames.get(i).and_then(|f| decode(f)) {
            Some(img) => {
                win.set_splash_frame(img);
                next.set(i + 1);
            }
            None => win.set_show_splash(false),
        }
    });
    Some(timer)
}
