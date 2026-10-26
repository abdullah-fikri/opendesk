# Deploying the relay

## Requirements

- PostgreSQL 13 or newer. The relay creates and migrates its tables itself.
- A TLS terminator in front of the relay. Clients must use `wss://` unless the relay is on the same machine.

## Docker Compose (recommended)

The repository's `docker-compose.yml` runs the published image and binds port 8088 to loopback only, so the
only way in is through the TLS terminator on the same host:

```bash
cp .env.example .env     # set DB_PASSWORD
docker compose pull
docker compose up -d
docker compose logs -f relay
```

The image is pinned to a release: `ghcr.io/abdullah-fikri/opendesk-relay:1.0.1`, never `latest`, so `docker ps`
shows which version runs. To upgrade or roll back, set `RELAY_VERSION=1.0.2` in `.env` and run
`docker compose pull && docker compose up -d`. Every release tag publishes `<version>`, `<major>.<minor>` and
`latest`.

`OPENDESK_TLS_PROXY=1` is set because connections from the terminator reach the container through the Docker
bridge, not loopback. Do not publish 8088 on a public interface with that setting.

Example Cloudflare Tunnel ingress:

```yaml
ingress:
  - hostname: relay.example.com
    service: http://127.0.0.1:8088
  - service: http_status:404
```

Clients then use `wss://relay.example.com`.

## Binary

```bash
make relay                      # Windows: builds\relay\opendesk-relay.exe
cd server-relay && go build -o opendesk-relay ./cmd/relay   # any platform
DB_PASSWORD=... ./opendesk-relay -config config.json
```

## Operations

- **Health:** `GET /healthz` returns `{"status":"ok"}`; the image has a Docker `HEALTHCHECK` on it.
- **Shutdown:** `SIGTERM` stops accepting connections and lets in-flight HTTP requests finish for 10 s.
  Clients reconnect automatically to the new instance.
- **Logs:** structured `key=value` lines on stdout. Payloads, clipboard text and tokens are never logged.
- **Upgrades:** pull the new image and restart. Migrations run automatically; they are forward-only, so back up
  the database before a major upgrade.
- **Scaling:** one relay instance holds all live sessions in memory. Run a single instance per relay address.
