//! The direct file channel's wire format (`file-channel`), without I/O: how messages become a byte stream and back,
//! and the `p2p-signal` that carries the candidates. `infra::file_channel` moves the bytes.
//!
//! On the TCP stream every frame is `[length u16 BE][Noise transport message]`. The plaintext of the frames, in
//! order, is a stream of messages `[kind u8][length u32 BE][payload]`: the file stream kinds of `domain::bulk`
//! (JSON file messages, binary chunks) plus a few of the channel's own.

use std::net::SocketAddr;

use serde_json::{Value, json};

/// Largest plaintext in one Noise transport message (65535 minus the 16-byte tag).
pub const SEGMENT: usize = 65535 - 16;
/// Largest message accepted (a binary chunk of 128 KiB plus its header, with room to spare).
pub const MAX_MESSAGE: usize = 1 << 20;
const HEADER: usize = 5;

/// Channel control: the viewer picked this connection (first message on it; the host adopts the connection it
/// arrives on and drops any other).
pub const KIND_SELECT: u8 = 0xF0;
/// Channel control: keep-alive while no file moves.
pub const KIND_PING: u8 = 0xF1;

/// Encodes one message for the stream.
pub fn encode(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Cuts a message stream into frame plaintexts of at most [`SEGMENT`] bytes.
pub fn segments(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.chunks(SEGMENT)
}

/// Reassembles messages from frame plaintexts.
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
}

impl Reader {
    /// Adds a frame's plaintext; returns the messages it completed, or None when the stream is not ours (a message
    /// larger than [`MAX_MESSAGE`]), which ends the connection.
    pub fn push(&mut self, plain: &[u8]) -> Option<Vec<(u8, Vec<u8>)>> {
        self.buf.extend_from_slice(plain);
        let mut out = Vec::new();
        let mut at = 0;
        while self.buf.len() - at >= HEADER {
            let len = u32::from_be_bytes(self.buf[at + 1..at + HEADER].try_into().ok()?) as usize;
            if len > MAX_MESSAGE {
                return None;
            }
            if self.buf.len() - at < HEADER + len {
                break;
            }
            out.push((self.buf[at], self.buf[at + HEADER..at + HEADER + len].to_vec()));
            at += HEADER + len;
        }
        self.buf.drain(..at);
        Some(out)
    }
}

/// The candidates signal: `{"v":2,"fc":["addr", ...]}`. Sent only to partners that list `file-channel`, so
/// older peers never see one.
pub fn signal(cands: &[SocketAddr]) -> String {
    json!({ "v": 2, "fc": cands.iter().map(|a| a.to_string()).collect::<Vec<_>>() }).to_string()
}

/// True for a file channel signal.
pub fn is_signal(v: &Value) -> bool {
    v.get("v").and_then(Value::as_u64) == Some(2) && v.get("fc").is_some_and(Value::is_array)
}

/// The partner's candidates in a signal: at most 8 usable addresses (no unspecified, multicast or port 0).
pub fn parse_signal(v: &Value) -> Vec<SocketAddr> {
    let Some(list) = v.get("fc").and_then(Value::as_array) else { return Vec::new() };
    let mut out: Vec<SocketAddr> = list
        .iter()
        .filter_map(|c| c.as_str()?.parse::<SocketAddr>().ok())
        .filter(|a| a.port() != 0 && !a.ip().is_unspecified() && !a.ip().is_multicast())
        .collect();
    out.dedup();
    out.truncate(8);
    out
}

/// Noise prologue binding the channel to this session's two devices (as the end-to-end handshake does).
pub fn prologue(viewer: &str, host: &str) -> String {
    format!("opendesk-fc-v1|{viewer}|{host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_any_cut_of_the_stream() {
        let big = vec![7u8; SEGMENT * 2 + 10];
        let mut stream = encode(2, &big);
        stream.extend(encode(1, b"{\"action\":\"end\"}"));
        stream.extend(encode(KIND_PING, &[]));
        for cut in [1, 3, 5, 4096, SEGMENT] {
            let mut r = Reader::default();
            let mut got = Vec::new();
            for piece in stream.chunks(cut) {
                got.extend(r.push(piece).unwrap());
            }
            assert_eq!(got.len(), 3, "cut {cut}");
            assert_eq!(got[0], (2, big.clone()));
            assert_eq!(got[1].1, b"{\"action\":\"end\"}");
            assert_eq!(got[2], (KIND_PING, vec![]));
        }
        assert!(segments(&stream).all(|s| s.len() <= SEGMENT));
    }

    #[test]
    fn an_oversized_message_ends_the_stream() {
        let mut head = vec![1u8];
        head.extend_from_slice(&((MAX_MESSAGE + 1) as u32).to_be_bytes());
        assert!(Reader::default().push(&head).is_none());
    }

    #[test]
    fn signal_round_trip_keeps_only_usable_addresses() {
        let cands: Vec<SocketAddr> = vec!["192.168.1.5:40000".parse().unwrap(), "[2001:db8::1]:40000".parse().unwrap()];
        let v: Value = serde_json::from_str(&signal(&cands)).unwrap();
        assert!(is_signal(&v));
        assert_eq!(parse_signal(&v), cands);
        let bad: Value = json!({"v": 2, "fc": ["0.0.0.0:1", "1.2.3.4:0", "224.0.0.1:5", "x", "5.6.7.8:9"]});
        assert_eq!(parse_signal(&bad), vec!["5.6.7.8:9".parse::<SocketAddr>().unwrap()]);
        // The end-to-end and P2P path signals are not ours.
        assert!(!is_signal(&json!({"v": 2, "e2e": "x"})));
        assert!(!is_signal(&json!({"v": 2, "c": []})));
    }
}
