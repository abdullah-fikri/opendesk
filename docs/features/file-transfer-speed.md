# File transfer speed

What makes file transfers fast, and what changed on the wire for it. Everything here falls back to the earlier
behaviour toward peers that do not know it; no new capability name was added (the relay keeps only 32 per client,
and a desktop build lists that many already).

## 1. Pipelined files (sender only)

A sender used to wait for the receiver's verdict (`ack`) on file N before starting file N+1: one round trip per
file, which dominated folders of many small files. Now the next file starts as soon as the previous file's `end`
is queued, and files wait for their verdicts together:

- **Window**: up to 32 files and folder entries wait for their verdict at once when the transfer runs over a
  direct path (P2P path or file channel), 4 over the relay. On the relay every transfer holds one of the 8 grants
  the relay allows per peer until its `ack` passes, and the file being streamed and up to two pulled files need
  theirs too.
- **Order** is unchanged on the wire: `start`, chunks, `end` of each file, files one after another. The receiver
  handles each `end` (verify, save) before anything after it, so nothing changes for it.
- **Groups**: a group's end marker (`start` with `groupEnd`) is sent only once every verdict before it is in, so the
  receiver closes the group with every file in it, including files resumed after a cut.
- **Failures** stay per file: a NACK or a verdict timeout (120 s after its `end`) fails only that file; a cut
  connection resumes every waiting file (oldest first, still within the window) from where the receiver stands.
- **Receivers without `file-ack`** (Android) send no verdict; a file counts as sent once all of it, `end` included,
  left this side, as before.
- **Transfer IDs** of sent files are now `ft_<micros>_<n>` (a counter keeps files started in the same microsecond
  apart). Receivers only require `[A-Za-z0-9_]{1,80}`.

### Resume of a file that had arrived

A connection lost while several files wait for their verdict may take only the receiver's `ack` with it. The
sender then resumes such a file (`start` with `resumeOf`) like any cut one. A receiver of this version that finds
the cut transfer among the ones it completed answers `resumeAt` = the file's size and confirms the following `end`
with an ACK, without receiving or saving the file again. Older receivers start such a file over (a second copy, as
before this change).

## 2. File channel: reconnect

A lost file channel (`file-channel`) used to stay down for the rest of the session. Now it is set up again:

- The side that loses it waits 1 s, then starts a fresh channel (new port, new candidates in a `p2p-signal`, the
  same `Noise_XX` handshake and peer-key check against the key the session's end-to-end handshake authenticated).
  A try that is not up within 20 s counts as failed; the wait doubles per failed try up to 30 s, at most 8 tries in
  a row. A channel that then stays up 60 s earns a fresh count.
- A side whose channel is down and that receives new candidates from the partner (the partner set its side up
  again) answers at once instead of waiting out its back-off. A channel still up or still connecting only connects
  to the new candidates, so two sides never keep replacing each other's fresh channel.
- Transfers cut by the loss resume as before (`file-resume`). A sender whose transfer was cut waits up to 10 s for
  the channel to come back before it resumes, so a large file does not finish on the slower relay path.

## 3. File channel: parallel streams

One TCP flow is held back by its own congestion window; a single loss halves it, and on a long path it takes
seconds to grow back. A channel may now run up to 8 TCP streams (default 4, `OPENDESK_FILE_STREAMS=N` on either
side, `1` turns it off). Each stream is its own TCP connection with its own `Noise_XX` handshake and key check.

Channel control messages (plaintext of the Noise frames, `[kind u8][length u32 BE][payload]`, see
`domain::file_channel`):

| Kind | Name | Direction | Payload |
| --- | --- | --- | --- |
| `0xF0` | select | viewer → host, first message of the picked connection | empty (one stream), or `[streams u8][token u64 BE]` |
| `0xF1` | ping | both | empty |
| `0xF2` | join | viewer → host, first message of an extra stream | `[token u64 BE]` of the pick |
| `0xF3` | hello | host → viewer, first message, only after a pick asking for more than one stream | `[streams u8]` |
| `0xF4` | seq | both, only when striping | `[seq u64 BE]` of the next message on this stream |

Negotiation:

1. The viewer's pick offers its stream count and a random token. A viewer offering one stream (or one from before
   this change) sends an empty pick.
2. A host that can stripe answers a pick asking for more than one stream with `hello` (its own count). Both sides
   then use `min(viewer, host)` streams. A host from before this change ignores the pick's payload and sends no
   hello: the viewer keeps one stream.
3. The viewer opens the extra streams to the address the picked connection runs to, from fresh ports, each
   starting with `join` and the token. The host accepts extra streams only for the current pick's token and only
   up to the agreed count. An extra stream that cannot connect (a NAT that let only the simultaneous open
   through) leaves the channel with fewer streams.

Striping: once a side knows its partner can (the host from the pick, the viewer from the hello), each message it
sends is preceded by `seq`, numbered from 0 per direction, and goes to whichever stream is free first. Messages
sent before striping began and the first striped one (`seq` 0) always take the picked stream, so they need no
reordering. The receiving side hands messages on in `seq` order: a message that arrives early waits for the earlier
ones still on another stream. Memory stays bounded: at most 16 MB wait for reordering (a stream over that stops
reading; the stream carrying the next message never does), and at most 32 MB wait for the relay connection thread
(`RECEIVE_WINDOW`, as before). A lost stream ends the whole channel (its messages took the order with them); the
channel is then set up again as in section 2.

## 4. Large files

Files up to 1 TiB, folders up to 4 TiB (receivers of this version; sizes and chunk offsets were 64-bit already,
the `deflated` flag is offset bit 63). Senders keep to the old limits (2 GiB per file, 32 GiB per folder) toward
partners that do not list `file-channel`: Android and desktop builds up to 1.5.x refuse anything bigger. Every
build that lists `file-channel` takes large files, so that capability stands for it. Clipboard offers follow the
same limits.

## 5. Throttles

- In flight: senders wait while more than ~1.5 MB of file messages are queued over the relay or the P2P path
  (video latency). Over the file channel the cap is 16 MB, so every stream stays busy on a long fat path; the
  channel's queue no longer blocks the relay connection thread (it is bounded by this cap instead).
- The 3 ms pause between chunks applies only on the relay path, as before.
- Each stream gathers its frames into one write of up to 256 KiB (a 128 KiB chunk is one system call, not three).

## To measure on real networks

- Loopback throughput, 1 vs 4 streams: `cargo test --release throughput -- --ignored --nocapture` in
  `client-desktop`.
- Still to benchmark against AnyDesk: a folder of 10 000 small files on LAN and on a 50-100 ms path; one 10 GB file
  over a lossy long path with 1 vs 4 streams; a channel drop in the middle of a large file (time until it moves
  again on the channel).
