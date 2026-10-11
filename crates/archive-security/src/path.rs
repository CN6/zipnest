//! Hardened entry-path handling for archive extraction.
//!
//! Two contracts live here, and they are deliberately different:
//!
//! * [`sanitize_entry_path`] **rejects** a name it cannot make safe. It is the
//!   guard for names *we* write into an archive, and for callers that want a
//!   yes/no answer.
//! * [`plan_entry_path`] / [`plan_entry_paths`] **never fail**: they map every
//!   archive name onto a safe, relative path inside the destination. Losing a
//!   file because its name is odd is not acceptable, so the odd part of the name
//!   is rewritten (or ` (2)`-suffixed on a collision) and the entry is written;
//!   the changes are reported so the UI can tell the user.

use std::collections::HashSet;
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

// ===========================================================================
// Lossless planning: rewrite the name, never drop the entry
// ===========================================================================

/// Longest component Windows accepts (bytes).
const MAX_COMPONENT: usize = 255;

/// Name given to an entry whose name carries nothing usable at all (`" . "`).
const FALLBACK_NAME: &str = "unnamed";

/// How a name had to be changed so the entry could be written.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum EntryPathFix {
    /// `..` components were dropped.
    Traversal,
    /// A leading `/`, `\`, drive (`C:`) or UNC prefix was dropped.
    Absolute,
    /// A Windows device name (`CON`, `nul.tar.gz`, `dir/COM1.log`) was renamed.
    ReservedName,
    /// The component was longer than 255 bytes and was cut.
    TooLong,
    /// Characters Windows cannot store (`<>:"|?*`, control chars) became `_`.
    IllegalChars,
    /// Trailing dots/spaces (which Windows quietly strips) were removed.
    TrailingDots,
    /// Nothing usable was left in the name; the entry got a generated name.
    Empty,
    /// Another entry already claimed this path — Windows paths are
    /// case-insensitive — so this one got a ` (n)` suffix.
    Collision,
}

impl EntryPathFix {
    /// Stable identifier, for diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            EntryPathFix::Traversal => "traversal",
            EntryPathFix::Absolute => "absolute",
            EntryPathFix::ReservedName => "reserved",
            EntryPathFix::TooLong => "too-long",
            EntryPathFix::IllegalChars => "illegal-chars",
            EntryPathFix::TrailingDots => "trailing-dots",
            EntryPathFix::Empty => "empty",
            EntryPathFix::Collision => "collision",
        }
    }
}

/// One archive entry with the path it will be written to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedEntry {
    /// The name exactly as the archive carried it.
    pub raw: String,
    /// Safe relative path inside the destination, `/`-separated.
    pub path: String,
    pub is_dir: bool,
    /// Every way the name had to be changed; empty when it was used verbatim.
    pub fixes: Vec<EntryPathFix>,
}

fn note(fixes: &mut Vec<EntryPathFix>, f: EntryPathFix) {
    if !fixes.contains(&f) {
        fixes.push(f);
    }
}

/// Cut `s` down to [`MAX_COMPONENT`] bytes on a char boundary, keeping the
/// extension readable (a 300-character `report….txt` keeps its `.txt`).
fn cut_to_max(s: &mut String) {
    if s.len() <= MAX_COMPONENT {
        return;
    }
    let (head, tail) = match s.rfind('.') {
        // Only treat a short trailing group as an extension; `a.b.c.d…` far
        // into a 300-character name is more likely part of the name itself.
        Some(dot) if dot > 0 && s.len() - dot <= 24 => {
            (s[..dot].to_string(), s[dot..].to_string())
        }
        _ => (s.clone(), String::new()),
    };
    let mut cut = (MAX_COMPONENT - tail.len()).min(head.len());
    while cut > 0 && !head.is_char_boundary(cut) {
        cut -= 1;
    }
    *s = format!("{}{}", &head[..cut], tail);
}

/// Clean one path component. `None` means the component could not name
/// anything at all and is dropped.
fn clean_component(comp: &str, fixes: &mut Vec<EntryPathFix>) -> Option<String> {
    // Windows silently strips trailing dots and spaces, so two names that look
    // different on screen would fight over one file: strip them up front.
    let trimmed = comp.trim_end_matches(['.', ' ']).trim_start_matches(' ');
    let mut s = if trimmed.len() == comp.len() {
        comp.to_string()
    } else {
        note(fixes, EntryPathFix::TrailingDots);
        trimmed.to_string()
    };
    if s.is_empty() {
        note(fixes, EntryPathFix::Empty);
        return None;
    }
    if s.chars().any(is_illegal) {
        note(fixes, EntryPathFix::IllegalChars);
        s = s.chars().map(|c| if is_illegal(c) { '_' } else { c }).collect();
    }
    // `CON.txt` is as unusable as `CON`: Windows matches the part before the
    // first dot. An appended `_` keeps the name recognisable.
    let stem_end = s.find('.').unwrap_or(s.len());
    if RESERVED
        .iter()
        .any(|r| s[..stem_end].eq_ignore_ascii_case(r))
    {
        note(fixes, EntryPathFix::ReservedName);
        s.insert(stem_end, '_');
    }
    if s.len() > MAX_COMPONENT {
        note(fixes, EntryPathFix::TooLong);
        cut_to_max(&mut s);
    }
    Some(s)
}

/// Map a raw archive name onto a safe relative path, changing as little as
/// possible and never failing.
///
/// Returns the path plus the list of changes made.
pub fn plan_entry_path(raw: &str) -> (String, Vec<EntryPathFix>) {
    let mut fixes = Vec::new();
    let t = raw.trim();
    if t.is_empty() {
        return (FALLBACK_NAME.to_string(), vec![EntryPathFix::Empty]);
    }
    // An absolute path is not an escape once the prefix is gone: `/a.txt` and
    // `C:\a.txt` both become `a.txt` inside the destination.
    if t.starts_with(['/', '\\']) {
        note(&mut fixes, EntryPathFix::Absolute);
    }
    if t.len() >= 2 && t.as_bytes()[1] == b':' && t.as_bytes()[0].is_ascii_alphabetic() {
        // Only a real drive prefix is an escape: `X:` or `X:\`/`X:/`. A colon
        // deeper in a name (`a:b.txt`, illegal on Windows) is cleaned instead.
        let drive_absolute = t.len() == 2 || matches!(t.as_bytes()[2], b'/' | b'\\');
        if drive_absolute {
            note(&mut fixes, EntryPathFix::Absolute);
        }
    }
    let mut out: Vec<String> = Vec::new();
    for comp in t.split(['/', '\\']) {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            // The `..` is what makes the name dangerous, not the file: drop the
            // component and keep the entry.
            note(&mut fixes, EntryPathFix::Traversal);
            continue;
        }
        if let Some(clean) = clean_component(comp, &mut fixes) {
            out.push(clean);
        }
    }
    if out.is_empty() {
        return (FALLBACK_NAME.to_string(), vec![EntryPathFix::Empty]);
    }
    (out.join("/"), fixes)
}

/// `name (n).ext` for a collision, keeping the extension readable.
fn rename_component(comp: &str, n: usize) -> String {
    let suffix = format!(" ({n})");
    match comp.rfind('.') {
        Some(dot) if dot > 0 => format!("{}{}{}", &comp[..dot], suffix, &comp[dot..]),
        _ => format!("{comp}{suffix}"),
    }
}

/// Index of the first component that stops `path` from being usable, or `None`
/// when the path is free.
///
/// A *file* conflicts with the path itself and with any ancestor; a *directory*
/// conflicts only with the path itself being a file — two entries sharing a
/// directory is normal. The shallowest blocking component is returned, because
/// renaming it moves everything below it out of the way.
fn blocking_component(
    path: &str,
    is_dir: bool,
    files: &HashSet<String>,
    dirs: &HashSet<String>,
) -> Option<usize> {
    let parts: Vec<&str> = path.split('/').collect();
    let n = parts.len();
    for i in 0..n {
        let prefix = parts[..=i].join("/").to_lowercase();
        // A file at any ancestor level (including the whole path) means this
        // path cannot be written.
        if files.contains(&prefix) {
            return Some(i);
        }
        if i + 1 == n && !is_dir && dirs.contains(&prefix) {
            return Some(i);
        }
    }
    None
}

/// Give every entry a path, resolving collisions deterministically.
///
/// Entries are handled in the order given (the engine's, i.e. archive order), so
/// the first name keeps the plain path and later ones move aside. Nothing is
/// ever dropped: the only way an entry ends up without a usable path is
/// [`MAX_ATTEMPTS`] collisions on one name, which no real archive produces.
pub fn plan_entry_paths(entries: &[(String, bool)]) -> Vec<PlannedEntry> {
    const MAX_ATTEMPTS: usize = 10_000;
    let mut files: HashSet<String> = HashSet::new();
    let mut dirs: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(entries.len());
    for (raw, is_dir) in entries {
        let (base, mut fixes) = plan_entry_path(raw);
        let mut path = base;
        let origin: Vec<String> = path.split('/').map(str::to_string).collect();
        let mut attempt = 1usize;
        loop {
            match blocking_component(&path, *is_dir, &files, &dirs) {
                None => break,
                Some(idx) => {
                    if attempt >= MAX_ATTEMPTS {
                        break;
                    }
                    attempt += 1;
                    // Rename the *original* component each time, so repeated
                    // attempts do not stack suffixes (`A (2) (3).TXT`).
                    let mut next = origin.clone();
                    next[idx] = rename_component(&origin[idx], attempt);
                    path = next.join("/");
                    note(&mut fixes, EntryPathFix::Collision);
                }
            }
        }
        let key = path.to_lowercase();
        if *is_dir {
            dirs.insert(key);
        } else {
            files.insert(key);
        }
        let parts: Vec<&str> = path.split('/').collect();
        for end in 1..parts.len() {
            dirs.insert(parts[..end].join("/").to_lowercase());
        }
        out.push(PlannedEntry {
            raw: raw.clone(),
            path,
            is_dir: *is_dir,
            fixes,
        });
    }
    out
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

    // ---- plan_entry_path: rewrite, never reject ----

    fn plan(raw: &str) -> (String, Vec<EntryPathFix>) {
        plan_entry_path(raw)
    }

    #[test]
    fn planning_keeps_normal_names_untouched() {
        assert_eq!(plan("a.txt"), ("a.txt".into(), vec![]));
        assert_eq!(plan("dir/sub/b.txt"), ("dir/sub/b.txt".into(), vec![]));
        assert_eq!(plan("win\\style.txt"), ("win/style.txt".into(), vec![]));
        assert_eq!(plan("中文/文件.txt"), ("中文/文件.txt".into(), vec![]));
    }

    #[test]
    fn planning_turns_traversal_into_a_plain_name() {
        let (p, f) = plan("../../evil.txt");
        assert_eq!(p, "evil.txt");
        assert_eq!(f, vec![EntryPathFix::Traversal]);
        // A `..` in the middle cannot climb out either.
        assert_eq!(plan("a/../../b").0, "a/b");
    }

    #[test]
    fn planning_strips_absolute_prefixes_instead_of_dropping_the_file() {
        assert_eq!(plan("/rooted.txt").0, "rooted.txt");
        assert_eq!(plan("\\\\srv\\share\\unc.txt").0, "srv/share/unc.txt");
        assert_eq!(plan("C:\\drive.txt").0, "C_/drive.txt");
        assert!(plan("/rooted.txt").1.contains(&EntryPathFix::Absolute));
    }

    #[test]
    fn planning_renames_device_names_but_keeps_the_entry() {
        assert_eq!(plan("CON").0, "CON_");
        assert_eq!(plan("nul.tar.gz").0, "nul_.tar.gz");
        assert_eq!(plan("dir/COM1.log").0, "dir/COM1_.log");
        assert_eq!(plan("aux.txt").1, vec![EntryPathFix::ReservedName]);
    }

    #[test]
    fn planning_replaces_illegal_characters() {
        assert_eq!(plan("a:b|c?.txt").0, "a_b_c_.txt");
        assert_eq!(plan("a:b|c?.txt").1, vec![EntryPathFix::IllegalChars]);
    }

    #[test]
    fn planning_truncates_over_long_components_on_a_char_boundary() {
        let long = format!("{}.txt", "x".repeat(300));
        let (p, f) = plan(&long);
        assert_eq!(p.len(), MAX_COMPONENT);
        assert!(p.ends_with(".txt"), "the extension must survive the cut: {p}");
        assert!(f.contains(&EntryPathFix::TooLong));
        // Multi-byte characters must not be cut in half.
        let wide = "宽".repeat(200); // 600 bytes
        let (p, _) = plan(&wide);
        assert!(p.len() <= MAX_COMPONENT);
        assert!(p.chars().all(|c| c == '宽'));
    }

    #[test]
    fn planning_never_fails() {
        // Every one of these used to drop the entry.
        let huge = "x".repeat(4000);
        for raw in [
            "../../evil.txt",
            "C:\\x.txt",
            "//srv/share/x",
            "CON",
            " . ",
            "///",
            huge.as_str(),
            "nested/../CON ",
        ] {
            let (p, _) = plan(raw);
            assert!(!p.is_empty(), "raw={raw:?} produced an empty path");
            assert!(!p.starts_with('/'), "raw={raw:?} stayed absolute");
            assert!(
                !p.split('/').any(|c| c == ".."),
                "raw={raw:?} kept a traversal segment"
            );
        }
    }

    #[test]
    fn plan_entry_paths_gives_every_entry_a_distinct_path() {
        let entries: Vec<(String, bool)> = [
            "a.txt",
            "a.txt",      // same name twice: the second moves aside
            "A.TXT",      // Windows cannot tell these apart either
            "CON",
            "CON_",       // ...so this one must move
            "dir/aux.txt",
            "nul",
        ]
        .iter()
        .map(|s| (s.to_string(), false))
        .collect();
        let planned = plan_entry_paths(&entries);
        let paths: Vec<String> = planned.iter().map(|p| p.path.clone()).collect();
        let unique: HashSet<String> = paths.iter().map(|p| p.to_lowercase()).collect();
        assert_eq!(unique.len(), paths.len(), "paths collided: {paths:?}");
        assert_eq!(paths[0], "a.txt");
        assert_eq!(paths[1], "a (2).txt");
        assert_eq!(paths[2], "A (3).TXT");
        assert_eq!(paths[3], "CON_");
        assert_eq!(paths[4], "CON_ (2)");
        assert_eq!(paths[5], "dir/aux_.txt");
        assert_eq!(paths[6], "nul_");
    }

    #[test]
    fn plan_entry_paths_moves_a_file_off_a_directory_it_blocks() {
        // `a` is a file and `a/b.txt` needs `a` to be a directory. Entries are
        // resolved in archive order, so the second one moves.
        let entries: Vec<(String, bool)> = vec![
            ("a".to_string(), false),
            ("a/b.txt".to_string(), false),
        ];
        let planned = plan_entry_paths(&entries);
        assert_eq!(planned[0].path, "a");
        assert_eq!(planned[1].path, "a (2)/b.txt");
        // The other order: the plain `a` entry arrives second and is what has
        // to move, because `a` is already a directory above a real file.
        let entries: Vec<(String, bool)> = vec![
            ("a/b.txt".to_string(), false),
            ("a".to_string(), false),
        ];
        let planned = plan_entry_paths(&entries);
        assert_eq!(planned[0].path, "a/b.txt");
        assert_eq!(planned[1].path, "a (2)");
        assert!(planned[1].fixes.contains(&EntryPathFix::Collision));
    }

    #[test]
    fn plan_entry_paths_keeps_directory_entries_idempotent() {
        let entries: Vec<(String, bool)> = vec![
            ("sub".to_string(), true),
            ("sub".to_string(), true),
            ("sub/".to_string(), true),
            ("sub/x.txt".to_string(), false),
        ];
        let planned = plan_entry_paths(&entries);
        assert_eq!(planned[0].path, "sub");
        assert_eq!(planned[1].path, "sub", "a second directory entry is the same directory");
        assert_eq!(planned[2].path, "sub");
        assert_eq!(planned[3].path, "sub/x.txt");
    }
}
