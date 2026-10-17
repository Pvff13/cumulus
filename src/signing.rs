//! Signatures on published updates (ed25519).
//!
//! A release ships `cumulus.exe` plus `cumulus.exe.sig`. The signature
//! covers the version number and the exe's SHA-256, so a file can't be
//! swapped for another one or an old release passed off as a new version.
//! Only the public key is built into the app; the private key stays on the
//! maintainer's machine (outside the repository).

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

/// Public half of the release signing key.
pub const RELEASE_KEY: &str = "d96daffbfb65ccf25fa23c98cfa753bfc1e4e303876f38f2a5e18c27fb966c4c";

fn message(version: &str, exe: &[u8]) -> Vec<u8> {
    format!("cumulus-update-v1\n{version}\n{}\n", hex(&Sha256::digest(exe))).into_bytes()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.trim();
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Checks that `exe` is the release `version` signed with the release key.
pub fn verify(version: &str, exe: &[u8], signature_hex: &str) -> Result<(), String> {
    verify_with(RELEASE_KEY, version, exe, signature_hex)
}

fn verify_with(public_hex: &str, version: &str, exe: &[u8], signature_hex: &str) -> Result<(), String> {
    let key = unhex::<32>(public_hex)
        .and_then(|b| VerifyingKey::from_bytes(&b).ok())
        .ok_or("no valid release key is built in")?;
    let sig = unhex::<64>(signature_hex).ok_or("the signature file is malformed")?;
    key.verify_strict(&message(version, exe), &Signature::from_bytes(&sig))
        .map_err(|_| "the download's signature doesn't match; not installing it".to_string())
}

/// Maintainer tools (debug builds only): create the key pair and sign releases.
#[cfg(debug_assertions)]
pub mod tools {
    use ed25519_dalek::{Signer, SigningKey};
    use std::path::Path;

    /// Writes a new private key to `path` (refusing to overwrite) and returns
    /// the public key to paste into `RELEASE_KEY`.
    pub fn keygen(path: &Path) -> Result<String, String> {
        if path.exists() {
            return Err(format!("{} already exists; not overwriting a signing key", path.display()));
        }
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| e.to_string())?;
        let key = SigningKey::from_bytes(&seed);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, super::hex(&seed)).map_err(|e| e.to_string())?;
        Ok(super::hex(key.verifying_key().as_bytes()))
    }

    /// Signs `exe` as release `version`; returns the signature as hex.
    pub fn sign(key_path: &Path, exe: &Path, version: &str) -> Result<String, String> {
        let seed = std::fs::read_to_string(key_path).map_err(|e| format!("read key: {e}"))?;
        let seed = super::unhex::<32>(&seed).ok_or("key file is malformed")?;
        let data = std::fs::read(exe).map_err(|e| format!("read exe: {e}"))?;
        let sig = SigningKey::from_bytes(&seed).sign(&super::message(version, &data));
        Ok(super::hex(&sig.to_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn signatures_bind_version_and_bytes() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public = hex(key.verifying_key().as_bytes());
        let exe = b"pretend this is an exe".to_vec();
        let sig = hex(&key.sign(&message("0.2.0", &exe)).to_bytes());
        assert!(verify_with(&public, "0.2.0", &exe, &sig).is_ok());
        // A different version, changed bytes or another key all fail.
        assert!(verify_with(&public, "0.3.0", &exe, &sig).is_err());
        let mut tampered = exe.clone();
        tampered[0] ^= 1;
        assert!(verify_with(&public, "0.2.0", &tampered, &sig).is_err());
        let other = hex(SigningKey::from_bytes(&[8u8; 32]).verifying_key().as_bytes());
        assert!(verify_with(&other, "0.2.0", &exe, &sig).is_err());
        assert!(verify_with(&public, "0.2.0", &exe, "zz").is_err());
    }
}
