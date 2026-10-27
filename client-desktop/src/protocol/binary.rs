//! Binary WebSocket messages: voice, video, file chunks and the multi-session tag.
//!
//! ```text
//! voice:  [0x02][rate_khz][pcm16le mono...]
//! video:  [0xFF 0xD8 ...]  a JPEG, or a framed key/delta/H.264 message (core_engine::codec::frame)
//! tagged: [0x10][9 ASCII digits: host clean ID][voice or video]   (multi-session viewers only)
//! monitor, host -> relay:  [0x11][JPEG]                            (monitor-wall thumbnail)
//! monitor, relay -> admin: [0x11][9 ASCII digits: host clean ID][JPEG]
//! file chunk:              [0x12][id length][id][offset u64 big endian][data...]   (`file-binary` only)
//! file chunk, relay -> client: [0x10][9 ASCII digits: sender clean ID][file chunk] (always tagged)
//! ```

/// First byte of a voice packet.
pub const AUDIO_MARKER: u8 = 0x02;
/// First byte of a session-tagged message.
pub const SESSION_TAG: u8 = 0x10;
/// Tag byte plus the 9-digit host ID.
pub const SESSION_TAG_LEN: usize = 10;
/// First byte of a monitor-wall thumbnail, in both directions.
pub const MONITOR_TAG: u8 = 0x11;
/// First byte of a binary file chunk.
pub const FILE_CHUNK: u8 = 0x12;
/// Longest transfer ID the relay accepts.
const MAX_FILE_ID: usize = 80;
/// Marker, ID length and offset.
const FILE_CHUNK_HEADER: usize = 1 + 1 + 8;

/// One binary file chunk: `data` belongs at byte `offset` of transfer `id`.
#[derive(Debug, PartialEq, Eq)]
pub struct FileChunk<'a> {
    pub id: &'a str,
    pub offset: u64,
    pub data: &'a [u8],
}

/// Encodes a file chunk. `None` if `id` is empty or longer than the relay accepts.
pub fn file_chunk(id: &str, offset: u64, data: &[u8]) -> Option<Vec<u8>> {
    if id.is_empty() || id.len() > MAX_FILE_ID {
        return None;
    }
    let mut out = Vec::with_capacity(FILE_CHUNK_HEADER + id.len() + data.len());
    out.push(FILE_CHUNK);
    out.push(id.len() as u8);
    out.extend_from_slice(id.as_bytes());
    out.extend_from_slice(&offset.to_be_bytes());
    out.extend_from_slice(data);
    Some(out)
}

/// Decodes an untagged file chunk. `None` for anything else or a malformed header.
pub fn parse_file_chunk(bin: &[u8]) -> Option<FileChunk<'_>> {
    if bin.len() < FILE_CHUNK_HEADER || bin[0] != FILE_CHUNK {
        return None;
    }
    let n = bin[1] as usize;
    if n == 0 || n > MAX_FILE_ID || bin.len() < FILE_CHUNK_HEADER + n {
        return None;
    }
    let id = std::str::from_utf8(&bin[2..2 + n]).ok()?;
    let offset = u64::from_be_bytes(bin[2 + n..10 + n].try_into().ok()?);
    Some(FileChunk { id, offset, data: &bin[10 + n..] })
}

/// True for a voice packet.
pub fn is_audio(bin: &[u8]) -> bool {
    bin.len() > 1 && bin[0] == AUDIO_MARKER
}

fn valid_host_id(id: &str) -> bool {
    id.len() == SESSION_TAG_LEN - 1 && id.bytes().all(|b| b.is_ascii_digit())
}

/// Prefixes a viewer -> host message with the session tag. `None` if `host` is not a clean ID.
pub fn tag(host: &str, payload: &[u8]) -> Option<Vec<u8>> {
    if !valid_host_id(host) {
        return None;
    }
    let mut out = Vec::with_capacity(SESSION_TAG_LEN + payload.len());
    out.push(SESSION_TAG);
    out.extend_from_slice(host.as_bytes());
    out.extend_from_slice(payload);
    Some(out)
}

/// Splits a host -> viewer message into (host ID, payload). Untagged voice and video never start with the
/// tag byte, so `None` means "untagged: the single session".
pub fn untag(bin: &[u8]) -> Option<(&str, &[u8])> {
    if bin.len() <= SESSION_TAG_LEN || bin[0] != SESSION_TAG {
        return None;
    }
    let id = std::str::from_utf8(&bin[1..SESSION_TAG_LEN]).ok()?;
    valid_host_id(id).then(|| (id, &bin[SESSION_TAG_LEN..]))
}

/// True for a monitor-wall thumbnail; it never belongs to a viewer session.
pub fn is_monitor(bin: &[u8]) -> bool {
    bin.first() == Some(&MONITOR_TAG)
}

/// Prefixes a JPEG with the monitor tag (host -> relay).
pub fn monitor_frame(jpeg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + jpeg.len());
    out.push(MONITOR_TAG);
    out.extend_from_slice(jpeg);
    out
}

/// Splits a relay -> administrator thumbnail into (host ID, JPEG).
#[cfg(feature = "admin")]
pub fn untag_monitor(bin: &[u8]) -> Option<(&str, &[u8])> {
    if bin.len() <= SESSION_TAG_LEN || bin[0] != MONITOR_TAG {
        return None;
    }
    let id = std::str::from_utf8(&bin[1..SESSION_TAG_LEN]).ok()?;
    valid_host_id(id).then(|| (id, &bin[SESSION_TAG_LEN..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_round_trip_and_untagged_payloads_pass_through() {
        let audio = [AUDIO_MARKER, 48, 1, 2];
        let tagged = tag("222222222", &audio).unwrap();
        assert_eq!(untag(&tagged), Some(("222222222", &audio[..])));
        assert_eq!(untag(&audio), None);
        assert_eq!(untag(&[0xFF, 0xD8, 0, 0, 0, 0, 0, 0, 0, 0, 0]), None);
        assert!(tag("12345", &audio).is_none());
        assert!(is_audio(&audio) && !is_audio(&[AUDIO_MARKER]));
    }

    #[test]
    fn file_chunks_round_trip_and_reject_malformed_headers() {
        let bin = file_chunk("ft_1", 1 << 40, b"abc").unwrap();
        assert_eq!(&bin[..6], &[FILE_CHUNK, 4, b'f', b't', b'_', b'1']);
        assert_eq!(parse_file_chunk(&bin), Some(FileChunk { id: "ft_1", offset: 1 << 40, data: b"abc" }));
        let tagged = tag("111111111", &bin).unwrap();
        let (from, inner) = untag(&tagged).unwrap();
        assert_eq!((from, parse_file_chunk(inner).unwrap().id), ("111111111", "ft_1"));
        assert_eq!(parse_file_chunk(&file_chunk("x", 0, b"").unwrap()).unwrap().data, b"");
        assert!(file_chunk("", 0, b"x").is_none() && file_chunk(&"a".repeat(81), 0, b"x").is_none());
        assert!(parse_file_chunk(&bin[..10]).is_none(), "truncated offset");
        assert!(parse_file_chunk(&[FILE_CHUNK, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_none(), "empty id");
        assert!(parse_file_chunk(&[AUDIO_MARKER, 48, 1, 2]).is_none() && !is_audio(&bin) && !is_monitor(&bin));
    }

    #[test]
    fn monitor_frames_are_never_session_traffic() {
        let jpeg = [0xFF, 0xD8, 0xFF, 1];
        let out = monitor_frame(&jpeg);
        assert!(is_monitor(&out) && !is_audio(&out) && untag(&out).is_none());
        assert!(!is_monitor(&jpeg));
        #[cfg(feature = "admin")]
        {
            let mut tagged = vec![MONITOR_TAG];
            tagged.extend_from_slice(b"222222222");
            tagged.extend_from_slice(&jpeg);
            assert_eq!(untag_monitor(&tagged), Some(("222222222", &jpeg[..])));
            assert_eq!(untag_monitor(&out), None);
        }
    }
}
