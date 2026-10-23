# Being controlled

Code: `ui/main/MainActivity.kt`, `platform/capture/ScreenCaptureService.kt`,
`platform/input/OpenDeskAccessibilityService.kt`, `session/RemoteInputHandler.kt`.

1. A PC sends `connect-request`; the relay forwards `incoming-request` to the phone.
2. The phone asks for screen-capture consent (Android's own dialog). Declining rejects the session.
3. On consent, a foreground service mirrors the screen at 1080 px width and streams JPEG frames (quality 80, up
   to 60 FPS) while the relay activates the session.
4. Input from the PC goes through the accessibility service: clicks become taps at the same relative position,
   Escape is Back and the Windows key is Home. Other events have no phone equivalent.
5. "End Session" or the PC disconnecting stops the capture service.

The accessibility service must be enabled once in system settings; the app prompts on first run and shows the
state of both permissions. Voice works in both directions; the PC may mute the phone's microphone but not
unmute it.
