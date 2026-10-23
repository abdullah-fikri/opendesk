# Android architecture

```text
ui.main.MainActivity ──implements──▶ session.RelayConnection.Listener
ui.viewer.RemoteViewerActivity ──▶ RelayConnection.sendInput / videoSink
                                       │
session.RelayConnection (process-wide) ─┼─▶ data.network.GatewayClient ──▶ relay
                                       ├─▶ session.FileReceiver
                                       ├─▶ session.RemoteInputHandler ──▶ platform.input.OpenDeskAccessibilityService
                                       └─▶ platform.audio.VoiceCallManager
platform.capture.ScreenCaptureService ──frameSink──▶ RelayConnection.sendBinary
```

- Activities only render and forward user actions. Message routing, session state and file handling live in
  `session`.
- `RelayConnection` is a singleton because the relay connection outlives any one activity (the viewer screen,
  the capture service and the main screen all use it).
- Platform services do not know about the relay: the capture service and the voice manager receive a sink
  function for their output.
- DTOs (`data.dto`) are plain data classes; Gson omits null fields when sending.

## Threads

| Thread | Work |
| --- | --- |
| Main | Activities, `RelayConnection.Listener` callbacks, text messages from the relay |
| OkHttp socket | Binary messages: voice goes to `VoiceCallManager`, video to the viewer's latest-frame slot |
| `OpenDesk-FrameDecoder` | Decodes the newest JPEG and posts at most one frame to the main thread |
| Capture thread | Mirrors the screen and sends JPEG frames |
| `OpenDesk-VoiceMic` | Microphone capture while unmuted |

## Protocol

Android advertises no capabilities: it receives plain JPEG video and the single-session message shapes. See
[docs/protocol.md](../../docs/protocol.md).
