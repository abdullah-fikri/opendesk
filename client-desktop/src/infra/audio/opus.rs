//! Opus voice packets: `[0x02][0x4F][seq u16 big-endian][Opus packet]`, one 20 ms mono frame each.
//!
//! The codec is libopus 1.3.1 translated to Rust (`unsafe-libopus`, BSD-3-Clause): no C toolchain, no DLL.
//! The encoder runs at the capture rate (16, 24 or 48 kHz) with in-band FEC; the decoder always outputs 48 kHz.
//! The sequence number lets the decoder recover a lost frame from the next packet's FEC data and conceal
//! short gaps (PLC).
// The encoder is only exercised by tests until a sender negotiates Opus.
#![cfg_attr(not(test), allow(dead_code))]

use std::ptr;

use unsafe_libopus::varargs::{VarArg, VarArgs};
use unsafe_libopus::{
    OPUS_APPLICATION_VOIP, OPUS_OK, OPUS_SET_BITRATE_REQUEST, OPUS_SET_INBAND_FEC_REQUEST,
    OPUS_SET_PACKET_LOSS_PERC_REQUEST, OPUS_SET_SIGNAL_REQUEST, OPUS_SIGNAL_VOICE, OpusDecoder, OpusEncoder,
    opus_decode, opus_decoder_create, opus_decoder_destroy, opus_encode, opus_encoder_create, opus_encoder_ctl_impl,
    opus_encoder_destroy,
};

/// Second byte of an Opus voice packet; PCM packets carry the rate there (16, 24 or 48).
pub const OPUS_TAG: u8 = 0x4F;
/// Decoded audio is always 48 kHz mono.
pub const DECODE_RATE: u32 = 48_000;
/// 20 ms at 48 kHz.
const DECODE_FRAME: usize = 960;
/// Largest Opus packet duration (120 ms at 48 kHz).
const MAX_DECODE_SAMPLES: usize = 5760;
const HEADER_LEN: usize = 4;
const MAX_PACKET: usize = 1275;
const BITRATE: i32 = 28_000;
const EXPECTED_LOSS_PERCENT: i32 = 5;
/// Longer gaps are a new stream (sender restarted, call resumed), not loss to conceal.
const MAX_CONCEALED_FRAMES: u16 = 3;

/// Splits an Opus voice packet into (seq, Opus payload); `None` for PCM or malformed packets.
pub fn parse(bin: &[u8]) -> Option<(u16, &[u8])> {
    if bin.len() <= HEADER_LEN || bin[0] != crate::protocol::binary::AUDIO_MARKER || bin[1] != OPUS_TAG {
        return None;
    }
    Some((u16::from_be_bytes([bin[2], bin[3]]), &bin[HEADER_LEN..]))
}

/// Mono VoIP encoder producing complete voice packets.
pub struct Encoder {
    st: *mut OpusEncoder,
    seq: u16,
    samples: Vec<i16>,
    out: [u8; MAX_PACKET],
}

// The encoder state is owned and used by one thread at a time.
unsafe impl Send for Encoder {}

impl Encoder {
    /// Opens an encoder for `rate` Hz (8, 12, 16, 24 or 48 kHz).
    pub fn new(rate: u32) -> Option<Self> {
        let mut err = 0;
        let st = unsafe { opus_encoder_create(rate as i32, 1, OPUS_APPLICATION_VOIP, &mut err) };
        if st.is_null() || err != OPUS_OK {
            return None;
        }
        let enc = Self { st, seq: 0, samples: Vec::new(), out: [0; MAX_PACKET] };
        for (request, value) in [
            (OPUS_SET_BITRATE_REQUEST, BITRATE),
            (OPUS_SET_INBAND_FEC_REQUEST, 1),
            (OPUS_SET_PACKET_LOSS_PERC_REQUEST, EXPECTED_LOSS_PERCENT),
            (OPUS_SET_SIGNAL_REQUEST, OPUS_SIGNAL_VOICE),
        ] {
            unsafe { opus_encoder_ctl_impl(enc.st, request, VarArgs::new(vec![VarArg::I32(value)])) };
        }
        Some(enc)
    }

    /// Encodes one frame of 16-bit little-endian PCM (2.5 to 60 ms at the encoder's rate) into a voice packet.
    pub fn encode(&mut self, pcm_le: &[u8]) -> Option<Vec<u8>> {
        self.samples.clear();
        self.samples.extend(pcm_le.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
        let n = unsafe {
            opus_encode(
                self.st,
                self.samples.as_ptr(),
                self.samples.len() as i32,
                self.out.as_mut_ptr(),
                MAX_PACKET as i32,
            )
        };
        if n <= 0 {
            return None;
        }
        let mut pkt = Vec::with_capacity(HEADER_LEN + n as usize);
        pkt.extend_from_slice(&[crate::protocol::binary::AUDIO_MARKER, OPUS_TAG]);
        pkt.extend_from_slice(&self.seq.to_be_bytes());
        pkt.extend_from_slice(&self.out[..n as usize]);
        self.seq = self.seq.wrapping_add(1);
        Some(pkt)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { opus_encoder_destroy(self.st) };
    }
}

/// 48 kHz mono decoder with FEC recovery and loss concealment.
pub struct Decoder {
    st: *mut OpusDecoder,
    next_seq: Option<u16>,
    pcm: Vec<i16>,
}

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Option<Self> {
        let mut err = 0;
        let st = unsafe { opus_decoder_create(DECODE_RATE as i32, 1, &mut err) };
        if st.is_null() || err != OPUS_OK {
            return None;
        }
        Some(Self { st, next_seq: None, pcm: vec![0; MAX_DECODE_SAMPLES] })
    }

    /// Decodes packet `seq` into 48 kHz 16-bit little-endian PCM, 20 ms per item: concealed or FEC-recovered
    /// frames for a short gap first, then the packet itself. Empty if the packet is corrupt.
    pub fn decode(&mut self, seq: u16, payload: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let lost = self.next_seq.map_or(0, |next| seq.wrapping_sub(next));
        if (1..=MAX_CONCEALED_FRAMES).contains(&lost) {
            for _ in 1..lost {
                self.run(&[], DECODE_FRAME, false, &mut frames);
            }
            // The frame just before this packet is rebuilt from the packet's FEC data.
            self.run(payload, DECODE_FRAME, true, &mut frames);
        }
        self.next_seq = Some(seq.wrapping_add(1));
        self.run(payload, MAX_DECODE_SAMPLES, false, &mut frames);
        frames
    }

    /// One `opus_decode` call; an empty `data` asks for packet loss concealment.
    fn run(&mut self, data: &[u8], frame: usize, fec: bool, frames: &mut Vec<Vec<u8>>) {
        let data_ptr = if data.is_empty() { ptr::null() } else { data.as_ptr() };
        let n = unsafe {
            opus_decode(self.st, data_ptr, data.len() as i32, self.pcm.as_mut_ptr(), frame as i32, fec as i32)
        };
        if n <= 0 {
            return;
        }
        for chunk in self.pcm[..n as usize].chunks(DECODE_FRAME) {
            frames.push(chunk.iter().flat_map(|s| s.to_le_bytes()).collect());
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { opus_decoder_destroy(self.st) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 20 ms of a 440 Hz tone (with a slow 3 Hz amplitude swell, like speech syllables) at `rate`.
    fn voice_frame(rate: u32, index: usize) -> Vec<u8> {
        let len = rate as usize / 50;
        (0..len)
            .flat_map(|i| {
                let t = (index * len + i) as f32 / rate as f32;
                let env = 0.5 + 0.5 * (std::f32::consts::TAU * 3.0 * t).sin();
                let s = (std::f32::consts::TAU * 440.0 * t).sin() * env * 12_000.0;
                (s as i16).to_le_bytes()
            })
            .collect()
    }

    fn rms(pcm: &[u8]) -> f64 {
        let n = pcm.len() / 2;
        let sum: f64 = pcm.chunks_exact(2).map(|b| f64::from(i16::from_le_bytes([b[0], b[1]])).powi(2)).sum();
        (sum / n as f64).sqrt()
    }

    #[test]
    fn round_trip_keeps_length_and_energy() {
        for rate in [16_000, 24_000, 48_000] {
            let mut enc = Encoder::new(rate).unwrap();
            let mut dec = Decoder::new().unwrap();
            let (mut input, mut output) = (Vec::new(), Vec::new());
            for i in 0..50 {
                let frame = voice_frame(rate, i);
                let pkt = enc.encode(&frame).unwrap();
                let (seq, payload) = parse(&pkt).unwrap();
                assert_eq!(seq, i as u16);
                let decoded = dec.decode(seq, payload);
                assert_eq!(decoded.len(), 1);
                assert_eq!(decoded[0].len(), DECODE_FRAME * 2);
                input.extend(frame);
                output.extend(decoded.concat());
            }
            // Skip the codec's start-up delay; the energy must come through within a few dB.
            let ratio = rms(&output[DECODE_FRAME * 4..]) / rms(&input[(rate as usize / 25)..]);
            assert!((0.6..1.6).contains(&ratio), "rate {rate}: energy ratio {ratio}");
        }
    }

    #[test]
    fn gaps_are_concealed_and_restarts_are_not() {
        let mut enc = Encoder::new(48_000).unwrap();
        let mut dec = Decoder::new().unwrap();
        let pkts: Vec<Vec<u8>> = (0..20).map(|i| enc.encode(&voice_frame(48_000, i)).unwrap()).collect();
        let mut play = |i: usize| {
            let (seq, payload) = parse(&pkts[i]).unwrap();
            dec.decode(seq, payload).len()
        };
        assert_eq!(play(0), 1);
        // One lost packet: rebuilt from FEC, then the packet itself.
        assert_eq!(play(2), 2);
        // Two lost: one concealed, one from FEC, then the packet.
        assert_eq!(play(5), 3);
        // A long jump is a new stream.
        assert_eq!(play(15), 1);
    }

    #[test]
    fn pcm_packets_are_not_opus() {
        assert!(parse(&[0x02, 48, 0, 0, 1, 2]).is_none());
        assert!(parse(&[0x02, OPUS_TAG, 0, 0]).is_none());
        assert_eq!(parse(&[0x02, OPUS_TAG, 1, 2, 9]), Some((0x0102, &[9u8][..])));
    }

    /// Bytes per second for 10 s of synthetic voice at 48 kHz: Opus vs PCM (printed with `--nocapture`).
    #[test]
    fn opus_is_far_smaller_than_pcm() {
        let mut enc = Encoder::new(48_000).unwrap();
        let frames = 500;
        let (mut opus, mut pcm) = (0usize, 0usize);
        for i in 0..frames {
            let frame = voice_frame(48_000, i);
            pcm += 2 + frame.len();
            opus += enc.encode(&frame).unwrap().len();
        }
        let secs = frames as f64 / 50.0;
        println!("voice 10 s @48 kHz: opus {:.0} B/s, pcm {:.0} B/s", opus as f64 / secs, pcm as f64 / secs);
        assert!(opus * 10 < pcm);
    }
}
