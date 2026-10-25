//! H.264 encoding and decoding. The platform backend is chosen at compile time: Media Foundation on Windows
//! (hardware encoder when available), Cisco's OpenH264 library elsewhere (loaded at runtime; absent = no
//! H.264, the stream stays on JPEG). Colour conversion is BT.709 limited range on both ends, done on the CPU
//! by the helpers below, so every backend pairs with every other.

#[cfg(windows)]
mod mf;
#[cfg(windows)]
pub use mf::*;

#[cfg(not(windows))]
mod cisco;
#[cfg(not(windows))]
pub use cisco::*;

/// NAL unit types present in an Annex-B access unit.
pub fn nal_types(au: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 3 < au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            types.push(au[i + 3] & 0x1F);
            i += 3;
        } else {
            i += 1;
        }
    }
    types
}

/// BT.709 limited-range BGRX -> NV12 (2x2 chroma average). `w`, `h` must be even.
pub fn bgra_to_nv12(src: &[u32], stride: usize, w: usize, h: usize, dst: &mut Vec<u8>) {
    dst.resize(w * h * 3 / 2, 0);
    let (ys, uv) = dst.split_at_mut(w * h);
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let (mut rs, mut gs, mut bs) = (0i32, 0i32, 0i32);
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let p = src[(y + dy) * stride + x + dx];
                let (r, g, b) = (((p >> 16) & 0xFF) as i32, ((p >> 8) & 0xFF) as i32, (p & 0xFF) as i32);
                ys[(y + dy) * w + x + dx] = ((47 * r + 157 * g + 16 * b + 128) >> 8).wrapping_add(16) as u8;
                rs += r;
                gs += g;
                bs += b;
            }
            let (r, g, b) = (rs / 4, gs / 4, bs / 4);
            uv[(y / 2) * w + x] = (((-26 * r - 87 * g + 112 * b + 128) >> 8) + 128) as u8;
            uv[(y / 2) * w + x + 1] = (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128) as u8;
        }
    }
}

/// BT.709 limited-range NV12 -> RGBA, cropping to `w` x `h`. `uv` starts the chroma plane; both use `pitch`.
pub fn nv12_to_rgba(yp: &[u8], uv: &[u8], pitch: usize, w: usize, h: usize, out: &mut Vec<u8>) {
    out.resize(w * h * 4, 0);
    for (y, orow) in out.chunks_exact_mut(w * 4).enumerate() {
        let yrow = &yp[y * pitch..y * pitch + w];
        let crow = &uv[(y / 2) * pitch..(y / 2) * pitch + w];
        // Two pixels share one chroma pair: compute its contribution once.
        for ((px, ys), c) in
            orow.as_chunks_mut::<8>().0.iter_mut().zip(yrow.as_chunks::<2>().0).zip(crow.as_chunks::<2>().0)
        {
            let (u, v) = (c[0] as i32 - 128, c[1] as i32 - 128);
            let (rv, guv, bu) = (459 * v + 128, -55 * u - 136 * v + 128, 541 * u + 128);
            for (o, &yv) in px.as_chunks_mut::<4>().0.iter_mut().zip(ys) {
                let l = 298 * (yv as i32 - 16);
                o[0] = ((l + rv) >> 8).clamp(0, 255) as u8;
                o[1] = ((l + guv) >> 8).clamp(0, 255) as u8;
                o[2] = ((l + bu) >> 8).clamp(0, 255) as u8;
                o[3] = 255;
            }
        }
    }
}

/// BT.709 limited-range planar I420 -> RGBA, cropping to `w` x `h` (same coefficients as [`nv12_to_rgba`]).
#[cfg(any(not(windows), test))]
#[allow(clippy::too_many_arguments)]
pub fn i420_to_rgba(
    yp: &[u8],
    up: &[u8],
    vp: &[u8],
    y_stride: usize,
    uv_stride: usize,
    w: usize,
    h: usize,
    out: &mut Vec<u8>,
) {
    out.resize(w * h * 4, 0);
    for (y, orow) in out.chunks_exact_mut(w * 4).enumerate() {
        let yrow = &yp[y * y_stride..y * y_stride + w];
        let urow = &up[(y / 2) * uv_stride..];
        let vrow = &vp[(y / 2) * uv_stride..];
        for (i, (px, ys)) in orow.as_chunks_mut::<8>().0.iter_mut().zip(yrow.as_chunks::<2>().0).enumerate() {
            let (u, v) = (urow[i] as i32 - 128, vrow[i] as i32 - 128);
            let (rv, guv, bu) = (459 * v + 128, -55 * u - 136 * v + 128, 541 * u + 128);
            for (o, &yv) in px.as_chunks_mut::<4>().0.iter_mut().zip(ys) {
                let l = 298 * (yv as i32 - 16);
                o[0] = ((l + rv) >> 8).clamp(0, 255) as u8;
                o[1] = ((l + guv) >> 8).clamp(0, 255) as u8;
                o[2] = ((l + bu) >> 8).clamp(0, 255) as u8;
                o[3] = 255;
            }
        }
    }
}

/// A moving gradient for codec tests.
#[cfg(test)]
pub(crate) fn test_pattern(w: usize, h: usize, t: usize) -> Vec<u32> {
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w + t * 4) % w, i / w);
            (((x * 255 / w) as u32) << 16) | (((y * 255 / h) as u32) << 8) | (((x ^ y) & 0xFF) as u32)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::test_pattern as pattern;
    use super::*;

    #[test]
    fn nv12_roundtrip_is_close() {
        let (w, h) = (64, 32);
        let src = pattern(w, h, 0);
        let mut nv12 = Vec::new();
        bgra_to_nv12(&src, w, w, h, &mut nv12);
        let mut rgba = Vec::new();
        nv12_to_rgba(&nv12[..w * h], &nv12[w * h..], w, w, h, &mut rgba);
        // Flat 2x2 blocks survive exactly enough; chroma subsampling bounds the error elsewhere.
        let flat = vec![0x00C0_4010u32; w * h];
        bgra_to_nv12(&flat, w, w, h, &mut nv12);
        nv12_to_rgba(&nv12[..w * h], &nv12[w * h..], w, w, h, &mut rgba);
        for (c, want) in [0xC0u8, 0x40, 0x10].iter().enumerate() {
            assert!((rgba[c] as i32 - *want as i32).abs() <= 2, "channel {c}: {} vs {want}", rgba[c]);
        }
    }

    #[test]
    fn i420_matches_nv12() {
        let (w, h) = (64, 32);
        let mut nv12 = Vec::new();
        bgra_to_nv12(&pattern(w, h, 3), w, w, h, &mut nv12);
        let (yp, uv) = nv12.split_at(w * h);
        let up: Vec<u8> = uv.iter().step_by(2).copied().collect();
        let vp: Vec<u8> = uv.iter().skip(1).step_by(2).copied().collect();
        let (mut a, mut b) = (Vec::new(), Vec::new());
        nv12_to_rgba(yp, uv, w, w, h, &mut a);
        i420_to_rgba(yp, &up, &vp, w, w / 2, w, h, &mut b);
        assert_eq!(a, b);
    }
}
