# Monitor wall (admin edition)

Code: `ui/monitor/` (`MonitorWallActivity`, `MonitorGrid`, `ThumbnailDecoder`, `WallSettingsSheet`),
`domain/MonitorWall.kt` (paging, rotation, selection, thumbnail framing; tests in `MonitorWallTest`),
`session/RelayConnection.kt` (`watchMonitors`, `monitor-state`, `0x11` routing), `data/settings/SettingsRepository.kt`.

The admin edition's Devices page has a **Monitor wall** button. The wall shows live thumbnails of many computers
at once, like the desktop admin's wall, using the same relay protocol
([docs/features/monitor-wall.md](../../../docs/features/monitor-wall.md)). The phone only **watches**: it advertises
`monitor-view`, never `monitor-host`, so a phone can never be put on a wall.

## Use

- **Toolbar**: Devices (back), the page (‹ 1 / 3 ›), Wall settings, Full screen. Full screen hides the toolbar
  and system bars; the faint corner button or Back leaves it.
- **Swipe** left or right to turn the page. **Double-tap** a live tile to close the wall and connect to that
  computer as usual (the normal session rules then apply). There is no other input: the wall is view only, and the
  relay enforces that.
- **Wall settings** (bottom sheet, applied at once): layout (1x1, 2x2, 3x3 on phones; 2x2, 3x3, 4x4 on tablets),
  turning pages automatically every 10, 30 or 60 s, and either every online computer or a hand-picked list (offline
  ones then show as Offline).
- Tiles show the name administrators gave the computer on the relay, a status dot, and while there is no picture:
  Connecting…, Waiting for the picture…, Offline, Cannot be monitored (Android or older desktop clients) or Not
  allowed.
- The layout adapts to the screen: the grid picks the column count that gives the largest 16:10 tiles, so a 2x2
  page is one column of four on a phone held upright and a 2x2 or 4x1 row in landscape. Rotation keeps the wall
  running (the activity handles configuration changes itself).

## Data and battery

- Only the visible page is watched (`monitor-set` with at most 16 IDs). Turning the page sends one new list; the
  relay stops the hosts nobody else watches.
- Leaving the screen (Home, screen off, another app) sends an empty list, so nothing streams in the background;
  coming back watches the page again. After a relay reconnect the list is sent again, since a watch belongs to the
  connection.
- On mobile data a banner says a full page can use up to about 1 MB/s while the screens change (about 40-65 KB/s
  per tile at the relay's 640 px and 2 fps, close to nothing while a screen is still).
- Thumbnails are decoded on one worker thread, newest frame per computer only, and the tiles update at most 10
  times a second. 16 tiles of 640x400 in RGB_565 take about 8 MB.
- The screen stays on while the wall is open.

## Messages

| Direction | Message | Use |
| --- | --- | --- |
| phone → relay | `register.caps` + `monitor-view` | Admin edition only. |
| phone → relay | `monitor-set {targets[]}` | The visible page; empty stops. Sent only when the relay lists `monitor`. |
| relay → phone | `monitor-state {targetClean, state}` | `live`, `offline`, `unsupported`, `denied`. |
| relay → phone | binary `[0x11][9 ASCII digits][JPEG]` | One thumbnail. Checked before end-to-end decryption (sealed packets start with `FF` or `02`); a payload that is not a plain JPEG or is over 512 KiB is dropped. |

A relay without the `monitor` capability shows "This relay does not run the monitor wall"; `denied` tiles mean the
relay does not treat this phone as an administrator (device credentials without the `admin` flag).
