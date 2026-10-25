//! Video and data encodings: framed key/delta video, H.264, JPEG and base64.

mod base64;
pub mod delta;
pub mod frame;
pub mod h264;
mod jpeg;

pub use base64::{from_base64, to_base64};
pub use jpeg::JpegEncoder;
