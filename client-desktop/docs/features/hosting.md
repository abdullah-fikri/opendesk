# Hosting

Code: `src/service/host/` (`mod.rs` lifecycle, `capture.rs`, `input.rs`), `src/infra/privacy/`.

A desktop host accepts every session the relay grants, without a prompt (unattended access). Linux hosts need an
X11 session: in a Wayland session the client registers as `viewer-only`, explains it under the device ID and ends
any host session it is handed ([platforms](../../../docs/platforms.md#wayland)). One viewer at a
time controls the machine; the window shows who, with microphone and disconnect controls, and the tray can end
it.

## Capture loop

One thread per hosted session (`capture.rs`):

- Captures the selected monitor with DXGI Desktop Duplication (GDI as fallback), following the secure desktop
  (UAC, lock screen) when the process runs as SYSTEM under the service
  ([autostart](autostart-and-cli.md#windows-service)); on Linux with X11 MIT-SHM, paced by the loop.
- Encodes with `core_engine::stream::StreamEncoder`: plain JPEG for viewers without `video-delta`, otherwise key
  and delta frames, plus H.264 for motion when the viewer supports it.
- Paces at the viewer's cap (`fps-N` or `set-performance`, 60 without one), at most the host monitor's refresh
  rate; above 60 only with a hardware H.264 encoder, on any path (P2P, UDP relay or WebSocket relay;
  `OPENDESK_WS_HIGH_FPS=0` keeps the WebSocket relay at 60). Encodes only while the previous frame has left the
  socket (or the P2P path acknowledged it); the wait feeds a congestion level that first steps the rate down
  (144 → 120 → 90 → 60), then lowers quality and bitrate.
- Follows the viewer's performance preset (`perf-*` or `set-performance`, Medium without one): the H.264 bitrate
  is halved on Low and doubled on High, and on High with a hardware encoder motion switches to H.264 at once.
  A preset change applies on the next frame.
- While its video goes over the WebSocket relay, the relay socket's send buffer shrinks to 512 KiB
  (`OPENDESK_WS_SNDBUF`), so frames cannot pile up in an auto-tuned kernel buffer of several megabytes and
  congestion shows sooner; 512 KiB still carries about 17 Mbps at 234 ms RTT. Afterwards it is set to 4 MiB
  for file transfers.
- Rebuilds the pipeline when the viewer switches monitor or resolution; re-checks the monitor list every 2 s and
  announces changes (`monitors`), and announces the cursor shape when the viewer draws its own.
- Releases every key the viewer left pressed when the session ends.

## Input

`input.rs` applies events only while a session is hosted, on the input desktop like the capture thread:
pointer events are mapped onto the streamed monitor,
keys are injected with `SendInput` (XTEST on Linux) and tracked so none stays stuck, text is typed as Unicode, and `clipboard` sets
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

Both are Windows-only. Linux hosts advertise `host-features` without `privacy-screen` and `block-input`, so the
viewer's Privacy menu is hidden for them; a request from an older viewer is logged and ignored.
