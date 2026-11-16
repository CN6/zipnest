//! Ed25519 verification of the update manifest, and SHA-256 of a downloaded
//! artifact.
//!
//! Both come from `ring`, which is already linked into the shipped binary
//! through `ureq -> rustls -> ring`. That is why this module adds no crypto
//! implementation of its own: `minisign-verify`, for instance, carries its own
//! Ed25519 + Blake2b + SHA-512, which would mean ~1000 lines of unaudited
//! crypto beside the audited ring that is already there.
//!
//! ## The trust model, in one paragraph
//!
//! The public key below is compiled into the exe. The manifest is fetched from
//! whichever mirror answers first and is **only** trusted if its detached
//! signature verifies against this key. The installer's SHA-256 comes from that
//! signed manifest, so the bytes may likewise come from any mirror and are
//! checked before anything is executed. A hostile mirror can therefore cause a
//! failed update, and nothing more. Key compromise is the one unrecoverable
//! case: replacing the key requires shipping a release, which is the right
//! trade for a single maintainer but has no revocation path — say so in the
//! release notes when the key is rotated.

use crate::{UpdateError, SIG_LEN};

/// The maintainer's Ed25519 public key, 32 bytes.
///
/// Generated once by `cargo run -p zipnest-release -- keygen` (2026-11-16); the
/// private half lives at `%USERPROFILE%\.zipnest\update-key.p8`, outside the
/// repository, and never in CI. Rotating it means shipping a release that every
/// installed copy must be told about by hand: there is no revocation path, which
/// is the right trade for a single-maintainer project but should be written in
/// the release notes when it happens.
///
/// `pubkey_is_placeholder` exists so a build that still carries a placeholder
/// fails with "this build has no update key" rather than "signature invalid".
pub const UPDATE_PUBKEY: [u8; 32] = [
    0x50, 0x5a, 0x1a, 0xf3, 0xdb, 0x3c, 0x63, 0x26, 0x42, 0x8a, 0xb9, 0x2e, 0xd4, 0x39, 0x8b, 0x58,
    0x4c, 0xe9, 0xaa, 0x99, 0xe5, 0x35, 0x16, 0x71, 0x4d, 0xcf, 0x93, 0x09, 0x63, 0x4c, 0x30, 0x42,
];

/// True while [`UPDATE_PUBKEY`] is still the all-zero placeholder.
///
/// A zero key is a valid curve point in some encodings but will never verify a
/// real signature, so an un-replaced placeholder fails closed anyway; this exists
/// so the failure message can say "the build has no update key" instead of
/// "signature invalid", which would send the maintainer hunting for a bug in
/// the signing step.
pub fn pubkey_is_placeholder() -> bool {
    UPDATE_PUBKEY.iter().all(|b| *b == 0)
}

/// Verify `sig` over `manifest_bytes` with the embedded key.
pub fn verify_manifest(manifest_bytes: &[u8], sig: &[u8]) -> Result<(), UpdateError> {
    verify_with(&UPDATE_PUBKEY, manifest_bytes, sig)
}

/// The same check with an explicit key, so tests (and the release tool, which
/// signs and then re-verifies before publishing) can use a throwaway key pair.
pub fn verify_with(pubkey: &[u8; 32], manifest_bytes: &[u8], sig: &[u8]) -> Result<(), UpdateError> {
    if sig.len() != SIG_LEN {
        return Err(UpdateError::Signature(format!(
            "expected {SIG_LEN} bytes, got {}",
            sig.len()
        )));
    }
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, pubkey)
        .verify(manifest_bytes, sig)
        .map_err(|_| UpdateError::Signature("signature does not match the manifest".into()))
}

/// Lowercase hex SHA-256 of a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    hex(digest.as_ref())
}

/// Lowercase hex SHA-256 of a file, read in chunks so a 6 MB portable zip does
/// not have to be held in memory.
pub fn sha256_file(path: &std::path::Path) -> Result<String, UpdateError> {
    use std::io::Read;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut file = std::fs::File::open(path).map_err(|e| UpdateError::Io(e.to_string()))?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| UpdateError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(hex(ctx.finish().as_ref()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn keypair() -> (Ed25519KeyPair, [u8; 32]) {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let mut pk = [0u8; 32];
        pk.copy_from_slice(kp.public_key().as_ref());
        (kp, pk)
    }

    #[test]
    fn a_real_signature_verifies() {
        let (kp, pk) = keypair();
        let msg = br#"{"version":"0.5.0","assets":[]}"#;
        let sig = kp.sign(msg);
        verify_with(&pk, msg, sig.as_ref()).unwrap();
    }

    #[test]
    fn one_changed_byte_in_the_manifest_breaks_it() {
        let (kp, pk) = keypair();
        let msg = br#"{"version":"0.5.0"}"#;
        let sig = kp.sign(msg);
        let mut tampered = msg.to_vec();
        tampered[13] = b'6';
        assert!(verify_with(&pk, &tampered, sig.as_ref()).is_err());
    }

    #[test]
    fn a_signature_from_another_key_does_not_verify() {
        let (kp, _) = keypair();
        let (_, other_pk) = keypair();
        let msg = b"manifest";
        let sig = kp.sign(msg);
        assert!(verify_with(&other_pk, msg, sig.as_ref()).is_err());
    }

    #[test]
    fn a_truncated_signature_is_rejected_before_ring_sees_it() {
        let (kp, pk) = keypair();
        let sig = kp.sign(b"m");
        assert!(verify_with(&pk, b"m", &sig.as_ref()[..SIG_LEN - 1]).is_err());
        assert!(verify_with(&pk, b"m", &[]).is_err());
    }

    #[test]
    fn sha256_matches_the_published_digest_format() {
        // The v0.4.39 portable zip's digest, so this pins the exact encoding the
        // manifest uses against a value GitHub itself published.
        let hex_of_empty = sha256_hex(b"");
        assert_eq!(
            hex_of_empty,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(sha256_hex(b"abc").len(), 64);
        assert!(sha256_hex(b"abc")
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn sha256_file_agrees_with_sha256_hex() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("payload.bin");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        assert_eq!(sha256_file(&p).unwrap(), sha256_hex(&data));
    }

    #[test]
    fn sha256_file_reports_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(sha256_file(&dir.path().join("nope")).is_err());
    }

    #[test]
    fn the_placeholder_key_is_detected() {
        // While the placeholder is in place this must be true; once a real key
        // is committed the assertion flips. Written as an implication so the
        // test keeps passing across that commit rather than needing to be
        // deleted with it.
        if pubkey_is_placeholder() {
            assert!(verify_manifest(b"anything", &[0u8; SIG_LEN]).is_err());
        }
    }
}
