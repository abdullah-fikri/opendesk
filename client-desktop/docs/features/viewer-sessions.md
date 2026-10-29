# Viewer sessions

Code: `src/service/viewer/` (`registry.rs`, `controller.rs`, `decoder.rs`); UI in `ui/views/workspace/`.
Protocol: [multi-session](../../../docs/features/multi-session.md).

## Opening and closing

- Connecting to a device opens a tab immediately, marked "Connecting...", and sends `connect-request`. Connecting
  to a device that already has a tab focuses it.
- At most four tabs (one against a relay without `multi-session`; there a pending connect is replaced by a new
  one, a running session never is).
- A tab closes when the user closes it, the host ends the session or goes offline, or the connect fails.
- Losing the relay connection closes every tab.

## Focus

The focused tab receives keyboard input, voice, the microphone and the clipboard. In split view the first click
on another tile only focuses it. When focus moves, keys still held on the previous host are released and the
newly focused host receives the clipboard if it changed since that host last saw it.

## Video

Each connected tab has a decoder thread. Hidden tabs (background tabs in tab layout) are not decoded; when one
becomes visible again it requests a key frame. A broken delta chain also requests one, at most every 300 ms.
The status line shows resolution, frame rate and path (P2P or relay) for the focused tab.

## Resolution

| Mode | Meaning |
| --- | --- |
| Auto (default) | The host streams at the size of the tab or tile. Resizes are reported 300 ms after they stop and only when the size changes by 32 px or 4 %. |
| Native | The host's full resolution. |
| 1080p, 720p, 480p | Fit into that box on any aspect ratio. |

The last chosen mode applies to tabs opened later.

## Toolbar actions

Monitor picker (numbered when the host has several monitors), send files, microphone, privacy mode on the host
(normal, frozen, maintenance screen, black screen), block the host's physical input, system shortcuts
(Win, Alt+Tab, Task Manager, Explorer, Show desktop), resolution and voice quality.

## Stats overlay

The **Stats** toolbar button shows a translucent panel in the top-right corner of every session view; it is off
at start and stays as chosen until the app quits. Once a second it shows the `stats-ping` round trip, the path
(P2P or Relay), received kbps, shown FPS against the host's target, the host's encoder (HW or SW H.264, or JPEG),
where a frame's latency goes (avg / max), host grab and encode time (avg / max), idle and dropped frames, the
congestion level, and CPU and RAM of OpenDesk on both machines. Hosts send `host-stats` only while the overlay is
shown; hosts that predate it show "–" for their rows and the RTT. Over P2P the ping goes direct and the answer
through the relay (host events always take the relay), so the RTT is that of control messages.

The latency rows follow one frame from capture to screen; together they make up Total:

| Row | From → to |
| --- | --- |
| Host | capture on the host → handed to its send queue (grab work, encode, waiting for the previous frame) |
| Network | send queue → complete in the viewer (relay or P2P, including all fragments) |
| Decode | received → decoded (including a wait while the previous frame decodes) |
| Display | decoded → handed to the UI; the screen's next refresh adds up to one more frame |
| Total | capture → handed to the UI |

Host, Network and Total need hosts that list `frame-stamp` (others show "–"). Network and Total also map the host's
clock onto the viewer's from the ping, taking the answer as sent halfway through the round trip: accurate to a few
ms on the relay path, while over P2P the ping and answer take different routes, which can shift up to half that
difference between Network and the true value. Host grab is the capture work only; waiting for the screen to change
is not counted (the `host.perf` log shows it as `wait`). CPU is a share of the whole machine, like Task Manager.
