//! The queue item every producer (UI, services, capture, audio) hands to the relay connection.

use super::ClientMessage;

/// One item for the connection's outbound queue. The connection decides the path (P2P or relay),
/// coalesces mouse moves and paces file chunks behind video and voice.
#[derive(Debug)]
pub enum Outgoing {
    /// A control or input message, sent as soon as possible.
    Message(ClientMessage),
    /// A file-transfer message, rate-limited; `status` is shown when it leaves the queue.
    File { message: ClientMessage, status: Option<String> },
    /// Mouse position for a host, coalesced per host: (target clean ID, x, y), normalized.
    MouseMove(String, f64, f64),
    /// Encoded video for the hosted viewer; only the newest frame per tick is sent.
    VideoFrame(Vec<u8>),
    /// Voice packet for the current partner.
    Audio(Vec<u8>),
}
