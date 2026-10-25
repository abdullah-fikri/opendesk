//! Standard base64 (RFC 4648, with padding) used by the JSON wire format.

/// Encodes `bytes` as standard base64 with padding.
pub fn to_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let (chunks, rem) = bytes.as_chunks::<3>();

    for chunk in chunks {
        let b0 = chunk[0] as usize;
        let b1 = chunk[1] as usize;
        let b2 = chunk[2] as usize;
        out.push(TABLE[b0 >> 2] as char);
        out.push(TABLE[((b0 & 3) << 4) | (b1 >> 4)] as char);
        out.push(TABLE[((b1 & 0xF) << 2) | (b2 >> 6)] as char);
        out.push(TABLE[b2 & 0x3F] as char);
    }

    match rem.len() {
        1 => {
            let b0 = rem[0] as usize;
            out.push(TABLE[b0 >> 2] as char);
            out.push(TABLE[(b0 & 3) << 4] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let b0 = rem[0] as usize;
            let b1 = rem[1] as usize;
            out.push(TABLE[b0 >> 2] as char);
            out.push(TABLE[((b0 & 3) << 4) | (b1 >> 4)] as char);
            out.push(TABLE[(b1 & 0xF) << 2] as char);
            out.push('=');
        }
        _ => {}
    }

    out
}

/// Decodes standard base64 (padding optional); `None` on any invalid character.
pub fn from_base64(s: &str) -> Option<Vec<u8>> {
    let mut table = [255u8; 256];
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for (i, &c) in CHARS.iter().enumerate() {
        table[c as usize] = i as u8;
    }

    let clean = s.trim().as_bytes();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;

    for &b in clean {
        if b == b'=' {
            break;
        }
        let val = table[b as usize];
        if val == 255 {
            continue;
        }
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}
