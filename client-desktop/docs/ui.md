# UI

The window is written in Slint (`ui/`). Rust fills its properties and implements its callbacks; the `.slint`
files contain layout and presentation logic only.

## Files

| Path | Contents |
| --- | --- |
| `ui/app_window.slint` | `MainWindow`: the public contract (properties and callbacks) and top-level layout |
| `ui/types.slint` | Structs shared with Rust: `HistoryItem`, `HostItem`, `SessionView`, `MonitorItem`, `MonitorTile`, `MonitorPick` |
| `ui/theme.slint` | Colors (light and dark: `Theme.dark` follows `Theme.appearance`), sizes and stroke icons |
| `ui/fonts/` | Inter (text) and JetBrains Mono (device IDs), embedded by `app_window.slint` |
| `ui/components/` | Reusable widgets: `controls.slint` (`Btn`, `Seg`/`SegWrap`, `Switch`, `Card`, `Pop`), `Glyph` (line icons) and `AppTile` (coloured icon tiles), both SVG files in `ui/icons/` that Slint renders at their displayed pixel size, `window_frame.slint` (window buttons, move and resize of the frameless window), `Icon`, `ToolButton`, menu rows and popovers, `Segment`, `VDivider`, `RemoteViewerCanvas` |
| `ui/views/` | Screens: `DashboardView` (Devices: sidebar, top bar, cards or list), `HostActiveView` with its `HeaderBar` |
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
| Dashboard lists | `history_items`, `hosts_online` / `hosts_offline` (rows matching the search and the sidebar filter, written by `src/ui/dashboard_model.rs`), `server_hosts` (both, in display order), `history_total`, `host_total`, `host_online_total`, `host_offline_total`, `host_filter` (in-out: 0 all, 1 online, 2 offline), `dashboard_view_mode` (in-out: 0 cards, 1 list), `list_search` (in-out, the search box), `list_query` (applied filter), `list_selected` (in-out, selected desk ID) | `list_search_edited(text)` (debounced in Rust), `host_filter_changed()`, `list_move_selection(delta)`, `list_connect_selected()` |
| Hosting | `hosting`, `hosting_partner`, `hosting_mic_muted` | `hosting_send_files()`, `hosting_toggle_mic()`, `end_hosting()` |
| Viewer sessions | `sessions` (model, updated in place), `focused_session` (in-out), `view_layout` (in-out), `show_new_session` (in-out) | `focus_session`, `close_session`, `set_view_layout`, `new_session_page`, `session_mouse_*`, `session_key_event`, `session_release_keys`, `session_viewport_resized`, `select_monitor`, `session_send_files`, `session_toggle_mic`, `session_set_host_mic(id, on)`, `session_set_privacy`, `session_toggle_block_input`, `session_shortcut` (menu shown only when `system_keys_menu`, i.e. outside Windows), `session_set_resolution`, `session_set_audio_quality` |
| Monitor wall (admin) | `monitor_open` (in-out), `monitor_tiles` (model, updated in place), `monitor_picks`, `monitor_grid`, `monitor_page`, `monitor_pages`, `monitor_total`, `monitor_rotate`, `monitor_rotate_secs`, `monitor_all`, `monitor_relay_ok` | `open_monitor`, `close_monitor`, `monitor_step_page(delta)`, `monitor_set_grid(n)`, `monitor_set_rotation(on, secs)`, `monitor_set_all(all)`, `monitor_toggle_host(id)` (bound only in the admin edition) |
| Settings (admin) | `settings_open` (in-out), `settings_page` (in-out), `security_supported`, `security_enabled`, `security_managed_by_env`, `security_items`, `host_alias_supported` | `security_refresh`, `security_set_enabled(on)`, `security_add(id, admin)`, `security_add_online`, `security_remove(id)`, `security_reset(id)`, `save_host_alias(id, name)` (bound only in the admin edition) |
| Settings › This device | `mic_on_at_start`, `pointer_lock` | `save_mic_on_at_start(on)`, `save_pointer_lock(on)` (bound in every edition) |
| Appearance and window | global `Theme.appearance` 0 = system, 1 = light, 2 = dark (Rust sets it from `config.json` `dark_mode`, unset = system); global `WindowFrame.maximized` | `save_appearance(appearance)`; `WindowFrame`: `minimize`, `toggle-maximize`, `close`, `drag`, `resize(edge)`, `state-changed` (`src/ui/window_frame.rs`) |
| Dialogs (UI-owned) | `show_settings`, `show_rename_modal`, `rename_*`, `show_delete_modal`, `delete_*` | |

Every session callback takes the session ID (the host's clean desk ID) first. Pointer coordinates are normalized
to 0..10000 of the remote screen; viewport sizes are physical pixels.

## Visual rules

The window has no system title bar (`no-frame`): minimize, maximize and close (46 x 32 each) sit flush in its
top-right corner over every page, empty bar space moves the window (a double click maximizes it), and 5 px edges
resize it. The bars are 56 px high (the session tab strip 44 px) with their controls below the window buttons, and
keep about 150 px free on the right for them.

Light and dark palettes from the redesign (`Theme`): background `#F6F5F2` / `#242424`, sidebar `#ECEAE6` /
`#2B2B2B`, cards `#FFFFFF` / `#333333` with a soft shadow, OpenDesk red `#DB3141` / `#D63342` for primary
actions, green for online, amber for warnings, blue for information. Corners: 7 px buttons and inputs, 12 px
cards, 14 px dialogs and popovers. Sidebar entries and settings use coloured gradient tiles (`AppTile`); every
other icon is a single-colour `Glyph`. Icons are SVG, not `Path`: the GPU renderer draws small paths and
shrinks large PNGs without smoothing, which looks jagged. For the same reason the logo is `Brand.logo`, which
Rust scales from `ui/logo-light.png` / `ui/logo-dark.png` to the exact physical size (`src/ui/window_frame.rs`).
Both come from the designer's transparent logo; the light variant darkens the white ribbon to slate so it does not
vanish on white. The window, taskbar and tray icon stay `icon.png` / `icon.ico`. Text uses Inter and device IDs
JetBrains Mono (`Theme.mono`), both bundled in `ui/fonts`. Tooltips use `Tooltip { TipBox { text: …; } }` so they
match the theme.

The appearance follows the system until the user picks Light or Dark (Light / System / Dark in the sidebars and
the wall settings). On Windows the system setting is read from the registry (`AppsUseLightTheme`, re-read every
2 s, `src/infra/system_theme.rs`), since winit's own check can report the wrong mode on Windows 11. Viewer sessions use the same tokens: rounded tabs on the sidebar colour, a toolbar of
redesign buttons, popovers in the `Pop` style, and the letterbox around the remote screen in `Theme.canvas`;
overlays on the remote picture (stats, reconnecting, click to focus) are dark glass with white text in both
appearances.

Responsive: the sidebar shows tiles only below 980 px; the dashboard stacks quick connect and this device's ID
when its content is narrower than 620 px; Settings shows tiles only below 760 px. The window minimum is 640 x 480,
and 880 x 560 while a viewer session is open (its toolbar needs the width). A component must not derive its own
layout from its own width through `if` branches directly under the window layout: that makes a binding loop (see
the wrapper around `HeaderBar` in `app_window.slint`).
