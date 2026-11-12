# Session resume

A remote session used to be bound to one WebSocket: when the socket of either side dropped, the relay closed the
session and the client tore down its direct path, even when that UDP path was still healthy. With
`session-resume` a dropped relay connection suspends the session for a fixed 30 s grace instead. The wire contract
is in [protocol.md](../protocol.md#session-resume); this page records the design decisions.

Implemented in:

- Relay: `server-relay/internal/usecase/resume.go` (suspend, resume, expiry), `relay.go` (`Register`,
  `Unregister`), `handler/websocket_handler.go` and `handler/resume.go` (messages, the expiry loop). Tests:
  `usecase/resume_test.go`, `handler/resume_test.go`, `e2e/relay_e2e_test.go`.
- Desktop: `client-desktop/src/service/relay/resume.rs` (waiting sessions, reconnect while P2P keeps running),
  `connection.rs`, `dispatcher.rs` (see [relay-connection.md](../../client-desktop/docs/features/relay-connection.md)).
- Android does not list the capability: its sessions end with its connection, as before.

## When it applies

Only when the relay, the host and the viewer all list `session-resume`. A relay that lists it still ends at once
every session with a partner that does not, so older clients see exactly the old behavior. The relay can be
deployed first. `OPENDESK_SESSION_RESUME=0` on a desktop client stops it from listing the capability.

## Decisions

### 1. Identity and encryption

- **Proof of identity.** Resuming takes over live sessions, including a host that accepts input unattended, so a
  desk ID alone must never be enough. The relay hands each capable registration a fresh 128-bit `resumeToken`
  over the TLS connection and accepts it once, from the next registration of the same ID. Device authentication
  (`OPENDESK_DEVICE_CREDENTIALS` or the allowlist token) runs first and unchanged, so a different credential for
  the same ID is refused before the token is looked at. A correct credential without the token (another
  installation with a copied token, a restarted process, an impostor on an open relay) cannot resume: it
  replaces the registration as before, which ends the sessions. Clients keep the token in memory only.
- **End-to-end keys.** `Noise_XX` transport keys live in the two clients and are used statelessly with explicit
  nonces; every relay stream accepts only increasing nonces, so messages lost during the gap leave a gap and
  nothing else. Both clients keep their channel across the gap, so there is no new handshake and nothing is
  re-sent for it. A relay that attached the session to the wrong connection could still neither read nor inject
  sealed traffic. A session whose handshake had not finished when the socket dropped (handshake messages may
  have been lost) is ended by the client instead of resumed, so it can never continue unsealed; if the relay
  resumed it anyway, the client answers with `session-disconnect`.

### 2. Multi-session viewers and the monitor wall

- Each session is suspended and resumed on its own. A viewer with four tabs that loses its connection gets them
  all back in `registered-ack.resumed`; one of its hosts that drops makes only that tab wait.
- Sessions with a partner without the capability end at once, the others wait.
- The monitor wall is not a session and is unchanged: a host's tiles show `offline` while it is gone and `live`
  after it registers again; an administrator's own watches end with its connection and its client sends
  `monitor-set` again after registering.

### 3. Direct and UDP-relayed paths

The relay does not release the session's UDP relay pair while it is suspended, and the clients keep their P2P
links and pump them while the WebSocket reconnects in the background. Video, voice and viewer input keep flowing
over a live direct or UDP-relayed path; the host keeps capturing while any path is alive and stops sending
(its frame gate stays closed) while none is. Host events (cursor, monitors) only use the relay, so they are sent
again after the resume. After a resume both clients gather candidates again (the network may have changed, for
example Wi-Fi to a hotspot) and signal them; a signal that arrives after the path is set up replaces the
candidate list and restarts probing.

### 4. Administrator actions

The relay is the control plane, so during the grace an administrator cannot reach the lost connection. Taking
a device off the allowlist (or switching the allowlist on while it is unlisted) ends its suspension at once and
its partners receive `session-closed`; it cannot register again. Resetting a device's token does not end a
suspension, just as it does not disconnect a connected device; the device enrolls again when it registers. There
is no administrator action that closes somebody else's session. A partner's own `session-disconnect` ends the
session at once. In every case the grace is never extended: at worst an action takes effect at the end of
30 s.

### 5. Relay restart

The relay keeps sessions in memory. After a restart the client's token is unknown, `registered-ack` has no
`resumed`, and the client ends its waiting sessions at once with "The connection was lost for too long."

### 6. A new registration while the old socket is still registered

The relay notices a dead socket only after 4 missed pings (60 s by default), so a client usually registers again
long before. When the token matches the registration that is still current, the new connection takes over its
sessions directly: the partners receive `session-resumed` (never the orphaning `session-closed` "The remote device
reconnected."), the UDP relay pair is kept, and the old socket is closed silently. Its file grants are dropped.

### 7. File transfers

Transfers in flight still fail when a socket drops (the client aborts them, the relay drops the grants); only
the screen session survives. Resuming transfers is a separate piece of work.

## Input safety

Nothing is queued for a suspended connection: input that arrives seconds later lands on a screen that has
changed. A viewer whose tab waits for its host drops local input that has no live path (it is not queued either)
and shows "Reconnecting… (N s)" over the last frame; the Close button keeps working. Over a live P2P path input
keeps flowing, since that path delivers it in order and at once.

## Expiry and timing

- Relay: the grace runs from the moment the relay noticed the drop; a loop checks every second and ends what has
  expired. A resume after the deadline is refused even before that loop runs.
- Client: its own 30 s deadline runs from the moment it noticed the drop (or received `session-suspended`). The
  first reconnect attempt starts after 300 ms (1.5 s without waiting sessions), later ones back off, and slow
  attempts overlap so one stuck TCP connect cannot use up the grace.
- Each side enforces its own deadline; the relay's starts later (when it notices), so a client that gave up has
  already ended its sessions and answers a late resume with `session-disconnect`.

## Tests

Relay unit tests cover resume within the grace, expiry, a registration that arrives before the old socket is
noticed, peers without the capability, the same ID with other credentials or without the token, partner
disconnects and revocation during the grace. The relay end-to-end test drops a host socket for 5 s (the session
returns and frames flow), for 35 s (the session is closed) and checks that an old viewer keeps the old behavior.
Before a release the manual matrix runs on real links: packet loss 5/10/20 % and 300 ms lag with jitter (session
stays), total outages of 5 and 20 s (session returns by itself) and of 40 s (closed with a clear message), the
host's Wi-Fi off for 10 s, a relay restart (sessions close), and old hosts, old viewers and Android (old behavior),
each over the relay WebSocket and over the UDP relay.
