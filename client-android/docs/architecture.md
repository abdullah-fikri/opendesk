# Android architecture

The Android app is a viewer only: it controls computers and can never be controlled.

```text
ui.main.MainActivity ──implements──▶ session.RelayConnection.Listener
ui.viewer.RemoteViewerActivity ──implements──▶ RelayConnection.ViewerListener
        │  sendInput / videoSink
        ▼
session.RelayConnection (process-wide) ──▶ data.network.GatewayClient ──▶ relay
        ├─▶ session.FileReceiver
        └─▶ platform.audio.VoiceCallManager
```

- Activities only render and forward user actions. Message routing, session state, reconnection and file
  handling live in `session`.
- `RelayConnection` is a singleton because the relay connection outlives any one activity: the main screen
  being recreated (configuration change) keeps it; only leaving the app (`isFinishing`) stops it.
- The viewer handles rotation and keyboard changes itself (`configChanges`), so the stream, zoom and last frame
  survive them. A viewer restored after process death finds no session and closes.
- `domain` is plain Kotlin and unit-tested: desk IDs, relay URL rules, capabilities, the viewport (fit, zoom,
  pan, touch to remote coordinates) and Android-to-Win32 key mapping.
- DTOs (`data.dto`) are plain data classes; Gson omits null fields when sending. R8 keeps them unrenamed
  (`app/proguard-rules.pro`).

## Threads

| Thread | Work |
| --- | --- |
| Main | Activities, listener callbacks, text messages from the relay, reconnection timer |
| OkHttp socket | Binary messages: voice goes to `VoiceCallManager`, video to the viewer's latest-frame slot |
| `OpenDesk-FrameDecoder` | Sleeps until a frame arrives, decodes the newest JPEG, hands at most one bitmap to the main thread (always the newest) |
| `OpenDesk-VoiceMic` | Microphone capture while unmuted |

## Reconnection

A dropped connection ends the session (the relay does the same) and is retried after 1 s, doubling up to 30 s.
The app retries at once when the default network becomes available or when the main screen comes back to the
foreground. A `connect-request` is only sent while the relay is connected.

## Protocol

Android registers with `platform: "android"` and `caps: ["viewer-only"]`. It advertises no video or
multi-session capability, so it receives plain JPEG video and the single-session message shapes. It declines any
`incoming-request` and ends any session in which the relay would make it the host. See
[docs/protocol.md](../../docs/protocol.md).
