# core-engine

Windows-only library with the low-level building blocks of the desktop client. It knows nothing about the
network, the UI or sessions.

| Module | Contents |
| --- | --- |
| `capture` | `DesktopCapturer`: DXGI Desktop Duplication (dirty and move rectangles, cursor overlay) with GDI fallback; monitor enumeration; cursor shape |
| `diff` | Exact block-level change detection between frames |
| `codec` | Framed key/delta video (`frame`, `delta`), H.264 through Media Foundation (`h264`), JPEG through GDI+ (`jpeg`), base64 |
| `stream` | `StreamEncoder`: per tick decides between nothing, a full JPEG, a key frame, a delta or H.264, and the JPEG quality under congestion |
| `input` | Mouse and keyboard injection |
| `clipboard` | Clipboard text and file lists |
| `desktop` | Attaching capture threads to the input desktop (Winlogon, UAC) |

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

Ignored tests need an interactive desktop (live capture benchmarks) and are run by hand.
