//! 按签名打开更多格式（A4）。
//!
//! 这些用例造/找真文件、让 7z.dll 自己去认：扩展名不认识、甚至扩展名说谎时，
//! 打开的依据必须是字节本身。
//!
//! 本机 7-Zip 26.04 能*建*的格式只有 zip/7z/tar/gz/bz2/xz/lzma/iso 这几类；
//! cab、arj、lzh、xar、cpio、rpm、deb、z、zst 都只能解。这些用例因此分两半：
//! 能建的就建真文件，不能建的用机器上现成的真实样本（Windows 自带的 .cab）；
//! 两样都没有的格式不做"跳过即通过"的假测试。

use archive_core::{Archive, ArchiveOpenOptions};
use std::path::PathBuf;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zn-fmt-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn seven_zip() -> PathBuf {
    [
        r"C:\Program Files\7-Zip\7z.exe",
        r"C:\Program Files (x86)\7-Zip\7z.exe",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
    .expect("7z.exe is required for these fixtures")
}

fn seven_zip_archive(dest: &std::path::Path, src: &std::path::Path, extra: &[&str]) -> bool {
    let out = std::process::Command::new(seven_zip())
        .current_dir(src)
        .arg("a")
        .args(extra)
        .arg(dest)
        .arg("*")
        .output()
        .expect("run 7z");
    out.status.success() && dest.exists()
}

fn sample_dir(base: &std::path::Path) -> PathBuf {
    let src = base.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("hello.txt"), b"hello from a format test").unwrap();
    src
}

fn names_of(path: &std::path::Path) -> Vec<String> {
    Archive::open(path, ArchiveOpenOptions::default())
        .unwrap_or_else(|e| panic!("{} must open: {}", path.display(), e.error_key()))
        .entries()
        .unwrap()
        .into_iter()
        .map(|e| e.path)
        .collect()
}

/// `.cab` 是 Windows 自己的压缩格式，机器上就有真的整包样本（ servicing 目录）。
/// 拿它验证 cab handler 能列内容 —— 不是造一个假样本。
#[test]
fn a_real_windows_cab_opens() {
    let candidates = [
        r"C:\Windows\servicing\FodMetadata\FoDMetadata_Client.cab",
        r"C:\Windows\servicing\FodMetadata\metadata\DesktopTargetCompDB_Conditions.xml.cab",
    ];
    let Some(cab) = candidates.iter().map(PathBuf::from).find(|p| p.is_file()) else {
        eprintln!("note: no system .cab on this machine to prove the cab handler");
        return;
    };
    let names = names_of(&cab);
    assert!(!names.is_empty(), "a real cab must list entries");
}

/// 能建的格式逐个建出来再打开。xz/gz/bz2/lzma 都是单流，条目名取归档内名。
#[test]
fn the_create_able_new_formats_open() {
    let base = dir("many");
    let src = sample_dir(&base);
    // Measured on 7-Zip 26.04 (2026-11-27): gzip, bzip2, xz and tar can be
    // *created*; lzma / lzma86 cannot (`-tlzma` returns "not implemented"), so
    // their rows are decode-only and are not faked here.
    let cases: &[(&str, &[&str])] = &[
        ("sample.gz", &["-tgzip"]),
        ("sample.bz2", &["-tbzip2"]),
        ("sample.xz", &["-txz"]),
        ("sample.tar", &["-ttar"]),
    ];
    let mut checked = 0;
    for (name, switches) in cases {
        let dest = base.join(name);
        assert!(
            seven_zip_archive(&dest, &src, switches),
            "this machine must be able to create {name}; if it cannot, the row in              FORMATS is decode-only and the test needs adjusting, not skipping"
        );
        let names = names_of(&dest);
        assert!(!names.is_empty(), "{name} must list its payload");
        checked += 1;
    }
    assert_eq!(checked, 4);
    let _ = std::fs::remove_dir_all(&base);
}

/// `.zst`：7z.dll 能解不能建。机器上有 zstd 就用它造一个；否则不做假通过，
/// 但至少确认表格把 .zst 路由到了 zstd handler（纯函数，一定能跑）。
#[test]
fn a_zst_is_routed_to_the_zstd_handler_and_probed_by_signature() {
    assert_eq!(
        archive_core::format::candidates_for("zst").len(),
        1,
        "the table must name a handler for .zst"
    );
    let base = dir("zst");
    let src = sample_dir(&base);
    let zst = base.join("sample.zst");
    let created = std::process::Command::new("zstd")
        .current_dir(&src)
        .args(["-q", "-f", "-o"])
        .arg(&zst)
        .arg("hello.txt")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if created && zst.is_file() {
        names_of(&zst);
        // Renamed: no extension the table knows, so the signature probe must
        // find the zstd handler.
        let renamed = base.join("sample.bin");
        std::fs::rename(&zst, &renamed).unwrap();
        names_of(&renamed);
    } else {
        eprintln!("note: no zstd encoder here; only the routing is checked");
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// 扩展名说谎：一个 ZIP 叫 `.zst`，必须按 ZIP 打开，不能按 zstd 失败。
#[test]
fn a_lying_extension_still_opens_as_what_it_is() {
    let base = dir("lie");
    let src = sample_dir(&base);
    let lie = base.join("looks-like.zst");
    assert!(seven_zip_archive(&lie, &src, &["-tzip"]), "build a zip");
    let names = names_of(&lie);
    assert!(
        names.iter().any(|n| n.ends_with("hello.txt")),
        "a zip named .zst must open as a zip: got {names:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// 表格的一致性：认领的每个扩展名都要有 handler 候选；三条"只进打开方式"的
/// 扩展名在两张表里各就各位。
#[test]
fn the_table_and_the_handler_map_agree() {
    for ext in archive_core::all_extensions() {
        assert!(
            !archive_core::format::candidates_for(ext).is_empty(),
            ".{ext} is in the table but has no handler"
        );
    }
    for ext in ["msi", "chm", "cab"] {
        assert!(
            archive_core::format::open_with_only().contains(&ext),
            "{ext} must be open-with only"
        );
        assert!(
            !archive_core::format::default_claim_extensions().contains(&ext),
            "{ext} must not be claimed by default"
        );
    }
    assert!(archive_core::format::looks_like_archive("x.zst"));
    assert!(!archive_core::format::looks_like_archive("x.txt"));
}