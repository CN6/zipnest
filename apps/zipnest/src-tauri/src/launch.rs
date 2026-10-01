//! Command-line handling for "open this archive" launches (double-click an
//! associated file, `zipnest file.7z`, single-instance argv forwarding).

const ARCHIVE_EXTENSIONS: [&str; 9] =
    ["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "iso"];

fn ends_with_archive_ext(lower: &str) -> bool {
    ARCHIVE_EXTENSIONS
        .iter()
        .any(|e| lower.ends_with(&format!(".{e}")))
}

/// True when a path names an archive, including multi-volume tails (`.7z.001`).
pub fn is_archive_path(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if ends_with_archive_ext(&lower) {
        return true;
    }
    match lower.strip_suffix(".001") {
        Some(rest) => ends_with_archive_ext(rest),
        None => false,
    }
}

/// First non-flag argument that looks like an existing archive. `args[0]` (the
/// executable) is skipped; returns `None` for a plain launch.
pub fn archive_arg(args: &[String]) -> Option<String> {
    args.iter()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .find(|a| is_archive_path(a) && std::path::Path::new(a).is_file())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_plain_and_volume_archives() {
        assert!(is_archive_path(r"C:\x\a.7z"));
        assert!(is_archive_path(r"C:\x\a.tar.gz"));
        assert!(is_archive_path(r"C:\x\a.7z.001"));
        assert!(!is_archive_path(r"C:\x\a.zip.bak"));
        assert!(!is_archive_path(r"C:\x\archive"));
    }

    #[test]
    fn picks_first_existing_archive_after_flags() {
        let tmp = std::env::temp_dir().join(format!("zn-launch-{}.7z", std::process::id()));
        fs::write(&tmp, b"not really an archive").unwrap();
        let args: Vec<String> = ["ZipNest.exe", "--flag", tmp.to_str().unwrap(), "other"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(archive_arg(&args).as_deref(), Some(tmp.to_str().unwrap()));
        // Flags are skipped; the exe (arg[0]) never matches.
        let plain = vec!["ZipNest.exe".to_string(), "--x".to_string()];
        assert_eq!(archive_arg(&plain), None);
    }

    #[test]
    fn ignores_missing_and_non_archive_args() {
        let args: Vec<String> = [
            "ZipNest.exe",
            r"C:\definitely\missing.zip",
            r"C:\definitely\readme.txt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(archive_arg(&args), None);
    }
}
