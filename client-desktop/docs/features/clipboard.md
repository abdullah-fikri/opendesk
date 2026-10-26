# Clipboard

Code: `src/service/clipboard_sync.rs` (watcher), `src/service/relay/dispatcher.rs` (`clipboard-sync`),
`src/service/viewer/controller.rs` (focus changes, Ctrl+V).

- The watcher polls the clipboard every 350 ms and sends changes to the current partner: the hosted viewer, or
  the focused viewer session.
- Text goes as `relay-clipboard`. Files are offered to partners with `file-offer`; a host without offers sends
  copied files to the viewer's clipboard ([file transfer](file-transfer.md)).
- Received text replaces the local clipboard. Copies on an unfocused host are ignored.
- On focus change the newly focused host gets the viewer's clipboard if it has not seen it yet.
- The service's SYSTEM agent sees clipboard changes but not their content, so it reads the clipboard through
  `OpenDesk.exe --clipboard-read <file>` started as the logged-on user (`src/infra/user_clipboard.rs`).
  Writing works as SYSTEM.
- Anything the app wrote to the clipboard itself (received text or files) is marked as already synced, so it is
  never echoed back.
