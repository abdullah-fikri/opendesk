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

/// BT.709 limited-range BGRX -> NV12 (2x2 chroma average). `w`, `h` must be even. Uses AVX2 when the CPU has
/// it (bit-identical output), else the portable loop.
pub fn bgra_to_nv12(src: &[u32], stride: usize, w: usize, h: usize, dst: &mut Vec<u8>) {
    dst.resize(w * h * 3 / 2, 0);
    assert!(h == 0 || src.len() >= (h - 1) * stride + w, "source smaller than {w}x{h}");
    let (ys, uv) = dst.split_at_mut(w * h);
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: AVX2 is present; the bounds were checked above.
        unsafe { avx2::bgra_to_nv12(src, stride, w, h, ys, uv) };
        return;
    }
    bgra_to_nv12_columns(src, stride, w, h, 0, ys, uv);
}

/// The portable conversion of columns `from..w` (`from` even).
fn bgra_to_nv12_columns(src: &[u32], stride: usize, w: usize, h: usize, from: usize, ys: &mut [u8], uv: &mut [u8]) {
    for y in (0..h).step_by(2) {
        for x in (from..w).step_by(2) {
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

/// AVX2 versions of [`bgra_to_nv12`] and [`nv12_to_rgba`].
#[cfg(target_arch = "x86_64")]
mod avx2 {
    use std::arch::x86_64::*;

    /// 8 pixels of two rows per step, the same integer arithmetic as the portable loop in 32-bit lanes.
    ///
    /// # Safety
    /// The CPU must support AVX2; `src` must hold `h` rows of `stride` pixels (the last at least `w`), `ys`
    /// `w * h` bytes and `uv` `w * h / 2` bytes.
    #[target_feature(enable = "avx2")]
    pub unsafe fn bgra_to_nv12(src: &[u32], stride: usize, w: usize, h: usize, ys: &mut [u8], uv: &mut [u8]) {
        let simd_w = w / 8 * 8;
        let mask = _mm256_set1_epi32(0xFF);
        let (c47, c157, c16) = (_mm256_set1_epi32(47), _mm256_set1_epi32(157), _mm256_set1_epi32(16));
        let (cu_r, cu_g, cu_b) = (_mm256_set1_epi32(-26), _mm256_set1_epi32(-87), _mm256_set1_epi32(112));
        let (cv_r, cv_g, cv_b) = (_mm256_set1_epi32(112), _mm256_set1_epi32(-102), _mm256_set1_epi32(-10));
        let (round, c128) = (_mm256_set1_epi32(128), _mm256_set1_epi32(128));
        let split = |p: __m256i| {
            (
                _mm256_and_si256(_mm256_srli_epi32(p, 16), mask),
                _mm256_and_si256(_mm256_srli_epi32(p, 8), mask),
                _mm256_and_si256(p, mask),
            )
        };
        let luma = |r, g, b| {
            let s = _mm256_add_epi32(
                _mm256_add_epi32(_mm256_mullo_epi32(r, c47), _mm256_mullo_epi32(g, c157)),
                _mm256_add_epi32(_mm256_mullo_epi32(b, c16), round),
            );
            _mm256_add_epi32(_mm256_srai_epi32(s, 8), c16)
        };
        let chroma = |r, g, b, cr, cg, cb| {
            let s = _mm256_add_epi32(
                _mm256_add_epi32(_mm256_mullo_epi32(r, cr), _mm256_mullo_epi32(g, cg)),
                _mm256_add_epi32(_mm256_mullo_epi32(b, cb), round),
            );
            _mm256_add_epi32(_mm256_srai_epi32(s, 8), c128)
        };
        // SAFETY: the caller guarantees AVX2 and the buffer sizes; every pointer below stays inside its row.
        unsafe {
            for y in (0..h).step_by(2) {
                let (row0, row1) = (src.as_ptr().add(y * stride), src.as_ptr().add((y + 1) * stride));
                let (y0, y1, c) =
                    (ys.as_mut_ptr().add(y * w), ys.as_mut_ptr().add((y + 1) * w), uv.as_mut_ptr().add(y / 2 * w));
                for x in (0..simd_w).step_by(8) {
                    let (r0, g0, b0) = split(_mm256_loadu_si256(row0.add(x).cast()));
                    let (r1, g1, b1) = split(_mm256_loadu_si256(row1.add(x).cast()));
                    // Luma of both rows to bytes: [row0 x..x+8][row1 x..x+8].
                    let words = _mm256_permute4x64_epi64(
                        _mm256_packus_epi32(luma(r0, g0, b0), luma(r1, g1, b1)),
                        0b11_01_10_00,
                    );
                    let bytes = _mm256_packus_epi16(words, _mm256_setzero_si256());
                    _mm_storel_epi64(y0.add(x).cast(), _mm256_castsi256_si128(bytes));
                    _mm_storel_epi64(y1.add(x).cast(), _mm256_extracti128_si256(bytes, 1));
                    // 2x2 averages: column sums, then adjacent pairs -> [c0 c1 c0 c1 | c2 c3 c2 c3], divided by 4.
                    let avg = |a, b| {
                        let s = _mm256_add_epi32(a, b);
                        _mm256_srai_epi32(_mm256_hadd_epi32(s, s), 2)
                    };
                    let (r, g, b) = (avg(r0, r1), avg(g0, g1), avg(b0, b1));
                    let u = chroma(r, g, b, cu_r, cu_g, cu_b);
                    let v = chroma(r, g, b, cv_r, cv_g, cv_b);
                    // [u0 v0 u1 v1 | u2 v2 u3 v3] -> 8 interleaved bytes.
                    let uv32 = _mm256_unpacklo_epi32(u, v);
                    let uv8 = _mm256_packus_epi16(_mm256_packus_epi32(uv32, uv32), _mm256_setzero_si256());
                    let (lo, hi) = (_mm256_castsi256_si128(uv8), _mm256_extracti128_si256(uv8, 1));
                    std::ptr::write_unaligned(c.add(x).cast::<i32>(), _mm_cvtsi128_si32(lo));
                    std::ptr::write_unaligned(c.add(x + 4).cast::<i32>(), _mm_cvtsi128_si32(hi));
                }
            }
        }
        if simd_w < w {
            super::bgra_to_nv12_columns(src, stride, w, h, simd_w, ys, uv);
        }
    }

    /// AVX2 version of [`super::nv12_to_rgba`]: 8 pixels per step (4 chroma pairs), the same integer arithmetic
    /// in 32-bit lanes; clamping to 0..=255 matches the portable `clamp`.
    ///
    /// # Safety
    /// The CPU must support AVX2; `yp` must hold `h` rows of `pitch` bytes (the last at least `w`), `uv`
    /// `(h + 1) / 2` such rows and `out` `w * h * 4` bytes.
    #[target_feature(enable = "avx2")]
    pub unsafe fn nv12_to_rgba(yp: &[u8], uv: &[u8], pitch: usize, w: usize, h: usize, out: &mut [u8]) {
        let simd_w = w / 8 * 8;
        let (c16, c128, c255) = (_mm256_set1_epi32(16), _mm256_set1_epi32(128), _mm256_set1_epi32(255));
        let (c298, c459, c541) = (_mm256_set1_epi32(298), _mm256_set1_epi32(459), _mm256_set1_epi32(541));
        let (cg_u, cg_v) = (_mm256_set1_epi32(-55), _mm256_set1_epi32(-136));
        let alpha = _mm256_set1_epi32(0xFF00_0000u32 as i32);
        let zero = _mm256_setzero_si256();
        let channel =
            |l, c| _mm256_min_epi32(_mm256_max_epi32(_mm256_srai_epi32(_mm256_add_epi32(l, c), 8), zero), c255);
        // SAFETY: the caller guarantees AVX2 and the buffer sizes; every load reads 8 bytes at `x < simd_w <= w`
        // of its row and every store writes 8 pixels inside the output row.
        unsafe {
            for y in 0..h {
                let yrow = yp.as_ptr().add(y * pitch);
                let crow = uv.as_ptr().add(y / 2 * pitch);
                let orow = out.as_mut_ptr().add(y * w * 4);
                for x in (0..simd_w).step_by(8) {
                    let luma = _mm256_cvtepu8_epi32(_mm_loadl_epi64(yrow.add(x).cast()));
                    // [u0 v0 u1 v1 | u2 v2 u3 v3] -> each pair repeated for its two pixels.
                    let pairs = _mm256_sub_epi32(_mm256_cvtepu8_epi32(_mm_loadl_epi64(crow.add(x).cast())), c128);
                    let u = _mm256_shuffle_epi32(pairs, 0b10_10_00_00);
                    let v = _mm256_shuffle_epi32(pairs, 0b11_11_01_01);
                    let rv = _mm256_add_epi32(_mm256_mullo_epi32(v, c459), c128);
                    let guv = _mm256_add_epi32(
                        _mm256_add_epi32(_mm256_mullo_epi32(u, cg_u), _mm256_mullo_epi32(v, cg_v)),
                        c128,
                    );
                    let bu = _mm256_add_epi32(_mm256_mullo_epi32(u, c541), c128);
                    let l = _mm256_mullo_epi32(_mm256_sub_epi32(luma, c16), c298);
                    // One RGBA pixel per 32-bit lane, little-endian: R | G << 8 | B << 16 | A << 24.
                    let px = _mm256_or_si256(
                        _mm256_or_si256(channel(l, rv), _mm256_slli_epi32(channel(l, guv), 8)),
                        _mm256_or_si256(_mm256_slli_epi32(channel(l, bu), 16), alpha),
                    );
                    _mm256_storeu_si256(orow.add(x * 4).cast(), px);
                }
            }
        }
        if simd_w < w {
            super::nv12_to_rgba_columns(yp, uv, pitch, w, h, simd_w, out);
        }
    }
}

/// BT.709 limited-range NV12 -> RGBA, cropping to `w` x `h`. `uv` starts the chroma plane; both use `pitch`.
/// Uses AVX2 when the CPU has it (byte-identical output), else the portable loop. With an odd `w` the last
/// column of each row is left as it was (NV12 frames from the decoders always have even sizes).
pub fn nv12_to_rgba(yp: &[u8], uv: &[u8], pitch: usize, w: usize, h: usize, out: &mut Vec<u8>) {
    out.resize(w * h * 4, 0);
    assert!(h == 0 || yp.len() >= (h - 1) * pitch + w, "luma plane smaller than {w}x{h}");
    assert!(h == 0 || uv.len() >= (h - 1) / 2 * pitch + w, "chroma plane smaller than {w}x{h}");
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: AVX2 is present; the plane sizes were checked above and `out` holds `w * h` pixels.
        unsafe { avx2::nv12_to_rgba(yp, uv, pitch, w, h, out) };
        return;
    }
    nv12_to_rgba_columns(yp, uv, pitch, w, h, 0, out);
}

/// The portable conversion of columns `from..w` (`from` even) into `out` (`w * h` RGBA pixels).
fn nv12_to_rgba_columns(yp: &[u8], uv: &[u8], pitch: usize, w: usize, h: usize, from: usize, out: &mut [u8]) {
    for y in 0..h {
        let orow = &mut out[(y * w + from) * 4..(y + 1) * w * 4];
        let yrow = &yp[y * pitch + from..y * pitch + w];
        let crow = &uv[(y / 2) * pitch + from..(y / 2) * pitch + w];
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
    fn simd_conversion_is_identical_to_the_portable_one() {
        // Odd widths in multiples of 2 exercise the scalar tail; a stride wider than the frame the padding.
        for (w, h, stride) in [(64, 32, 64), (1728, 6, 1728), (70, 4, 80), (2, 2, 2), (14, 2, 16)] {
            let mut src = pattern(stride, h, 5);
            // Extremes and noise, so every rounding and saturation path is hit.
            for (i, p) in src.iter_mut().enumerate() {
                *p = match i % 7 {
                    0 => 0x00FF_FFFF,
                    1 => 0,
                    2 => (i as u32).wrapping_mul(2_654_435_761),
                    _ => *p,
                };
            }
            let mut fast = Vec::new();
            bgra_to_nv12(&src, stride, w, h, &mut fast);
            let mut slow = vec![0; w * h * 3 / 2];
            let (ys, uv) = slow.split_at_mut(w * h);
            bgra_to_nv12_columns(&src, stride, w, h, 0, ys, uv);
            assert!(fast == slow, "{w}x{h} stride {stride}");
        }
    }

    /// `cargo test --release conversion_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn conversion_speed() {
        let (w, h) = (1728, 1080);
        let src = pattern(w, h, 1);
        let mut dst = vec![0u8; w * h * 3 / 2];
        let t = std::time::Instant::now();
        for _ in 0..100 {
            let (ys, uv) = dst.split_at_mut(w * h);
            bgra_to_nv12_columns(&src, w, w, h, 0, ys, uv);
        }
        let slow = t.elapsed().as_secs_f64() * 10.0;
        let mut dst = Vec::new();
        let t = std::time::Instant::now();
        for _ in 0..100 {
            bgra_to_nv12(&src, w, w, h, &mut dst);
        }
        let fast = t.elapsed().as_secs_f64() * 10.0;
        println!("{w}x{h} BGRX -> NV12: portable {slow:.2} ms, selected {fast:.2} ms");
    }

    /// Random bytes from a fixed-seed xorshift, so failures reproduce.
    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn simd_nv12_to_rgba_is_identical_to_the_portable_one() {
        // Widths below, at and past the 8-pixel vector width, odd sizes, and a pitch wider than the frame.
        let sizes = [
            (2, 2, 2),
            (8, 2, 8),
            (14, 3, 16),
            (17, 5, 32),
            (64, 32, 64),
            (70, 7, 80),
            (1919, 9, 1920),
            (1920, 1080, 2048),
        ];
        for (seed, (w, h, pitch)) in sizes.into_iter().enumerate() {
            let mut yp = noise(h * pitch, seed as u64 + 1);
            let mut uv = noise(h.div_ceil(2) * pitch, seed as u64 + 100);
            // Extremes so every clamp path is hit: black/white luma with saturated chroma.
            for (i, b) in yp.iter_mut().enumerate().filter(|(i, _)| i % 5 == 0) {
                *b = if i % 2 == 0 { 0 } else { 255 };
            }
            for (i, b) in uv.iter_mut().enumerate().filter(|(i, _)| i % 3 == 0) {
                *b = if i % 2 == 0 { 0 } else { 255 };
            }
            // Same starting contents, so the column odd widths leave untouched compares equal too.
            let start = noise(w * h * 4, seed as u64 + 200);
            let mut fast = start.clone();
            nv12_to_rgba(&yp, &uv, pitch, w, h, &mut fast);
            let mut slow = start;
            nv12_to_rgba_columns(&yp, &uv, pitch, w, h, 0, &mut slow);
            assert!(fast == slow, "{w}x{h} pitch {pitch}");
        }
    }

    /// `cargo test --release nv12_to_rgba_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn nv12_to_rgba_speed() {
        let (w, h) = (1920, 1080);
        let (yp, uv) = (noise(w * h, 1), noise(w * h / 2, 2));
        let mut out = vec![0u8; w * h * 4];
        let t = std::time::Instant::now();
        for _ in 0..100 {
            nv12_to_rgba_columns(&yp, &uv, w, w, h, 0, &mut out);
        }
        let slow = t.elapsed().as_secs_f64() * 10.0;
        let mut out = Vec::new();
        let t = std::time::Instant::now();
        for _ in 0..100 {
            nv12_to_rgba(&yp, &uv, w, w, h, &mut out);
        }
        let fast = t.elapsed().as_secs_f64() * 10.0;
        println!("{w}x{h} NV12 -> RGBA: portable {slow:.2} ms, selected {fast:.2} ms");
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
