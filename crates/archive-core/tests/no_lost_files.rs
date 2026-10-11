//! One rule, tested hard: **an extraction that reports success must have
//! written every entry it was asked to write.**
//!
//! Entry names that Windows cannot store verbatim (device names, illegal
//! characters, over-long components, `..` traversal, absolute/drive/UNC
//! prefixes) are extremely common in archives produced on other systems or by
//! older tools. They must not cost the user a file: the name is mapped to a
//! safe name inside the destination and the entry is still written, and the
//! mapping is reported in `ExtractStats::renamed`.
//!
//! Only a genuine write failure may be missing from disk, and those must come
//! back in `ExtractStats::failed` so the UI reports a failure instead of a
//! success.

mod common;

use archive_core::{Archive, ArchiveOpenOptions, ExtractOptions, OnConflict};
use common::{hostile_zip, tmpdir, walk_files};

#[test]
fn every_weird_name_extracts_and_nothing_is_ever_lost() {
    let (zip, cases, dirs) = hostile_zip("nolost");
    let arc = Archive::open(&zip, ArchiveOpenOptions::default()).expect("open hostile zip");
    let dest = tmpdir("nolost-out");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    let stats = arc
        .extract(&opts, None, &mut |_p| true)
        .expect("a weird name is not a reason to fail the whole run");

    let landed = walk_files(&dest);
    let mut want: Vec<Vec<u8>> = cases.iter().map(|(_, b)| b.clone()).collect();
    let mut got: Vec<Vec<u8>> = landed.iter().map(|(_, b)| b.clone()).collect();
    want.sort();
    got.sort();

    assert_eq!(
        got,
        want,
        "content mismatch after extracting {} entries.\n  skipped: {:?}\n  failed: {:?}\n  on disk: {:?}",
        cases.len(),
        stats.skipped,
        stats.failed,
        landed.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>()
    );
    assert!(
        stats.failed.is_empty(),
        "no entry may be reported as failed on an ordinary filesystem: {:?}",
        stats.failed
    );
    assert!(
        stats.skipped.is_empty(),
        "nothing may be skipped for its name: {:?}",
        stats.skipped
    );
    assert_eq!(stats.files as usize, cases.len(), "files written");
    for d in &dirs {
        let d = String::from_utf8_lossy(d).trim_end_matches('/').to_string();
        assert!(dest.join(&d).is_dir(), "directory {d} must exist");
    }
    // Every rewritten name is reported, and every reported name is on disk.
    // (Ten hostile names plus the over-long one; `normal\ok.txt`-style entries
    // only had their separators normalized and must NOT be listed here.)
    assert_eq!(
        stats.renamed.len(),
        11,
        "which names changed: {:?}",
        stats.renamed
    );
    for r in &stats.renamed {
        let on_disk = dest.join(r.to.replace('/', std::path::MAIN_SEPARATOR_STR));
        assert!(
            on_disk.is_file(),
            "{} was reported as written to {} but is not there",
            r.from,
            r.to
        );
    }
    // The over-long entry keeps its extension and a legal component length.
    let long = landed
        .iter()
        .find(|(_, b)| b == b"too-long")
        .expect("the 300-character entry must be on disk");
    assert!(long.0.ends_with(".txt"), "extension lost: {}", long.0);
    assert!(long.0.len() <= 255, "component too long: {}", long.0);

    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::remove_file(&zip);
}

/// Nothing may ever land outside the destination — the traversal, absolute,
/// drive and UNC entries above are the reason this matters.
#[test]
fn no_weird_name_escapes_the_destination() {
    let (zip, _cases, _dirs) = hostile_zip("noescape");
    let arc = Archive::open(&zip, ArchiveOpenOptions::default()).unwrap();
    let dest = tmpdir("noescape-out");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    arc.extract(&opts, None, &mut |_p| true).expect("extract");

    for probe in [
        std::env::temp_dir().join("escape.txt"),
        std::env::temp_dir().join("rooted.txt"),
        std::env::temp_dir().join("drive.txt"),
    ] {
        assert!(!probe.exists(), "{} escaped the destination", probe.display());
    }
    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::remove_file(&zip);
}

/// A genuine write failure must be *reported*, never counted as success.
#[test]
fn a_file_that_cannot_be_written_is_reported_as_failed() {
    let (zip, cases, _dirs) = hostile_zip("blocked");
    let arc = Archive::open(&zip, ArchiveOpenOptions::default()).unwrap();
    let dest = tmpdir("blocked-out");
    std::fs::create_dir_all(&dest).unwrap();
    // `deep` has to be a directory for `deep/a/b/c/d/e/f/g.txt`. A *file* with
    // that name makes that entry unwritable on every platform.
    std::fs::write(dest.join("deep"), b"in the way").unwrap();

    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    let stats = arc.extract(&opts, None, &mut |_p| true).expect("extract");

    assert!(
        stats.failed.iter().any(|f| f.path.ends_with("g.txt")),
        "the blocked entry must be reported as failed, got {:?}",
        stats.failed
    );
    let f = stats.failed.iter().find(|f| f.path.ends_with("g.txt")).unwrap();
    assert_eq!(f.reason, "error.entry_io");
    assert!(f.detail.is_some(), "the OS reason must reach the user");
    // Everything else still extracted: one bad path must not throw the run
    // away — but the run as a whole is a failure, not a success.
    assert_eq!(stats.files as usize, cases.len() - 1);
    assert!(!stats.failed.is_empty());
    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::remove_file(&zip);
}

/// Two entries whose names only differ where Windows cannot store the
/// difference (case, or after cleaning) must both land, not overwrite each
/// other.
#[test]
fn colliding_names_all_reach_the_disk() {
    let mut z = common::Zip::default();
    for (name, body) in [
        ("report.txt", "first"),
        ("Report.TXT", "second"),
        ("a:b.txt", "third"),
        ("a_b.txt", "fourth"),
    ] {
        z.add(name.as_bytes(), body.as_bytes());
    }
    let zip = std::env::temp_dir().join(format!("zn-collide-{}.zip", std::process::id()));
    std::fs::write(&zip, z.finish()).unwrap();

    let arc = Archive::open(&zip, ArchiveOpenOptions::default()).unwrap();
    let dest = tmpdir("collide-out");
    let opts = ExtractOptions {
        dest: dest.clone(),
        entries: (0..arc.len()).collect(),
        max_total_bytes: u64::MAX,
        on_conflict: OnConflict::Overwrite,
    };
    let stats = arc.extract(&opts, None, &mut |_p| true).expect("extract");

    let landed = walk_files(&dest);
    let mut got: Vec<Vec<u8>> = landed.iter().map(|(_, b)| b.clone()).collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            b"first".to_vec(),
            b"fourth".to_vec(),
            b"second".to_vec(),
            b"third".to_vec()
        ],
        "all four must be on disk, got {:?}",
        landed.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>()
    );
    assert_eq!(stats.files, 4);
    assert_eq!(
        stats.renamed.len(),
        3,
        "case collision, illegal character, and the collision it caused: {:?}",
        stats.renamed
    );
    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::remove_file(&zip);
}
