//! Wire format between this client, the relay and the remote peer (see `docs/protocol.md` at the repository root).
//!
//! Types here only describe data on the wire. Decisions about what to send and what a message means
//! belong to the service layer.

pub mod binary;
pub mod file_payload;
pub mod input_event;
pub mod message;
pub mod outgoing;

pub use file_payload::{FileAction, FilePayload, OfferEntry};
pub use input_event::{HostStats, InputEvent, MonitorInfo};
#[cfg(feature = "admin")]
pub use message::SecurityDevice;
pub use message::{ClientMessage, HostEntry, ServerMessage};
pub use outgoing::Outgoing;
