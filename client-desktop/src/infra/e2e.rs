//! End-to-end encryption between the two peers of a session (`docs/features/e2e.md`).
//!
//! When both peers list the `e2e` capability, the viewer and the host run `Noise_XX_25519_ChaChaPoly_BLAKE2s`
//! with their static device keys ([`crate::infra::identity`]) over `p2p-signal` messages. The one handshake
//! keys both paths:
//! - the direct UDP path (`infra::p2p`), nonce domain 0, instead of its own `Noise_NN` handshake;
//! - the relay path: video, voice and file chunks are sealed after the header bytes the relay routes on, and
//!   input events are sealed inside an `{"action":"e2e"}` event. One nonce domain per stream (the top byte of
//!   the 64-bit Noise nonce), so streams the relay queues separately never share a counter.
//!
//! Sealed binary message: `[routing header, marked as sealed][first nonce u64 BE][segments]`. The segments
//! seal `[original length u32 BE][original message]` (header included, so a changed header is detected) in
//! pieces of at most one Noise message each, with consecutive nonces.

use parking_lot::Mutex;
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use core_engine::codec::{from_base64, to_base64};
use serde_json::{Value, json};

use crate::infra::identity::Identity;
use crate::protocol::binary;

const NOISE_XX: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Marks a sealed video (byte 7, reserved in framed video) or voice (byte 1, never a rate or 0x4F) message.
const SEALED: u8 = 0xE2;
/// Offset field of a sealed file chunk; the real offset is inside.
const SEALED_OFFSET: [u8; 8] = [0xFF; 8];
const FRAMED_VIDEO: [u8; 4] = [0xFF, 0xD8, 0x00, 0x4F];
const FRAMED_HEADER: usize = 8;
const TAG: usize = 16;
/// Largest plaintext of one Noise transport message.
const SEGMENT: usize = 65535 - TAG;
/// Bits below the stream's domain byte in a nonce.
const DOMAIN_SHIFT: u32 = 56;
/// Every P2P datagram nonce is below this (domain 0).
pub const P2P_NONCE_LIMIT: u64 = 1 << DOMAIN_SHIFT;
/// Largest handshake message accepted from a signal.
const MAX_HANDSHAKE: usize = 256;

/// One relay-path stream, with its own nonce domain and replay counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Video = 1,
    Audio = 2,
    File = 3,
    Input = 4,
}
const STREAMS: usize = 5;

/// Client-side kill switch: `OPENDESK_E2E=0` neither advertises nor uses end-to-end encryption.
pub fn enabled() -> bool {
    std::env::var("OPENDESK_E2E").map(|v| v != "0").unwrap_or(true)
}

/// True for a `p2p-signal` payload that belongs to the end-to-end handshake (not to the P2P path).
pub fn is_signal(v: &Value) -> bool {
    v.get("v").and_then(Value::as_u64) == Some(2) && (v.get("e2e").is_some() || v.get("e2e-ready").is_some())
}

/// What the peer's static key check decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Trust,
    Reject,
}

enum Phase {
    Pending(Box<snow::HandshakeState>),
    Done,
    Failed,
}

struct Inner {
    phase: Phase,
    /// Next nonce per stream (outbound).
    tx: [u64; STREAMS],
    /// Highest nonce opened per stream (inbound); every stream arrives in order through its relay queue.
    rx: [Option<u64>; STREAMS],
    /// A sealed message arrived on this stream: plain ones on it are dropped from now on.
    sealed_seen: [bool; STREAMS],
}

/// The end-to-end channel of one session (one per partner).
pub struct E2e {
    pub partner: String,
    is_host: bool,
    inner: Mutex<Inner>,
    transport: OnceLock<Arc<snow::StatelessTransportState>>,
    peer_key: OnceLock<Vec<u8>>,
    /// Outbound traffic is sealed: the host once it read the last handshake message, the viewer once the host
    /// confirmed that (`e2e-ready`), so nothing sealed reaches a host that cannot open it yet.
    sending: AtomicBool,
}

impl E2e {
    /// Starts the handshake. The viewer initiates: the returned signal goes to the partner.
    pub fn start(is_host: bool, local_clean: &str, partner: &str, id: &Identity) -> Option<(Arc<E2e>, Option<String>)> {
        let local: String = local_clean.chars().filter(|c| c.is_ascii_digit()).collect();
        let (viewer, host) = if is_host { (partner, local.as_str()) } else { (local.as_str(), partner) };
        let prologue = format!("opendesk-e2e-v1|{viewer}|{host}");
        let builder = snow::Builder::new(NOISE_XX.parse().ok()?).prologue(prologue.as_bytes()).ok()?;
        let builder = builder.local_private_key(id.secret()).ok()?;
        let mut hs = if is_host { builder.build_responder() } else { builder.build_initiator() }.ok()?;
        let first = if is_host {
            None
        } else {
            let mut buf = [0u8; MAX_HANDSHAKE];
            let n = hs.write_message(&[], &mut buf).ok()?;
            Some(handshake_signal(&buf[..n]))
        };
        let e2e = Arc::new(E2e {
            partner: partner.to_string(),
            is_host,
            inner: Mutex::new(Inner {
                phase: Phase::Pending(Box::new(hs)),
                tx: [0; STREAMS],
                rx: [None; STREAMS],
                sealed_seen: [false; STREAMS],
            }),
            transport: OnceLock::new(),
            peer_key: OnceLock::new(),
            sending: AtomicBool::new(false),
        });
        Some((e2e, first))
    }

    /// Handles one handshake signal. `verify` judges the peer's static key once it is known; a rejected key
    /// ends the handshake (the viewer then never sends its last message, so neither side seals anything).
    /// Returns the signal to send back, if any.
    pub fn on_signal(&self, v: &Value, verify: impl FnOnce(&[u8]) -> Verdict) -> Option<String> {
        let mut inner = self.inner.lock();
        if v.get("e2e-ready").is_some() {
            if !self.is_host && self.transport.get().is_some() {
                self.sending.store(true, Ordering::SeqCst);
            }
            return None;
        }
        let msg = v.get("e2e").and_then(Value::as_str).and_then(from_base64)?;
        let Phase::Pending(hs) = &mut inner.phase else { return None };
        let mut buf = [0u8; MAX_HANDSHAKE];
        if msg.len() > MAX_HANDSHAKE || hs.read_message(&msg, &mut buf).is_err() {
            inner.phase = Phase::Failed;
            return None;
        }
        if self.peer_key.get().is_none()
            && let Some(key) = hs.get_remote_static().map(<[u8]>::to_vec)
        {
            if verify(&key) == Verdict::Reject {
                inner.phase = Phase::Failed;
                return None;
            }
            let _ = self.peer_key.set(key);
        }
        let mut reply = None;
        if !hs.is_handshake_finished() && hs.is_my_turn() {
            match hs.write_message(&[], &mut buf) {
                Ok(n) => reply = Some(handshake_signal(&buf[..n])),
                Err(_) => {
                    inner.phase = Phase::Failed;
                    return None;
                }
            }
        }
        if hs.is_handshake_finished() {
            let Phase::Pending(hs) = std::mem::replace(&mut inner.phase, Phase::Failed) else { return None };
            let Ok(t) = hs.into_stateless_transport_mode() else { return None };
            let _ = self.transport.set(Arc::new(t));
            inner.phase = Phase::Done;
            if self.is_host {
                self.sending.store(true, Ordering::SeqCst);
                reply = Some(json!({ "v": 2, "e2e-ready": true }).to_string());
            }
        }
        reply
    }

    /// True for the channel this machine opened as the host of the session.
    pub fn is_host(&self) -> bool {
        self.is_host
    }

    /// The session's transport keys once the handshake completed (for the P2P path).
    pub fn transport(&self) -> Option<Arc<snow::StatelessTransportState>> {
        self.transport.get().cloned()
    }

    /// The partner's verified static key.
    #[cfg(test)]
    pub fn peer_key(&self) -> Option<&[u8]> {
        self.peer_key.get().map(Vec::as_slice)
    }

    /// True while this side seals what it sends (shown as "E2E on").
    pub fn is_on(&self) -> bool {
        self.sending.load(Ordering::SeqCst)
    }

    /// Seals a binary message for the relay path. None: send it unchanged (not sealing yet, or a kind that
    /// stays plain, such as a legacy JPEG).
    pub fn seal(&self, msg: &[u8]) -> Option<Vec<u8>> {
        if !self.is_on() {
            return None;
        }
        let (stream, header) = classify_plain(msg)?;
        let t = self.transport.get()?;
        let nonce = self.reserve(stream, msg.len())?;
        let mut out = Vec::with_capacity(header + 8 + 4 + msg.len() + TAG * (2 + msg.len() / SEGMENT));
        out.extend_from_slice(&msg[..header]);
        mark(stream, &mut out[..header]);
        out.extend_from_slice(&nonce.to_be_bytes());
        seal_segments(t, nonce, msg, &mut out)?;
        Some(out)
    }

    /// Opens a binary message from the relay path. `Borrowed` passes a plain message through; None drops it
    /// (forged, replayed, or plain on a stream that is already sealed).
    pub fn open<'a>(&self, msg: &'a [u8]) -> Option<Cow<'a, [u8]>> {
        let mut inner = self.inner.lock();
        let Some((stream, header)) = classify_sealed(msg) else {
            return match plain_stream(msg) {
                Some(s) if inner.sealed_seen[s as usize] => None,
                _ => Some(Cow::Borrowed(msg)),
            };
        };
        let t = self.transport.get()?;
        let body = msg.get(header..)?;
        let nonce = u64::from_be_bytes(body.get(..8)?.try_into().ok()?);
        let (plain, last) = open_checked(t, &mut inner, stream, nonce, &body[8..])?;
        let mut expected = plain.get(..header)?.to_vec();
        mark(stream, &mut expected);
        if expected != msg[..header] || classify_plain(&plain).map(|c| c.0) != Some(stream) {
            return None;
        }
        inner.rx[stream as usize] = Some(last);
        inner.sealed_seen[stream as usize] = true;
        Some(Cow::Owned(plain))
    }

    /// Seals an input event for `relay-input`: `{"action":"e2e","d":base64}`. None: send it unchanged.
    pub fn seal_event(&self, event: &Value) -> Option<Value> {
        if !self.is_on() {
            return None;
        }
        let t = self.transport.get()?;
        let json = serde_json::to_vec(event).ok()?;
        let nonce = self.reserve(Stream::Input, json.len())?;
        let mut out = nonce.to_be_bytes().to_vec();
        seal_segments(t, nonce, &json, &mut out)?;
        Some(json!({ "action": "e2e", "d": to_base64(&out) }))
    }

    /// Opens an input event from `input-command`. None drops it (forged, replayed, or plain once events from
    /// this partner are sealed).
    pub fn open_event(&self, event: &Value) -> Option<Value> {
        let mut inner = self.inner.lock();
        if event.get("action").and_then(Value::as_str) != Some("e2e") {
            return (!inner.sealed_seen[Stream::Input as usize]).then(|| event.clone());
        }
        let t = self.transport.get()?;
        let body = event.get("d").and_then(Value::as_str).and_then(from_base64)?;
        let nonce = u64::from_be_bytes(body.get(..8)?.try_into().ok()?);
        let (plain, last) = open_checked(t, &mut inner, Stream::Input, nonce, &body[8..])?;
        let value: Value = serde_json::from_slice(&plain).ok()?;
        inner.rx[Stream::Input as usize] = Some(last);
        inner.sealed_seen[Stream::Input as usize] = true;
        Some(value)
    }

    /// Takes the nonces for one message of `len` bytes on `stream`.
    fn reserve(&self, stream: Stream, len: usize) -> Option<u64> {
        let segments = segments(len) as u64;
        let mut inner = self.inner.lock();
        let counter = inner.tx[stream as usize];
        let next = counter.checked_add(segments).filter(|n| *n < P2P_NONCE_LIMIT)?;
        inner.tx[stream as usize] = next;
        Some(((stream as u64) << DOMAIN_SHIFT) | counter)
    }
}

fn handshake_signal(msg: &[u8]) -> String {
    json!({ "v": 2, "e2e": to_base64(msg) }).to_string()
}

fn segments(len: usize) -> usize {
    (4 + len).div_ceil(SEGMENT)
}

/// Appends the sealed segments of `[len u32 BE][msg]` to `out`, starting at `nonce`.
fn seal_segments(t: &snow::StatelessTransportState, nonce: u64, msg: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let mut plain = Vec::with_capacity(4 + msg.len());
    plain.extend_from_slice(&u32::try_from(msg.len()).ok()?.to_be_bytes());
    plain.extend_from_slice(msg);
    for (i, piece) in plain.chunks(SEGMENT).enumerate() {
        let at = out.len();
        out.resize(at + piece.len() + TAG, 0);
        let n = t.write_message(nonce + i as u64, piece, &mut out[at..]).ok()?;
        out.truncate(at + n);
    }
    Some(())
}

/// Opens the segments starting at `nonce` on `stream`, after the domain and replay checks. Returns the
/// original message and the last nonce used; the caller records it once the content checks pass too.
fn open_checked(
    t: &snow::StatelessTransportState,
    inner: &mut Inner,
    stream: Stream,
    nonce: u64,
    sealed: &[u8],
) -> Option<(Vec<u8>, u64)> {
    if nonce >> DOMAIN_SHIFT != stream as u64 || inner.rx[stream as usize].is_some_and(|last| nonce <= last) {
        return None;
    }
    let mut plain = Vec::with_capacity(sealed.len());
    let mut buf = vec![0u8; SEGMENT];
    let mut n = nonce;
    for piece in sealed.chunks(SEGMENT + TAG) {
        if n >> DOMAIN_SHIFT != stream as u64 {
            return None;
        }
        let len = t.read_message(n, piece, &mut buf).ok()?;
        plain.extend_from_slice(&buf[..len]);
        n += 1;
    }
    let len = u32::from_be_bytes(plain.get(..4)?.try_into().ok()?) as usize;
    if plain.len() != 4 + len {
        return None; // a dropped or added segment
    }
    plain.drain(..4);
    Some((plain, n - 1))
}

/// Stream and routing header length of a message that can be sealed.
fn classify_plain(msg: &[u8]) -> Option<(Stream, usize)> {
    if msg.len() >= FRAMED_HEADER && msg[..4] == FRAMED_VIDEO {
        return Some((Stream::Video, FRAMED_HEADER));
    }
    if binary::is_audio(msg) {
        return Some((Stream::Audio, 2));
    }
    let chunk = binary::parse_file_chunk(msg)?;
    Some((Stream::File, 10 + chunk.id.len()))
}

/// Stream and routing header length of a sealed message.
fn classify_sealed(msg: &[u8]) -> Option<(Stream, usize)> {
    if msg.len() >= FRAMED_HEADER && msg[..4] == FRAMED_VIDEO && msg[7] == SEALED {
        return Some((Stream::Video, FRAMED_HEADER));
    }
    if binary::is_audio(msg) && msg[1] == SEALED {
        return Some((Stream::Audio, 2));
    }
    let chunk = binary::parse_file_chunk(msg)?;
    let header = 10 + chunk.id.len();
    (msg[header - 8..header] == SEALED_OFFSET).then_some((Stream::File, header))
}

/// The stream a plain message would belong to: any JPEG or framed video, voice, file chunks.
fn plain_stream(msg: &[u8]) -> Option<Stream> {
    if msg.len() > 2 && msg[0] == 0xFF && msg[1] == 0xD8 {
        return Some(Stream::Video);
    }
    classify_plain(msg).map(|c| c.0)
}

/// Turns a plain routing header into its sealed form; the relay still reads the same routing bytes.
fn mark(stream: Stream, header: &mut [u8]) {
    match stream {
        Stream::Video => header[7] = SEALED,
        Stream::Audio => header[1] = SEALED,
        Stream::File => {
            let n = header.len();
            header[n - 8..].copy_from_slice(&SEALED_OFFSET);
        }
        Stream::Input => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::identity::tests_support::identity;

    /// Runs the handshake between a fresh viewer and host; `trust` is the viewer's verdict on the host key.
    fn session(trust: Verdict) -> (Arc<E2e>, Arc<E2e>) {
        let (vid, hid) = (identity(), identity());
        let (viewer, first) = E2e::start(false, "111111111", "222222222", &vid).unwrap();
        let (host, none) = E2e::start(true, "222222222", "111111111", &hid).unwrap();
        assert!(none.is_none(), "the host never initiates");
        let parse = |s: String| serde_json::from_str::<Value>(&s).unwrap();
        let msg1 = parse(first.unwrap());
        assert!(is_signal(&msg1));
        let msg2 = parse(host.on_signal(&msg1, |_| Verdict::Trust).unwrap());
        let host_key = hid.public;
        let msg3 = viewer.on_signal(&msg2, |k| {
            assert_eq!(k, host_key, "viewer sees the host's static key");
            trust
        });
        if let Some(msg3) = msg3 {
            let ready = parse(
                host.on_signal(&parse(msg3), |k| {
                    assert_eq!(k, vid.public);
                    Verdict::Trust
                })
                .unwrap(),
            );
            assert!(is_signal(&ready));
            assert!(!viewer.is_on(), "viewer waits for the host's confirmation");
            assert!(viewer.on_signal(&ready, |_| Verdict::Trust).is_none());
        }
        (viewer, host)
    }

    fn framed(kind: u8, len: usize) -> Vec<u8> {
        let mut f = vec![0xFF, 0xD8, 0x00, 0x4F, 1, kind, 1, 0];
        f.extend((0..len).map(|i| i as u8));
        f
    }

    #[test]
    fn xx_handshake_keys_both_sides() {
        let (viewer, host) = session(Verdict::Trust);
        assert!(viewer.is_on() && host.is_on());
        let (tv, th) = (viewer.transport().unwrap(), host.transport().unwrap());
        let mut buf = [0u8; 64];
        let n = tv.write_message(3, b"p2p", &mut buf).unwrap();
        let mut out = [0u8; 64];
        let len = th.read_message(3, &buf[..n], &mut out).unwrap();
        assert_eq!(&out[..len], b"p2p");
        assert!(viewer.peer_key().is_some() && host.peer_key().is_some());
    }

    #[test]
    fn rejected_host_key_leaves_both_sides_plain() {
        let (viewer, host) = session(Verdict::Reject);
        assert!(!viewer.is_on() && !host.is_on());
        assert!(viewer.transport().is_none() && host.transport().is_none());
        let frame = framed(1, 100);
        assert!(host.seal(&frame).is_none(), "nothing is sealed without a handshake");
        assert_eq!(viewer.open(&frame).unwrap().as_ref(), &frame[..]);
    }

    #[test]
    fn media_round_trip_keeps_the_relay_routing_header() {
        let (viewer, host) = session(Verdict::Trust);
        // A key frame larger than one Noise message, an H.264 IDR, voice and a file chunk.
        let key = framed(1, 200_000);
        let sealed = host.seal(&key).unwrap();
        assert_eq!(&sealed[..7], &key[..7], "relay reads magic, kind and flags");
        assert_ne!(&sealed[8..], &key[8..]);
        assert_eq!(viewer.open(&sealed).unwrap().as_ref(), &key[..]);
        assert!(viewer.open(&sealed).is_none(), "replay accepted");

        let voice = [binary::AUDIO_MARKER, 0x4F, 0, 1, 9, 9, 9];
        let sealed = viewer.seal(&voice).unwrap();
        assert_eq!(sealed[0], binary::AUDIO_MARKER);
        assert_eq!(host.open(&sealed).unwrap().as_ref(), &voice[..]);

        let chunk = binary::file_chunk("ft_1", 1 << 20, &[7u8; 1000]).unwrap();
        let sealed = viewer.seal(&chunk).unwrap();
        let routed = binary::parse_file_chunk(&sealed).unwrap();
        assert_eq!(routed.id, "ft_1", "relay still finds the grant");
        assert_eq!(host.open(&sealed).unwrap().as_ref(), &chunk[..]);

        // Plain messages on a stream that is sealed now are dropped; other streams still pass.
        assert!(viewer.open(&framed(2, 10)).is_none());
        assert!(host.open(&chunk).is_none());
        let pcm = [binary::AUDIO_MARKER, 48, 0, 0];
        assert!(viewer.open(&pcm).is_some(), "no sealed voice seen from the host yet");
    }

    #[test]
    fn tampered_header_or_payload_and_wrong_domain_are_rejected() {
        let (viewer, host) = session(Verdict::Trust);
        let frame = framed(2, 500);
        let sealed = host.seal(&frame).unwrap();
        let mut bad = sealed.clone();
        bad[5] = 1; // a delta disguised as a key frame
        assert!(viewer.open(&bad).is_none());
        let mut bad = sealed.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(viewer.open(&bad).is_none());
        // A video message relabeled as voice is in the wrong nonce domain.
        let mut relabeled = vec![binary::AUDIO_MARKER, SEALED];
        relabeled.extend_from_slice(&sealed[8..]);
        assert!(viewer.open(&relabeled).is_none());
        assert_eq!(viewer.open(&sealed).unwrap().as_ref(), &frame[..]);
    }

    #[test]
    fn input_events_round_trip_and_plain_ones_stop_after_the_first_sealed() {
        let (viewer, host) = session(Verdict::Trust);
        let event = json!({ "action": "keydown", "keyCode": 65 });
        assert_eq!(host.open_event(&event), Some(event.clone()), "plain before anything sealed");
        let sealed = viewer.seal_event(&event).unwrap();
        assert_eq!(sealed["action"], "e2e");
        assert!(!sealed.to_string().contains("keyCode"));
        assert_eq!(host.open_event(&sealed), Some(event.clone()));
        assert!(host.open_event(&sealed).is_none(), "replay accepted");
        assert!(host.open_event(&event).is_none(), "plain event injected after the switch");
    }

    #[test]
    fn legacy_jpeg_and_monitor_frames_are_never_sealed() {
        let (_, host) = session(Verdict::Trust);
        assert!(host.seal(&[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4, 5]).is_none());
        assert!(host.seal(&binary::monitor_frame(&[0xFF, 0xD8, 0xFF])).is_none());
    }
}
