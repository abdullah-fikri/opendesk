# Desktop client architecture

## Layers

```text
main.rs ─▶ cli.rs (CLI commands, exits)
        └▶ app.rs (composition root)
              │ builds
              ▼
   ui/ ──────▶ service/ ──────▶ protocol/, domain/
   (Slint)     (use cases)  └──▶ infra/ (Windows, files, network)
      ▲            │
      └── UiPort ◀─┘   services report to the UI only through this trait
```

| Module | May use | Must not use |
| --- | --- | --- |
| `domain` | `std` | anything else in the crate, I/O, Slint, serde_json values |
| `protocol` | `domain` (none today), serde | services, infra, UI |
| `infra` | `domain`, `protocol`, `core_engine` | `service`, `ui` |
| `service` | `domain`, `protocol`, `infra`, `core_engine` | `ui`, Slint widgets (only `slint::SharedPixelBuffer` as a frame container) |
| `ui` | everything above | business decisions: callbacks translate arguments and call a service |

The two exceptions are documented where they occur: `domain::keyboard` calls `VkKeyScanW` to map characters
with the active keyboard layout, and `service::ui_port::Frame` carries a `SharedPixelBuffer` so decoded frames
reach the UI without a copy.

## Shared state

`service::context::AppContext` (`Ctx = Arc<AppContext>`) is built once in `app.rs` and passed to every service:

| Field | Meaning |
| --- | --- |
| `outbox` | Sender of the relay connection's queue (`protocol::Outgoing`) |
| `ui` | `Arc<dyn UiPort>`: the Slint presenter, or `NoUi` for the headless edition |
| `viewers` | `service::viewer::Registry`: viewer sessions, focus, layout |
| `host` | `service::host::HostState`: the hosted session, capture settings, held keys |
| `relay` | Gateway URL, reconnect flag, relay capabilities |
| `voice`, `injector` | Audio engine and input injector |
| `partner()` | The current partner: the hosted viewer, else the focused viewer session |

File transfer keeps its own module state (`service::file_transfer`): partner capabilities, pending ACKs and the
in-flight chunk counter used for backpressure.

## Threads

| Thread | Started by | Work |
| --- | --- | --- |
| UI (main) | `app.rs` | Slint event loop; callbacks call services |
| Relay connection | `service::relay::spawn` | Connect, register, pump the outbox, read and dispatch messages, reconnect |
| Capture (per hosted session) | `service::host::start` | Capture, encode, announce monitors and cursor |
| Decoder (per viewer session) | `service::viewer::decoder::spawn` | Decode video, report frames to the UI |
| Clipboard watcher | `service::clipboard_sync::spawn` | Poll the clipboard, send changes to the partner |
| Settings watcher | `service::settings::spawn_watcher` | Pick up `opendesk server <url>` from the CLI |
| File senders | `service::file_transfer::sender` | One per send job |
| Audio, P2P, privacy, tray, OLE clipboard | `infra::*` | Windows API loops |

Services never block the UI thread: the presenter forwards every `UiPort` call with
`upgrade_in_event_loop`.

## Request flow example: a mouse click in a session tile

1. Slint calls `session_mouse_down(id, x, y, button)` (`ui/callbacks.rs`).
2. `service::viewer::controller::mouse` checks focus (a click on an unfocused tile only focuses it) and sends
   `ClientMessage::RelayInput { event: InputEvent::MouseDown {..} }` to the outbox.
3. The relay connection sends it over the P2P path when one is up, else over the WebSocket.
4. On the host, `service::relay::dispatcher` decodes `input-command` into `InputEvent` and
   `service::host::input::apply` injects the click.
