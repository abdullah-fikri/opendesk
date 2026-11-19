# Dashboard

Code: `ui/views/dashboard.slint`,
`src/ui/dashboard_model.rs`; matching rules in `src/domain/list_search.rs`.

The home screen shows this device's ID, a form to connect by ID, and two lists: **Session History** (every
edition) and **Relay Hosts** (admin edition). The same screen is the "+" page while viewer sessions are open.

## Sidebar

The admin edition's sidebar lists Devices › All Devices, Online and Offline (the relay's hosts, with counts) and
Workspaces › Session History and Monitor Wall (opens the wall). Other editions show Session History only. All
Devices shows two sections, Online then Offline; Online and Offline show one. Opening a host view asks the relay
for a fresh host list; switching views clears the selection. The bottom of the sidebar shows the Appearance
switch (System, Light or Dark; System until the user picks one, kept in `config.json`).

Devices show as cards (default) or as list rows (the view switch next to the search). A card shows the name and
ID; its details (platform, device name, when an offline host was last seen) appear on hover or selection, next
to Rename and Connect. An offline host's card is dimmed and its Connect is disabled.

## Search

The top bar holds the search box (centred over the content), the card/list switch, on the host views a Refresh
button, and Settings. The Monitor Wall opens from the sidebar.

- The lists filter while you type, once typing pauses for 200 ms; Enter is not needed. Clearing the box (× or
  Esc) shows every row at once. Ctrl+F moves the focus to the box from anywhere on the dashboard.
- Matching ignores case. The text is split into words and every word must occur somewhere in the row: in the
  name, device name, desk ID or platform of a host, or the name or desk ID of a history entry. Desk IDs are
  compared without spaces, so `418541` finds `418 541 168`.
- One query applies to both lists. A host-list refresh or a history change keeps the current filter.
- When nothing matches, the page says "No devices found" with a Clear search link.

## Selection and keyboard

- Clicking a card or row selects it: red outline and a solid red Connect (the others keep a soft red one), plus
  Rename (and Remove on history entries). Clicking a button acts without selecting.
- A double click connects, like the Connect button or Enter.
- ↑ and ↓ move the selection through the shown rows (also while typing in the search box) and scroll it into
  view; Enter connects to the selected row (not to an offline host).
- The selection follows the desk ID, so it survives a refresh or a new search, and is cleared once its row is no
  longer shown.
