//! Update check and verification against GitHub Releases.
//!
//! A release carries `manifest.json` (version + SHA-256 of every file) and `manifest.json.sig`
//! (raw 64-byte Ed25519 signature over the exact manifest bytes). Nothing is installed unless the
//! signature verifies against the key compiled into this binary and every file hash matches.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const REPO: &str = "Codings-Biggest-Fan/kestrel";
const MAX_DOWNLOAD: u64 = 64 << 20;

/// Hex Ed25519 public key, set at build time by the release workflow.
pub const UPDATE_PUBKEY: Option<&str> = option_env!("KESTREL_UPDATE_PUBKEY");

#[derive(Debug)]
pub enum Error {
    Http(String),
    NoSigningKey,
    BadSignature,
    BadManifest(String),
    HashMismatch(String),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "download failed: {e}"),
            Error::NoSigningKey => write!(f, "this build has no update key; updates are disabled"),
            Error::BadSignature => write!(f, "update signature is invalid"),
            Error::BadManifest(e) => write!(f, "bad update manifest: {e}"),
            Error::HashMismatch(n) => write!(f, "{n} does not match the signed manifest"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub sha256: String,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

pub struct Release {
    pub version: String,
    assets: Vec<GhAsset>,
}

/// Compare dotted numeric versions; a leading `v` is ignored.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-'])
            .map_while(|p| p.parse().ok())
            .collect()
    };
    parse(candidate) > parse(current)
}

fn get(url: &str) -> Result<Vec<u8>> {
    let resp = ureq::get(url)
        .set(
            "User-Agent",
            concat!("kestrel-updater/", env!("CARGO_PKG_VERSION")),
        )
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| Error::Http(e.to_string()))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_DOWNLOAD)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

/// Latest published release, if newer than this build.
pub fn check() -> Result<Option<Release>> {
    let body = get(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    ))?;
    let r: GhRelease =
        serde_json::from_slice(&body).map_err(|e| Error::BadManifest(e.to_string()))?;
    let version = r.tag_name.trim_start_matches('v').to_string();
    Ok(
        is_newer(&version, kestrel_core::VERSION).then_some(Release {
            version,
            assets: r.assets,
        }),
    )
}

pub fn verify_manifest(manifest: &[u8], sig: &[u8], pubkey_hex: &str) -> Result<Manifest> {
    let key: [u8; 32] = hex(pubkey_hex)
        .and_then(|k| k.try_into().ok())
        .ok_or(Error::NoSigningKey)?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| Error::NoSigningKey)?;
    let sig = Signature::from_slice(sig).map_err(|_| Error::BadSignature)?;
    key.verify(manifest, &sig)
        .map_err(|_| Error::BadSignature)?;
    serde_json::from_slice(manifest).map_err(|e| Error::BadManifest(e.to_string()))
}

fn hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    s.len().is_multiple_of(2).then(|| {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
            .collect()
    })?
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Download, verify and stage a release in `dir/<version>`. Returns the staged directory.
pub fn stage(rel: &Release, dir: &Path) -> Result<PathBuf> {
    let pubkey = UPDATE_PUBKEY.ok_or(Error::NoSigningKey)?;
    let url = |name: &str| {
        rel.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
            .ok_or_else(|| Error::BadManifest(format!("release has no {name}")))
    };
    let manifest_bytes = get(&url("manifest.json")?)?;
    let sig = get(&url("manifest.json.sig")?)?;
    let manifest = verify_manifest(&manifest_bytes, &sig, pubkey)?;
    if manifest.version != rel.version {
        return Err(Error::BadManifest(
            "manifest version differs from the release tag".into(),
        ));
    }
    let out = dir.join(&manifest.version);
    std::fs::create_dir_all(&out)?;
    for f in &manifest.files {
        if f.name.contains(['/', '\\']) || f.name.starts_with('.') {
            return Err(Error::BadManifest(format!("unsafe file name {}", f.name)));
        }
        let data = get(&url(&f.name)?)?;
        if !sha256_hex(&data).eq_ignore_ascii_case(&f.sha256) {
            return Err(Error::HashMismatch(f.name.clone()));
        }
        std::fs::write(out.join(&f.name), data)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn keypair() -> (SigningKey, String) {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = sk
            .verifying_key()
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        (sk, pk)
    }

    #[test]
    fn versions() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn signed_manifest_verifies() {
        let (sk, pk) = keypair();
        let m = br#"{"version":"0.2.0","files":[{"name":"kestrel-ui.exe","sha256":"00"}]}"#;
        let sig = sk.sign(m).to_bytes();
        assert_eq!(verify_manifest(m, &sig, &pk).unwrap().version, "0.2.0");
    }

    #[test]
    fn tampered_manifest_is_rejected() {
        let (sk, pk) = keypair();
        let sig = sk.sign(br#"{"version":"0.2.0","files":[]}"#).to_bytes();
        assert!(matches!(
            verify_manifest(br#"{"version":"6.6.6","files":[]}"#, &sig, &pk),
            Err(Error::BadSignature)
        ));
    }

    #[test]
    fn sha256() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
