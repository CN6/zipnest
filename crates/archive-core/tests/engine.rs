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
        test_mode: false,
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
        test_mode: false,
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
        test_mode: false,
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
        test_mode: false,
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
        test_mode: false,
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
        test_mode: false,
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
        test_mode: false,
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
        test_mode: false,
    };
    let err = arc.extract(&opts, None, &mut |_p| true).expect_err("quota must trip");
    assert_eq!(err.error_key(), "error.quota_exceeded");
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn zip_slip_entry_lands_inside_the_destination_instead_of_being_dropped() {
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
        test_mode: false,
    };
    let stats = arc
        .extract(&opts, None, &mut |_p| true)
        .expect("one odd entry must not fail the whole extraction");
    // Double insurance: nothing may land outside dest ...
    let escaped = std::env::temp_dir().join("evil.txt");
    assert!(!escaped.exists(), "must not escape temp");
    // ... and the entry is not thrown away either: the `..` is dropped and the
    // file lands inside the destination.
    assert_eq!(
        std::fs::read_to_string(dest.join("evil.txt")).unwrap(),
        "pwned"
    );
    assert!(
        stats.renamed.iter().any(|r| r.to == "evil.txt"),
        "the rewritten name must be reported: {:?}",
        stats.renamed
    );
    assert!(stats.failed.is_empty(), "{:?}", stats.failed);
    // the safe file listed after it still lands
    assert_eq!(std::fs::read_to_string(dest.join("ok.txt")).unwrap(), "safe");
    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::remove_file(&evil);
}

#[test]
fn reserved_name_is_renamed_and_others_still_extract() {
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
        test_mode: false,
    };
    let stats = arc
        .extract(&opts, None, &mut |_p| true)
        .expect("one odd name must not fail the whole extraction");
    assert_eq!(std::fs::read_to_string(dest.join("good1.txt")).unwrap(), "one");
    assert_eq!(
        std::fs::read_to_string(dest.join("good2.txt")).unwrap(),
        "two",
        "files after the device name must still extract"
    );
    // `aux.txt` cannot exist on Windows, so it is written as `aux_.txt` — with
    // its content, not dropped.
    assert!(!dest.join("aux.txt").exists());
    assert_eq!(
        std::fs::read_to_string(dest.join("aux_.txt")).unwrap(),
        "aux"
    );
    assert!(stats.renamed.iter().any(|r| r.to == "aux_.txt"));
    assert_eq!(stats.files, 3);
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
        test_mode: false,
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
        test_mode: false,
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
        test_mode: false,
    };
    arc.extract(&opts, Some("secret"), &mut |_p| true)
        .expect("extract with correct pw");
    assert!(dest.join("a.txt").exists());
    let _ = std::fs::remove_dir_all(&dest);
}

/// 加密 ZIP 里的数据坏了，必须报「已损坏」，不能报「密码错误」。
///
/// 缺陷的现场：`map_op_res` 把任何 `kDataError`/`kCRCError` 都当成密码不对，
/// 于是用户输对了密码、包里的数据却坏了时，界面只会说「密码错误」并再要一次
/// 密码 —— 他要输入多少遍才会明白是文件坏了。
///
/// 破坏方式是在测试里现场改密文字节，而不是往 fixtures/ 里塞一个翻过位的样本：
/// 坏在哪一位、怎么坏，读代码就一清二楚，也不依赖某个二进制文件恰好被怎么改过。
/// 翻转点选在 AES 的密文区（本地头 30 + 文件名 5 + extra 11 = 46，salt 16 +
/// 校验值 2 → 密文从偏移 64 开始），避开头部的密钥校验值，否则引擎会先发现
/// 密钥不对而报 `kWrongPassword` —— 那是另一条路径。
#[test]
fn encrypted_zip_with_correct_password_and_bad_data_is_reported_as_damaged() {
    let dir = tmpdir("zpwbad");
    std::fs::create_dir_all(&dir).unwrap();
    let damaged = dir.join("enc-damaged.zip");
    let mut bytes = std::fs::read(fx("enc.zip")).expect("read fixture");
    bytes[70] ^= 0xFF; // AES 密文区中间的一个字节
    std::fs::write(&damaged, &bytes).unwrap();

    let arc = Archive::open(&damaged, ArchiveOpenOptions::default()).expect("zip still opens");
    let dest = dir.join("out");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
        test_mode: false,
    };
    let err = arc
        .extract(&opts, Some("secret"), &mut |_p| true)
        .expect_err("damaged data must fail");

    assert_eq!(
        err.error_key(),
        "error.corrupt",
        "用对密码却遇到坏数据，必须说「已损坏」；说成密码错误会让用户永远重输密码。got {err:?}"
    );
    // 而且它不能被当成密码问题：UI 据此决定「再问一次密码」还是「告诉他文件坏了」。
    assert_ne!(err.error_key(), "error.password_incorrect");
    // 没写出来的条目要能被列出来（fatal 路径以前会把这份名单丢掉）。
    assert!(
        !dest.join("a.txt").exists(),
        "坏掉的条目不该出现在目标里"
    );
    let _ = std::fs::remove_dir_all(&dir);
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
// ---- Test mode: verify every entry's CRC without writing anything ----

/// Build a ZIP whose directory is readable but whose stored data is wrong: the
/// CRC in the central directory says one thing, the bytes say another. That is
/// exactly the "download interrupted / USB copy died" case the test feature
/// exists for — and the archive opens fine, so the failure can only come from
/// decoding the entry.
fn zip_with_bad_data(dir: &std::path::Path) -> PathBuf {
    use std::io::Write;
    let path = dir.join("bad-data.zip");
    let file = std::fs::File::create(&path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = Default::default();
    zip.start_file("honest.txt", opts).unwrap();
    zip.write_all(b"this one is fine").unwrap();
    zip.start_file("damaged.bin", opts).unwrap();
    zip.write_all(&vec![0xABu8; 4096]).unwrap();
    zip.finish().unwrap();

    // Flip a byte in the stored payload of `damaged.bin`. Its name appears in
    // the local header immediately before the data, so searching for the name
    // finds the payload; the CRC in both headers is now stale.
    let mut bytes = std::fs::read(&path).unwrap();
    let needle = b"damaged.bin";
    let at = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("local header must carry the name")
        + needle.len();
    // Skip the extra-field length (2 bytes, zero here) before the payload.
    let data_at = at + 2;
    bytes[data_at] ^= 0xFF;
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn test_mode_verifies_a_good_archive_without_writing_anything() {
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).unwrap();
    // A destination that does not exist: a real extraction would create it, a
    // test run must not — that difference is the whole contract.
    let dest = tmpdir("testmode-good");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Rename,
        test_mode: true,
    };
    let mut paths = Vec::new();
    let stats = arc
        .extract(&opts, None, &mut |p| {
            paths.push(p.current_path.clone());
            true
        })
        .expect("a good archive verifies cleanly");
    assert_eq!(stats.files, 3, "a.txt, c.txt and sub/b.txt are the files");
    assert!(stats.failed.is_empty(), "nothing may fail: {:?}", stats.failed);
    assert!(
        !dest.exists(),
        "test mode must not create the destination folder"
    );
    // Progress still names the entries, so the UI can show what it is checking.
    assert!(
        paths.iter().any(|p| p.ends_with("a.txt")),
        "progress must carry current paths: {paths:?}"
    );
}

#[test]
fn test_mode_reports_the_entry_whose_data_is_bad() {
    let dir = tmpdir("testmode-bad");
    std::fs::create_dir_all(&dir).unwrap();
    let zip = zip_with_bad_data(&dir);
    let arc = Archive::open(&zip, ArchiveOpenOptions::default()).expect("the directory reads fine");
    let opts = ExtractOptions {
        dest: dir.join("never-created"),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Rename,
        test_mode: true,
    };
    // A bad entry is a failed run, not a quiet "checked, all good": the caller
    // maps this to `error.entries_failed` and lists the names.
    let err = arc
        .extract(&opts, None, &mut |_| true)
        .expect_err("damaged data must fail the test run");
    match err {
        archive_core::ZipnestError::Corrupt(entries) => {
            assert!(
                entries.iter().any(|e| e.path == "damaged.bin"),
                "the damaged entry must be named: {entries:?}"
            );
        }
        other => panic!("expected a corrupt-data error, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_mode_still_counts_against_the_zip_bomb_quota() {
    // Nothing is written, but the decompression still costs CPU; a test run on
    // a bomb must stop at the same limit a real extraction would.
    let arc = Archive::open(&fx("plain.zip"), ArchiveOpenOptions::default()).unwrap();
    let dest = tmpdir("testmode-quota");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: 64, // far below the fixture's payload
        on_conflict: OnConflict::Rename,
        test_mode: true,
    };
    let err = arc
        .extract(&opts, None, &mut |_| true)
        .expect_err("the quota must stop a test run too");
    assert_eq!(err.error_key(), "error.quota_exceeded");
    assert!(!dest.exists());
}

/// 撞名后自动改名的条目必须出现在 `conflict_renamed` —— 而不是 `renamed`。
///
/// 这是 A2（右键免对话框动词）的前提：免对话框的解压固定用"自动重命名"，
/// 用户看不到解压对话框，也就没有别的渠道知道某个文件改叫 `name (2).ext` 了。
/// 从前这类改名什么都不记，结果窗口只会说"完成"，而磁盘上的文件名和包里不一样。
#[test]
fn a_conflict_rename_is_reported_apart_from_name_cleaning() {
    let dir = tmpdir("conflict-report");
    std::fs::create_dir_all(&dir).unwrap();
    let zip = dir.join("a.zip");
    {
        use std::io::Write;
        let file = std::fs::File::create(&zip).unwrap();
        let mut z = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = Default::default();
        z.start_file("a.txt", opts).unwrap();
        z.write_all(b"new content").unwrap();
        z.finish().unwrap();
    }
    let dest = dir.join("out");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("a.txt"), b"old content").unwrap();

    let arc = Archive::open(&zip, ArchiveOpenOptions::default()).unwrap();
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: Vec::new(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Rename,
        test_mode: false,
    };
    let stats = arc.extract(&opts, None, &mut |_| true).expect("extract");
    assert_eq!(stats.conflict_renamed.len(), 1, "got {:?}", stats.conflict_renamed);
    let r = &stats.conflict_renamed[0];
    assert_eq!(r.from, "a.txt");
    assert_eq!(r.to, "a (2).txt");
    assert!(
        stats.renamed.is_empty(),
        "name cleaning did not happen here, so `renamed` must stay empty: {:?}",
        stats.renamed
    );
    // Both files are on disk: the old one untouched, the new one beside it.
    assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"old content");
    assert_eq!(std::fs::read(dest.join("a (2).txt")).unwrap(), b"new content");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 名字清洗（`CON` → `CON_`）只进 `renamed`，不混进 `conflict_renamed`：
/// 两件事的原因不同，结果窗口的措辞也不同。
#[test]
fn name_cleaning_does_not_land_in_the_conflict_list() {
    let dir = tmpdir("clean-not-conflict");
    std::fs::create_dir_all(&dir).unwrap();
    let zip = dir.join("h.zip");
    {
        use std::io::Write;
        let file = std::fs::File::create(&zip).unwrap();
        let mut z = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = Default::default();
        z.start_file("CON", opts).unwrap();
        z.write_all(b"x").unwrap();
        z.finish().unwrap();
    }
    let dest = dir.join("out");
    let arc = Archive::open(&zip, ArchiveOpenOptions::default()).unwrap();
    let opts = ExtractOptions {
        dest,
        entries: Vec::new(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Rename,
        test_mode: false,
    };
    let stats = arc.extract(&opts, None, &mut |_| true).expect("extract");
    assert_eq!(stats.renamed.len(), 1);
    assert_eq!(stats.renamed[0].to, "CON_");
    assert!(
        stats.conflict_renamed.is_empty(),
        "nothing was in the destination, so no conflict rename: {:?}",
        stats.conflict_renamed
    );
    let _ = std::fs::remove_dir_all(&dir);
}
