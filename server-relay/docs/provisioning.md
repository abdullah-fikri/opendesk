# Device credentials

## Open relay (default)

With `OPENDESK_DEVICE_CREDENTIALS` unset, the relay admits every device and trusts the admin flag the client
claims. Anyone who can reach the relay and knows a desk ID can register as that ID, and Windows hosts accept
sessions without a prompt. Keep the relay address private, or provision credentials.

## Provisioned relay

Set a JSON map from clean desk ID to token and admin flag:

```bash
OPENDESK_DEVICE_CREDENTIALS='{"123456789":{"token":"<random, at least 32 characters>","admin":true},
                              "987654321":{"token":"<another random secret>","admin":false}}'
```

Generate tokens with, for example, `openssl rand -hex 32`. Once the variable is set:

- Only listed desk IDs may register, and only with their exact token (compared in constant time).
- Tokens shorter than 32 characters never match.
- Only the relay's `admin` flag grants the host list; the client's claim is ignored.
- Rejected registrations are logged with the desk ID and address, which is the usual cause of "it connects but
  nothing happens".
- Malformed JSON stops the relay at startup rather than opening it to everyone.

Give each device its token:

| Client | Where |
| --- | --- |
| Windows | `device_token` in `%APPDATA%\OpenDesk\config.json`, or the `OPENDESK_DEVICE_TOKEN` environment variable |
| Android | Relay settings dialog, "Device token" |

A device's desk ID is shown in the app and by `opendesk status` on Windows.
