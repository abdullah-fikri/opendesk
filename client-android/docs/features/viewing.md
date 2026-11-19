# Viewing a computer

Code: `ui/devices/` (`DevicesPage`, `ConnectSheet`, `DeviceMenu`, `DeviceDirectory`), `data/devices/LocalDeviceStore.kt`, `ui/sessions/SessionsPage.kt`, `ui/viewer/RemoteViewerActivity.kt`,
`ui/viewer/RemoteKeyboardView.kt`, `domain/Viewport.kt`, `domain/WindowsKeys.kt`, `session/RelayConnection.kt`.

## Connecting

- **Devices** lists the computers (see administrator mode in [settings](settings.md)), with search by name or ID.
  The brand bar shows whether the relay is connected. In administrator mode the list waits up to 4 s for the
  relay's registry (skeleton rows), then falls back to recent computers.
- Tap a computer (phone) to open the connect sheet: its ID and platform, presence, and Connect. The sheet then
  shows Connecting… (the end-to-end handshake may take up to 10 s), and on failure the reason with Try again. A
  request the relay never answers fails after 20 s. On tablets the same happens in the detail pane beside the list,
  which also links to the computer's recent sessions and its pinned key.
- Press and hold a computer for its menu (iOS-style: the list blurs, the row lifts): **Connect**, **Rename**
  (administrator list: the relay's name, seen by every administrator, via `host-rename`; otherwise a name kept on
  this phone) and **Delete** (removes it and its sessions from this phone; connecting again brings it back).
- Or type a desk ID in the bar at the bottom and tap Connect (or the keyboard's Go key).
- Desktop hosts accept at once; the viewer opens on `session-connected`, even if the app was in the background.
- **Sessions** lists every attempt grouped by day (Today, Yesterday, Earlier this week, Older) with its duration,
  or Failed. Tap one to connect again.
- A changed host key (or a host that stopped offering end-to-end encryption) closes the session and asks whether to
  trust the new key.

## The remote screen

- A pill at the top shows the computer, the measured frame rate, **E2E** while the session is end-to-end
  encrypted, and the red button that ends the session. Tap the computer's name for the touch-gesture guide (also
  shown once in the first session).
- **Controls** at the bottom opens the controls: a bottom sheet in portrait, a panel on the right in landscape.
  Trackpad / Touch; Left, Double, Right, Hold (left button down until tapped again); Esc, Tab, Win; Keyboard;
  Scroll (one-finger drags turn the wheel); Fit; microphone; Screen (next monitor). Tools that are on (Hold,
  Scroll, microphone) are tinted red. Back or the handle closes the controls.
- **Trackpad mode** (default): dragging moves a virtual cursor (1.35x the finger's on-screen movement); tap
  clicks at the cursor. **Touch mode**: the cursor jumps to the finger.
- Tap = left click, double tap = double click, long press or two-finger tap = right click.
- Two fingers: the first clear movement decides. Fingers moving apart or together zoom (1x to 5x, around the
  fingers' midpoint; moving them pans); fingers moving together vertically scroll (120 per step). In trackpad mode
  a zoomed view follows the cursor. **Fit** returns to the whole screen.
- **Keyboard** opens the soft keyboard with a bar of Esc, Tab, Win and Hide above it. Characters are sent as
  `text` (independent of the computer's keyboard layout); Backspace, Delete, Enter, Tab, Escape, arrows, Home/End
  and Page Up/Down are sent as `keydown`/`keyup` with Win32 virtual-key codes. Word suggestions are disabled; if a
  keyboard still composes, the computer sees the same corrections. The picture shrinks above the keyboard.
- Mouse moves are throttled to one every 15 ms. Frames are decoded on a worker thread, newest frame only.
- Rotation keeps everything. Leaving the app stops decoding, releases a held button and mutes the microphone;
  the session continues. Back or the end button ends the session (releasing a held button first). If the relay
  connection drops, the session ends and the viewer closes with a notice.
