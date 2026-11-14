# File transfer

Code: `src/service/file_transfer/` (`sender.rs`, `receiver.rs`, `offer.rs`, `tree.rs`), payload type
`src/protocol/file_payload.rs`, limits and path rules `src/domain/transfer.rs`, paste streams
`src/infra/virtual_clipboard.rs`. Relay rules: [file grants](../../../server-relay/docs/features/file-grants.md).

## Ways to send

| Trigger | Behavior |
| --- | --- |
| Drop files on the window | Sent to the current partner and pasted into its focused Explorer window |
| "Send files" button | File picker, then the same |
| Ctrl+V in a session with files on the clipboard | Sent and pasted, unless those exact files were already offered to this host in this session |
| Copy files while a partner supports `file-offer` | Only names and sizes are sent; the partner's clipboard shows virtual files, and each paste pulls one file |
| Copy files on a host without offers | Sent to the viewer's clipboard, once per real copy (below) |

A host whose viewer cannot take offers (the viewer runs as the service's SYSTEM agent, or is not on Windows)
falls back to sending copied files. It sends only files that differ from what the clipboard held at the
previous look: files copied before the session started, or still there when another program merely touched the
clipboard (a clipboard manager, an app gaining focus after Alt+Tab), are not sent. The log says which.

## Progress

Every sent and received file gets a row in the floating "File transfers" panel (`src/ui/transfer_model.rs`):
name, direction, a progress bar, size and speed, then "Done" or the reason it failed. Finished rows disappear
after 4 s, failed ones after 15 s (or with their close button). A second Ctrl+V while the same files are still
being sent to that host only shows "Still sending these files"; they are pasted once when they arrive.

## Sending

Each file is `start`, chunks and `end` with its SHA-256. Chunks are 128 KiB binary messages when the receiver
and the relay both list `file-binary`, else 32 KiB base64 inside JSON (Android, older peers and relays); pulled
clipboard offers always use JSON, which the paste stream reads. A 10 MB file takes 10.0 MB on the wire in 79
messages instead of 13.4 MB in 308. Senders wait while more than 48 chunks' worth (32 KiB units, ~1.5 MB) are
queued, so progress follows the network. With `file-ack` the receiver confirms; "Sent" then means verified on
the other side, and a missing confirmation fails after 120 s. Without it (Android) "Sent (not confirmed by the
receiver)" only means every byte left this machine. A lost relay connection fails every transfer on it; there is
no resume.

### Pacing

File bytes share the uplink with the session: a full modem queue held the P2P path's keep-alives and video for
over 3 s, so the path dropped three times during one 86 MB transfer and video fell to 3-7 FPS. The relay
connection therefore paces file messages (`src/domain/file_pacing.rs`, `src/service/relay/file_pacer.rs`), LEDBAT
style:

- While files flow it sends a WebSocket ping after the file bytes, at most every 50 ms; the relay answers it only
  after reading everything before it, so the round trip includes every queue on the way (socket buffer, modem,
  the relay's backpressure from a slow receiver). No new message: every relay answers pings.
- The base round trip is the lowest of the last five minutes. When the round trip (the lowest of the last three
  samples) is more than 25 ms above it, the file rate halves, at most once per base round trip; an unanswered
  ping that is already that late counts too. While it stays below, the rate grows by up to 10% per sample, but
  only when file messages actually waited for the rate.
- The rate starts at 1 MB/s, never drops below 64 KB/s (transfers always move) and goes up to 256 MB/s. A token
  bucket spends it, on top of the per-tick budget that keeps file messages behind video and voice.
- The rate is logged every 10 s while files flow (`File pacing: ... KB/s, round trip ... ms`).
- `OPENDESK_FILE_RATE_LIMIT=0` turns pacing off (the behavior before it existed).

Folders go only to `file-folder` receivers: folder entries first, then files with their relative path, then a
repeat of the first folder entry that marks the end. For other receivers folders are skipped with a notice.

Several loose files (no folder) go to `file-group` receivers as one group too, closed by a bare group end
([protocol](../../../docs/protocol.md#file-transfer)), so the receiver puts them all on its clipboard and pastes
them once instead of file by file. Older receivers, a single file, and names the folder rules would reject
(device names, invalid characters) keep one transfer per file.

## Receiving

SECURITY: everything from the peer is validated before it touches the disk.

- IDs: letters, digits and `_`, at most 80 characters. At most 8 files and 4 folder transfers at once.
- Names and relative paths pass `domain::transfer::safe_rel_path`: no absolute or UNC paths, drive letters,
  streams, `..`, device names (`CON`, `NUL`, `COM1`...), invalid characters or over-long paths.
- Limits: 2 GB per file; 10 000 entries and 32 GB per folder transfer.
- Bytes go to a `.part` file and are linked under the final name only when size and SHA-256 match. Existing
  files and folders are never overwritten or merged into; a copy gets `name (1)`.
- Binary chunks are accepted only from the sender the relay tagged, at exactly the current offset and up to
  128 KiB; anything else fails the transfer with a NACK.
- Transfers without a chunk for 60 s are abandoned. Completed IDs are remembered for 10 minutes to reject replays.

Finished files land in `Downloads\OpenDesk` and are put on the clipboard (and pasted when the sender asked and a
session runs). Files received this way are never sent back to the partner they came from by copy-paste
(`src/domain/received_files.rs`): for 10 minutes, the clipboard watcher, Ctrl+V and focus changes leave out the
received items themselves, anything inside a received folder, and copies of them pasted elsewhere (same name,
and for a file the same size). Each item left out is logged with its reason. Other partners may still get them,
and a drop or the "Send files" button always sends.
