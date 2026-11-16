//! Lossless tiles for text and UI (`tile-lossless`): blocks with few distinct colors (text, icons, window
//! chrome) are sent pixel-exact instead of as JPEG, which blurs glyph edges and adds colour fringes (4:2:0).
//! Such blocks are also small once deflated: a palette of a handful of colours plus long runs of indices.
//!
//! Payload layout (big-endian): `width u16, height u16, mode u8`, then
//! - mode 1 (palette): `count-1 u8`, `count` x RGB, deflated palette indices (one byte per pixel, row-major);
//! - mode 0 (direct): deflated RGB triplets (only when an atlas holds more than 256 colours).

use miniz_oxide::deflate::compress_to_vec;
use miniz_oxide::inflate::decompress_to_vec_with_limit;

const MODE_DIRECT: u8 = 0;
const MODE_PALETTE: u8 = 1;
/// Deflate level: fast; screen content compresses well even at low levels.
const LEVEL: u8 = 1;

/// Most distinct colours a block may have and still count as text/UI. ClearType text on one background stays
/// well below; photos and gradients go far above within a few rows.
pub const MAX_BLOCK_COLORS: usize = 48;

/// True when the `w` x `h` region at (`x`, `y`) of BGRX `pixels` has at most [`MAX_BLOCK_COLORS`] colours.
/// Stops at the first colour over the limit, so photo blocks are rejected quickly.
pub fn is_flat(pixels: &[u32], stride: usize, x: usize, y: usize, w: usize, h: usize) -> bool {
    if x + w > stride {
        return false;
    }
    let mut seen = [0u32; MAX_BLOCK_COLORS];
    let mut n = 0;
    let mut last = u32::MAX;
    for row in y..y + h {
        let Some(line) = pixels.get(row * stride + x..row * stride + x + w) else { return false };
        for &p in line {
            let c = p & 0x00FF_FFFF;
            // Runs of the same colour are the common case.
            if c == last {
                continue;
            }
            last = c;
            if !seen[..n].contains(&c) {
                if n == MAX_BLOCK_COLORS {
                    return false;
                }
                seen[n] = c;
                n += 1;
            }
        }
    }
    true
}

/// Encodes a BGRX atlas (`w` x `h`, tightly packed).
pub fn encode(atlas: &[u32], w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + w * h / 4);
    out.extend_from_slice(&(w as u16).to_be_bytes());
    out.extend_from_slice(&(h as u16).to_be_bytes());
    let pixels = &atlas[..w * h];
    let mut palette: Vec<u32> = Vec::with_capacity(256);
    let mut indices = Vec::with_capacity(pixels.len());
    let mut last = (u32::MAX, 0u8);
    for &p in pixels {
        let c = p & 0x00FF_FFFF;
        if c == last.0 {
            indices.push(last.1);
            continue;
        }
        let i = match palette.iter().position(|&q| q == c) {
            Some(i) => i,
            None if palette.len() < 256 => {
                palette.push(c);
                palette.len() - 1
            }
            None => return encode_direct(out, pixels),
        };
        last = (c, i as u8);
        indices.push(i as u8);
    }
    out.push(MODE_PALETTE);
    out.push((palette.len().max(1) - 1) as u8);
    if palette.is_empty() {
        palette.push(0);
    }
    for c in &palette {
        out.extend_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, *c as u8]);
    }
    out.extend_from_slice(&compress_to_vec(&indices, LEVEL));
    out
}

fn encode_direct(mut out: Vec<u8>, pixels: &[u32]) -> Vec<u8> {
    out.push(MODE_DIRECT);
    let rgb: Vec<u8> = pixels.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8]).collect();
    out.extend_from_slice(&compress_to_vec(&rgb, LEVEL));
    out
}

/// Decodes a payload into `(RGBA, width, height)`; None for anything malformed (never more than the stated
/// size is inflated).
pub fn decode(payload: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let w = u16::from_be_bytes([*payload.first()?, *payload.get(1)?]) as usize;
    let h = u16::from_be_bytes([*payload.get(2)?, *payload.get(3)?]) as usize;
    let n = w * h;
    if n == 0 {
        return None;
    }
    let mut rgba = Vec::with_capacity(n * 4);
    match *payload.get(4)? {
        MODE_PALETTE => {
            let count = *payload.get(5)? as usize + 1;
            let colors = payload.get(6..6 + count * 3)?;
            let indices = decompress_to_vec_with_limit(payload.get(6 + count * 3..)?, n).ok()?;
            if indices.len() != n || indices.iter().any(|&i| i as usize >= count) {
                return None;
            }
            let pal: Vec<[u8; 4]> = colors.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2], 255]).collect();
            rgba.resize(n * 4, 0);
            for (out, &i) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(&indices) {
                out.copy_from_slice(&pal[i as usize]);
            }
        }
        MODE_DIRECT => {
            let rgb = decompress_to_vec_with_limit(payload.get(5..)?, n * 3).ok()?;
            if rgb.len() != n * 3 {
                return None;
            }
            rgba.resize(n * 4, 255);
            for (out, c) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(rgb.as_chunks::<3>().0) {
                out[..3].copy_from_slice(c);
            }
        }
        _ => return None,
    }
    Some((rgba, w as u32, h as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba_of(atlas: &[u32]) -> Vec<u8> {
        atlas.iter().flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]).collect()
    }

    #[test]
    fn palette_atlas_round_trips_exactly() {
        let (w, h) = (64, 32);
        let atlas: Vec<u32> = (0..w * h).map(|i| if (i / 3) % 5 == 0 { 0xFF20_2020 } else { 0xFFF0_F0F0 }).collect();
        let enc = encode(&atlas, w, h);
        assert_eq!(enc[4], MODE_PALETTE);
        assert!(enc.len() < w * h / 4, "two colours compress to a fraction: {} bytes", enc.len());
        assert_eq!(decode(&enc), Some((rgba_of(&atlas), w as u32, h as u32)));
    }

    #[test]
    fn many_colours_fall_back_to_direct_and_still_round_trip() {
        let (w, h) = (40, 20);
        let atlas: Vec<u32> = (0..(w * h) as u32).map(|i| i.wrapping_mul(2_654_435_761)).collect();
        let enc = encode(&atlas, w, h);
        assert_eq!(enc[4], MODE_DIRECT);
        assert_eq!(decode(&enc), Some((rgba_of(&atlas), w as u32, h as u32)));
    }

    #[test]
    fn malformed_payloads_are_refused() {
        let enc = encode(&[0xFF00_0000; 16], 4, 4);
        assert!(decode(&enc[..enc.len() - 2]).is_none(), "truncated deflate stream");
        let mut big = enc.clone();
        big[0..4].copy_from_slice(&[0, 8, 0, 8]); // claims 8x8 but carries 16 indices
        assert!(decode(&big).is_none());
        let mut bad_index = encode(&[0xFF00_0000, 0xFF11_1111, 0xFF00_0000, 0xFF11_1111], 2, 2);
        bad_index[5] = 0; // palette of one colour, indices still reach 1
        assert!(decode(&bad_index).is_none());
        assert!(decode(&[0, 1, 0, 1, 9]).is_none(), "unknown mode");
        assert!(decode(&[]).is_none());
    }

    #[test]
    fn flat_detection_stops_at_the_colour_limit() {
        let stride = 40;
        let mut px = vec![0xFFFF_FFFFu32; stride * 32];
        assert!(is_flat(&px, stride, 0, 0, 32, 32));
        for i in 0..MAX_BLOCK_COLORS {
            px[(i / 32) * stride + i % 32] = i as u32; // inside the block's first two rows
        }
        assert!(!is_flat(&px, stride, 0, 0, 32, 32), "{} colours with white", MAX_BLOCK_COLORS + 1);
        assert!(is_flat(&px, stride, 0, 2, 32, 30), "rows below are plain");
        assert!(!is_flat(&px, stride, 30, 0, 32, 1), "out of bounds is not flat");
    }
}

#[cfg(all(test, windows))]
mod screen_tests {
    use super::*;
    use crate::capture::{DesktopCapturer, enumerate_monitors};
    use crate::codec::delta::{build_atlas, pack_atlas};
    use crate::codec::jpeg::JpegEncoder;
    use crate::diff::merge_blocks;
    use std::time::Instant;

    /// Real desktop: share of text/UI blocks and lossless vs JPEG size for them.
    /// `cargo test --release lossless_on_this_screen -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn lossless_on_this_screen() {
        let bounds = enumerate_monitors()[0];
        let mut cap = DesktopCapturer::new_for_monitor(bounds, 1920);
        cap.grab(500);
        let (w, h) = (cap.width() as usize, cap.height() as usize);
        let px = cap.pixels().to_vec();
        let (gw, gh) = (w.div_ceil(32), h.div_ceil(32));
        let t = Instant::now();
        let flat: Vec<bool> = (0..gw * gh)
            .map(|i| {
                let (bx, by) = ((i % gw) * 32, (i / gw) * 32);
                is_flat(&px, w, bx, by, 32.min(w - bx), 32.min(h - by))
            })
            .collect();
        let classify_ms = t.elapsed().as_secs_f64() * 1000.0;
        let n_flat = flat.iter().filter(|&&f| f).count();
        let rects = merge_blocks(&flat, gw, 32, w as i32, h as i32);
        let (aw, ah, patches) = pack_atlas(&rects, w as i32);
        let atlas = build_atlas(&px, w, &patches, aw, ah);
        let t = Instant::now();
        let ll = encode(&atlas, aw as usize, ah as usize);
        let ll_ms = t.elapsed().as_secs_f64() * 1000.0;
        let jpeg = JpegEncoder::new(82);
        let t = Instant::now();
        let jp = jpeg.encode_bgra(&atlas, aw, ah, aw as usize).unwrap();
        let jp_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        assert!(decode(&ll).is_some());
        let dec_ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "\n{w}x{h}: {n_flat}/{} blocks text/UI ({:.0}%), classify {classify_ms:.2} ms\n\
             same blocks: lossless {} KB in {ll_ms:.2} ms (decode {dec_ms:.2} ms) vs JPEG q82 {} KB in {jp_ms:.2} ms",
            gw * gh,
            100.0 * n_flat as f64 / (gw * gh) as f64,
            ll.len() / 1024,
            jp.len() / 1024
        );
    }
}
