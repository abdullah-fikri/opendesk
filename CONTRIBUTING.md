# Contributor & Developer Guide (CONTRIBUTING.md) 🛠️

Welcome to the **OpenDesk** developer guide! This document provides technical instructions for cloning, setting up the local environment, building components, running local tests, and contributing to the project.

---

## 📋 System Prerequisites

Ensure your development environment meets the following minimum requirements:

| Component | Target Modules | Minimum Version | Notes |
| :--- | :--- | :--- | :--- |
| **Rust Toolchain** | `core-engine`, `client-desktop` | Stable 1.75+ (`x86_64-pc-windows-msvc`) | Includes `cargo` and `rustc` |
| **Go** | `server-relay` | Go 1.21+ | For the high-concurrency relay gateway |
| **JDK & Android SDK** | `client-android` | JDK 17+ & Android Studio Iguana+ | Gradle 8.x, Android SDK API 34 |
| **Inno Setup** *(Optional)* | `installer` | Inno Setup 6.x | Required to compile `.iss` installer bundles |
| **Docker** *(Optional)* | `server-relay` | Docker & Docker Compose | For local PostgreSQL + Relay container testing |
| **GNU Make** *(Optional)* | Root Automation | GNU Make (via MinGW, Chocolatey, or Scoop) | For unified `make` targets |

---

## 🚀 Workspace Setup

### 1. Clone the Repository
```bash
git clone https://github.com/your-org/opendesk.git
cd opendesk
```

### 2. Module Responsibilities
- **`core-engine/`**: Pure Rust core crate. Implements screen capture (Windows DXGI Desktop Duplication API), dirty-rect differential detection, and hardware input injection (`SendInput`).
- **`client-desktop/`**: Windows Desktop binary crate (Rust + Slint UI). Handles WebSocket communication, wire protocol serializing, audio streaming, system tray daemon, and user interface.
- **`client-android/`**: Native Kotlin Android application. Implements `ScreenCaptureService` (MediaProjection) and `OpenDeskAccessibilityService` (touch/gesture simulation).
- **`server-relay/`**: Go-based high-throughput relay server. Manages Desk ID registration, peer signaling, and binary data relaying.
- **`protocol/`**: Protobuf definitions (`messages.proto`) for inter-component wire communication.
- **`tools/`**: Development scripts for automated testing, local relays, and single-device dual-instance simulations.

---

## 🔨 Build Guide

### Master Makefile (Recommended)

Run `make help` to inspect available automation targets:

```bash
# Print help menu and all build targets
make help

# 1. Build Windows Client (Standard GUI)
make build-windows

# 2. Build Windows Client (Admin Edition with Server Host List)
make build-windows-admin

# 3. Build Windows Client (Headless Zero-GUI Host Daemon)
make build-windows-host

# 4. Build Relay Gateway Server (Go)
make build-server

# 5. Build Android Debug APK
make build-android

# 6. Build Windows Setup Installer (.exe)
make build-setup

# 7. Build All Components (Windows + Server + Installer)
make all

# 8. Clean build artifacts and compiler caches
make clean
```

All compiled output binaries are automatically placed into the `builds/` directory (`builds/windows/`, `builds/android/`, `builds/other/`).

---

### Per-Component Manual Compilation

If you are focusing on an individual component:

#### A. Core Engine (Rust)
```bash
cd core-engine
cargo check
cargo test
cargo build --release
```

#### B. Client Desktop (Rust + Slint)
```bash
cd client-desktop
# Run debug GUI locally
cargo run

# Build release binary with Admin features
cargo build --release --features admin

# Build release binary as a Headless Host daemon
cargo build --release --features headless
```

#### C. Relay Server (Go)
```bash
cd server-relay
go mod download

# Run directly in development
go run main.go

# Compile optimized release binary
go build -ldflags="-s -w" -o server-relay.exe .
```

#### D. Android Client (Gradle)
```bash
cd client-android
# Windows CMD / PowerShell
gradlew.bat assembleDebug

# Linux / macOS
./gradlew assembleDebug
```
The output APK is generated at `client-android/app/build/outputs/apk/debug/app-debug.apk`.

---

## 🧪 Local Testing & Debugging Workflow

To verify remote desktop behavior on a single development machine:

### 1. Launch Local Relay Server
Using Docker:
```bash
docker compose up -d
```
Or run directly using the provided batch script:
```cmd
tools\start-relay.bat
```

### 2. Dual-Client Simulation on Single PC
Run the test script:
```cmd
tools\uji-coba-1-device.bat
```
This script will:
1. Launch a **Host instance** snapped to the left side of your screen.
2. Launch a **Viewer instance** snapped to the right side of your screen.
3. Both instances automatically connect to the local relay (`ws://127.0.0.1:8088/ws`).
4. Copy the Host's Desk ID into the Viewer and click **Connect** to verify video frames and input streaming.

### 3. Configuration & Log Paths
- **Windows User Config & Address Book**: `%APPDATA%\OpenDesk\`
  - `history.json` (recent connections and address book)
  - `opendesk_config.json` (relay URL and preferences)

---

## 🔄 Protocol Changes (`protocol/messages.proto`)

When modifying message definitions in `protocol/messages.proto`:

1. Ensure the `protoc` compiler is installed.
2. Regenerate Go bindings:
   ```bash
   protoc --proto_path=protocol --go_out=server-relay/protocol --go_opt=paths=source_relative protocol/messages.proto
   ```
3. Update deserializers in `client-desktop/src/protocol.rs` and `core-engine`.
4. Update message handlers in `client-android/app/src/main/java/com/opendesk/app/GatewayClient.kt`.

---

## 📐 Code Style & Conventions

Before submitting pull requests, ensure your code adheres to standard linters:

### Rust
```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

### Go
```bash
gofmt -s -w .
go vet ./...
```

### Git Commit Conventions
Follow **Conventional Commits**:
- `feat(desktop): add audio device selector modal`
- `fix(core): resolve DXGI memory leak on display re-enumeration`
- `perf(relay): optimize WebSocket buffer allocation`
- `docs: update setup instructions in CONTRIBUTING.md`

---

## 📬 Submitting a Pull Request (PR)

1. Fork the repository and create a feature branch (`git checkout -b feature/hardware-h264`).
2. Make atomic, well-formatted commits.
3. Ensure affected modules compile without errors or warnings.
4. Open a Pull Request detailing:
   - Summary of the problem and proposed solution.
   - Any architectural or protocol modifications.
   - Exact steps taken to verify functionality.
