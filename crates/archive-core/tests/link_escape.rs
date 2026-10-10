//! A hostile TAR must not write outside the destination.
//!
//! The fixture (`fixtures/link-escape.tar`, built by
//! `fixtures/make_link_escape.py`) contains a directory symlink `d` -> `..`
//! followed by `d/pwn.txt`. If the extractor follows the link, `pwn.txt` lands
//! one level *above* the destination — even though the entry path itself passed
//! `archive_security::sanitize_entry_path`, because only names are vetted there.
//!
//! Two things can stop it and this test insists that at least one does: the
//! engine may refuse to materialise the link on Windows (creating one needs
//! SeCreateSymbolicLinkPrivilege), or `extract::has_link_ancestor` refuses the
//! write. Either way nothing may appear outside the destination, and the
//! ordinary entry must still be extracted (so a pass cannot mean "nothing ran").

use std::path::PathBuf;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/link-escape.tar")
}

#[test]
fn a_symlink_entry_cannot_write_outside_the_destination() {
    let root = std::env::temp_dir().join(format!("zipnest-escape-{}", std::process::id()));
    let dest = root.join("dest");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&dest).unwrap();

    let archive =
        archive_core::Archive::open(&fixture(), archive_core::ArchiveOpenOptions::default())
            .expect("the fixture opens");

    let opts = archive_core::ExtractOptions {
        dest: dest.clone(),
        entries: Vec::new(), // empty selection = the whole archive
        max_total_bytes: u64::MAX,
        on_conflict: archive_core::OnConflict::Overwrite,
    };
    let stats = archive
        .extract(&opts, None, &mut |_| true)
        .expect("extraction runs to completion");

    // Nothing may have escaped: the only thing beside `dest` is `dest` itself.
    assert!(
        !root.join("pwn.txt").exists(),
        "the symlinked entry escaped to {}",
        root.join("pwn.txt").display()
    );
    // And the ordinary entry landed, so the run really did extract something.
    assert!(
        dest.join("keep.txt").exists(),
        "keep.txt should still be extracted (stats: {stats:?})"
    );

    // Which layer stopped it? Printed rather than asserted: it is a property of
    // the engine on this platform, not of this code. Run with --nocapture.
    //
    // Measured with the shipped 7z.dll on Windows 10 build 28020: the link entry
    // comes out as a regular, empty *file* (the handler does not create symlinks
    // here), so `d/pwn.txt` cannot be created at all — it is skipped, and the
    // write never reaches `has_link_ancestor`. The guard is what protects the
    // other shapes of the same attack: another engine (the DLL is replaceable by
    // design), a hard-link entry, or a symlink/junction that already exists in
    // the destination.
    let link = dest.join("d");
    let link_kind = match std::fs::symlink_metadata(&link) {
        Ok(md) if md.file_type().is_symlink() => {
            "a symlink (the engine created it; has_link_ancestor refused the write)".to_string()
        }
        Ok(md) if md.is_dir() => "a real directory".to_string(),
        Ok(md) => format!("a regular file ({} bytes)", md.len()),
        Err(_) => "absent (the engine skipped the link entry)".to_string(),
    };
    eprintln!(
        "link entry d: {link_kind}; wrote {} file(s), skipped {}. pwn.txt under d: {}",
        stats.files,
        stats.skipped.len(),
        dest.join("d").join("pwn.txt").exists()
    );

    let _ = std::fs::remove_dir_all(&root);
}
