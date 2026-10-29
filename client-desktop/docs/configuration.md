# Desktop configuration

## Files

All under `%APPDATA%\OpenDesk` on Windows and `~/.config/opendesk` (`$XDG_CONFIG_HOME/opendesk`) on Linux:

| File | Contents |
| --- | --- |
| `config.json` | `gateway_url`, `desk_id`, `device_token`, `performance` (`low`, `medium`, `high`; see [viewer sessions](features/viewer-sessions.md#performance)). Created on first run with a new desk ID. |
| `history.json` | Recently contacted devices: `desk_id`, `alias`, `last_connected` (date). |
| `opendesk.trace` | Log. |
| `user_exit.flag`, `watchdog.lock`, `opendesk-watchdog.vbs` | Watchdog state (Windows, non-installed builds only). |
| `instance.lock` | Linux: held by the running instance (single instance). |

Received files go to `%USERPROFILE%\Downloads\OpenDesk` (Windows) or `~/Downloads/OpenDesk` (Linux).

## Command-line arguments

| Argument | Meaning |
| --- | --- |
| `--background`, `--minimized` | Start hidden in the tray (used by autostart and the watchdog). |
| `--id=<desk id>` | Use another desk ID for this run (testing). |
| `server <url>`, `status`, `id`, `stop`, `uninstall`, `help` | CLI commands; see [autostart and CLI](features/autostart-and-cli.md). |

## Environment variables

| Variable | Default | Meaning |
| --- | --- | --- |
| `OPENDESK_DEVICE_TOKEN` | `device_token` from `config.json` | Relay credential ([provisioning](../../server-relay/docs/provisioning.md)). |
| `OPENDESK_P2P` | on | `0` keeps every session on the relay. |
| `OPENDESK_UDP_RELAY` | on | `0` never routes the P2P path through the relay's UDP forwarder. |
| `OPENDESK_E2E` | on | `0` turns end-to-end encryption off ([e2e](features/e2e.md)). |
| `OPENDESK_STUN` | `stun.l.google.com:19302` | STUN server for the public P2P candidate; `off` uses LAN candidates only. |
| `OPENDESK_VIDEO_H264` | auto | `0` disables H.264; `always` uses it for every changed frame. |
| `OPENDESK_MAX_FPS` | unset | Caps the frame rate on both sides, whatever the preset (`60` restores the 60 FPS pipeline). |
| `OPENDESK_WS_HIGH_FPS` | on | Host: `0` keeps sessions on the WebSocket relay at 60 FPS (more only over P2P or the UDP relay). |
| `OPENDESK_WS_SNDBUF` | 524288 | Host: socket send buffer in bytes while a hosted session's video goes over the WebSocket relay; `0` leaves the OS default ([hosting](features/hosting.md#capture-loop)). |
| `OPENDESK_CAPTURE` | DXGI | Windows: `gdi` forces GDI capture (for machines where Desktop Duplication misbehaves). |
| `OPENDESK_OPENH264` | search path | Linux: path of Cisco's `libopenh264-2.6.0-*.so` (see [core-engine](../../core-engine/README.md)). |
| `OPENDESK_NO_AUTOSTART` | unset | Linux: any value stops the app from writing `~/.config/autostart/opendesk.desktop`. |
