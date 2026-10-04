use archive_core::{Archive, ArchiveOpenOptions};
use std::path::PathBuf;

fn fx(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

#[test]
fn opens_plain_zip_and_counts_items() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).expect("open");
    // fixture: a.txt, c.txt, sub/ (dir), sub/b.txt
    assert_eq!(arc.len(), 4);
}

#[test]
fn rejects_corrupt_archive() {
    let err = Archive::open(&fx("corrupt.zip"), ArchiveOpenOptions::default())
        .expect_err("corrupt must fail");
    assert_eq!(err.error_key(), "error.not_an_archive");
}

#[test]
fn lists_entries_with_metadata() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).unwrap();
    let es = arc.entries().unwrap();
    assert_eq!(es.len(), 4);

    let a = es.iter().find(|e| e.path == "a.txt").expect("a.txt entry");
    assert!(!a.is_dir);
    assert!(a.size > 0, "a.txt must have a size");
    assert!(a.crc.is_some(), "a.txt must carry a CRC");
    assert!(a.mtime.is_some(), "a.txt must carry a modification time");
    assert!(!a.encrypted);

    let sub = es
        .iter()
        .find(|e| e.path == "sub" || e.path == "sub/")
        .expect("sub dir entry");
    assert!(sub.is_dir);

    let b = es.iter().find(|e| e.path.ends_with("b.txt")).expect("b.txt");
    assert_eq!(b.size, "nested content".len() as u64); // fixture is BOM-free, no trailing newline
}

#[test]
fn enc_zip_flags_encrypted() {
    let arc = Archive::open(&fx("enc.zip"), ArchiveOpenOptions::default()).expect("zip opens");
    let es = arc.entries().unwrap();
    let a = es.iter().find(|e| e.path == "a.txt").expect("a.txt");
    assert!(a.encrypted, "entry in password-protected zip must be flagged");
}
#[test]
fn reads_entry_bytes_matching_source() {
    let arc = Archive::open(&fx("nested.zip"), ArchiveOpenOptions::default()).unwrap();
    let b = arc
        .entries()
        .unwrap()
        .iter()
        .find(|e| e.path.ends_with("b.txt"))
        .unwrap()
        .index;
    let got = arc
        .read_entry(b, &ArchiveOpenOptions::default(), None)
        .unwrap();
    assert_eq!(got, b"nested content");
}

#[test]
fn read_entry_respects_max_bytes() {
    let arc = Archive::open(&fx("nested.zip"), ArchiveOpenOptions::default()).unwrap();
    let c = arc.entries().unwrap().iter().find(|e| e.path == "c.txt").unwrap().index;
    let got = arc
        .read_entry(c, &ArchiveOpenOptions::default(), Some(10))
        .unwrap();
    assert_eq!(got.len(), 10);
}
// ---- Task 8: secure extraction / progress / cancel ----

use archive_core::{ExtractOptions, OnConflict};

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zn-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn extracts_plain_zip_to_disk() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).unwrap();
    let dest = tmpdir("ex");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    let mut ticks = 0;
    let stats = arc
        .extract(&opts, None, &mut |_p| {
            ticks += 1;
            true
        })
        .expect("extract");
    assert!(stats.files >= 3, "3 files expected, got {}", stats.files);
    assert!(ticks >= 1, "progress must tick");
    assert!(std::fs::read_to_string(dest.join("a.txt"))
        .unwrap()
        .contains("hello zipnest"));
    assert_eq!(
        std::fs::read_to_string(dest.join("sub/b.txt")).unwrap().trim(),
        "nested content"
    );
    // c.txt is the only Deflate-compressed entry in the fixture (8406 bytes);
    // Store entries passed while Deflate silently produced 0 bytes.
    let c_len = std::fs::metadata(dest.join("c.txt"))
        .expect("c.txt must exist")
        .len();
    assert_eq!(c_len, 8406, "deflated entry must be fully written");
    let _ = std::fs::remove_dir_all(&dest);
}

/// Any `<name>.zipnest-part-<pid>` temp files under `dir`, recursively.
fn temp_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(".zipnest-part-"))
            {
                found.push(p);
            }
        }
    }
    found
}

/// Regression: the destination used to be truncated with `File::create` before
/// the entry's bytes were known to be complete. A failure mid-entry must leave
/// the file that was already there byte-for-byte intact and leave no temp file.
#[test]
fn failed_extract_keeps_the_existing_file_intact() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).unwrap();
    let dest = tmpdir("keepold");
    std::fs::create_dir_all(&dest).unwrap();
    let existing = dest.join("a.txt");
    std::fs::write(&existing, "ORIGINAL CONTENT").unwrap();
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: 4, // trips the zip-bomb guard on the first chunk
        on_conflict: OnConflict::Overwrite,
    };
    let err = arc.extract(&opts, None, &mut |_p| true).expect_err("quota must trip");
    assert_eq!(err.error_key(), "error.quota_exceeded");
    assert_eq!(
        std::fs::read_to_string(&existing).unwrap(),
        "ORIGINAL CONTENT",
        "a failed extraction must not truncate the pre-existing file"
    );
    assert!(
        temp_files(&dest).is_empty(),
        "temp files left behind: {:?}",
        temp_files(&dest)
    );
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn cancel_leaves_no_temp_files_behind() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).unwrap();
    let dest = tmpdir("canceltmp");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    // Cancel after the second tick so at least one entry has started.
    let mut ticks = 0;
    let err = arc
        .extract(&opts, None, &mut |_p| {
            ticks += 1;
            ticks < 2
        })
        .expect_err("must cancel");
    assert_eq!(err.error_key(), "error.cancelled");
    assert!(
        temp_files(&dest).is_empty(),
        "temp files left behind: {:?}",
        temp_files(&dest)
    );
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn skip_policy_keeps_the_existing_file_and_reports_it() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).unwrap();
    let dest = tmpdir("skipold");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("a.txt"), "ORIGINAL CONTENT").unwrap();
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Skip,
    };
    let stats = arc.extract(&opts, None, &mut |_p| true).expect("extract");
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "ORIGINAL CONTENT"
    );
    assert!(
        stats.skipped.iter().any(|p| p == "a.txt"),
        "a skipped entry must be reported, got {:?}",
        stats.skipped
    );
    // Entries without a conflict still land.
    assert!(dest.join("c.txt").exists(), "unrelated files must still extract");
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn rename_policy_writes_beside_the_existing_file() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).unwrap();
    let dest = tmpdir("renameold");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("a.txt"), "ORIGINAL CONTENT").unwrap();
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Rename,
    };
    arc.extract(&opts, None, &mut |_p| true).expect("extract");
    assert_eq!(
        std::fs::read_to_string(dest.join("a.txt")).unwrap(),
        "ORIGINAL CONTENT"
    );
    let renamed = dest.join("a (2).txt");
    assert!(renamed.exists(), "expected {}", renamed.display());
    assert!(std::fs::read_to_string(&renamed).unwrap().contains("hello zipnest"));
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn extracted_files_keep_their_archived_mtime() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).unwrap();
    let archived = arc
        .entries()
        .unwrap()
        .into_iter()
        .find(|e| e.path == "a.txt")
        .and_then(|e| e.mtime);
    let dest = tmpdir("mtime");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    arc.extract(&opts, None, &mut |_p| true).expect("extract");
    let written = std::fs::metadata(dest.join("a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let Some(expected) = archived else {
        panic!("fixture entry must carry an mtime");
    };
    let delta = written
        .duration_since(expected)
        .or_else(|_| expected.duration_since(written))
        .unwrap_or_default();
    assert!(
        delta < std::time::Duration::from_secs(2),
        "archived mtime not restored (off by {delta:?})"
    );
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn extract_cancel_returns_cancelled() {
    let arc = Archive::open(&fx("nested.zip"), ArchiveOpenOptions::default()).unwrap();
    let dest = tmpdir("cancel");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    let err = arc.extract(&opts, None, &mut |_p| false).expect_err("must cancel");
    assert_eq!(err.error_key(), "error.cancelled");
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn extract_quota_enforced() {
    let arc = Archive::open(&fx("nested.zip"), ArchiveOpenOptions::default()).unwrap();
    let dest = tmpdir("quota");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: 10,
        on_conflict: OnConflict::Overwrite,
    };
    let err = arc.extract(&opts, None, &mut |_p| true).expect_err("quota must trip");
    assert_eq!(err.error_key(), "error.quota_exceeded");
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn zip_slip_entry_is_skipped_and_other_files_extract() {
    use std::io::Write as _;
    // Craft an archive with a malicious relative path followed by a safe file.
    let evil = std::env::temp_dir().join(format!("zn-slip-{}.zip", std::process::id()));
    {
        let f = std::fs::File::create(&evil).unwrap();
        let mut w = zip::ZipWriter::new(f);
        w.start_file::<_, ()>("../../evil.txt", Default::default()).unwrap();
        w.write_all(b"pwned").unwrap();
        w.start_file::<_, ()>("ok.txt", Default::default()).unwrap();
        w.write_all(b"safe").unwrap();
        w.finish().unwrap();
    }
    let arc = Archive::open(&evil, ArchiveOpenOptions::default()).expect("open evil");
    let dest = tmpdir("slipdest");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    let stats = arc
        .extract(&opts, None, &mut |_p| true)
        .expect("one bad entry must not fail the whole extraction");
    // double insurance: nothing may land outside dest
    let escaped = std::env::temp_dir().join("evil.txt");
    assert!(!escaped.exists(), "must not escape temp");
    assert!(!dest.join("evil.txt").exists());
    assert!(
        stats.skipped.iter().any(|p| p.ends_with("evil.txt")),
        "the escapee must be reported as skipped: {:?}",
        stats.skipped
    );
    // the safe file listed after it still lands
    assert_eq!(std::fs::read_to_string(dest.join("ok.txt")).unwrap(), "safe");
    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::remove_file(&evil);
}

#[test]
fn reserved_name_is_skipped_and_others_still_extract() {
    use std::io::Write as _;
    let z = std::env::temp_dir().join(format!("zn-reserved-{}.zip", std::process::id()));
    {
        let f = std::fs::File::create(&z).unwrap();
        let mut w = zip::ZipWriter::new(f);
        for (name, body) in [("good1.txt", "one"), ("aux.txt", "aux"), ("good2.txt", "two")] {
            w.start_file::<_, ()>(name, Default::default()).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }
    let arc = Archive::open(&z, ArchiveOpenOptions::default()).unwrap();
    let dest = tmpdir("reserved");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    let stats = arc
        .extract(&opts, None, &mut |_p| true)
        .expect("one bad name must not fail the whole extraction");
    assert_eq!(std::fs::read_to_string(dest.join("good1.txt")).unwrap(), "one");
    assert_eq!(
        std::fs::read_to_string(dest.join("good2.txt")).unwrap(),
        "two",
        "files after the hostile name must still extract"
    );
    assert!(!dest.join("aux.txt").exists());
    assert!(stats.skipped.iter().any(|p| p.ends_with("aux.txt")));
    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::remove_file(&z);
}
// ---- Task 9: password callback loop ----

#[test]
fn encrypted_7z_open_requires_password() {
    let err = Archive::open(&fx("enc.7z"), ArchiveOpenOptions::default())
        .expect_err("need pw");
    assert_eq!(err.error_key(), "error.password_required");
}

#[test]
fn encrypted_7z_wrong_password_rejected() {
    let err = Archive::open(
        &fx("enc.7z"),
        ArchiveOpenOptions { password: Some("nope".into()) },
    )
    .expect_err("wrong pw");
    assert_eq!(err.error_key(), "error.password_incorrect");
}

#[test]
fn encrypted_7z_with_password_opens_and_extracts() {
    let arc = Archive::open(
        &fx("enc.7z"),
        ArchiveOpenOptions { password: Some("secret".into()) },
    )
    .expect("open with pw");
    let dest = tmpdir("pw");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    arc.extract(&opts, Some("secret"), &mut |_p| true)
        .expect("extract");
    assert!(dest.join("a.txt").exists());
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn encrypted_zip_wrong_password_on_extract() {
    let arc = Archive::open(&fx("enc.zip"), ArchiveOpenOptions::default()).expect("zip opens");
    let dest = tmpdir("zpw");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    let err = arc
        .extract(&opts, Some("wrong"), &mut |_p| true)
        .expect_err("wrong pw");
    assert_eq!(err.error_key(), "error.password_incorrect");
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn encrypted_zip_extract_with_password_succeeds() {
    let arc = Archive::open(&fx("enc.zip"), ArchiveOpenOptions::default()).expect("zip opens");
    let dest = tmpdir("zpwok");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    arc.extract(&opts, Some("secret"), &mut |_p| true)
        .expect("extract with correct pw");
    assert!(dest.join("a.txt").exists());
    let _ = std::fs::remove_dir_all(&dest);
}
// ---- Task 10: centralized HRESULT mapping ----

#[test]
fn hresult_mapping_table() {
    assert_eq!(
        archive_core::map_hresult(0x8000_4004u32 as i32).error_key(),
        "error.cancelled"
    );
    assert_eq!(
        archive_core::map_hresult(0x8000_4005u32 as i32).error_key(),
        "error.engine"
    );
    assert_eq!(
        archive_core::map_hresult(0x8000_4001u32 as i32).error_key(),
        "error.engine"
    );
    // kind() mirrors error_key()
    let e = archive_core::map_hresult(0x8000_4004u32 as i32);
    assert_eq!(e.kind(), e.error_key());
}
// ---- Task 11: one-off compatibility probe (slow) ----

#[test]
#[ignore = "slow: builds a 5000-entry archive first; run with -- --ignored"]
fn enumerates_5000_entries_within_budget() {
    use std::process::Command;
    let sevenzip = r"C:\Program Files\7-Zip\7z.exe";
    if !std::path::Path::new(sevenzip).exists() {
        eprintln!("7z.exe not found; skipping compatibility probe");
        return;
    }
    let pid = std::process::id();
    let work = std::env::temp_dir().join(format!("zn-bigwork-{pid}"));
    let zip = std::env::temp_dir().join(format!("zn-big-{pid}.zip"));
    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::remove_file(&zip);
    std::fs::create_dir_all(&work).unwrap();
    for i in 0..5000u32 {
        std::fs::write(work.join(format!("f{i:04}.txt")), format!("entry {i}")).unwrap();
    }
    let st = Command::new(sevenzip)
        .args(["a", "-tzip", "-y"])
        .arg(&zip)
        .arg(format!("{}\\", work.display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run 7z");
    assert!(st.success(), "7z archive creation failed");

    let t0 = std::time::Instant::now();
    let arc = Archive::open(&zip, ArchiveOpenOptions::default()).expect("open big zip");
    let es = arc.entries().expect("enumerate big zip");
    let elapsed = t0.elapsed();
    assert!(es.len() >= 5000, "expected >=5000 items, got {}", es.len());
    assert!(elapsed < std::time::Duration::from_secs(5), "took {elapsed:?}");

    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::remove_file(&zip);
}
/// Timestamps must be a real wall-clock date, not merely internally consistent.
///
/// The FILETIME epoch delta once lost a digit, which pushed every timestamp
/// ~332 years into the future. A round-trip assertion cannot see that (both
/// ends share the constant), so the absolute value is pinned here. The
/// 2020..2035 window is timezone-independent — the fixtures were built in 2026.
#[test]
fn mtime_is_a_sane_date() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).unwrap();
    let e = arc
        .entries()
        .unwrap()
        .into_iter()
        .find(|e| e.path == "a.txt")
        .unwrap();
    let t = e.mtime.expect("fixture must carry an mtime");
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .expect("post-1970")
        .as_secs();
    assert!(
        (1_577_836_800..1_893_456_000).contains(&secs),
        "mtime {secs} is not a plausible 2020s date"
    );
}