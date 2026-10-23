# Autostart, watchdog and CLI

Code: `src/infra/autostart.rs`, `src/cli.rs`, installer in `installer/opendesk.iss`.

## Single instance

A named mutex allows one running instance. A second launch wakes the running one (shows its window) and exits.

## Autostart

| Install type | Mechanism |
| --- | --- |
| Installed (setup) | A Task Scheduler task `OpenDesk Autostart` starts the app elevated at logon, with restart on failure. Elevation lets viewers control elevated windows (Task Manager, "Run as administrator" apps). A non-elevated launch hands over to the task. |
| Portable (no task) | `HKCU\...\Run` entries for the app and a VBScript watchdog that restarts it within 10 s if it is killed. "Stop & Exit" in the tray pauses the watchdog until the next launch. |

Closing the window keeps OpenDesk in the tray, so the machine stays reachable.

## CLI

At startup the app writes `opendesk.cmd` to `%LOCALAPPDATA%\Microsoft\WindowsApps`, which is on `PATH` (the
uninstaller removes it):

| Command | Effect |
| --- | --- |
| `opendesk server <url>` | Set the relay; the running app reconnects within about a second |
| `opendesk status`, `opendesk id`, `opendesk help` | Show the device name, desk ID and relay |
| `opendesk stop` | Stop OpenDesk and its watchdog (needs an elevated terminal when the app runs elevated) |
| `opendesk uninstall` | Stop everything and remove autostart entries before uninstalling |
