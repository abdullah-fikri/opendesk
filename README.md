# OpenDesk 🖥️⚡

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Android%20%7C%20Linux%20(Server)-green.svg)]()
[![Stack](https://img.shields.io/badge/Stack-Rust%20%7C%20Go%20%7C%20Kotlin-orange.svg)]()

**OpenDesk** is a high-performance, lightweight, and secure open-source remote desktop solution (an independent, self-hosted alternative to AnyDesk, RustDesk, and TeamViewer). Built with native performance in mind—**Zero Electron, Zero Web-Wrappers**.

---

## 🌟 Key Features

- **Ultra-Low Latency & High FPS**: Native screen capture via Windows DXGI Desktop Duplication API combined with dirty-rect differential detection at hardware speed.
- **Minimal Memory Footprint (~30MB RAM)**: Built with pure Rust and Slint UI, avoiding heavy browser runtime engines.
- **Multi-Platform Support**:
  - **Windows Client**: Standard GUI client, Admin Console (Server Host List), and Headless Host Daemon.
  - **Android Client**: Full two-way remote support with `MediaProjection` (screen capture) and `AccessibilityService` (touch & gesture injection).
- **Hardware-Tied Desk ID**: Unique device identifiers derived from hardware hashes (Windows hardware hash & Android secure ID) without requiring third-party accounts.
- **Self-Hosted High-Concurrency Relay**: Ultra-fast Go relay gateway leveraging Protocol Buffers v3 over WebSockets, packaged for Docker Compose.
- **Comprehensive Remote Control Suite**:
  - Real-time audio streaming
  - Drag-and-drop file transfers
  - Bidirectional clipboard synchronization
  - Headless/silent host daemon for unattended access
  - Secure connection consent confirmation dialogs
  - Privacy screen support (blank host display during active sessions)

---

## 🏗️ Monorepo Architecture

```
opendesk/
├── core-engine/       # Native Rust engine (DXGI capture, dirty-rect diff, input injection)
├── client-desktop/    # Windows Desktop GUI (Rust + Slint UI + System Tray)
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

### 1. Download Binaries
Pre-built binaries can be compiled directly via `make all` or retrieved from [builds/](file:///d:/opendesk/builds/) / GitHub Releases:
- **Windows Installer**: `OpenDesk-Setup.exe`
- **Windows Portable**: `OpenDesk.exe`
- **Android APK**: `OpenDesk.apk`
- **Relay Server**: `server-relay.exe` / Docker container

### 2. Self-Host Relay Server (Docker)
To spin up your private relay server with PostgreSQL session storage:
```bash
docker compose up -d
```
The relay server listens on port `8088`.

### 3. Connect Two Machines
1. Launch `OpenDesk.exe` on both computers.
2. Note the **Desk ID** on the host machine.
3. On the viewer machine, enter the host's Desk ID and click **Connect**.
4. On the host machine, click **Accept** on the consent prompt.

### 4. Windows Host CLI Commands
```cmd
# Check current Desk ID, status, and relay URL
opendesk status

# Print Desk ID only (ideal for automated provisioning scripts)
opendesk id

# Point client to a custom self-hosted relay server
opendesk server --wss://relay.your-domain.com/ws
```

---

## 🔒 Security & Privacy

- All payload frames, inputs, and controls are transmitted via encrypted WSS channels.
- Zero telemetry or third-party dependency; full data sovereignty remains on your self-hosted relay.
- Privacy Screen capability blacks out physical screens during remote administration.

---

## 🤝 Contributing & Development

Looking to compile from source, run local integration tests, or contribute features?  
Check out our comprehensive developer guide:  
👉 **[CONTRIBUTING.md](file:///d:/opendesk/CONTRIBUTING.md)**

---

## 📄 License

OpenDesk is licensed under the [MIT License](LICENSE).
