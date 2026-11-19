# Settings

Code: `ui/settings/SettingsPage.kt`, `domain/Edition.kt`, `ui/settings/RelayPage.kt`, `ui/settings/SecurityPage.kt`,
`data/settings/SettingsRepository.kt`, `domain/GatewayUrl.kt`, `domain/DeskId.kt`.

The Settings tab lists **Connection** (Relay, Security), **This phone** (its ID with Copy, the microphone),
**Appearance** and **About** (the admin edition says so there). Relay and Security open as pages with a "‹ Settings" link back.

| Setting | Stored as | Notes |
| --- | --- | --- |
| Relay address | `gateway_url` | `https://` becomes `wss://`, `http://` becomes `ws://`; a bare address defaults to `wss://`, except `localhost`, `127.*` and the emulator's `10.0.2.2`, which default to `ws://`. Empty until configured; the app then opens on the Relay page. The page shows "Saved as wss://…". |
| Device token | `device_token` | Optional; when set, at least 32 characters ([provisioning](../../../server-relay/docs/provisioning.md)). A token typed on the Relay page goes to every relay; one the relay issued goes back to that relay only. |
| Monitor wall | `monitor_grid`, `monitor_rotate`, `monitor_rotate_secs`, `monitor_all`, `monitor_selected` | Admin edition only, set in the wall's own sheet ([monitor wall](monitor-wall.md)). Defaults: 2x2 on phones, 3x3 on tablets, rotation off (10 s), every online computer. |
| Desk ID | `hardware_desk_id` | Derived once from `ANDROID_ID`, then kept. Used by the relay to identify and authorize this phone. The admin edition derives a different one (both editions on one phone share `ANDROID_ID`). |
| Appearance | `appearance` | 0 = follow the phone (default), 1 = light, 2 = dark; applied at start by `OpenDeskApp` (`AppCompatDelegate`). |
| Gesture guide seen | `gestures_seen` | The touch-gesture guide is shown once, in the first remote session. |

The Relay page checks as you type: a plain `ws://` address to another host and a token shorter than 32
characters are marked in red and Save & Connect stays disabled. Release builds refuse plaintext `ws://` to anything
but this device (`res/xml/network_security_config.xml`); use `wss://` through a TLS terminator, as for every remote
relay. Debug builds allow `ws://` anywhere for testing on a LAN. While the relay cannot be reached the page shows
when the next automatic retry runs.

The Security page shows end-to-end encryption (always on), this phone's key fingerprint, the computers whose key
is pinned (tap one to forget its key), and the device token.

Settings, the pinned keys and the session history (`opendesk_history`) are excluded from cloud backup and
device-to-device transfer (`res/xml/data_extraction_rules.xml`) because the token is a credential.

## Appearance

By default the app follows the phone's light or dark setting (Settings → Appearance can force Light or Dark): `Theme.OpenDesk` is DayNight and every colour is a token in
`res/values/colors.xml` (light) and `res/values-night/colors.xml` (dark), the redesign's palette. The main screen
is recreated when the setting changes; so is the remote screen, which keeps its session and last frame.

Phones get a bottom bar (Devices, Sessions, Settings); tablets (`sw600dp`) a navigation rail and, on Devices, the
list beside the selected computer (`layout-sw600dp`). Text is Inter, IDs JetBrains Mono (`res/font`, the same
files as the desktop). The logo is the user's own: the launcher icon is its light-background variant (`client-desktop/ui/logo-light.png`) on white (with a
monochrome layer for themed icons) and the in-app logo uses `client-desktop/ui/logo-light.png` or `logo-dark.png`
for the current theme.

The UI is in English with an Indonesian translation (`res/values-in`); Android picks it from the system language.

## Edition

There is no administrator switch: the edition is chosen at build time (Gradle flavor `standard` or `admin`, see
the [README](../../README.md#editions)). The admin edition registers with `isAdmin` and asks for the relay's device
registry (`get-hosts` / `host-list`), so Devices lists every computer with its online / offline state; when the
relay does not treat the phone as an administrator (device security decides roles), Devices falls back to the
computers this phone connected to. Installs that had the old `admin_mode` switch lose it on first start.