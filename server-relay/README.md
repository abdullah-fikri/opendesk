# OpenDesk Relay

The relay every OpenDesk client connects to: it registers devices, pairs viewer and host sessions, forwards
input, clipboard, files, voice and video between them, and keeps a device registry in PostgreSQL. Written in
Go, one binary, no CGO.

## Run

With Docker (from the repository root):

```bash
cp .env.example .env        # set DB_PASSWORD at least
docker compose up -d        # pulls ghcr.io/abdullah-fikri/opendesk-relay:latest
```

From source (Go 1.26, PostgreSQL reachable at `localhost:5432`):

```bash
cd server-relay
go run ./cmd/relay -config config.json
```

The relay listens on `0.0.0.0:8088`. Put a TLS terminator in front of it (for example a Cloudflare Tunnel or
nginx) and give clients the `wss://` address. Endpoints:

| Path | Purpose |
| --- | --- |
| WebSocket upgrade on any path | Client connections ([protocol](../docs/protocol.md)) |
| `GET /` | Status: version, active peers, uptime |
| `GET /healthz` | Liveness for Docker and load balancers |
| `GET /metrics` | Counters, requires `Authorization: Bearer $OPENDESK_METRICS_TOKEN` |

## Develop

```bash
go vet ./...
go test -race ./...
```

| Directory | Layer | Contents |
| --- | --- | --- |
| `cmd/relay` | Entrypoint | Composition root, graceful shutdown |
| `internal/handler` | Transport | HTTP endpoints, WebSocket loop, DTO mapping |
| `internal/usecase` | Use cases | Registration, session state machine, routing, file grants |
| `internal/dto` | Wire DTOs | JSON message shapes |
| `internal/repository/postgres` | Storage | Repositories and versioned SQL migrations |
| `internal/model` | Domain | Desk IDs, capabilities, hosts, sessions |
| `internal/config`, `internal/metrics`, `internal/version` | Support | Settings, counters, build version |

## Documentation

- [Architecture](docs/architecture.md)
- [Configuration](docs/configuration.md)
- [Deployment](docs/deployment.md)
- [Device credentials](docs/provisioning.md)
- [Metrics](docs/metrics.md)
- [Benchmarks](docs/benchmarks.md)
- Features: [sessions](docs/features/sessions.md), [file grants](docs/features/file-grants.md),
  [multi-session](../docs/features/multi-session.md)
