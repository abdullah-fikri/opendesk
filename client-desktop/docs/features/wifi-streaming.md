# Wi-Fi streaming mode

Code: `src/infra/wifi/` (`mod.rs` counts sessions, `win.rs` talks to the WLAN service).

Windows Wi-Fi drivers scan for other networks in the background every few seconds. Each scan takes the radio off
the current channel for a moment, which shows up as a latency spike of around 100 ms in a remote session. While
at least one session runs, OpenDesk asks Windows for *media streaming mode* on every connected Wi-Fi adapter, the
same setting games and video streaming apps use.

- **When.** From the first session to the end of the last one, on both sides: every viewer tab (including one
  still connecting) and the hosted session's capture loop each count once. Several tabs or a host restarting its
  capture keep it on; it is released when the count drops to zero. The monitor wall does not count (thumbnails
  are not latency-sensitive, and a wall left open all day would keep the setting on).
- **What.** `WlanSetInterface(wlan_intf_opcode_media_streaming_mode, TRUE)` on each connected wireless interface.
  OpenDesk also tries to turn background scans off; Windows may refuse that to a standard user, in which case
  only media streaming mode applies. Releasing turns background scans on again where OpenDesk turned them off and
  closes the WLAN handle, which hands media streaming mode back. Both also end if the process exits.
- **Where it does nothing.** Machines without Wi-Fi or without the WLAN service (`wlanapi.dll` is loaded at run
  time, so Windows Server without the Wireless LAN feature still starts), Wi-Fi not connected when the first
  session starts, and other platforms.
- **Threads.** The WLAN calls run on one `wifi-streaming` worker thread, started with the first session, so a
  slow WLAN service never delays a session or the UI.
- **Log.** One `wifi` line with the outcome (interfaces changed, or why not), repeated only when the outcome
  changes.
- **Off switch.** `OPENDESK_WIFI_STREAMING=0` ([configuration](../configuration.md)).
