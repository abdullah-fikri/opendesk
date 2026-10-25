//! JPEG encoding with the pure-Rust `jpeg-encoder` crate (AVX2 when the CPU has it), 4:2:0 like GDI+.

use jpeg_encoder::{ColorType, Encoder, SamplingFactor};

/// JPEG encoder with an adjustable quality. Stateless between frames.
pub struct JpegEncoder {
    quality: u32,
}

impl JpegEncoder {
    /// An encoder with `quality` (1..=100).
    pub fn new(quality: u32) -> Self {
        Self { quality: quality.clamp(1, 100) }
    }

    /// Changes the JPEG quality (0..=100) for later frames.
    pub fn set_quality(&mut self, quality: u32) {
        self.quality = quality.clamp(1, 100);
    }

    /// Current JPEG quality.
    pub fn quality(&self) -> u32 {
        self.quality
    }

    /// Encodes top-down BGRX pixels; `stride` is in pixels.
    pub fn encode_bgra(&self, pixels: &[u32], width: i32, height: i32, stride: usize) -> Option<Vec<u8>> {
        let (w, h) = (usize::try_from(width).ok()?, usize::try_from(height).ok()?);
        if w == 0 || h == 0 || w > u16::MAX as usize || h > u16::MAX as usize || stride < w {
            return None;
        }
        let rows = pixels.get(..stride * (h - 1) + w)?;
        let packed: Vec<u32>;
        let px: &[u32] = if stride == w {
            rows
        } else {
            packed = rows.chunks(stride).flat_map(|r| &r[..w]).copied().collect();
            &packed
        };
        // SAFETY: u32 has no padding and every byte is a valid u8. On little-endian targets (the only ones
        // `capture::host_support` accepts) a BGRX u32 is the bytes B, G, R, X; the encoder ignores the fourth.
        let bytes = unsafe { std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px)) };
        let mut out = Vec::with_capacity(w * h / 4);
        let mut enc = Encoder::new(&mut out, self.quality as u8);
        enc.set_sampling_factor(SamplingFactor::R_4_2_0);
        enc.encode(bytes, w as u16, h as u16, ColorType::Bgra).ok()?;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_strided_frame() {
        let pixels = vec![0x0040_80C0u32; 20 * 8];
        let jpeg = JpegEncoder::new(80).encode_bgra(&pixels, 16, 8, 20).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        assert!(JpegEncoder::new(80).encode_bgra(&pixels, 30, 8, 20).is_none());
    }
}
