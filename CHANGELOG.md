# Changelog

All notable changes to OpenDesk. Versions follow [Semantic Versioning](https://semver.org); the single source of
truth is `VERSION`. The release workflow copies the section of the tagged version into the GitHub Release.

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

[1.0.2]: https://github.com/abdullah-fikri/opendesk/releases/tag/v1.0.2
[1.0.1]: https://github.com/abdullah-fikri/opendesk/releases/tag/v1.0.1
[1.0.0]: https://github.com/abdullah-fikri/opendesk/releases/tag/v1.0.0
