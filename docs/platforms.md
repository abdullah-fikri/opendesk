# Platforms

What each OpenDesk client can do, and why the gaps exist. Peers advertise per-OS capabilities at registration
([protocol.md](protocol.md#capabilities)), so a viewer only offers controls the host actually has, and nobody can
connect to a device that cannot be controlled.

## Roles

| Platform | View other devices | Be controlled (host) | Packages |
| --- | --- | --- | --- |
| Windows 10/11 | Yes, up to 4 hosts | Yes, unattended | `.exe` (portable), Inno Setup installers: Standard, Admin, Headless host |
| Linux, X11 session | Yes, up to 4 hosts | Yes, unattended | `.deb` (`opendesk`, `opendesk-admin`, `opendesk-host`), `.tar.gz` |
| Linux, Wayland session | Yes, up to 4 hosts | No: registers as `viewer-only` and says why under its device ID | same |
| Android 7+ | Yes, one host | Never: viewer only, registers as `viewer-only` | `.apk`, `.aab` |
| macOS | Not supported (the desktop client may compile; nothing is tested or packaged) | | |

Linux was verified on Debian 12 and is built on Ubuntu 22.04 in CI; any distribution with glibc 2.35+, X11 or
XWayland and PipeWire/PulseAudio/ALSA should work.

## Desktop features

| Feature | Windows | Linux (X11) | Notes for Linux |
| --- | --- | --- | --- |
| Screen capture | DXGI Desktop Duplication, GDI fallback | X11 MIT-SHM, plain `GetImage` on remote displays | Wayland: see below |
| Multi-monitor, monitor picker | Yes | Yes (RandR 1.5) | |
| Cursor shape on the viewer | Yes | Yes (XFixes cursor names) | |
| Delta video (JPEG regions) | Yes (GDI+ JPEG) | Yes (pure-Rust JPEG) | |
| H.264 for motion | Media Foundation, hardware encoder when present | Cisco OpenH264 2.6.0, software, **when installed** | Run `install-openh264.sh` (shipped in the packages). Without it Linux hosts and viewers use JPEG only and do not advertise `video-h264`. |
| Mouse and keyboard injection | `SendInput` | XTEST | Characters missing from the layout are typed through a temporary keymap entry |
| System shortcuts (Win, Alt+Tab, ...) | Yes | Sent as keys (Super, Alt+Tab, ...) | What they do depends on the desktop environment |
| Following UAC / lock screen | Yes (with the `OpenDeskService` service the installer creates) | n/a | Needs a logged-on user; not the logon screen after a reboot |
| Privacy screen (frozen, maintenance, black) | Yes | **No** | Needs a capture-excluded overlay, which X11 has no equivalent for. Linux hosts advertise `host-features` without `privacy-screen`, so viewers hide the option. |
| Block the host's physical input | Yes | **No** | Would need exclusive grabs that also block XTEST. Hidden the same way (`block-input`). |
| Viewer: forward Win key / Alt+Tab | Yes (low-level keyboard hook: Win, Win+key, Alt+Tab/Esc/F4/Space, Ctrl+Esc, Ctrl+Shift+Esc, PrintScreen; no shortcut menu) | No | The local desktop keeps these keys; use the toolbar's shortcut menu |
| Voice (16/24/48 kHz) | waveIn/waveOut | cpal on ALSA (PipeWire and PulseAudio serve it) | |
| Clipboard text sync | Yes | Yes (X11; Wayland compositors with data-control) | |
| Copy files, paste on the other side (pull on paste) | Yes (Explorer virtual files) | No: `file-offer` is not advertised | Partners send copied files as normal transfers |
| Send files: picker | Yes | Yes (`zenity` or `kdialog`) | |
| Send files: drag and drop onto the window | Yes | No | |
| Receive files | `Downloads\OpenDesk` | `~/Downloads/OpenDesk` | Auto-paste into the focused file manager is Windows-only |
| Direct encrypted P2P path | Yes | Yes | |
| Tray icon | Yes | StatusNotifierItem (KDE, XFCE, Cinnamon, MATE, GNOME with the AppIndicator extension) | Without a tray host the app keeps running; launching it again shows the window |
| Start at login | SYSTEM service (installer), elevated logon task (fallback) or `HKCU\...\Run` + watchdog | `/etc/xdg/autostart` (packages) or `~/.config/autostart` (portable) | No watchdog |
| Single instance; a second launch shows the window | Named mutex | Lock file + Unix socket | |
| `opendesk` CLI | Yes (`opendesk.cmd` on `PATH`) | Yes (`/usr/bin/opendesk` from the packages) | `stop` asks the running app to exit over its socket |
| Settings and log | `%APPDATA%\OpenDesk` | `~/.config/opendesk` (`$XDG_CONFIG_HOME`) | |
| Rendering | OpenGL (femtovg) | OpenGL, software fallback | Works on VMs and remote sessions without a GPU |

### Wayland

Capturing a Wayland desktop needs the xdg-desktop-portal ScreenCast API with PipeWire, and injecting input needs
the RemoteDesktop portal or `uinput`; both require user confirmation flows that do not exist yet in OpenDesk. A
Linux desktop in a Wayland session therefore:

- works fully as a **viewer** (the window runs through XWayland or natively, depending on the toolkit backend);
- registers with the `viewer-only` capability, so the relay refuses connect requests to it with "can only view
  other devices";
- shows under its device ID: *Wayland host capture is not supported yet. To let others control this computer, sign
  out and choose an X11 session ("Ubuntu on Xorg", "GNOME on Xorg" or "Plasma (X11)").*

Detection: `XDG_SESSION_TYPE=wayland`, or `WAYLAND_DISPLAY` without `DISPLAY`.

## Android

The Android app is a viewer only; see [client-android/README.md](../client-android/README.md) for gestures, the
permission list and why it no longer asks for Accessibility or screen capture. It advertises `viewer-only`,
receives plain JPEG frames (no video capabilities), sends text and Win32 key codes, can talk over voice and can
receive files.
