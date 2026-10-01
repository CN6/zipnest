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
