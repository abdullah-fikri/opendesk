# Voice

Code: `src/infra/audio/` (DSP in `mod.rs`; devices: `win.rs` waveIn/waveOut, `unix.rs` cpal on ALSA, which
PipeWire and PulseAudio serve), routing in `src/service/relay/connection.rs`.

- Two-way voice between session partners. Muting your own microphone also mutes the partner's (`set-mic`);
  that path never unmutes.
- The viewer can turn the host's microphone on and off with the session toolbar's **Host mic** button
  (`set-host-mic`). The host reports its state (`host-mic`) at session start and on every change. Both sides
  list `remote-mic`; with a host up to 1.2.x the button is hidden.
- Microphone when a session starts: Settings › This device › Audio › Microphone (On / Off), stored as
  `mic_on_at_start` in `config.json`. It applies to every new session on this machine, as host and as viewer,
  and sets only the initial state: the microphone button still changes it per session. Unset means on in every
  edition. The Host edition has no window, so it is turned off there by writing `"mic_on_at_start": false` to
  `config.json`; the Standard edition has no Settings page yet and uses the same file.
- Quality per session: 16 kHz, 24 kHz or 48 kHz mono, 16-bit PCM (`set-audio-quality`, either side).
- Captured audio passes a high-pass filter (about 85 Hz), a noise gate and a soft limiter before it is sent as
  `[0x02][rate_khz][pcm]`.
- A viewer of several hosts plays and talks to the focused host only.
- The capture device opens only while a session is active.
