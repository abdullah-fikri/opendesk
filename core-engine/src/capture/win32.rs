//! Raw Win32 (user32/gdi32) declarations shared by the capture backends.
#![allow(non_camel_case_types, non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;

pub(super) type HWND = *mut c_void;
pub(super) type HDC = *mut c_void;
pub(super) type HBITMAP = *mut c_void;
pub(super) type HGDIOBJ = *mut c_void;
pub(super) type HICON = *mut c_void;
pub(super) type BOOL = i32;
pub(super) type DWORD = u32;
pub(super) type WORD = u16;
pub(super) type UINT = u32;

pub(super) const SRCCOPY: DWORD = 0x00CC0020;
pub(super) const CURSOR_SHOWING: DWORD = 0x00000001;
pub(super) const BI_RGB: DWORD = 0;
pub(super) const COLORONCOLOR: i32 = 3;
pub(super) const HALFTONE: i32 = 4;
pub(super) const DESKTOPHORZRES: i32 = 118;
pub(super) const DESKTOPVERTRES: i32 = 117;

pub(super) const ENUM_CURRENT_SETTINGS: DWORD = 0xFFFFFFFF;
pub(super) const DISPLAY_DEVICE_ATTACHED_TO_DESKTOP: DWORD = 0x00000001;
pub(super) const DISPLAY_DEVICE_PRIMARY_DEVICE: DWORD = 0x00000004;
pub(super) const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: isize = -4;

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct POINT {
    pub(super) x: i32,
    pub(super) y: i32,
}

#[repr(C)]
pub(super) struct CURSORINFO {
    pub(super) cb_size: DWORD,
    pub(super) flags: DWORD,
    pub(super) h_cursor: HICON,
    pub(super) pt_screen_pos: POINT,
}

#[repr(C)]
pub(super) struct DISPLAY_DEVICEW {
    pub(super) cb: DWORD,
    pub(super) device_name: [u16; 32],
    pub(super) device_string: [u16; 128],
    pub(super) state_flags: DWORD,
    pub(super) device_id: [u16; 128],
    pub(super) device_key: [u16; 128],
}

#[repr(C)]
pub(super) struct DEVMODEW {
    pub(super) dm_device_name: [u16; 32],
    pub(super) dm_spec_version: WORD,
    pub(super) dm_driver_version: WORD,
    pub(super) dm_size: WORD,
    pub(super) dm_driver_extra: WORD,
    pub(super) dm_fields: DWORD,
    pub(super) dm_position: POINT,
    pub(super) dm_display_orientation: DWORD,
    pub(super) dm_display_fixed_output: DWORD,
    pub(super) dm_color: i16,
    pub(super) dm_duplex: i16,
    pub(super) dm_y_resolution: i16,
    pub(super) dm_tt_option: i16,
    pub(super) dm_collate: i16,
    pub(super) dm_form_name: [u16; 32],
    pub(super) dm_log_pixels: WORD,
    pub(super) dm_bits_per_pel: DWORD,
    pub(super) dm_pels_width: DWORD,
    pub(super) dm_pels_height: DWORD,
    pub(super) dm_display_flags: DWORD,
    pub(super) dm_display_frequency: DWORD,
    pub(super) dm_icm_method: DWORD,
    pub(super) dm_icm_intent: DWORD,
    pub(super) dm_media_type: DWORD,
    pub(super) dm_dither_type: DWORD,
    pub(super) dm_reserved1: DWORD,
    pub(super) dm_reserved2: DWORD,
    pub(super) dm_panning_width: DWORD,
    pub(super) dm_panning_height: DWORD,
}

#[repr(C)]
pub(super) struct BITMAPINFOHEADER {
    pub(super) bi_size: DWORD,
    pub(super) bi_width: i32,
    pub(super) bi_height: i32,
    pub(super) bi_planes: u16,
    pub(super) bi_bit_count: u16,
    pub(super) bi_compression: DWORD,
    pub(super) bi_size_image: DWORD,
    pub(super) bi_x_pels_per_meter: i32,
    pub(super) bi_y_pels_per_meter: i32,
    pub(super) bi_clr_used: DWORD,
    pub(super) bi_clr_important: DWORD,
}

#[repr(C)]
pub(super) struct BITMAPINFO {
    pub(super) bmi_header: BITMAPINFOHEADER,
    pub(super) bmi_colors: [u32; 1],
}

#[link(name = "user32")]
unsafe extern "system" {
    pub(super) fn SetProcessDpiAwarenessContext(value: isize) -> BOOL;
    pub(super) fn SetProcessDPIAware() -> BOOL;
    pub(super) fn GetSystemMetrics(n_index: i32) -> i32;
    pub(super) fn GetDC(h_wnd: HWND) -> HDC;
    pub(super) fn ReleaseDC(h_wnd: HWND, h_dc: HDC) -> i32;
    pub(super) fn GetCursorInfo(pci: *mut CURSORINFO) -> BOOL;
    pub(super) fn DrawIcon(h_dc: HDC, x: i32, y: i32, h_icon: HICON) -> BOOL;
    pub(super) fn LoadCursorW(h_instance: *mut c_void, lp_cursor_name: *const u16) -> HICON;
    pub(super) fn EnumDisplayDevicesW(
        lp_device: *const u16,
        i_dev_num: DWORD,
        lp_display_device: *mut DISPLAY_DEVICEW,
        dw_flags: DWORD,
    ) -> BOOL;
    pub(super) fn EnumDisplaySettingsExW(
        lpsz_device_name: *const u16,
        i_mode_num: DWORD,
        lp_dev_mode: *mut DEVMODEW,
        dw_flags: DWORD,
    ) -> BOOL;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    pub(super) fn CreateCompatibleDC(h_dc: HDC) -> HDC;
    pub(super) fn DeleteDC(h_dc: HDC) -> BOOL;
    pub(super) fn GetDeviceCaps(h_dc: HDC, index: i32) -> i32;
    pub(super) fn CreateDIBSection(
        hdc: HDC,
        pbmi: *const BITMAPINFO,
        usage: UINT,
        ppv_bits: *mut *mut c_void,
        h_section: *mut c_void,
        offset: DWORD,
    ) -> HBITMAP;
    pub(super) fn SelectObject(h_dc: HDC, h: HGDIOBJ) -> HGDIOBJ;
    pub(super) fn DeleteObject(ho: HGDIOBJ) -> BOOL;
    pub(super) fn BitBlt(
        h_dc: HDC,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        h_dc_src: HDC,
        x1: i32,
        y1: i32,
        rop: DWORD,
    ) -> BOOL;
    pub(super) fn StretchBlt(
        h_dc_dest: HDC,
        x_dest: i32,
        y_dest: i32,
        w_dest: i32,
        h_dest: i32,
        h_dc_src: HDC,
        x_src: i32,
        y_src: i32,
        w_src: i32,
        h_src: i32,
        rop: DWORD,
    ) -> BOOL;
    pub(super) fn SetStretchBltMode(h_dc: HDC, mode: i32) -> i32;
    pub(super) fn SetBrushOrgEx(h_dc: HDC, x: i32, y: i32, lppt: *mut POINT) -> BOOL;
}
