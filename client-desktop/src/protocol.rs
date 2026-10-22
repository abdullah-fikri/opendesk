use serde_json::Value;

pub enum OutgoingMsg {
    Json(Value),
    MouseMove(f64, f64),
    BinaryFrame(Vec<u8>),
    AudioChunk(String),
}
