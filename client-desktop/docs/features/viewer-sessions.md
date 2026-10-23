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
