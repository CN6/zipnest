use archive_core::{create::{collect_sources, create_archive, CreateSource}, Archive, ArchiveOpenOptions, types::{CreateFormat, CompressionLevel, CompressionMethod, CreateOptions}};
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf { std::env::temp_dir().join(format!("zn-create-{}-{}", std::process::id(), name)) }

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
    let _ = std::fs::remove_dir_all(&base);
}
