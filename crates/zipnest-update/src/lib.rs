//! ZipNest's update path: find a newer release, download it, verify it, and
//! hand it to the NSIS installer.
//!
//! Split out of `apps/zipnest-native/src/main.rs` (which was ~6500 lines and had
//! exactly one `ureq` call in it) so the parts that can be proven — version
//! comparison, manifest validation, signature verification, the resume decision,
//! the install-mode check, exit-code mapping — have unit tests that do not need
//! an egui context.
//!
//! ## What this crate guarantees
//!
//! Everything it returns has been checked against a manifest whose signature
//! verifies against a public key compiled into the exe. The transport is
//! therefore untrusted by construction: Gitee, `gh-proxy`, `ghfast`, jsDelivr
//! and GitHub direct are all just pipes, and the worst a hostile pipe can do is
//! refuse to serve. See [`verify`] for the trust model and [`source`] for the
//! measured ordering of those pipes.
//!
//! ## What it deliberately does not do
//!
//! * No telemetry, no identifiers, no cookies, no account.
//! * No silent install without a visible UAC prompt — the binaries are not
//!   Authenticode-signed, and pretending otherwise would be worse than the
//!   prompt.
//! * No install over a portable copy: [`install::detect`] refuses, because
//!   installing into Program Files would leave the user's portable exe at the
//!   old version.

pub mod download;
pub mod install;
pub mod manifest;
pub mod source;
pub mod verify;
pub mod version;

pub use manifest::SIG_LEN;

use std::time::Duration;

/// Everything that can go wrong, in terms the UI can turn into one sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// The manifest could not be fetched from any source.
    Network { source: String, detail: String },
    /// Every source answered, but none produced a valid manifest.
    Manifest(String),
    /// The detached signature did not verify. **Always fatal** — a manifest
    /// that fails this was tampered with or the build has the wrong key.
    Signature(String),
    /// A download stopped short. Retryable: the `.part` is kept.
    Incomplete { source: String, got: u64, want: u64 },
    /// A complete download whose hash was wrong. Not retryable from the same
    /// source; the caller moves to the next one.
    HashMismatch { source: String, got: String, want: String },
    /// No source produced a usable file.
    Source(String),
    /// The user pressed 取消.
    Cancelled,
    Io(String),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::Network { source, detail } => write!(f, "network {source}: {detail}"),
            UpdateError::Manifest(m) => write!(f, "manifest: {m}"),
            UpdateError::Signature(m) => write!(f, "signature: {m}"),
            UpdateError::Incomplete { source, got, want } => {
                write!(f, "incomplete {source}: {got}/{want}")
            }
            UpdateError::HashMismatch { source, .. } => write!(f, "hash mismatch {source}"),
            UpdateError::Source(m) => write!(f, "no source: {m}"),
            UpdateError::Cancelled => write!(f, "cancelled"),
            UpdateError::Io(m) => write!(f, "io: {m}"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl UpdateError {
    /// i18n key for the dialog. Every key exists in `apps/zipnest-native`'s
    /// zh-CN and en-US tables; the app never shows a raw English error string.
    pub fn message_key(&self) -> &'static str {
        match self {
            UpdateError::Network { .. } => "update.error.network",
            UpdateError::Manifest(_) => "update.error.manifest",
            UpdateError::Signature(_) => "update.error.signature",
            UpdateError::Incomplete { .. } => "update.error.incomplete",
            UpdateError::HashMismatch { .. } => "update.error.hash",
            UpdateError::Source(_) => "update.error.source",
            UpdateError::Cancelled => "update.error.cancelled",
            UpdateError::Io(_) => "update.error.io",
        }
    }
}

/// A newer release, ready to offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub version: String,
    pub notes_zh: String,
    pub notes_en: String,
    pub installer_size: u64,
    /// False when the installed version is older than the manifest's
    /// `min_version`: the in-app install path has not been tested across that
    /// gap, so the app offers the download page instead.
    pub in_app_install: bool,
}

impl Offer {
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

/// The agent used for manifest fetches.
///
/// Short budgets on purpose: a check is a courtesy that runs at startup, and on
/// a line that blackholes packets an unbounded one leaves the status-bar
/// spinner running forever (the v0.4.10 bug). `Proxy::try_from_env()` is
/// `ureq`'s default, so `HTTPS_PROXY`/`ALL_PROXY` work with no code — which is
/// what a Clash/v2ray user in China needs.
pub fn manifest_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(8)))
        .timeout_per_read(Some(Duration::from_secs(10)))
        .timeout_global(Some(source::MANIFEST_BUDGET))
        .max_redirects(2)
        .max_redirects_will_error(true)
        .build()
        .into()
}

/// Fetch and verify the manifest from the first source that works.
///
/// Returns the manifest, the raw bytes it was parsed from, and the source that
/// answered. The bytes are returned because the signature covers exactly them.
pub fn fetch_manifest(agent: &ureq::Agent, user_agent: &str) -> Result<(manifest::Manifest, Vec<u8>, String), UpdateError> {
    let mut last = UpdateError::Source("no manifest source".into());
    for (m_url, s_url) in source::manifest_sources() {
        // Both files, from the same source: a manifest from one mirror with a
        // signature from another is a mismatch by definition.
        let bytes = match get_small(agent, &m_url, user_agent) {
            Ok(b) => b,
            Err(e) => {
                last = e;
                continue;
            }
        };
        let sig = match get_small(agent, &s_url, user_agent) {
            Ok(b) => b,
            Err(e) => {
                last = e;
                continue;
            }
        };
        // Signature first, parse second: never hand unverified bytes to serde.
        if let Err(e) = verify::verify_manifest(&bytes, &sig) {
            last = e;
            continue;
        }
        match manifest::Manifest::parse(&bytes) {
            Ok(m) => return Ok((m, bytes, m_url)),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn get_small(agent: &ureq::Agent, url: &str, user_agent: &str) -> Result<Vec<u8>, UpdateError> {
    let mut res = agent
        .get(url)
        .header("User-Agent", user_agent)
        // A cache-busting hint for the CDN-backed mirrors, which otherwise serve
        // a branch ref for up to 12 hours.
        .header("Cache-Control", "no-cache")
        .call()
        .map_err(|e| UpdateError::Network {
            source: url.to_string(),
            detail: download::describe_ureq(&e),
        })?;
    res.body_mut()
        .with_config()
        // A manifest is ~600 bytes; anything near this is a wrong URL or an
        // error page, and must not be read into memory.
        .limit(64 * 1024)
        .read_to_vec()
        .map_err(|e| UpdateError::Network {
            source: url.to_string(),
            detail: e.to_string(),
        })
}

/// Is there a newer release than `current`?
///
/// Returns `None` when the installed version is already current, so the caller
/// has one place where "nothing to offer" is expressed.
pub fn check(
    agent: &ureq::Agent,
    user_agent: &str,
    current: &str,
    lang: &str,
) -> Result<Option<Offer>, UpdateError> {
    let (manifest, _bytes, _src) = fetch_manifest(agent, user_agent)?;
    if !version::is_newer(current, &manifest.version) {
        return Ok(None);
    }
    let Some(installer) = manifest.installer() else {
        return Ok(None);
    };
    let in_app_install = may_install_in_app(current, &manifest.min_version);
    let notes = manifest.notes(lang).to_string();
    Ok(Some(Offer {
        version: manifest.version.clone(),
        notes_zh: manifest.notes_zh.clone(),
        notes_en: notes,
        installer_size: installer.size,
        in_app_install,
    }))
}

/// May a build on `current` use the in-app downloader for a release whose
/// manifest says `min_version`?
///
/// True when the manifest names no floor, or when `current` is at least as
/// new as that floor. `is_newer(a, b)` answers "is b newer than a", so the
/// condition is `!is_newer(current, min_version)` - note the argument
/// order. Getting it backwards (`!is_newer(min_version, current)`) inverts
/// the meaning: with the shipped `min_version` of 0.4.0 it told *every*
/// user that the upgrade gap was too wide, so the in-app path was never
/// offered at all and mainland users were pushed back to the GitHub page
/// this redesign exists to avoid. It is a function with tests for that
/// reason.
pub fn may_install_in_app(current: &str, min_version: &str) -> bool {
    min_version.trim().is_empty() || !version::is_newer(current, min_version)
}

/// Format bytes for the progress dialog ("5.1 MB").
pub fn pretty_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.1} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_carry_a_key_the_ui_can_translate() {
        let cases = [
            UpdateError::Network {
                source: "x".into(),
                detail: "y".into(),
            },
            UpdateError::Manifest("m".into()),
            UpdateError::Signature("s".into()),
            UpdateError::Incomplete {
                source: "x".into(),
                got: 1,
                want: 2,
            },
            UpdateError::HashMismatch {
                source: "x".into(),
                got: "a".into(),
                want: "b".into(),
            },
            UpdateError::Source("x".into()),
            UpdateError::Cancelled,
            UpdateError::Io("x".into()),
        ];
        for c in cases {
            assert!(c.message_key().starts_with("update.error."), "{c:?}");
        }
    }

    #[test]
    fn sizes_read_the_way_a_user_expects() {
        assert_eq!(pretty_size(512), "512 B");
        assert_eq!(pretty_size(2048), "2 KB");
        assert_eq!(pretty_size(5_137_077), "4.9 MB");
        assert_eq!(pretty_size(6_291_746), "6.0 MB");
        assert_eq!(pretty_size(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    #[test]
    fn offer_notes_fall_back_between_languages() {
        let mut o = Offer {
            version: "0.5.0".into(),
            notes_zh: "修复".into(),
            notes_en: String::new(),
            installer_size: 1,
            in_app_install: true,
        };
        assert_eq!(o.notes("en-US"), "修复");
        o.notes_zh.clear();
        assert_eq!(o.notes("zh-CN"), "");
    }

    #[test]
    fn a_user_at_or_above_min_version_gets_the_in_app_installer() {
        // The exact case that was broken: ship.ps1 writes min_version 0.4.0,
        // and a user on 0.5.0 being offered 0.5.1 must get the downloader.
        assert!(may_install_in_app("0.5.0", "0.4.0"));
        // Equal is fine too - min_version means "at least this".
        assert!(may_install_in_app("0.4.0", "0.4.0"));
        assert!(may_install_in_app("0.6.2", "0.5.0"));
    }

    #[test]
    fn a_user_below_min_version_is_sent_to_the_download_page() {
        // The one case the field exists for: 0.4.39 cannot verify itself, so
        // the first verifying release has to be installed by hand.
        assert!(!may_install_in_app("0.4.39", "0.5.0"));
        assert!(!may_install_in_app("0.4.0", "0.5.0"));
        assert!(!may_install_in_app("0.3.9", "0.4.0"));
    }

    #[test]
    fn an_absent_min_version_means_no_floor() {
        // A manifest without the field must not lock anybody out.
        assert!(may_install_in_app("0.1.0", ""));
        assert!(may_install_in_app("0.1.0", "   "));
    }

    #[test]
    fn a_prerelease_installed_build_is_not_treated_as_older() {
        // A beta build parses to the same segments as its release, so a beta
        // tester is not sent to the download page by a floor they already meet.
        assert!(may_install_in_app("0.5.0-beta.1", "0.5.0"));
    }
}
