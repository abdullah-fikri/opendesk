//! JPEG encoding through GDI+.

use std::ffi::c_void;
use std::ptr;

type HGLOBAL = *mut c_void;
type BOOL = i32;

#[repr(C)]
struct GUID {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const CLSID_JPEG: GUID =
    GUID { data1: 0x557cf401, data2: 0x1a04, data3: 0x11d3, data4: [0x9a, 0x73, 0x00, 0x00, 0xf8, 0x1e, 0xf3, 0x2e] };

const ENCODER_QUALITY_GUID: GUID =
    GUID { data1: 0x1d5be4b5, data2: 0xfa4a, data3: 0x452d, data4: [0x9c, 0xdd, 0x5d, 0xb3, 0x51, 0x05, 0xe7, 0xeb] };

#[repr(C)]
struct GdiplusStartupInput {
    gdiplus_version: u32,
    debug_event_callback: *mut c_void,
    suppress_background_thread: BOOL,
    suppress_external_codecs: BOOL,
}

#[repr(C)]
struct EncoderParameter {
    guid: GUID,
    number_of_values: u32,
    type_: u32,
    value: *const c_void,
}

#[repr(C)]
struct EncoderParameters {
    count: u32,
    parameter: [EncoderParameter; 1],
}

#[link(name = "gdiplus")]
unsafe extern "system" {
    fn GdiplusStartup(token: *mut usize, input: *const GdiplusStartupInput, output: *mut c_void) -> i32;
    fn GdiplusShutdown(token: usize);
    fn GdipCreateBitmapFromScan0(
        width: i32,
        height: i32,
        stride: i32,
        format: i32,
        scan0: *mut u8,
        bitmap: *mut *mut c_void,
    ) -> i32;
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

/// JPEG encoder backed by GDI+. One per thread; GDI+ is started and shut down with it.
pub struct JpegEncoder {
    gdiplus_token: usize,
    encoder_params: EncoderParameters,
    quality_val: Box<u32>,
}

impl JpegEncoder {
    /// Starts GDI+ and prepares an encoder with `quality` (0..=100).
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

    /// Changes the JPEG quality (0..=100) for later frames.
    pub fn set_quality(&mut self, quality: u32) {
        *self.quality_val = quality.clamp(1, 100);
    }

    /// Current JPEG quality.
    pub fn quality(&self) -> u32 {
        *self.quality_val
    }

    /// Encodes a top-down BGRX pixel buffer in place (no copy); `stride` is in pixels.
    pub fn encode_bgra(&self, pixels: &[u32], width: i32, height: i32, stride: usize) -> Option<Vec<u8>> {
        const PIXEL_FORMAT_32BPP_RGB: i32 = 0x0002_2009;
        if width <= 0 || height <= 0 || pixels.len() < stride * (height as usize - 1) + width as usize {
            return None;
        }
        unsafe {
            let mut gdip_bmp: *mut c_void = ptr::null_mut();
            let st = GdipCreateBitmapFromScan0(
                width,
                height,
                (stride * 4) as i32,
                PIXEL_FORMAT_32BPP_RGB,
                pixels.as_ptr() as *mut u8,
                &mut gdip_bmp,
            );
            if st != 0 || gdip_bmp.is_null() {
                return None;
            }
            self.save_and_dispose(gdip_bmp)
        }
    }

    unsafe fn save_and_dispose(&self, gdip_bmp: *mut c_void) -> Option<Vec<u8>> {
        let _ = &self.quality_val;
        unsafe {
            let mut stream: *mut c_void = ptr::null_mut();
            if CreateStreamOnHGlobal(ptr::null_mut(), 1, &mut stream) != 0 || stream.is_null() {
                GdipDisposeImage(gdip_bmp);
                return None;
            }

            let status = GdipSaveImageToStream(gdip_bmp, stream, &CLSID_JPEG, &self.encoder_params);

            GdipDisposeImage(gdip_bmp);

            if status != 0 {
                let vtable = *(stream as *mut *mut usize);
                let release_fn: extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*vtable.add(2));
                release_fn(stream);
                return None;
            }

            let mut h_global: HGLOBAL = ptr::null_mut();
            GetHGlobalFromStream(stream, &mut h_global);
            // GlobalSize is the allocation (rounded up); the stream position is the real JPEG length.
            let mut size = GlobalSize(h_global);
            let vtable = *(stream as *mut *mut usize);
            let seek_fn: extern "system" fn(*mut c_void, i64, u32, *mut u64) -> i32 =
                std::mem::transmute(*vtable.add(5));
            let mut pos = 0u64;
            if seek_fn(stream, 0, 1, &mut pos) == 0 && pos > 0 {
                size = size.min(pos as usize);
            }
            let ptr_buf = GlobalLock(h_global) as *const u8;

            let mut result = Vec::with_capacity(size);
            if !ptr_buf.is_null() && size > 0 {
                result.extend_from_slice(std::slice::from_raw_parts(ptr_buf, size));
                GlobalUnlock(h_global);
            }

            let vtable = *(stream as *mut *mut usize);
            let release_fn: extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*vtable.add(2));
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
