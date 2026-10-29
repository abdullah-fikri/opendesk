//! The signed release manifest and the rules that decide whether this device installs it. Pure: no I/O.
//!
//! `manifest.json` is published next to the installers of every release, with a detached ed25519 signature
//! `manifest.json.sig` (hex of the 64-byte signature over the exact bytes of `manifest.json`). Only a manifest
//! that verifies against [`RELEASE_PUBLIC_KEY`] is ever parsed; the installer it names is used only after its
//! size and SHA-256 match (see `fetch`).

use std::cmp::Ordering;
use std::fmt;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::domain::desk_id;

/// Public half of the release signing key (hex, 32 bytes), embedded so a compromised download source cannot
/// ship an update.
///
/// Generated with `openssl genpkey -algorithm ed25519`; the private key is kept offline and in the
/// `UPDATE_SIGNING_KEY` CI secret. Printed with
/// `openssl pkey -in release-update.pem -pubout -outform DER | tail -c 32 | od -An -v -tx1 | tr -d ' \n'`.
/// Rotating it means shipping a build with the new key, signed with the old one, first. See `docs/auto-update.md`.
pub const RELEASE_PUBLIC_KEY: &str = "aebd97f5bfd502be4bdcff24104156dc6f34998b93b6f5fe593bf6a2d90a98b8";

/// A release version `major.minor.patch` (no pre-release tags: releases are tagged `vX.Y.Z`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let mut parts = s.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
        let v = Version(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(v)
    }

    /// The running build.
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is x.y.z")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// Contents of `manifest.json`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Manifest {
    pub version: String,
    /// `stable` for now; a device follows the channel of the manifest URL it is configured with.
    pub channel: String,
    /// Oldest version this release may be rolled back to: older builds cannot read the data it writes.
    /// A manual rollback below it needs `--force`.
    pub min_version: String,
    /// Share of devices (0-100, by [`rollout_bucket`]) offered this release automatically.
    pub rollout_percent: u8,
    pub installer_url: String,
    /// Lowercase hex SHA-256 of the installer.
    pub sha256: String,
    /// Installer size in bytes.
    pub size: u64,
}

impl Manifest {
    pub fn version(&self) -> Option<Version> {
        Version::parse(&self.version)
    }

    pub fn min_version(&self) -> Option<Version> {
        Version::parse(&self.min_version)
    }
}

#[derive(Debug, PartialEq)]
pub enum VerifyError {
    BadKey,
    BadSignature,
    Invalid(String),
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerifyError::BadKey => write!(f, "no valid release key is configured"),
            VerifyError::BadSignature => write!(f, "manifest signature does not verify"),
            VerifyError::Invalid(e) => write!(f, "invalid manifest: {e}"),
        }
    }
}

/// Verifies `signature` (hex text) over `bytes` with `public_key_hex`, then parses and checks the manifest.
pub fn verify(bytes: &[u8], signature: &str, public_key_hex: &str) -> Result<Manifest, VerifyError> {
    let key: [u8; 32] = hex_decode(public_key_hex).and_then(|k| k.try_into().ok()).ok_or(VerifyError::BadKey)?;
    // The all-zero placeholder (and other weak points) must never verify anything.
    if key == [0; 32] {
        return Err(VerifyError::BadKey);
    }
    let key = VerifyingKey::from_bytes(&key).map_err(|_| VerifyError::BadKey)?;
    if key.is_weak() {
        return Err(VerifyError::BadKey);
    }
    let sig: [u8; 64] = hex_decode(signature).and_then(|s| s.try_into().ok()).ok_or(VerifyError::BadSignature)?;
    key.verify_strict(bytes, &Signature::from_bytes(&sig)).map_err(|_| VerifyError::BadSignature)?;

    let manifest: Manifest = serde_json::from_slice(bytes).map_err(|e| VerifyError::Invalid(e.to_string()))?;
    let invalid = |what: &str| Err(VerifyError::Invalid(what.to_string()));
    if manifest.version().is_none() || manifest.min_version().is_none() {
        return invalid("version or min_version is not x.y.z");
    }
    if manifest.rollout_percent > 100 {
        return invalid("rollout_percent above 100");
    }
    if !manifest.installer_url.starts_with("https://") {
        return invalid("installer_url is not https");
    }
    if hex_decode(&manifest.sha256).is_none_or(|h| h.len() != 32) {
        return invalid("sha256 is not 64 hex digits");
    }
    if manifest.size == 0 {
        return invalid("size is 0");
    }
    Ok(manifest)
}

/// Stable 0-99 bucket of a device, from the SHA-256 of its clean desk ID.
pub fn rollout_bucket(desk_id: &str) -> u8 {
    let hash = Sha256::digest(desk_id::clean(desk_id).as_bytes());
    (u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]) % 100) as u8
}

/// Why an update is (not) installed.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Install(Version),
    UpToDate,
    /// The manifest is older than the running build; auto-update never downgrades.
    Older,
    NotInRollout,
    /// Rolled back from before; never retried automatically.
    Bad,
    /// Pinned to another version.
    Pinned,
    /// `opendesk update <version>` below the rollback floor without `--force`.
    BelowFloor,
    /// The manifest fetched for `opendesk update <version>` describes another release.
    WrongRelease,
}

/// What this device should do with a verified `manifest`. `forced` (`opendesk update`) skips the
/// rollout and the bad mark; a pin always wins.
pub fn decide(
    manifest: &Manifest,
    current: Version,
    desk_id: &str,
    bad: &[String],
    pinned: Option<&str>,
    forced: bool,
) -> Decision {
    let Some(version) = manifest.version() else { return Decision::UpToDate };
    if pinned.and_then(Version::parse).is_some_and(|p| p != version) {
        return Decision::Pinned;
    }
    match version.cmp(&current) {
        Ordering::Equal => return Decision::UpToDate,
        Ordering::Less => return Decision::Older,
        Ordering::Greater => {}
    }
    if !forced && bad.iter().any(|b| Version::parse(b) == Some(version)) {
        return Decision::Bad;
    }
    if !forced && rollout_bucket(desk_id) >= manifest.rollout_percent {
        return Decision::NotInRollout;
    }
    Decision::Install(version)
}

/// `opendesk update <version>`: install the release `target` names, newer or older than the running build.
/// Rollout and bad marks do not apply (the administrator chose it); going below `floor` needs `force`.
pub fn decide_target(
    manifest: &Manifest,
    current: Version,
    target: Version,
    floor: Option<Version>,
    force: bool,
) -> Decision {
    if manifest.version() != Some(target) {
        return Decision::WrongRelease;
    }
    if target == current {
        return Decision::UpToDate;
    }
    if target < current && !rollback_allowed(target, floor, force) {
        return Decision::BelowFloor;
    }
    Decision::Install(target)
}

/// May this device go back to `target`? `floor` is the `min_version` of the newest manifest it installed.
pub fn rollback_allowed(target: Version, floor: Option<Version>, forced: bool) -> bool {
    forced || floor.is_none_or(|f| target >= f)
}

pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// Test-only key from a fixed seed; never used for releases.
    pub fn test_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    pub fn test_public_hex() -> String {
        hex_encode(test_key().verifying_key().as_bytes())
    }

    pub fn sign(bytes: &[u8]) -> String {
        hex_encode(&test_key().sign(bytes).to_bytes())
    }

    pub fn manifest_json(version: &str, rollout: u8) -> Vec<u8> {
        serde_json::to_vec(&Manifest {
            version: version.into(),
            channel: "stable".into(),
            min_version: "1.0.0".into(),
            rollout_percent: rollout,
            installer_url: "https://example.invalid/OpenDesk-Setup.exe".into(),
            sha256: "ab".repeat(32),
            size: 10,
        })
        .unwrap()
    }

    #[test]
    fn versions_parse_and_order() {
        assert_eq!(Version::parse("1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("v1.10.0"), Some(Version(1, 10, 0)));
        assert!(Version(1, 10, 0) > Version(1, 9, 9));
        for bad in ["1.2", "1.2.3.4", "1.2.x", "", "1.2.3-rc1"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn signed_manifest_verifies() {
        let bytes = manifest_json("9.0.0", 100);
        let m = verify(&bytes, &sign(&bytes), &test_public_hex()).unwrap();
        assert_eq!(m.version(), Some(Version(9, 0, 0)));
    }

    #[test]
    fn tampered_or_unsigned_manifests_are_rejected() {
        let bytes = manifest_json("9.0.0", 100);
        let sig = sign(&bytes);
        let mut tampered = bytes.clone();
        tampered[5] ^= 1;
        assert_eq!(verify(&tampered, &sig, &test_public_hex()), Err(VerifyError::BadSignature));
        assert_eq!(verify(&bytes, "00", &test_public_hex()), Err(VerifyError::BadSignature));
        let other = hex_encode(SigningKey::from_bytes(&[8u8; 32]).verifying_key().as_bytes());
        assert_eq!(verify(&bytes, &sig, &other), Err(VerifyError::BadSignature));
    }

    /// Output of `scripts/sign-manifest.sh` with a throwaway OpenSSL key: the script and the client agree.
    #[test]
    fn manifests_signed_by_the_openssl_script_verify() {
        let public = "ab8951a732142210107fb39c81258f7d0ffedb5c69bf365052d628c432e81691";
        let bytes = concat!(
            r#"{"version":"9.9.9","channel":"stable","min_version":"9.9.9","rollout_percent":50,"#,
            r#""installer_url":"https://example.invalid/s.exe","#,
            r#""sha256":"b5d54c39e66671c9731b9f471e585d8262cd4f54963f0c93082d8dcf334d4c78","size":4}"#,
            "\n"
        );
        let sig = "e21e0bbe8086c4bd8daebb18c6856d136441eb94b5ac8a1304245505f759dd606be7f1b60d659aa6806e69d2f71dde4edcd0bc7e73f587e318ae7ae3fbbb6d02";
        let m = verify(bytes.as_bytes(), sig, public).unwrap();
        assert_eq!((m.version.as_str(), m.rollout_percent, m.size), ("9.9.9", 50, 4));
    }

    #[test]
    fn the_release_key_is_real_and_rejects_other_signers() {
        let bytes = manifest_json("9.0.0", 100);
        assert_eq!(verify(&bytes, &sign(&bytes), &"0".repeat(64)), Err(VerifyError::BadKey));
        // A valid, strong key: a signature from any other key is a bad signature, not a bad key.
        assert_eq!(verify(&bytes, &sign(&bytes), RELEASE_PUBLIC_KEY), Err(VerifyError::BadSignature));
    }

    #[test]
    fn signed_but_unsafe_manifests_are_rejected() {
        let mut m: Manifest = serde_json::from_slice(&manifest_json("9.0.0", 100)).unwrap();
        m.installer_url = "http://example.invalid/setup.exe".into();
        let bytes = serde_json::to_vec(&m).unwrap();
        assert!(matches!(verify(&bytes, &sign(&bytes), &test_public_hex()), Err(VerifyError::Invalid(_))));
    }

    #[test]
    fn rollout_bucket_is_stable_and_spread() {
        assert_eq!(rollout_bucket("123 456 789"), rollout_bucket("123456789"));
        let buckets: std::collections::HashSet<u8> =
            (0..500).map(|i| rollout_bucket(&format!("{:09}", i * 7919))).collect();
        assert!(buckets.len() > 80);
        assert!(buckets.iter().all(|b| *b < 100));
    }

    #[test]
    fn decide_follows_version_rollout_bad_and_pin() {
        let m: Manifest = serde_json::from_slice(&manifest_json("1.1.0", 100)).unwrap();
        let cur = Version(1, 0, 0);
        assert_eq!(decide(&m, cur, "1", &[], None, false), Decision::Install(Version(1, 1, 0)));
        assert_eq!(decide(&m, Version(1, 1, 0), "1", &[], None, false), Decision::UpToDate);
        assert_eq!(decide(&m, Version(1, 2, 0), "1", &[], None, true), Decision::Older);
        let bad = vec!["1.1.0".to_string()];
        assert_eq!(decide(&m, cur, "1", &bad, None, false), Decision::Bad);
        assert_eq!(decide(&m, cur, "1", &bad, None, true), Decision::Install(Version(1, 1, 0)));
        assert_eq!(decide(&m, cur, "1", &[], Some("1.0.0"), true), Decision::Pinned);
        assert_eq!(decide(&m, cur, "1", &[], Some("1.1.0"), false), Decision::Install(Version(1, 1, 0)));

        let none: Manifest = serde_json::from_slice(&manifest_json("1.1.0", 0)).unwrap();
        assert_eq!(decide(&none, cur, "1", &[], None, false), Decision::NotInRollout);
        assert_eq!(decide(&none, cur, "1", &[], None, true), Decision::Install(Version(1, 1, 0)));
    }

    #[test]
    fn decide_target_installs_the_chosen_release_in_either_direction() {
        let m: Manifest = serde_json::from_slice(&manifest_json("1.1.0", 0)).unwrap();
        let v = Version(1, 1, 0);
        assert_eq!(decide_target(&m, Version(1, 0, 0), v, None, false), Decision::Install(v));
        assert_eq!(decide_target(&m, Version(1, 2, 0), v, None, false), Decision::Install(v));
        assert_eq!(decide_target(&m, v, v, None, false), Decision::UpToDate);
        assert_eq!(decide_target(&m, Version(1, 0, 0), Version(1, 2, 0), None, false), Decision::WrongRelease);
        let floor = Some(Version(1, 2, 0));
        assert_eq!(decide_target(&m, Version(1, 3, 0), v, floor, false), Decision::BelowFloor);
        assert_eq!(decide_target(&m, Version(1, 3, 0), v, floor, true), Decision::Install(v));
    }

    #[test]
    fn rollback_below_min_version_needs_force() {
        let floor = Some(Version(1, 1, 0));
        assert!(rollback_allowed(Version(1, 1, 0), floor, false));
        assert!(!rollback_allowed(Version(1, 0, 9), floor, false));
        assert!(rollback_allowed(Version(1, 0, 9), floor, true));
        assert!(rollback_allowed(Version(0, 1, 0), None, false));
    }
}
