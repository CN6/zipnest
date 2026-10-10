//! The text behind 设置 → 帮助与反馈 → 「复制诊断信息」.
//!
//! A bug report that says "解压不行" costs a round trip to ask for the version
//! and the Windows build. This produces the facts a maintainer needs, and it is
//! deliberately boring: version, OS, architecture, where the exe lives, whether
//! ZipNest currently owns the archive handlers, the engine state and the last
//! error *key*. Nothing the user was working on.
//!
//! Hard rules (asserted in the tests):
//! - never an archive name, a file list, or an extracted path;
//! - the account name is stripped out of every path (`%USERPROFILE%`);
//! - the only way this text leaves the machine is the user copying it, or a
//!   `mailto:` link they click. There is no network call anywhere here.

use crate::settings::Settings;

/// Everything the report may contain. Filled in by the caller so the rendering
/// itself stays pure and testable.
pub struct Diagnostics {
    pub version: String,
    /// e.g. `Windows 10.0.26100 (x86_64)`.
    pub os: String,
    /// Real path of the running exe; masked on the way out.
    pub exe: String,
    /// `%USERPROFILE%`, used only to mask `exe`.
    pub profile_dir: Option<String>,
    /// Whether `7z.dll` loads right now.
    pub engine_ok: bool,
    pub settings: Settings,
    /// i18n key of the most recent failure, if any.
    pub last_error: Option<String>,
}

/// Replace a leading user-profile directory with `%USERPROFILE%`.
///
/// The account name is the only part of a path that identifies a person, and
/// `C:\Program Files\ZipNest\zipnest.exe` (the usual install) does not contain
/// it at all. Case-insensitive, so `c:\users\me` and `C:\Users\Me` both match.
pub fn mask_profile(path: &str, profile: Option<&str>) -> String {
    let Some(profile) = profile else {
        return path.to_string();
    };
    let profile = profile.trim_end_matches(['\\', '/']);
    if profile.is_empty() {
        return path.to_string();
    }
    // Compare case-insensitively (Unicode-aware, as before), but cut with `get`
    // so a byte offset that lands inside a character — a lower-case mapping can
    // change a string's byte length, e.g. 'İ' -> "i̇" — yields `None` instead of
    // panicking on the UI thread while the user is copying diagnostics.
    let Some(head) = path.get(..profile.len()) else {
        return path.to_string();
    };
    if head.to_lowercase() != profile.to_lowercase() {
        return path.to_string();
    }
    // Only a real directory boundary counts: `C:\Users\me2` must not be
    // truncated as if it were `C:\Users\me`.
    let Some(rest) = path.get(profile.len()..) else {
        return path.to_string();
    };
    if !rest.is_empty() && !rest.starts_with(['\\', '/']) {
        return path.to_string();
    }
    format!("%USERPROFILE%{rest}")
}

/// Render the report. One `key: value` per line, stable keys, ASCII labels so a
/// pasted report stays greppable whatever the UI language is.
pub fn render(d: &Diagnostics) -> String {
    let s = &d.settings;
    let mut out = String::new();
    out.push_str("ZipNest diagnostics\n");
    out.push_str(&format!("version: {}\n", d.version));
    out.push_str(&format!("os: {}\n", d.os));
    out.push_str(&format!(
        "exe: {}\n",
        mask_profile(&d.exe, d.profile_dir.as_deref())
    ));
    out.push_str(&format!(
        "integration: associate={} context_menu={} applied={}\n",
        s.associate, s.context_menu, s.integration_applied
    ));
    out.push_str(&format!(
        "settings: lang={} theme={} zoom={} overwrite={} preview_max={} extract_max={}\n",
        s.language,
        s.theme_mode,
        s.ui_zoom,
        s.overwrite_policy,
        s.preview_max_bytes,
        s.max_extract_bytes
    ));
    out.push_str(&format!(
        "engine (7z.dll): {}\n",
        if d.engine_ok { "ok" } else { "NOT AVAILABLE" }
    ));
    out.push_str(&format!(
        "last error: {}\n",
        d.last_error.as_deref().unwrap_or("(none)")
    ));
    out.push_str("note: no file names, archive contents or account name are included.\n");
    out
}

/// `mailto:` URL with the report as the body. Percent-encoded by hand: the body
/// has spaces and newlines, and a raw one of either truncates the URL at the
/// mail client.
pub fn mailto_url(address: &str, subject: &str, body: &str) -> String {
    format!(
        "mailto:{}?subject={}&body={}",
        percent_encode(address),
        percent_encode(subject),
        percent_encode(body)
    )
}

/// RFC 3986: keep the unreserved set, everything else becomes %XX. Used for a
/// mailto query, where newlines must become `%0D%0A` and `&`/`?`/`#` must not
/// be allowed to break the URL.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' => {
                out.push(b as char)
            }
            b'\n' => out.push_str("%0D%0A"),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            language: "zh-CN".into(),
            theme_mode: "system".into(),
            ui_zoom: 125,
            overwrite_policy: "rename".into(),
            associate: true,
            context_menu: true,
            integration_applied: true,
            ..Default::default()
        }
    }

    fn report() -> String {
        render(&Diagnostics {
            version: "0.4.9".into(),
            os: "Windows 10.0.26100 (x86_64)".into(),
            exe: r"C:\Users\yaoyao\AppData\Local\ZipNest\zipnest.exe".into(),
            profile_dir: Some(r"C:\Users\yaoyao".into()),
            engine_ok: true,
            settings: settings(),
            last_error: None,
        })
    }

    #[test]
    fn the_report_carries_what_a_maintainer_asks_for_first() {
        let r = report();
        assert!(r.contains("version: 0.4.9"));
        assert!(r.contains("os: Windows 10.0.26100 (x86_64)"));
        assert!(r.contains("engine (7z.dll): ok"));
        // The state that decides "is ZipNest the default?", which is the whole
        // point of the v0.4.8/0.4.9 work.
        assert!(r.contains("associate=true context_menu=true applied=true"));
        assert!(r.contains("last error: (none)"));
        // Weird settings values must be visible, not silent.
        assert!(r.contains("zoom=125"));
    }

    #[test]
    fn the_account_name_never_reaches_the_report() {
        let r = report();
        assert!(r.contains(r"%USERPROFILE%\AppData\Local\ZipNest\zipnest.exe"));
        assert!(
            !r.to_lowercase().contains("yaoyao"),
            "the Windows account name must be masked out"
        );
    }

    #[test]
    fn a_normal_install_path_is_left_alone() {
        // Nothing identifying in it, so the mask is a no-op and the report keeps
        // the path we actually need for diagnosing "started from the wrong copy".
        let r = render(&Diagnostics {
            version: "0.4.9".into(),
            os: "Windows 10.0.26100 (x86_64)".into(),
            exe: r"C:\Program Files\ZipNest\zipnest.exe".into(),
            profile_dir: Some(r"C:\Users\yaoyao".into()),
            engine_ok: false,
            settings: settings(),
            last_error: Some("error.dll_missing".into()),
        });
        assert!(r.contains(r"exe: C:\Program Files\ZipNest\zipnest.exe"));
        assert!(r.contains("engine (7z.dll): NOT AVAILABLE"));
        assert!(r.contains("last error: error.dll_missing"));
    }

    #[test]
    fn masking_only_truncates_on_a_real_directory_boundary() {
        assert_eq!(
            mask_profile(r"C:\Users\me\a\b.txt", Some(r"C:\Users\me")),
            r"%USERPROFILE%\a\b.txt"
        );
        // Trailing separator on either side, and case differences, still match.
        assert_eq!(
            mask_profile(r"c:\users\ME\a", Some(r"C:\Users\me\")),
            r"%USERPROFILE%\a"
        );
        // A sibling account whose name merely starts the same must stay intact.
        assert_eq!(
            mask_profile(r"C:\Users\me2\a", Some(r"C:\Users\me")),
            r"C:\Users\me2\a"
        );
        // No profile known: better an unmasked path than a wrong guess.
        assert_eq!(mask_profile(r"C:\Users\me\a", None), r"C:\Users\me\a");
        assert_eq!(mask_profile(r"C:\Users\me\a", Some("")), r"C:\Users\me\a");
    }

    #[test]
    fn the_mail_link_is_usable_in_a_mail_client() {
        let body = report();
        let url = mailto_url("feedback@example.com", "[ZipNest v0.4.9] 问题反馈", &body);
        assert!(url.starts_with("mailto:feedback@example.com?subject="));
        assert!(url.contains("&body="));
        // Anything that would end the URL early has to be encoded.
        assert!(!url.contains(' '), "raw spaces truncate the mailto query");
        assert!(!url.contains('\n'), "raw newlines truncate the mailto query");
        assert!(!body.is_empty());
        let encoded_body = url.split("&body=").nth(1).unwrap();
        assert!(encoded_body.contains("%0D%0A"), "line breaks must survive as CRLF");
        // `:` is encoded, `.` is unreserved so it stays readable — which is what
        // keeps a pasted link recognizable.
        assert!(encoded_body.contains("version%3A%200.4.9"));
    }
}
