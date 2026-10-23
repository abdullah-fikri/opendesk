//! Framed video messages for viewers that advertise `video-delta` (and later `video-h264`).
//!
//! Layout (all integers big-endian):
//! ```text
//! 0  FF D8 00 4F   magic: starts with the JPEG SOI so existing relays forward it like a JPEG frame;
//!                  0x00 can never follow SOI in a real JPEG, so legacy frames are never mistaken for it
//! 4  version (1)
//! 5  kind          1 = key (payload is one full JPEG), 2 = delta (payload is one JPEG atlas), 3 = H.264 access unit
//! 6  flags         bit0 = H.264 IDR
//! 7  reserved
//! 8  seq u32       a delta applies only on top of seq-1; key frames restart the chain
//! 12 width u16, height u16
//! 16 move count u16, patch count u16
//! 20 moves:   src_x, src_y, dst_x, dst_y, w, h   (6 x u16) copied inside the framebuffer before patches
//!    patches: dst_x, dst_y, w, h, atlas_x, atlas_y (6 x u16) blitted from the decoded payload
//!    payload to the end of the message
//! ```

pub const MAGIC: [u8; 4] = [0xFF, 0xD8, 0x00, 0x4F];
pub const VERSION: u8 = 1;
const FIXED: usize = 20;

pub const KIND_KEY: u8 = 1;
pub const KIND_DELTA: u8 = 2;
pub const KIND_H264: u8 = 3;
pub const FLAG_IDR: u8 = 1;

/// Region copied inside the viewer framebuffer (DXGI move rects, e.g. scrolling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveRect {
    pub src_x: u16,
    pub src_y: u16,
    pub dst_x: u16,
    pub dst_y: u16,
    pub w: u16,
    pub h: u16,
}

/// Region of the decoded payload image placed at (dst_x, dst_y).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Patch {
    pub dst_x: u16,
    pub dst_y: u16,
    pub w: u16,
    pub h: u16,
    pub atlas_x: u16,
    pub atlas_y: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: u8,
    pub flags: u8,
    pub seq: u32,
    pub width: u16,
    pub height: u16,
    pub moves: Vec<MoveRect>,
    pub patches: Vec<Patch>,
}

/// True for framed messages; anything else on the video channel is a legacy full JPEG.
pub fn is_framed(bytes: &[u8]) -> bool {
    bytes.len() >= FIXED && bytes[..4] == MAGIC
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn get16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

impl FrameHeader {
    pub fn encode(&self, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(FIXED + (self.moves.len() + self.patches.len()) * 12 + payload.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&[VERSION, self.kind, self.flags, 0]);
        out.extend_from_slice(&self.seq.to_be_bytes());
        put16(&mut out, self.width);
        put16(&mut out, self.height);
        put16(&mut out, self.moves.len() as u16);
        put16(&mut out, self.patches.len() as u16);
        for m in &self.moves {
            for v in [m.src_x, m.src_y, m.dst_x, m.dst_y, m.w, m.h] {
                put16(&mut out, v);
            }
        }
        for p in &self.patches {
            for v in [p.dst_x, p.dst_y, p.w, p.h, p.atlas_x, p.atlas_y] {
                put16(&mut out, v);
            }
        }
        out.extend_from_slice(payload);
        out
    }

    /// Parses a framed message; None for legacy JPEGs, unknown versions or truncated input.
    pub fn parse(bytes: &[u8]) -> Option<(FrameHeader, &[u8])> {
        if !is_framed(bytes) || bytes[4] != VERSION {
            return None;
        }
        let n_moves = get16(bytes, 16) as usize;
        let n_patches = get16(bytes, 18) as usize;
        let body = FIXED + (n_moves + n_patches) * 12;
        if bytes.len() < body {
            return None;
        }
        let field = |i: usize, k: usize| get16(bytes, FIXED + i * 12 + k * 2);
        let moves = (0..n_moves)
            .map(|i| MoveRect { src_x: field(i, 0), src_y: field(i, 1), dst_x: field(i, 2), dst_y: field(i, 3), w: field(i, 4), h: field(i, 5) })
            .collect();
        let patches = (n_moves..n_moves + n_patches)
            .map(|i| Patch { dst_x: field(i, 0), dst_y: field(i, 1), w: field(i, 2), h: field(i, 3), atlas_x: field(i, 4), atlas_y: field(i, 5) })
            .collect();
        let header = FrameHeader {
            kind: bytes[5],
            flags: bytes[6],
            seq: u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            width: get16(bytes, 12),
            height: get16(bytes, 14),
            moves,
            patches,
        };
        Some((header, &bytes[body..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = FrameHeader {
            kind: KIND_DELTA,
            flags: 0,
            seq: 0xDEAD_BEEF,
            width: 1920,
            height: 1080,
            moves: vec![MoveRect { src_x: 0, src_y: 40, dst_x: 0, dst_y: 0, w: 800, h: 600 }],
            patches: vec![
                Patch { dst_x: 32, dst_y: 64, w: 64, h: 32, atlas_x: 0, atlas_y: 0 },
                Patch { dst_x: 1888, dst_y: 1056, w: 32, h: 24, atlas_x: 64, atlas_y: 0 },
            ],
        };
        let msg = h.encode(b"payload");
        assert_eq!(&msg[..2], &[0xFF, 0xD8], "relays only forward SOI-prefixed binaries");
        let (back, payload) = FrameHeader::parse(&msg).unwrap();
        assert_eq!(back, h);
        assert_eq!(payload, b"payload");
    }

    #[test]
    fn legacy_and_broken_input_is_rejected() {
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 0xFF, 0xD9];
        assert!(!is_framed(&jpeg));
        assert!(FrameHeader::parse(&jpeg).is_none());
        let h = FrameHeader { kind: KIND_KEY, flags: 0, seq: 1, width: 2, height: 2, moves: vec![], patches: vec![Patch { dst_x: 0, dst_y: 0, w: 2, h: 2, atlas_x: 0, atlas_y: 0 }] };
        let msg = h.encode(&[]);
        assert!(FrameHeader::parse(&msg[..msg.len() - 1]).is_none(), "truncated rect list");
        let mut future = msg.clone();
        future[4] = 99;
        assert!(FrameHeader::parse(&future).is_none(), "unknown version");
    }
}
