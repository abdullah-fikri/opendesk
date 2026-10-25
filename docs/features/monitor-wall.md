# Monitor wall

The admin edition can watch many machines at once, like a CCTV wall: a grid of 2x2, 3x3 or 4x4 live
thumbnails, paged when there are more devices, with optional automatic page rotation. It is **view only**: no
input, clipboard, files, voice, privacy screen or monitor switching, for anyone, enforced by the relay.

The wall's toolbar keeps only navigation (Back, pages, Full screen, Settings). **Settings** opens a side panel with
the layout (2x2, 3x3, 4x4), automatic page rotation and the devices on the wall. **Full screen** hides every
control, the app header included, and shows only the screens; Esc or the faint corner button leaves it, and the
arrow keys turn pages. Tiles use the names administrators gave devices on the relay.

Implemented in:

- Relay: `server-relay/internal/usecase/monitor.go` (watches, fan-out), `handler/websocket_handler.go`
  (`monitor-set`), tests in `usecase/monitor_test.go` and `handler/monitor_test.go`.
- Host side, every desktop edition: `client-desktop/src/service/host/monitor_stream.rs`.
- Wall, admin edition only (`--features admin`): `client-desktop/src/domain/monitor_wall.rs` (paging, rotation,
  selection), `src/service/monitor_wall.rs`, `src/ui/monitor_wall_model.rs`, `ui/views/monitor/`.
- Android advertises nothing and shows as "cannot be monitored".

## Negotiation

| Capability | Who | Meaning |
| --- | --- | --- |
| `monitor` | relay | The relay runs the wall (`registered-ack.caps`). |
| `monitor-host` | client | Streams thumbnails on `monitor-start`. Every desktop edition, including headless. |
| `monitor-view` | client | Understands `monitor-state` and the `0x11` tag. Admin edition only. |

Without `monitor` the wall says the relay must be updated. A host without `monitor-host` (Android, older desktop
clients) shows as `unsupported` and is never sent anything.

## Messages

```text
admin ──monitor-set {targets[]}──▶ relay ──monitor-start {maxWidth, fps}──▶ host   (first watcher)
admin ◀──monitor-state {targetClean, state}── relay                                  (per target, on changes)
admin ◀──[0x11][host ID][JPEG]── relay ◀──[0x11][JPEG]── host                         (fan-out)
                                 relay ──monitor-stop──▶ host                        (last watcher gone)
```

- `monitor-set` replaces the administrator's whole watch list: at most 16 clean IDs (the visible page); invalid
  IDs, duplicates and the sender itself are ignored, an empty list stops watching. One message per page change.
- `monitor-state.state`: `live`, `offline` (not connected; goes live by itself when it registers),
  `unsupported`, `denied` (the sender is not an administrator).
- `monitor-start`: plain JPEGs at most `maxWidth` px wide (relay asks 640), at most `fps` per second (relay asks
  2). Hosts clamp to 160..960 px and 1..5 fps.
- Thumbnails are plain JPEGs (`FF D8 FF`), so every frame stands alone: no key-frame requests, and dropping any of
  them is harmless. Framed or oversized (> 512 KiB) payloads are refused.
- A watch belongs to the administrator's connection: when it drops, its watches end; when a host drops, its
  watchers see `offline` and keep the watch.

## Authorization and read-only enforcement

- **Admin only.** The relay accepts `monitor-set` only from a connection with the admin role, decided exactly like
  `get-hosts`: the claimed role on an open relay, only the stored `admin` flag when `OPENDESK_DEVICE_CREDENTIALS`
  is set. Others get `denied`. The client additionally compiles the wall out of non-admin builds (no
  `monitor-view`, no `monitor-set`, wall callbacks unbound; the Slint markup is shared but never instantiated
  because `is_admin` is false).
- **Read-only by construction.** A watch is not a session. Session authorization (`ActivePartner`, `RouteFile`,
  the session paths of `ForwardBinary`) never sees it, so `relay-input`, `relay-clipboard`, `relay-file`,
  `p2p-signal` and voice between a watcher and a watched host are dropped in both directions. The administrator
  never addresses the host at all: only the relay sends it `monitor-start`/`monitor-stop`. A watcher's `0x11`
  binary is refused (only a streaming host may send one).
- The wall UI has no input handlers; a double click on a live tile opens a normal interactive session (which then
  follows the normal session rules) after closing the wall.

## Interaction with sessions

- Watches do not count toward the four interactive sessions and do not make a host busy: a watched host still
  accepts a session, and an administrator watching 16 hosts can still open four sessions.
- The host's thumbnail stream is independent of its session capture (GDI on its own thread, DXGI stays with the
  session) and keeps running during a session.
- The wall replaces the dashboard; it is reachable only while no viewer session is open.

## Consent

Windows hosts are unattended (every session is accepted without a prompt), and a watch follows the same rule.
When a watch starts, the host's status line says "An administrator is viewing this screen (view only)" and the
log records start and stop. Android hosts, which ask for consent, cannot be watched at all.

## Load

Measured on an Intel Core Ultra 5 125U, 1920x1200 desktop (`bench_monitor_stream`, `bench_wall_decode`,
`BenchmarkMonitorFanOut`, `BenchmarkMonitorPage`):

| Where | Per tile | Per full 4x4 page |
| --- | --- | --- |
| Host (one stream however many watchers) | 640x400 JPEG, 20-32 KB; ~5 ms CPU per captured frame, ~1 % of one core at 2 fps; nothing sent while the screen is unchanged | n/a |
| Network, host to relay and relay to admin | at most ~40-65 KB/s while the screen changes, ~0 when idle | at most ~0.6-1.0 MB/s (5-8 Mbit/s), typically far less |
| Relay | ~0.2 µs per tile per frame (3.4 µs to fan one frame out to 16 admins); per tile at most 2 frames held (one being written, one waiting), plus the newest frame per watched host | 2.8 µs to fan out a frame from each of 16 hosts |
| Admin | JPEG decode ~1.6 ms | 16 x 2 fps: ~5 % of one core on one worker thread; at most 10 UI updates per second |

- Only the visible page is watched: changing page sends one `monitor-set`, the relay stops the hosts nobody else
  watches. Hidden pages cost nothing anywhere.
- A slow administrator only drops its own frames: each tile has its own bounded queue (the relay's frame queue;
  a plain JPEG flushes anything older). The host keeps at most one thumbnail waiting in its socket and sends
  thumbnails only between frames of a running session's video.
- The thumbnails are already tile-sized by the host, so the admin does not downscale; Slint scales on the GPU.

## Settings

Stored in `config.json` under `monitor` (`grid`, `rotate`, `rotate_secs`, `all`, `selected`). Every edition keeps
the section, so switching editions never loses it. Defaults: 3x3, rotation off (10 s when switched on), all
online devices.

## Limitations

- The primary monitor only.
- The device list is the relay's registry (`host-list`); a relay without its database lists nothing, so there is
  nothing to put on the wall.
- No per-host opt-out other than not running a desktop client with `monitor-host` (all current desktop editions
  advertise it).
