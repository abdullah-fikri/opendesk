# Changelog

All notable changes to OpenDesk. Versions follow [Semantic Versioning](https://semver.org); the single source of
truth is `VERSION`. The release workflow copies the section of the tagged version into the GitHub Release.

## [1.2.1] - 2026-10-29

### Added
- Performance presets: one "Performance" setting (Low / Medium / High) replaces the frame-rate picker in the relay
  settings dialog. Low streams 30 FPS at half the H.264 bitrate, Medium (the default for new installs) 60 FPS as
  before, High follows the viewer's monitor refresh rate at twice the bitrate and switches motion to H.264 at
  once. The congestion governor still lowers rate and bitrate on every preset. A changed preset applies to
  running sessions without reconnecting (`set-performance`); older hosts keep using the `fps-N` cap. Existing
  settings carry over: 30 FPS becomes Low, 60 Medium, Auto and Maximum High.
- Latency in the session stats overlay: Host, Network, Decode, Display and Total per frame, plus CPU and RAM of
  the viewer and the host process. Hosts stamp frames only for viewers that list `frame-stamp`; with older hosts
  those rows show "–".

### Changed
- "Host grab" in the stats overlay counts capture work only; waiting for the desktop to change is logged as
  `wait=` in `host.perf`.
- The relay keeps up to 32 client capabilities instead of 16.
- Above 60 FPS now works over the WebSocket relay too, not only over P2P or the UDP relay; a hardware H.264
  encoder on the host is still required. `OPENDESK_WS_HIGH_FPS=0` restores the old rule.
- While a hosted session's video goes over the WebSocket relay, the socket send buffer is 512 KiB
  (`OPENDESK_WS_SNDBUF`), so frames no longer queue in a multi-megabyte kernel buffer on a slow link.
- Saving the relay settings dialog with an unchanged address no longer reconnects (and ends running sessions).

## [1.2.0] - 2026-10-29

### Added
- Android: end-to-end encryption with desktop hosts (the same Noise_XX protocol as the desktop). Video, voice and
  input are sealed; the host's key is pinned on the first connection and a changed key closes the session with a
  dialog that can trust the new key. No touch or keystroke leaves the phone before the encryption is up.
- Android: framed key/delta video (`video-delta`), needed for the encryption and lighter on the network.
- `opendesk update [latest|<version>|status]`: install the newest release or a chosen one (newer or older; it then
  stays on it), and show the update state. `opendesk rollback`, `pin`, `unpin` and `version` are commands now;
  the older `--update-now`, `--rollback`, `--pin-version` and `--unpin` still work.
- Auto-update is live: the release signing key is set, so Windows installs from this version on update
  themselves from signed manifests.
- `SECURITY.md` (private vulnerability reports), `CLA.md` with a CLA check on pull requests, `MAINTAINERS.md`.

### Changed
- A host whose key was pinned but that no longer offers end-to-end encryption now closes the session (like a
  changed key) instead of only warning; `opendesk trust <id>` accepts it.

### Fixed
- `opendesk help` and every other command printed its output twice in a terminal.
- A mistyped command (`opendesk updte`) started the app silently; it now prints the help.
- `opendesk help` no longer prints the device status; `opendesk status` does.

## [1.1.3] - 2026-10-28

### Added
- UDP relay: when two clients cannot reach each other directly (networks that drop UDP from unknown addresses),
  the direct path runs through the relay's UDP forwarder instead of the WebSocket relay. No router settings
  needed; the datagrams stay end-to-end encrypted and the direct route still wins when it works. Enabled on the
  relay with `OPENDESK_UDP_RELAY_PUBLIC` (needs a public UDP port, e.g. a VPS); the session overlay shows
  `Relay (UDP)`. `OPENDESK_UDP_RELAY=0` on a client turns it off.

### Fixed
- P2P candidates are no longer lost when the relay sits behind a tunnel: the relay accepts short bursts of
  signals and clients send their candidates again while probing.
- STUN retries lost requests and falls back to a second server, so a single lost datagram no longer costs the
  public candidate.
- A host without a microphone no longer logs a warning every 500 ms.
- Logs show the NAT type, the partner's candidates and how many datagrams arrived, to diagnose failed P2P paths.

## [1.1.2] - 2026-10-28

### Added
- End-to-end encryption: when both sides run this version, video, voice, input, clipboard and file chunks are
  encrypted between the two devices (Noise_XX); the relay forwards them without being able to read them. The
  host's key is pinned on the first connection and a changed key closes the session with a warning
  (`opendesk trust <id>` accepts it, e.g. after a reinstall). `opendesk status` shows this device's fingerprint.
  Sessions with older versions keep working, unencrypted end to end. `OPENDESK_E2E=0` turns it off.
- Frame rates above 60: a viewer asks for its monitor's refresh rate (server settings dialog: Auto / 30 / 60 / Maximum) and a
  host on a 120/144/240 Hz monitor streams up to that rate over a direct (P2P) path with a hardware H.264
  encoder. Relayed sessions, software encoders and older partners stay at 60. Under congestion the rate steps
  down (144 → 120 → 90 → 60) before quality drops. `OPENDESK_MAX_FPS=60` restores the 60 FPS limit.
- Session stats overlay: RTT, path, encryption, bitrate, shown vs. target frame rate, decode time, the host's
  encoder and capture/encode times, dropped frames and congestion.
- Opus voice (about 30 kbps instead of 768 kbps raw PCM) when both sides support it.
- Auto-update infrastructure for the Windows service: signed manifests, verified downloads into a folder only
  SYSTEM and Administrators can write, a health check that rolls a failed update back, and
  `opendesk --update-now`, `--rollback`, `--pin-version`, `--unpin`. Not active in this release: no release
  signing key is configured yet, so no update is ever installed.

### Changed
- The installer runs in 64-bit mode and installs to `Program Files`; 1.0.x installs in `Program Files (x86)`
  are migrated automatically (custom folders are kept). Device ID, settings and history are unchanged.
- File transfers send 128 KiB binary chunks instead of base64 JSON when partner and relay support it.
- The relay writes to each client from one queue with priorities (input and control first, then voice, video,
  files), so a slow receiver no longer holds up its sender.
- Above 60 FPS, H.264 takes over any change bigger than typing at once; typing stays on sharp JPEG regions.

### Performance
- Idle in the tray: the relay connection loop backs off and Windows runs the process under EcoQoS while no
  session is active, far fewer wakeups.
- Hardware H.264 above 60 FPS no longer waits for each frame's own output (one frame more latency), and the
  colour conversion uses AVX2 where available: about 3 ms per frame instead of 11 ms on Intel Quick Sync.
  The 60 FPS path also gets the faster conversion.
- Viewer: the decoder wakes on new frames instead of polling, frames are coalesced while the UI is busy and
  their buffers reused, and only changed session fields are written per frame.
- The host waits on the video gate instead of polling it every millisecond.
- Release binary built with fat LTO and stripped (about 15 MB instead of 35 MB).

### Fixed
- `opendesk.trace` no longer grows without bound: it rotates at 10 MB.
- `config.json` and `history.json` are written atomically, so a crash or power loss cannot leave them broken.
- A panic is logged with its thread and backtrace; a crashed capture, decoder or relay thread ends its session
  cleanly (keys released on the host) or restarts, instead of leaving a frozen session. Locks no longer poison.
- Clipboard text no longer overtakes the sender's queued file messages through the relay.
- The tray icon is loaded from the exe's own resource.

## [1.0.2] - 2026-10-26

### Fixed
- Windows hosts no longer grow by about 60 MB every time the stream is rebuilt (viewer window resized,
  resolution changed): hardware H.264 encoders are now shut down when replaced.
- Ctrl+V of files copied before the session (or before the host had focus) did nothing; they are now sent and
  pasted like a drop.
- An auto-paste no longer pastes an older transfer when the host's clipboard was busy: the clipboard is retried,
  checked, and given time to reach Explorer before Ctrl+V.
- The service's SYSTEM agent no longer advertises clipboard file offers, which the user's Explorer could not
  pull from; partners send copied files as normal transfers instead.
- Keys reach the host as a physical keyboard would send them (scan code, extended-key flag), and the viewer's
  keyboard hook is reinstalled for every session.
- Headless edition: a Start Menu launch with the service installed no longer waits 20 s for a tray window.
- Copy and paste from the PC being controlled to the viewer (text, files and folders) did nothing when OpenDesk
  runs through the service: Windows lets the SYSTEM agent see that the clipboard changed but not what is on it.
  The agent now reads the clipboard through `OpenDesk.exe --clipboard-read`, started as the logged-on user
  (about 0.1 s per copy). The same applied to Ctrl+V of text on a viewer running through the service.
- Win and Alt+Tab from keyboards whose keys pass through a keyboard tool (PowerToys, AutoHotkey, laptop hotkey
  utilities) stayed on the viewer's PC; they are forwarded too now.
- Copied text could be lost when another program (Windows clipboard history, a clipboard manager) held the
  clipboard at that moment; reading and writing it is now retried, and a failure is logged.
- Win, Alt+Tab and the other system shortcuts never reached the host on Windows viewers: while the OpenDesk
  window was in front, Windows skipped every low-level keyboard hook because the UI toolkit registers the
  keyboard for raw input. OpenDesk now drops that registration (typing is unaffected), and the hook runs on its
  own thread and recognises OpenDesk's window by process, not by a handle looked up by title.
- The pointer disappeared over the remote screen after Win, Alt+Tab or typing on the host: Windows hides the
  host's cursor for keyboard use and only shows it again on mouse input, which moving the cursor did not count
  as. The host now sends mouse input with every move, and the viewer always shows its pointer over the remote
  screen.
- `docker-compose.yml` pulls the relay image of a release (`opendesk-relay:1.0.1`) instead of `latest`;
  `RELAY_VERSION` in `.env` picks another one.

### Changed
- The log shows clipboard text sent and received (length only, never the content) and why a Win / Alt+Tab
  press stayed on the viewer's PC.

## [1.0.1] - 2026-10-26

### Added
- Windows: the installer sets up the `OpenDeskService` service, which runs OpenDesk as SYSTEM in the logged-on
  user's session. Viewers can now see and click the UAC prompt and unlock the lock screen, as if they sat at the
  machine. Settings stay in the user's profile; the elevated logon task remains as the fallback.
- A "File transfers" panel shows every sent and received file with a progress bar, size, speed and the
  result. Pressing Ctrl+V again while the same files are still on their way no longer sends them twice.

### Known limitations
- The logon screen after a reboot or sign-out, Remote Desktop sessions and Ctrl+Alt+Del are still not reachable.
- With the service, the OpenDesk window on the host runs as SYSTEM: install it only where you trust the local
  users.

## [1.0.0] - 2026-10-25

First public release.

### Clients
- Desktop client for **Windows** and **Linux** (X11; Wayland sessions are viewer-only) in three editions:
  standard, admin and headless host. Installers for Windows, `.deb` packages and a `.tar.gz` for Linux.
- **Android** app as a viewer: one host at a time with touch gestures, zoom and a soft keyboard. The app cannot be
  controlled and asks only for internet access and, when the microphone is switched on, audio recording.
- **Multi-session**: up to four hosts at once from one desktop viewer, as tabs or a split grid.
- Capture with DXGI (GDI fallback) on Windows and X11/MIT-SHM on Linux; changed regions as JPEG atlases, H.264
  for motion (Media Foundation on Windows, Cisco OpenH264 on Linux when installed); stream size follows the
  viewer window.
- Direct encrypted UDP path between desktops (Noise protocol) with automatic fallback to the relay.
- Two-way voice, clipboard text sync, file transfer, multi-monitor picker, cursor shape mirroring.
- Windows hosts: privacy screen, blocking the host's physical input, unattended start at logon with a watchdog.
- Windows viewers forward system keys straight from the keyboard (Win, Win+key, Alt+Tab, Alt+Esc, Alt+F4,
  Alt+Space, Ctrl+Esc, Ctrl+Shift+Esc, PrintScreen); Linux viewers keep a shortcut menu.

### Administration (admin edition)
- **Monitor wall**: a read-only CCTV-style grid (2×2, 3×3, 4×4) of every online host, with paging, automatic
  page rotation and full screen. Thumbnails are streamed only for the visible page and only when the screen
  changes; watching never takes one of the four session slots.
- **Client token**: an allowlist of devices on the relay (Settings › Security). Listed devices receive their token
  automatically on their first connection (trust on first use); tokens can be reset or revoked.
- **Device names** set by administrators, stored on the relay and shown everywhere.

### Relay
- Single Go binary backed by PostgreSQL; schema migrations run automatically at start (`0001`-`0003`).
- Docker image `ghcr.io/abdullah-fikri/opendesk-relay` for `linux/amd64` and `linux/arm64`.
- Optional per-device credentials (`OPENDESK_DEVICE_CREDENTIALS`), token-protected `/metrics`, `OPENDESK_P2P_DISABLED`.

### Security notes
- With neither the client token nor `OPENDESK_DEVICE_CREDENTIALS` enabled the relay is open: any client may claim
  the administrator role. Turn one of them on before exposing a relay to the internet
  (see `docs/features/device-security.md`).
- When an administrator changes a connected device's role, the new role applies from that device's next
  connection.

### Known limitations
- Hosts on Wayland, macOS and Android cannot be controlled.
- Ctrl+Alt+Del, the UAC prompt and the lock screen are reachable only when the host runs as SYSTEM; no Windows
  service that does this ships yet.
- Windows binaries are not code-signed yet (SmartScreen warns on first run); Linux packages are unsigned `.deb`
  files. Verify downloads against `SHA256SUMS.txt`.

[1.2.1]: https://github.com/abdullah-fikri/opendesk/releases/tag/v1.2.1
[1.2.0]: https://github.com/abdullah-fikri/opendesk/releases/tag/v1.2.0
[1.1.3]: https://github.com/abdullah-fikri/opendesk/releases/tag/v1.1.3
[1.1.2]: https://github.com/abdullah-fikri/opendesk/releases/tag/v1.1.2
[1.0.2]: https://github.com/abdullah-fikri/opendesk/releases/tag/v1.0.2
[1.0.1]: https://github.com/abdullah-fikri/opendesk/releases/tag/v1.0.1
[1.0.0]: https://github.com/abdullah-fikri/opendesk/releases/tag/v1.0.0
