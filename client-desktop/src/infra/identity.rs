//! This device's static X25519 key for end-to-end encryption (`docs/features/e2e.md`). Generated once with
//! snow's resolver and kept in `identity.json` in the app directory.
//!
//! The private key is protected at rest:
//! - Windows: DPAPI with machine scope (`CRYPTPROTECT_LOCAL_MACHINE`), because the SYSTEM agent of the Windows
//!   service and the app started by the user read the same file in the user's profile. The blob is useless
//!   on another machine; on this one the file's ACL (inherited from the profile: the user, SYSTEM,
//!   Administrators) keeps other users out.
//! - Linux: a file readable by its owner only (0600).

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use core_engine::codec::{from_base64, to_base64};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::infra::{logger, storage};

const IDENTITY_FILE: &str = "identity.json";
/// Static keys are X25519.
pub const KEY_LEN: usize = 32;
const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// This device's static keypair.
pub struct Identity {
    pub public: [u8; KEY_LEN],
    secret: [u8; KEY_LEN],
}

impl Identity {
    /// The private key, for `snow::Builder::local_private_key`.
    pub fn secret(&self) -> &[u8] {
        &self.secret
    }

    fn generate() -> Option<Identity> {
        let kp = snow::Builder::new(NOISE_PARAMS.parse().ok()?).generate_keypair().ok()?;
        Some(Identity { public: kp.public.as_slice().try_into().ok()?, secret: kp.private.as_slice().try_into().ok()? })
    }
}

/// `identity.json`: the public key in clear, the private key protected as `protect` says.
#[derive(Serialize, Deserialize)]
struct Stored {
    v: u32,
    public: String,
    /// `dpapi` (Windows, machine scope) or `file` (owner-only file permissions).
    protect: String,
    secret: String,
}

/// This device's identity, loaded or created on first use. None if no key could be created or stored, then
/// end-to-end encryption stays off.
pub fn local() -> Option<&'static Identity> {
    static ID: OnceLock<Option<Identity>> = OnceLock::new();
    ID.get_or_init(|| load_or_create(&storage::app_dir().join(IDENTITY_FILE))).as_ref()
}

fn load_or_create(path: &Path) -> Option<Identity> {
    if let Ok(text) = fs::read_to_string(path) {
        match decode(&text) {
            Some(id) => return Some(id),
            // A key that no longer opens (restored on another machine, damaged file): a new identity. Viewers
            // that pinned the old key are warned and must accept the new one.
            None => logger::warn("identity", "Stored device key is unreadable; creating a new one."),
        }
    }
    let id = Identity::generate()?;
    let text = encode(&id)?;
    if let Err(e) = storage::write_atomic_private(path, text.as_bytes()) {
        logger::error("identity", &format!("Could not store the device key: {e}"));
        return None;
    }
    logger::info("identity", &format!("Created device key {}.", fingerprint(&id.public)));
    Some(id)
}

fn encode(id: &Identity) -> Option<String> {
    let (protect, secret) = protect(&id.secret)?;
    let stored = Stored { v: 1, public: to_base64(&id.public), protect: protect.into(), secret: to_base64(&secret) };
    serde_json::to_string_pretty(&stored).ok()
}

fn decode(text: &str) -> Option<Identity> {
    let stored: Stored = serde_json::from_str(text).ok()?;
    if stored.v != 1 {
        return None;
    }
    let public: [u8; KEY_LEN] = from_base64(&stored.public)?.try_into().ok()?;
    let secret: [u8; KEY_LEN] = unprotect(&stored.protect, &from_base64(&stored.secret)?)?.try_into().ok()?;
    Some(Identity { public, secret })
}

#[cfg(windows)]
fn protect(secret: &[u8]) -> Option<(&'static str, Vec<u8>)> {
    Some(("dpapi", dpapi::protect(secret)?))
}

#[cfg(not(windows))]
fn protect(secret: &[u8]) -> Option<(&'static str, Vec<u8>)> {
    Some(("file", secret.to_vec()))
}

fn unprotect(kind: &str, blob: &[u8]) -> Option<Vec<u8>> {
    match kind {
        #[cfg(windows)]
        "dpapi" => dpapi::unprotect(blob),
        "file" => Some(blob.to_vec()),
        _ => None,
    }
}

/// Short form of a public key for people to compare: the first 10 bytes of its SHA-256, as 5 groups of 4 hex
/// digits (`3f9a 01c2 ...`).
pub fn fingerprint(public: &[u8]) -> String {
    let digest = Sha256::digest(public);
    let hex: String = digest[..10].iter().map(|b| format!("{b:02x}")).collect();
    hex.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

#[cfg(windows)]
mod dpapi {
    use std::ffi::c_void;
    use std::ptr;

    #[repr(C)]
    struct DataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;
    const CRYPTPROTECT_LOCAL_MACHINE: u32 = 0x4;
    /// Ties the blob to this use: another program's DPAPI blob never opens as our key, nor ours as theirs.
    const ENTROPY: &[u8] = b"opendesk-identity-v1";

    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            data_in: *const DataBlob,
            description: *const u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
        fn CryptUnprotectData(
            data_in: *const DataBlob,
            description: *mut *mut u16,
            entropy: *const DataBlob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(mem: *mut c_void) -> *mut c_void;
    }

    fn blob(data: &[u8]) -> DataBlob {
        DataBlob { cb_data: data.len() as u32, pb_data: data.as_ptr() as *mut u8 }
    }

    /// Seals (`protect`) or opens a blob; DPAPI LocalAllocs the result, which is copied and freed.
    fn run(protect: bool, data: &[u8]) -> Option<Vec<u8>> {
        let input = blob(data);
        let entropy = blob(ENTROPY);
        let mut out = DataBlob { cb_data: 0, pb_data: ptr::null_mut() };
        let null = ptr::null_mut::<c_void>();
        // SAFETY: the input blobs point at live slices the calls only read; null description, reserved and
        // prompt pointers are allowed; `out` is freed exactly once below.
        unsafe {
            let ok = if protect {
                let flags = CRYPTPROTECT_UI_FORBIDDEN | CRYPTPROTECT_LOCAL_MACHINE;
                CryptProtectData(&input, ptr::null(), &entropy, null, null, flags, &mut out)
            } else {
                CryptUnprotectData(&input, ptr::null_mut(), &entropy, null, null, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            };
            if ok == 0 || out.pb_data.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(out.pb_data, out.cb_data as usize).to_vec();
            LocalFree(out.pb_data.cast());
            Some(bytes)
        }
    }

    pub fn protect(secret: &[u8]) -> Option<Vec<u8>> {
        run(true, secret)
    }

    pub fn unprotect(blob: &[u8]) -> Option<Vec<u8>> {
        run(false, blob)
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    /// A fresh identity that is never stored.
    pub fn identity() -> super::Identity {
        super::Identity::generate().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_created_once_and_reloaded() {
        let dir = std::env::temp_dir().join(format!("opendesk-identity-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(IDENTITY_FILE);
        let _ = fs::remove_file(&path);
        let first = load_or_create(&path).unwrap();
        let again = load_or_create(&path).unwrap();
        assert_eq!(first.public, again.public);
        assert_eq!(first.secret(), again.secret());
        #[cfg(windows)]
        assert!(!fs::read_to_string(&path).unwrap().contains(&to_base64(first.secret())), "key stored in clear");
        // A damaged file is replaced by a new identity instead of failing.
        fs::write(&path, "{}").unwrap();
        assert_ne!(load_or_create(&path).unwrap().public, first.public);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_is_short_grouped_and_key_specific() {
        let a = fingerprint(&[1u8; 32]);
        assert_eq!(a.len(), 24);
        assert_eq!(a.split(' ').count(), 5);
        assert_ne!(a, fingerprint(&[2u8; 32]));
    }
}
