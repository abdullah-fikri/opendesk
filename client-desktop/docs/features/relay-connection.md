# Relay connection

Code: `src/service/relay/connection.rs` (transport loop), `src/service/relay/dispatcher.rs` (message handling).

## Lifecycle

1. Resolve the URL. If the configured relay is a remote `wss://` address but a relay listens on
   `127.0.0.1:8088`, the client uses loopback (the relay machine would otherwise hairpin through its own tunnel).
2. Connect and send `register` with the edition's platform tag, device name, token and capabilities.
3. Switch the socket to non-blocking with `TCP_NODELAY`, then loop every millisecond:
   - drain the outbox (control messages and voice at once, mouse moves coalesced per host, only the newest
     video frame);
   - send up to 8 queued file messages (64 when no screen session runs);
   - flush, which re-opens the video backpressure gate once the previous frame has left the socket;
   - read every available message and dispatch it.
4. On a read error: fail every transfer on the connection, end every session, show the reason, wait 1.5 s and
   reconnect. A failed connect retries after 2 s. Changing the relay (UI or CLI) or the tray's "Reconnect"
   reconnects immediately.

## Message handling

| Message | Effect |
| --- | --- |
| `registered-ack` | Store relay capabilities, enable multi-session when both sides support it, show "Online". |
| `host-list` | Admin list (this device excluded); also the monitor wall's device list. |
| `monitor-start`, `monitor-stop` | Start or stop the monitor-wall thumbnail stream (`service/host/monitor_stream.rs`). |
| `monitor-state` | Admin edition: one wall tile's state (`service/monitor_wall.rs`). |
| `session-connected` as host | Start hosting (`service::host::start`), optionally a P2P link. |
| `session-connected` as viewer | Connect the viewer tab (`viewer::controller::on_connected`), optionally a P2P link. |
| `input-command` | From a viewed host: cursor, monitors, mic, voice quality. Otherwise: the hosted viewer's input. |
| `clipboard-sync` | Set the local clipboard (ignored from unfocused tabs). |
| `file-transfer` | ACKs, offers, pulls, or bytes for the receiver. |
| `p2p-signal` | Handed to the P2P link of that partner. |
| `session-closed` | Close that tab, or end everything for the host role and single-session relays. |
| `error` | Close the failed pending tab and show the message. |

Every message is decoded into `protocol::ServerMessage` first; unknown types are ignored.
