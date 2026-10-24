# Voice

Code: `src/infra/audio/` (DSP in `mod.rs`; devices: `win.rs` waveIn/waveOut, `unix.rs` cpal on ALSA, which
PipeWire and PulseAudio serve), routing in `src/service/relay/connection.rs`.

- Two-way voice between session partners. The microphone is on when a session starts; either side can mute the
  other, but only the local user can unmute.
- Quality per session: 16 kHz, 24 kHz or 48 kHz mono, 16-bit PCM (`set-audio-quality`, either side).
- Captured audio passes a high-pass filter (about 85 Hz), a noise gate and a soft limiter before it is sent as
  `[0x02][rate_khz][pcm]`.
- A viewer of several hosts plays and talks to the focused host only.
- The capture device opens only while a session is active.
