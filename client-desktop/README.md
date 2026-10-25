# OpenDesk for Windows and Linux

Native desktop client: control up to four remote machines in tabs or a split view, or let one remote viewer
control this machine (Linux: in an X11 session; see [docs/platforms.md](../docs/platforms.md)). Rust with a [Slint](https://slint.dev) UI; screen capture and codecs come from
[`core-engine`](../core-engine).

## Editions

One code base, three binaries selected with Cargo features:

| Edition | Build | Description |
| --- | --- | --- |
| Standard | `cargo build --release` | Window, tray, viewer and host |
| Admin | `cargo build --release --features admin` | Standard plus the relay's device list |
| Headless host | `cargo build --release --features headless` | No window or tray: an unattended host for servers and kiosks |

From the repository root, `make windows`, `make windows-admin`, `make windows-host` build into `builds\windows`
and `make setup` builds the installer. On Linux, `make linux`, `make linux-admin`, `make linux-host` build into
`builds/linux`, `make linux-deb` builds the `opendesk`, `opendesk-admin` and `opendesk-host` packages and
`make linux-tar` a portable archive.

## Develop

Requirements: Rust (stable 1.92+, edition 2024) on Windows 10/11 (optional: `windres` from MinGW to embed the
icon) or Linux (packages in [CONTRIBUTING.md](../CONTRIBUTING.md#linux)).

```bash
cargo run --release          # runs the standard edition
cargo test --release         # unit tests (some ignored tests need a desktop session; see their comments)
cargo clippy --release --all-targets -- -D warnings
cargo fmt
```

A local relay for development: `make run-relay` (needs PostgreSQL), then set the relay to
`ws://127.0.0.1:8088` in the app.

## Layout

| Path | Layer | Contents |
| --- | --- | --- |
| `src/main.rs`, `src/cli.rs`, `src/app.rs` | Entrypoints | CLI commands, composition root |
| `src/ui/`, `ui/*.slint` | Presentation | Slint window, callbacks, presenter, tray, drag-and-drop |
| `src/service/` | Use cases | Relay connection, viewer sessions, hosting, file transfer, clipboard, history |
| `src/protocol/` | Wire DTOs | Typed relay messages, input events, file payloads, binary framing |
| `src/infra/` | Adapters | Storage, audio, P2P transport, privacy overlay, tray, autostart, virtual clipboard; OS-specific modules have `win.rs` and `unix.rs` |
| `src/domain/` | Domain | Desk IDs, settings, history, capabilities, resolution policy, key mapping, transfer rules |

See [docs/architecture.md](docs/architecture.md) for the dependency rules and threads.

## Documentation

- [Architecture](docs/architecture.md) · [Configuration](docs/configuration.md) · [UI contract](docs/ui.md)
- Features: [relay connection](docs/features/relay-connection.md), [viewer sessions](docs/features/viewer-sessions.md),
  [hosting](docs/features/hosting.md), [file transfer](docs/features/file-transfer.md),
  [clipboard](docs/features/clipboard.md), [direct P2P path](docs/features/p2p.md), [voice](docs/features/voice.md),
  [autostart and CLI](docs/features/autostart-and-cli.md)
