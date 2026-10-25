# Settings

Code: `data/settings/SettingsRepository.kt`, `domain/GatewayUrl.kt`, `domain/DeskId.kt`.

| Setting | Stored as | Notes |
| --- | --- | --- |
| Relay address | `gateway_url` | `https://` becomes `wss://`, `http://` becomes `ws://`; a bare address defaults to `wss://`, except `localhost`, `127.*` and the emulator's `10.0.2.2`, which default to `ws://`. Empty until configured; the settings dialog opens automatically then. |
| Device token | `device_token` | Optional; when set, at least 32 characters ([provisioning](../../../server-relay/docs/provisioning.md)). |
| Desk ID | `hardware_desk_id` | Derived once from `ANDROID_ID`, then kept. Used by the relay to identify and authorize this phone. |

Release builds refuse plaintext `ws://` to anything but this device (`res/xml/network_security_config.xml`), and
the settings dialog says so instead of failing silently; use `wss://` through a TLS terminator, as for every
remote relay. Debug builds allow `ws://` anywhere for testing on a LAN.

Settings are excluded from cloud backup and device-to-device transfer (`res/xml/data_extraction_rules.xml`)
because the token is a credential.

The UI is in English with an Indonesian translation (`res/values-in`); Android picks it from the system language.
