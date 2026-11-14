//! The queue item every producer (UI, services, capture, audio) hands to the relay connection.

use super::ClientMessage;

/// How far a sent transfer is once the file message carrying this mark has reached the relay: `key` names the
/// transfer (`target:id`), `done` counts its file bytes up to and including that message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMark {
    pub key: String,
    pub done: u64,
}

/// One item for the connection's outbound queue. The connection decides the path (P2P or relay),
/// coalesces mouse moves and paces file chunks behind video and voice.
#[derive(Debug)]
pub enum Outgoing {
    /// A control or input message, sent as soon as possible.
    Message(ClientMessage),
    /// A file-transfer message, rate-limited; `status` is shown when it leaves the queue, `mark` reported once
    /// the relay has read it.
    File { message: ClientMessage, status: Option<String>, mark: Option<FileMark> },
    /// A binary file chunk (`binary::file_chunk`) for `target`, paced like `File`.
    FileChunk { target: String, frame: Vec<u8>, status: Option<String>, mark: Option<FileMark> },
    /// Mouse position for a host, coalesced per host: (target clean ID, x, y), normalized.
    MouseMove(String, f64, f64),
    /// Encoded video for the hosted viewer; only the newest frame per tick is sent.
    VideoFrame(Vec<u8>),
    /// A monitor-wall thumbnail, already tagged (`binary::monitor_frame`); always through the relay, which fans
    /// it out. Only the newest is kept while the previous one is still in the socket.
    MonitorFrame(Vec<u8>),
    /// Voice packet for the current partner.
    Audio(Vec<u8>),
}
