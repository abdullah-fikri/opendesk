# File browser: explorer operations

The two-pane file browser (this computer on the left, the remote device on the right) now behaves like Finder or
Explorer: opening files, multiple selection, sorting, rename, delete, extract and compress, on both sides. The
base protocol (`list`, `get`, `mkdir`, `listing`, `done`, `failed`) is in [protocol.md](../protocol.md); this page
records what was added on top of it and why.

Implemented in:

- `client-desktop/src/domain/file_browser.rs`: the messages, `Selection` (click / toggle / range / arrow keys),
  column sorting, and the path rules `valid_folder_name`, `valid_host_dir`, `valid_host_item`.
- `client-desktop/src/service/file_browser.rs`: requests and answers on both sides, the host's file system work
  (as the signed-in user, `infra::user_scope`), Open on a remote file.
- `client-desktop/src/infra/archive.rs`: .zip / .tar / .tar.gz reading and .zip writing (pure Rust on
  `miniz_oxide`), with the zip-slip, link and size guards.
- `client-desktop/src/infra/shell.rs`: open with the default app, show in Explorer, Recycle Bin.
- `client-desktop/src/ui/file_browser_model.rs`, `client-desktop/ui/views/file_browser.slint`: the panes.

## Protocol additions

All messages are JSON with `op`, sent like the existing ones (file stream kind 3 over a direct path, else the
sealed `browse` relay event). `req` pairs every answer with its request, as before.

From the viewer:

| `op` | Fields | Meaning |
|---|---|---|
| `rename` | `req`, `path`, `name` | Rename the item at `path` to `name`, in the same folder. |
| `delete` | `req`, `paths` | Delete these items (folders with their contents). Sent only after the viewer's user confirmed. |
| `extract` | `req`, `path`, `folder` (bool, default false) | Unpack the archive next to it, or with `folder` into a new folder named after it ("photos/" for photos.zip; "photos (1)/" if taken). |
| `compress` | `req`, `paths` | Pack these items, all from one folder, into a new .zip in that folder (`<name>.zip` for one item, else `Archive.zip`; a free name if taken). |

From the host:

| `op` | Fields | Meaning |
|---|---|---|
| `listing` | + `ops` (array of strings, omitted when empty) | The operations above this host serves: `["rename","delete","extract","compress"]`. |
| `progress` | `req`, `done`, `total` | How far a long request (extract, compress) got; at most every 400 ms. Only the ratio matters. |
| `done` | + `note` (string, omitted when empty) | What the request did, for the viewer's status line ("Extracted 12 items into photos"). |

Every request is answered with exactly one `done` or `failed`, after any number of `progress`.

### Compatibility

- **Older host, newer viewer.** An older host's `listing` has no `ops`, so the viewer leaves the remote Rename,
  Delete, Extract and Compress entries disabled (and says the host needs an update if they are triggered by key).
  It never sends a request the old host would drop without answering. Browsing, copying, New Folder and Open
  (which is a plain `get`) still work. A device without the file browser at all (no `file-channel` capability)
  still shows "This device runs an older OpenDesk without the file browser".
- **Newer host, older viewer.** The extra `ops` and `note` fields are ignored by older viewers (unknown JSON
  fields are skipped), and they never send the new requests. A `done` without a note is byte-for-byte the old one.
- Unknown `op`s are still dropped (they do not decode), as before.

## Viewer behavior

- **Open** (double-click, Enter, menu): a folder opens in its pane; a local file opens in its default app
  (`ShellExecuteW` on Windows, `open` / `xdg-open` elsewhere). A remote file is fetched with an ordinary `get`
  into a fresh folder under `%TEMP%\OpenDesk\open\`, shown in the transfer panel, and opened as soon as it is
  placed there. Opening a file on the remote device itself is not offered: the host agent usually runs as SYSTEM
  in another session, where a started app would not be visible to anyone.
- **Selection**: click, Ctrl/Cmd+click (toggle), Shift+click (range from the anchor), Ctrl+A, Esc (clear; a
  second Esc closes the browser), arrows / Page Up / Page Down / Home / End with optional Shift. A click on empty
  space clears. Rubber-band selection is not implemented. The pane footer shows "3 of 24 selected · 4.2 MB".
- The context menu, the → / ← buttons and drag-and-drop act on the whole selection; folders are copied
  recursively in both directions (the existing folder transfer, `tree::walk`, `FolderTag` groups), and names that
  exist get "name (1).ext".
- **Other operations**: New Folder, Rename (F2), Delete (Del, with a confirmation), Refresh (F5), Copy Path,
  Show in Explorer (local), Back (Backspace) and Up (Alt+Up), editable path bar, drives at the top level, sorting
  by Name / Size / Modified (click a header; again to reverse; folders stay first).
- **Delete** asks first. Locally on Windows the items go to the Recycle Bin (`SHFileOperationW` with
  `FOF_ALLOWUNDO`; it warns instead of silently deleting what does not fit in the bin). Remote deletes are
  permanent, and the prompt says so: the host agent runs as SYSTEM, whose Recycle Bin would be the wrong user's.

## Security

The rules in `service/file_browser.rs` still hold: a host serves only the viewer of its current session, and every
file system call for it runs as the signed-in user. In addition:

- **Paths from the network** must pass `valid_host_dir` (absolute; no `..`; no NUL; no `\\.\`, `\\?\`, `\??\`
  device or verbatim prefix; on Windows no `:` stream after the drive). Paths that are changed (rename, delete,
  extract, compress) must also pass `valid_host_item`: not a drive or file system root. `list`, `mkdir` and `get`
  now go through `valid_host_dir` too.
- **New names** (rename, new folder) must pass `valid_folder_name`: one component, no separators, no reserved
  device names (`CON`, `nul.txt`...), no trailing dot or space. Rename never replaces an existing item.
- **Archives** (`infra::archive`): every entry name passes `safe_rel_path` (no absolute or drive paths, `..`,
  device names, streams) and is joined below the target; unsafe entries are skipped and counted in the note. Tar
  symlinks, hard links and devices are never created. If a folder the archive writes into exists but is a link
  or a file, that entry is skipped, so nothing is written through a link out of the target. Files are created with
  `create_new`, under a free name when taken: extraction never overwrites, which is why it needs no confirmation.
  The number of entries (20,000) and the bytes actually written (16 GB, counted while writing, not taken from the
  headers) are bounded; a zip whose header understates an entry's size fails at the first extra byte. Any failure
  removes everything that extraction created. ZIP64, encrypted zips, .7z and .rar are refused with a message:
  their decoders would be a large new dependency for a rare case.
- **Compress** walks the selection with `tree::walk` (links inside folders are skipped, the same limits as a
  folder transfer) and only creates a new file (`create_new`); a failure removes the partial .zip.
