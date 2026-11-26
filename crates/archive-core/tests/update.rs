//! 向已有 ZIP / 7z 追加与删除。
//!
//! 每条用例都从磁盘上的真实归档出发：先建一个包，再用 `update_archive`
//! 改它，然后重新打开确认内容。断言的是**最终磁盘上的那个文件**，不是我们
//! 自己回报的数字 —— 一个报告"完成"而磁盘上旧的没了的实现，在这里过不去。

use archive_core::types::{
    CompressionLevel, CompressionMethod, CreateFormat, CreateOptions, CreateSource,
};
use archive_core::update::{update_archive, update_format_for, UpdateFormat, UpdateOptions, UpdatePlan};
use archive_core::{Archive, ArchiveOpenOptions};
use std::path::{Path, PathBuf};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zn-update-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn opts(fmt: CreateFormat) -> CreateOptions {
    CreateOptions {
        format: fmt,
        level: CompressionLevel::Store,
        method: CompressionMethod::Copy,
        password: None,
        encrypt_names: false,
        volume_bytes: None,
        sfx: None,
    }
}

fn make_archive(dir: &Path, name: &str, fmt: CreateFormat, entries: &[(&str, &[u8])]) -> PathBuf {
    let mut sources = Vec::new();
    for (node, body) in entries {
        let p = dir.join("src").join(node);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        sources.push(CreateSource {
            path: p,
            node: (*node).to_string(),
        });
    }
    let dest = dir.join(name);
    archive_core::create_archive(&sources, &dest, &opts(fmt), &mut |_| true).expect("create");
    dest
}

fn listing(path: &Path, password: Option<&str>) -> Vec<(String, Vec<u8>)> {
    // `ArchiveOpenOptions` is not `Clone` (it deliberately has no derived
    // traits beyond Default), so each call builds its own.
    let open = || ArchiveOpenOptions {
        password: password.map(str::to_string),
    };
    let arc = Archive::open(path, open()).expect("reopen");
    let entries = arc.entries().expect("list");
    let mut out = Vec::new();
    for e in entries.iter().filter(|e| !e.is_dir) {
        let bytes = arc.read_entry(e.index, &open(), None).expect("read entry");
        out.push((e.path.clone(), bytes));
    }
    out.sort();
    out
}

fn find_index(path: &Path, name: &str) -> u32 {
    let arc = Archive::open(path, ArchiveOpenOptions::default()).unwrap();
    arc.entries()
        .unwrap()
        .iter()
        .find(|e| e.path == name)
        .unwrap_or_else(|| panic!("no entry {name}"))
        .index
}

fn update_opts(fmt: UpdateFormat) -> UpdateOptions {
    UpdateOptions {
        format: fmt,
        level: Some(CompressionLevel::Normal),
        method: Some(CompressionMethod::Auto),
        password: None,
    }
}

fn no_temp_left(dir: &Path) {
    let leftovers: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("zipnest-part"))
        .collect();
    assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
}

#[test]
fn adding_a_file_keeps_the_old_entries_and_writes_the_new_one() {
    let dir = tmp("add");
    let archive = make_archive(
        &dir,
        "a.zip",
        CreateFormat::Zip,
        &[("one.txt", b"first"), ("dir/two.txt", b"second")],
    );
    let new_file = dir.join("added.txt");
    std::fs::write(&new_file, b"added content").unwrap();

    let arc = Archive::open(&archive, ArchiveOpenOptions::default()).unwrap();
    let plan = UpdatePlan {
        remove: Vec::new(),
        add: vec![CreateSource {
            path: new_file,
            node: "added.txt".into(),
        }],
    };
    let stats = update_archive(&arc, &archive, &plan, &update_opts(UpdateFormat::Zip), &mut |_| true)
        .expect("update");
    drop(arc);
    assert_eq!(stats.added, 1);
    assert_eq!(stats.kept, 2);
    assert_eq!(stats.removed, 0);

    // Compare paths with separators normalized: the handler stores the names
    // exactly as the writer of the *original* archive spelled them, and 7-Zip's
    // own client writes `dir\two.txt`. The content is what this test is about.
    let normalized: Vec<(String, Vec<u8>)> = listing(&archive, None)
        .into_iter()
        .map(|(p, b)| (p.replace('\\', "/"), b))
        .collect();
    assert_eq!(normalized.len(), 3, "got {normalized:?}");
    assert!(normalized.contains(&("added.txt".to_string(), b"added content".to_vec())));
    assert!(normalized.contains(&("dir/two.txt".to_string(), b"second".to_vec())));
    assert!(normalized.contains(&("one.txt".to_string(), b"first".to_vec())));
    no_temp_left(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn removing_an_entry_drops_exactly_that_entry() {
    let dir = tmp("remove");
    let archive = make_archive(
        &dir,
        "a.zip",
        CreateFormat::Zip,
        &[("keep.txt", b"keep me"), ("drop.txt", b"drop me")],
    );
    let drop_index = find_index(&archive, "drop.txt");

    let arc = Archive::open(&archive, ArchiveOpenOptions::default()).unwrap();
    let plan = UpdatePlan {
        remove: vec![drop_index],
        add: Vec::new(),
    };
    let stats = update_archive(&arc, &archive, &plan, &update_opts(UpdateFormat::Zip), &mut |_| true)
        .expect("update");
    drop(arc);
    assert_eq!(stats.removed, 1);
    assert_eq!(stats.kept, 1);
    let got = listing(&archive, None);
    assert_eq!(got, vec![("keep.txt".to_string(), b"keep me".to_vec())]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_same_edit_works_on_a_seven_z() {
    let dir = tmp("seven");
    let archive = make_archive(
        &dir,
        "a.7z",
        CreateFormat::SevenZ,
        &[("keep.txt", b"keep"), ("drop.txt", b"drop")],
    );
    let drop_index = find_index(&archive, "drop.txt");
    let added = dir.join("new.txt");
    std::fs::write(&added, b"brand new").unwrap();
    let arc = Archive::open(&archive, ArchiveOpenOptions::default()).unwrap();
    let plan = UpdatePlan {
        remove: vec![drop_index],
        add: vec![CreateSource {
            path: added,
            node: "new.txt".into(),
        }],
    };
    let stats = update_archive(
        &arc,
        &archive,
        &plan,
        &update_opts(UpdateFormat::SevenZ),
        &mut |_| true,
    )
    .expect("update 7z");
    drop(arc);
    assert_eq!((stats.kept, stats.added, stats.removed), (1, 1, 1));
    let got = listing(&archive, None);
    assert_eq!(
        got,
        vec![
            ("keep.txt".to_string(), b"keep".to_vec()),
            ("new.txt".to_string(), b"brand new".to_vec()),
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cancelled_update_leaves_the_original_untouched() {
    let dir = tmp("cancel");
    let archive = make_archive(&dir, "a.zip", CreateFormat::Zip, &[("a.txt", b"original")]);
    let before = std::fs::read(&archive).unwrap();
    let added = dir.join("b.txt");
    std::fs::write(&added, [0u8; 200_000]).unwrap();
    let arc = Archive::open(&archive, ArchiveOpenOptions::default()).unwrap();
    let plan = UpdatePlan {
        remove: Vec::new(),
        add: vec![CreateSource {
            path: added,
            node: "b.txt".into(),
        }],
    };
    let err = update_archive(&arc, &archive, &plan, &update_opts(UpdateFormat::Zip), &mut |_| false)
        .expect_err("cancel must surface");
    drop(arc);
    assert_eq!(err.error_key(), "error.cancelled");
    assert_eq!(std::fs::read(&archive).unwrap(), before, "the original must not change");
    no_temp_left(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_encrypted_archive_stays_encrypted_and_readable() {
    let dir = tmp("enc");
    let src = dir.join("secret.txt");
    std::fs::write(&src, b"classified").unwrap();
    let archive = dir.join("e.7z");
    let mut o = opts(CreateFormat::SevenZ);
    o.password = Some("hunter2".into());
    archive_core::create_archive(
        &[CreateSource {
            path: src,
            node: "secret.txt".into(),
        }],
        &archive,
        &o,
        &mut |_| true,
    )
    .unwrap();

    let added = dir.join("more.txt");
    std::fs::write(&added, b"added under the same key").unwrap();
    let open = ArchiveOpenOptions {
        password: Some("hunter2".into()),
    };
    let arc = Archive::open(&archive, open).unwrap();
    let mut uo = update_opts(UpdateFormat::SevenZ);
    uo.password = Some("hunter2".into());
    let plan = UpdatePlan {
        remove: Vec::new(),
        add: vec![CreateSource {
            path: added,
            node: "more.txt".into(),
        }],
    };
    let stats = update_archive(&arc, &archive, &plan, &uo, &mut |_| true).expect("encrypted update");
    drop(arc);
    assert_eq!(stats.added, 1);
    // The names stay readable (this archive was created without header
    // encryption, `he`, which is the default), so "opening without a password"
    // succeeding is correct. What must NOT happen is the *contents* becoming
    // readable: every entry must still be flagged encrypted, and reading one
    // without the key must fail.
    let no_pw = Archive::open(&archive, ArchiveOpenOptions::default())
        .expect("names are not encrypted in this archive, so listing must work");
    let entries = no_pw.entries().unwrap();
    assert!(
        entries.iter().all(|e| e.encrypted),
        "an update must not strip the per-entry encryption flag: {entries:?}"
    );
    let secret = entries.iter().find(|e| e.path == "secret.txt").unwrap();
    let read = no_pw.read_entry(secret.index, &ArchiveOpenOptions::default(), None);
    assert!(
        read.is_err(),
        "the payload must still require the password after an update"
    );
    let got = listing(&archive, Some("hunter2"));
    assert_eq!(
        got,
        vec![
            ("more.txt".to_string(), b"added under the same key".to_vec()),
            ("secret.txt".to_string(), b"classified".to_vec()),
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_rewritten_zip_opens_with_the_official_client() {
    let exe = [r"C:\Program Files\7-Zip\7z.exe", r"C:\Program Files (x86)\7-Zip\7z.exe"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .expect("7z.exe");
    let dir = tmp("compat");
    let archive = make_archive(&dir, "c.zip", CreateFormat::Zip, &[("a.txt", b"hi")]);
    let added = dir.join("b.txt");
    std::fs::write(&added, b"there").unwrap();
    let arc = Archive::open(&archive, ArchiveOpenOptions::default()).unwrap();
    let plan = UpdatePlan {
        remove: Vec::new(),
        add: vec![CreateSource {
            path: added,
            node: "b.txt".into(),
        }],
    };
    update_archive(&arc, &archive, &plan, &update_opts(UpdateFormat::Zip), &mut |_| true).unwrap();
    drop(arc);
    let out = std::process::Command::new(&exe)
        .arg("l")
        .arg(&archive)
        .output()
        .expect("run 7z");
    assert!(out.status.success(), "7z refused the rewritten archive");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("a.txt") && text.contains("b.txt"), "listing: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_plan_is_refused() {
    let dir = tmp("empty");
    let archive = make_archive(&dir, "a.zip", CreateFormat::Zip, &[("a.txt", b"x")]);
    let arc = Archive::open(&archive, ArchiveOpenOptions::default()).unwrap();
    let err = update_archive(
        &arc,
        &archive,
        &UpdatePlan::default(),
        &update_opts(UpdateFormat::Zip),
        &mut |_| true,
    )
    .expect_err("empty plan");
    assert_eq!(err.error_key(), "error.engine");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_zip_and_seven_z_are_editable() {
    assert_eq!(update_format_for("zip"), Some(UpdateFormat::Zip));
    assert_eq!(update_format_for("ZIP"), Some(UpdateFormat::Zip));
    assert_eq!(update_format_for("7z"), Some(UpdateFormat::SevenZ));
    for other in ["rar", "tar", "gz", "bz2", "xz", "iso", "tar.gz", ""] {
        assert_eq!(update_format_for(other), None, "{other} must not be editable");
    }
}