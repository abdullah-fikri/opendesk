//! H.264 through Media Foundation: hardware encoder MFT when available (NVENC/QSV/AMF expose async MFTs),
//! the Microsoft software MFT otherwise; the viewer decodes with the Microsoft H.264 decoder MFT.
//! Colour conversion is BT.709 limited range on both ends, done on the CPU.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::VARIANT_BOOL;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_UI4};
use windows::core::{GUID, Interface};

type Result<T> = windows::core::Result<T>;

fn fail() -> windows::core::Error {
    windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)
}

/// COM (MTA) for the calling thread plus process-wide MFStartup.
fn ensure_mf() -> Result<()> {
    static STARTED: OnceLock<bool> = OnceLock::new();
    unsafe {
        // S_FALSE / RPC_E_CHANGED_MODE are fine: COM is usable on this thread either way.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    if *STARTED.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL).is_ok() }) { Ok(()) } else { Err(fail()) }
}

fn var_u32(v: u32) -> VARIANT {
    let mut x = VARIANT::default();
    unsafe {
        let inner = &mut *x.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = v;
    }
    x
}

fn var_bool(v: bool) -> VARIANT {
    let mut x = VARIANT::default();
    unsafe {
        let inner = &mut *x.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = VARIANT_BOOL(if v { -1 } else { 0 });
    }
    x
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn video_type(subtype: &GUID, w: u32, h: u32, fps: u32) -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        Ok(t)
    }
}

fn sample_from(bytes: &[u8], time: i64, duration: i64) -> Result<IMFSample> {
    unsafe {
        let buf = MFCreateMemoryBuffer(bytes.len() as u32)?;
        let mut p = std::ptr::null_mut();
        buf.Lock(&mut p, None, None)?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        buf.Unlock()?;
        buf.SetCurrentLength(bytes.len() as u32)?;
        let s = MFCreateSample()?;
        s.AddBuffer(&buf)?;
        s.SetSampleTime(time)?;
        s.SetSampleDuration(duration)?;
        Ok(s)
    }
}

fn sample_bytes(s: &IMFSample) -> Result<Vec<u8>> {
    unsafe {
        let buf = s.ConvertToContiguousBuffer()?;
        let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
        buf.Lock(&mut p, None, Some(&mut len))?;
        let v = std::slice::from_raw_parts(p, len as usize).to_vec();
        buf.Unlock()?;
        Ok(v)
    }
}

/// Runs ProcessOutput once; allocates the output sample when the MFT does not provide its own.
fn process_output(mft: &IMFTransform) -> Result<IMFSample> {
    unsafe {
        let info = mft.GetOutputStreamInfo(0)?;
        let provides = (info.dwFlags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32)
            != 0;
        let own = if provides {
            None
        } else {
            let s = MFCreateSample()?;
            s.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(1 << 20))?)?;
            Some(s)
        };
        let mut out = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(own),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        let r = mft.ProcessOutput(0, &mut out, &mut status);
        let sample = std::mem::ManuallyDrop::take(&mut out[0].pSample);
        drop(std::mem::ManuallyDrop::take(&mut out[0].pEvents));
        r?;
        sample.ok_or_else(fail)
    }
}

/// NAL unit types present in an Annex-B access unit.
pub fn nal_types(au: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 3 < au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            types.push(au[i + 3] & 0x1F);
            i += 3;
        } else {
            i += 1;
        }
    }
    types
}

/// BT.709 limited-range BGRX -> NV12 (2x2 chroma average). `w`, `h` must be even.
pub fn bgra_to_nv12(src: &[u32], stride: usize, w: usize, h: usize, dst: &mut Vec<u8>) {
    dst.resize(w * h * 3 / 2, 0);
    let (ys, uv) = dst.split_at_mut(w * h);
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            let (mut rs, mut gs, mut bs) = (0i32, 0i32, 0i32);
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let p = src[(y + dy) * stride + x + dx];
                let (r, g, b) = (((p >> 16) & 0xFF) as i32, ((p >> 8) & 0xFF) as i32, (p & 0xFF) as i32);
                ys[(y + dy) * w + x + dx] = ((47 * r + 157 * g + 16 * b + 128) >> 8).wrapping_add(16) as u8;
                rs += r;
                gs += g;
                bs += b;
            }
            let (r, g, b) = (rs / 4, gs / 4, bs / 4);
            uv[(y / 2) * w + x] = (((-26 * r - 87 * g + 112 * b + 128) >> 8) + 128) as u8;
            uv[(y / 2) * w + x + 1] = (((112 * r - 102 * g - 10 * b + 128) >> 8) + 128) as u8;
        }
    }
}

/// BT.709 limited-range NV12 -> RGBA, cropping to `w` x `h`. `uv` starts the chroma plane; both use `pitch`.
pub fn nv12_to_rgba(yp: &[u8], uv: &[u8], pitch: usize, w: usize, h: usize, out: &mut Vec<u8>) {
    out.resize(w * h * 4, 0);
    for (y, orow) in out.chunks_exact_mut(w * 4).enumerate() {
        let yrow = &yp[y * pitch..y * pitch + w];
        let crow = &uv[(y / 2) * pitch..(y / 2) * pitch + w];
        // Two pixels share one chroma pair: compute its contribution once.
        for ((px, ys), c) in orow.chunks_exact_mut(8).zip(yrow.chunks_exact(2)).zip(crow.chunks_exact(2)) {
            let (u, v) = (c[0] as i32 - 128, c[1] as i32 - 128);
            let (rv, guv, bu) = (459 * v + 128, -55 * u - 136 * v + 128, 541 * u + 128);
            for (o, &yv) in px.chunks_exact_mut(4).zip(ys) {
                let l = 298 * (yv as i32 - 16);
                o[0] = ((l + rv) >> 8).clamp(0, 255) as u8;
                o[1] = ((l + guv) >> 8).clamp(0, 255) as u8;
                o[2] = ((l + bu) >> 8).clamp(0, 255) as u8;
                o[3] = 255;
            }
        }
    }
}

/// H.264 encoder (Media Foundation transform), hardware when available.
pub struct H264Encoder {
    /// Encoded width.
    pub width: u32,
    /// Encoded height.
    pub height: u32,
    /// Name of the transform, for logs.
    pub name: String,
    /// True for a hardware encoder.
    pub hardware: bool,
    mft: IMFTransform,
    events: Option<IMFMediaEventGenerator>,
    codec: Option<ICodecAPI>,
    /// NeedInput events not yet answered (async MFTs only).
    credits: u32,
    fps: u32,
    frame_index: i64,
    bitrate: u32,
    sequence_header: Vec<u8>,
    nv12: Vec<u8>,
}

unsafe impl Send for H264Encoder {}

impl H264Encoder {
    /// Hardware MFT first (unless `allow_hw` is false), then the Microsoft software encoder.
    pub fn new(width: u32, height: u32, fps: u32, bitrate: u32, allow_hw: bool) -> Result<Self> {
        ensure_mf()?;
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(fail());
        }
        if allow_hw && let Ok(list) = Self::hardware_mfts() {
            for (mft, name) in list {
                if let Ok(e) = Self::configure(mft, name, true, width, height, fps, bitrate) {
                    return Ok(e);
                }
            }
        }
        let mft: IMFTransform = unsafe { CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER)? };
        Self::configure(mft, "Microsoft H.264 (software)".into(), false, width, height, fps, bitrate)
    }

    fn hardware_mfts() -> Result<Vec<(IMFTransform, String)>> {
        unsafe {
            let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
            let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
            let mut arr: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut n = 0u32;
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut arr,
                &mut n,
            )?;
            let mut out = Vec::new();
            for i in 0..n as usize {
                if let Some(act) = std::ptr::read(arr.add(i)) {
                    let mut name = String::from("hardware H.264");
                    let mut p = windows::core::PWSTR::null();
                    let mut len = 0;
                    if act.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut p, &mut len).is_ok() {
                        name = p.to_string().unwrap_or(name);
                        CoTaskMemFree(Some(p.0 as *const _));
                    }
                    if let Ok(mft) = act.ActivateObject::<IMFTransform>() {
                        out.push((mft, name));
                    }
                }
            }
            CoTaskMemFree(Some(arr as *const _));
            Ok(out)
        }
    }

    fn configure(
        mft: IMFTransform,
        name: String,
        hardware: bool,
        w: u32,
        h: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<Self> {
        unsafe {
            let mut events = None;
            if let Ok(attrs) = mft.GetAttributes() {
                if attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0 {
                    attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
                    events = Some(mft.cast::<IMFMediaEventGenerator>()?);
                }
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let codec = mft.cast::<ICodecAPI>().ok();
            if let Some(c) = &codec {
                // Settings an encoder does not support are skipped; low latency + CBR matter most.
                let _ = c.SetValue(&CODECAPI_AVLowLatencyMode, &var_bool(true));
                let _ = c
                    .SetValue(&CODECAPI_AVEncCommonRateControlMode, &var_u32(eAVEncCommonRateControlMode_CBR.0 as u32));
                let _ = c.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &var_u32(bitrate));
                let _ = c.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &var_u32(0));
                // Long GOP: key frames only on demand (start, viewer request), P-frames otherwise.
                let _ = c.SetValue(&CODECAPI_AVEncMPVGOPSize, &var_u32(fps * 60));
            }
            let out_t = video_type(&MFVideoFormat_H264, w, h, fps)?;
            out_t.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            out_t.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;
            mft.SetOutputType(0, &out_t, 0)?;
            let in_t = video_type(&MFVideoFormat_NV12, w, h, fps)?;
            mft.SetInputType(0, &in_t, 0)?;
            let mut sequence_header = Vec::new();
            if let Ok(cur) = mft.GetOutputCurrentType(0)
                && let Ok(n) = cur.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER)
            {
                sequence_header.resize(n as usize, 0);
                if cur.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut sequence_header, None).is_err() {
                    sequence_header.clear();
                }
            }
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(Self {
                width: w,
                height: h,
                name,
                hardware,
                mft,
                events,
                codec,
                credits: 0,
                fps,
                frame_index: 0,
                bitrate,
                sequence_header,
                nv12: Vec::new(),
            })
        }
    }

    /// Current target bitrate in bits per second.
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Changes the CBR target on the fly (adaptive quality); ignored by encoders that cannot.
    pub fn set_bitrate(&mut self, bps: u32) {
        if let Some(c) = &self.codec
            && unsafe { c.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &var_u32(bps)) }.is_ok()
        {
            self.bitrate = bps;
        }
    }

    /// Next encoded frame is an IDR.
    pub fn force_keyframe(&mut self) {
        if let Some(c) = &self.codec {
            unsafe {
                let _ = c.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &var_u32(1));
            }
        }
    }

    /// Drops output still queued inside an async MFT (stale frames after leaving H.264 mode).
    pub fn discard_pending(&mut self) {
        let _ = self.pump(Duration::ZERO, false);
    }

    /// Encodes one frame. Returns (Annex-B access unit, is_idr) when the encoder produced output.
    pub fn encode(&mut self, bgra: &[u32], stride: usize) -> Result<Option<(Vec<u8>, bool)>> {
        // The encoder may have been created on another thread; this one needs COM too.
        ensure_mf()?;
        let mut nv12 = std::mem::take(&mut self.nv12);
        bgra_to_nv12(bgra, stride, self.width as usize, self.height as usize, &mut nv12);
        let dur = 10_000_000 / self.fps as i64;
        let sample = sample_from(&nv12, self.frame_index * dur, dur);
        self.nv12 = nv12;
        let sample = sample?;
        self.frame_index += 1;

        let au = if self.events.is_some() {
            // Async (hardware) MFT: input only when it asked for it; wait briefly for the matching output.
            let mut out = self.pump(Duration::ZERO, false)?;
            if self.credits == 0 {
                out = out.or(self.pump(Duration::from_millis(20), false)?);
            }
            if self.credits > 0 {
                self.credits -= 1;
                unsafe { self.mft.ProcessInput(0, &sample, 0)? };
                if out.is_none() {
                    out = self.pump(Duration::from_millis(40), true)?;
                }
            }
            out
        } else {
            unsafe { self.mft.ProcessInput(0, &sample, 0)? };
            let mut au: Option<Vec<u8>> = None;
            loop {
                match process_output(&self.mft) {
                    Ok(s) => au.get_or_insert_with(Vec::new).extend(sample_bytes(&s)?),
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                    Err(e) => return Err(e),
                }
            }
            au
        };
        Ok(au.map(|mut au| {
            let types = nal_types(&au);
            let idr = types.contains(&5);
            // Decoders need SPS/PPS in band; some encoders only put them in the media type.
            if idr && !types.contains(&7) && !self.sequence_header.is_empty() {
                let mut with = self.sequence_header.clone();
                with.extend_from_slice(&au);
                au = with;
            }
            (au, idr)
        }))
    }

    /// Handles async MFT events until an output arrives (when `until_output`) or `wait` elapses.
    fn pump(&mut self, wait: Duration, until_output: bool) -> Result<Option<Vec<u8>>> {
        let Some(events) = self.events.clone() else { return Ok(None) };
        let deadline = Instant::now() + wait;
        let mut out: Option<Vec<u8>> = None;
        loop {
            match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(ev) => {
                    let t = unsafe { ev.GetType()? };
                    if t == METransformNeedInput.0 as u32 {
                        self.credits += 1;
                        if !until_output && wait > Duration::ZERO {
                            return Ok(out);
                        }
                    } else if t == METransformHaveOutput.0 as u32 {
                        match process_output(&self.mft) {
                            Ok(s) => {
                                out = Some(sample_bytes(&s)?);
                                if until_output {
                                    return Ok(out);
                                }
                            }
                            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => unsafe {
                                let t = self.mft.GetOutputAvailableType(0, 0)?;
                                self.mft.SetOutputType(0, &t, 0)?;
                            },
                            Err(e) => return Err(e),
                        }
                    }
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if Instant::now() >= deadline {
                        return Ok(out);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// H.264 decoder (Media Foundation transform) producing RGBA.
pub struct H264Decoder {
    mft: IMFTransform,
    out_w: u32,
    out_h: u32,
    pitch: usize,
    rgba: Vec<u8>,
    frame_index: i64,
}

unsafe impl Send for H264Decoder {}

impl H264Decoder {
    /// Creates a decoder for `width` x `height` streams.
    pub fn new(width: u32, height: u32) -> Result<Self> {
        ensure_mf()?;
        unsafe {
            let mft: IMFTransform = CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?;
            if let Ok(attrs) = mft.GetAttributes() {
                // No frame reordering delay: output as soon as a frame is decoded.
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let in_t = video_type(&MFVideoFormat_H264, width, height, 60)?;
            mft.SetInputType(0, &in_t, 0)?;
            let mut d =
                Self { mft, out_w: width, out_h: height, pitch: width as usize, rgba: Vec::new(), frame_index: 0 };
            d.pick_output()?;
            d.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            d.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(d)
        }
    }

    fn pick_output(&mut self) -> Result<()> {
        unsafe {
            let mut i = 0;
            loop {
                let t = self.mft.GetOutputAvailableType(0, i)?;
                i += 1;
                if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    self.mft.SetOutputType(0, &t, 0)?;
                    let size = t.GetUINT64(&MF_MT_FRAME_SIZE)?;
                    self.out_w = (size >> 32) as u32;
                    self.out_h = size as u32;
                    self.pitch = t
                        .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                        .map(|s| s as i32 as usize)
                        .unwrap_or(self.out_w as usize)
                        .max(self.out_w as usize);
                    return Ok(());
                }
            }
        }
    }

    /// Decodes one access unit; returns RGBA cropped to `width` x `height` when a picture came out.
    pub fn decode(&mut self, au: &[u8], width: u32, height: u32) -> Result<Option<&[u8]>> {
        let mut out = std::mem::take(&mut self.rgba);
        let got = self.decode_into(au, width, height, &mut out);
        self.rgba = out;
        Ok(if got? { Some(&self.rgba) } else { None })
    }

    /// Like `decode` but converts straight into `out` (e.g. the viewer framebuffer), saving a frame copy.
    pub fn decode_into(&mut self, au: &[u8], width: u32, height: u32, out: &mut Vec<u8>) -> Result<bool> {
        let sample = sample_from(au, self.frame_index * 166_666, 166_666)?;
        self.frame_index += 1;
        unsafe { self.mft.ProcessInput(0, &sample, 0)? };
        let mut got = false;
        loop {
            match process_output(&self.mft) {
                Ok(s) => {
                    self.convert(&s, width.min(self.out_w), height.min(self.out_h), out)?;
                    got = true;
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => self.pick_output()?,
                Err(e) => return Err(e),
            }
        }
        Ok(got)
    }

    fn convert(&self, s: &IMFSample, w: u32, h: u32, out: &mut Vec<u8>) -> Result<()> {
        unsafe {
            let buf = s.ConvertToContiguousBuffer()?;
            if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
                let (mut p, mut pitch) = (std::ptr::null_mut(), 0i32);
                b2.Lock2D(&mut p, &mut pitch)?;
                let pitch = pitch.unsigned_abs() as usize;
                let total = pitch * self.out_h as usize * 3 / 2;
                let data = std::slice::from_raw_parts(p, total);
                let (yp, uv) = data.split_at(pitch * self.out_h as usize);
                nv12_to_rgba(yp, uv, pitch, w as usize, h as usize, out);
                b2.Unlock2D()?;
            } else {
                let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
                buf.Lock(&mut p, None, Some(&mut len))?;
                let data = std::slice::from_raw_parts(p, len as usize);
                let split = (self.pitch * self.out_h as usize).min(data.len());
                let (yp, uv) = data.split_at(split);
                nv12_to_rgba(yp, uv, self.pitch, w as usize, h as usize, out);
                buf.Unlock()?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(w: usize, h: usize, t: usize) -> Vec<u32> {
        (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w + t * 4) % w, i / w);
                (((x * 255 / w) as u32) << 16) | (((y * 255 / h) as u32) << 8) | (((x ^ y) & 0xFF) as u32)
            })
            .collect()
    }

    #[test]
    fn nv12_roundtrip_is_close() {
        let (w, h) = (64, 32);
        let src = pattern(w, h, 0);
        let mut nv12 = Vec::new();
        bgra_to_nv12(&src, w, w, h, &mut nv12);
        let mut rgba = Vec::new();
        nv12_to_rgba(&nv12[..w * h], &nv12[w * h..], w, w, h, &mut rgba);
        // Flat 2x2 blocks survive exactly enough; chroma subsampling bounds the error elsewhere.
        let flat = vec![0x00C0_4010u32; w * h];
        bgra_to_nv12(&flat, w, w, h, &mut nv12);
        nv12_to_rgba(&nv12[..w * h], &nv12[w * h..], w, w, h, &mut rgba);
        for (c, want) in [0xC0u8, 0x40, 0x10].iter().enumerate() {
            assert!((rgba[c] as i32 - *want as i32).abs() <= 2, "channel {c}: {} vs {want}", rgba[c]);
        }
    }

    #[test]
    fn software_encode_decode_roundtrip() {
        let (w, h) = (320u32, 192u32);
        let mut enc = match H264Encoder::new(w, h, 30, 2_000_000, false) {
            Ok(e) => e,
            Err(_) => return, // N editions of Windows ship without the Media Foundation codecs.
        };
        let mut dec = H264Decoder::new(w, h).unwrap();
        let mut shown = 0;
        for t in 0..10 {
            let frame = pattern(w as usize, h as usize, t);
            if let Some((au, idr)) = enc.encode(&frame, w as usize).unwrap() {
                if t == 0 {
                    assert!(idr, "stream starts with an IDR");
                }
                if let Some(rgba) = dec.decode(&au, w, h).unwrap() {
                    assert_eq!(rgba.len(), (w * h * 4) as usize);
                    shown += 1;
                }
            }
        }
        assert!(shown >= 8, "low-latency decode should not buffer ({shown} of 10 shown)");
    }
}
