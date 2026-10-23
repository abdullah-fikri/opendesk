use serde_json::Value;

pub enum OutgoingMsg {
    Json(Value),
    FileTransfer {
        msg: Value,
        status_text: Option<String>,
    },
    MouseMove(f64, f64),
    BinaryFrame(Vec<u8>),
    AudioChunk(Vec<u8>),
}
