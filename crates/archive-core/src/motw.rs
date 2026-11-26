//! The "mark of the web" (`Zone.Identifier`), copied from an archive to the
//! files extracted from it (A5).
//!
//! Windows records "this file came from the internet" as an alternate data
//! stream on NTFS. A browser writes it on the downloaded *archive*; the files
//! inside are written by whatever unpacks them, so they only carry the mark if
//! the unarchiver copies it. Without it SmartScreen and Office's Protected View
//! treat those files as local and stop warning — a silent security downgrade
//! that 7-Zip and WinRAR both fixed.
//!
//! Rules kept here:
//!
//! * read the archive's own stream once, up front, and hand the *bytes* down,
//!   so every extracted file gets the same mark;
//! * only regular files are marked. A directory cannot usefully carry one (a
//!   child file created later would not inherit it), and a symlink's stream
//!   belongs to the link, not the target;
//! * a filesystem without ADS support (FAT32, exFAT, a network share) is not an
//!   error: the file is extracted, the write fails, and the caller counts it so
//!   the result window can say so.

use std::io::Write;
use std::path::Path;

/// ADS stream name Windows uses for the zone.
const ZONE_STREAM: &str = "Zone.Identifier";

/// The archive's own `Zone.Identifier` payload, if it has one.
///
/// `None` covers both "the archive is not marked" and "it cannot be asked"
/// (a FAT32 stick, a share that refuses the stream) — in either case there is
/// nothing to propagate.
pub fn read_from(path: &Path) -> Option<Vec<u8>> {
    let stream = stream_path(path)?;
    std::fs::read(stream).ok().filter(|b| !b.is_empty())
}

/// Write `mark` onto `file`'s `Zone.Identifier` stream.
///
/// Returns `false` when the filesystem refused it — not an error, but something
/// the caller counts and reports.
pub fn write_to(file: &Path, mark: &[u8]) -> bool {
    // Windows will happily *create* an empty file when asked to open a stream
    // on a path that does not exist (`CreateFileW` on `x:Zone.Identifier`
    // materialises `x`). Extraction only ever marks a file it has just written,
    // so a missing target is a programming error, not something to act on — and
    // turning it into a stray empty file would be worse than reporting it.
    if !file.is_file() {
        return false;
    }
    let Some(stream) = stream_path(file) else {
        return false;
    };
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&stream)
    {
        Ok(mut f) => f.write_all(mark).is_ok(),
        Err(_) => false,
    }
}

/// `<path>:Zone.Identifier` as an `OsString`, so a path that is not valid
/// UTF-8 still gets a stream name rather than being skipped.
///
/// The verbatim `\\?\` prefix is deliberately **not** added here: Windows'
/// stream syntax does not combine with it on every build, and a destination
/// long enough to need the prefix usually sits on a volume where the ADS does
/// not matter anyway. The fallback is "unmarked", which is reported.
fn stream_path(path: &Path) -> Option<std::ffi::OsString> {
    let mut s = path.as_os_str().to_os_string();
    s.push(":");
    s.push(ZONE_STREAM);
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("zn-motw-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const MARK: &[u8] = b"[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/a.zip\r\n";

    /// Round trip on NTFS (the temp dir): what goes in comes out, and an
    /// unmarked file answers `None` rather than an empty buffer.
    #[test]
    fn a_mark_round_trips_through_the_ads() {
        let d = dir("roundtrip");
        let f = d.join("a.txt");
        std::fs::write(&f, b"content").unwrap();
        assert_eq!(read_from(&f), None, "an unmarked file has no mark");
        assert!(write_to(&f, MARK), "NTFS must accept the stream");
        assert_eq!(read_from(&f).as_deref(), Some(MARK));
        // The plain file's own bytes are untouched.
        assert_eq!(std::fs::read(&f).unwrap(), b"content");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The stream name is `<path>:Zone.Identifier`, built from the `OsStr` so a
    /// non-UTF-8 path still gets one.
    #[test]
    fn the_stream_name_is_appended_to_the_whole_path() {
        let p = Path::new(r"C:\out\file.txt");
        assert_eq!(
            stream_path(p).unwrap(),
            std::ffi::OsString::from(r"C:\out\file.txt:Zone.Identifier")
        );
    }

    /// A missing file cannot be marked, and marking it must not *create* it:
    /// Windows' own behaviour for a stream on a missing path is to materialise
    /// an empty file, which is why `write_to` checks first.
    #[test]
    fn marking_a_missing_file_reports_failure() {
        let d = dir("missing");
        assert!(!write_to(&d.join("nope.txt"), MARK));
        assert_eq!(read_from(&d.join("nope.txt")), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}