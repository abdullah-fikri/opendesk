use serde_json::Value;

pub enum OutgoingMsg {
    Json(Value),
    FileTransfer {
        msg: Value,
        status_text: Option<String>,
    },
    /// Coalesced per target host: (target clean ID, normX, normY).
    MouseMove(String, f64, f64),
    BinaryFrame(Vec<u8>),
    AudioChunk(Vec<u8>),
}
