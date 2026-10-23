# OpenDesk for Android

Control a Windows PC from the phone, or let a PC control the phone. Kotlin, no third-party UI framework;
OkHttp for the WebSocket and Gson for JSON.

## Build

Requirements: JDK 17, Android SDK 34.

```bash
./gradlew assembleDebug          # app/build/outputs/apk/debug/app-debug.apk
./gradlew testDebugUnitTest lintDebug
```

From the repository root on Windows: `make android` (copies the APK to `builds\android\OpenDesk.apk`). Release
builds are signed in CI when signing secrets are configured ([docs/ci-cd.md](../docs/ci-cd.md)). The version comes
from the repository's `VERSION` file.

## Use

1. Open the app and set the relay address (and a device token if your relay requires one).
2. To control a PC: enter its desk ID and tap **Connect**.
3. To be controlled: enable the OpenDesk accessibility service when asked, and allow screen capture when a PC
   connects. The phone asks for consent on every session.

## Layout

| Package | Layer | Contents |
| --- | --- | --- |
| `ui.main`, `ui.viewer` | Presentation | `MainActivity`, `RemoteViewerActivity` |
| `session` | Use cases | `RelayConnection` (message routing), `FileReceiver`, `RemoteInputHandler` |
| `data.dto` | Wire DTOs | Relay messages |
| `data.network`, `data.settings` | Adapters | `GatewayClient` (WebSocket), `SettingsRepository` (preferences) |
| `platform` | Android services | Screen capture, accessibility input, voice |
| `domain` | Domain | Desk IDs, relay URL rules |

## Documentation

[Architecture](docs/architecture.md) · features: [viewing a PC](docs/features/viewing.md),
[being controlled](docs/features/hosting.md), [receiving files](docs/features/file-receive.md),
[settings](docs/features/settings.md)
