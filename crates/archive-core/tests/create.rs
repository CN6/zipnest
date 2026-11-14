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

/// 进程级环境变量锁：`ZIPNEST_SFX_DIR` 是进程全局的，而 cargo test 默认多
/// 线程跑同一个二进制里的用例。所有会改动它的用例都先拿这把锁。
fn sfx_dir_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 设置 `ZIPNEST_SFX_DIR`，drop 时恢复原值（含「原本没设」）。
struct SfxDirEnv(Option<String>);

impl SfxDirEnv {
    fn set(dir: &std::path::Path) -> Self {
        let old = std::env::var("ZIPNEST_SFX_DIR").ok();
        std::env::set_var("ZIPNEST_SFX_DIR", dir);
        SfxDirEnv(old)
    }
}

impl Drop for SfxDirEnv {
    fn drop(&mut self) {
        match &self.0 {
            Some(v) => std::env::set_var("ZIPNEST_SFX_DIR", v),
            None => std::env::remove_var("ZIPNEST_SFX_DIR"),
        }
    }
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

/// 缺陷 A：被跳过的链接必须被记账。以前 `collect_one` 直接 `return Ok(())`，
/// 一个只装着链接的目录会走进一个空包，而引擎照样报「完成」。
#[test]
fn skipped_links_are_reported() {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    let base = tmp("skipped-links"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    write_src(&base, "src/real.txt", b"real");
    write_src(&base, "src/sub/inner.txt", b"inner");
    let link_file = base.join("src/link.txt");
    let link_dir = base.join("src/linkdir");
    // 建符号链接需要特权（SeCreateSymbolicLinkPrivilege），像 extract.rs 的
    // has_link_ancestor 测试那样在没权限的机器上跳过正向用例。
    if symlink_file("real.txt", &link_file).is_err() {
        eprintln!("note: this platform refused to create a symlink; positive case skipped");
        let _ = std::fs::remove_dir_all(&base);
        return;
    }

    let collected = archive_core::collect_sources_detailed(&[base.join("src")], &base).unwrap();
    // 链接本身不进包。
    assert!(collected.sources.iter().all(|s| s.node != "src/link.txt"));
    let file_link = collected.skipped_links.iter()
        .find(|l| l.path.ends_with("link.txt"))
        .expect("file symlink must be reported as skipped");
    assert!(!file_link.is_dir, "a file symlink is not a directory link");

    if symlink_dir("sub", &link_dir).is_ok() {
        let collected = archive_core::collect_sources_detailed(&[base.join("src")], &base).unwrap();
        assert!(collected.sources.iter().all(|s| s.node != "src/linkdir"));
        let dir_link = collected.skipped_links.iter()
            .find(|l| l.path.ends_with("linkdir"))
            .expect("directory symlink must be reported as skipped");
        assert!(dir_link.is_dir, "a directory symlink must be flagged as a directory link");
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// Junction 不需要任何特权，所以这一条在任何 Windows 账户下都会真的跑到：
/// `symlink_metadata` 把 junction 也当 symlink，整棵子树都不该进包 —— 但必须
/// 出现在 skipped 列表里，否则用户会以为子目录已经打包了。
#[test]
fn junctions_are_skipped_and_reported() {
    let base = tmp("junction"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    write_src(&base, "src/real.txt", b"real");
    write_src(&base, "elsewhere/inside.txt", b"inside");
    let made = std::process::Command::new("cmd")
        .current_dir(base.join("src"))
        // 相对路径 + 在链接的父目录里执行：绝对路径经 MSYS 转换后 mklink 会报
        // 「系统找不到指定的路径」，相对形式在两种 shell 里都稳。
        .args(["/c", "mklink", "/J", "junction", "..\\elsewhere"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !made {
        eprintln!("note: mklink /J failed; junction case skipped");
        let _ = std::fs::remove_dir_all(&base);
        return;
    }

    let collected = archive_core::collect_sources_detailed(&[base.join("src")], &base).unwrap();
    assert!(collected.sources.iter().all(|s| !s.node.starts_with("src/junction")),
        "a junction and its whole subtree must stay out of the archive, got {:?}",
        collected.sources.iter().map(|s| &s.node).collect::<Vec<_>>());
    let link = collected.skipped_links.iter()
        .find(|l| l.path.ends_with("junction"))
        .expect("the junction must be reported as skipped");
    assert!(link.is_dir, "a junction resolves to a directory");

    // 建包成功但内容为空时，调用方至少能从 skipped 列表看出「东西没进去」。
    let dest = base.join("out.zip");
    create_archive(&collected.sources, &dest, &opts(CreateFormat::Zip), &mut |_| true).unwrap();
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    let paths: Vec<String> = arc.entries().unwrap().iter().map(|e| e.path.replace('\\', "/")).collect();
    assert!(paths.iter().all(|p| !p.contains("junction")), "junction content must not be packed, got {paths:?}");
    assert!(paths.contains(&"src/real.txt".to_string()), "the real file must still be packed");
    let _ = std::fs::remove_dir_all(&base);
}

/// 既有签名保持可用：`collect_sources` 仍然只返回条目表，跳过项由新的
/// `collect_sources_detailed` 给出。两者对同一棵树给出同一份 sources。
#[test]
fn collect_sources_wrapper_matches_detailed() {
    let base = tmp("collect-compat"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    write_src(&base, "src/a.txt", b"a");
    write_src(&base, "src/sub/b.txt", b"b");
    let plain = collect_sources(&[base.join("src")], &base).unwrap();
    let detailed = archive_core::collect_sources_detailed(&[base.join("src")], &base).unwrap();
    let plain_nodes: Vec<&String> = plain.iter().map(|s| &s.node).collect();
    let detailed_nodes: Vec<&String> = detailed.sources.iter().map(|s| &s.node).collect();
    assert_eq!(plain_nodes, detailed_nodes);
    assert!(detailed.skipped_links.is_empty());
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
        assert!(w[0].0 <= w[1].0, "done_bytes must be non-decreasing, got {:?}", samples);
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

/// P0 regression: a create that fails midway must not truncate an archive that
/// already lives at `dest`, and must not leave a temp file behind. Before the
/// temp+rename fix, `File::create(dest)` zeroed the target before the first
/// byte of the new archive was known to be complete.
#[test]
fn failed_create_preserves_existing_destination_and_leaves_no_temp() {
    let base = tmp("preservedest");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.bin", &vec![7u8; 3_000_000]);
    let dest = base.join("out.7z");
    std::fs::write(&dest, b"ORIGINAL CONTENT").unwrap();

    let srcs = vec![CreateSource { path: src, node: "big.bin".into() }];
    let mut o = opts(CreateFormat::SevenZ);
    o.level = CompressionLevel::Normal;
    let err = create_archive(&srcs, &dest, &o, &mut |_| false).expect_err("cancel must abort");
    assert_eq!(err.error_key(), "error.cancelled", "got {err:?}");

    assert_eq!(
        std::fs::read_to_string(&dest).unwrap(),
        "ORIGINAL CONTENT",
        "a failed create must not truncate the pre-existing destination"
    );
    let leftovers = leftover_tmp(&base);
    assert!(leftovers.is_empty(), "temp file left behind after failed create: {leftovers:?}");
    let _ = std::fs::remove_dir_all(&base);
}

/// The success path still commits: the archive lands at `dest` and no temp
/// sibling survives the rename.
#[test]
fn successful_create_commits_over_existing_destination() {
    let base = tmp("commitdest");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"fresh payload");
    let dest = base.join("out.zip");
    std::fs::write(&dest, b"STALE").unwrap();
    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let stats = create_archive(&srcs, &dest, &opts(CreateFormat::Zip), &mut |_| true).unwrap();
    assert_eq!(stats.bytes_out, std::fs::metadata(&dest).unwrap().len());
    let arc = Archive::open(&dest, ArchiveOpenOptions::default()).unwrap();
    assert_eq!(arc.entries().unwrap()[0].path, "a.txt");
    let leftovers = leftover_tmp(&base);
    assert!(leftovers.is_empty(), "temp file left behind after successful create: {leftovers:?}");
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

/// 卷序列的现状：从 `.001` 起连续存在的那些文件的路径（升序）。
fn volume_series(dest: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for i in 1u32.. {
        let p = PathBuf::from(format!("{}.{:03}", dest.display(), i));
        if !p.exists() { break; }
        out.push(p);
    }
    out
}

/// 目录里所有形如 `<name>.old-<tag>.<NNN>` 的备份残留。
fn leftover_backups(dir: &std::path::Path, name: &str) -> Vec<String> {
    std::fs::read_dir(dir).unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(&format!("{name}.old-")))
        .collect()
}

/// 拼出一套可读的分卷归档并验证内容：`vols` 必须齐全，合起来能打开。
fn assert_volume_set_readable(base: &std::path::Path, dest: &std::path::Path, expect_len: u64) {
    let vols = volume_series(dest);
    assert!(!vols.is_empty(), "expected a volume series at {}", dest.display());
    let joined = base.join("joined-check.7z");
    std::fs::write(&joined, {
        let mut bytes = Vec::new();
        for v in &vols { bytes.extend(std::fs::read(v).unwrap()); }
        bytes
    }).unwrap();
    let arc = Archive::open(&joined, ArchiveOpenOptions::default()).unwrap();
    let e = arc.entries().unwrap().into_iter().find(|e| e.path.ends_with("big.bin")).expect("big.bin present");
    assert_eq!(arc.read_entry(e.index, &ArchiveOpenOptions::default(), None).unwrap().len() as u64, expect_len);
    let _ = std::fs::remove_file(&joined);
}

/// 刑部实测之一：第二次分卷压缩中途取消，旧的卷序列必须仍然完整可读。
///
/// 上一版在写任何字节之前就 `clear_volumes(dest)`，取消后目录里只剩空目录 ——
/// 用户原来的归档没了，新的也没有。现在暂存卷写在 `<dest>.zipnest-part-*`
/// 上，取消只会让守卫删掉暂存卷。
#[test]
fn cancelling_a_second_split_keeps_the_previous_volumes_readable() {
    let base = tmp("volcancel-keep"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.bin", &vec![0x5Au8; 2_500_000]);
    let dest = base.join("vol.7z");
    let srcs = vec![CreateSource { path: src.clone(), node: "big.bin".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.volume_bytes = Some(1_000_000);

    let first = create_archive(&srcs, &dest, &o, &mut |_| true).expect("first split must succeed");
    let before = volume_series(&dest);
    assert!(before.len() >= 3, "sanity: the first run wrote a volume series");

    let err = create_archive(&srcs, &dest, &o, &mut |_| false)
        .expect_err("a cancelled second split must fail");
    assert_eq!(err.error_key(), "error.cancelled", "got {err:?}");

    // 旧序列一个不少、内容能打开。
    assert_eq!(volume_series(&dest), before, "the previous volume series must survive a cancel");
    assert_volume_set_readable(&base, &dest, 2_500_000);
    // 暂存卷和备份都不该留。
    let strays = leftover_tmp(&base);
    assert!(strays.is_empty(), "staged volumes left behind after cancel: {strays:?}");
    assert!(leftover_backups(&base, "vol.7z").is_empty(), "no backup may be created on a cancel");
    // 第一次运行报告的输出字节数，应当等于磁盘上这些卷的实际总大小
    // （finalize 会把每一卷截到逻辑长度）。这里原本留着一行
    // `… .len() * 0 + first.bytes_out` 的调试残渣，恒真，clippy 的
    // `erasing_op` 会直接判红。
    let on_disk: u64 = before.iter().map(|p| std::fs::metadata(p).unwrap().len()).sum();
    assert_eq!(first.bytes_out, on_disk, "bytes_out must match the series on disk");
    let _ = std::fs::remove_dir_all(&base);
}

/// 刑部实测之二：旧序列里某卷被别的进程以独占句柄占住（`share_mode(0)`）。
///
/// 改名会因共享冲突失败 ⇒ 整体失败并回滚：**旧卷必须一个不少、内容不变**，
/// 不能出现「.001 已让位成备份、.002 失败」这类半套状态。暂存卷也不能留下。
#[test]
fn a_locked_previous_volume_aborts_the_replace_and_keeps_the_old_set() {
    use std::os::windows::fs::OpenOptionsExt;
    let base = tmp("vollocked"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.bin", &vec![0x5Au8; 2_500_000]);
    let dest = base.join("vol.7z");
    let srcs = vec![CreateSource { path: src, node: "big.bin".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.volume_bytes = Some(1_000_000);

    create_archive(&srcs, &dest, &o, &mut |_| true).expect("first split must succeed");
    let before = volume_series(&dest);
    assert!(before.len() >= 2, "sanity: at least two volumes");
    let snapshot: Vec<Vec<u8>> = before.iter().map(|p| std::fs::read(p).unwrap()).collect();

    // 占住第二卷：改名它必然失败（share_mode(0) 不给别人 DELETE / 改名的余地），
    // 而第一卷此前已经成功让位 —— 正好考验回滚。
    let locked = std::fs::OpenOptions::new().read(true).share_mode(0)
        .open(&before[1]).expect("locking the second volume must succeed");

    let err = create_archive(&srcs, &dest, &o, &mut |_| true)
        .expect_err("the replace must fail while a previous volume is locked");
    assert_eq!(err.error_key(), "error.io", "got {err:?}");

    drop(locked);
    // 旧序列完整、逐字节未变。
    assert_eq!(volume_series(&dest), before, "every previous volume must come back");
    for (path, expected) in before.iter().zip(&snapshot) {
        assert_eq!(&std::fs::read(path).unwrap(), expected, "{} must be byte-identical", path.display());
    }
    assert_volume_set_readable(&base, &dest, 2_500_000);
    // 备份全部改回去了，暂存卷被守卫删掉。
    assert!(leftover_backups(&base, "vol.7z").is_empty(),
        "rollback must move every backup back: {:?}", leftover_backups(&base, "vol.7z"));
    let strays = leftover_tmp(&base);
    assert!(strays.is_empty(), "staged volumes left behind after a failed replace: {strays:?}");
    let _ = std::fs::remove_dir_all(&base);
}

/// 刑部实测之三：连续两次成功覆盖后，目录里只有新一代卷 —— 没有 `.old-*`
/// 残留，也没有第一次多出来的高编号旧卷。
#[test]
fn two_successful_overwrites_leave_only_the_new_series() {
    let base = tmp("voltwoclean"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    // 第二次的输入更小，卷数会变少：多出来的高编号旧卷必须消失。
    let big = write_src(&base, "big.bin", &vec![0x5Au8; 2_500_000]);
    let small = write_src(&base, "small.bin", &vec![0x11u8; 1_200_000]);
    let dest = base.join("vol.7z");
    let mut o = opts(CreateFormat::SevenZ); o.volume_bytes = Some(1_000_000);

    let big_srcs = vec![CreateSource { path: big, node: "big.bin".into() }];
    create_archive(&big_srcs, &dest, &o, &mut |_| true).expect("first split must succeed");
    let first_vols = volume_series(&dest);
    assert!(first_vols.len() >= 3, "sanity: the first run splits into >=3 volumes");

    let small_srcs = vec![CreateSource { path: small, node: "small.bin".into() }];
    create_archive(&small_srcs, &dest, &o, &mut |_| true).expect("second split must succeed");
    let second_vols = volume_series(&dest);
    assert!(second_vols.len() < first_vols.len(),
        "the smaller input must yield fewer volumes: first {}, second {}",
        first_vols.len(), second_vols.len());

    // 目录里除了新一代卷，不该再有别的属于这个归档的东西。
    assert!(leftover_backups(&base, "vol.7z").is_empty(),
        "backups must be deleted after success: {:?}", leftover_backups(&base, "vol.7z"));
    let strays = leftover_tmp(&base);
    assert!(strays.is_empty(), "staged volumes left behind after success: {strays:?}");
    let names: Vec<String> = std::fs::read_dir(&base).unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("vol.7z"))
        .collect();
    assert_eq!(names.len(), second_vols.len(),
        "only the new series may remain beside the destination, got {names:?}");

    // 新内容确实是第二次打包的那个文件。
    let joined = base.join("joined-new.7z");
    let mut bytes = Vec::new();
    for v in &second_vols { bytes.extend(std::fs::read(v).unwrap()); }
    std::fs::write(&joined, &bytes).unwrap();
    let arc = Archive::open(&joined, ArchiveOpenOptions::default()).unwrap();
    let names: Vec<String> = arc.entries().unwrap().iter().map(|e| e.path.replace('\\', "/")).collect();
    assert!(names.iter().any(|p| p.ends_with("small.bin")), "got {names:?}");
    let _ = std::fs::remove_dir_all(&base);
}

/// 缺陷 B：同一路径分卷两次都必须成功。`ensure` 用 `create_new`，以前第二次
/// 必定撞上第一次留下的 `.001` 而失败（UI 上只看到「操作失败」）。
#[test]
fn splitting_twice_to_the_same_path_succeeds() {
    let base = tmp("voltwice"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.bin", &vec![0x5Au8; 2_500_000]);
    let dest = base.join("vol.7z");
    let srcs = vec![CreateSource { path: src, node: "big.bin".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.volume_bytes = Some(1_000_000);
    let first = create_archive(&srcs, &dest, &o, &mut |_| true).expect("first split must succeed");
    let vols_after_first: Vec<PathBuf> = join_volumes(&dest, &base.join("ignored.7z"));
    assert!(vols_after_first.len() >= 3, "sanity: the first run wrote a volume series");

    let second = create_archive(&srcs, &dest, &o, &mut |_| true)
        .expect("splitting to the same path a second time must overwrite, not fail");
    assert_eq!(first.bytes_out, second.bytes_out, "same input must give the same total size");

    // 旧的卷序列必须被完整替换：不留下第一次多出来的高编号卷。
    let vols_after_second = join_volumes(&dest, &base.join("ignored2.7z"));
    assert_eq!(vols_after_first.len(), vols_after_second.len(),
        "stale trailing volumes from the first run must not survive");
    // 结果仍然是一个能读的分卷归档。
    let joined = base.join("joined.7z");
    join_volumes(&dest, &joined);
    let arc = Archive::open(&joined, ArchiveOpenOptions::default()).unwrap();
    let e = arc.entries().unwrap().into_iter().find(|e| e.path.ends_with("big.bin")).expect("big.bin present");
    assert_eq!(arc.read_entry(e.index, &ArchiveOpenOptions::default(), None).unwrap().len(), 2_500_000);
    let _ = std::fs::remove_dir_all(&base);
}

/// 覆盖旧卷只允许删「<dest>.<3 位以上纯数字>」这一组的连续序列。邻近的、
/// 编号不连续的文件，以及同名目录，都不属于我们，必须原样留下。
#[test]
fn clearing_stale_volumes_leaves_unrelated_files_alone() {
    let base = tmp("volclear"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let dest = base.join("vol.7z");
    let vol = |i: &str| base.join(format!("vol.7z.{i}"));
    // 我们自己的序列：.001、.002 连续，.004 起断开（第 3 卷本来就没有）。
    for i in ["002", "001", "004"] { std::fs::write(vol(i), b"stale").unwrap(); }
    // 不是我们的东西：别的名字、非数字后缀、两位数字、同名目录。
    std::fs::write(base.join("other.7z.001"), b"keep").unwrap();
    std::fs::write(base.join("vol.7z.bak"), b"keep").unwrap();
    std::fs::write(base.join("vol.7z.01"), b"keep").unwrap();
    std::fs::create_dir_all(vol("005")).unwrap();
    // 大小写不同的同一序列（Windows 上文件名不区分大小写，这里建的是同一份
    // 文件的另一种拼写，必须在扫描里被认出来）。
    std::fs::write(base.join("VOL.7Z.003"), b"stale").unwrap();

    archive_core::com::outstream::clear_volumes(&dest).unwrap();

    assert!(!vol("001").exists(), ".001 must be cleared");
    assert!(!vol("002").exists(), ".002 must be cleared");
    assert!(!vol("003").exists(), "a differently-cased .003 must be cleared");
    // 扫描按连续编号走，但 .003 存在时 .004 也在连续段内，所以它也该被清掉。
    assert!(!vol("004").exists(), ".004 belongs to the contiguous run and must be cleared");
    assert!(vol("005").is_dir(), "a directory is never deleted");
    assert!(base.join("other.7z.001").exists(), "a different name must survive");
    assert!(base.join("vol.7z.bak").exists(), "a non-numeric suffix must survive");
    assert!(base.join("vol.7z.01").exists(), "fewer than 3 digits is not our volume");
    let _ = std::fs::remove_dir_all(&base);
}

/// 相对路径的 dest 不参与「清旧卷」：含义不明，宁可返回错误也不猜。
#[test]
fn clearing_volumes_refuses_relative_destinations() {
    let err = archive_core::com::outstream::clear_volumes(std::path::Path::new("vol.7z"))
        .expect_err("a relative destination must be refused");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
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

/// 取消一个**首次**分卷：一个用户可见的卷都不该出现（暂存卷也不行）。
///
/// 这里的断言以前是「取消后 `.001`–`.004` 一个都不剩」，在旧实现下等同于
/// 「删掉用户的旧归档」—— 那条断言正是刑部封还的缺陷本身。现在这套断言只
/// 针对「该 dest 本来就没有旧归档」的情形：取消之后目录必须和取消之前一样
/// 干净。「有旧归档时取消必须保留旧归档」由
/// `cancelling_a_second_split_keeps_the_previous_volumes_readable` 负责。
#[test]
fn cancelling_a_first_split_leaves_no_partial_volumes() {
    let base = tmp("volcancel"); let _ = std::fs::remove_dir_all(&base); std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "big.bin", &vec![0x5Au8; 2_500_000]);
    let dest = base.join("vol.7z");
    let srcs = vec![CreateSource { path: src, node: "big.bin".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.volume_bytes = Some(1_000_000);
    let err = create_archive(&srcs, &dest, &o, &mut |_| false)
        .expect_err("cancelled create must fail");
    assert_eq!(err.kind(), "error.cancelled", "got {err:?}");
    assert!(volume_series(&dest).is_empty(),
        "no user-visible volume may appear when a first split is cancelled: {:?}",
        volume_series(&dest));
    let strays = leftover_tmp(&base);
    assert!(strays.is_empty(), "staged volumes left behind after cancel: {strays:?}");
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

/// 缺陷 C：SFX 中途失败时，用户原有的 `dest` 必须原样留在原地。
///
/// 以前 `concat_files` 先 `File::create(dest)` 截断目标，失败时再
/// `remove_file(dest)`：用户在覆盖确认里答应的「替换」，实际结果是原文件和新
/// 文件都没有。现在载荷先写临时文件、成功才 rename。
///
/// 失败点选在拼接的第一步：把 `ZIPNEST_SFX_DIR` 指向一个 stub 被独占锁住的
/// 目录，`concat_files` 读 stub 时得到共享冲突。旧实现这时 `dest` 已经被截断、
/// 随后被删掉；新实现下 `dest` 完全没被碰过。
#[test]
fn failed_sfx_preserves_existing_destination() {
    use std::os::windows::fs::OpenOptionsExt;
    let _serial = sfx_dir_lock();
    let base = tmp("sfxfail");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let src = write_src(&base, "a.txt", b"sfx body");
    let dest = base.join("packed.exe");
    std::fs::write(&dest, b"ORIGINAL EXE").unwrap();

    // 一个完整但被锁住的 stub：查找会成功（`is_file` 不受共享模式影响），
    // 真正读取时才失败。
    let stub_dir = base.join("sfx-stub");
    std::fs::create_dir_all(&stub_dir).unwrap();
    std::fs::write(stub_dir.join("7zCon.sfx"), vendored_stub(SfxKind::Console)).unwrap();
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(stub_dir.join("7zCon.sfx"))
        .expect("locking the stub copy must succeed");
    let _env = SfxDirEnv::set(&stub_dir);

    let srcs = vec![CreateSource { path: src, node: "a.txt".into() }];
    let mut o = opts(CreateFormat::SevenZ); o.sfx = Some(SfxKind::Console);
    let err = create_archive(&srcs, &dest, &o, &mut |_| true)
        .expect_err("an unreadable stub must abort the SFX");
    assert_eq!(err.kind(), "error.io", "got {err:?}");

    assert_eq!(
        std::fs::read(&dest).unwrap(),
        b"ORIGINAL EXE",
        "a failed SFX must leave the pre-existing destination untouched"
    );
    let leftovers = leftover_tmp(&base);
    assert!(leftovers.is_empty(), "temp file left behind after failed SFX: {leftovers:?}");
    drop(locked);
    let _ = std::fs::remove_dir_all(&base);
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
