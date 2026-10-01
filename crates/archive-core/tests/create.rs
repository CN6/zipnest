use archive_core::{create::{collect_sources, create_archive, CreateSource}, Archive, ArchiveOpenOptions, types::{CreateFormat, CompressionLevel, CompressionMethod, CreateOptions, SfxKind}};
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf { std::env::temp_dir().join(format!("zn-create-{}-{}", std::process::id(), name)) }

/// The official `7z.exe` client, used to prove an SFX holds a readable archive.
fn seven_zip_exe() -> PathBuf {
    for p in [r"C:\Program Files\7-Zip\7z.exe", r"C:\Program Files (x86)\7-Zip\7z.exe"] {
        let pb = PathBuf::from(p);
        if pb.exists() { return pb; }
    }
    panic!("7z.exe not found; required to verify SFX output");
}

/// `7z l <archive>` stdout.
fn list_with_7z(archive: &std::path::Path) -> String {
    let out = std::process::Command::new(seven_zip_exe()).arg("l").arg(archive).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The vendored stub for `kind`, resolved relative to this crate at test time.
fn vendored_stub(kind: SfxKind) -> Vec<u8> {
    let name = match kind { SfxKind::Gui => "7z.sfx", SfxKind::Console => "7zCon.sfx" };
    let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../vendor/7zip-sfx")).join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("stub {} unreadable: {e}", path.display()))
}

fn write_src(dir: &std::path::Path, rel: &str, body: &[u8]) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, body).unwrap();
    p
}

fn opts(fmt: CreateFormat) -> CreateOptions {
    CreateOptions { format: fmt, level: CompressionLevel::Store, method: CompressionMethod::Copy,
        password: None, encrypt_names: false, volume_bytes: None, sfx: None }
}

/// Names in `dir` that look like the driver's scratch `.tar` files.
fn leftover_tmp(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir).unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("zipnest"))
        .collect()
}

#[test]
fn creates_zip_with_one_file_roundtrip() {
    let base = tmp("zip"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"hello create");
    let dest = base.join("out.zip");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let mut n = 0u64;
    create_archive(&srcs, &dest, &opts(CreateFormat::Zip), &mut |_| { n += 1; true }).unwrap();
    assert!(dest.exists(), "output zip must exist");
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    let es = arc.entries().unwrap();
    assert_eq!(es.len(), 1);
    assert_eq!(es[0].path, "a.txt");
    let bytes = arc.read_entry(0, &ArchiveOpenOptions::default(), None).unwrap();
    assert_eq!(bytes, b"hello create");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn deflate_compresses_repetitive_zip() {
    let base = tmp("defl"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.txt", &vec![b'A'; 200_000]);
    let store = base.join("store.zip"); let deflate = base.join("deflate.zip");
    let srcs = vec![CreateSource { path: src.clone(), node: "big.txt".into() }];
    let mut o = opts(CreateFormat::Zip); o.level = CompressionLevel::Normal; o.method = CompressionMethod::Deflate;
    create_archive(&srcs, &store, &{ let mut s = o.clone(); s.level = CompressionLevel::Store; s.method = CompressionMethod::Copy; s }, &mut |_| true).unwrap();
    create_archive(&srcs, &deflate, &o, &mut |_| true).unwrap();
    assert!(std::fs::metadata(&deflate).unwrap().len() * 4 < std::fs::metadata(&store).unwrap().len(),
        "deflate must be far smaller than store on repetitive data");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn lzma2_is_default_small_for_7z() {
    let base = tmp("lz"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.txt", &vec![b'B'; 200_000]);
    let dest = base.join("out.7z");
    let srcs = vec![CreateSource { path: src, node: "big.txt".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.level = CompressionLevel::Normal; o.method = CompressionMethod::Lzma2;
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    assert!(std::fs::metadata(&dest).unwrap().len() < 20_000, "LZMA2 must compress repetitive data well");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn copy_method_disables_7z_compression() {
    let base = tmp("7zcopy"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.txt", &vec![b'C'; 200_000]);
    let dest = base.join("out.7z");
    let srcs = vec![CreateSource { path: src, node: "big.txt".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.level = CompressionLevel::Store; o.method = CompressionMethod::Copy;
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    assert!(std::fs::metadata(&dest).unwrap().len() > 100_000,
        "Copy method must leave repetitive data essentially uncompressed; the LZMA2 default would be tiny");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn collects_directory_tree_preserving_structure() {
    let base = tmp("tree"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    write_src(&base, "src/a.txt", b"aaa");
    write_src(&base, "src/sub/b.txt", b"bbb");
    let dest = base.join("out.zip");
    let sources = archive_core::create::collect_sources(&[base.join("src")], &base).unwrap();
    create_archive(&sources, &dest, &opts(CreateFormat::Zip), &mut |_| true).unwrap();
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    let paths: Vec<String> = arc.entries().unwrap().iter().map(|e| e.path.replace('\\', "/")).collect();
    assert!(paths.contains(&"src/a.txt".to_string()));
    assert!(paths.contains(&"src/sub/b.txt".to_string()));
    assert!(paths.iter().any(|p| p == "src/sub/" || p == "src/sub"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn collects_empty_directory_entry() {
    let base = tmp("emptydir"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(base.join("src/empty")).unwrap();
    write_src(&base, "src/keep.txt", b"k");
    let dest = base.join("out.zip");
    let sources = collect_sources(&[base.join("src")], &base).unwrap();
    create_archive(&sources, &dest, &opts(CreateFormat::Zip), &mut |_| true).unwrap();
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    let paths: Vec<String> = arc.entries().unwrap().iter().map(|e| e.path.replace('\\', "/")).collect();
    assert!(paths.iter().any(|p| p == "src/empty/" || p == "src/empty"),
        "empty dir must be emitted as a directory entry, got {paths:?}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn rejects_source_outside_base() {
    let base = tmp("outside"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let other = tmp("outside-elsewhere"); let _ = std::fs::remove_dir_all(&other); std::fs::create_dir_all(&other).unwrap();
    let src = write_src(&other, "x.txt", b"x");
    let err = collect_sources(&[src], &base).unwrap_err();
    assert_eq!(err.kind(), "error.io", "an input not under base must be rejected, got {err:?}");
    let _ = std::fs::remove_dir_all(&base); let _ = std::fs::remove_dir_all(&other);
}

#[test]
fn skips_symlinked_entries() {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    let base = tmp("symlink"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    write_src(&base, "src/real.txt", b"real");
    let link_file = base.join("src/link.txt");
    let link_dir = base.join("src/linkdir");
    // Symlink creation needs developer mode / elevation on Windows; skip if denied.
    if symlink_file("real.txt", &link_file).is_err() { let _ = std::fs::remove_dir_all(&base); return; }
    let _ = symlink_dir("sub", &link_dir);
    let sources = collect_sources(&[base.join("src")], &base).unwrap();
    assert!(sources.iter().all(|s| s.node != "src/link.txt"), "file symlink must be skipped");
    assert!(sources.iter().all(|s| s.node != "src/linkdir"), "dir symlink must be skipped");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn creates_plain_tar_roundtrip() {
    let base = tmp("tar"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"plain tar body");
    let dest = base.join("out.tar");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    create_archive(&srcs, &dest, &opts(CreateFormat::Tar), &mut |_| true).unwrap();
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    let e = arc.entries().unwrap().into_iter().find(|e| !e.is_dir).expect("file entry");
    assert_eq!(arc.read_entry(e.index, &ArchiveOpenOptions::default(), None).unwrap(), b"plain tar body");
    let _ = std::fs::remove_dir_all(&base);
}

// NOTE: 7-Zip's GZIP/BZIP2/XZ `IInArchive` exposes the decompressed payload as
// a single entry; it does *not* cascade into TAR (that is a client-side
// `CArchiveLink` feature used by 7z.exe/7zFM, verified against 7z 26.03). So a
// compressed TAR roundtrip is verified in two explicit layers: open the
// compressed stream, then open the TAR it carries.
fn assert_compressed_tar_roundtrip(fmt: CreateFormat, dest_name: &str, body: &[u8], tag: &str) {
    let base = tmp(tag); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    write_src(&base, "src/a.txt", body);
    let dest = base.join(dest_name);
    let sources = archive_core::create::collect_sources(&[base.join("src")], &base).unwrap();
    create_archive(&sources, &dest, &opts(fmt), &mut |_| true).unwrap();

    // The intermediate `.tar` must not survive a successful create.
    let leftovers = leftover_tmp(&base);
    assert!(leftovers.is_empty(), "temp tar left behind after success: {leftovers:?}");

    // Layer 1: the compressor holds a single stream carrying the TAR. GZIP stores
    // the name in its header, so its entry is `out.tar` (the destination minus
    // `.gz`); BZIP2/XZ store no name and 7-Zip's *client* fills it from the
    // archive filename, so the raw reader reports an empty path for those.
    let outer = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    let outer_entries = outer.entries().unwrap();
    assert_eq!(outer_entries.len(), 1, "compressor holds exactly one stream");
    if fmt == CreateFormat::TarGz {
        assert_eq!(outer_entries[0].path.replace('\\', "/"), "out.tar");
    }
    let tar_bytes = outer.read_entry(outer_entries[0].index, &ArchiveOpenOptions::default(), None).unwrap();

    // Layer 2: the decompressed bytes are a valid TAR carrying the source tree.
    let inner_tar = base.join("out.tar");
    std::fs::write(&inner_tar, &tar_bytes).unwrap();
    let inner = Archive::open(&inner_tar, ArchiveOpenOptions::default()).unwrap();
    let e = inner.entries().unwrap().into_iter().find(|e| !e.is_dir).expect("file entry");
    assert_eq!(inner.read_entry(e.index, &ArchiveOpenOptions::default(), None).unwrap(), body);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn creates_targz_roundtrip() {
    assert_compressed_tar_roundtrip(CreateFormat::TarGz, "out.tar.gz", b"tar gz body", "tgz");
}

#[test]
fn creates_tarbz2_roundtrip() {
    assert_compressed_tar_roundtrip(CreateFormat::TarBz2, "out.tar.bz2", b"tar bz2 body", "tbz2");
}

#[test]
fn creates_tarxz_roundtrip() {
    assert_compressed_tar_roundtrip(CreateFormat::TarXz, "out.tar.xz", b"tar xz body", "txz");
}

#[test]
fn cleans_up_temp_tar_on_failure() {
    let base = tmp("tarfail"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    write_src(&base, "src/a.txt", b"x");
    // Make the destination unwritable as a file so the *second* pass fails after
    // the intermediate `.tar` has already been written.
    let dest = base.join("out.tar.gz");
    std::fs::create_dir_all(&dest).unwrap();
    let sources = archive_core::create::collect_sources(&[base.join("src")], &base).unwrap();
    let err = create_archive(&sources, &dest, &opts(CreateFormat::TarGz), &mut |_| true).unwrap_err();
    assert_eq!(err.kind(), "error.io", "outer pass must surface the io error, got {err:?}");
    let leftovers = leftover_tmp(&base);
    assert!(leftovers.is_empty(), "temp tar left behind after failure: {leftovers:?}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn create_reports_progress_and_cancel_stops() {
    let base = tmp("prog"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.bin", &vec![7u8; 3_000_000]);
    let dest = base.join("out.7z");
    let srcs = vec![CreateSource { path: src, node: "big.bin".into() }];
    // Collect every sample; the closure runs on an engine thread and calling
    // panic!/assert! from it could abort the process, so all checks run after.
    let mut samples: Vec<(u64, u64)> = Vec::new();
    let mut o = opts(CreateFormat::SevenZ); o.level = CompressionLevel::Normal;
    create_archive(&srcs, &dest, &o, &mut |p| { samples.push((p.done_bytes, p.total_bytes)); true }).unwrap();

    assert!(!samples.is_empty(), "progress callback was never invoked");
    for &(done, total) in &samples {
        assert!(done <= total, "done_bytes {done} exceeded total_bytes {total}");
    }
    for w in samples.windows(2) {
        assert!(w[0].0 <= w[1].0, "done_bytes must be non-decreasing, got {:?}", &samples);
    }
    assert!(samples.last().unwrap().0 > 0, "no bytes reported as completed");
    assert_eq!(samples.last().unwrap().1, 3_000_000, "total_bytes must be the pre-scanned source size");

    // Keep the source and its directory alive for the cancel pass: deleting the
    // whole base here would make the second create fail with an io error before
    // the handler can ever consult the progress callback.
    let _ = std::fs::remove_file(&dest);

    let dest2 = base.join("cancel.7z");
    let err = create_archive(&srcs, &dest2, &o, &mut |_| false).expect_err("cancel must abort");
    assert_eq!(err.error_key(), "error.cancelled");
    let _ = std::fs::remove_dir_all(&base);
}

/// Cancel during the WRAP pass of a compressed TAR (not the first TAR pass):
/// the temp `.tar` must still be cleaned up. The wrap pass is the one that
/// reports the inner node (`out.tar`); the first pass only reports sources.
#[test]
fn cancelling_wrap_pass_removes_temp_tar() {
    let base = tmp("wrapcancel"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    write_src(&base, "src/big.bin", &vec![7u8; 3_000_000]);
    let dest = base.join("out.tar.gz");
    let sources = archive_core::create::collect_sources(&[base.join("src")], &base).unwrap();
    let mut saw_wrap = false;
    let err = create_archive(&sources, &dest, &opts(CreateFormat::TarGz), &mut |p| {
        if p.current_path.ends_with(".tar") { saw_wrap = true; return false; }
        true
    }).expect_err("cancel during wrap must abort");
    assert!(saw_wrap, "cancel must have fired during the wrap pass, not the first pass");
    assert_eq!(err.error_key(), "error.cancelled", "got {err:?}");
    let leftovers = leftover_tmp(&base);
    assert!(leftovers.is_empty(), "temp tar left behind after wrap cancel: {leftovers:?}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn creates_7z_with_one_file_roundtrip() {
    let base = tmp("7z"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"seven zip body");
    let dest = base.join("out.7z");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    create_archive(&srcs, &dest, &opts(CreateFormat::SevenZ), &mut |_| true).unwrap();
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    assert_eq!(arc.read_entry(0, &ArchiveOpenOptions::default(), None).unwrap(), b"seven zip body");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn creates_encrypted_zip_needs_password() {
    let base = tmp("encz"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"secret body");
    let dest = base.join("enc.zip");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let mut o = opts(CreateFormat::Zip); o.password = Some("pw123".into());
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    // Without a password the archive still opens (ZIP encrypts data, not the
    // central directory) but every file entry must be flagged encrypted.
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    assert!(arc.entries().unwrap().iter().any(|e| e.encrypted), "entry must be marked encrypted");
    // WinZip AES marks local headers with extra field id 0x9901 ("AE"); this
    // proves AES-256 was used, not legacy ZipCrypto.
    let raw = std::fs::read(&dest).unwrap();
    assert!(raw.windows(2).any(|w| w == [0x01, 0x99]), "ZIP must carry the WinZip AES extra field");
    // The correct password reads the plaintext back.
    let arc = Archive::open(&dest, ArchiveOpenOptions { password: Some("pw123".into()) }).unwrap();
    assert_eq!(arc.read_entry(0, &ArchiveOpenOptions { password: Some("pw123".into()) }, None).unwrap(), b"secret body");
    // A wrong password must not yield plaintext: the ciphertext is key-gated.
    let err = arc.read_entry(0, &ArchiveOpenOptions { password: Some("WRONG".into()) }, None).unwrap_err();
    assert!(matches!(err.kind(), "error.password_incorrect" | "error.engine"),
        "wrong password must fail, got {err:?}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn creates_encrypted_7z_needs_password() {
    let base = tmp("enc7z"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"seven secret");
    let dest = base.join("enc.7z");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.password = Some("pw7z".into());
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    assert!(arc.entries().unwrap().iter().any(|e| e.encrypted), "entry must be marked encrypted");
    let arc = Archive::open(&dest, ArchiveOpenOptions { password: Some("pw7z".into()) }).unwrap();
    assert_eq!(arc.read_entry(0, &ArchiveOpenOptions { password: Some("pw7z".into()) }, None).unwrap(), b"seven secret");
    // A wrong password must not yield plaintext: the ciphertext is key-gated.
    let err = arc.read_entry(0, &ArchiveOpenOptions { password: Some("WRONG".into()) }, None).unwrap_err();
    assert!(matches!(err.kind(), "error.password_incorrect" | "error.engine"),
        "wrong password must fail, got {err:?}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn creates_header_encrypted_7z_hides_names() {
    let base = tmp("he"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"hidden");
    let dest = base.join("he.7z");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.password = Some("pw".into()); o.encrypt_names = true;
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    // Header-encrypted: even the entry names are sealed, so opening without a
    // password fails before any name is exposed.
    let err = Archive::open(&dest, ArchiveOpenOptions::default()).expect_err("no password");
    assert_eq!(err.error_key(), "error.password_required");
    // The correct password reveals the names and the payload (proof the archive
    // is merely header-encrypted, not corrupt).
    let pw = ArchiveOpenOptions { password: Some("pw".into()) };
    let arc = Archive::open(&dest, pw).unwrap();
    let names: Vec<String> = arc.entries().unwrap().iter().map(|e| e.path.replace('\\', "/")).collect();
    assert!(names.contains(&"a.txt".to_string()), "correct password must reveal names, got {names:?}");
    assert_eq!(arc.read_entry(0, &ArchiveOpenOptions { password: Some("pw".into()) }, None).unwrap(), b"hidden");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn rejects_7z_header_encryption_without_password() {
    // Encrypting the 7z index without a key is impossible; the request must be
    // refused rather than silently writing an unencrypted index.
    let base = tmp("he-nopw"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"x");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let dest = base.join("out.7z");
    let mut o = opts(CreateFormat::SevenZ); o.encrypt_names = true;
    let err = create_archive(&srcs, &dest, &o, &mut |_| true)
        .expect_err("header encryption without a password must be rejected");
    assert_eq!(err.error_key(), "error.password_required");
    assert!(!dest.exists(), "no archive may be written when header encryption is refused");
    let _ = std::fs::remove_dir_all(&base);
}

/// Concatenate `<stem>.001`, `.002`, ... into `joined` and return the volume
/// paths in order. The split is a plain byte partition, so joining the volumes
/// rebuilds the original single-stream archive.
fn join_volumes(stem: &std::path::Path, joined: &std::path::Path) -> Vec<PathBuf> {
    let mut bytes = Vec::new();
    let mut vols = Vec::new();
    for i in 1u32.. {
        let p = PathBuf::from(format!("{}.{:03}", stem.display(), i));
        if !p.exists() { break; }
        bytes.extend(std::fs::read(&p).unwrap());
        vols.push(p);
    }
    std::fs::write(joined, &bytes).unwrap();
    vols
}

/// Create a 2.5 MB stored source split into 1 MB volumes and assert the volume
/// files partition the archive losslessly and open as a normal archive.
fn assert_split_roundtrip(fmt: CreateFormat, tag: &str, joined_name: &str) {
    let base = tmp(tag); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.bin", &vec![0x5Au8; 2_500_000]);
    let dest = base.join(if fmt == CreateFormat::SevenZ { "vol.7z" } else { "vol.zip" });
    let srcs = vec![CreateSource { path: src, node: "big.bin".into() }];
    let mut o = opts(fmt); o.volume_bytes = Some(1_000_000);
    let stats = create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();

    assert!(!dest.exists(), "the unsplit base path must not be created");
    let v1 = PathBuf::from(format!("{}.001", dest.display()));
    let v2 = PathBuf::from(format!("{}.002", dest.display()));
    assert!(v1.exists(), "first volume must exist");
    assert!(v2.exists(), "second volume must exist");

    let joined = base.join(joined_name);
    let vols = join_volumes(&dest, &joined);
    assert!(vols.len() >= 3, "2.5 MB in 1 MB volumes must yield >=3 volumes");
    for (i, v) in vols.iter().enumerate() {
        let len = std::fs::metadata(v).unwrap().len();
        if i + 1 < vols.len() {
            assert_eq!(len, 1_000_000, "volume {i} must be exactly full");
        } else {
            assert!(len > 0 && len <= 1_000_000, "last volume size {len} out of range");
        }
    }
    assert_eq!(stats.bytes_out, std::fs::metadata(&joined).unwrap().len(),
        "stats.bytes_out must equal the total split size");

    let arc = Archive::open(&joined, ArchiveOpenOptions::default()).unwrap();
    let e = arc.entries().unwrap().into_iter()
        .find(|e| e.path.ends_with("big.bin")).expect("big.bin present");
    assert_eq!(
        arc.read_entry(e.index, &ArchiveOpenOptions::default(), None).unwrap().len(),
        2_500_000
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn splits_7z_into_volumes() {
    assert_split_roundtrip(CreateFormat::SevenZ, "vol7z", "vol.joined.7z");
}

#[test]
fn splits_zip_into_volumes() {
    assert_split_roundtrip(CreateFormat::Zip, "volzip", "vol.joined.zip");
}

#[test]
fn opens_split_7z_from_first_volume() {
    let base = tmp("rvol7z"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.bin", &vec![0x33u8; 2_500_000]);
    let dest = base.join("vol.7z");
    let srcs = vec![CreateSource { path: src, node: "big.bin".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.volume_bytes = Some(1_000_000);
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    assert!(base.join("vol.7z.002").exists());
    let arc = Archive::open(&base.join("vol.7z.001"), ArchiveOpenOptions::default()).unwrap();
    let e = arc.entries().unwrap().into_iter().find(|e| e.path.ends_with("big.bin")).expect("entry");
    assert_eq!(arc.read_entry(e.index, &ArchiveOpenOptions::default(), None).unwrap().len(), 2_500_000);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn opens_split_zip_from_first_volume() {
    let base = tmp("rvolzip"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.bin", &vec![0x44u8; 2_500_000]);
    let dest = base.join("vol.zip");
    let srcs = vec![CreateSource { path: src, node: "big.bin".into() }];
    let mut o = opts(CreateFormat::Zip); o.volume_bytes = Some(1_000_000);
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    assert!(base.join("vol.zip.002").exists());
    let arc = Archive::open(&base.join("vol.zip.001"), ArchiveOpenOptions::default()).unwrap();
    let e = arc.entries().unwrap().into_iter().find(|e| e.path.ends_with("big.bin")).expect("entry");
    assert_eq!(arc.read_entry(e.index, &ArchiveOpenOptions::default(), None).unwrap().len(), 2_500_000);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn opens_single_volume_7z_from_first_volume() {
    let base = tmp("svol7z"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "small.bin", b"tiny seven zip body");
    let dest = base.join("vol.7z");
    let srcs = vec![CreateSource { path: src, node: "small.bin".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.volume_bytes = Some(1_000_000);
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    assert!(base.join("vol.7z.001").exists(), "first volume must exist");
    assert!(!base.join("vol.7z.002").exists(), "single-volume content must not spill");
    let arc = Archive::open(&base.join("vol.7z.001"), ArchiveOpenOptions::default()).unwrap();
    let e = arc.entries().unwrap().into_iter().find(|e| e.path.ends_with("small.bin")).expect("entry");
    assert_eq!(arc.read_entry(e.index, &ArchiveOpenOptions::default(), None).unwrap(), b"tiny seven zip body");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn opens_single_volume_zip_from_first_volume() {
    let base = tmp("svolzip"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "small.bin", b"tiny zip body");
    let dest = base.join("vol.zip");
    let srcs = vec![CreateSource { path: src, node: "small.bin".into() }];
    let mut o = opts(CreateFormat::Zip); o.volume_bytes = Some(1_000_000);
    create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();
    assert!(base.join("vol.zip.001").exists(), "first volume must exist");
    assert!(!base.join("vol.zip.002").exists(), "single-volume content must not spill");
    let arc = Archive::open(&base.join("vol.zip.001"), ArchiveOpenOptions::default()).unwrap();
    let e = arc.entries().unwrap().into_iter().find(|e| e.path.ends_with("small.bin")).expect("entry");
    assert_eq!(arc.read_entry(e.index, &ArchiveOpenOptions::default(), None).unwrap(), b"tiny zip body");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn deletes_partial_volumes_on_cancel() {
    let base = tmp("volcancel"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.bin", &vec![0x5Au8; 2_500_000]);
    let dest = base.join("vol.7z");
    let srcs = vec![CreateSource { path: src, node: "big.bin".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.volume_bytes = Some(1_000_000);
    let err = create_archive(&srcs, &dest, &o, &mut |_| false)
        .expect_err("cancelled create must fail");
    assert_eq!(err.kind(), "error.cancelled", "got {err:?}");
    for i in 1..=4 {
        let p = base.join(format!("vol.7z.{i:03}"));
        assert!(!p.exists(), "partial volume {} must be deleted on cancel", p.display());
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn rejects_volumes_for_tar_formats() {
    let base = tmp("voltar"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"x");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let mut o = opts(CreateFormat::Tar); o.volume_bytes = Some(1_000_000);
    let err = create_archive(&srcs, &base.join("out.tar"), &o, &mut |_| true)
        .expect_err("volumes on TAR must be rejected");
    assert_eq!(err.kind(), "error.engine", "got {err:?}");
    let _ = std::fs::remove_dir_all(&base);
}

/// Create a 7z SFX with `kind` and assert it is a runnable PE whose embedded
/// archive the official 7-Zip client can read.
fn assert_sfx_roundtrip(kind: SfxKind, tag: &str) {
    let base = tmp(tag); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"sfx body");
    let dest = base.join("packed.exe");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.sfx = Some(kind);
    let stats = create_archive(&srcs, &dest, &o, &mut |_| true).unwrap();

    // The product is a PE executable whose prefix is exactly the vendored stub,
    // followed by a normal .7z archive.
    let head = std::fs::read(&dest).unwrap();
    assert_eq!(&head[0..2], b"MZ", "SFX must be a PE executable");
    let stub = vendored_stub(kind);
    assert!(head.starts_with(&stub), "output must begin with the vendored stub");
    assert!(head.len() > stub.len(), "a payload must follow the stub");
    assert_eq!(stats.bytes_out, head.len() as u64, "bytes_out is the whole .exe");

    // The official client reads the embedded archive past the stub.
    let listing = list_with_7z(&dest);
    assert!(listing.contains("a.txt"), "7z must find the embedded archive, got:\n{listing}");

    // The intermediate .7z must not survive the concat.
    let leftovers = leftover_tmp(&base);
    assert!(leftovers.is_empty(), "temp archive left behind: {leftovers:?}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn creates_7z_sfx_runnable() {
    assert_sfx_roundtrip(SfxKind::Console, "sfx-con");
}

#[test]
fn creates_7z_gui_sfx_runnable() {
    assert_sfx_roundtrip(SfxKind::Gui, "sfx-gui");
}

#[test]
fn rejects_sfx_for_non_7z_formats() {
    // The stubs carry only a 7z payload; other formats must be refused rather
    // than producing an .exe the stub cannot open.
    let base = tmp("sfx-badfmt"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"x");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    for fmt in [CreateFormat::Zip, CreateFormat::Tar] {
        let dest = base.join("out.exe");
        let mut o = opts(fmt); o.sfx = Some(SfxKind::Console);
        let err = create_archive(&srcs, &dest, &o, &mut |_| true)
            .expect_err("SFX on a non-7z format must be rejected");
        assert_eq!(err.kind(), "error.engine", "{fmt:?} gave {err:?}");
        assert!(!dest.exists(), "no output may be written when SFX is refused");
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn rejects_sfx_with_volumes() {
    // A split SFX is out of scope: the stub expects one contiguous stream.
    let base = tmp("sfx-vol"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"x");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let dest = base.join("out.exe");
    let mut o = opts(CreateFormat::SevenZ); o.sfx = Some(SfxKind::Console); o.volume_bytes = Some(1_000_000);
    let err = create_archive(&srcs, &dest, &o, &mut |_| true)
        .expect_err("SFX with volumes must be rejected");
    assert_eq!(err.kind(), "error.engine", "got {err:?}");
    assert!(!dest.exists(), "no output may be written when SFX+volumes is refused");
    assert!(!base.join("out.exe.001").exists(), "no volume may be written");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn rejects_password_for_tar_formats() {
    // TAR/compressed-TAR handlers have no encryption; a password request must
    // be rejected outright rather than silently ignored.
    let base = tmp("pw-tar"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"plain");
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    for fmt in [CreateFormat::Tar, CreateFormat::TarGz, CreateFormat::TarBz2, CreateFormat::TarXz] {
        let dest = base.join("out.bin");
        let mut o = opts(fmt); o.password = Some("pw".into());
        let err = create_archive(&srcs, &dest, &o, &mut |_| true)
            .expect_err("password on a TAR format must be rejected");
        assert_eq!(err.kind(), "error.password_unsupported", "format {fmt:?} gave {err:?}");
        assert!(!dest.exists(), "no archive may be written when the password is unsupported");
    }
    let _ = std::fs::remove_dir_all(&base);
}
