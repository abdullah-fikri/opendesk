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

## Performance

One preset in the relay settings dialog, like a game's graphics quality option, for every session this machine
views (`Settings::performance`, `domain/performance.rs`):

| Preset | Frame rate | H.264 bitrate | Video |
| --- | --- | --- | --- |
| Low | 30 | ×0.5 | Auto (sharp JPEG regions, H.264 for sustained motion) |
| Medium (default) | 60 | ×1 | Auto |
| High | This machine's fastest monitor refresh rate (up to 240) | ×2 | Auto, but motion switches to H.264 at once (hosts with a hardware encoder) |

The congestion governor lowers frame rate and bitrate on every preset, and hosts without a hardware H.264 encoder
stay at 60. Resolution and voice quality stay per session in the toolbar. The viewer lists the preset at
registration (`perf-low`, `perf-high`; Medium lists nothing) after its `fps-N` cap, and sends `set-performance`
at session start and when the preset changes, so a change applies to running sessions at once. Hosts before
presets only read the registration's `fps-N`: with no session open, saving a new preset re-registers; with
sessions open they keep the old cap until the next connection. Saving the dialog with an unchanged relay
address no longer reconnects.

Configs from before presets keep their frame-rate choice: 30 becomes Low, 60 Medium, Auto and Maximum High. The
config keeps `max_fps` in step (30, 60, 0) so an older build reads the same frame rate.

## Toolbar actions

Monitor picker (numbered when the host has several monitors), send files, microphone, privacy mode on the host
(normal, frozen, maintenance screen, black screen), block the host's physical input, system shortcuts
(Win, Alt+Tab, Task Manager, Explorer, Show desktop), resolution and voice quality.

## Stats overlay

The **Stats** toolbar button shows a translucent panel in the top-right corner of every session view; it is off
at start and stays as chosen until the app quits. Once a second it shows the `stats-ping` round trip, the path
(P2P or Relay), received kbps, shown FPS against the host's target, viewer decode time (avg / max), the host's
encoder (HW or SW H.264, or JPEG), host grab and encode time (avg / max), idle and dropped frames, and the
congestion level. Hosts send `host-stats` only while the overlay is shown; hosts that predate it show "–" for
their rows and the RTT. Over P2P the ping goes direct and the answer through the relay (host events always take
the relay), so the RTT is that of control messages.
