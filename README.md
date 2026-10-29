<div align="center">

  <img src="./assets/icon.png" alt="OpenDesk logo" width="150" />

  <h1>OpenDesk</h1>

  <h3>Self-hosted remote desktop for Windows, Linux and Android</h3>

  <p>Native clients, your own relay, no third-party cloud.</p>

  <p>
    <a href="https://github.com/abdullah-fikri/opendesk/actions/workflows/ci.yml"><img src="https://github.com/abdullah-fikri/opendesk/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
    <a href="./LICENSE"><img src="https://img.shields.io/badge/License-AGPLv3%20%2F%20Commercial-blue.svg" alt="License: AGPLv3 / Commercial" /></a>
    <img src="https://img.shields.io/badge/version-1.2.0-informational.svg" alt="Version 1.2.0" />
    <img src="https://img.shields.io/badge/Stack-Rust%20%7C%20Go%20%7C%20Kotlin-orange.svg" alt="Stack" />
  </p>

  <p>
    <a href="https://github.com/abdullah-fikri/opendesk/releases/latest"><b>Download</b></a> ·
    <a href="#features"><b>Features</b></a> ·
    <a href="#editions"><b>Editions</b></a> ·
    <a href="#quick-start"><b>Quick start</b></a> ·
    <a href="#security"><b>Security</b></a> ·
    <a href="#repository"><b>Repository</b></a> ·
    <a href="./CONTRIBUTING.md"><b>Contributing</b></a>
  </p>

</div>

---

## About

OpenDesk lets you control Windows and Linux computers from other computers or from an Android phone, through a
relay server you run yourself. Clients are native (Rust with Slint on Windows and Linux, Kotlin on Android); the
relay is a single Go binary backed by PostgreSQL. No screen, keystroke or file passes through anyone else's servers.

| | View other devices | Can be controlled |
| --- | --- | --- |
| Windows | Yes | Yes |
| Linux (X11 session) | Yes | Yes |
| Linux (Wayland session) | Yes | No (shows how to switch to X11) |
| Android | Yes | Never: the app is a viewer only |

Feature differences between platforms are listed in [docs/platforms.md](docs/platforms.md).

## Features

**Remote control**
- Up to four hosts at once from one desktop viewer, as tabs or a split grid; Android views one at a time with
  touch gestures, zoom and a soft keyboard.
- Windows: DXGI Desktop Duplication capture with GDI fallback. Linux: X11 (MIT-SHM). Only changed regions are sent
  (JPEG atlases), with H.264 for sustained motion (hardware encoder on Windows, Cisco OpenH264 on Linux when
  installed).
- Stream resolution follows the viewer window automatically, or native, 1080p, 720p, 480p.
- Multi-monitor hosts with a monitor picker; cursor shape mirrored on the viewer.
- Direct encrypted UDP path between desktops (Noise protocol) when the network allows it, with automatic fallback
  to the relay.

**Collaboration**
- Two-way voice (16, 24 or 48 kHz) with noise gating; either side can mute the other.
- File transfer by drag-and-drop, file picker or Ctrl+V, including folders, verified with SHA-256 and pasted
  straight into the remote Explorer window (Windows).
- Copy files on one Windows machine and paste on the other: files are pulled only when pasted.
- Clipboard text sync that follows the focused session.

**Host control** (Windows hosts; viewers hide what a host cannot do)
- Privacy screen on the host (frozen, maintenance or black), invisible to the viewer's stream.
- Block the host's physical keyboard and mouse during a session.
- System keys straight from your keyboard on Windows (Win, Alt+Tab, Ctrl+Shift+Esc, ...); a shortcut menu on Linux.
- Client token (admin edition): only devices an administrator lists may connect; each receives its token
  automatically, so devices need only the relay address.

**Operations**
- Unattended hosts: start at logon (Windows: elevated, so admin windows can be controlled, with a watchdog in
  portable installs; Linux: XDG autostart), stay in the tray.
- `opendesk` CLI on Windows and Linux to set the relay, show the device ID, stop or prepare uninstall.
- Admin edition with a live list of every device on the relay and a read-only monitor wall (up to 16 live
  thumbnails per page, paged and optionally auto-rotating).
- Relay with per-device credentials, metrics, health endpoint, versioned database migrations and a published
  container image.

## Editions

| Edition | File | For |
| --- | --- | --- |
| Standard | Windows: `OpenDesk-Setup-<version>.exe` / `OpenDesk-<version>.exe`; Linux: `opendesk_<version>-1_amd64.deb` / `opendesk-<version>-linux-x86_64.tar.gz` | Desktops: view other machines and be controlled |
| Admin | `OpenDesk-Admin-Setup-<version>.exe`; `opendesk-admin_<version>-1_amd64.deb` | Administrators: Standard plus the relay's device list |
| Headless host | `OpenDesk-Host-Setup-<version>.exe`; `opendesk-host_<version>-1_amd64.deb` | Servers, VMs and kiosks: no window or tray, incoming sessions only |
| Android | `OpenDesk-<version>.apk` (`.aab` for Google Play) | Phones: view and control a computer (viewer only) |
| Relay | `ghcr.io/abdullah-fikri/opendesk-relay` | Your server |

Download them from [GitHub Releases](https://github.com/abdullah-fikri/opendesk/releases).

## Quick start

**1. Run a relay** (Linux server with Docker and PostgreSQL):

```bash
git clone https://github.com/abdullah-fikri/opendesk.git && cd opendesk
cp .env.example .env            # set DB_PASSWORD
docker compose up -d
```

Put a TLS terminator in front of `127.0.0.1:8088` (for example a Cloudflare Tunnel) so clients can reach
`wss://relay.example.com`. Details: [server-relay/docs/deployment.md](server-relay/docs/deployment.md).

**2. Install the clients** and point them at the relay: in the app's relay settings, or with
`opendesk server wss://relay.example.com` on Windows and Linux.

- Windows: run the installer (or the portable `.exe`).
- Debian/Ubuntu: `sudo apt install ./opendesk_<version>-1_amd64.deb`. For H.264 video, also run
  `/usr/share/opendesk/install-openh264.sh` (downloads Cisco's OpenH264; without it video uses JPEG only).
- Android: install the APK; see [client-android/README.md](client-android/README.md) for signing and Play Protect.

**3. Connect.** Each device shows its 9-digit ID. Enter the other device's ID and press Connect. Desktop hosts
accept immediately.

## Security

Read this before exposing a relay:

- **The relay is open by default.** Anyone who can reach it can register. Provision per-device tokens with
  `OPENDESK_DEVICE_CREDENTIALS` ([guide](server-relay/docs/provisioning.md)) and keep the address private.
- **Desktop hosts are unattended.** Windows and Linux hosts accept every session the relay grants, without a
  prompt. Combined with an open relay, knowing a device ID is enough to control it. Android phones can never be
  controlled.
- **Encryption:** use `wss://` (the relay refuses plaintext from remote addresses); the direct path uses Noise
  (`Noise_NN_25519_ChaChaPoly_BLAKE2s`) bound to the relay-approved pair.
- **Files from a peer** are validated, staged, checksum-verified and never overwrite existing files.

More: [docs/architecture.md](docs/architecture.md#security-model). Report vulnerabilities privately: [SECURITY.md](SECURITY.md).

## Repository

```text
opendesk/
├── client-desktop/   Windows and Linux client (Rust + Slint) docs: client-desktop/docs
├── core-engine/      Capture, codecs, input (Rust library)   docs: core-engine/README.md
├── client-android/   Android viewer (Kotlin)                 docs: client-android/docs
├── server-relay/     Relay (Go + PostgreSQL)                 docs: server-relay/docs
├── installer/        Inno Setup script (Windows)
├── packaging/        Linux desktop entries and the OpenH264 installer
├── docs/             Architecture, wire protocol, CI/CD, cross-component features
├── scripts/          Repository tooling (version check)
├── .github/          CI, release and Dependabot configuration
├── Makefile          Developer tasks (make help)
├── VERSION           Release version, checked against every component
└── docker-compose.yml
```

- [Architecture](docs/architecture.md) · [Platforms](docs/platforms.md) · [Wire protocol](docs/protocol.md) ·
  [CI/CD and releases](docs/ci-cd.md)
- [Contributing](CONTRIBUTING.md): build, test, layering rules and code conventions
- [Security policy](SECURITY.md) · [Contributor License Agreement](CLA.md) · [Maintainers](MAINTAINERS.md)

## Building from source

`make` works on Windows (cmd.exe) and Linux (sh); `make help` lists the targets for the OS you are on.

```bash
make help        # all targets
make test        # every unit test
make relay       # builds/relay/opendesk-relay(.exe)
make android     # builds/android/OpenDesk.apk

# Windows
make windows     # builds\windows\OpenDesk.exe
make setup       # installer (needs Inno Setup 6)

# Linux
make linux       # builds/linux/opendesk
make linux-deb   # builds/linux/opendesk*.deb (needs `cargo install cargo-deb`)
make linux-tar   # builds/linux/opendesk-<version>-linux-<arch>.tar.gz
```

Prerequisites per component are listed in [CONTRIBUTING.md](CONTRIBUTING.md#prerequisites).

---

## Support

If you find OpenDesk useful and want to support its ongoing development, server costs, or new hardware testing, consider buying us a coffee or donating!

<p align="center">
  <a href="https://saweria.co/yourusername"><img src="https://img.shields.io/badge/Saweria-Donate-FFA500?style=for-the-badge&logo=cashapp&logoColor=white" alt="Saweria" /></a>
  &nbsp;
  <a href="https://trakteer.id/yourusername"><img src="https://img.shields.io/badge/Trakteer-Support-red?style=for-the-badge&logo=ko-fi&logoColor=white" alt="Trakteer" /></a>
  &nbsp;
  <a href="https://www.buymeacoffee.com/yourusername"><img src="https://img.shields.io/badge/Buy_Me_A_Coffee-Support-yellow?style=for-the-badge&logo=buy-me-a-coffee&logoColor=black" alt="Buy Me A Coffee" /></a>
  &nbsp;
  <a href="https://paypal.me/yourusername"><img src="https://img.shields.io/badge/PayPal-Donate-00457C?style=for-the-badge&logo=paypal&logoColor=white" alt="PayPal" /></a>
</p>

---

## License

OpenDesk is distributed under a **Dual-Licensing Model**:
- **Community & Open-Source**: Free under the [GNU Affero General Public License v3.0 (AGPLv3)](./LICENSE).
- **Commercial & Enterprise**: For organizations requiring closed-source deployments, proprietary integrations, SLA support, or white-labeling without AGPLv3 copyleft constraints, a **Commercial License** is available. See [LICENSE](./LICENSE) for details.
