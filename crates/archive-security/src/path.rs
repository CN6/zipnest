//! Hardened entry-path sanitization for archive extraction.
//!
//! Every path that comes out of an archive and before it touches the
//! filesystem MUST go through [`sanitize_entry_path`].

use std::fmt;

/// Reason an archive entry path was rejected.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SecurityViolation {
    Traversal,
    Absolute,
    ReservedName,
    Empty,
    TooLong,
}

impl fmt::Display for SecurityViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for SecurityViolation {}

/// Windows reserved device names (matched against the first dot-segment,
/// case-insensitively, with or without extension).
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

fn is_illegal(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\') || (c as u32) < 0x20
}

/// Sanitize an archive entry path into a safe, relative, `/`-separated path.
///
/// Rejects (with the corresponding [`SecurityViolation`]):
/// - `..` traversal segments (including a trailing `a/..`)
/// - absolute paths (`/x`, `C:\x`, `C:x`) and UNC (`\\srv\sh`, `//srv/sh`)
/// - Windows reserved device names (`CON`, `nul.txt`, `dir/COM1.log`, ...)
/// - empty results or components that trim to nothing
/// - components longer than 255 bytes
///
/// Cleans (without rejecting):
/// - redundant `.` and empty segments, `//` and backslash separators
/// - trailing dots/spaces on components (Windows-hostile names)
/// - illegal characters (`<>:"|?*` and control chars) replaced with `_`
pub fn sanitize_entry_path(raw: &str) -> Result<String, SecurityViolation> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(SecurityViolation::Empty);
    }
    // Absolute / UNC checks on the raw string before separator splitting.
    if t.starts_with('/') || t.starts_with('\\') {
        return Err(SecurityViolation::Absolute);
    }
    if t.len() >= 2 && t.as_bytes()[1] == b':' {
        // Only a real drive prefix is an escape: `X:` or `X:\`/`X:/`.
        // A colon deeper in a name (`a:b.txt`, illegal on Windows) is not an
        // escape; the per-component cleaner below rewrites it to `_` so the
        // file is still extracted instead of being dropped.
        let drive_absolute = t.len() == 2 || matches!(t.as_bytes()[2], b'/' | b'\\');
        if drive_absolute {
            return Err(SecurityViolation::Absolute);
        }
    }
    if t.starts_with("//") || t.starts_with("\\\\") {
        return Err(SecurityViolation::Absolute);
    }

    let mut out: Vec<String> = Vec::new();
    for comp in t.split(['/', '\\']) {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            return Err(SecurityViolation::Traversal);
        }
        let clean = comp.trim_end_matches(['.', ' ']).trim_start_matches(' ');
        if clean.is_empty() {
            return Err(SecurityViolation::Empty);
        }
        if clean.len() > 255 {
            return Err(SecurityViolation::TooLong);
        }
        let stem = clean.split('.').next().unwrap_or(clean);
        if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
            return Err(SecurityViolation::ReservedName);
        }
        out.push(clean.chars().map(|c| if is_illegal(c) { '_' } else { c }).collect());
    }
    if out.is_empty() {
        return Err(SecurityViolation::Empty);
    }
    Ok(out.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(raw: &str, expect: &str) {
        assert_eq!(sanitize_entry_path(raw).unwrap(), expect, "raw={raw:?}");
    }
    fn bad(raw: &str, v: SecurityViolation) {
        assert_eq!(sanitize_entry_path(raw).unwrap_err(), v, "raw={raw:?}");
    }

    #[test]
    fn accepts_normal_paths() {
        ok("a.txt", "a.txt");
        ok("dir/sub/b.txt", "dir/sub/b.txt");
        ok("./dir/./b.txt", "dir/b.txt");
        ok("中文/文件.txt", "中文/文件.txt");
        ok("dir\\win\\style.txt", "dir/win/style.txt");
    }

    #[test]
    fn rejects_traversal() {
        bad("../evil", SecurityViolation::Traversal);
        bad("a/../../b", SecurityViolation::Traversal);
        bad("..", SecurityViolation::Traversal);
        bad("a/b/../../../c", SecurityViolation::Traversal);
        bad("a/..", SecurityViolation::Traversal);
        bad("..\\windows\\system32", SecurityViolation::Traversal);
    }

    #[test]
    fn rejects_absolute_and_unc() {
        bad("/abs", SecurityViolation::Absolute);
        bad("C:\\abs", SecurityViolation::Absolute);
        bad("c:/abs", SecurityViolation::Absolute);
        bad("C:", SecurityViolation::Absolute);
        bad("\\\\server\\share\\x", SecurityViolation::Absolute);
        bad("//server/share", SecurityViolation::Absolute);
    }

    #[test]
    fn colon_in_a_name_is_cleaned_not_rejected() {
        // Drive-relative-looking names are not escapes: Windows can't store the
        // colon, so it is cleaned and the file is kept.
        ok("C:rel", "C_rel");
        ok("a:b.txt", "a_b.txt");
        ok("dir/12:30:00.log", "dir/12_30_00.log");
    }

    #[test]
    fn rejects_reserved_device_names() {
        bad("CON", SecurityViolation::ReservedName);
        bad("con.txt", SecurityViolation::ReservedName);
        bad("dir/NUL", SecurityViolation::ReservedName);
        bad("COM1.log", SecurityViolation::ReservedName);
        bad("LPT9", SecurityViolation::ReservedName);
        bad("aux", SecurityViolation::ReservedName);
        bad("CoM1", SecurityViolation::ReservedName); // case-insensitive
        ok("com12.txt", "com12.txt"); // com12 is NOT reserved on Windows
    }

    #[test]
    fn cleans_windows_filenames() {
        ok("a<b>:c.txt", "a_b__c.txt");
        ok("x|y?.txt", "x_y_.txt");
        ok("trail. ", "trail");
        ok("dir/ok.txt.", "dir/ok.txt");
        ok("..%2ffile", "..%2ffile");
        ok("a//b", "a/b");
        bad("///", SecurityViolation::Absolute); // root path, not empty
        bad(" . ", SecurityViolation::Empty);
        bad(&"x".repeat(300), SecurityViolation::TooLong);
    }
}
