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
| `register` | `deskId`, `deviceToken`, `platform`, `deviceName`, `isAdmin`, `caps[]` | Announce this device. `deviceToken` is checked only when the relay has credentials configured or its device allowlist on; then `isAdmin` is ignored in favor of the relay's own flag (see [Device security](#device-security)). |
| `get-hosts` | | Administrators only: request the device registry. |
| `connect-request` | `targetId` | Open a viewer session to a device. |
| `approval-response` | `fromClean`, `accept` | Legacy: an Android host (app before the viewer-only release) answers an `incoming-request`. |
| `relay-input` | `targetClean`, `event` | Input or control event for the partner (see [Input events](#input-events)). |
| `relay-clipboard` | `targetClean`, `text` | Clipboard text for the partner. |
| `relay-file` | `targetClean`, `payload` | One file-transfer message (see [File transfer](#file-transfer)). |
| `session-disconnect` | `targetClean` | End this client's session with that partner. |
| `p2p-signal` | `targetClean`, `candidate` | Noise handshake and UDP candidates for a direct path, and the `e2e` handshake (max 4096 bytes; per partner bursts of 4, then 10 per second). |
| `monitor-set` | `targets[]` | Administrators with `monitor-view` only: replace the hosts on the monitor wall (at most 16; empty stops). See [Monitor wall](#monitor-wall). |
| `security-get` | | Administrators only: request the device allowlist (`security-state`). |
| `security-set` | `enabled` | Administrators only: turn the device allowlist on or off. |
| `security-allow` | `targetId`, `admin` | Administrators only: list a device (replacing its role if listed). |
| `security-allow-online` | | Administrators only: list every connected device, keeping administrators' role. |
| `security-remove` | `targetClean` | Administrators only: take a device off the list. |
| `security-reset` | `targetClean` | Administrators only: forget a device's token; its next connection enrolls again. |
| `host-rename` | `targetClean`, `alias` | Administrators only: name a device in the host list (at most 64 characters; empty clears it). |

## Relay to client

| `type` | Fields | Meaning |
| --- | --- | --- |
| `registered-ack` | `deskId`, `caps[]` | Registration accepted; `caps` lists relay features. |
| `host-list` | `hosts[]`: `clean_id`, `desk_id`, `device_name`, `alias`, `platform`, `online`, `last_seen` (RFC 3339) | Device registry (administrators). `alias` is the name an administrator gave the device, empty when none. |
| `incoming-request` | `fromId`, `fromClean`, `deviceName` | Sent to Android peers only: a viewer asks to connect. Current apps are `viewer-only` and never get one; older apps still answer it. |
| `session-connected` | `targetId`, `targetClean`, `role` (`viewer`/`host`), `peerCaps[]` | The session is live; `role` is the receiver's role. |
| `input-command` | `event`, `fromClean`* | A relayed `relay-input`. |
| `clipboard-sync` | `text`, `fromClean`* | A relayed `relay-clipboard`. |
| `file-transfer` | `fromClean`, `payload` | A relayed `relay-file`. `fromClean` is stamped by the relay. |
| `p2p-signal` | `fromClean`, `candidate` | A relayed signal. |
| `udp-relay-offer` | `targetClean`, `addr`, `token` | The relay's UDP forwarder for the session with `targetClean`: bind with the 32-hex-digit token at `addr` (`ip:port`). Sent after `session-connected` when both clients list `p2p.v1` and `udp-relay`. See `client-desktop/docs/features/p2p.md`. |
| `session-closed` | `message`, `targetClean`* | The session ended (partner left, disconnected, or reconnected). |
| `error` | `message`, `targetClean`* | A request failed (invalid ID, target offline or busy, rejected). |
| `monitor-state` | `targetClean`, `state` (`live`, `offline`, `unsupported`, `denied`) | Monitor wall: what one tile shows (administrators with `monitor-view`). |
| `monitor-start` | `maxWidth`, `fps` | Monitor wall: an administrator watches this host; stream thumbnails (hosts with `monitor-host`). |
| `monitor-stop` | | Monitor wall: nobody watches this host any more. |
| `device-credential` | `token` | The relay enrolled this device: present `token` as `deviceToken` to this relay from now on. |
| `security-state` | `enabled`, `managedByEnv`, `devices[]`: `cleanId`, `deskId`, `name`, `admin`, `enrolled`, `enrolledAt` (RFC 3339 or empty), `online` | The device allowlist (administrators), after `security-get` and after every change. |

\* Only for multi-session viewers (see [docs/features/multi-session.md](features/multi-session.md)).

## Sessions

- A desktop host (Windows, Linux) accepts every `connect-request` immediately (unattended access).
- A target that registered with `viewer-only` (the Android app, Linux desktops in a Wayland session) is never a
  host: the relay answers the `connect-request` with `error` ("can only view other devices").
- Legacy Android apps without `viewer-only` receive `incoming-request` and the session stays pending until they
  answer.
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
| `file-binary` | client + relay | File chunks as binary messages (`0x12`, below) instead of base64 JSON. The relay sends them only to clients that list it. |
| `cursor-shape` | client | Draws its own cursor from `cursor-shape` events; the host stops painting it. |
| `video-delta` | client | Framed key/delta video; without it the host sends plain JPEGs. |
| `video-h264` | client | H.264 inside the framed format. |
| `frame-stamp` | client | Framed video to this viewer may end in a capture stamp (below) for the stats overlay's latency rows. |
| `audio-opus` | client | Decodes Opus voice (`[0x02][0x4F]...`); voice to it is Opus, to everyone else PCM. |
| `p2p.v1` | client | Direct encrypted UDP path. |
| `udp-relay` | client + relay | The direct path may run through the relay's UDP forwarder (`udp-relay-offer`). |
| `e2e` | client | End-to-end encryption: `Noise_XX` with device keys over `p2p-signal`, then sealed relay media and input events (see [Binary messages](#binary-messages) and `client-desktop/docs/features/e2e.md`). Needs the relay's `p2p-signal`. |
| `multi-session` | client + relay | Up to four viewer sessions per connection. |
| `monitor-host` | client | Streams monitor-wall thumbnails on `monitor-start` (every desktop edition). |
| `monitor-view` | client | Shows the monitor wall: `monitor-state` and the `0x11` tag (admin edition). |
| `viewer-only` | client | This device never hosts; the relay refuses connect requests to it. |
| `host-features` | client | The host lists its optional host controls below. Without it the host is a legacy Windows host that has all of them. |
| `privacy-screen` | client, with `host-features` | `set-privacy` works on this host. |
| `block-input` | client, with `host-features` | `set-block-input` works on this host. |
| `fps-N` (e.g. `fps-144`) | client | The viewer wants at most N frames per second; without it the host streams 60. Listed after every other client capability (relays up to 1.2.0 keep only the first 16 names, newer ones 32). |
| `perf-low`, `perf-medium`, `perf-high` | client | The viewer's performance preset: the host halves (Low) or doubles (High) its H.264 bitrate, and on High switches motion to H.264 at once. Without one the host uses Medium. Listed after `fps-N`; Medium is not listed. |
| `p2p-signal`, `file-grant`, `file-offer`, `file-binary`, `caps`, `monitor`, `device-security`, `host-alias` | relay | Relay features in `registered-ack`. |

Per platform (see [platforms.md](platforms.md)):

- Windows: `file-ack`, `file-offer`, `file-folder`, `file-binary`, `cursor-shape`, `video-delta`, `frame-stamp`,
  `video-h264` (when the Media Foundation decoder exists), `audio-opus`, `p2p.v1`, `multi-session`. No `host-features`: Windows hosts have every
  host control.
- Linux: the same without `file-offer`, with `video-h264` only when Cisco's OpenH264 is installed, plus
  `host-features` (without `privacy-screen` and `block-input`), plus `viewer-only` in a Wayland session.
- Android: `viewer-only`, `video-delta` and `e2e`, so it receives framed JPEG video (no H.264) and single-session
  message shapes.

`register.platform` is `windows-pc`, `windows-admin`, `windows-host`, `linux-pc`, `linux-admin`, `linux-host` or
`android`; it is informational except for `android`.

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
| `set-performance` | `preset` (`low`, `medium`, `high`), `maxFps` (0 = none) | viewer to host: at session start and when the preset changes; replaces the registration's `perf-*` and `fps-N` for this session |
| `set-mic` | `muted` | either; only `muted: true` is honored |
| `cursor-shape` | `shape` (0 hidden, 1 arrow, 2 text, 3 hand, ...) | host to viewer |
| `monitors` | `list[]` (`index`, `width`, `height`, `primary`), `active` | host to viewer |
| `stats` | `on` | viewer to host: stats overlay shown or hidden |
| `stats-ping` | `t` (viewer clock, ms) | viewer to host, once a second while the overlay is shown |
| `stats-pong` | `t` (copied from `stats-ping`), `h` (host monotonic clock, µs; absent up to 1.2.0) | host to viewer, at once |
| `host-stats` | `fps`, `targetFps`, `grabMs`, `grabMaxMs` (capture work, without waiting for a screen change), `encodeMs`, `encodeMaxMs`, `kbps`, `idle`, `gateSkips`, `congestion` (0..1), `encoder` (H.264 name, empty = JPEG), `cpu` (host process, % of the machine), `ramMb` (its working set) | host to viewer, once a second between `stats` on and off |

Peers ignore actions they do not know, so a viewer never gets `host-stats` from a host that predates it.

## File transfer

`relay-file.payload`, selected by `action`. The relay authorizes on `action` and `id` only:

| `action` | Fields | Rule |
| --- | --- | --- |
| `start` | `id`, `name`, `size`; folders: `dir`, `group`, `rel`, `groupEnd`, `autoPaste` | Needs an active session; creates a grant bound to the two connections. |
| `chunk` | `id`, `chunk` (base64, at most 32 KiB decoded) | Needs the grant. With `file-binary` a binary chunk replaces it. |
| `end` | `id`, `name`, `sha256` (hex), `autoPaste` | Needs the grant. |
| `ack` | `id`, `ok` | Receiver to sender; consumes the grant. |
| `offer` | `id`, `files[]` (`name`, `size` or `dir`) | Clipboard offer; needs an active session. |
| `pull` | `id` (offer), `index`, `tid` | Ask the offerer to send entry `index` as transfer `tid`. |

A grant lets a transfer finish after its screen session ends, for 60 s of idleness, but only between the
same two connections. At most 8 grants per sender. See `client-desktop/docs/features/file-transfer.md`.

When the receiver and the relay both list `file-binary`, chunks are binary messages (below) and everything else
stays JSON. The relay authorizes a binary chunk exactly like a JSON one: the grant for this sender, `id` and
receiver; anything else is dropped. Everything from `start` to `end` (JSON and binary) goes through the
receiver's file queue in order, and `relay-clipboard` text never overtakes file messages its sender queued
before it.

## Binary messages

```text
voice   [0x02][rate_khz: 16|24|48][PCM 16-bit little-endian mono ...]
voice   [0x02][0x4F][seq u16 big-endian][Opus packet]   20 ms mono, only when both sides list audio-opus
video   [0xFF 0xD8 ...]   a plain JPEG, or a framed message (below)
tagged  [0x10][9 ASCII digits: host clean ID][voice or video]   multi-session viewers only
monitor [0x11][JPEG]                                           host to relay: monitor-wall thumbnail
monitor [0x11][9 ASCII digits: host clean ID][JPEG]            relay to administrator
file    [0x12][id length: 1..80][id][offset: u64 big-endian][data: at most 128 KiB]   sender to relay (file-binary)
file    [0x10][9 ASCII digits: sender clean ID][file chunk]   relay to receiver, always tagged
```

Between two peers that list `e2e`, once their handshake completed, video, voice and file chunks are sealed and
keep only their routing header in clear: `[FF D8 00 4F ver kind flags 0xE2]`, `[0x02 0xE2]` or
`[0x12 len id FF*8]`, then a nonce and the encrypted original message. The relay reads these exactly as before.
Input events between them are `{"action":"e2e","d":...}`; receivers that do not use `e2e` never get one.

A multi-session viewer may put the session tag in front of a file chunk it sends, as for voice; the grant
decides the receiver either way. The relay always tags file chunks it delivers with the sender, like `fromClean`
on `file-transfer`. Receivers append a chunk only at exactly the current `offset`.

Framed video (`video-delta`), all integers big-endian:

```text
0   FF D8 00 4F   magic (starts like a JPEG; 0x00 never follows SOI in a real JPEG)
4   version = 1
5   kind: 1 key (payload = JPEG), 2 delta (payload = JPEG atlas), 3 H.264 access unit
6   flags: bit 0 = H.264 IDR, bit 1 = stamped
7   reserved
8   seq (u32): a delta applies only on top of seq-1
12  width, height (u16)
16  move count, patch count (u16)
20  moves   (src_x, src_y, dst_x, dst_y, w, h) x u16, applied first
    patches (dst_x, dst_y, w, h, atlas_x, atlas_y) x u16, blitted from the payload
    payload
    stamp trailer, only with flag bit 1 (viewers that list frame-stamp):
      captured_us (u64): host monotonic clock at capture, µs
      host_us (u32): capture to hand-off for sending on the host, µs
```

The viewer maps `captured_us` onto its own clock with the offset from `stats-ping`/`stats-pong` (`h` taken as
halfway through the round trip) to split a frame's latency into host, network, decode and display.

Opus voice keeps the `0x02` marker, so every relay version forwards it as audio. `0x4F` is never a rate tag;
`seq` counts packets so the receiver can rebuild one lost frame from the next packet's in-band FEC and conceal
short gaps. Receivers always accept PCM, and a sender uses Opus only toward a partner that listed `audio-opus`.

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

## Device security

Administrator-managed allowlist with automatic enrollment (details in
[docs/features/device-security.md](features/device-security.md)).

- Off by default: every device is admitted with its claimed role.
- On (or `OPENDESK_DEVICE_CREDENTIALS` set): a device listed in `OPENDESK_DEVICE_CREDENTIALS` needs that token.
  Otherwise an unlisted device receives `error` and the connection closes; a listed device without a token is
  admitted and receives `device-credential` right after `registered-ack`; a listed device with a token must
  present it.
- `security-set {enabled: true}` lists the administrator who sent it. Every change applies to connected devices
  at once: listed devices without a token receive `device-credential`, unlisted ones receive `error` and are
  disconnected.

## Compatibility

- Unknown message types and unknown event actions are ignored by every component.
- New fields are optional; never change the meaning of an existing one.
- New behavior is gated by a capability. A capability is only ever added, never redefined.
- Message shapes for clients without a capability stay exactly as they were (for example `fromClean` and
  `targetClean` are added only for multi-session viewers).
