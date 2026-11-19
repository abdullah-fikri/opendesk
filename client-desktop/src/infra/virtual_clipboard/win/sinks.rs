//! Pull sinks: transfer IDs we asked the partner to send, routed to the paste stream reading them.

use super::*;

pub(super) enum SinkMsg {
    Data(Vec<u8>),
    End(bool),
}

pub(super) struct Sink {
    pub(super) from: String,
    pub(super) name: String,
    pub(super) size: u64,
    pub(super) received: u64,
    pub(super) hash: Sha256,
    pub(super) out: SinkOut,
}

/// Where a pull's verified bytes go.
pub(super) enum SinkOut {
    /// A paste stream in this process.
    Local(Sender<SinkMsg>),
    /// A paste stream in the clipboard helper, under this transfer ID.
    Helper(String),
}

impl SinkOut {
    /// Delivers one message; a closed stream means the paste was cancelled and later bytes are dropped.
    pub(super) fn send(&self, msg: SinkMsg) {
        use crate::infra::clipboard_helper;
        match (self, msg) {
            (SinkOut::Local(tx), msg) => {
                let _ = tx.send(msg);
            }
            (SinkOut::Helper(tid), SinkMsg::Data(bytes)) => {
                clipboard_helper::send_data(tid, &bytes);
            }
            (SinkOut::Helper(tid), SinkMsg::End(ok)) => {
                clipboard_helper::send(&ToHelper::End { tid: tid.clone(), ok });
            }
        }
    }
}

pub(super) static SINKS: Mutex<Option<HashMap<String, Sink>>> = Mutex::new(None);

pub(super) fn with_sinks<R>(f: impl FnOnce(&mut HashMap<String, Sink>) -> R) -> R {
    f(SINKS.lock().get_or_insert_with(HashMap::new))
}

/// Handles a `file-transfer` payload if it belongs to one of our pulls. Returns `(status, ack)` like
/// `FileReceiver::handle_payload`, or None when the transfer is not ours.
pub fn on_transfer(
    from: &str,
    action: FileAction,
    id: &str,
    payload: &FilePayload,
) -> Option<(Option<String>, Option<bool>)> {
    with_sinks(|sinks| {
        if sinks.get(id).is_none_or(|s| s.from != from) {
            return None;
        }
        Some(match action {
            FileAction::Start => {
                let size = payload.size;
                if size == Some(sinks[id].size) {
                    (None, None)
                } else {
                    let sink = sinks.remove(id).unwrap();
                    sink.out.send(SinkMsg::End(false));
                    (Some(format!("Paste failed for {}: the file size changed", sink.name)), Some(false))
                }
            }
            FileAction::Chunk => {
                let bytes = payload.chunk.as_deref().filter(|b64| b64.len() <= 48_000).and_then(from_base64);
                sink_data(sinks, id, bytes)
            }
            FileAction::End => {
                let sink = sinks.remove(id).unwrap();
                let digest: String = sink.hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
                let ok = sink.received == sink.size && payload.sha256.as_deref() == Some(digest.as_str());
                sink.out.send(SinkMsg::End(ok));
                let status = if ok {
                    format!("Pasted: {}", sink.name)
                } else {
                    format!("Paste failed for {}: size or SHA-256 mismatch", sink.name)
                };
                (Some(status), Some(ok))
            }
            _ => (None, None),
        })
    })
}

/// Largest binary chunk a paste stream accepts (the sender's `BINARY_CHUNK_SIZE`).
pub(super) const MAX_BINARY_CHUNK: usize = 128 * 1024;

/// A binary chunk (`file-pull-bin`) for a paste stream we requested; None when it belongs to no paste. The
/// relay keeps the order, so `offset` must be exactly where the stream stands.
pub fn on_chunk(from: &str, id: &str, offset: u64, data: &[u8]) -> Option<(Option<String>, Option<bool>)> {
    with_sinks(|sinks| {
        let sink = sinks.get(id).filter(|s| s.from == from)?;
        let ok = offset == sink.received && data.len() <= MAX_BINARY_CHUNK;
        Some(sink_data(sinks, id, ok.then(|| data.to_vec())))
    })
}

/// Hands one chunk to the paste stream `id`, or fails the paste when `bytes` is None or overruns the size.
pub(super) fn sink_data(
    sinks: &mut HashMap<String, Sink>,
    id: &str,
    bytes: Option<Vec<u8>>,
) -> (Option<String>, Option<bool>) {
    let sink = sinks.get_mut(id).unwrap();
    match bytes.filter(|b| sink.received + b.len() as u64 <= sink.size) {
        Some(bytes) => {
            sink.hash.update(&bytes);
            sink.received += bytes.len() as u64;
            let pct = crate::domain::transfer::percent(sink.received, sink.size);
            let status = format!("Pasting {} ({}%)...", sink.name, pct);
            sink.out.send(SinkMsg::Data(bytes));
            (Some(status), None)
        }
        None => {
            let sink = sinks.remove(id).unwrap();
            sink.out.send(SinkMsg::End(false));
            (Some(format!("Paste failed for {}", sink.name)), Some(false))
        }
    }
}

/// Fails every paste in progress (relay connection lost).
pub fn abort_pulls() {
    with_sinks(|sinks| {
        for (_, sink) in sinks.drain() {
            sink.out.send(SinkMsg::End(false));
        }
    });
}
