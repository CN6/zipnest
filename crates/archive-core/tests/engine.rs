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