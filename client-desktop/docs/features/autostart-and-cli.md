# Autostart, watchdog and CLI

Code: `src/infra/autostart/` (`win.rs`, `unix.rs`), `src/cli.rs`, installer in `installer/opendesk.iss`, Linux
packaging in `client-desktop/Cargo.toml` (`[package.metadata.deb]`) and `packaging/linux/`.

## Single instance

One running instance per user: a named mutex on Windows, a locked `instance.lock` on Linux. A second launch wakes
the running one (shows its window) and exits; on Linux it does so through a per-user Unix socket
(`$XDG_RUNTIME_DIR/opendesk-<uid>.sock`), which `opendesk stop` also uses.

## Autostart

| Install type | Mechanism |
| --- | --- |
| Installed (setup) | The service `OpenDeskService` (see below) runs the app as SYSTEM in the logged-on user's session. A launch from the Start Menu hands over to it and shows its window. |
| Installed, service not created | A Task Scheduler task `OpenDesk Autostart` starts the app elevated at logon, with restart on failure. Elevation lets viewers control elevated windows (Task Manager, "Run as administrator" apps), not the UAC prompt. A non-elevated launch hands over to the task. |
| Portable (no task) | `HKCU\...\Run` entries for the app and a VBScript watchdog that restarts it within 10 s if it is killed. "Stop & Exit" in the tray pauses the watchdog until the next launch. |
| Linux package (`.deb`) | `/etc/xdg/autostart/opendesk.desktop` starts `opendesk --background` at every graphical login. No watchdog. |
| Linux portable (`.tar.gz`, AppImage, `cargo run` excluded) | The app writes `~/.config/autostart/opendesk.desktop` pointing at itself (the AppImage file when run as one). `opendesk uninstall` removes it; `OPENDESK_NO_AUTOSTART=1` prevents it. |

Closing the window keeps OpenDesk in the tray, so the machine stays reachable.

## Windows service

Code: `src/infra/system_service/win.rs`. `OpenDesk.exe --service` runs under the Service Control Manager as
LocalSystem (auto start, restart on failure). Only SYSTEM may open the Winlogon desktop that shows the UAC prompt
and the lock screen, so the service keeps one copy of the app, the agent, running as SYSTEM in the active console
session; its capture and input threads follow the input desktop there.

- The agent gets the logged-on user's environment block, so settings, history, the log and received files stay
  in that user's `%APPDATA%` and `Downloads`.
- The service polls every 2 s: it starts the agent 10 s after a user logs on (so the tray exists), restarts it when it exits (an agent
  that exits within 10 s is retried after 30 s), and stops it when the console moves to another session.
- "Stop & Exit" and `opendesk stop` write `user_exit.flag`; the agent exits on it and the service leaves it
  stopped until the next launch from the Start Menu removes the flag. "Restart" just exits and lets the service
  start it again.
- The send-files picker is an Explorer window that could start programs, so the agent never shows it as SYSTEM:
  it runs `OpenDesk.exe --pick-files <file>` as the logged-on user and reads the chosen paths back.

Not covered: the logon screen after a reboot or sign-out (no user session, so no agent), Remote Desktop sessions
(only the console session is followed), and Ctrl+Alt+Del.

The app window and tray still run as SYSTEM, so anyone at the machine can use them (for example to send any file
the agent can read). Install the service only on machines whose local users you trust with that.

## CLI

On Windows the app writes `opendesk.cmd` at startup to `%LOCALAPPDATA%\Microsoft\WindowsApps`, which is on
`PATH` (the uninstaller removes it). The Linux packages install `/usr/bin/opendesk`; the same binary is the app
and the CLI.

| Command | Effect |
| --- | --- |
| `opendesk server <url>` | Set the relay; the running app reconnects within about a second |
| `opendesk status`, `opendesk id`, `opendesk help` | Show the device name, desk ID, device key fingerprint and relay |
| `opendesk trust <id>` | Accept a host's changed device key: forget the pinned one ([e2e](e2e.md)) |
| `opendesk stop` | Stop OpenDesk and its watchdog (Windows: the service's agent stops itself; an elevated-task copy needs an elevated terminal) |
| `opendesk uninstall` | Stop everything and remove autostart entries before uninstalling (Linux: the per-user entry) |
