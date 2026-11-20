# OpenDesk for Android

Control a computer from your phone. The Android app is a **viewer only**: it connects to OpenDesk desktops
through your relay, and nothing can ever connect to the phone or control it. Kotlin, no third-party UI framework;
OkHttp for the WebSocket and Gson for JSON.

## Build

Requirements: JDK 17 or newer (Android Studio's bundled JBR works), Android SDK platform 36.

```bash
./gradlew assembleDebug                 # both editions: app/build/outputs/apk/{standard,admin}/debug/
./gradlew testStandardDebugUnitTest testAdminDebugUnitTest lintStandardDebug lintAdminDebug
./gradlew assembleRelease               # R8-minified release APKs, signed when a keystore is configured
```

### Editions

Like the desktop (`--features admin`), the edition is chosen when building, never in the app:

| Gradle flavor | Application ID | Name | What it adds |
| --- | --- | --- | --- |
| `standard` | `com.opendesk.app` | OpenDesk | Devices lists the computers this phone connected to. |
| `admin` | `com.opendesk.app.admin` | OpenDesk Admin | Registers with `isAdmin`: Devices lists every device on the relay with its presence, and the [monitor wall](docs/features/monitor-wall.md). |

`BuildConfig.ADMIN` is read only through `domain/Edition.kt`. Both editions can be installed side by side; the
admin edition derives its own desk ID so the two never register the same one. The relay still decides: with device
credentials only its stored `admin` flag counts, so the admin APK alone grants nothing.

On Windows use `gradlew.bat`; if `JAVA_HOME` is unset, point it at `C:\Program Files\Android\Android Studio\jbr`.
From the repository root: `make android` (copies the APK to `builds/android/OpenDesk.apk`). The version comes
from the repository's `VERSION` file.

## Use

1. Open the app and set the relay address (`wss://relay.example.com`) and, if your relay requires one, this
   device's token.
2. Enter the computer's desk ID and tap **Connect**.

In the viewer:

| Gesture or button | Action |
| --- | --- |
| Tap / double tap | Left click / double click |
| Long press, or two-finger tap | Right click |
| Two-finger vertical drag | Scroll: the page follows the finger line by line; a flick coasts |
| Pinch (move the fingers to pan) | Zoom up to 5x; in trackpad mode a zoomed or filled view follows the cursor; **Fit** resets |
| One-finger drag | Trackpad mode: moves the cursor. Touch mode: the cursor jumps to the finger |
| **Keyboard** | Soft keyboard; text is typed on the computer, Backspace, Enter and arrows are sent as keys |
| **Esc**, **Tab**, **Win** | Those keys; Win opens the Start menu |
| **Hold** | Keeps the left button down for dragging until you tap again |
| Hold the right edge, slide, lift | Session menu: keyboard, trackpad/touch, display (fit, fill, next monitor), keys, scroll mode, more controls. A tap on the grip keeps it open |
| Swipe down from the top | Session pill: computer, FPS, E2E, Disconnect; hides again by itself |

Rotation keeps the session, zoom and picture. Leaving the app pauses the picture and mutes the microphone; the
session stays open until you press Back or Disconnect. A dropped relay connection is retried automatically
(1 s doubling to 30 s, and immediately when the network returns).

## Why Play Protect used to block the app, and what changed

Earlier versions could also *be controlled*. To do that they declared an **AccessibilityService** (to inject
taps), a **MediaProjection foreground service** (to capture the screen) and accepted **remote input** over the
network, plus the overlay permission. That is exactly the permission profile of Android banking trojans, so:

- Google Play Protect flagged or blocked the sideloaded APK, and installing it meant turning Play Protect off.
- Banking and payment apps refuse to run while an accessibility service from an app outside the Play Store is
  enabled, and some flag the device as compromised.

The app is now a viewer only. It contains no accessibility service, no screen capture, no overlay, no
foreground service and no remote-input handling, and it registers with the relay as `viewer-only`, so desktops and
relays never treat the phone as a host (an `incoming-request` from an older relay is declined automatically).

| Permission | Before | Now |
| --- | --- | --- |
| `INTERNET`, `ACCESS_NETWORK_STATE` | yes | yes (relay connection; reconnect when the network returns) |
| `RECORD_AUDIO` | yes | yes, asked only when you turn the microphone on |
| `MODIFY_AUDIO_SETTINGS`, `VIBRATE` | yes | removed (not needed) |
| `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_MEDIA_PROJECTION` | yes | removed |
| `SYSTEM_ALERT_WINDOW` (draw over other apps) | yes | removed |
| `BIND_ACCESSIBILITY_SERVICE` (accessibility service) | yes | removed |
| `POST_NOTIFICATIONS` (runtime request) | requested | removed |

Other hardening: release builds are minified with R8 and target the latest SDK (36); cleartext traffic is refused
except to `localhost` (debug builds allow `ws://` anywhere for LAN testing); app data is excluded from cloud
backup and device transfer because it holds the relay token; there is no dynamic code loading.

**Never disable Play Protect to install OpenDesk.** If a warning still appears, use one of the options below.

## Distribution

Play Protect warns about any sideloaded app it has not seen before ("unknown app"), however clean. In order of
preference:

1. **Google Play, internal testing or closed testing track** (the real fix). Upload the signed release build
   (as an `.aab` via `./gradlew bundleRelease`, or the APK) in the Play Console, add testers by email, and they
   install it from the Play Store with no warnings. Internal testing needs no review and is limited to 100
   testers; closed testing reaches more people after a short review.
2. **Sideloaded APK signed with your own key.** Always sign releases with the same key (an unsigned or
   debug-signed APK is far more likely to be blocked). If Play Protect still warns, submit the APK through
   Google's Play Protect appeal form ("Play Protect appeals" in the Google Play Console Help) so it is scanned
   and allow-listed; future builds signed with the same key benefit from that reputation.
3. **Debug builds** are for development only.

## Release signing

The keystore and its passwords are never committed (`*.jks`, `*.keystore` and `keystore.properties` are
ignored). Create a key once and keep it backed up: every update must be signed with the same key.

```bash
keytool -genkeypair -v -keystore opendesk-release.jks -alias opendesk \
  -keyalg RSA -keysize 4096 -validity 10000
```

Official releases are signed with the certificate whose SHA-256 is
`65:F5:86:32:9A:15:F2:CD:36:09:FD:EA:1A:C2:50:D7:D3:F7:99:25:55:E9:78:79:09:D6:CE:A0:30:36:DA:C6`; check a download
with `apksigner verify --print-certs OpenDesk-<version>.apk`.

Signing values are read from environment variables, or from Gradle properties of the same name (for example
in `~/.gradle/gradle.properties`, outside the repository):

| Name | Value |
| --- | --- |
| `ANDROID_KEYSTORE_FILE` | Absolute path to the `.jks` file |
| `ANDROID_KEYSTORE_PASSWORD` | Keystore password |
| `ANDROID_KEY_ALIAS` | Key alias (`opendesk` above) |
| `ANDROID_KEY_PASSWORD` | Key password |

```properties
# ~/.gradle/gradle.properties (never in the repository)
ANDROID_KEYSTORE_FILE=/home/me/keys/opendesk-release.jks
ANDROID_KEYSTORE_PASSWORD=...
ANDROID_KEY_ALIAS=opendesk
ANDROID_KEY_PASSWORD=...
```

Without `ANDROID_KEYSTORE_FILE` the release APK is built unsigned. In CI the same names come from repository
secrets ([docs/ci-cd.md](../docs/ci-cd.md)); `ANDROID_KEYSTORE_BASE64` holds the keystore itself.

## Layout

| Package | Layer | Contents |
| --- | --- | --- |
| `ui.main` | Presentation: shell | `MainActivity` (wiring and relay events), `NavBar`/`Page` (navigation), `EdgeToEdge` (insets), `ConnectFlow` (one connection attempt: target, state, timeout), `MainHost` (the contract its pages use) |
| `ui.devices`, `ui.sessions`, `ui.settings` | Presentation: pages | One package per tab: the Devices list, connect sheet, long-press menu (`DeviceMenu` look, `DeviceActions` Connect/Rename/Delete) and `DeviceDirectory` (which list to show); Sessions; Settings with Relay and Security |
| `ui.viewer` | Presentation: remote screen | `RemoteViewerActivity` (lifecycle and wiring), `FrameDecoder` + `FrameCompositor` (video thread), `ScreenRenderer` (frame, zoom, cursor), `TouchController` (gestures), `ControlsOverlay` (pill, radial menu, controls panel), `RadialMenuView` + `EdgeHandle` (radial menu), `RemoteInput` (input to the computer), `RemoteKeyboardView` |
| `ui.common` | Shared UI | `Ui` (view helpers), `Alerts` (dialogs) |
| `session` | Use cases | `RelayConnection` (message routing, reconnection), `E2eChannel`, `FileReceiver` |
| `data.dto` | Wire DTOs | Relay messages |
| `data.network`, `data.settings`, `data.history`, `data.devices`, `data.security` | Repositories | `GatewayClient` (WebSocket), `SettingsRepository` (preferences), `SessionHistory` (sessions this phone opened), `LocalDeviceStore` (names and deletions kept on this phone), `DeviceKeys` |
| `crypto`, `platform` | Infrastructure | Noise handshake; voice |
| `domain` | Models and rules (plain Kotlin, unit-tested) | `DeviceItem` and search, desk IDs, relay URL rules, capabilities, viewport math, key mapping |

Dependencies point down the table: pages talk to the activity only through `MainHost`, and nothing below `ui`
imports Android views.

## Documentation

[Architecture](docs/architecture.md) · features: [viewing a computer](docs/features/viewing.md),
[receiving files](docs/features/file-receive.md), [settings](docs/features/settings.md)
