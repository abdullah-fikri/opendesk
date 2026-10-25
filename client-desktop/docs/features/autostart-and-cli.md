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
| Installed (setup) | A Task Scheduler task `OpenDesk Autostart` starts the app elevated at logon, with restart on failure. Elevation lets viewers control elevated windows (Task Manager, "Run as administrator" apps). A non-elevated launch hands over to the task. |
| Portable (no task) | `HKCU\...\Run` entries for the app and a VBScript watchdog that restarts it within 10 s if it is killed. "Stop & Exit" in the tray pauses the watchdog until the next launch. |
| Linux package (`.deb`) | `/etc/xdg/autostart/opendesk.desktop` starts `opendesk --background` at every graphical login. No watchdog. |
| Linux portable (`.tar.gz`, AppImage, `cargo run` excluded) | The app writes `~/.config/autostart/opendesk.desktop` pointing at itself (the AppImage file when run as one). `opendesk uninstall` removes it; `OPENDESK_NO_AUTOSTART=1` prevents it. |

Closing the window keeps OpenDesk in the tray, so the machine stays reachable.

## CLI

On Windows the app writes `opendesk.cmd` at startup to `%LOCALAPPDATA%\Microsoft\WindowsApps`, which is on
`PATH` (the uninstaller removes it). The Linux packages install `/usr/bin/opendesk`; the same binary is the app
and the CLI.

| Command | Effect |
| --- | --- |
| `opendesk server <url>` | Set the relay; the running app reconnects within about a second |
| `opendesk status`, `opendesk id`, `opendesk help` | Show the device name, desk ID and relay |
| `opendesk stop` | Stop OpenDesk and its watchdog (Windows: needs an elevated terminal when the app runs elevated) |
| `opendesk uninstall` | Stop everything and remove autostart entries before uninstalling (Linux: the per-user entry) |
