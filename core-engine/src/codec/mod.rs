use std::ffi::c_void;
use std::ptr;

type HBITMAP = *mut c_void;
type HGLOBAL = *mut c_void;
type BOOL = i32;

#[repr(C)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

pub const CLSID_JPEG: GUID = GUID {
    data1: 0x557cf401,
    data2: 0x1a04,
    data3: 0x11d3,
    data4: [0x9a, 0x73, 0x00, 0x00, 0xf8, 0x1e, 0xf3, 0x2e],
};

pub const ENCODER_QUALITY_GUID: GUID = GUID {
    data1: 0x1d5be4b5,
    data2: 0xfa4a,
    data3: 0x452d,
    data4: [0x9c, 0xdd, 0x5d, 0xb3, 0x51, 0x05, 0xe7, 0xeb],
};

#[repr(C)]
pub struct GdiplusStartupInput {
    pub gdiplus_version: u32,
    pub debug_event_callback: *mut c_void,
    pub suppress_background_thread: BOOL,
    pub suppress_external_codecs: BOOL,
}

#[repr(C)]
pub struct EncoderParameter {
    pub guid: GUID,
    pub number_of_values: u32,
    pub type_: u32,
    pub value: *const c_void,
}

#[repr(C)]
pub struct EncoderParameters {
    pub count: u32,
    pub parameter: [EncoderParameter; 1],
}

#[link(name = "gdiplus")]
unsafe extern "system" {
    fn GdiplusStartup(token: *mut usize, input: *const GdiplusStartupInput, output: *mut c_void) -> i32;
    fn GdiplusShutdown(token: usize);
    fn GdipCreateBitmapFromHBITMAP(hbm: HBITMAP, hpal: *mut c_void, bitmap: *mut *mut c_void) -> i32;
    fn GdipDisposeImage(image: *mut c_void) -> i32;
    fn GdipSaveImageToStream(
        image: *mut c_void,
        stream: *mut c_void,
        clsid_encoder: *const GUID,
        encoder_params: *const EncoderParameters,
    ) -> i32;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CreateStreamOnHGlobal(h_global: HGLOBAL, f_delete_on_release: BOOL, ppstm: *mut *mut c_void) -> i32;
    fn GetHGlobalFromStream(pstm: *mut c_void, phglobal: *mut HGLOBAL) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalLock(h_mem: HGLOBAL) -> *mut c_void;
    fn GlobalUnlock(h_mem: HGLOBAL) -> BOOL;
    fn GlobalSize(h_mem: HGLOBAL) -> usize;
}

pub struct JpegEncoder {
    gdiplus_token: usize,
    encoder_params: EncoderParameters,
    quality_val: Box<u32>,
}

impl JpegEncoder {
    pub fn new(quality: u32) -> Self {
        let mut token: usize = 0;
        let startup = GdiplusStartupInput {
            gdiplus_version: 1,
            debug_event_callback: ptr::null_mut(),
            suppress_background_thread: 0,
            suppress_external_codecs: 0,
        };
        unsafe {
            GdiplusStartup(&mut token, &startup, ptr::null_mut());
        }

        let quality_box = Box::new(quality.clamp(1, 100));
        let val_ptr = &*quality_box as *const u32 as *const c_void;

        Self {
            gdiplus_token: token,
            encoder_params: EncoderParameters {
                count: 1,
                parameter: [EncoderParameter {
                    guid: ENCODER_QUALITY_GUID,
                    number_of_values: 1,
                    type_: 4,
                    value: val_ptr,
                }],
            },
            quality_val: quality_box,
        }
    }

    pub fn encode_hbitmap(&self, hbm: HBITMAP) -> Option<Vec<u8>> {
        let _ = &self.quality_val;
        unsafe {
            let mut gdip_bmp: *mut c_void = ptr::null_mut();
            if GdipCreateBitmapFromHBITMAP(hbm, ptr::null_mut(), &mut gdip_bmp) != 0 || gdip_bmp.is_null() {
                return None;
            }

            let mut stream: *mut c_void = ptr::null_mut();
            if CreateStreamOnHGlobal(ptr::null_mut(), 1, &mut stream) != 0 || stream.is_null() {
                GdipDisposeImage(gdip_bmp);
                return None;
            }

            let status = GdipSaveImageToStream(
                gdip_bmp,
                stream,
                &CLSID_JPEG,
                &self.encoder_params,
            );

            GdipDisposeImage(gdip_bmp);

            if status != 0 {
                let vtable = *(stream as *mut *mut usize);
                let release_fn: extern "system" fn(*mut c_void) -> u32 =
                    std::mem::transmute(*vtable.add(2));
                release_fn(stream);
                return None;
            }

            let mut h_global: HGLOBAL = ptr::null_mut();
            GetHGlobalFromStream(stream, &mut h_global);
            let size = GlobalSize(h_global);
            let ptr_buf = GlobalLock(h_global) as *const u8;

            let mut result = Vec::with_capacity(size);
            if !ptr_buf.is_null() && size > 0 {
                result.extend_from_slice(std::slice::from_raw_parts(ptr_buf, size));
                GlobalUnlock(h_global);
            }

            let vtable = *(stream as *mut *mut usize);
            let release_fn: extern "system" fn(*mut c_void) -> u32 =
                std::mem::transmute(*vtable.add(2));
            release_fn(stream);

            Some(result)
        }
    }
}

impl Drop for JpegEncoder {
    fn drop(&mut self) {
        let _ = &self.quality_val;
        unsafe {
            if self.gdiplus_token != 0 {
                GdiplusShutdown(self.gdiplus_token);
            }
        }
    }
}

pub fn to_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let chunks = bytes.chunks_exact(3);
    let rem = chunks.remainder();

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
