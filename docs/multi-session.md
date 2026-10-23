# Multi-session viewing (tabs and split view)

Status: relay side implemented (`server-relay`, gated by the `multi-session` capability). Desktop client: not started; this document is the plan.

## Goal

A desktop viewer controls up to **4 hosts at once**, shown either as Chrome-like tabs or as a split view (2 or 4 tiles). Hard constraints:

- Existing desktop clients and the Android app (one session per peer) keep working unchanged, against both the old and the new relay.
- Nothing gets heavier for people who use one session: no extra per-message work on the legacy path and no extra threads or decoders until a second session is opened.

Out of scope: one host controlled by several viewers at once. The host side stays single-session. A host that is controlled by viewer A refuses viewer B exactly as it does today. Allowing several controllers would need input arbitration, cursor ownership and consent UX on the host, and nothing in the request needs it.

## Model

| Role | Sessions | State on the relay |
|---|---|---|
| Legacy peer (no cap), any role | 1 | `Peer.SessionWith` / `SessionActive` (unchanged) |
| Host (any client) | 1 | `SessionWith` / `SessionActive` (unchanged) |
| Viewer with `multi-session` cap | 0..4 viewer sessions | `Peer.views[hostClean] -> viewSession{active, frame queue}` |

The **session key is the host's 9-digit clean desk ID**. The relay already sends it as `targetClean` in `session-connected`, so no new identifier is needed. On the viewer, a tab is identified by its host's clean ID. There is no separate `sessionId`: a viewer can hold only one session per host, so the host ID is unique, and the relay can check it against session state on every message.

A peer that advertises `multi-session` cannot host and view at the same time. While it holds any view it is "busy", so `connect-request`s that target it fail. While it is hosting, its own `connect-request`s fail. This keeps every message's direction unambiguous.

## Wire protocol (what a client must implement)

### Negotiation

- The relay adds `"multi-session"` to `caps` in `registered-ack`. A client must enable multi-session only when it sees this cap. Against an older relay it stays single-session.
- The client adds `"multi-session"` to the `caps` array of `register`. It should add it only if it really demultiplexes as described below. Once the cap is sent, the relay switches that connection to the multi-session rules for its whole lifetime. Hosts never need the cap: a host gets the same bytes as today.

### Connecting

- Send `connect-request {targetId}` once per host, as today. Each successful connect produces `session-connected {targetId, targetClean, role:"viewer", peerCaps}`, scoped to that host.
- Android hosts: the session stays pending until the host approves, as today. A pending session counts toward the cap of 4.
- The connect fails with `error {message, targetClean}` when the target is offline, busy, already open in this viewer, or the viewer already holds 4 sessions. Note: `targetClean` appears only for multi-session viewers.
- If an Android host rejects, the viewer gets `error {message, targetClean: <host>}`.

### Text messages from viewer to host

Every viewer-to-host message **must** carry `targetClean` = the host's clean ID: `relay-input`, `relay-clipboard`, `relay-file`, `p2p-signal`, `relay-frame`, `session-disconnect`. A multi-session viewer that omits `targetClean` reaches nobody. There is no "default session". The relay forwards the message to the host unchanged, as today.

### Text messages from host to viewer

| Message | Source identification |
|---|---|
| `session-connected` | `targetClean` (existing) |
| `file-transfer`, `p2p-signal` | `fromClean` (existing) |
| `clipboard-sync`, `input-command`, `frame` (legacy JSON/base64 video) | `fromClean`, **added only for multi-session viewers** |
| `session-closed` | `targetClean` = the host whose session ended, **added only for multi-session viewers** |

### Binary messages: the session tag

Binary messages carry no sender today. For a multi-session viewer, the relay frames every binary message it delivers, in both directions, like this:

```text
offset  size  value
0       1     0x10            session tag marker
1       9     ASCII digits    host clean desk ID, e.g. "222222222"
10      ...   payload         the unchanged legacy binary: 0x02 audio, or FF D8 video (JPEG / framed delta / H.264)
```

- **Host to viewer:** the relay prepends the 10-byte tag. It writes the tag and the payload into one WebSocket message without copying the payload. Large messages may arrive as several WebSocket fragments. Standard WebSocket libraries (tungstenite, OkHttp) reassemble them, so the application sees one message.
- **Viewer to host** (voice audio, for example): the viewer **must** send `0x10 + hostClean + payload`. The relay checks that the viewer has an active session with that host, strips the tag and forwards only the payload, so the host sees exactly the legacy bytes. An untagged binary message from a multi-session viewer is dropped.
- `0x10` is never the first byte of a legacy binary message (those start with `0x02` or `0xFF`). The relay reads the tag only on connections that advertised `multi-session`. Legacy peers are never tagged and never parsed.
- Clients must ignore tagged messages for a host they hold no open tab for. A frame that was already queued can still arrive just after `session-disconnect`.

### Disconnect semantics

- `session-disconnect {targetClean: host}` ends only that pair. The host gets `session-closed`, unchanged for legacy hosts. Other tabs are untouched.
- When a host ends the session or goes offline, the viewer gets `session-closed {message, targetClean: host}`. Only that tab closes.
- When the viewer's connection drops or is replaced by a re-register, every host it viewed gets `session-closed` and is released.
- File grants (`relay-file` start/chunk/end/ack) are bound to the two exact connections. They survive the end of the session, as today, and can never cross to another pair. `maxGrantsPerPeer` (8) is shared across all of a viewer's sessions.

### P2P

`p2p-signal` is already pair-scoped through `targetClean` and `fromClean`. The relay's 100 ms signal rate limit is now kept **per partner**, so four handshakes in parallel do not starve each other. For a legacy peer this changes nothing, because it has one partner. Each tab owns its own `P2p` link (its own UDP socket and Noise session). Video arriving on a P2P link belongs to that link's host, so the P2P datagram format needs no tag.

## Relay implementation notes (done)

- `Peer.Multi` is set once at register. `Peer.views` is nil for legacy peers, so the legacy hot path adds one nil-map lookup. `BenchmarkRelayRouting` stays at about 22 ns/op with 0 allocations.
- Each view has its **own ordered frame queue and writer goroutine**, so a key frame or congestion from host A never flushes host B's delta chain. Memory cost per view: one goroutine and an 8-slot channel. The goroutine stops when the session ends or the viewer disconnects.
- All session-state helpers (`activePartnerLocked`, `setSession`, `approveSession`, `closePendingSession`, `closeSession`, `registerPeer`, `removePeer`) go through `sessionLocked`, `endLocked` and `detachPartnersLocked`. These treat `SessionWith` and `views` uniformly, under the same single `peersMu`. No new locks were added.
- DB bookkeeping: for a multi-session viewer, `StartSession` no longer closes the viewer's other open rows, and `session-disconnect` closes only that pair's row.
- Tests: `server-relay/internal/gateway/multisession_test.go`.

## Client architecture changes (desktop, Rust)

Today, process-wide singletons assume one partner. The main ones:

- `active_partner: Arc<Mutex<String>>`, read in network.rs and in about 15 UI callbacks in main.rs: input, monitor switch, privacy, block input, resolution, disconnect, file drop.
- One `VideoInbox` (`latest_bin_frame`) and one decoder thread holding one `ViewerDecoder` (JPEG, delta and H.264).
- One `p2p_link: Option<Arc<P2p>>` in the network loop, which is torn down whenever `active_partner` changes.
- One `remote_resolution`, one clipboard sequence, one virtual-clipboard offer slot, and one `remote_canvas` in Slint.
- Host-side capture (`spawn_host_capture_loop`, `is_host_capturing`) stays single. The host role does not change.

### Target structure

```text
NetworkWorker (one WebSocket, as today)
 ├─ sessions: HashMap<HostClean, ViewerSession>     (max 4, viewer role)
 ├─ focused: Option<HostClean>                     (keyboard/clipboard/file target)
 └─ host_session: Option<...>                      (existing single host role, unchanged)

ViewerSession
 ├─ host_clean, peer_caps, state (Pending/Active)
 ├─ inbox: Arc<VideoInbox>  + decoder thread with its own ViewerDecoder
 ├─ p2p: Option<Arc<P2p>>
 ├─ remote_resolution, monitor index, key-frame request state
 └─ ui slot index (tab/tile)
```

- **Demultiplexing:** in the WebSocket read loop, if `cap multi-session` was negotiated and `bin[0] == 0x10 && bin.len() > 10`, split off the host ID and push the payload into `sessions[host].inbox`. Audio (`payload[0] == 0x02`) goes to the voice engine only for the focused session, or is muted (see risks). Untagged binary keeps the current path, for the host role.
- **Outgoing:** `OutgoingMsg` gains a `target: HostClean` field (or carries `targetClean` in the JSON). The coalescing of mouse moves becomes per target. Binary audio from the viewer gets the tag prepended.
- **`active_partner` replacement:** split into
  1. `focused_session` (UI focus, drives keyboard, clipboard, file drop, toolbar actions), and
  2. per-session state used by routing.

  Every UI callback that sends something resolves its target from the tile or tab where the event happened: mouse events from that tile's canvas, keyboard from the focused tab. The single-session code path is the same as "4-slot map with one entry, always focused".
- **Clipboard and files:** clipboard pushes and virtual-clipboard file offers go to the **focused** session only. On focus change, the client may push the current clipboard to the newly focused host, which matches today's behaviour on session start. Incoming `clipboard-sync` from an unfocused host updates the local clipboard only if the user enabled it, and is otherwise ignored. File drag and drop targets the tile under the cursor.
- **Decoders:** a decoder thread and its `ViewerDecoder` are created when a session becomes active and dropped when it closes. Nothing is pre-allocated, so one session costs exactly what it does today.
- **UI (Slint):** `remote_canvas` becomes a component instance per session. The tab bar shows `[device name | status | x]`. Split view is a 1x2 or 2x2 grid of the same components. Only the focused tile receives keyboard input. A visible focus ring shows which tile that is.

### Phased client plan

1. **Refactor without behaviour change.** Introduce `ViewerSession` and a `sessions` map holding at most one entry. Move the decoder, inbox, P2P link and resolution state into it. Replace reads of `active_partner` with `focused_session`. Do not send the cap yet. Ship it and verify legacy parity: same bytes on the wire, same CPU and memory.
2. **Protocol support.** Send `multi-session` only when the relay advertises it. Demultiplex tagged binary, tag outgoing binary, always send `targetClean`, and handle `targetClean` on `session-closed` and `error`. Still allow one open session in the UI, which verifies the tagged path end to end.
3. **Tabs.** Allow up to 4 sessions with a tab bar. Only the visible tab is decoded and rendered at full rate (see below). Wire disconnect per tab, plus reconnect-on-error per tab.
4. **Split view.** 2x2 or 1x2 grid with focus handling. Ask hosts for lower resolution or frame rate when their tile is small, using the existing `resolution_event` and auto-box mode, which already sizes the stream to the viewport.
5. **Polish.** Per-tab audio choice, clipboard policy setting, P2P per tab, and per-tab stats.

The Android viewer can adopt the same protocol later. Until then it simply never sends the cap.

## Resources (viewer)

- **Decoders:** up to 4 `ViewerDecoder`s. H.264 decode of 4 x 1080p30 streams is feasible on most desktops (hardware decode where available). Software decode of 4 x 1080p60 is not, on weak machines. Mitigations, in order:
  - Background tabs: tell the host to pause or slow down. Today: send a resolution or quality event and drop frames until the tab is shown again, then request a key frame. Later: add a dedicated pause event.
  - Split view: each tile is at most 1/2 or 1/4 of the window. Auto-box resolution makes each host encode at tile size, so total decoded pixels stay about one screen's worth.
- **Memory:** one RGBA framebuffer per session (1080p is about 8 MB), plus decoder state. Under 100 MB for 4 sessions.
- **Network:** each host streams independently. The relay queues frames per session (8 frames max each), so one slow host cannot stall the others, but the viewer's downlink is shared. Congestion control is still per host (CongestionMeter). The viewer should prefer the focused tile when the link is saturated.
- **Relay:** each view adds one goroutine and a small channel. Per-message overhead for legacy traffic is unchanged. Tagged writes cost a 10-byte header and no payload copy.

## Risks and open questions

1. **Audio:** mixing 4 hosts' audio is noisy. Proposal: play only the focused session's audio, with a per-tab speaker toggle. Voice from the viewer's microphone goes to the focused host only.
2. **Keyboard capture and shortcuts:** global hooks (privacy mode, block input, special keys) currently assume one partner. Each must pick the focused session. Focus has to be unambiguous in split view, or keys reach the wrong machine. This is the main safety risk.
3. **Clipboard leakage:** pushing the viewer's clipboard to every host would leak data between unrelated machines. Hence focused-only. Confirm with the user whether automatic sync on focus change is wanted.
4. **Grant cap:** `maxGrantsPerPeer = 8` is shared across sessions. Raise it for multi-session viewers if parallel transfers to several hosts turn out to be common.
5. **Mixed roles:** a multi-session viewer cannot be hosted while it views, and vice versa. Should a desktop be both controlled and controlling at once? This was deliberately not supported. Revisit only on demand.
6. **Fragmented WebSocket messages:** tagged frames are written with a streaming writer and can be fragmented. tungstenite reassembles them. Any custom client must too.
7. **DB:** `remote_sessions` rows are per pair. `CloseSessionsForPeer` on viewer disconnect closes all of them (correct). Admin host list views are unchanged.
8. **Refactor collisions:** `active_partner` touches main.rs, network.rs and file_transfer.rs. Phase 1 should land as one focused change, coordinated with any concurrent client work.
