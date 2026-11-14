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

## Small hosts (half a CPU, a few hundred MB)

The relay forwards video without copying it, so a small container is enough, but two host settings decide whether
sessions through it stutter. Apply both, restart the relay, and check them with the commands below.

**1. CPU limit with a short period.** `cpus: "0.5"` gives the container 50 ms of CPU per 100 ms period. A key
frame burst can use that up early, and the kernel then freezes the relay for the rest of the period: up to 50 ms
of stalled video, periodically. The same half CPU as 5 ms per 10 ms caps a freeze at about 5 ms. In the relay
service, replace `cpus` (Docker refuses both together: "Conflicting options: Nano CPUs and CPU Period"):

```yaml
services:
  relay:
    cpu_period: 10000   # µs
    cpu_quota: 5000     # µs per period = 0.5 CPU
    deploy:
      resources:
        limits:
          memory: 500M  # unchanged; remove only `cpus`
```

If the platform does not allow `cpu_period`, set `GOMAXPROCS: "1"` in the relay's environment instead (keeping
`cpus`). Go otherwise runs at least two threads even under half a CPU; with one, a short burst can no longer use
up the 50 ms quota in a fraction of the period, so only sustained load above half a CPU is throttled.

**2. UDP socket buffers.** The UDP relay asks the kernel for 4 MB receive and send buffers, so a burst that
arrives while the relay is frozen waits instead of being dropped. Linux silently caps the request at
`net.core.rmem_max` / `net.core.wmem_max` (often 208 KB). Raise both on the Docker host (not in the container):

```bash
sudo sysctl -w net.core.rmem_max=8388608 net.core.wmem_max=8388608
printf 'net.core.rmem_max=8388608\nnet.core.wmem_max=8388608\n' | sudo tee /etc/sysctl.d/90-opendesk-relay.conf
docker compose up -d --force-recreate relay   # the buffers are taken when the socket opens
```

The memory limit needs nothing: without `GOMEMLIMIT` the relay gives the Go garbage collector 80 % of the
container's memory limit (log line `GC memory limit set`).

**Check:**

| What | Command | Good |
| --- | --- | --- |
| Start-up log | `docker compose logs relay \| grep -E 'UDP socket buffers\|GC memory limit'` | `UDP socket buffers ... rcvbuf=4194304 sndbuf=4194304` at INFO; a WARN `capped by the kernel` means step 2 is missing |
| CPU limit | `docker exec opendesk-relay cat /sys/fs/cgroup/cpu.max` | `5000 10000` |
| Throttling | `docker exec opendesk-relay cat /sys/fs/cgroup/cpu.stat` before and after a session | `nr_throttled` and `throttled_usec` barely grow; fast growth means the CPU limit is too small |
| UDP drops | `docker exec opendesk-relay cat /proc/net/snmp \| grep Udp:` before and after a session | `RcvbufErrors` (and `SndbufErrors`) stay flat. The container has its own network namespace, so `netstat -su` on the host does not show these; `sudo nsenter -t $(docker inspect -f '{{.State.Pid}}' opendesk-relay) -n netstat -su` does (`receive buffer errors`) |
| Load | `docker stats opendesk-relay` during a session | CPU well below the limit (50 % for half a CPU); a CPU pinned at the limit means the host is too small |
| Video drops | `GET /metrics` (`OPENDESK_METRICS_TOKEN`) | `droppedFrames` grows only while a viewer's link is congested |
