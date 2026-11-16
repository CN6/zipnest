//! The signed update manifest (`update.json`), and its detached signature.
//!
//! Why a file in git instead of the GitHub REST API, which the app used until
//! v0.4.39: an unauthenticated `api.github.com` call costs one of **60 requests
//! per hour per IP**, and in China that budget belongs to the carrier's NAT IP,
//! shared with every other GitHub user on it. Measured on the maintainer's own
//! line, 53 requests exhausted it and every later call returned 403 for 13
//! minutes — an update check failing for a reason that has nothing to do with
//! the firewall. A static file served from Gitee's raw endpoint answered 15 out
//! of 15 attempts in 0.34–0.61 s with no rate limit at all.
//!
//! The file is signed, so *every* transport below is untrusted by construction:
//! a mirror that substitutes the manifest cannot produce a valid signature, and
//! a mirror that substitutes the installer cannot match the SHA-256 the signed
//! manifest carries. Adding, reordering or removing mirrors is therefore a
//! shipping-constant change, not a security review.

use serde::{Deserialize, Serialize};

use crate::UpdateError;

/// One downloadable artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    /// File name as published, e.g. `ZipNest_0.5.0_x64-setup.exe`. The download
    /// URL is built from the version + this name, so the manifest deliberately
    /// carries no URLs: switching mirrors must not require a release.
    pub name: String,
    pub size: u64,
    /// Lowercase hex SHA-256 of the whole file.
    pub sha256: String,
}

/// What a newer release tells the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// Oldest version this release will update *from*. An installed version
    /// below it is offered the browser download instead of the in-app install,
    /// because the upgrade path has not been tested.
    #[serde(default)]
    pub min_version: String,
    /// One short paragraph for the update dialog, in the app's two languages.
    #[serde(default)]
    pub notes_zh: String,
    #[serde(default)]
    pub notes_en: String,
    /// `YYYY-MM-DD`, informational only.
    #[serde(default)]
    pub date: String,
    pub assets: Vec<Asset>,
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, UpdateError> {
        let m: Manifest =
            serde_json::from_slice(bytes).map_err(|e| UpdateError::Manifest(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    /// Reject anything we could not act on, at parse time rather than halfway
    /// through a download.
    fn validate(&self) -> Result<(), UpdateError> {
        if self.version.trim().is_empty() {
            return Err(UpdateError::Manifest("no version".into()));
        }
        if self.assets.is_empty() {
            return Err(UpdateError::Manifest("no assets".into()));
        }
        for a in &self.assets {
            if a.name.is_empty()
                || a.name.contains('/')
                || a.name.contains('\\')
                || a.name.contains("..")
            {
                return Err(UpdateError::Manifest(format!("bad asset name: {}", a.name)));
            }
            if a.size == 0 {
                return Err(UpdateError::Manifest(format!("zero size: {}", a.name)));
            }
            if a.sha256.len() != 64 || !a.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(UpdateError::Manifest(format!("bad sha256: {}", a.name)));
            }
        }
        Ok(())
    }

    /// The installer for this release. The portable zip is published in the
    /// same manifest but never installed by the app.
    pub fn installer(&self) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name.ends_with("-setup.exe"))
    }

    pub fn notes(&self, lang: &str) -> &str {
        if lang == "en-US" {
            if self.notes_en.is_empty() {
                &self.notes_zh
            } else {
                &self.notes_en
            }
        } else if self.notes_zh.is_empty() {
            &self.notes_en
        } else {
            &self.notes_zh
        }
    }
}

/// A detached 64-byte Ed25519 signature over the manifest's **raw bytes**.
///
/// Raw bytes, not a re-serialised struct: the signature must cover exactly what
/// was published, or a byte-order change in serde would invalidate every
/// manifest ever shipped.
pub type Signature = Vec<u8>;

/// Signature length for Ed25519.
pub const SIG_LEN: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{
        "version": "0.5.0",
        "min_version": "0.4.0",
        "date": "2026-11-20",
        "notes_zh": "修好了几处解压问题。",
        "notes_en": "A few extraction fixes.",
        "assets": [
            {"name":"ZipNest_0.5.0_x64-setup.exe","size":5137077,
             "sha256":"64db71098d0ce0abe713fdbd1ff7f433ec24dafb73fa246912fa3c11b4a1d865"},
            {"name":"ZipNest_0.5.0_x64_portable.zip","size":6291746,
             "sha256":"35beadacefa1ca54038a2654dc756278fcbc61e53f646797426284cb50fb4603"}
        ]
    }"#;

    #[test]
    fn a_good_manifest_parses_and_finds_the_installer() {
        let m = Manifest::parse(GOOD.as_bytes()).unwrap();
        assert_eq!(m.version, "0.5.0");
        assert_eq!(m.assets.len(), 2);
        let inst = m.installer().unwrap();
        assert_eq!(inst.name, "ZipNest_0.5.0_x64-setup.exe");
        assert_eq!(inst.size, 5137077);
    }

    #[test]
    fn the_portable_zip_is_never_mistaken_for_the_installer() {
        let m = Manifest::parse(GOOD.as_bytes()).unwrap();
        assert!(!m.installer().unwrap().name.contains("portable"));
    }

    #[test]
    fn notes_fall_back_to_the_other_language() {
        let mut m = Manifest::parse(GOOD.as_bytes()).unwrap();
        assert_eq!(m.notes("zh-CN"), "修好了几处解压问题。");
        assert_eq!(m.notes("en-US"), "A few extraction fixes.");
        m.notes_en.clear();
        assert_eq!(m.notes("en-US"), "修好了几处解压问题。");
        m.notes_zh.clear();
        assert_eq!(m.notes("zh-CN"), "");
    }

    #[test]
    fn broken_json_is_rejected() {
        assert!(Manifest::parse(b"{").is_err());
        assert!(Manifest::parse(b"").is_err());
        assert!(Manifest::parse(br#"{"version":"0.5.0"}"#).is_err());
    }

    #[test]
    fn a_manifest_without_assets_is_rejected() {
        let j = r#"{"version":"0.5.0","assets":[]}"#;
        assert!(Manifest::parse(j.as_bytes()).is_err());
    }

    #[test]
    fn an_asset_with_a_bad_hash_is_rejected() {
        let j = r#"{"version":"0.5.0","assets":[
            {"name":"a.exe","size":1,"sha256":"deadbeef"}]}"#;
        assert!(Manifest::parse(j.as_bytes()).is_err());

        let j = r#"{"version":"0.5.0","assets":[
            {"name":"a.exe","size":1,"sha256":"zzdb71098d0ce0abe713fdbd1ff7f433ec24dafb73fa246912fa3c11b4a1d865"}]}"#;
        assert!(Manifest::parse(j.as_bytes()).is_err());
    }

    #[test]
    fn an_asset_name_cannot_escape_the_download_directory() {
        for name in ["../evil.exe", "sub/dir.exe", "sub\\dir.exe", ".."] {
            let j = format!(
                r#"{{"version":"0.5.0","assets":[{{"name":"{name}","size":1,
                 "sha256":"64db71098d0ce0abe713fdbd1ff7f433ec24dafb73fa246912fa3c11b4a1d865"}}]}}"#
            );
            assert!(
                Manifest::parse(j.as_bytes()).is_err(),
                "accepted {name:?}"
            );
        }
    }

    #[test]
    fn a_zero_sized_asset_is_rejected() {
        let j = r#"{"version":"0.5.0","assets":[{"name":"a.exe","size":0,
                   "sha256":"64db71098d0ce0abe713fdbd1ff7f433ec24dafb73fa246912fa3c11b4a1d865"}]}"#;
        assert!(Manifest::parse(j.as_bytes()).is_err());
    }
}
