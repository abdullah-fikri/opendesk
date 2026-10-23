# Settings

Code: `data/settings/SettingsRepository.kt`, `domain/GatewayUrl.kt`, `domain/DeskId.kt`.

| Setting | Stored as | Notes |
| --- | --- | --- |
| Relay address | `gateway_url` | Normalized like the desktop: `https://` becomes `wss://`, private addresses default to `ws://`, others to `wss://`. Empty until configured; the settings dialog opens automatically then. |
| Device token | `device_token` | Optional; when set, at least 32 characters ([provisioning](../../../server-relay/docs/provisioning.md)). |
| Desk ID | `hardware_desk_id` | Derived once from `ANDROID_ID`, then kept. |

The UI is in English with an Indonesian translation (`res/values-in`); Android picks it from the system language.
