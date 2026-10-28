# Architecture

OpenDesk is a self-hosted remote desktop. Devices never connect to each other directly at first: each keeps one
WebSocket to a relay that you run, and the relay authenticates devices, pairs sessions and forwards traffic.
Desktop peers may then move video, voice and input to an encrypted direct UDP path.

```text
 ┌──────────────────┐          WSS           ┌──────────────────┐          WSS          ┌──────────────────┐
 │  client-desktop  │ ─────────────────────▶ │   server-relay   │ ◀──────────────────── │  client-android  │
 │  Windows, Linux  │                        │  Go + PostgreSQL │                       │  Kotlin          │
 │  viewer and host │ ◀── direct UDP (P2P) ─▶│  session broker  │                       │  viewer only     │
 └────────┬─────────┘   desktop to desktop   └──────────────────┘                       └──────────────────┘
          │ uses
 ┌────────▼─────────┐
 │   core-engine    │  screen capture, video codecs, input injection, clipboard (Windows APIs / X11)
 └──────────────────┘
```

| Component | Role | Docs |
| --- | --- | --- |
| [`server-relay`](../server-relay) | Registration, session state machine, authorized routing, file grants, device registry | [server-relay/docs](../server-relay/docs) |
| [`client-desktop`](../client-desktop) | Windows and Linux app: views up to 4 hosts, hosts one viewer, files, clipboard, voice; admin edition: monitor wall | [client-desktop/docs](../client-desktop/docs) |
| [`core-engine`](../core-engine) | Library used by the desktop client: capture (DXGI/GDI, X11), framed JPEG/H.264 video, input, clipboard | [core-engine/README.md](../core-engine/README.md) |
| [`client-android`](../client-android) | Android app: views a computer; never hosts | [client-android/docs](../client-android/docs) |
| [`installer`](../installer) | Inno Setup script for the Windows installers | |
| [`packaging`](../packaging) | Linux desktop entries and the OpenH264 installer (the `.deb` metadata is in `client-desktop/Cargo.toml`) | |

The wire format shared by all of them is specified in [protocol.md](protocol.md). What each OS supports is in
[platforms.md](platforms.md).

## Platform code

OS-specific code sits behind one module API per facility, with a backend per OS selected by `cfg` at compile
time (static dispatch, no trait objects in the capture or encode path):

- `core-engine`: `capture`, `input`, `clipboard`, `desktop`, `codec::jpeg` and `codec::h264` each have a Windows
  backend (`win`, `gdiplus`, `mf`) and a backend for other systems (`x11`, `unix`, `portable`, `cisco`).
- `client-desktop/src/infra`: `audio`, `autostart`, `privacy`, `tray`, `virtual_clipboard` and `window` are
  directories with `win.rs` and `unix.rs`; small differences (paths, clock, device name) are inline `cfg`s.
- Features a platform lacks are not faked: the peer advertises what it can do (`host-features`, `viewer-only`,
  `file-offer`, `video-h264`) and the other side hides or avoids the rest.

## Layering

Every component follows the same rule: code is split by responsibility into layers, and a layer depends only
on the layers below it. Business rules never know how they are displayed, stored or transported.

| Layer | Relay (Go) | Desktop (Rust) | Android (Kotlin) |
| --- | --- | --- | --- |
| Entrypoint | `cmd/relay` | `main.rs`, `cli.rs`, `app.rs` | `ui/*Activity` |
| Presentation / transport | `internal/handler` | `ui/` (Slint) | `ui/` |
| Use cases | `internal/usecase` | `service/` | `session/` |
| Wire DTOs | `internal/dto` | `protocol/` | `data/dto` |
| Storage and OS adapters | `internal/repository/postgres` | `infra/` | `data/network`, `data/settings`, `platform/` |
| Domain types and rules | `internal/model` | `domain/` | `domain/` |

Rules that keep it that way:

- **Use cases talk to the outside only through interfaces they own.** The relay's `usecase` package declares
  `HostRepository`, `SessionRepository` and `Conn`; the desktop's services report to the UI only through
  `service::ui_port::UiPort`; Android activities observe `RelayConnection.Listener`.
- **DTOs carry data only.** Validation and decisions live in the use-case layer; DTOs never contain logic.
- **Domain code performs no I/O** and is unit-tested without mocks.
- **The composition root wires everything** (`cmd/relay/main.go`, `client-desktop/src/app.rs`) and contains no
  behavior.

## Security model

- **Relay access.** By default the relay admits every device that can reach it. Set
  `OPENDESK_DEVICE_CREDENTIALS` to require a per-device token (see
  [server-relay/docs/provisioning.md](../server-relay/docs/provisioning.md)). Keep the relay URL private either
  way.
- **Unattended desktop hosts.** Windows and Linux hosts accept every session request without a prompt. Anyone
  who can register on your relay and knows a desk ID can control that machine. Android devices and Linux desktops
  in a Wayland session register as `viewer-only` and the relay refuses every connect request to them.
- **Transport.** Use `wss://` through a TLS terminator. The relay refuses plaintext WebSocket from
  non-loopback addresses unless told TLS ends at a proxy. The P2P path is encrypted with Noise
  (`Noise_NN_25519_ChaChaPoly_BLAKE2s`), bound to the relay-approved pair.
- **End to end.** Desktop peers that both list `e2e` run `Noise_XX` with per-device keys; viewers pin host keys
  on first use and close the session when one changes. Those keys then protect P2P, and on the relay path video,
  voice, file chunks and input events, keeping in clear only the bytes the relay routes on. The relay still sees
  file names, clipboard text and metadata. See
  [client-desktop/docs/features/e2e.md](../client-desktop/docs/features/e2e.md).
- **Authorization.** The relay forwards input, clipboard, files and signals only inside an active session,
  checked from both sides, and stamps the sender of every file message itself.
- **Monitor wall.** Administrators may watch up to 16 hosts' thumbnails at once
  ([features/monitor-wall.md](features/monitor-wall.md)). A watch is not a session, so it carries nothing from the
  administrator to the host; Windows hosts accept it unattended, like a session, and say so in their status line.
- **Files.** Receivers validate every name and path from the peer, stage bytes outside the destination, verify
  SHA-256 and never overwrite an existing file.
