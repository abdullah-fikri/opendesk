//! Datagram sealing (Noise transport, anti-replay window), relay tokens and fragment reassembly.

use super::*;

pub(super) fn parse_token(hex: &str) -> Option<[u8; 16]> {
    let bytes = hex.as_bytes();
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

/// Sliding anti-replay window over explicit nonces (bit = nonce % window).
pub(super) struct Replay {
    pub(super) top: Option<u64>,
    pub(super) bits: [u64; (REPLAY_WINDOW / 64) as usize],
}

impl Replay {
    pub(super) fn new() -> Self {
        Self { top: None, bits: [0; (REPLAY_WINDOW / 64) as usize] }
    }
    pub(super) fn bit(n: u64) -> (usize, u64) {
        let i = n % REPLAY_WINDOW;
        ((i / 64) as usize, 1u64 << (i % 64))
    }
    pub(super) fn check(&self, n: u64) -> bool {
        match self.top {
            None => true,
            Some(top) if n > top => true,
            Some(top) if top - n >= REPLAY_WINDOW => false,
            Some(_) => {
                let (w, m) = Self::bit(n);
                self.bits[w] & m == 0
            }
        }
    }
    /// Call only after the packet authenticated.
    pub(super) fn mark(&mut self, n: u64) {
        let top = self.top.unwrap_or(n);
        if self.top.is_none() || n > top {
            if self.top.is_none() || n - top >= REPLAY_WINDOW {
                self.bits = [0; (REPLAY_WINDOW / 64) as usize];
            } else {
                for k in top + 1..n {
                    let (w, m) = Self::bit(k);
                    self.bits[w] &= !m;
                }
                let (w, m) = Self::bit(n);
                self.bits[w] &= !m;
            }
            self.top = Some(n);
        }
        let (w, m) = Self::bit(n);
        self.bits[w] |= m;
    }
}

pub(super) fn seal(t: &snow::StatelessTransportState, nonce: u64, plain: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![0u8; HEADER + plain.len() + TAG];
    out[0] = MAGIC;
    out[1..HEADER].copy_from_slice(&nonce.to_be_bytes());
    let n = t.write_message(nonce, plain, &mut out[HEADER..]).ok()?;
    out.truncate(HEADER + n);
    Some(out)
}

pub(super) fn open(t: &snow::StatelessTransportState, replay: &mut Replay, pkt: &[u8]) -> Option<Vec<u8>> {
    if pkt.len() < HEADER + TAG || pkt.len() > MAX_DATAGRAM || pkt[0] != MAGIC {
        return None;
    }
    let nonce = u64::from_be_bytes(pkt[1..HEADER].try_into().ok()?);
    // Nonces above the P2P domain belong to relay-path streams sealed with the same keys (`infra::e2e`).
    if nonce >= P2P_NONCE_LIMIT || !replay.check(nonce) {
        return None;
    }
    let mut plain = vec![0u8; pkt.len() - HEADER];
    let n = t.read_message(nonce, &pkt[HEADER..], &mut plain).ok()?;
    replay.mark(nonce);
    plain.truncate(n);
    Some(plain)
}

/// Latest-frame-wins reassembly: a newer frame id discards the incomplete older one.
pub(super) struct Reasm {
    pub(super) id: u32,
    pub(super) parts: Vec<Option<Vec<u8>>>,
    pub(super) got: usize,
}

impl Reasm {
    pub(super) fn push(slot: &mut Option<Reasm>, id: u32, idx: usize, count: usize, data: &[u8]) -> Option<Vec<u8>> {
        if count == 0 || count > MAX_FRAGS || idx >= count {
            return None;
        }
        let newer = slot.as_ref().is_none_or(|r| (id.wrapping_sub(r.id) as i32) > 0);
        if newer {
            *slot = Some(Reasm { id, parts: vec![None; count], got: 0 });
        }
        let r = slot.as_mut()?;
        if r.id != id || r.parts.len() != count {
            return None;
        }
        if r.parts[idx].is_none() {
            r.parts[idx] = Some(data.to_vec());
            r.got += 1;
        }
        if r.got < count {
            return None;
        }
        let frame = slot.take()?.parts.into_iter().flatten().flatten().collect();
        Some(frame)
    }
}
