use archive_core::{create::{create_archive, CreateSource}, Archive, ArchiveOpenOptions, types::{CreateFormat, CompressionLevel, CompressionMethod, CreateOptions}};
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
