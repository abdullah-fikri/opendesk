#!/usr/bin/env bash
# End-to-end tests against a real relay and PostgreSQL, all throwaway: never the production relay on 8088.
#   1. relay repositories against PostgreSQL (migrations included)
#   2. the relay binary over WebSocket, with restarts (server-relay/e2e)
#   3. the desktop client (standard and admin edition) against a running relay (relay_e2e)
#
# Usage: scripts/e2e.sh
#   OPENDESK_TEST_PG_PORT  use an existing throwaway PostgreSQL (user/password "postgres") instead of starting
#                          one with Docker (CI passes its service container here)
#   E2E_RELAY_PORT         port of the relay started for the client tests (default 18088)
#   E2E_SKIP_CLIENT=1      only the relay parts (no Rust toolchain needed)
set -euo pipefail
cd "$(dirname "$0")/.."
root=$(pwd)

relay_port="${E2E_RELAY_PORT:-18088}"
if [[ "$relay_port" == 8088 ]]; then
  echo "refusing to use port 8088 (production relay)" >&2
  exit 1
fi
# The relay lets these override its config file; inherited ones could point it at a real database.
unset DB_HOST DB_PORT DB_USER DB_PASSWORD DB_NAME OPENDESK_DEVICE_CREDENTIALS OPENDESK_METRICS_TOKEN \
  OPENDESK_TLS_PROXY OPENDESK_P2P_DISABLED
work=$(mktemp -d)
pg_container=""
relay_pid=""

cleanup() {
  [[ -n "$relay_pid" ]] && kill "$relay_pid" 2>/dev/null || true
  [[ -n "$pg_container" ]] && docker rm -f "$pg_container" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

step() { printf '\n==> %s\n' "$*"; }

if [[ -z "${OPENDESK_TEST_PG_PORT:-}" ]]; then
  step "PostgreSQL (docker, port 55432)"
  pg_container="opendesk-e2e-pg-$$"
  docker run --rm -d --name "$pg_container" -p 127.0.0.1:55432:5432 -e POSTGRES_PASSWORD=postgres \
    postgres:16-alpine >/dev/null
  export OPENDESK_TEST_PG_PORT=55432
  for _ in $(seq 60); do
    docker exec "$pg_container" pg_isready -U postgres -h 127.0.0.1 >/dev/null 2>&1 && break
    sleep 1
  done
  docker exec "$pg_container" pg_isready -U postgres -h 127.0.0.1 >/dev/null
fi

step "Relay repositories against PostgreSQL"
(cd server-relay && go test -count=1 -v -run Postgres ./internal/repository/postgres)

step "Relay binary end to end"
(cd server-relay && go test -count=1 -v ./e2e)

[[ "${E2E_SKIP_CLIENT:-}" == 1 ]] && { step "client tests skipped"; exit 0; }

step "Relay for the client tests (port $relay_port)"
# A database of its own: the repository tests above leave the allowlist switched on in theirs.
create_db="CREATE DATABASE opendesk_e2e_client"
if [[ -n "$pg_container" ]]; then
  docker exec "$pg_container" psql -q -U postgres -c "$create_db"
else
  PGPASSWORD=postgres psql -q -h 127.0.0.1 -p "$OPENDESK_TEST_PG_PORT" -U postgres \
    -c "DROP DATABASE IF EXISTS opendesk_e2e_client WITH (FORCE)" -c "$create_db"
fi
bin="$work/relay"
[[ "${OS:-}" == Windows_NT ]] && bin="$bin.exe"
(cd server-relay && go build -o "$bin" ./cmd/relay)
cat > "$work/config.json" <<EOF
{"host": "127.0.0.1", "port": $relay_port, "ping_interval_seconds": 15, "read_buffer_size": 32768,
 "write_buffer_size": 32768, "db_host": "127.0.0.1", "db_port": $OPENDESK_TEST_PG_PORT, "db_user": "postgres",
 "db_password": "postgres", "db_name": "opendesk_e2e_client"}
EOF
"$bin" -config "$work/config.json" > "$work/relay.log" 2>&1 &
relay_pid=$!
for _ in $(seq 50); do
  curl -fsS "http://127.0.0.1:$relay_port/healthz" >/dev/null 2>&1 && break
  sleep 0.2
done
curl -fsS "http://127.0.0.1:$relay_port/healthz" >/dev/null || { cat "$work/relay.log"; exit 1; }

# The client stores settings in its app directory; keep the developer's own config.json untouched.
export APPDATA="$work/appdata" XDG_CONFIG_HOME="$work/config"
mkdir -p "$APPDATA" "$XDG_CONFIG_HOME"
export OPENDESK_E2E_RELAY="ws://127.0.0.1:$relay_port"

# The tests share device IDs within one process, so they run one at a time.
step "Desktop client end to end (standard edition)"
cargo test --manifest-path "$root/client-desktop/Cargo.toml" --release --locked relay_e2e -- --ignored --test-threads=1
step "Desktop client end to end (admin edition)"
cargo test --manifest-path "$root/client-desktop/Cargo.toml" --release --locked --features admin relay_e2e \
  -- --ignored --test-threads=1

step "all end-to-end tests passed"
