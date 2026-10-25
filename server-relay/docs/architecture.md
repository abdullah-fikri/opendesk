# Relay architecture

```text
cmd/relay/main.go ──▶ config.Load ──▶ postgres.Open (+ migrations) ──▶ usecase.New ──▶ handler.New ──▶ http.Server
```

## Layers

| Package | Depends on | Responsibility |
| --- | --- | --- |
| `cmd/relay` | everything | Wiring, signal handling, graceful shutdown. No logic. |
| `internal/handler` | usecase, dto, model, metrics | Upgrades WebSockets, reads messages, decodes DTOs, calls the usecase, encodes replies. Owns every user-facing message text. |
| `internal/usecase` | model (and the interfaces it declares itself) | Business rules: who may register, who may talk to whom, session lifecycle, file grants, frame queues. |
| `internal/dto` | nothing | JSON shapes only. |
| `internal/repository/postgres` | model | Implements `usecase.HostRepository` and `usecase.SessionRepository`; runs migrations. |
| `internal/model` | nothing | Desk ID rules, capability names, entities. |

`internal/usecase/ports.go` declares the interfaces the use cases need (`Conn`, `HostRepository`,
`SessionRepository`, `FrameMetrics`). Outer layers implement them, so the use cases can be tested with
in-memory fakes and never import transport or storage code.

## Concurrency

- One goroutine per connection reads and dispatches messages (`handler/websocket_handler.go`).
- `usecase.Relay` holds every peer and session in memory behind one `sync.RWMutex`. Methods ending in `Locked`
  require it; nothing else touches session fields.
- Every peer has a frame-writer goroutine with a bounded queue; every multi-session view has its own. Other
  writes go through `Peer.write`, serialized by a per-peer mutex. A failed write closes the connection, which
  ends its read loop and runs the normal cleanup.
- Storage writes run synchronously with a 5 s timeout. They are an audit trail: a failure is logged and the
  live session continues.
- Background: `handler.RunPinger` (WebSocket pings) and `Relay.RunJanitor` (expires file grants), both stopped
  by the shutdown context.

## Storage

Two tables, created by versioned migrations in `internal/repository/postgres/migrations`:

| Table | Contents |
| --- | --- |
| `hosts_registry` | One row per device ever registered: name, platform, admin flag, online flag, last seen. |
| `remote_sessions` | One row per session: viewer, host, status (`pending`, `active`, `closed`, `rejected`), times. |
| `schema_migrations` | Applied migration versions. |

Migrations are append-only: never edit a shipped file; add `000N_description.sql`. At startup the relay marks
every host offline and closes every open session left over from a previous run.

## Adding a message type

1. Add the request and response shapes to `internal/dto` and the protocol table in `docs/protocol.md`.
2. Add the rule to `internal/usecase` with a unit test.
3. Add a `case` in `connection.onText` and a handler method in `websocket_handler.go`.
4. Classify it in `ingressKind` for metrics.
