# Viewing a computer

Code: `ui/viewer/RemoteViewerActivity.kt`, `ui/viewer/RemoteKeyboardView.kt`, `domain/Viewport.kt`,
`domain/WindowsKeys.kt`, `session/RelayConnection.kt`.

- Enter the computer's desk ID and tap Connect (or the keyboard's Go key). Desktop hosts accept at once; the
  viewer screen opens on `session-connected`, even if the app was in the background meanwhile.
- **Trackpad mode** (default): dragging moves a virtual cursor (1.35x the finger's on-screen movement); tap
  clicks at the cursor. **Touch mode**: the cursor jumps to the finger.
- Tap = left click, double tap = double click, long press or two-finger tap = right click.
- Two fingers: the first clear movement decides. Fingers moving apart or together zoom (1x to 5x, around the
  fingers' midpoint; moving them pans); fingers moving together vertically scroll (120 per step). In trackpad mode
  a zoomed view follows the cursor. **Fit** returns to the whole screen.
- **Keyboard** opens the soft keyboard. Characters are sent as `text` (independent of the computer's keyboard
  layout); Backspace, Delete, Enter, Tab, Escape, arrows, Home/End and Page Up/Down are sent as `keydown`/`keyup`
  with Win32 virtual-key codes. Word suggestions are disabled; if a keyboard still composes, the computer sees the
  same corrections. The picture shrinks above the keyboard.
- **Esc**, **Tab** and **Win** buttons; next monitor; microphone; hold (left button down until tapped again);
  scroll up/down; disconnect.
- Mouse moves are throttled to one every 15 ms. Frames are decoded on a worker thread, newest frame only; the
  badge shows resolution, measured frame rate and mode.
- Rotation keeps everything. Leaving the app stops decoding, releases a held button and mutes the microphone;
  the session continues. Back or Disconnect ends the session (releasing a held button first). If the relay
  connection drops, the session ends and the viewer closes with a notice.
