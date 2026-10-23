<div align="center">

  <img src="./assets/icon.png" alt="OpenDesk Logo" width="150" />

  <h1>OpenDesk</h1>

  <h3>High-Performance, Lightweight, and Resilient Remote Desktop</h3>

  <p>
    Native performance for modern fleets — <b>Zero Electron, Zero Web-Wrappers</b>
  </p>

  <p>
    <a href="./LICENSE"><img src="https://img.shields.io/badge/License-AGPLv3%20%2F%20Commercial-blue.svg" alt="License: AGPLv3 / Commercial" /></a>
    <img src="https://img.shields.io/badge/Platform-Windows%20%7C%20Android%20%7C%20Linux%20(Server)-green.svg" alt="Platform" />
    <img src="https://img.shields.io/badge/Stack-Rust%20%7C%20Go%20%7C%20Kotlin-orange.svg" alt="Stack" />
    <a href="#-support--donate"><img src="https://img.shields.io/badge/Support-Donate-ff69b4.svg" alt="Donate" /></a>
  </p>

  <p>
    | <a href="#-about-opendesk"><b>About</b></a> |
    <a href="#-key-features"><b>Features</b></a> |
    <a href="#-application-editions"><b>Editions</b></a> |
    <a href="#️-monorepo-architecture"><b>Architecture</b></a> |
    <a href="#-quick-start"><b>Quick Start</b></a> |
    <a href="./CONTRIBUTING.md"><b>Contributing</b></a> |
    <a href="#-support--donate"><b>Donate</b></a> |
  </p>

</div>

---

## 📖 About OpenDesk

**OpenDesk** is a modern, high-performance remote desktop and device management solution engineered to deliver fluid, low-latency remote control without relying on proprietary cloud intermediaries.

Built from the ground up for speed, reliability, and security, OpenDesk combines a pure **Rust** client with direct **Windows DXGI Desktop Duplication** and an ultra-fast **Go** relay server.

### Why OpenDesk?
- **Pure Native Speed**: Most modern remote tools bundle heavy web engines (Electron/Chromium) that consume 300MB+ of RAM and introduce input lag. OpenDesk runs on native Rust and Slint UI, consuming only **~30MB of RAM** while maintaining **60+ FPS** hardware-accelerated capture.
- **Complete Data Sovereignty**: Never route sensitive desktop data or internal company screens through third-party servers. OpenDesk's lightweight Go relay server can be self-hosted on any private VPS, cloud container, or local network in seconds.
- **Fleet & Server Isolation**: Ideal for businesses and IT support teams. Clients communicate strictly within their assigned private relay, guaranteeing zero cross-tenant visibility or external data leakage.
- **Battle-Tested Resilience**: Built for real-world network conditions—featuring chunk-based file transfers that automatically resume through drops, multi-tier adaptive video/audio scaling, and an unattended host daemon with a 10-second watchdog self-healing mechanism.

---

## 🌟 Key Features

### 🚀 Performance & Native Architecture
- **Ultra-Low Latency & High FPS**: Native screen capture via Windows DXGI Desktop Duplication API with dirty-rect differential detection executed at hardware speed.
- **Ultra-Light Memory Footprint (~30MB RAM)**: Written in pure Rust and Slint UI; eliminates heavy browser engines and web runtimes.

### 🎥 Adaptive Display & Audio Streaming
- **Dynamic Capture Scaling**: Adapt display streaming to your bandwidth and network latency. Toggle between native **1:1** pixel-perfect capture or scale dynamically to **1080p**, **720p**, and **480p** for uninterrupted smoothness.
- **Multi-Tier Real-Time Audio**: Low-latency system audio streaming with selectable quality profiles tailored to current network stability.

### 📁 Resilient File Transfer & Clipboard
- **Interruption-Proof File Transfers**: Chunk-based file transfers continue seamlessly even across transient network drops, avoiding painful re-transfers of large files.
- **Bidirectional Clipboard**: Instant text and clipboard synchronization between host and viewer.

### 🏢 Server-Level Isolation (Enterprise Ready)
- **Dedicated Multi-Tenant & Fleet Isolation**: Clients communicate strictly within their assigned relay server. Hosts on one relay are completely isolated and invisible to clients on another.
- **Zero Third-Party Dependency**: Complete data sovereignty—ideal for corporate internal networks, private clouds, and regulated industries.

### 🛡️ High-Availability Daemon & Watchdog
- **Auto-Start on Boot**: Host daemon initializes silently on system startup.
- **10-Second Watchdog Self-Healing**: Monitored by a background supervisor. If the host process is terminated via End Task or unexpected crash, the watchdog automatically respawns the host service within **10 seconds**, guaranteeing continuous unattended access.

---

## 📦 Application Editions

OpenDesk is distributed in four specialized editions tailored to different operational roles:

| Edition | Target File | Interface | Description |
| :--- | :--- | :--- | :--- |
| **OpenDesk Standard** | `OpenDesk-Setup.exe` | Standard GUI | Full-featured installer package for regular desktop users, featuring connection manager and address book. |
| **OpenDesk Portable** | `OpenDesk.exe` | Standard GUI | Single standalone binary with zero installation wizard required. Runs directly from USB drives or temp folders. |
| **OpenDesk Admin** | `OpenDesk-Admin.exe` | Admin GUI | Dedicated administrator console featuring an integrated **Server Host List** to view, monitor, and connect to all active endpoints on the relay. |
| **OpenDesk Host** | `OpenDesk-Host.exe` | Headless (CLI) | Silent, zero-GUI daemon designed for servers, unattended cloud VMs, and kiosk terminals. Minimum resource consumption. |
| **OpenDesk Android** | `OpenDesk.apk` | Mobile GUI | Native mobile remote control client using `MediaProjection` (screen capture) and `AccessibilityService` (touch/gestures). |

---

## 🏗️ Monorepo Architecture

```
opendesk/
├── core-engine/       # Native Rust engine (DXGI capture, dirty-rect diff, input injection)
├── client-desktop/    # Windows Desktop GUI (Rust + Slint UI + System Tray + Watchdog)
├── client-android/    # Android Client (Kotlin + MediaProjection + Accessibility)
├── server-relay/      # Relay Gateway Server (Go + WebSocket + Protobuf + PostgreSQL)
├── protocol/          # Wire Protocol Definitions (Protocol Buffers v3)
├── installer/         # Windows Installer Scripts (Inno Setup 6)
├── tools/             # Automation, testing, launcher & daemon management scripts
├── builds/            # Production binaries destination (git ignored)
└── Makefile           # Master build automation
```

---

## 🚀 Quick Start

### 1. Pre-Built Binaries
Output binaries can be compiled locally via `make all` or retrieved from [builds/](./builds/):
- **Standard Installer**: `builds/windows/OpenDesk-Setup.exe`
- **Admin Edition**: `builds/windows/OpenDesk-Admin-Setup.exe`
- **Portable Binary**: `builds/windows/OpenDesk.exe`
- **Headless Host**: `builds/windows/OpenDesk-Host.exe`
- **Android APK**: `builds/android/OpenDesk.apk`
- **Relay Server**: `builds/other/server-relay.exe`

### 2. Run Self-Hosted Relay (Docker Compose)
Start your private relay server in seconds:
```bash
docker compose up -d
```
The relay service listens on TCP port `8088`.

### 3. Connect Two Computers
1. Launch `OpenDesk.exe` on both machines.
2. Read the **Desk ID** from the host computer.
3. On the viewer computer, enter the host's Desk ID and click **Connect**.
4. The host displays a consent modal; click **Accept** to initiate the session.

### 4. Windows Host CLI Utility
```cmd
# Check current Desk ID, connection status, and relay URL
opendesk status

# Print Desk ID only (ideal for automated IT enrollment scripts)
opendesk id

# Switch relay server endpoint on the fly
opendesk server --wss://relay.your-company.com/ws
```

---

## 🔒 Security & Privacy

- **Encrypted Channels**: All screen frames, audio, and inputs travel over TLS/WSS-encrypted tunnels.
- **Consent Confirmation**: Incoming remote sessions require explicit user acceptance unless unattended credentials are pre-configured.
- **Privacy Screen**: Blank the physical host display during active sessions to protect sensitive data from on-site shoulder surfing.

---

## 🤝 Contributing & Development

Looking to build from source, set up local testing environments, or contribute to code?  
Read our technical contributor guide:  
👉 **[CONTRIBUTING.md](./CONTRIBUTING.md)**

---

## 💖 Support & Donate

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

## 📄 License

OpenDesk is distributed under a **Dual-Licensing Model**:
- **Community & Open-Source**: Free under the [GNU Affero General Public License v3.0 (AGPLv3)](./LICENSE).
- **Commercial & Enterprise**: For organizations requiring closed-source deployments, proprietary integrations, SLA support, or white-labeling without AGPLv3 copyleft constraints, a **Commercial License** is available. See [LICENSE](./LICENSE) for details.
