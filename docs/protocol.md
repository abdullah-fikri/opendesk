# Wire protocol

Every client keeps one WebSocket to the relay. Text messages are JSON objects with a `type`; binary messages
carry voice and video. This document is the contract between `server-relay`, `client-desktop` and
`client-android`. Change it first, then the code, and keep older clients working (see
[Compatibility](#compatibility)).

Type definitions:

| Component | Outbound | Inbound |
| --- | --- | --- |
| Relay (Go) | `server-relay/internal/dto/outbound.go` | `server-relay/internal/dto/inbound.go` |
| Desktop (Rust) | `client-desktop/src/protocol/message.rs` (`ClientMessage`) | same file (`ServerMessage`) |
| Android (Kotlin) | `client-android/.../data/dto/RelayMessages.kt` | same file |

## Connection

- Transport: `wss://` through a TLS terminator, or `ws://` on loopback. The relay refuses plaintext from other
  addresses unless `OPENDESK_TLS_PROXY=1` says TLS ends at a proxy in front of it.
- The relay pings every 15 s (`ping_interval_seconds`) and drops a connection after 4 missed intervals.
  Clients must answer WebSocket pings (every standard library does).
- Largest message: 16 MiB.
- The first message must be `register`. Everything else is ignored until `registered-ack`.
- A second connection registering the same desk ID replaces the first; the old one's partners receive
  `session-closed`.

## Desk IDs

Nine digits, no leading zero. Clients may send them formatted (`"123 456 789"`); the relay strips non-digits.
The digits-only form is called the *clean* ID and is what `targetClean` / `fromClean` carry.

## Client to relay

| `type` | Fields | Meaning |
| --- | --- | --- |
| `register` | `deskId`, `deviceToken`, `platform`, `deviceName`, `isAdmin`, `caps[]` | Announce this device. `deviceToken` is checked only when the relay has credentials configured; then `isAdmin` is ignored in favor of the relay's own flag. |
| `get-hosts` | | Administrators only: request the device registry. |
| `connect-request` | `targetId` | Open a viewer session to a device. |
| `approval-response` | `fromClean`, `accept` | An Android host answers an `incoming-request`. |
| `relay-input` | `targetClean`, `event` | Input or control event for the partner (see [Input events](#input-events)). |
| `relay-clipboard` | `targetClean`, `text` | Clipboard text for the partner. |
| `relay-file` | `targetClean`, `payload` | One file-transfer message (see [File transfer](#file-transfer)). |
| `session-disconnect` | `targetClean` | End this client's session with that partner. |
| `p2p-signal` | `targetClean`, `candidate` | Noise handshake and UDP candidates for a direct path (max 4096 bytes, 10 per second per partner). |
| `monitor-set` | `targets[]` | Administrators with `monitor-view` only: replace the hosts on the monitor wall (at most 16; empty stops). See [Monitor wall](#monitor-wall). |

## Relay to client

| `type` | Fields | Meaning |
| --- | --- | --- |
| `registered-ack` | `deskId`, `caps[]` | Registration accepted; `caps` lists relay features. |
| `host-list` | `hosts[]`: `clean_id`, `desk_id`, `device_name`, `platform`, `online`, `last_seen` (RFC 3339) | Device registry (administrators). |
| `incoming-request` | `fromId`, `fromClean`, `deviceName` | Sent to Android hosts only: a viewer asks to connect. |
| `session-connected` | `targetId`, `targetClean`, `role` (`viewer`/`host`), `peerCaps[]` | The session is live; `role` is the receiver's role. |
| `input-command` | `event`, `fromClean`* | A relayed `relay-input`. |
| `clipboard-sync` | `text`, `fromClean`* | A relayed `relay-clipboard`. |
| `file-transfer` | `fromClean`, `payload` | A relayed `relay-file`. `fromClean` is stamped by the relay. |
| `p2p-signal` | `fromClean`, `candidate` | A relayed signal. |
| `session-closed` | `message`, `targetClean`* | The session ended (partner left, disconnected, or reconnected). |
| `error` | `message`, `targetClean`* | A request failed (invalid ID, target offline or busy, rejected). |
| `monitor-state` | `targetClean`, `state` (`live`, `offline`, `unsupported`, `denied`) | Monitor wall: what one tile shows (administrators with `monitor-view`). |
| `monitor-start` | `maxWidth`, `fps` | Monitor wall: an administrator watches this host; stream thumbnails (hosts with `monitor-host`). |
| `monitor-stop` | | Monitor wall: nobody watches this host any more. |

\* Only for multi-session viewers (see [docs/features/multi-session.md](features/multi-session.md)).

## Sessions

- A Windows host accepts every `connect-request` immediately (unattended access). An Android host receives
  `incoming-request` and the session stays pending until it answers.
- A peer that hosts or views cannot be taken as a host by someone else.
- Every relayed message is authorized: the relay forwards only between the two connections of an active
  session, checked from both sides.

## Capabilities

Clients list what they understand in `register.caps`; the partner sees them in `session-connected.peerCaps`.
Features are used only when both sides (and, where noted, the relay) support them.

| Capability | Who | Meaning |
| --- | --- | --- |
| `file-ack` | client | Answers `end` with an `ack`. |
| `file-offer` | client + relay | Copy-paste of files by offer and pull. |
| `file-folder` | client | Folder transfers (`group`/`rel`). |
| `cursor-shape` | client | Draws its own cursor from `cursor-shape` events; the host stops painting it. |
| `video-delta` | client | Framed key/delta video; without it the host sends plain JPEGs. |
| `video-h264` | client | H.264 inside the framed format. |
| `p2p.v1` | client | Direct encrypted UDP path. |
| `multi-session` | client + relay | Up to four viewer sessions per connection. |
| `monitor-host` | client | Streams monitor-wall thumbnails on `monitor-start` (every desktop edition). |
| `monitor-view` | client | Shows the monitor wall: `monitor-state` and the `0x11` tag (admin edition). |
| `p2p-signal`, `file-grant`, `file-offer`, `caps`, `monitor` | relay | Relay features in `registered-ack`. |

Android advertises no capabilities, so it always receives plain JPEGs and single-session message shapes.

## Input events

`relay-input.event` / `input-command.event` objects, selected by `action`. Coordinates `normX`/`normY` are
0..1 of the streamed monitor.

| `action` | Fields | Direction |
| --- | --- | --- |
| `mousemove` | `normX`, `normY` | viewer to host |
| `mousedown`, `mouseup`, `click`, `dblclick` | `normX`, `normY`, `button` (0 left, 2 right) | viewer to host |
| `wheel` | `normX`, `normY`, `deltaY` | viewer to host |
| `keydown`, `keyup` | `keyCode` (Win32 virtual-key) | viewer to host |
| `keycombo` | `keyCode`, `ctrl`, `alt`, `shift` | viewer to host |
| `text` | `text` | viewer to host (typed characters) |
| `clipboard` | `text` | viewer to host, right before a forwarded Ctrl+V |
| `shortcut` | `name`: `taskmgr`, `win`, `explorer`, `desktop`, `alt-tab` | viewer to host |
| `switch-monitor` | | viewer to host: next monitor |
| `select-monitor` | `index` | viewer to host |
| `set-resolution` | `mode` (0 native, 480/720/1080), optional `auto`, `width`, `height` | viewer to host |
| `set-privacy` | `mode`: 0 normal, 1 frozen, 2 maintenance, 3 black | viewer to host |
| `set-block-input` | `blocked` | viewer to host |
| `video-keyframe` | | viewer to host: delta chain broken |
| `set-audio-quality` | `quality`: 0 = 16 kHz, 1 = 24 kHz, 2 = 48 kHz | either |
| `set-mic` | `muted` | either; only `muted: true` is honored |
| `cursor-shape` | `shape` (0 hidden, 1 arrow, 2 text, 3 hand, ...) | host to viewer |
| `monitors` | `list[]` (`index`, `width`, `height`, `primary`), `active` | host to viewer |

## File transfer

`relay-file.payload`, selected by `action`. The relay authorizes on `action` and `id` only:

| `action` | Fields | Rule |
| --- | --- | --- |
| `start` | `id`, `name`, `size`; folders: `dir`, `group`, `rel`, `groupEnd`, `autoPaste` | Needs an active session; creates a grant bound to the two connections. |
| `chunk` | `id`, `chunk` (base64, at most 32 KiB decoded) | Needs the grant. |
| `end` | `id`, `name`, `sha256` (hex), `autoPaste` | Needs the grant. |
| `ack` | `id`, `ok` | Receiver to sender; consumes the grant. |
| `offer` | `id`, `files[]` (`name`, `size` or `dir`) | Clipboard offer; needs an active session. |
| `pull` | `id` (offer), `index`, `tid` | Ask the offerer to send entry `index` as transfer `tid`. |

A grant lets a transfer finish after its screen session ends, for 60 s of idleness, but only between the
same two connections. At most 8 grants per sender. See `client-desktop/docs/features/file-transfer.md`.

## Binary messages

```text
voice   [0x02][rate_khz: 16|24|48][PCM 16-bit little-endian mono ...]
video   [0xFF 0xD8 ...]   a plain JPEG, or a framed message (below)
tagged  [0x10][9 ASCII digits: host clean ID][voice or video]   multi-session viewers only
monitor [0x11][JPEG]                                           host to relay: monitor-wall thumbnail
monitor [0x11][9 ASCII digits: host clean ID][JPEG]            relay to administrator
```

Framed video (`video-delta`), all integers big-endian:

```text
0   FF D8 00 4F   magic (starts like a JPEG; 0x00 never follows SOI in a real JPEG)
4   version = 1
5   kind: 1 key (payload = JPEG), 2 delta (payload = JPEG atlas), 3 H.264 access unit
6   flags: bit 0 = H.264 IDR
7   reserved
8   seq (u32): a delta applies only on top of seq-1
12  width, height (u16)
16  move count, patch count (u16)
20  moves   (src_x, src_y, dst_x, dst_y, w, h) x u16, applied first
    patches (dst_x, dst_y, w, h, atlas_x, atlas_y) x u16, blitted from the payload
    payload
```

The relay keeps at most 8 queued frames per receiver. A key frame or IDR flushes the queue; a delta that does
not fit flushes it too, and the viewer then sends `video-keyframe`.

## Monitor wall

A read-only grid of many hosts for administrators (details in
[docs/features/monitor-wall.md](features/monitor-wall.md)). A watch is **not a session**: it never authorizes
input, clipboard, files, P2P signals or voice in either direction, does not count toward the four viewer sessions
and does not make a host busy.

- The administrator (admin role decided as for `get-hosts`, and `monitor-view`) sends `monitor-set` with the hosts
  on its visible page. Others receive `monitor-state {state: "denied"}` per target.
- The relay answers each new target with `monitor-state` and sends `monitor-start` to a watched host with
  `monitor-host` that is not streaming yet, once however many administrators watch it. Hosts without the
  capability are reported `unsupported` and never contacted.
- The host sends `[0x11][JPEG]`: plain JPEGs only (no framed video), at most 512 KiB, scaled to `maxWidth`, at most
  `fps` per second, and only when the screen changed. The relay forwards them only from a host it asked to stream,
  to every watcher, through one bounded queue per tile, and shows the newest one to a new watcher at once.
- When the last watcher leaves (new `monitor-set`, or its connection drops) the host receives `monitor-stop`. When
  the host drops, watchers receive `offline` and keep watching; it goes `live` again when it registers.

## Compatibility

- Unknown message types and unknown event actions are ignored by every component.
- New fields are optional; never change the meaning of an existing one.
- New behavior is gated by a capability. A capability is only ever added, never redefined.
- Message shapes for clients without a capability stay exactly as they were (for example `fromClean` and
  `targetClean` are added only for multi-session viewers).
