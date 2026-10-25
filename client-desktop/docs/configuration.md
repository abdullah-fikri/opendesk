# Desktop configuration

## Files

All under `%APPDATA%\OpenDesk` on Windows and `~/.config/opendesk` (`$XDG_CONFIG_HOME/opendesk`) on Linux:

| File | Contents |
| --- | --- |
| `config.json` | `gateway_url`, `desk_id`, `device_token`. Created on first run with a new desk ID. |
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
| `OPENDESK_STUN` | `stun.l.google.com:19302` | STUN server for the public P2P candidate; `off` uses LAN candidates only. |
| `OPENDESK_VIDEO_H264` | auto | `0` disables H.264; `always` uses it for every changed frame. |
| `OPENDESK_CAPTURE` | DXGI | Windows: `gdi` forces GDI capture (for machines where Desktop Duplication misbehaves). |
| `OPENDESK_OPENH264` | search path | Linux: path of Cisco's `libopenh264-2.6.0-*.so` (see [core-engine](../../core-engine/README.md)). |
| `OPENDESK_NO_AUTOSTART` | unset | Linux: any value stops the app from writing `~/.config/autostart/opendesk.desktop`. |
