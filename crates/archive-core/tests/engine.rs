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

use archive_core::ExtractOptions;

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
        overwrite: true,
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
        overwrite: true,
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
        overwrite: true,
    };
    let err = arc.extract(&opts, None, &mut |_p| true).expect_err("quota must trip");
    assert_eq!(err.error_key(), "error.quota_exceeded");
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn zip_slip_entry_is_blocked() {
    use std::io::Write as _;
    // Craft an archive with a malicious relative path.
    let evil = std::env::temp_dir().join(format!("zn-slip-{}.zip", std::process::id()));
    {
        let f = std::fs::File::create(&evil).unwrap();
        let mut w = zip::ZipWriter::new(f);
        w.start_file::<_, ()>("../../evil.txt", Default::default()).unwrap();
        w.write_all(b"pwned").unwrap();
        w.finish().unwrap();
    }
    let arc = Archive::open(&evil, ArchiveOpenOptions::default()).expect("open evil");
    let dest = tmpdir("slipdest");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        overwrite: true,
    };
    let err = arc.extract(&opts, None, &mut |_p| true).expect_err("must be blocked");
    assert_eq!(err.error_key(), "error.security_blocked");
    // double insurance: nothing may land outside dest
    let escaped = std::env::temp_dir().join("evil.txt");
    assert!(!escaped.exists(), "must not escape temp");
    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::remove_file(&evil);
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
        overwrite: true,
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
        overwrite: true,
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
        overwrite: true,
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