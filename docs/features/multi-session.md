# Multi-session viewing

A desktop viewer can control up to **four hosts at once**, shown as tabs or as a split grid. Hosts stay
single-session: a host controlled by one viewer refuses others. The admin edition's read-only
[monitor wall](monitor-wall.md) is separate: its watches are not sessions and count toward none of these limits.

Implemented in:

- Relay: `server-relay/internal/usecase` (`Peer.views`, `openSession`, `ForwardBinary`), tests in
  `multi_session_test.go` and `handler/handler_test.go`.
- Desktop: `client-desktop/src/service/viewer` (see
  [client-desktop/docs/features/viewer-sessions.md](../../client-desktop/docs/features/viewer-sessions.md)).
- Android does not advertise the capability and always uses one session.

## Negotiation

- The client sends `multi-session` in `register.caps` (the headless edition never does).
- The relay lists `multi-session` in `registered-ack.caps`.
- Only when both are present does the connection use the rules below, for its whole lifetime.

## Model

| Peer | Sessions | Relay state |
| --- | --- | --- |
| Without the capability, any role | 1 | `sessionWith`, `sessionActive` |
| Host (any client) | 1 | `sessionWith`, `sessionActive` |
| Viewer with the capability | 0 to 4 | `views[hostClean] -> {active, own frame queue}` |

The session key is the **host's clean desk ID**. A viewer holds at most one session per host, so the ID is
unique and the relay checks it against session state on every message. A peer holding any view counts as busy
and cannot be taken as a host.

## Messages

- One `connect-request` per host. Each success yields its own `session-connected {targetClean, role: "viewer"}`.
  Legacy Android apps (before the viewer-only release) keep it pending until they approve; a pending session
  counts toward the limit of four. Current Android apps and Linux desktops in a Wayland session are `viewer-only`
  and are refused.
- Failures (offline, busy, already open, limit reached, rejected, viewer-only) arrive as
  `error {message, targetClean}`.
- Every viewer-to-host text message must carry `targetClean`; a multi-session viewer that omits it reaches
  nobody.
- Host-to-viewer `input-command` and `clipboard-sync` gain `fromClean`; `session-closed` gains `targetClean`.
  Peers without the capability receive the old shapes.

## Binary tag

```text
[0x10][9 ASCII digits: host clean ID][voice or video payload, unchanged]
```

- Host to viewer: the relay prepends the tag without copying the payload; large messages may arrive
  fragmented, which WebSocket libraries reassemble.
- Viewer to host (voice): the viewer must send the tag; the relay checks the session, strips the tag and
  forwards the plain payload. Untagged binary from a multi-session viewer is dropped.
- `0x10` never starts an untagged message (those start with `0x02` or `0xFF`), and the relay parses tags only on
  multi-session connections.
- Clients drop tagged messages for a host without an open tab (a queued frame can still arrive just after
  `session-disconnect`).

## Disconnects and isolation

- `session-disconnect {targetClean}` ends only that pair; other tabs keep streaming.
- A host leaving or going offline closes only its tab (`session-closed {targetClean}`).
- When the viewer's connection drops or is replaced, every host it viewed is released and told.
- Each view has its own ordered frame queue, so a key frame or congestion from one host never flushes another
  host's delta chain. The viewer's single writer serves those queues in turn, after its input and control
  messages.
- File grants stay bound to the two exact connections; the limit of 8 grants is shared across a viewer's
  sessions.
- P2P signal rate limiting is per partner, and each tab owns its own P2P link.

## Client rules

- Only the focused session receives keyboard input. In split view the first click on an unfocused tile only
  focuses it.
- Clipboard follows focus: on focus change the viewer sends its clipboard to the newly focused host (unless that
  host already has it); copies on unfocused hosts are ignored.
- Only the focused session's audio plays; the microphone goes to the focused host.
- Background tabs are not decoded. Showing one again requests a key frame. Split tiles request a stream sized to
  the tile through automatic resolution.
