//! Pull-on-paste virtual files need Explorer's clipboard formats, so outside Windows this client does not
//! advertise `file-offer`: partners send copied files as ordinary transfers and nothing reaches this module.

use std::sync::mpsc::Sender;

use crate::protocol::{FileAction, FilePayload, Outgoing};

/// No pulls are ever started here, so no transfer belongs to one.
pub fn on_transfer(
    _from: &str,
    _action: FileAction,
    _id: &str,
    _payload: &FilePayload,
) -> Option<(Option<String>, Option<bool>)> {
    None
}

/// Nothing to abort.
pub fn abort_pulls() {}

/// Offers are not advertised; one from a misbehaving peer is ignored.
pub fn set_offer(_from: &str, _payload: &FilePayload, _tx_out: Sender<Outgoing>) -> Option<String> {
    None
}

/// Nothing to clear.
pub fn clear_offer(_partner: &str) {}
