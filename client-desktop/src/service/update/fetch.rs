//! Download of the manifest and the installers (HTTPS only, through the system TLS stack like the relay
//! connection), and verification of a downloaded file against its manifest.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::manifest::{self, Manifest, Version};
use crate::domain::edition::Edition;

/// Releases of this repository; `latest/download` always serves the newest release's assets.
const DEFAULT_BASE: &str = "https://github.com/abdullah-fikri/opendesk/releases/latest/download/";
/// A manifest is a few hundred bytes; anything larger is not one.
const MAX_MANIFEST: u64 = 64 * 1024;

/// Manifest file of this edition (each edition has its own installer).
pub fn manifest_name(edition: Edition) -> &'static str {
    match edition {
        Edition::Standard => "manifest.json",
        Edition::Admin => "manifest-admin.json",
        Edition::Headless => "manifest-host.json",
    }
}

/// The manifest URL to check: `configured` (settings or `OPENDESK_UPDATE_URL`) or the GitHub release.
pub fn latest_url(configured: &str, edition: Edition) -> String {
    let configured = configured.trim();
    if configured.is_empty() { format!("{DEFAULT_BASE}{}", manifest_name(edition)) } else { configured.to_string() }
}

/// The manifest published with release `version`, for a `latest` URL in the GitHub layout
/// (`.../latest/download/<file>` -> `.../download/v<version>/<file>`). Mirrors use the same layout.
pub fn versioned_url(latest: &str, version: Version) -> Option<String> {
    latest
        .contains("/latest/download/")
        .then(|| latest.replacen("/latest/download/", &format!("/download/v{version}/"), 1))
}

fn agent() -> Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(Arc::new(tls))
        .https_only(true)
        .redirects(5)
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .user_agent(concat!("OpenDesk/", env!("CARGO_PKG_VERSION")))
        .build())
}

fn get(url: &str) -> Result<Box<dyn Read + Send + Sync>, String> {
    let response = agent()?.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    Ok(response.into_reader())
}

fn get_small(url: &str) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    get(url)?.take(MAX_MANIFEST + 1).read_to_end(&mut body).map_err(|e| e.to_string())?;
    if body.len() as u64 > MAX_MANIFEST {
        return Err(format!("{url}: larger than {MAX_MANIFEST} bytes"));
    }
    Ok(body)
}

/// Downloads `url` and its `.sig` and returns the manifest only if it verifies against the release key.
pub fn fetch_manifest(url: &str) -> Result<Manifest, String> {
    let bytes = get_small(url)?;
    let sig = get_small(&format!("{url}.sig"))?;
    manifest::verify(&bytes, &String::from_utf8_lossy(&sig), manifest::RELEASE_PUBLIC_KEY).map_err(|e| e.to_string())
}

/// Downloads the installer of `m` into `dir` and returns its path once size and SHA-256 match.
pub fn download_installer(m: &Manifest, dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let part = dir.join(format!("{}.part", m.version));
    let done = dir.join(format!("OpenDesk-Setup-{}.exe", m.version));
    let result = get(&m.installer_url).and_then(|body| copy_verified(body, &part, m.size, &m.sha256));
    if let Err(e) = result {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, &done).map_err(|e| e.to_string())?;
    Ok(done)
}

/// Streams `body` to `dest`, refusing more than `size` bytes, and checks size and hash.
fn copy_verified(mut body: impl Read, dest: &Path, size: u64, sha256: &str) -> Result<(), String> {
    let mut out = File::create(dest).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = body.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > size {
            return Err(format!("installer is larger than the {size} bytes of the manifest"));
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
    }
    out.sync_all().map_err(|e| e.to_string())?;
    check(total, &manifest::hex_encode(&hasher.finalize()), size, sha256)
}

/// Re-checks a file on disk (an installer is verified again right before it runs).
pub fn verify_file(path: &Path, size: u64, sha256: &str) -> Result<(), String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let total = io::copy(&mut file, &mut hasher).map_err(|e| e.to_string())?;
    check(total, &manifest::hex_encode(&hasher.finalize()), size, sha256)
}

fn check(total: u64, actual: &str, size: u64, sha256: &str) -> Result<(), String> {
    if total != size {
        return Err(format!("installer has {total} bytes, the manifest says {size}"));
    }
    if !actual.eq_ignore_ascii_case(sha256.trim()) {
        return Err("installer SHA-256 does not match the manifest".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(data: &[u8]) -> String {
        manifest::hex_encode(&Sha256::digest(data))
    }

    #[test]
    fn urls_follow_the_release_layout() {
        assert_eq!(
            latest_url("", Edition::Admin),
            "https://github.com/abdullah-fikri/opendesk/releases/latest/download/manifest-admin.json"
        );
        assert_eq!(latest_url(" https://m.example/x.json ", Edition::Standard), "https://m.example/x.json");
        assert_eq!(
            versioned_url(&latest_url("", Edition::Standard), Version(1, 2, 3)).unwrap(),
            "https://github.com/abdullah-fikri/opendesk/releases/download/v1.2.3/manifest.json"
        );
        assert_eq!(versioned_url("https://m.example/x.json", Version(1, 2, 3)), None);
    }

    #[test]
    fn only_a_file_matching_size_and_hash_is_accepted() {
        let dir = std::env::temp_dir().join(format!("opendesk-fetch-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("setup.part");
        let data = b"fake installer bytes";
        assert!(copy_verified(&data[..], &dest, data.len() as u64, &sha(data)).is_ok());
        assert!(verify_file(&dest, data.len() as u64, &sha(data)).is_ok());
        assert!(verify_file(&dest, data.len() as u64, &sha(b"other")).is_err());
        assert!(copy_verified(&data[..], &dest, data.len() as u64 - 1, &sha(data)).is_err());
        assert!(copy_verified(&data[..], &dest, data.len() as u64 + 1, &sha(data)).is_err());
        assert!(copy_verified(&data[..], &dest, data.len() as u64, &sha(b"other")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
