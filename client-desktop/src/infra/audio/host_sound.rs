//! Host sound (`audio-system`) helpers shared by the Windows backend: loopback format conversion to 48 kHz
//! stereo, 20 ms Opus packets on the host and their decoding on the viewer. Pure logic, no devices.

use parking_lot::Mutex;

use super::opus;

/// Converts loopback capture (the device's mix format: any rate, any channel count, float or integer samples)
/// into 48 kHz interleaved stereo i16. Extra channels (centre, surround) are dropped and mono is duplicated;
/// other rates are resampled linearly, which is plenty for monitoring what the host plays.
pub struct LoopbackConverter {
    channels: usize,
    /// Bytes per sample in one channel (2, 3 or 4).
    container: usize,
    float: bool,
    /// Input frames per output frame.
    step: f64,
    /// Position of the next output frame between `prev` (0) and the next input frame (1).
    t: f64,
    prev: [f32; 2],
}

impl LoopbackConverter {
    /// `None` for a format this cannot read (no channels, or a sample size other than 16, 24 or 32 bits, or
    /// 32-bit float).
    pub fn new(rate: u32, channels: usize, block_align: usize, float: bool) -> Option<Self> {
        if rate == 0 || channels == 0 || !block_align.is_multiple_of(channels) {
            return None;
        }
        let container = block_align / channels;
        if !(2..=4).contains(&container) || (float && container != 4) {
            return None;
        }
        Some(Self {
            channels,
            container,
            float,
            step: f64::from(rate) / f64::from(opus::DECODE_RATE),
            t: 0.0,
            prev: [0.0; 2],
        })
    }

    /// Bytes per input frame.
    pub fn block_align(&self) -> usize {
        self.channels * self.container
    }

    fn sample(&self, b: &[u8]) -> f32 {
        if self.float {
            return f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        // Little-endian integer, left-aligned into an i32 so every container size scales alike.
        let mut v = 0i32;
        for (i, byte) in b.iter().enumerate() {
            v |= i32::from(*byte) << (8 * (4 - self.container + i));
        }
        v as f32 / 2_147_483_648.0
    }

    /// Appends the 48 kHz stereo samples for `raw` (whole input frames) to `out`.
    pub fn push(&mut self, raw: &[u8], out: &mut Vec<i16>) {
        let to_i16 = |x: f32| (x.clamp(-1.0, 1.0) * 32767.0) as i16;
        for frame in raw.chunks_exact(self.block_align()) {
            let left = self.sample(&frame[..self.container]);
            let right = if self.channels > 1 { self.sample(&frame[self.container..2 * self.container]) } else { left };
            let cur = [left, right];
            while self.t < 1.0 {
                let t = self.t as f32;
                out.push(to_i16(self.prev[0] + (cur[0] - self.prev[0]) * t));
                out.push(to_i16(self.prev[1] + (cur[1] - self.prev[1]) * t));
                self.t += self.step;
            }
            self.t -= 1.0;
            self.prev = cur;
        }
    }
}

/// Host side of host sound: cuts 48 kHz stereo samples into 20 ms Opus packets. Silent frames are not sent:
/// a host that plays nothing costs no bandwidth.
pub struct HostSoundSender {
    pending: Vec<i16>,
    pcm: Vec<u8>,
    opus: Option<opus::Encoder>,
}

/// 20 ms of 48 kHz stereo, in samples.
const HOST_SOUND_FRAME: usize = 960 * 2;

impl HostSoundSender {
    pub fn new() -> Self {
        Self { pending: Vec::new(), pcm: Vec::with_capacity(HOST_SOUND_FRAME * 2), opus: opus::Encoder::host_sound() }
    }

    /// Samples waiting for the converter's output.
    pub fn buffer(&mut self) -> &mut Vec<i16> {
        &mut self.pending
    }

    /// Encodes every complete frame waiting and hands each packet to `send`.
    pub fn drain(&mut self, mut send: impl FnMut(Vec<u8>)) {
        let mut start = 0;
        while self.pending.len() - start >= HOST_SOUND_FRAME {
            let frame = &self.pending[start..start + HOST_SOUND_FRAME];
            start += HOST_SOUND_FRAME;
            if frame.iter().all(|&s| s == 0) {
                continue;
            }
            self.pcm.clear();
            self.pcm.extend(frame.iter().flat_map(|s| s.to_le_bytes()));
            if let Some(pkt) = self.opus.as_mut().and_then(|e| e.encode(&self.pcm)) {
                send(pkt);
            }
        }
        self.pending.drain(..start);
    }
}

/// Viewer side of host sound: one stereo decoder (only the focused host's sound plays).
pub struct HostSoundReceiver {
    opus: Mutex<Option<opus::Decoder>>,
}

impl HostSoundReceiver {
    pub fn new() -> Self {
        Self { opus: Mutex::new(None) }
    }

    /// Hands every 20 ms of 48 kHz interleaved stereo PCM (16-bit little-endian) in `bin` to `play`.
    pub fn receive(&self, bin: &[u8], mut play: impl FnMut(Vec<u8>)) {
        let Some((seq, payload)) = opus::parse_host_sound(bin) else { return };
        let mut dec = self.opus.lock();
        if dec.is_none() {
            *dec = opus::Decoder::stereo();
        }
        if let Some(dec) = dec.as_mut() {
            dec.decode(seq, payload).into_iter().for_each(&mut play);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f32_frames(frames: &[[f32; 2]]) -> Vec<u8> {
        frames.iter().flat_map(|f| f.iter().flat_map(|s| s.to_le_bytes())).collect()
    }

    #[test]
    fn converter_reads_float_stereo_at_48k() {
        let mut c = LoopbackConverter::new(48_000, 2, 8, true).unwrap();
        let mut out = Vec::new();
        c.push(&f32_frames(&[[0.5, -0.5], [1.0, -1.0], [0.0, 0.25]]), &mut out);
        // One frame of delay (the interpolation start), then the input as is.
        assert_eq!(out, [0, 0, 16383, -16383, 32767, -32767]);
    }

    #[test]
    fn converter_keeps_front_pair_and_duplicates_mono() {
        // 16-bit 5.1 (6 channels): only front left and right survive.
        let mut c = LoopbackConverter::new(48_000, 6, 12, false).unwrap();
        let frame: Vec<u8> = [16384i16, -16384, 999, 999, 999, 999].iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = Vec::new();
        c.push(&[frame.clone(), frame].concat(), &mut out);
        assert_eq!(&out[2..], [16383, -16383]);
        // 24-bit packed mono.
        let mut c = LoopbackConverter::new(48_000, 1, 3, false).unwrap();
        let half = (1i32 << 22).to_le_bytes();
        let mut out = Vec::new();
        c.push(&[&half[..3], &half[..3]].concat(), &mut out);
        assert_eq!(&out[2..], [16383, 16383]);
    }

    #[test]
    fn converter_resamples_to_48k() {
        for (rate, expect) in [(44_100u32, 48_000usize), (96_000, 48_000), (48_000, 48_000)] {
            let mut c = LoopbackConverter::new(rate, 2, 8, true).unwrap();
            let mut out = Vec::new();
            c.push(&f32_frames(&vec![[0.1, 0.1]; rate as usize]), &mut out);
            let frames = out.len() / 2;
            assert!(frames.abs_diff(expect) <= 1, "{rate} Hz: {frames} frames for one second");
        }
    }

    #[test]
    fn converter_rejects_unreadable_formats() {
        assert!(LoopbackConverter::new(48_000, 0, 0, false).is_none());
        assert!(LoopbackConverter::new(48_000, 2, 2, false).is_none(), "8-bit");
        assert!(LoopbackConverter::new(48_000, 2, 4, true).is_none(), "16-bit float");
        assert!(LoopbackConverter::new(0, 2, 8, true).is_none());
    }

    #[test]
    fn host_sound_skips_silence_and_round_trips() {
        let mut tx = HostSoundSender::new();
        let mut sent = Vec::new();
        tx.buffer().extend(std::iter::repeat_n(0i16, HOST_SOUND_FRAME));
        tx.buffer().extend((0..HOST_SOUND_FRAME).map(|i| ((i as f32 * 0.05).sin() * 8000.0) as i16));
        tx.buffer().extend(std::iter::repeat_n(1i16, 10));
        tx.drain(|p| sent.push(p));
        assert_eq!(sent.len(), 1, "the silent frame is skipped");
        assert_eq!(tx.buffer().len(), 10, "an incomplete frame waits");
        let rx = HostSoundReceiver::new();
        let mut played = Vec::new();
        rx.receive(&sent[0], |pcm| played.push(pcm.len()));
        rx.receive(&[0x02, 48, 0, 0], |pcm| played.push(pcm.len()));
        assert_eq!(played, [HOST_SOUND_FRAME * 2]);
    }
}
