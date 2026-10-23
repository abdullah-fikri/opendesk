# UI

The window is written in Slint (`ui/`). Rust fills its properties and implements its callbacks; the `.slint`
files contain layout and presentation logic only.

## Files

| Path | Contents |
| --- | --- |
| `ui/app_window.slint` | `MainWindow`: the public contract (properties and callbacks) and top-level layout |
| `ui/types.slint` | Structs shared with Rust: `HistoryItem`, `HostItem`, `SessionView`, `MonitorItem` |
| `ui/theme.slint` | Colors, sizes and stroke icons |
| `ui/components/` | Reusable widgets: `Icon`, `ToolButton`, menu rows and popovers, `Segment`, `VDivider`, `RemoteViewerCanvas` |
| `ui/views/` | Screens: `HeaderBar`, `DashboardView`, `HostActiveView` |
| `ui/views/workspace/` | Viewer sessions: `SessionWorkspace` with `SessionTab`, `SessionToolbar`, `SessionTile`, `MonitorPicker` |
| `ui/dialogs/` | Relay settings, rename and delete dialogs |

One component per file. A component used by more than one view goes in `components/`; one used by a single
view stays next to it.

## Contract

Rust side: `src/ui/presenter.rs` writes properties (from `UiPort` events), `src/ui/callbacks.rs` binds callbacks
to services.

| Group | Properties (written by Rust unless marked) | Callbacks |
| --- | --- | --- |
| Device and relay | `local_desk_id`, `gateway_url` (in-out), `status_text`, `is_relay_online`, `is_admin`, `history_items`, `server_hosts` | `request_connect(id)`, `copy_local_id()`, `refresh_hosts()`, `save_gateway_url(url)`, `open_rename`, `save_rename`, `delete_history` |
| Hosting | `hosting`, `hosting_partner`, `hosting_mic_muted` | `hosting_send_files()`, `hosting_toggle_mic()`, `end_hosting()` |
| Viewer sessions | `sessions` (model, updated in place), `focused_session` (in-out), `view_layout` (in-out), `show_new_session` (in-out) | `focus_session`, `close_session`, `set_view_layout`, `new_session_page`, `session_mouse_*`, `session_key_event`, `session_release_keys`, `session_viewport_resized`, `select_monitor`, `session_send_files`, `session_toggle_mic`, `session_set_privacy`, `session_toggle_block_input`, `session_shortcut`, `session_set_resolution`, `session_set_audio_quality` |
| Dialogs (UI-owned) | `show_settings`, `show_rename_modal`, `rename_*`, `show_delete_modal`, `delete_*` | |

Every session callback takes the session ID (the host's clean desk ID) first. Pointer coordinates are normalized
to 0..10000 of the remote screen; viewport sizes are physical pixels.

## Visual rules

Square corners (radius 0, at most 2 px where a shape needs it). Palette: background `#09090b`, surfaces
`#111114`/`#18181b`, dividers `#1f1f23`/`#27272a`, text `#f4f4f5`/`#a1a1aa`/`#71717a`, accent `#dc2626`.
