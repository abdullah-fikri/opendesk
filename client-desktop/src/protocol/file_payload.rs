//! Payload of `relay-file` / `file-transfer` messages. The relay authorizes on `action` and `id` only and
//! forwards everything else untouched (see `docs/features/file-transfer.md`).

use serde::{Deserialize, Serialize};

/// What a file-transfer message does.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum FileAction {
    /// Opens a transfer (file or folder entry).
    Start,
    /// One base64 chunk of at most 32 KiB.
    Chunk,
    /// Closes a transfer and carries its SHA-256.
    End,
    /// Receiver's verdict on `end` (peers with `file-ack`).
    Ack,
    /// Clipboard file offer: names and sizes only.
    Offer,
    /// Pastes one offered file; the offerer answers with a normal transfer under `tid`.
    Pull,
    /// Anything this version does not understand.
    #[default]
    #[serde(other)]
    Unknown,
}

/// One file-transfer message. Fields not used by an action are omitted on the wire.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct FilePayload {
    pub action: FileAction,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Base64 data (`chunk`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk: Option<String>,
    /// Lowercase hex SHA-256 of the whole file (`end`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The entry is a folder (`start` of a folder transfer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<bool>,
    /// Folder transfer this entry belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Path of the entry relative to the folder root, `\`-separated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel: Option<String>,
    /// Last entry of a folder transfer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_end: Option<bool>,
    /// Paste the received files into the receiver's focused Explorer window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_paste: Option<bool>,
    /// Offered entries (`offer`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<OfferEntry>>,
    /// Offered entry to send (`pull`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<u64>,
    /// Transfer ID the puller chose (`pull`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tid: Option<String>,
    /// Ack verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
}

/// One entry of a clipboard file offer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct OfferEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<bool>,
}

impl FilePayload {
    fn new(action: FileAction, id: &str) -> Self {
        Self { action, id: id.to_string(), ..Self::default() }
    }

    /// `start` of a single file.
    pub fn start(id: &str, name: &str, size: u64) -> Self {
        Self { name: Some(name.to_string()), size: Some(size), ..Self::new(FileAction::Start, id) }
    }

    /// `chunk` carrying base64 `data`.
    pub fn chunk(id: &str, data: String) -> Self {
        Self { chunk: Some(data), ..Self::new(FileAction::Chunk, id) }
    }

    /// `end` with the file's checksum.
    pub fn end(id: &str, name: &str, sha256: String, auto_paste: bool) -> Self {
        Self {
            name: Some(name.to_string()),
            sha256: Some(sha256),
            auto_paste: Some(auto_paste),
            ..Self::new(FileAction::End, id)
        }
    }

    /// Receiver's verdict for transfer `id`.
    pub fn ack(id: &str, ok: bool) -> Self {
        Self { ok: Some(ok), ..Self::new(FileAction::Ack, id) }
    }

    /// Clipboard offer `id` listing `files`.
    pub fn offer(id: &str, files: Vec<OfferEntry>) -> Self {
        Self { files: Some(files), ..Self::new(FileAction::Offer, id) }
    }

    /// Requests entry `index` of offer `offer_id`, to be sent as transfer `tid`.
    // Only the Windows virtual clipboard pulls offered files.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn pull(offer_id: &str, index: u64, tid: &str) -> Self {
        Self { index: Some(index), tid: Some(tid.to_string()), ..Self::new(FileAction::Pull, offer_id) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payloads_round_trip_with_wire_names() {
        let p =
            FilePayload { group_end: Some(true), auto_paste: Some(false), ..FilePayload::start("ft_1", "a.txt", 3) };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(
            v,
            json!({ "action": "start", "id": "ft_1", "name": "a.txt", "size": 3, "groupEnd": true, "autoPaste": false })
        );
        assert_eq!(serde_json::from_value::<FilePayload>(v).unwrap(), p);
        let odd: FilePayload = serde_json::from_value(json!({ "action": "resume", "id": "x" })).unwrap();
        assert_eq!(odd.action, FileAction::Unknown);
    }
}
