# Hosting

Code: `src/service/host/` (`mod.rs` lifecycle, `capture.rs`, `input.rs`), `src/infra/privacy.rs`.

A Windows host accepts every session the relay grants, without a prompt (unattended access). One viewer at a
time controls the machine; the window shows who, with microphone and disconnect controls, and the tray can end
it.

## Capture loop

One thread per hosted session (`capture.rs`):

- Captures the selected monitor with DXGI Desktop Duplication (GDI as fallback), following the secure desktop
  (UAC, lock screen) when the process may.
- Encodes with `core_engine::stream::StreamEncoder`: plain JPEG for viewers without `video-delta`, otherwise key
  and delta frames, plus H.264 for motion when the viewer supports it.
- Paces at 60 FPS and encodes only while the previous frame has left the socket (or the P2P path acknowledged
  it); the wait feeds a congestion level that lowers quality.
- Rebuilds the pipeline when the viewer switches monitor or resolution; re-checks the monitor list every 2 s and
  announces changes (`monitors`), and announces the cursor shape when the viewer draws its own.
- Releases every key the viewer left pressed when the session ends.

## Input

`input.rs` applies events only while a session is hosted: pointer events are mapped onto the streamed monitor,
keys are injected with `SendInput` and tracked so none stays stuck, text is typed as Unicode, and `clipboard` sets
the local clipboard right before a forwarded Ctrl+V. A viewer may mute this machine's microphone, never unmute it.

## Privacy

| Mode | On the host's physical screen |
| --- | --- |
| Normal | Nothing |
| Frozen | The screen as it was when enabled |
| Maintenance | "System Maintenance in Progress" |
| Black | Black |

The overlay is excluded from capture, so the viewer keeps seeing the real desktop. "Block input" ignores the
physical keyboard and mouse while injected input still works. Both reset when the session ends.
