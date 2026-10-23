# File transfer

Code: `src/service/file_transfer/` (`sender.rs`, `receiver.rs`, `offer.rs`, `tree.rs`), payload type
`src/protocol/file_payload.rs`, limits and path rules `src/domain/transfer.rs`, paste streams
`src/infra/virtual_clipboard.rs`. Relay rules: [file grants](../../../server-relay/docs/features/file-grants.md).

## Ways to send

| Trigger | Behavior |
| --- | --- |
| Drop files on the window | Sent to the current partner and pasted into its focused Explorer window |
| "Send files" button | File picker, then the same |
| Ctrl+V in a session with files on the clipboard | Sent and pasted, unless the host supports offers |
| Copy files while a partner supports `file-offer` | Only names and sizes are sent; the partner's clipboard shows virtual files, and each paste pulls one file |
| Copy files on a host without offers | Sent to the viewer's clipboard |

## Sending

Each file is `start`, 32 KiB base64 chunks and `end` with its SHA-256. Senders wait while more than 48 chunks are
queued, so progress follows the network. With `file-ack` the receiver confirms; "Sent" then means verified on
the other side, and a missing confirmation fails after 120 s. Without it (Android) "Sent (not confirmed by the
receiver)" only means every byte left this machine. A lost relay connection fails every transfer on it; there is
no resume.

Folders go only to `file-folder` receivers: folder entries first, then files with their relative path, then a
repeat of the first folder entry that marks the end. For other receivers folders are skipped with a notice.

## Receiving

SECURITY: everything from the peer is validated before it touches the disk.

- IDs: letters, digits and `_`, at most 80 characters. At most 8 files and 4 folder transfers at once.
- Names and relative paths pass `domain::transfer::safe_rel_path`: no absolute or UNC paths, drive letters,
  streams, `..`, device names (`CON`, `NUL`, `COM1`...), invalid characters or over-long paths.
- Limits: 2 GB per file; 10 000 entries and 32 GB per folder transfer.
- Bytes go to a `.part` file and are linked under the final name only when size and SHA-256 match. Existing
  files and folders are never overwritten or merged into; a copy gets `name (1)`.
- Transfers without a chunk for 60 s are abandoned. Completed IDs are remembered for 10 minutes to reject replays.

Finished files land in `Downloads\OpenDesk` and are put on the clipboard (and pasted when the sender asked and a
session runs). Files received this way are recognized by the clipboard watcher and never sent back.
