//! Voice call audio: microphone capture into binary voice packets (`[0x02][rate_khz][pcm16le]`, or Opus, see
//! `opus`) and playback of received packets, with a light DSP stage (high-pass, noise gate, soft limiter) on
//! captured audio. The device backend is selected at compile time: waveIn/waveOut on Windows, cpal (ALSA, which
//! PipeWire and PulseAudio serve) elsewhere.

use parking_lot::Mutex;

pub mod opus;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::*;

/// Maps audio quality tier (0 = 16kHz, 1 = 24kHz, 2 = 48kHz HD) to (sample_rate_hz, rate_tag_khz, frame_bytes_20ms)
pub fn quality_to_audio_params(quality: usize) -> (u32, u8, usize) {
    match quality {
        0 => (16000, 16, 640),  // 20ms @ 16kHz 16-bit mono
        1 => (24000, 24, 960),  // 20ms @ 24kHz 16-bit mono
        _ => (48000, 48, 1920), // 20ms @ 48kHz 16-bit mono (WhatsApp / AnyDesk HD Voice)
    }
}

/// Capture side shared by both backends: wraps each processed 20 ms frame as a PCM packet or, while the
/// partner listed `audio-opus`, as an Opus packet (PCM again if the encoder fails).
pub(crate) struct VoiceSender {
    rate: u32,
    rate_tag_khz: u8,
    opus: Option<opus::Encoder>,
}

impl VoiceSender {
    pub(crate) fn new(rate: u32, rate_tag_khz: u8) -> Self {
        Self { rate, rate_tag_khz, opus: None }
    }

    pub(crate) fn packet(&mut self, pcm: &[u8], use_opus: bool) -> Vec<u8> {
        if use_opus {
            if self.opus.is_none() {
                self.opus = opus::Encoder::new(self.rate);
            }
            if let Some(pkt) = self.opus.as_mut().and_then(|e| e.encode(pcm)) {
                return pkt;
            }
        }
        // Binary fast-path: [0x02, rate_khz, ...pcm_bytes] (even total length)
        let mut pkt = Vec::with_capacity(2 + pcm.len());
        pkt.extend_from_slice(&[crate::protocol::binary::AUDIO_MARKER, self.rate_tag_khz]);
        pkt.extend_from_slice(pcm);
        pkt
    }
}

/// Splits a received PCM voice packet (`[0x02, rate_khz, pcm...]` or legacy `[0x02, pcm...]` at 16 kHz) into
/// (rate_hz, pcm).
fn pcm_packet(bin: &[u8]) -> (u32, &[u8]) {
    // Even total length with valid rate_khz tag (16, 24, 48) -> new header format
    if bin.len().is_multiple_of(2) && (bin[1] == 16 || bin[1] == 24 || bin[1] == 48) {
        ((bin[1] as u32) * 1000, &bin[2..])
    } else {
        (16000, &bin[1..])
    }
}

/// Receive side shared by both backends: PCM packets pass through, Opus packets go through one decoder (only
/// the focused session's voice plays, so there is a single stream). Relay and P2P threads both deliver here.
pub(crate) struct VoiceReceiver {
    opus: Mutex<Option<opus::Decoder>>,
}

impl VoiceReceiver {
    pub(crate) fn new() -> Self {
        Self { opus: Mutex::new(None) }
    }

    /// Hands every (rate_hz, pcm16le) chunk in voice packet `bin` (longer than 2 bytes) to `play`.
    pub(crate) fn receive(&self, bin: &[u8], mut play: impl FnMut(u32, Vec<u8>)) {
        let Some((seq, payload)) = opus::parse(bin) else {
            let (rate, pcm) = pcm_packet(bin);
            if !pcm.is_empty() {
                play(rate, pcm.to_vec());
            }
            return;
        };
        let mut dec = self.opus.lock();
        if dec.is_none() {
            *dec = opus::Decoder::new();
        }
        if let Some(dec) = dec.as_mut() {
            for pcm in dec.decode(seq, payload) {
                play(opus::DECODE_RATE, pcm);
            }
        }
    }
}

/// VoIP DSP pipeline: High-Pass DC/rumble filter + Smooth Noise Gate + Soft-Knee Limiter (Zero Hard-Clipping)
pub(crate) struct VoiceDspState {
    prev_in: f32,
    prev_out: f32,
    env: f32,
}

impl VoiceDspState {
    pub(crate) fn new() -> Self {
        Self { prev_in: 0.0, prev_out: 0.0, env: 0.0 }
    }

    pub(crate) fn process_pcm_16le(&mut self, raw: &[u8], out: &mut [u8]) {
        let n = raw.len().min(out.len()) & !1;
        let mut i = 0;
        while i < n {
            let s = i16::from_le_bytes([raw[i], raw[i + 1]]) as f32 / 32768.0;

            // 1. 1st-order high-pass filter (~85Hz cutoff) removes DC offset, fan rumble & mic pops
            let hp = 0.985 * (self.prev_out + s - self.prev_in);
            self.prev_in = s;
            self.prev_out = hp;

            // 2. Envelope follower for smooth noise gate (suppresses idle mic hiss)
            let abs_x = hp.abs();
            if abs_x > self.env {
                self.env = 0.80 * self.env + 0.20 * abs_x; // fast attack
            } else {
                self.env = 0.995 * self.env + 0.005 * abs_x; // smooth release
            }

            let gate_gain = if self.env < 0.004 { (self.env / 0.004).powi(2) } else { 1.0 };

            // 3. Clean 1.65x vocal presence pre-gain + Padé soft-knee tanh limiter (never hard-clips!)
            let x = hp * gate_gain * 1.65;
            let limited = if x >= 1.5 {
                0.96
            } else if x <= -1.5 {
                -0.96
            } else {
                let x2 = x * x;
                x * (27.0 + x2) / (27.0 + 9.0 * x2)
            };

            let sample_out = (limited * 32760.0).clamp(-32768.0, 32767.0) as i16;
            let bytes = sample_out.to_le_bytes();
            out[i] = bytes[0];
            out[i + 1] = bytes[1];
            i += 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_plays_pcm_as_sent_and_opus_at_48khz() {
        let rx = VoiceReceiver::new();
        let mut got = Vec::new();
        rx.receive(&[0x02, 24, 1, 0, 2, 0], |rate, pcm| got.push((rate, pcm.len())));
        rx.receive(&[0x02, 1, 0, 2, 0], |rate, pcm| got.push((rate, pcm.len())));
        let mut enc = opus::Encoder::new(16_000).unwrap();
        rx.receive(&enc.encode(&[0u8; 640]).unwrap(), |rate, pcm| got.push((rate, pcm.len())));
        assert_eq!(got, [(24_000, 4), (16_000, 4), (48_000, 1920)]);
    }
}
