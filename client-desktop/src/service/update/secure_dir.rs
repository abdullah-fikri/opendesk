//! `%ProgramData%\OpenDesk\updates`, writable by SYSTEM and Administrators only (Windows).
//!
//! Any user may create folders in `%ProgramData%`, so a folder that already exists is trusted only when its
//! owner is SYSTEM or Administrators; otherwise it was planted, and its contents are deleted before the
//! protected ACL is applied. Links are refused.

use std::ffi::c_void;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr;

type BOOL = i32;

/// Owner Administrators, protected DACL: full control for SYSTEM and Administrators, inherited by children.
const SDDL: &str = "O:BAG:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
const SDDL_REVISION_1: u32 = 1;
const OWNER_SECURITY_INFORMATION: u32 = 0x1;
const GROUP_SECURITY_INFORMATION: u32 = 0x2;
const DACL_SECURITY_INFORMATION: u32 = 0x4;
const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
const WIN_LOCAL_SYSTEM_SID: u32 = 22;
const WIN_BUILTIN_ADMINISTRATORS_SID: u32 = 26;

#[repr(C)]
struct SECURITY_ATTRIBUTES {
    n_length: u32,
    lp_security_descriptor: *mut c_void,
    b_inherit_handle: BOOL,
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string_sd: *const u16,
        revision: u32,
        sd: *mut *mut c_void,
        sd_len: *mut u32,
    ) -> BOOL;
    fn SetFileSecurityW(file_name: *const u16, info: u32, sd: *mut c_void) -> BOOL;
    fn GetFileSecurityW(file_name: *const u16, info: u32, sd: *mut c_void, len: u32, needed: *mut u32) -> BOOL;
    fn GetSecurityDescriptorOwner(sd: *const c_void, owner: *mut *mut c_void, defaulted: *mut BOOL) -> BOOL;
    fn IsWellKnownSid(sid: *const c_void, well_known_sid_type: u32) -> BOOL;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateDirectoryW(path: *const u16, attributes: *const SECURITY_ATTRIBUTES) -> BOOL;
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect()
}

/// The update directory under `%ProgramData%`.
pub fn root() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("OpenDesk").join("updates")
}

/// Creates or repairs `dir` and its parent with the protected ACL. Only the service (SYSTEM) calls this.
pub fn ensure(dir: &Path) -> io::Result<()> {
    let sd = Descriptor::new()?;
    if let Some(parent) = dir.parent() {
        ensure_one(parent, &sd)?;
    }
    ensure_one(dir, &sd)
}

fn ensure_one(dir: &Path, sd: &Descriptor) -> io::Result<()> {
    let path = wide(dir);
    match fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            return Err(io::Error::other(format!("{} is not a plain directory", dir.display())));
        }
        Ok(_) => {
            if !trusted_owner(&path) {
                crate::infra::logger::warn("update", &format!("{} has a foreign owner; emptying it.", dir.display()));
                apply(&path, sd)?;
                for entry in fs::read_dir(dir)?.flatten() {
                    let p = entry.path();
                    // Neither call follows links.
                    let _ = if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        fs::remove_dir_all(&p)
                    } else {
                        fs::remove_file(&p)
                    };
                }
            }
        }
        Err(_) => {
            let sa = SECURITY_ATTRIBUTES {
                n_length: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lp_security_descriptor: sd.0,
                b_inherit_handle: 0,
            };
            if unsafe { CreateDirectoryW(path.as_ptr(), &sa) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    apply(&path, sd)
}

fn apply(path: &[u16], sd: &Descriptor) -> io::Result<()> {
    let info = OWNER_SECURITY_INFORMATION
        | GROUP_SECURITY_INFORMATION
        | DACL_SECURITY_INFORMATION
        | PROTECTED_DACL_SECURITY_INFORMATION;
    if unsafe { SetFileSecurityW(path.as_ptr(), info, sd.0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn trusted_owner(path: &[u16]) -> bool {
    let mut buf = [0u64; 64];
    let mut needed = 0u32;
    let sd = buf.as_mut_ptr() as *mut c_void;
    unsafe {
        if GetFileSecurityW(path.as_ptr(), OWNER_SECURITY_INFORMATION, sd, 512, &mut needed) == 0 {
            return false;
        }
        let (mut owner, mut defaulted) = (ptr::null_mut(), 0);
        if GetSecurityDescriptorOwner(sd, &mut owner, &mut defaulted) == 0 || owner.is_null() {
            return false;
        }
        IsWellKnownSid(owner, WIN_LOCAL_SYSTEM_SID) != 0 || IsWellKnownSid(owner, WIN_BUILTIN_ADMINISTRATORS_SID) != 0
    }
}

/// Self-relative security descriptor built from [`SDDL`], freed on drop.
struct Descriptor(*mut c_void);

impl Descriptor {
    fn new() -> io::Result<Descriptor> {
        let sddl: Vec<u16> = SDDL.encode_utf16().chain(std::iter::once(0)).collect();
        let mut sd = ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Descriptor(sd))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sddl_parses() {
        assert!(Descriptor::new().is_ok());
    }

    #[test]
    fn root_is_under_program_data() {
        assert!(root().ends_with(r"OpenDesk\updates"));
    }
}
