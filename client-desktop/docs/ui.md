# UI

The window is written in Slint (`ui/`). Rust fills its properties and implements its callbacks; the `.slint`
files contain layout and presentation logic only.

## Files

| Path | Contents |
| --- | --- |
| `ui/app_window.slint` | `MainWindow`: the public contract (properties and callbacks) and top-level layout |
| `ui/types.slint` | Structs shared with Rust: `HistoryItem`, `HostItem`, `SessionView`, `MonitorItem`, `MonitorTile`, `MonitorPick` |
| `ui/theme.slint` | Colors, sizes and stroke icons |
| `ui/components/` | Reusable widgets: `Icon`, `ToolButton`, menu rows and popovers, `Segment`, `VDivider`, `ListRow` (selectable dashboard list row), `RemoteViewerCanvas` |
| `ui/views/` | Screens: `HeaderBar`, `DashboardView` (with its `DashboardTab` and `SearchField`), `HostActiveView` |
| `ui/views/workspace/` | Viewer sessions: `SessionWorkspace` with `SessionTab`, `SessionToolbar`, `SessionTile`, `MonitorPicker` |
| `ui/views/monitor/` | Monitor wall (admin edition): `MonitorWallView` with `MonitorTileView`; layout, rotation and the device picker `MonitorPicker` sit in its Settings side panel; Full screen shows tiles only (Esc leaves) |
| `ui/views/settings/` | Settings (admin edition): `SettingsView`, a sidebar with four panes: Relay › Security (client-token allowlist, stored on the relay) and This device › Server (relay address, viewing performance), Audio (microphone) and Mouse (pointer lock), stored in `config.json` |
| `ui/components/scroll_area.slint` | `ScrollArea`: wheel/drag scrolling with a draggable scrollbar, used by the dashboard lists and the client-token list |
| `ui/dialogs/` | Relay settings, rename and delete dialogs; `WhatsNewModal` after an update (`show_whats_new`, `whats_new_title`, `whats_new_body`, `whats_new_closed(dont_show_again)`) |

One component per file. A component used by more than one view goes in `components/`; one used by a single
view stays next to it.

## Contract

Rust side: `src/ui/presenter.rs` writes properties (from `UiPort` events), `src/ui/callbacks.rs` binds callbacks
to services.

| Group | Properties (written by Rust unless marked) | Callbacks |
| --- | --- | --- |
| Device and relay | `local_desk_id`, `gateway_url` (in-out), `status_text`, `is_relay_online`, `is_admin` | `request_connect(id)`, `copy_local_id()`, `refresh_hosts()`, `save_gateway_url(url)`, `open_rename`, `save_rename`, `delete_history` |
| Dashboard lists | `history_items`, `server_hosts` (rows matching the search, written by `src/ui/dashboard_model.rs`), `history_total`, `host_total`, `list_search` (in-out, the search box), `list_query` (applied filter), `list_selected` (in-out, selected desk ID) | `list_search_edited(text)` (debounced in Rust), `list_move_selection(delta)`, `list_connect_selected()` |
| Hosting | `hosting`, `hosting_partner`, `hosting_mic_muted` | `hosting_send_files()`, `hosting_toggle_mic()`, `end_hosting()` |
| Viewer sessions | `sessions` (model, updated in place), `focused_session` (in-out), `view_layout` (in-out), `show_new_session` (in-out) | `focus_session`, `close_session`, `set_view_layout`, `new_session_page`, `session_mouse_*`, `session_key_event`, `session_release_keys`, `session_viewport_resized`, `select_monitor`, `session_send_files`, `session_toggle_mic`, `session_set_host_mic(id, on)`, `session_set_privacy`, `session_toggle_block_input`, `session_shortcut` (menu shown only when `system_keys_menu`, i.e. outside Windows), `session_set_resolution`, `session_set_audio_quality` |
| Monitor wall (admin) | `monitor_open` (in-out), `monitor_tiles` (model, updated in place), `monitor_picks`, `monitor_grid`, `monitor_page`, `monitor_pages`, `monitor_total`, `monitor_rotate`, `monitor_rotate_secs`, `monitor_all`, `monitor_relay_ok` | `open_monitor`, `close_monitor`, `monitor_step_page(delta)`, `monitor_set_grid(n)`, `monitor_set_rotation(on, secs)`, `monitor_set_all(all)`, `monitor_toggle_host(id)` (bound only in the admin edition) |
| Settings (admin) | `settings_open` (in-out), `settings_page` (in-out), `security_supported`, `security_enabled`, `security_managed_by_env`, `security_items`, `host_alias_supported` | `security_refresh`, `security_set_enabled(on)`, `security_add(id, admin)`, `security_add_online`, `security_remove(id)`, `security_reset(id)`, `save_host_alias(id, name)` (bound only in the admin edition) |
| Settings › This device | `mic_on_at_start` | `save_mic_on_at_start(on)` (bound in every edition) |
| Dialogs (UI-owned) | `show_settings`, `show_rename_modal`, `rename_*`, `show_delete_modal`, `delete_*` | |

Every session callback takes the session ID (the host's clean desk ID) first. Pointer coordinates are normalized
to 0..10000 of the remote screen; viewport sizes are physical pixels.

## Visual rules

Soft corners from `Theme` (`radius-sm` 6 px for buttons, `radius-md` 8 px for inputs and rows, `radius-lg` 10 px
for cards, 12 px for dialogs). Layered dark surfaces: background `#101013`, top bar `#0c0c0f`, cards `#17171c`,
inputs `#0b0b0e`, hairlines and fills as white at low alpha (`Theme.divider`, `#ffffff0a`..`#ffffff14`), text
`#f4f4f5`/`#a1a1aa`/`#71717a`, accent `#dc2626`. Settings icons are solid and coloured (`SolidIcon`).

Responsive: the dashboard stacks its two cards and moves the tabs above the search below 700 px width; Settings
turns its sidebar into a row of tabs below 640 px; the top bar hides the status text below 760 px. The window
minimum is 640 x 480, and 880 x 560 while a viewer session is open (its toolbar needs the width). A component
must not derive its own layout from its own width through `if` branches directly under the window layout: that
makes a binding loop (see the wrapper around `HeaderBar` in `app_window.slint`).
