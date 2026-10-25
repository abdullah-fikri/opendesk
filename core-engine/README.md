# core-engine

Library with the low-level platform building blocks of the desktop client, for Windows and Linux. It knows
nothing about the network, the UI or sessions. Every module has one public API and a backend per OS selected
with `cfg` (static dispatch).

| Module | Contents |
| --- | --- |
| `capture` | `DesktopCapturer`: Windows: DXGI Desktop Duplication (dirty and move rectangles, cursor overlay) with GDI fallback. Linux: X11 MIT-SHM (plain `GetImage` on remote displays) with an XFixes cursor overlay. Monitor enumeration (RandR on Linux), cursor shape, `host_support` (Wayland sessions cannot host) |
| `diff` | Exact block-level change detection between frames |
| `codec` | Framed key/delta video (`frame`, `delta`), H.264 (`h264`: Media Foundation on Windows, Cisco's OpenH264 loaded at runtime on Linux), JPEG (`jpeg`: GDI+ on Windows, `jpeg-encoder` elsewhere), base64 |
| `stream` | `StreamEncoder`: per tick decides between nothing, a full JPEG, a key frame, a delta or H.264, and the JPEG quality under congestion |
| `input` | Mouse and keyboard injection (`SendInput` / XTEST); key codes are Win32 virtual keys everywhere |
| `clipboard` | Clipboard text (both) and file lists (Windows) |
| `desktop` | Attaching capture threads to the input desktop (Winlogon, UAC); a no-op on Linux |

## Video pipeline

```text
capture (DXGI dirty rects / GDI) ──▶ diff (32 px blocks) ──▶ stream encoder
   ├─ nothing changed            ──▶ nothing, or a keep-alive
   ├─ viewer without video-delta ──▶ full JPEG
   ├─ small changes              ──▶ delta: changed blocks packed into one JPEG atlas + move rects
   ├─ sustained motion           ──▶ H.264 (hardware encoder when available), sharp key frame when it settles
   └─ key requested / resized    ──▶ key frame
```

The wire format is specified in [docs/protocol.md](../docs/protocol.md#binary-messages).

## Develop

```bash
cargo test --release
cargo clippy --release --all-targets -- -D warnings
```

Ignored tests need an interactive desktop (live capture benchmarks) and are run by hand. On Linux they run
headless under Xvfb, as CI does (the H.264 test needs `../packaging/linux/install-openh264.sh` first):

```bash
xvfb-run -a cargo test --release -- --include-ignored
```

H.264 on Linux: OpenDesk never compiles or bundles an H.264 codec. It loads Cisco's prebuilt OpenH264 2.6.0
(`libopenh264-2.6.0-linux64.8.so`), which Cisco licenses for the patents when downloaded from Cisco, and accepts it
only if its SHA-256 matches Cisco's release. Search order: `OPENDESK_OPENH264` (file), `~/.local/share/opendesk/`,
next to the executable, `/usr/lib/opendesk/`. Without it the encoder and decoder report an error, the client does
not advertise `video-h264`, and video stays on JPEG.
