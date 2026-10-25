//! Voice call audio: microphone capture into binary voice packets (`[0x02][rate_khz][pcm16le]`) and playback of
//! received packets, with a light DSP stage (high-pass, noise gate, soft limiter) on captured audio. The device
//! backend is selected at compile time: waveIn/waveOut on Windows, cpal (ALSA, which PipeWire and PulseAudio
//! serve) elsewhere.

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
