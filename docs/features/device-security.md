# Device security (client token)

Administrators decide which devices may use the relay, from the admin edition: **Settings › Security › Client
token**. Devices need no configuration besides the relay address: a listed device receives its own token the
first time it connects and presents it from then on.

## Using it

1. Open **Settings** in the header of the admin edition, then **Security › Client token**.
2. Add devices: type a desk ID (tick **Administrator** for another admin PC), or press **Add all online devices**.
3. Turn on **Require a client token**. The admin PC that turns it on is listed as an administrator automatically,
   so it cannot lock itself out. Connected devices that are not listed are disconnected at once; connected listed
   devices receive their token at once.

The switch is **off by default**. While it is off the relay is open as before, and the list can be prepared.

| Action | Effect |
| --- | --- |
| Add | The device may connect. It enrolls (receives its token) on its next connection, or at once if it is online. |
| Reset token | The device's next connection receives a new token. Use it after reinstalling a device (it lost its token). |
| Remove | The device can no longer connect; if it is online it is disconnected. |
| Turn off | Every device may connect again; stored tokens are kept and are used again when it is turned back on. |

Each row shows whether the device already holds its token ("Token issued …") or is still waiting for its first
connection.

## How it works

- The relay keeps the list in PostgreSQL (`allowed_devices`, `relay_settings`; migration `0003`). Tokens are
  32 random bytes (64 hex characters); only their SHA-256 is stored.
- Registration while the allowlist is on:
  - unlisted desk ID: refused (`Invalid device credentials.`);
  - listed, no token yet: admitted, and the relay sends `device-credential` with a new token;
  - listed with a token: admitted only with that token (constant-time comparison).
- Clients store the token with the relay URL that issued it (`device_token` and `device_token_relay` in
  `config.json`; Android preferences) and send it to that relay only.
- Only the stored administrator flag grants administrator rights while the allowlist is on; the client's claim
  is ignored.
- `OPENDESK_DEVICE_CREDENTIALS` keeps working (see `server-relay/docs/provisioning.md`): devices listed there
  always need their configured token, and setting it implies that every other device must be on the allowlist.

## Trust on first use

The first connection with a listed desk ID receives that ID's token. Someone who knows a listed ID and connects
before the real device would take its place; the real device is then refused and its row shows "Token issued" at
a time it did not connect. Add devices shortly before they connect, check the list after rollout, and use
**Reset token** (then reconnect the real device) if in doubt.

## Device names

Administrators can name devices in the **Relay Hosts** list (**Name**). The name is stored on the relay
(`hosts_registry.alias`), shown to every administrator, used on the monitor wall and on the client-token page,
and kept when the device reconnects. An empty name restores the device's own name. Names in **Session History**
stay local to one PC.

## Protocol

See [protocol](../protocol.md#device-security). Relays advertise `device-security` and `host-alias` in
`registered-ack`; the admin edition shows the pages only when they are present.
