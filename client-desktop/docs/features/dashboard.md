# Dashboard

Code: `ui/views/dashboard.slint` (with `dashboard_tab.slint`, `search_field.slint`, `components/list_row.slint`),
`src/ui/dashboard_model.rs`; matching rules in `src/domain/list_search.rs`.

The home screen shows this device's ID, a form to connect by ID, and two lists: **Session History** (every
edition) and **Relay Hosts** (admin edition). The same screen is the "+" page while viewer sessions are open.

## Tabs

In the admin edition a tab bar spans the list panel: Relay Hosts, Session History and Monitor Wall. Each tab
shows a count badge; while a search filters the list it reads shown/total, for example `3/8`. Monitor Wall opens
the wall instead of switching the list (it is hidden on the "+" page). Switching tabs clears the selection.
Opening Relay Hosts asks the relay for a fresh host list. Other editions have only the history, so there is no
tab bar, only the toolbar.

## Search

The toolbar under the tabs holds the search box and, on Relay Hosts, a Refresh button.

- The lists filter while you type, once typing pauses for 200 ms; Enter is not needed. Clearing the box (× or
  Esc) shows every row at once. Ctrl+F moves the focus to the box from anywhere on the dashboard.
- Matching ignores case. The text is split into words and every word must occur somewhere in the row: in the
  name, device name, desk ID or platform of a host, or the name or desk ID of a history entry. Desk IDs are
  compared without spaces, so `418541` finds `418 541 168`.
- One query applies to both lists. A host-list refresh or a history change keeps the current filter.
- When nothing matches, the list says so (`No hosts match "xyz"`).

## Selection and keyboard

- Clicking a row selects it: red border and left accent. The buttons of the other rows dim (they stay
  clickable), so the selected row's Connect is the obvious one. Clicking a button acts without selecting.
- A double click does not connect: only the Connect button or Enter does.
- ↑ and ↓ move the selection through the shown rows (also while typing in the search box) and scroll it into
  view; Enter connects to the selected row (not to an offline host).
- The selection follows the desk ID, so it survives a refresh or a new search, and is cleared once its row is no
  longer shown.
