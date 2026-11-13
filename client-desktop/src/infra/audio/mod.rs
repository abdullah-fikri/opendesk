//! Session audio: voice call (microphone capture into binary voice packets, `[0x02][rate_khz][pcm16le]` or Opus,
//! see `opus`, with a light DSP stage: high-pass, noise gate, soft limiter) and host sound (`audio-system`: the
//! host's speaker output, captured by loopback and sent as stereo Opus), plus playback of both. A session runs
//! one of them at a time ([`SessionAudio`]). The device backend is selected at compile time: waveIn/waveOut and
//! WASAPI loopback on Windows; cpal (ALSA, which PipeWire and PulseAudio serve) elsewhere, without host sound.

use std::sync::atomic::{AtomicU8, Ordering};

use parking_lot::Mutex;

pub mod opus;

#[cfg(any(windows, test))]
mod host_sound;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod unix;
#[cfg(not(windows))]
pub use unix::*;

/// What the current session's audio carries. Host sound and voice alternate: while host sound flows, the
/// microphone stays closed and voice packets are dropped, so the viewer's voice is never played on the host
/// while its output is captured (no echo). See `docs/protocol.md#host-sound`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAudio {
    /// Two-way voice (microphones per their own mute state).
    Voice = 0,
    /// Viewer: play the focused host's sound.
    PlayHostSound = 1,
    /// Host: capture this machine's speaker output and send it to the viewer.
    SendHostSound = 2,
}

/// [`SessionAudio`] shared between the services and the audio threads.
#[derive(Debug, Default)]
pub struct AudioMode(AtomicU8);

impl AudioMode {
    pub fn get(&self) -> SessionAudio {
        match self.0.load(Ordering::Relaxed) {
            1 => SessionAudio::PlayHostSound,
            2 => SessionAudio::SendHostSound,
            _ => SessionAudio::Voice,
        }
    }

    pub fn set(&self, mode: SessionAudio) {
        self.0.store(mode as u8, Ordering::Relaxed);
    }
}

/// The audio mode of a session: host sound when this build has it, the partner listed `audio-system` and the
/// viewer wants it (`on`), otherwise a voice call. `hosting` says which side of the session this machine is.
pub fn session_audio(hosting: bool, peer_audio_system: bool, on: bool) -> SessionAudio {
    match (HOST_SOUND && peer_audio_system && on, hosting) {
        (false, _) => SessionAudio::Voice,
        (true, true) => SessionAudio::SendHostSound,
        (true, false) => SessionAudio::PlayHostSound,
    }
}

/// What a received `0x02` packet is for in the current mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Voice,
    HostSound,
    Drop,
}

/// SECURITY: a peer decides only the packet's bytes, not what this side plays: voice plays only in a voice call,
/// host sound only on a viewer that asked for it, everything else is dropped.
fn route(bin: &[u8], mode: SessionAudio) -> Route {
    // Any packet tagged as host sound (even a truncated one) is never voice, so it can never be played as PCM.
    let host_sound = bin.get(1) == Some(&opus::HOST_SOUND_TAG);
    match mode {
        SessionAudio::Voice if !host_sound => Route::Voice,
        SessionAudio::PlayHostSound if host_sound => Route::HostSound,
        _ => Route::Drop,
    }
}

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

    #[test]
    fn voice_and_host_sound_never_play_together() {
        let voice = [0x02, 48, 1, 0, 2, 0];
        let opus_voice = [0x02, opus::OPUS_TAG, 0, 1, 9];
        let sound = [0x02, opus::HOST_SOUND_TAG, 0, 1, 9];
        assert_eq!(route(&voice, SessionAudio::Voice), Route::Voice);
        assert_eq!(route(&opus_voice, SessionAudio::Voice), Route::Voice);
        assert_eq!(route(&sound, SessionAudio::Voice), Route::Drop, "host sound needs the viewer's consent");
        // A truncated host sound packet must not be taken for legacy 16 kHz PCM.
        assert_eq!(route(&[0x02, opus::HOST_SOUND_TAG, 0, 1], SessionAudio::Voice), Route::Drop);
        assert_eq!(route(&sound, SessionAudio::PlayHostSound), Route::HostSound);
        assert_eq!(route(&voice, SessionAudio::PlayHostSound), Route::Drop);
        // A capturing host plays nothing, so its output never carries the viewer's voice back.
        for pkt in [&voice[..], &opus_voice, &sound] {
            assert_eq!(route(pkt, SessionAudio::SendHostSound), Route::Drop);
        }
    }

    #[test]
    fn host_sound_needs_the_peer_and_the_viewer() {
        // An old peer (no `audio-system`) keeps the voice call whatever the toggle says.
        assert_eq!(session_audio(true, false, true), SessionAudio::Voice);
        assert_eq!(session_audio(false, false, true), SessionAudio::Voice);
        // The viewer switched host sound off: voice call on both sides.
        assert_eq!(session_audio(true, true, false), SessionAudio::Voice);
        assert_eq!(session_audio(false, true, false), SessionAudio::Voice);
        let (host, viewer) = if HOST_SOUND {
            (SessionAudio::SendHostSound, SessionAudio::PlayHostSound)
        } else {
            (SessionAudio::Voice, SessionAudio::Voice)
        };
        assert_eq!(session_audio(true, true, true), host);
        assert_eq!(session_audio(false, true, true), viewer);
    }

    #[test]
    fn audio_mode_round_trips() {
        let mode = AudioMode::default();
        assert_eq!(mode.get(), SessionAudio::Voice);
        for m in [SessionAudio::PlayHostSound, SessionAudio::SendHostSound, SessionAudio::Voice] {
            mode.set(m);
            assert_eq!(mode.get(), m);
        }
    }
}
