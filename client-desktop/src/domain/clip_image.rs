//! Clipboard images on the wire (`clipboard-image`): always PNG, whatever the clipboard held. Windows keeps
//! copied images as a device-independent bitmap (CF_DIB, often uncompressed: ~8 MB for a 1080p screenshot) and
//! sometimes also as PNG; this converts between the two.

use std::io::Cursor;

use core_engine::clipboard::ClipImage;
use image::{ImageFormat, RgbaImage};

/// Largest PNG sent (it travels base64 inside one relay message, which the relay caps at 16 MB).
pub const MAX_PNG_BYTES: usize = 11 << 20;
/// Largest image accepted from the partner, in pixels (16K x 8K).
const MAX_PIXELS: u64 = 16384 * 8192;

const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

/// The PNG to send for a clipboard image; None when it cannot be read or is too large.
pub fn to_png(image: ClipImage) -> Option<Vec<u8>> {
    let png = match image {
        // The partner decodes it anyway; the signature is enough here.
        ClipImage::Png(png) if png.starts_with(PNG_SIGNATURE) => png,
        ClipImage::Png(_) => return None,
        ClipImage::Dib(dib) => encode_png(&dib_to_rgba(&dib)?)?,
    };
    (png.len() <= MAX_PNG_BYTES).then_some(png)
}

/// What to put on the clipboard for a received PNG: `(dib, png)`. None when it is not a PNG within limits.
pub fn from_png(png: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut reader = image::ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_PIXELS * 4 + (64 << 20));
    reader.limits(limits);
    let img = reader.decode().ok()?.to_rgba8();
    if u64::from(img.width()) * u64::from(img.height()) > MAX_PIXELS {
        return None;
    }
    Some((rgba_to_dib(&img), png.to_vec()))
}

fn encode_png(img: &RgbaImage) -> Option<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    // Opaque images (nearly every screenshot) without the alpha channel: a quarter fewer bytes to compress.
    if img.pixels().all(|p| p[3] == 255) {
        image::DynamicImage::ImageRgba8(img.clone()).to_rgb8().write_to(&mut out, ImageFormat::Png).ok()?;
    } else {
        img.write_to(&mut out, ImageFormat::Png).ok()?;
    }
    Some(out.into_inner())
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Pixels of a CF_DIB (24- or 32-bit, uncompressed or bit fields). 32-bit images whose alpha is all zero (most
/// programs leave it unset) count as opaque.
pub fn dib_to_rgba(dib: &[u8]) -> Option<RgbaImage> {
    let header = u32_at(dib, 0)? as usize;
    if header < 40 {
        return None;
    }
    let width = u32_at(dib, 4)? as i32;
    let height = u32_at(dib, 8)? as i32;
    let bits = u16_at(dib, 14)?;
    let compression = u32_at(dib, 16)?;
    let colors_used = u32_at(dib, 32)? as usize;
    if width <= 0 || height == 0 || !matches!(bits, 24 | 32) {
        return None;
    }
    let (w, h) = (width as u32, height.unsigned_abs());
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return None;
    }
    // Channel masks: right after a 40-byte header for bit fields, inside larger (V4/V5) headers.
    let (masks, masks_len) = match compression {
        BI_RGB => ([0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0], 0),
        BI_BITFIELDS | BI_ALPHABITFIELDS if bits == 32 => {
            let extra = if compression == BI_ALPHABITFIELDS { 16 } else { 12 };
            let alpha = if compression == BI_ALPHABITFIELDS || header >= 56 { u32_at(dib, 52)? } else { 0 };
            ([u32_at(dib, 40)?, u32_at(dib, 44)?, u32_at(dib, 48)?, alpha], if header == 40 { extra } else { 0 })
        }
        _ => return None,
    };
    let pixels_at = header + masks_len + colors_used * 4;
    let stride = (w as usize * bits as usize).div_ceil(32) * 4;
    let data = dib.get(pixels_at..pixels_at + stride * h as usize)?;
    let channel = |px: u32, mask: u32| -> u8 {
        if mask == 0 {
            return 0;
        }
        let v = (px & mask) >> mask.trailing_zeros();
        let max = mask >> mask.trailing_zeros();
        if max == 255 { v as u8 } else { (v * 255 / max) as u8 }
    };
    let mut img = RgbaImage::new(w, h);
    let mut any_alpha = false;
    for y in 0..h {
        // Positive height: rows stored bottom-up.
        let src = if height > 0 { h - 1 - y } else { y } as usize;
        let row = &data[src * stride..];
        for x in 0..w as usize {
            let rgba = if bits == 24 {
                [row[x * 3 + 2], row[x * 3 + 1], row[x * 3], 255]
            } else {
                let px = u32::from_le_bytes(row[x * 4..x * 4 + 4].try_into().unwrap());
                let mut c = [channel(px, masks[0]), channel(px, masks[1]), channel(px, masks[2]), 0];
                c[3] = if compression == BI_RGB { (px >> 24) as u8 } else { channel(px, masks[3]) };
                any_alpha |= c[3] != 0;
                c
            };
            img.put_pixel(x as u32, y, image::Rgba(rgba));
        }
    }
    if bits == 32 && !any_alpha {
        img.pixels_mut().for_each(|p| p[3] = 255);
    }
    Some(img)
}

/// A bottom-up 32-bit CF_DIB of `img` (BI_RGB, alpha in the top byte), which every Windows program reads.
pub fn rgba_to_dib(img: &RgbaImage) -> Vec<u8> {
    let (w, h) = img.dimensions();
    let mut dib = Vec::with_capacity(40 + w as usize * h as usize * 4);
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(w as i32).to_le_bytes());
    dib.extend_from_slice(&(h as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&BI_RGB.to_le_bytes());
    dib.extend_from_slice(&(w * h * 4).to_le_bytes());
    dib.extend_from_slice(&[0u8; 16]); // resolution, palette sizes
    for y in (0..h).rev() {
        for x in 0..w {
            let [r, g, b, a] = img.get_pixel(x, y).0;
            dib.extend_from_slice(&[b, g, r, a]);
        }
    }
    dib
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RgbaImage {
        RgbaImage::from_fn(7, 5, |x, y| image::Rgba([(x * 30) as u8, (y * 50) as u8, 200, 255]))
    }

    #[test]
    fn dib_round_trip_keeps_pixels() {
        let img = sample();
        assert_eq!(dib_to_rgba(&rgba_to_dib(&img)).unwrap(), img);
    }

    #[test]
    fn png_round_trip_through_the_wire_format() {
        let img = sample();
        let png = to_png(ClipImage::Dib(rgba_to_dib(&img))).unwrap();
        let (dib, same_png) = from_png(&png).unwrap();
        assert_eq!(same_png, png);
        assert_eq!(dib_to_rgba(&dib).unwrap(), img);
    }

    #[test]
    fn unset_alpha_counts_as_opaque() {
        let mut dib = rgba_to_dib(&sample());
        for px in dib[40..].chunks_mut(4) {
            px[3] = 0;
        }
        assert!(dib_to_rgba(&dib).unwrap().pixels().all(|p| p[3] == 255));
    }

    #[test]
    fn reads_24_bit_top_down_dibs() {
        // 2x2, top-down (negative height), rows padded to 8 bytes.
        let mut dib = rgba_to_dib(&RgbaImage::new(1, 1))[..40].to_vec();
        dib[4..8].copy_from_slice(&2i32.to_le_bytes());
        dib[8..12].copy_from_slice(&(-2i32).to_le_bytes());
        dib[14..16].copy_from_slice(&24u16.to_le_bytes());
        dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // red, green
        dib.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]); // blue, white
        let img = dib_to_rgba(&dib).unwrap();
        assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(1, 0).0, [0, 255, 0, 255]);
        assert_eq!(img.get_pixel(0, 1).0, [0, 0, 255, 255]);
        assert_eq!(img.get_pixel(1, 1).0, [255, 255, 255, 255]);
    }

    #[test]
    fn a_valid_png_is_sent_as_is_and_garbage_is_refused() {
        let png = encode_png(&sample()).unwrap();
        assert_eq!(to_png(ClipImage::Png(png.clone())).unwrap(), png);
        assert!(to_png(ClipImage::Png(vec![1, 2, 3])).is_none());
        assert!(from_png(b"not a png").is_none());
        assert!(dib_to_rgba(&[0u8; 10]).is_none());
    }
}
