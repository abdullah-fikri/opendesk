# Relay file lane

When neither the direct file channel (`file-channel`) nor a direct P2P path reaches the partner, a file used to go
over the single relay WebSocket. There it shared one TCP connection with the session's video, voice and input:
the client sent it in bursts of 8 units per 1 ms tick behind them, paced by the relay round trip
(`service/relay/file_pacer.rs`), the sender slept 3 ms per chunk, and the relay queued it behind video again
(`usecase/outbox.go`, one file message per 8 others). Between sites where hole punching fails this was the only
path, and the slowest. AnyDesk opens a connection of its own for files; with `file-lane` OpenDesk does the same
through the relay: a second, file-only WebSocket per device, its **file lane**.

Implemented in:

- Relay: `server-relay/internal/usecase/lane.go` (tokens, attach, forwarding, cut transfers), `file_grant.go`
  (`routeFile`: one authorization for both connections), `relay.go` (a lane ends with its registration),
  `handler/lane.go` (wire), `dto/lane.go`. Tests: `usecase/lane_test.go`, `handler/lane_test.go`,
  `e2e/file_lane_e2e_test.go`.
- Desktop: `client-desktop/src/service/relay/connection/file_lane.rs` (the lane's socket thread),
  `connection/lane.rs` (when to open it, sending, receiving, loss), `domain/file_route.rs` (`Path::Lane`),
  `infra/e2e.rs` (the lane's nonce domain).

## When it applies

Only when the relay lists `file-lane` in `registered-ack` and the partner lists `file-lane` (with `file-binary`)
in `peerCaps`. Path order for a new transfer: file channel, then the direct P2P path, then the lane, then the
main relay socket (`Path::choose`). A transfer keeps the path it started on. Older relays and partners see
exactly the old behavior. Switches: `OPENDESK_FILE_LANE_DISABLED=1` on the relay withdraws the capability;
`OPENDESK_FILE_LANE=0` on a desktop client neither lists nor uses it.

## Wire

All messages are JSON text unless noted; the lane connects to the same URL as the main connection.

| Direction | Connection | Message |
|---|---|---|
| client → relay | main | `{"type":"file-lane-request"}` |
| relay → client | main | `{"type":"file-lane-token","token":"<32 hex>"}` |
| client → relay | lane (first message) | `{"type":"file-lane-attach","deskId":"<id>","token":"<token>"}` |
| relay → client | lane | `{"type":"file-lane-ack"}`, or `{"type":"error","message":"File lane refused."}` and close |
| client → relay | lane | `relay-file` with action `start`, `chunk` or `end`; binary `0x10` + receiver clean ID + file chunk |
| relay → client | lane | `file-transfer` (with `fromClean`); binary `0x10` + sender clean ID + file chunk |
| relay → client | main | `{"type":"file-lane-lost","fromClean":"<id>","ids":["ft_…"]}` |

Offers, pulls, resume requests and acknowledgements stay on the main connection. A lane is pinged like a main
connection and dropped after the same silence.

## Decisions

### 1. Security

- **A lane is not a registration.** It attaches to an existing one: the device's main connection must be the
  current registration of its ID, not suspended or held for a resume, and must have listed `file-lane`.
- **One-time, short-lived token.** The main connection asks for a token (128 random bits) over its TLS socket;
  it is valid for 30 s, once, and only while that registration lasts (a newer registration of the ID, or a newer
  token, makes it useless). The relay compares it in constant time. A stranger without the token, with an old
  one, or with someone else's ID cannot attach, and is disconnected. The lane passes the same TLS check as a
  main connection (same endpoint), so a proxy in front of the relay needs no change.
- **Same authorization, not a weaker one.** Every lane message goes through `routeFile`, the function the main
  connection uses, as the lane's owner: `start` needs an active session with the target (`activePartnerLocked`,
  checked from both sides), creates the grant and counts against the 8-grant limit; `chunk` and `end` need that
  grant. On top: the lane must still be its owner's current lane, a transfer must continue on the connection it
  started on (a main-connection chunk cannot ride a lane grant or the other way round), binary chunks must name
  their receiver, and offers, pulls and acks are refused on a lane.
- **End to end.** Chunks are sealed exactly like relay chunks (`E2e::seal_lane_file`), so the relay still sees
  only the routing header and ciphertext. They use a nonce domain of their own (`Stream::LaneFile`): every
  relay stream accepts only increasing nonces, and the two sockets may deliver main and lane chunks interleaved
  in another order than they were sealed.

### 2. Throughput and the main socket's latency

- Client: the lane's own thread writes as long as the socket takes data (8 MiB send buffer, ~280 Mbps at 234 ms
  to a far relay); the 1 ms burst budget and the relay pacer do not apply, and the lane is a fast path for the
  sender (`file_transfer::set_fast_path`), so the 3 ms sleep per chunk does not either. In-flight units are
  released when a message is in the lane socket, so the sender's existing in-flight limit is its flow control.
- Relay: a receiving lane has its own writer and a 16 MiB / 256-message queue with nothing ahead of it. Video,
  voice and input of the session never wait behind lane traffic: they are on another socket and another writer.

### 3. Order

A transfer is delivered on one receiver connection, chosen at `start`: the receiver's lane when it has one,
else its main connection (a receiver without a lane still benefits from the sender's). While transfers from
one lane to a receiver run, new ones from that lane follow the same receiver connection, so the receiver gets a
lane's chunks in sealing order. Clipboard text and other main-connection messages are independent of lane
traffic, as with the file channel and the P2P path.

### 4. Backpressure and memory

A full receiving-lane queue makes the forwarding read loop of the sending lane wait for room, which stops
reading that socket, and TCP slows the sender: one slow receiver costs the relay at most its 16 MiB queue
(plus socket buffers), never unbounded memory. A receiver that takes nothing for the stall limit (10 s, the
socket write bound) is closed, like a stalled main connection. The client reader stops reading above 32 MiB
not yet handled, so a slow disk throttles the sender the same way.

### 5. Loss and resume

- A lane that drops (or is replaced by a newer one) cuts its transfers: the relay deletes their grants and tells
  the partner on the other end which ones on its main connection (`file-lane-lost`). The side whose lane dropped
  knows by itself.
- Both ends then do what a lost file channel does: the receiver parks what arrived and asks the sender to resume
  (`file-resume`), the sender's cut sends resume on whatever path is up (the main socket while the lane is
  down). A lane ends with its main connection; then everything is aborted as for any lost relay connection.
- The client reattaches automatically with a new token, after 1 s, doubling to 30 s; a lane that stayed up 30 s
  starts over at 1 s. It opens the lane while a session partner can use it and closes it 2 minutes after the
  last one, unless a transfer still runs on it.

## Open items

- Measure throughput against AnyDesk between the two sites through the VPS relay (the lane removes the client
  and relay pacing; the remaining limit is the two TCP legs).
- The relay bounds memory per receiving lane, not in total; with many simultaneous slow receivers the total is
  their count times 16 MiB.
- Android does not list `file-lane` yet.
