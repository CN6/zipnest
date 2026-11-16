//! Where the manifest and the installer come from.
//!
//! Measured on the maintainer's line on 2026-11-16 (China Telecom, Hunan), which
//! is the whole reason for this ordering:
//!
//! | source | result |
//! |---|---|
//! | `github.com` release page | intermittently dead — 10 consecutive zero-byte timeouts, then fine |
//! | GitHub's asset CDN | works, but 44–54 KB/s (a 5 MB installer = 2 min) |
//! | `api.github.com` | fast, but 60 requests/hour **per IP** — shared behind CGNAT |
//! | Gitee release attachment | **2.0 MB/s**, anonymous, no rate limit |
//! | gh-proxy.org / ghfast.top | 355–476 KB/s, byte-identical, works while github.com is down |
//!
//! The URLs are built from a version and a file name rather than stored in the
//! manifest, precisely so this list can be edited without shipping a release.
//! Integrity never depends on which one wins: the manifest is signed and the
//! installer is hashed.

use std::time::Duration;

/// GitHub repository (`owner/name`).
pub const GITHUB_REPO: &str = "CN6/zipnest";
/// Gitee mirror (`owner/name`). The namespace is the maintainer's personal
/// account, verified reachable on 2026-11-16; the repo itself is created by the
/// release script on first publish.
pub const GITEE_REPO: &str = "zipnest/zipnest";
/// Branch the manifest lives on in both repositories.
pub const MANIFEST_BRANCH: &str = "master";
/// File names of the manifest and its detached signature, at the repository root.
pub const MANIFEST_NAME: &str = "update.json";
pub const MANIFEST_SIG_NAME: &str = "update.json.sig";

/// Public base URL for tests and for a maintainer pointing a build at a local
/// static directory (`ZIPNEST_UPDATE_BASE=http://127.0.0.1:8000`). When set it
/// replaces the whole candidate list, so an end-to-end run never touches the
/// internet.
pub const BASE_OVERRIDE_ENV: &str = "ZIPNEST_UPDATE_BASE";

/// First line of defence against a slow-but-alive mirror: if no new bytes
/// arrive for this long, give up on the current source and try the next one.
///
/// `ureq`'s `timeout_per_read` does not cover time spent between application
/// reads, and a server dribbling one byte every 25 s would never trip it.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Connection setup (DNS + TCP + TLS) budget for one attempt.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// A single read may take this long before the transport is considered dead.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Overall budget for one manifest fetch. The manifest is ~600 bytes, so a
/// minute means the source is not going to answer usefully.
pub const MANIFEST_BUDGET: Duration = Duration::from_secs(20);

/// A build told to use a local base gets exactly one candidate.
fn override_base() -> Option<String> {
    std::env::var(BASE_OVERRIDE_ENV)
        .ok()
        .as_deref()
        .and_then(normalise_base)
}

/// Trim a base URL and reject a blank one.
///
/// Split out from [`override_base`] so it can be tested without touching the
/// process environment: this module's tests run on parallel threads, and a test
/// that `set_var`s a variable the other tests read is a race waiting to be lost.
/// It was lost — `installer_urls_carry_the_version_and_file_name` failed about
/// one run in twenty with the single local-base URL instead of the real list.
fn normalise_base(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// The `(manifest_url, signature_url)` pairs to try, in order.
pub fn manifest_sources() -> Vec<(String, String)> {
    manifest_sources_from(override_base().as_deref())
}

/// [`manifest_sources`] with the base chosen by the caller (`None` = the real
/// internet, which is what every pure test here uses).
fn manifest_sources_from(base: Option<&str>) -> Vec<(String, String)> {
    if let Some(base) = base {
        return vec![(
            format!("{base}/{MANIFEST_NAME}"),
            format!("{base}/{MANIFEST_SIG_NAME}"),
        )];
    }
    let gh_raw = format!(
        "https://raw.githubusercontent.com/{GITHUB_REPO}/{MANIFEST_BRANCH}/{MANIFEST_NAME}"
    );
    let pairs = vec![
        // Gitee raw: measured 15/15 in 0.34-0.61 s, `max-age=60`, no rate limit.
        format!("https://gitee.com/{GITEE_REPO}/raw/{MANIFEST_BRANCH}/{MANIFEST_NAME}"),
        // The same file through a public accelerator, which also serves
        // raw.githubusercontent.com and stayed up while github.com was down.
        format!("https://gh-proxy.org/{gh_raw}"),
        // jsDelivr: works, but a branch ref carries a 12-hour edge cache, so a
        // fresh release can be invisible here. Last resort for that reason.
        format!("https://cdn.jsdelivr.net/gh/{GITHUB_REPO}@{MANIFEST_BRANCH}/{MANIFEST_NAME}"),
        // Direct, no intermediary: 4 of 5 measured, with 7.7 s and 12 s outliers.
        gh_raw,
    ];
    pairs
        .into_iter()
        .map(|m| {
            let sig = m.replace(MANIFEST_NAME, MANIFEST_SIG_NAME);
            (m, sig)
        })
        .collect()
}

/// Where the installer for `version` may be downloaded from, in order.
///
/// `asset_id` is the GitHub REST asset id, when the caller has one: that
/// endpoint 302s straight to the signed CDN URL and never touches `github.com`.
/// Pass `None` and the GitHub leg is skipped.
pub fn installer_sources(version: &str, file_name: &str, asset_id: Option<u64>) -> Vec<String> {
    installer_sources_from(override_base().as_deref(), version, file_name, asset_id)
}

fn installer_sources_from(
    base: Option<&str>,
    version: &str,
    file_name: &str,
    asset_id: Option<u64>,
) -> Vec<String> {
    if let Some(base) = base {
        return vec![format!("{base}/{file_name}")];
    }
    let mut out = vec![
        // 2 MB/s measured; ~3 s for the setup exe. Does not honour Range, so a
        // failure here restarts the file from zero rather than resuming.
        format!("https://gitee.com/{GITEE_REPO}/releases/download/v{version}/{file_name}"),
        // 476 KB/s and 355 KB/s measured, both Range-capable, both byte-identical
        // to the GitHub digest, and both alive during a github.com outage.
        format!(
            "https://gh-proxy.org/https://github.com/{GITHUB_REPO}/releases/download/v{version}/{file_name}"
        ),
        format!(
            "https://ghfast.top/https://github.com/{GITHUB_REPO}/releases/download/v{version}/{file_name}"
        ),
    ];
    if let Some(id) = asset_id {
        // `Accept: application/octet-stream` is what makes this return bytes
        // instead of JSON. It lives here so no caller can forget it.
        out.push(format!(
            "https://api.github.com/repos/{GITHUB_REPO}/releases/assets/{id}"
        ));
    }
    out
}

/// "This URL needs the octet-stream Accept header" — true only for the REST
/// asset endpoint, which otherwise answers with metadata.
pub fn wants_octet_stream(url: &str) -> bool {
    url.starts_with("https://api.github.com/repos/") && url.contains("/releases/assets/")
}

/// Which source index answered, for the progress UI ("正在从第 2 个源下载…").
pub fn describe_source(index: usize, total: usize) -> String {
    format!("{}/{}", index + 1, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every test here is pure: it calls the `*_from(Some(..))` / `*_from(None)`
    // forms with the base spelled out, so none of them reads or writes
    // `ZIPNEST_UPDATE_BASE`. An earlier version set that variable in one test
    // and read it in the others, and `cargo test` runs them on parallel threads
    // of one process — `installer_urls_carry_the_version_and_file_name` lost the
    // race about one run in twenty.

    #[test]
    fn the_override_base_replaces_every_source() {
        let base = normalise_base("  http://127.0.0.1:8000/  ");
        assert_eq!(base.as_deref(), Some("http://127.0.0.1:8000"));

        let m = manifest_sources_from(base.as_deref());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].0, "http://127.0.0.1:8000/update.json");
        assert_eq!(m[0].1, "http://127.0.0.1:8000/update.json.sig");

        let d =
            installer_sources_from(base.as_deref(), "0.5.0", "setup.exe", Some(42));
        assert_eq!(d, vec!["http://127.0.0.1:8000/setup.exe".to_string()]);
    }

    #[test]
    fn a_blank_base_is_not_an_override() {
        // An exported-but-empty variable must not turn the source list into a
        // single relative URL.
        assert_eq!(normalise_base(""), None);
        assert_eq!(normalise_base("   "), None);
        assert_eq!(normalise_base("/"), None);
        assert_eq!(normalise_base("http://x"), Some("http://x".into()));
    }

    #[test]
    fn every_manifest_source_has_a_signature_url_beside_it() {
        for (m, s) in manifest_sources_from(None) {
            assert!(m.ends_with(MANIFEST_NAME), "{m}");
            assert!(s.ends_with(MANIFEST_SIG_NAME), "{s}");
            assert_eq!(m.replace(MANIFEST_NAME, MANIFEST_SIG_NAME), s);
        }
    }

    #[test]
    fn gitee_is_tried_before_github() {
        let src = manifest_sources_from(None);
        let gitee = src
            .iter()
            .position(|(m, _)| m.contains("gitee.com"))
            .expect("gitee must be a candidate");
        let gh_direct = src
            .iter()
            .position(|(m, _)| m.starts_with("https://raw.githubusercontent.com"))
            .expect("direct github must be a candidate");
        assert!(gitee < gh_direct, "gitee should be tried first");
        assert_eq!(gitee, 0, "gitee is the primary source");
    }

    #[test]
    fn the_manifest_is_never_fetched_through_the_github_api() {
        // The API's 60 requests/hour is per IP and shared behind carrier NAT,
        // which is the whole reason the manifest is a static file in git.
        for (m, s) in manifest_sources_from(None) {
            assert!(!m.contains("api.github.com"), "{m}");
            assert!(!s.contains("api.github.com"), "{s}");
        }
    }

    #[test]
    fn installer_urls_carry_the_version_and_file_name() {
        let urls =
            installer_sources_from(None, "0.5.0", "ZipNest_0.5.0_x64-setup.exe", None);
        assert_eq!(urls.len(), 3, "github leg is skipped without an asset id");
        for u in &urls {
            assert!(u.contains("v0.5.0"), "{u}");
            assert!(u.ends_with("ZipNest_0.5.0_x64-setup.exe"), "{u}");
        }
        assert!(urls[0].contains("gitee.com"));
    }

    #[test]
    fn the_github_rest_leg_is_last_and_asks_for_bytes() {
        let urls = installer_sources_from(
            None,
            "0.5.0",
            "ZipNest_0.5.0_x64-setup.exe",
            Some(751155713),
        );
        let last = urls.last().unwrap();
        assert!(last.contains("/releases/assets/751155713"));
        assert!(wants_octet_stream(last));
        // ...and nothing else does.
        for u in &urls[..urls.len() - 1] {
            assert!(!wants_octet_stream(u), "{u}");
        }
    }

    #[test]
    fn the_accelerator_prefixes_keep_the_upstream_path() {
        for u in installer_sources_from(None, "0.5.0", "a.exe", None) {
            if let Some(rest) = u.strip_prefix("https://gh-proxy.org/") {
                assert!(rest.starts_with("https://github.com/"), "{u}");
            }
            if let Some(rest) = u.strip_prefix("https://ghfast.top/") {
                assert!(rest.starts_with("https://github.com/"), "{u}");
            }
        }
    }

    #[test]
    fn the_source_index_shown_to_a_user_is_one_based() {
        assert_eq!(describe_source(0, 4), "1/4");
        assert_eq!(describe_source(3, 4), "4/4");
    }
}
