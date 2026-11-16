//! Signing half of the update trust model.
//!
//! The verifier lives in `crates/zipnest-update` and only ever sees a public
//! key compiled into the exe. This crate holds the private-key operations, is
//! `publish = false`, and is never part of the installer payload.
//!
//! ## Why `ring` and not `openssl` or minisign
//!
//! `ring::signature::Ed25519KeyPair::generate_pkcs8` emits a **PKCS#8 v2**
//! document, which is the only form `Ed25519KeyPair::from_pkcs8` accepts without
//! losing the public/private consistency check. `openssl genpkey -algorithm
//! ED25519` emits v1, so a key made that way silently needs
//! `from_pkcs8_maybe_unchecked` on the signing side — a footgun for one line of
//! saved effort. Using the same library for signing and verification also means
//! there is exactly one Ed25519 implementation in the project.

use std::path::{Path, PathBuf};

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Default location of the private key: outside the repository, next to the
/// Gitee token.
pub fn default_key_path() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(".zipnest").join("update-key.p8")
}

/// Generate a key pair, write the private half to `path`, and return
/// `(public key hex, path)`.
///
/// Never overwrites an existing key: replacing a published key means every
/// installed copy of ZipNest stops accepting updates, which must be a deliberate
/// act (delete the file first), not a re-run of a command.
pub fn generate_key(path: &Path) -> Result<([u8; 32], PathBuf), String> {
    if path.exists() {
        return Err(format!(
            "{} already exists — refusing to replace a published key. \
             Delete it deliberately if you really mean to rotate.",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|e| e.to_string())?;
    std::fs::write(path, pkcs8.as_ref()).map_err(|e| e.to_string())?;
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|e| e.to_string())?;
    let mut pk = [0u8; 32];
    pk.copy_from_slice(kp.public_key().as_ref());
    Ok((pk, path.to_path_buf()))
}

/// Sign `message` with the key stored at `path`.
pub fn sign_with_key_file(path: &Path, message: &[u8]) -> Result<Vec<u8>, String> {
    let pkcs8 = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let kp = Ed25519KeyPair::from_pkcs8(&pkcs8)
        .map_err(|e| format!("{} is not a PKCS#8 v2 Ed25519 key: {e}", path.display()))?;
    let sig = kp.sign(message);
    Ok(sig.as_ref().to_vec())
}

/// The public key of the pair at `path`, so the release script can check it
/// against the one compiled into the app before publishing.
pub fn public_key_of(path: &Path) -> Result<[u8; 32], String> {
    let pkcs8 = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let kp = Ed25519KeyPair::from_pkcs8(&pkcs8)
        .map_err(|e| format!("{} is not a PKCS#8 v2 Ed25519 key: {e}", path.display()))?;
    let mut pk = [0u8; 32];
    pk.copy_from_slice(kp.public_key().as_ref());
    Ok(pk)
}

/// Lowercase hex, the form `verify.rs`'s `UPDATE_PUBKEY` is written in.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Parse the hex form back into bytes — used by the release script to prove
/// that the key it signed with is the key the app will verify with.
pub fn unhex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err("odd number of hex digits".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16).ok_or("not hex")?;
        let lo = (pair[1] as char).to_digit(16).ok_or("not hex")?;
        out.push((hi * 16 + lo) as u8);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_key() -> (tempfile::TempDir, PathBuf, [u8; 32]) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k.p8");
        let (pk, _) = generate_key(&path).unwrap();
        (dir, path, pk)
    }

    #[test]
    fn a_generated_key_signs_and_verifies_with_its_own_public_key() {
        let (_d, path, pk) = tmp_key();
        let msg = br#"{"version":"0.5.0","assets":[]}"#;
        let sig = sign_with_key_file(&path, msg).unwrap();
        assert_eq!(sig.len(), 64);
        zipnest_update::verify::verify_with(&pk, msg, &sig).unwrap();
    }

    #[test]
    fn one_changed_byte_fails_verification() {
        let (_d, path, pk) = tmp_key();
        let msg = b"manifest bytes";
        let sig = sign_with_key_file(&path, msg).unwrap();
        let mut tampered = msg.to_vec();
        tampered[0] ^= 1;
        assert!(zipnest_update::verify::verify_with(&pk, &tampered, &sig).is_err());
    }

    #[test]
    fn the_public_key_of_a_key_file_matches_what_generation_returned() {
        let (_d, path, pk) = tmp_key();
        assert_eq!(public_key_of(&path).unwrap(), pk);
    }

    #[test]
    fn generating_over_an_existing_key_is_refused() {
        let (_d, path, _) = tmp_key();
        let err = generate_key(&path).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn a_garbage_key_file_is_reported_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.p8");
        std::fs::write(&path, b"not a key").unwrap();
        let err = sign_with_key_file(&path, b"m").unwrap_err();
        assert!(err.contains("PKCS#8"), "{err}");
    }

    #[test]
    fn hex_round_trips() {
        let pk = [0xde, 0xad, 0xbe, 0xef];
        assert_eq!(hex(&pk), "deadbeef");
        assert_eq!(unhex("deadbeef").unwrap(), pk.to_vec());
        assert_eq!(unhex(" DEADBEEF ").unwrap(), pk.to_vec());
        assert!(unhex("abc").is_err());
        assert!(unhex("zz").is_err());
    }

    #[test]
    fn the_shipped_public_key_is_thirty_two_bytes_of_hex() {
        // Whatever is compiled into the app must be expressible in the same hex
        // form the release script prints, or a rotation goes wrong quietly.
        let hexed = hex(&zipnest_update::verify::UPDATE_PUBKEY);
        assert_eq!(hexed.len(), 64);
        assert_eq!(
            unhex(&hexed).unwrap(),
            zipnest_update::verify::UPDATE_PUBKEY.to_vec()
        );
    }
}
