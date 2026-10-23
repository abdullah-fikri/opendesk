# Sessions

Code: `internal/usecase/session.go`, `relay.go`, `routing.go`; transport in `internal/handler/websocket_handler.go`.

## Lifecycle

```text
connect-request ──▶ Connect ──┬─ Windows host ──▶ active ──▶ session-connected to both
                              └─ Android host ──▶ pending ──▶ incoming-request to host
                                                    │
                       approval-response ───────────┤ accept ──▶ Approve ──▶ active ──▶ session-connected to both
                                                    └ reject ──▶ Reject  ──▶ closed ──▶ error to viewer
session-disconnect ──▶ Disconnect ──▶ session-closed to the partner
connection lost    ──▶ Unregister ──▶ session-closed to every partner
re-register        ──▶ Register replaces the old connection ──▶ session-closed to its partners
```

## Rules

- A device cannot connect to itself.
- The target must be online and idle: a peer that hosts or views anything cannot be taken as a host.
- A viewer without `multi-session` holds one session; with it, up to four (see
  [multi-session](../../../docs/features/multi-session.md)).
- A message is forwarded only if both connections hold an active session with each other
  (`activePartnerLocked`). Pending sessions forward nothing.
- Approvals and rejections apply only to the matching pending pair.
- A replaced or removed connection loses every right immediately; messages still in its read loop are ignored.

## Storage

Each session gets a `remote_sessions` row. Starting a session closes stale open rows of the same peers (for a
multi-session viewer only rows involving the host). Disconnects close the pair's row; a lost connection closes
all of that peer's rows. Storage is an audit trail: a failed write is logged and never blocks the live session.
