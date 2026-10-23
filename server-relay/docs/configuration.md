# Relay configuration

Settings come from `config.json` (path set with `-config`, default `./config.json`; a missing file is fine) and
environment variables, which win. Invalid values stop the relay at startup instead of being ignored.

## config.json

| Key | Default | Meaning |
| --- | --- | --- |
| `host` | `0.0.0.0` | Listen address |
| `port` | `8088` | Listen port |
| `ping_interval_seconds` | `15` | WebSocket ping period; a peer is dropped after 4 missed pings |
| `read_buffer_size` | `32768` | WebSocket read buffer per connection (bytes) |
| `write_buffer_size` | `32768` | WebSocket write buffer, pooled across connections (bytes) |
| `db_host` | `localhost` | PostgreSQL host; `localhost` also tries `host.docker.internal` |
| `db_port` | `5432` | PostgreSQL port |
| `db_user` | `postgres` | PostgreSQL user |
| `db_password` | `postgres` | PostgreSQL password (set `DB_PASSWORD` instead in production) |
| `db_name` | `opendesk` | Database name |

## Environment

| Variable | Meaning |
| --- | --- |
| `DB_HOST`, `DB_PORT`, `DB_USER`, `DB_PASSWORD`, `DB_NAME` | Override the database settings. |
| `OPENDESK_DEVICE_CREDENTIALS` | JSON map of per-device tokens; empty admits every device. See [provisioning](provisioning.md). Malformed JSON stops the relay. |
| `OPENDESK_METRICS_TOKEN` | Enables `GET /metrics` for `Authorization: Bearer <token>`; at least 32 characters. |
| `OPENDESK_TLS_PROXY=1` | Accept plaintext WebSocket from non-loopback addresses because TLS ends at a proxy (Docker behind a tunnel). |
| `OPENDESK_P2P_DISABLED=1` | Do not advertise or forward P2P signaling; every session stays on the relay. |

## Clients

Clients have their own switches, documented with them: `OPENDESK_DEVICE_TOKEN`, `OPENDESK_P2P`,
`OPENDESK_STUN`, `OPENDESK_VIDEO_H264`, `OPENDESK_CAPTURE` (see
[client-desktop/docs/configuration.md](../../client-desktop/docs/configuration.md)).
