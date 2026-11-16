//! Version comparison.
//!
//! Moved here from `apps/zipnest-native/src/main.rs`, where it had no test at
//! all — the single most important predicate in the update flow ("is this
//! release newer than me?") was unverified. The tests below are the reason this
//! is a function and not a line of inline code.
//!
//! `CURRENT_VERSION` comes from `env!("CARGO_PKG_VERSION")`, i.e. the workspace
//! version, which `packaging/check-versions.ps1` holds in agreement with the
//! installer, the MSIX manifest, the README and the CHANGELOG.

/// Split a tag or version into numeric segments, ignoring a leading `v`.
///
/// Segments that are not numbers are dropped, and so is any pre-release suffix
/// (`0.5.0-beta.1` → `[0, 5, 0]`). That makes a pre-release compare *equal* to
/// its release, which is deliberate: the updater must never offer a user
/// `0.5.0-beta.1` as "newer than 0.5.0", and a beta build's own version string
/// still parses to something usable.
pub fn parse_version(tag: &str) -> Vec<u32> {
    let core = tag.trim().trim_start_matches(['v', 'V']);
    // Drop a pre-release / build suffix wholesale before splitting: splitting on
    // '-' first would keep the `1` in `0.5.0-beta.1` and make a beta look newer
    // than the release it precedes.
    let core = core
        .split(['-', '+'])
        .next()
        .unwrap_or(core);
    core.split('.')
        .filter_map(|part| part.parse::<u32>().ok())
        .collect()
}

/// Is `tag` newer than `current`?
///
/// Compares segment by segment, padding the shorter one with zeros, so
/// `0.4.9 < 0.4.10` (the Vec-lexicographic ordering the old code relied on got
/// that right by luck, but `0.4` vs `0.4.0` wrong).
pub fn is_newer(current: &str, tag: &str) -> bool {
    let a = parse_version(current);
    let b = parse_version(tag);
    if b.is_empty() {
        return false;
    }
    let len = a.len().max(b.len());
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if y != x {
            return y > x;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_patch_is_newer() {
        assert!(is_newer("0.4.39", "0.4.40"));
        assert!(!is_newer("0.4.40", "0.4.39"));
        assert!(!is_newer("0.4.39", "0.4.39"));
    }

    #[test]
    fn double_digits_do_not_break_the_ordering() {
        // The classic trap: string or Vec<u32>-lexicographic comparison gets
        // "0.4.9" vs "0.4.10" wrong when it compares text.
        assert!(is_newer("0.4.9", "0.4.10"));
        assert!(!is_newer("0.4.10", "0.4.9"));
        assert!(is_newer("0.9.9", "0.10.0"));
    }

    #[test]
    fn a_leading_v_is_ignored() {
        assert!(is_newer("0.4.39", "v0.4.40"));
        assert!(is_newer("v0.4.39", "0.4.40"));
        assert!(!is_newer("0.4.40", "v0.4.40"));
    }

    #[test]
    fn missing_segments_count_as_zero() {
        assert!(!is_newer("0.4", "0.4.0"));
        assert!(!is_newer("0.4.0", "0.4"));
        assert!(is_newer("0.4", "0.4.1"));
        assert!(is_newer("0.4.39", "0.5"));
    }

    #[test]
    fn a_major_bump_is_newer() {
        assert!(is_newer("0.4.39", "1.0.0"));
        assert!(!is_newer("1.0.0", "0.9.9"));
    }

    #[test]
    fn a_prerelease_does_not_beat_its_release() {
        // Never push a user from a release onto a pre-release.
        assert!(!is_newer("0.5.0", "0.5.0-beta.1"));
        assert!(!is_newer("0.4.39", "0.4.39-rc1"));
    }

    #[test]
    fn garbage_never_counts_as_newer() {
        assert!(!is_newer("0.4.39", ""));
        assert!(!is_newer("0.4.39", "latest"));
        assert!(!is_newer("0.4.39", "v"));
        assert!(!is_newer("0.4.39", ".."));
    }

    #[test]
    fn the_current_version_parses() {
        // env!("CARGO_PKG_VERSION") is the single source of truth; a
        // formatting change in Cargo.toml would show up here first.
        let current = env!("CARGO_PKG_VERSION");
        assert_eq!(parse_version(current).len(), 3, "{current}");
    }

    #[test]
    fn segments_are_parsed_after_each_separator() {
        assert_eq!(parse_version("v0.4.39"), vec![0, 4, 39]);
        assert_eq!(parse_version(" 1.2.3 "), vec![1, 2, 3]);
        assert_eq!(parse_version("0.5.0-beta.1"), vec![0, 5, 0]);
        assert_eq!(parse_version("0.5.0+build.7"), vec![0, 5, 0]);
        assert!(parse_version("latest").is_empty());
    }
}
