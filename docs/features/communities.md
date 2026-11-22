# Communities, groups and deleting offline devices

Administrators (admin edition) can tidy the relay's shared device list in two ways:

- **Delete offline devices.** An offline device can be removed from the list. This is housekeeping, not a
  block: if the device runs OpenDesk with this relay again, it registers as usual and appears again.
- **Communities and groups.** An administrator creates communities (for example one per customer or site);
  each community holds groups (shops, floors, departments); devices are added to groups. Everything is stored on
  the relay and shared by every administrator, live.

The relay is the only authority: every request is checked there (administrator flag, names, limits, online
state). Hiding a button in a client is not security.

## Data model (relay, PostgreSQL migration `0004_communities.sql`)

| Table | Columns | Notes |
| --- | --- | --- |
| `communities` | `id`, `name`, `icon`, `color`, `created_by`, `created_at`, `updated_at` | `id` is a random 16-hex-digit string made by the relay |
| `community_groups` | `id`, `community_id` → `communities` (cascade), `name`, `created_at` | |
| `group_members` | `group_id` → `community_groups` (cascade), `host_clean_id` → `hosts_registry` (cascade), `added_at` | primary key `(group_id, host_clean_id)` |

- **Many-to-many.** A device may be in any number of groups, of the same or of different communities. Real
  setups need it (a manager's PC belongs to "Office" and to "Shop 3"; a kiosk is in a site group and in a
  "Kiosks" group); a one-group rule would force duplicate communities instead.
- **Names** are trimmed, control characters dropped, 1–48 characters. Community names are unique (ignoring case)
  on the relay; group names are unique inside their community (every shop may have a "Front desk").
- **Icon** is an optional short emoji (≤ 8 characters / 32 bytes, no ASCII letters or digits); **colour** is
  empty or one of `red orange yellow green teal blue indigo purple pink graphite`.
- **Limits:** 64 communities, 64 groups per community, 500 device IDs per request; registry edits are
  rate-limited per connection (burst 20, then one per 100 ms), like P2P signaling.

### What deleting a device removes

`DELETE FROM hosts_registry WHERE clean_id = ANY($1) AND online = FALSE`

- **Refused while online.** The relay refuses devices that are connected or reconnecting (a suspended
  session-resume connection is still registered). The check runs in memory and the `online = FALSE` condition
  repeats it in the same SQL statement, so a device that registers at that moment is never lost: either its
  upsert came first (row kept) or it re-inserts the row.
- **Removed with the row:** the administrator's alias (a column of the row) and every group membership
  (foreign-key cascade). Groups themselves stay.
- **Kept:** session history (`remote_sessions`, an audit trail that must not change when the list is tidied)
  and the device allowlist (`allowed_devices`). Deleting from the list is not a security action; removing a
  device from the allowlist stays on Settings › Security, so deleting cannot silently revoke (or keep) access.

Deleting a group removes only its memberships (the devices stay in All Devices and their other groups);
deleting a community removes its groups and their memberships.

## Protocol

Capabilities in `registered-ack`: `host-delete` (always, from this relay version) and `communities` (when the
relay has community storage). Clients show the features only when listed.

Client → relay (administrators only; others are ignored and logged):

| Type | Fields |
| --- | --- |
| `host-delete` | `targetCleans: [cleanId]` |
| `get-communities` | — |
| `community-create` | `name`, `icon`, `color` |
| `community-update` | `communityId`, `name`, `icon`, `color` |
| `community-delete` | `communityId` |
| `group-create` | `communityId`, `name` |
| `group-rename` | `groupId`, `name` |
| `group-delete` | `groupId` |
| `group-members` | `groupId`, `add: [cleanId]`, `remove: [cleanId]` (unregistered IDs are skipped) |

Relay → administrators:

```json
{"type":"community-list","communities":[
  {"id":"9f2c…","name":"Shops","icon":"🏪","color":"blue",
   "groups":[{"id":"41ab…","name":"Front desk","members":["123456789"]}]}]}
```

`community-list` answers `get-communities` and is broadcast to every connected administrator after each
change (and after a device deletion, which may change memberships). A successful `host-delete` broadcasts
`host-list` as before. Refusals are an `error` to the sender only, e.g. "1 device(s) are online and were not
deleted…", "That name is already used.", "Too many changes at once…".

### Compatibility

- **`host-list` is unchanged.** Group membership lives only in `community-list`, so Android admins and older
  desktop admins see exactly what they saw before.
- **New message types only.** Clients ignore types they do not know (Android's Gson `ServerMessage`, the desktop
  `ServerMessage::Unknown`), so the broadcast `community-list` is harmless to them; they never send the new
  requests.
- **Older relays** do not list the capabilities: the desktop hides deletion and communities entirely.
- **The desktop `HostEntry`** now also reads `clean_id` (sent by every relay version; defaults to empty).

## Desktop UI (admin edition)

- **All Devices** is unchanged, except that every device card / row has a context menu (right-click or the
  "…" button on hover): Connect, Rename…, Add to Group…, Remove from "group" (on a group page), Delete Device…
  (offline devices only). The Offline section has "Delete Offline…", which deletes the offline devices the page
  shows (search, sidebar filter and group applied) after a confirmation.
- **Sidebar › Communities.** No community yet: a subtle "+ New Community". One community: listed directly.
  Several: one "Communities" entry that opens a cascading glass picker beside the sidebar (each community with
  its tile and online/total count, a check on the open one, then "New Community…" and "Manage
  Communities…"), like a macOS submenu.
- **Community page.** Opening a community adds a second, narrower column (Slack-like): the community's tile and
  name with its "…" menu (Edit, New Group, Manage Members, Delete), then "All Members" and its groups with
  online/total counts and "+ New Group"; right-click a group to rename or delete it. The main area shows the
  selected group's devices with the same cards/list, search, online/offline sections and Connect buttons as
  All Devices.
- **Settings › Relay › Communities** is the full management page, laid out like macOS System Settings: inset
  lists with +/− toolbars. Select a community to see its groups; select a group to edit its members in a
  searchable list with checkboxes and online state. Double-click a row to rename it.
- **Sheets:** new / edit community (name, emoji, colour), new / rename group, "Add to Group" (every group,
  grouped by community, with checkboxes), and deletion confirmations.

Code: relay `internal/usecase/community.go`, `internal/handler/community_handler.go`,
`internal/repository/postgres/community_repository.go`; desktop `src/ui/community_model.rs`,
`ui/components/community.slint`, `ui/dialogs/community_sheets.slint`,
`ui/views/settings/communities_pane.slint`, the community parts of `ui/views/dashboard.slint`.
