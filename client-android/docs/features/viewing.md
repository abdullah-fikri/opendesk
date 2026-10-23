# Viewing a PC

Code: `ui/viewer/RemoteViewerActivity.kt`, `session/RelayConnection.kt`.

- Enter the PC's desk ID and tap Connect. Windows hosts accept at once; the viewer screen opens on
  `session-connected`.
- **Trackpad mode** (default): dragging moves a virtual cursor (1.35x sensitivity); tap clicks at the cursor.
  **Touch mode**: the cursor jumps to the finger.
- Tap = left click, double tap = double click, long press or two-finger tap = right click, two-finger vertical
  drag = scroll. Buttons: left, double, right, hold (left button down until tapped again), scroll up and down,
  next monitor, microphone, disconnect.
- Mouse moves are throttled to one every 15 ms. Frames are decoded on a worker thread, newest frame only; the
  badge shows resolution, measured frame rate and mode.
- Back or Disconnect ends the session (releasing a held button first).
