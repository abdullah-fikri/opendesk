//! H.264 through Cisco's prebuilt OpenH264 library, loaded at runtime. Only Cisco's own binary is accepted
//! (the `openh264` crate checks its SHA-256): that binary is covered by Cisco's H.264 patent license, which a
//! self-compiled copy is not, so OpenDesk never ships or builds the codec itself. Without the library both
//! constructors fail and the stream stays on JPEG (see `packaging/linux/install-openh264.sh`).

use std::fmt::Display;
use std::path::PathBuf;
use std::sync::OnceLock;

use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, RateControlMode, UsageType};
use openh264::formats::YUVSource;
use openh264_sys2::{ENCODER_OPTION_BITRATE, SBitrateInfo, SPATIAL_LAYER_ALL};

use super::{bgra_to_nv12, i420_to_rgba};

/// Codec error (library missing, rejected or failing).
pub type Error = Box<dyn std::error::Error + Send + Sync>;
type Result<T> = std::result::Result<T, Error>;

fn err(e: impl Display) -> Error {
    e.to_string().into()
}

/// File name of Cisco's OpenH264 2.6.0 binary for this CPU (download it from ciscobinary.openh264.org).
#[cfg(target_arch = "x86_64")]
pub const CISCO_LIBRARY: &str = "libopenh264-2.6.0-linux64.8.so";
/// File name of Cisco's OpenH264 2.6.0 binary for this CPU (download it from ciscobinary.openh264.org).
#[cfg(target_arch = "aarch64")]
pub const CISCO_LIBRARY: &str = "libopenh264-2.6.0-linux-arm64.8.so";
/// File name of Cisco's OpenH264 2.6.0 binary for this CPU (download it from ciscobinary.openh264.org).
#[cfg(target_arch = "arm")]
pub const CISCO_LIBRARY: &str = "libopenh264-2.6.0-linux-arm.8.so";
/// File name of Cisco's OpenH264 2.6.0 binary for this CPU (download it from ciscobinary.openh264.org).
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "arm")))]
pub const CISCO_LIBRARY: &str = "libopenh264-2.6.0-linux32.8.so";

/// Where the library is looked for, in order: `OPENDESK_OPENH264` (a file), the user's data directory,
/// next to the executable, then system-wide OpenDesk directories.
fn candidates() -> Vec<PathBuf> {
    let mut list = Vec::new();
    if let Some(p) = std::env::var_os("OPENDESK_OPENH264") {
        list.push(PathBuf::from(p));
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    if let Some(d) = data {
        list.push(d.join("opendesk").join(CISCO_LIBRARY));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)) {
        list.push(dir.join(CISCO_LIBRARY));
        list.push(dir.join("../lib/opendesk").join(CISCO_LIBRARY));
    }
    for dir in ["/usr/lib/opendesk", "/usr/local/lib/opendesk", "/opt/opendesk/lib"] {
        list.push(PathBuf::from(dir).join(CISCO_LIBRARY));
    }
    list
}

/// Path of a verified Cisco library, searched once per process.
pub fn library_path() -> Option<PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| candidates().into_iter().find(|p| p.is_file() && OpenH264API::from_blob_path(p).is_ok()))
        .clone()
}

fn api() -> Result<OpenH264API> {
    let path = library_path().ok_or_else(|| err(format!("{CISCO_LIBRARY} not installed")))?;
    OpenH264API::from_blob_path(path).map_err(err)
}

/// Borrowed planar I420 picture.
struct I420<'a> {
    data: &'a [u8],
    width: usize,
    height: usize,
}

impl YUVSource for I420<'_> {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }

    fn y(&self) -> &[u8] {
        &self.data[..self.width * self.height]
    }

    fn u(&self) -> &[u8] {
        let n = self.width * self.height;
        &self.data[n..n + n / 4]
    }

    fn v(&self) -> &[u8] {
        let n = self.width * self.height;
        &self.data[n + n / 4..]
    }
}

/// Software H.264 encoder (OpenH264, screen-content mode, constant bitrate).
pub struct H264Encoder {
    /// Encoded width.
    pub width: u32,
    /// Encoded height.
    pub height: u32,
    /// Name of the encoder, for logs.
    pub name: String,
    /// Always false: OpenH264 is a software encoder.
    pub hardware: bool,
    enc: Encoder,
    bitrate: u32,
    fps: u32,
    frames: u64,
    nv12: Vec<u8>,
    i420: Vec<u8>,
}

impl H264Encoder {
    /// An encoder for `width` x `height` (both even). `allow_hw` is ignored: there is no hardware path here.
    pub fn new(width: u32, height: u32, fps: u32, bitrate: u32, _allow_hw: bool) -> Result<Self> {
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(err("odd frame size"));
        }
        Ok(Self {
            width,
            height,
            name: format!("Cisco OpenH264 2.6.0 (software, {CISCO_LIBRARY})"),
            hardware: false,
            enc: Self::create(bitrate, fps)?,
            bitrate,
            fps,
            frames: 0,
            nv12: Vec::new(),
            i420: Vec::new(),
        })
    }

    /// Screen-content, constant-bitrate encoder; it initializes itself on the first frame.
    fn create(bitrate: u32, fps: u32) -> Result<Encoder> {
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bitrate))
            .max_frame_rate(FrameRate::from_hz(fps.max(1) as f32))
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .skip_frames(false);
        Encoder::with_api_config(api()?, config).map_err(err)
    }

    /// Current target bitrate in bits per second.
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Changes the target bitrate for later frames.
    pub fn set_bitrate(&mut self, bps: u32) {
        self.bitrate = bps;
        if self.frames == 0 {
            // Not initialized yet: the first encode would apply the old configuration, so rebuild it.
            if let Ok(enc) = Self::create(bps, self.fps) {
                self.enc = enc;
            }
            return;
        }
        let mut info = SBitrateInfo { iLayer: SPATIAL_LAYER_ALL, iBitrate: bps.min(i32::MAX as u32) as i32 };
        // SAFETY: ENCODER_OPTION_BITRATE takes a pointer to SBitrateInfo, valid for the call.
        unsafe {
            self.enc.raw_api().set_option(ENCODER_OPTION_BITRATE, (&raw mut info).cast());
        }
    }

    /// Makes the next encoded frame an IDR.
    pub fn force_keyframe(&mut self) {
        // The first frame is an IDR anyway, and the encoder only accepts the request once initialized.
        if self.frames > 0 {
            self.enc.force_intra_frame();
        }
    }

    /// Nothing is queued in a synchronous encoder.
    pub fn discard_pending(&mut self) {}

    /// Encodes one BGRX frame (`stride` in pixels). Returns the Annex-B access unit and whether it is an IDR,
    /// or None when the encoder skipped the frame.
    pub fn encode(&mut self, bgra: &[u32], stride: usize) -> Result<Option<(Vec<u8>, bool)>> {
        let (w, h) = (self.width as usize, self.height as usize);
        bgra_to_nv12(bgra, stride, w, h, &mut self.nv12);
        let (ys, uv) = self.nv12.split_at(w * h);
        self.i420.clear();
        self.i420.extend_from_slice(ys);
        self.i420.extend(uv.iter().step_by(2));
        self.i420.extend(uv.iter().skip(1).step_by(2));
        let pic = I420 { data: &self.i420, width: w, height: h };
        let stream = self.enc.encode(&pic).map_err(err)?;
        self.frames += 1;
        let idr = match stream.frame_type() {
            FrameType::Skip | FrameType::Invalid => return Ok(None),
            t => t == FrameType::IDR,
        };
        let au = stream.to_vec();
        Ok((!au.is_empty()).then_some((au, idr)))
    }
}

/// Software H.264 decoder (OpenH264, no reordering delay) producing RGBA.
pub struct H264Decoder {
    dec: Decoder,
    rgba: Vec<u8>,
}

impl H264Decoder {
    /// Creates a decoder; the stream size comes from its SPS.
    pub fn new(_width: u32, _height: u32) -> Result<Self> {
        let dec = Decoder::with_api_config(api()?, DecoderConfig::new()).map_err(err)?;
        Ok(Self { dec, rgba: Vec::new() })
    }

    /// Decodes one access unit; returns RGBA cropped to `width` x `height` when a picture came out.
    pub fn decode(&mut self, au: &[u8], width: u32, height: u32) -> Result<Option<&[u8]>> {
        let mut out = std::mem::take(&mut self.rgba);
        let got = self.decode_into(au, width, height, &mut out);
        self.rgba = out;
        Ok(if got? { Some(&self.rgba) } else { None })
    }

    /// Like `decode` but converts straight into `out` (e.g. the viewer framebuffer).
    pub fn decode_into(&mut self, au: &[u8], width: u32, height: u32, out: &mut Vec<u8>) -> Result<bool> {
        let Some(yuv) = self.dec.decode(au).map_err(err)? else { return Ok(false) };
        let (dw, dh) = yuv.dimensions();
        let (ys, us, _) = yuv.strides();
        let (w, h) = ((width as usize).min(dw), (height as usize).min(dh));
        i420_to_rgba(yuv.y(), yuv.u(), yuv.v(), ys, us, w, h, out);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::h264::test_pattern as pattern;

    /// Runs only where Cisco's library is installed (see `packaging/linux/install-openh264.sh`).
    #[test]
    fn software_encode_decode_roundtrip() {
        if library_path().is_none() {
            return;
        }
        let (w, h) = (320u32, 192u32);
        let mut enc = H264Encoder::new(w, h, 30, 2_000_000, false).unwrap();
        let mut dec = H264Decoder::new(w, h).unwrap();
        let mut shown = 0;
        for t in 0..10 {
            let frame = pattern(w as usize, h as usize, t);
            if t == 5 {
                enc.set_bitrate(1_000_000);
                enc.force_keyframe();
            }
            if let Some((au, idr)) = enc.encode(&frame, w as usize).unwrap() {
                assert_eq!(idr, t == 0 || t == 5, "frame {t}");
                if let Some(rgba) = dec.decode(&au, w, h).unwrap() {
                    assert_eq!(rgba.len(), (w * h * 4) as usize);
                    shown += 1;
                }
            }
        }
        assert!(shown >= 8, "low-latency decode should not buffer ({shown} of 10 shown)");
    }
}
