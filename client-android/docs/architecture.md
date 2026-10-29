# Android architecture

The Android app is a viewer only: it controls computers and can never be controlled.

```text
ui.main.MainActivity ──implements──▶ session.RelayConnection.Listener
ui.viewer.RemoteViewerActivity ──implements──▶ RelayConnection.ViewerListener
        │  sendInput / videoSink
        ▼
session.RelayConnection (process-wide) ──▶ data.network.GatewayClient ──▶ relay
        ├─▶ session.E2eChannel ──▶ crypto.NoiseXX      (end-to-end encryption)
        ├─▶ data.security.DeviceKeys                     (device key, pinned host keys)
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
  pan, touch to remote coordinates), Android-to-Win32 key mapping, the framed video header and key fingerprints.
- `crypto.NoiseXX` and `session.E2eChannel` are unit-tested against vectors produced by the desktop client, so
  both sides seal and open the same bytes.
- DTOs (`data.dto`) are plain data classes; Gson omits null fields when sending. R8 keeps them unrenamed
  (`app/proguard-rules.pro`).

## Threads

| Thread | Work |
| --- | --- |
| Main | Activities, listener callbacks, text messages from the relay, reconnection timer |
| OkHttp socket | Binary messages: opened by the end-to-end channel, then voice goes to `VoiceCallManager` and video to the viewer's queue |
| `OpenDesk-FrameDecoder` | Applies video in order (`FrameCompositor`: key frames, deltas, legacy JPEGs) from a queue of at most 8 messages, requests a key frame when the chain breaks or it falls behind, and hands at most one bitmap to the main thread (always the newest) |
| `OpenDesk-VoiceMic` | Microphone capture while unmuted |

## Reconnection

A dropped connection ends the session (the relay does the same) and is retried after 1 s, doubling up to 30 s.
The app retries at once when the default network becomes available or when the main screen comes back to the
foreground. A `connect-request` is only sent while the relay is connected.

## Protocol

Android registers with `platform: "android"` and `caps: ["viewer-only", "video-delta", "e2e"]`. It receives
framed JPEG key and delta video (no H.264) and the single-session message shapes. It declines any
`incoming-request` and ends any session in which the relay would make it the host. See
[docs/protocol.md](../../docs/protocol.md).

## End-to-end encryption

The same protocol as the desktop ([client-desktop/docs/features/e2e.md](../../client-desktop/docs/features/e2e.md)):
`Noise_XX_25519_ChaChaPoly_BLAKE2s` with device keys over `p2p-signal`, then sealed video, voice and input events.
The primitives come from BouncyCastle; the handshake is `crypto/NoiseXX.kt`.

- **Device key.** An X25519 key created on first use, wrapped with an AES-GCM key held by the Android Keystore,
  stored in app-private preferences that are excluded from backups. Its fingerprint is shown in the relay
  settings dialog.
- **Pinning.** The first key a host presents is pinned. A changed key, or a pinned host that no longer offers
  `e2e` (or a relay that stops forwarding `p2p-signal`), closes the session with a dialog; **Trust the new key**
  forgets the pin, like `opendesk trust <id>` on a desktop.
- **No input in clear.** While the handshake runs, touches, keys and voice are held back (only `video-keyframe`
  goes out). A handshake that does not complete within 10 s closes the session.
- **What stays visible to the relay:** files to the phone (they arrive as JSON, which is not sealed), clipboard
  and session metadata, and plain JPEGs from hosts too old for `video-delta`. Hosts without `e2e` keep working,
  unencrypted end to end, until the phone has pinned their key.
