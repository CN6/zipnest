use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn fx(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

type Events = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

fn service() -> (zipnest_ipc::IpcService, Events) {
    let events: Events = Arc::new(Mutex::new(Vec::new()));
    let e2 = events.clone();
    let svc = zipnest_ipc::IpcService::new(
        Arc::new(move |name: &str, v: serde_json::Value| {
            e2.lock().unwrap().push((name.to_string(), v));
        }),
        std::time::Duration::from_millis(0),
    );
    (svc, events)
}

fn wait_events(events: &Events, ms: u64, mut pred: impl FnMut(&[(String, serde_json::Value)]) -> bool) {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_millis(ms) {
        if pred(&events.lock().unwrap()) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(pred(&events.lock().unwrap()), "events not seen within {ms}ms");
}

#[test]
fn open_returns_root_entries_only() {
    let (svc, _) = service();
    let r = svc
        .open_archive(fx("plain.zip").to_string_lossy().into(), None)
        .unwrap();
    assert!(!r.encrypted);
    assert_eq!(r.format, "zip");
    let names: Vec<_> = r.entries.iter().map(|e| e.path.as_str()).collect();
    assert!(names.contains(&"a.txt"));
    assert!(names.contains(&"c.txt"));
    assert!(names.iter().any(|n| *n == "sub" || *n == "sub/"));
    assert!(!names.iter().any(|n| n.contains("b.txt")));
}

#[test]
fn list_children_returns_dir_children() {
    let (svc, _) = service();
    let r = svc
        .open_archive(fx("plain.zip").to_string_lossy().into(), None)
        .unwrap();
    let kids = svc.list_children(r.id, "sub/".into()).unwrap();
    assert_eq!(kids.len(), 1);
    assert!(kids[0].path.ends_with("b.txt"));
}

/// 列目录和搜索必须能在归档锁被 worker 线程一直握着的时候照常返回。
///
/// 回归场景（已核实的缺陷）：extract 的 worker 从 `shared.lock()` 起一直握着
/// 归档的 `Mutex` 到解压结束，从前 `list_children` / `search_entries` 在同一把
/// 锁上同步阻塞，而 UI 线程在 navigate / run_search 里直接调用它们 —— 解压运行中
/// 翻一个目录，窗口就整块冻住，连取消按钮都点不到。现在两条路径都只读打开时缓存
/// 的条目列表，完全不碰那把锁。
#[test]
fn listing_and_search_do_not_wait_for_the_archive_lock() {
    let svc = std::sync::Arc::new(zipnest_ipc::IpcService::with_settings_path(
        Arc::new(|_: &str, _: serde_json::Value| {}),
        std::time::Duration::ZERO,
        std::env::temp_dir().join(format!("zipnest-lock-{}.json", std::process::id())),
    ));
    let r = svc
        .open_archive(fx("plain.zip").to_string_lossy().into(), None)
        .unwrap();

    // 让另一个线程把归档锁整段握住，模拟正在解压的 worker。握住之前、放下之前
    // 都用一次握手确认，测试才不会在"还没锁上"的情况下侥幸通过。
    let shared = svc.registry.get(r.id).expect("the archive must be registered");
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = shared.lock().unwrap_or_else(|e| e.into_inner());
        let _ = held_tx.send(());
        let _ = release_rx.recv();
    });
    held_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the helper thread never took the lock");

    // 锁确实被别的线程握着。这两次调用必须立刻返回：若实现退回成"列目录要取
    // 归档锁"，它们会超时失败，而不是把整个测试挂死。
    let (tx, rx) = std::sync::mpsc::channel();
    let caller = std::sync::Arc::clone(&svc);
    std::thread::spawn(move || {
        let kids = caller.list_children(r.id, "sub/".into());
        let hits = caller.search_entries(r.id, "b.txt".into(), 100);
        let _ = tx.send((kids, hits));
    });
    let got = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("list_children / search_entries blocked on the archive lock");
    let kids = got.0.expect("listing must succeed while a job holds the lock");
    assert!(kids.iter().any(|e| e.path.ends_with("b.txt")));
    let hits = got.1.expect("search must succeed while a job holds the lock");
    assert!(hits.iter().any(|e| e.path.ends_with("b.txt")), "got {hits:?}");

    let _ = release_tx.send(());
    let _ = holder.join();
}

/// 关闭归档必须把打开时缓存的条目列表一并丢掉，否则一个已关闭的归档的全部条目
/// 会按 id 永远留在进程里 —— 又一个泄漏，只是这次漏的是条目而不是句柄。
#[test]
fn closing_an_archive_drops_its_cached_entry_list() {
    let (svc, _) = service();
    let r = svc
        .open_archive(fx("plain.zip").to_string_lossy().into(), None)
        .unwrap();
    assert!(svc.registry.entries(r.id).is_some(), "open must cache the list");

    svc.close_archive(r.id);
    assert!(
        svc.registry.entries(r.id).is_none(),
        "closed archive must not keep its entry list"
    );
    assert_eq!(svc.open_archive_count(), 0);
}

#[test]
fn closing_an_archive_really_releases_it() {
    // The UI closes the archive it is replacing; without that, every archive a
    // session opened stayed alive (7z handler + file handle + password).
    let (svc, _) = service();
    let first = svc.open_archive(fx("plain.zip").to_string_lossy().into(), None).unwrap();
    let second = svc.open_archive(fx("plain.zip").to_string_lossy().into(), None).unwrap();
    assert_ne!(first.id, second.id, "each open gets its own id");

    svc.close_archive(first.id);
    // The closed id is gone: no listing, no read.
    assert!(svc.list_children(first.id, String::new()).is_err());
    assert!(svc.read_entry_bytes(first.id, "a.txt".into(), 1024).is_err());
    // ... while the other archive is untouched.
    assert!(!svc.list_children(second.id, String::new()).unwrap().is_empty());

    // Closing twice, or an id that never existed, is a harmless no-op.
    svc.close_archive(first.id);
    svc.close_archive(9_999);
}

#[test]
fn the_registry_does_not_grow_without_bound() {
    // Regression guard for the leak: open/close in a loop must not accumulate
    // archives, while two live opens must both be held.
    let svc = zipnest_ipc::IpcService::with_settings_path(
        Arc::new(|_: &str, _: serde_json::Value| {}),
        std::time::Duration::ZERO,
        std::env::temp_dir().join("zipnest-registry-len.json"),
    );
    for _ in 0..5 {
        let r = svc.open_archive(fx("plain.zip").to_string_lossy().into(), None).unwrap();
        svc.close_archive(r.id);
    }
    assert_eq!(svc.open_archive_count(), 0, "closed archives must not linger");

    let a = svc.open_archive(fx("plain.zip").to_string_lossy().into(), None).unwrap();
    let b = svc.open_archive(fx("nested.zip").to_string_lossy().into(), None).unwrap();
    assert_eq!(svc.open_archive_count(), 2);
    svc.close_archive(a.id);
    assert_eq!(svc.open_archive_count(), 1);
    svc.close_archive(b.id);
    assert_eq!(svc.open_archive_count(), 0);
}

#[test]
fn read_entry_bytes_roundtrip() {
    let (svc, _) = service();
    let r = svc
        .open_archive(fx("nested.zip").to_string_lossy().into(), None)
        .unwrap();
    let kids = svc.list_children(r.id, "sub/".into()).unwrap();
    let got = svc.read_entry_bytes(r.id, kids[0].path.clone(), 1024).unwrap();
    assert_eq!(got, b"nested content");
}

#[test]
fn open_encrypted_7z_needs_password_and_accepts_it() {
    let (svc, _) = service();
    let err = svc
        .open_archive(fx("enc.7z").to_string_lossy().into(), None)
        .unwrap_err();
    assert_eq!(err.key, "error.password_required");
    let ok = svc.open_archive(fx("enc.7z").to_string_lossy().into(), Some("secret".into()));
    assert!(ok.is_ok());
}

#[test]
fn list_children_bad_dir_is_empty_not_panic() {
    let (svc, _) = service();
    let r = svc
        .open_archive(fx("plain.zip").to_string_lossy().into(), None)
        .unwrap();
    let kids = svc.list_children(r.id, "nope/".into()).unwrap();
    assert!(kids.is_empty());
}

#[test]
fn extract_emits_progress_and_finished_ok() {
    let (svc, events) = service();
    let r = svc
        .open_archive(fx("plain.zip").to_string_lossy().into(), None)
        .unwrap();
    let dest = std::env::temp_dir().join(format!("zipnest-t4-{}", std::process::id()));
    let job = svc
        .extract(r.id, vec!["a.txt".into()], dest.to_string_lossy().into(), Some("overwrite".into()), None)
        .unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, _)| n == "job_finished"));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["job_id"], job);
    assert_eq!(fin.1["ok"], true);
    assert!(fin.1["error_key"].is_null());
    assert!(v.iter().any(|(n, _)| n == "job_progress"));
    let out = dest.join("a.txt");
    assert!(out.exists(), "extracted file missing: {}", out.display());
    std::fs::remove_dir_all(&dest).ok();
}

// A relative destination is resolved against the process working directory,
// which for a shell launch is nowhere the user would look: the run would report
// success and the files would be somewhere else. Refused up front.
#[test]
fn extract_refuses_a_relative_destination() {
    let (svc, _events) = service();
    let r = svc
        .open_archive(fx("plain.zip").to_string_lossy().into(), None)
        .unwrap();
    for dest in ["out", r"out\sub", "."] {
        let err = svc
            .extract(r.id, vec![], dest.to_string(), Some("overwrite".into()), None)
            .expect_err("a relative destination must not start a job");
        assert_eq!(err.key, "error.io");
    }
}

// Empty selection = "extract the whole archive". This is the one-click
// "解压整个压缩包" path from the UI (no rows selected).
#[test]
fn extract_empty_selection_unpacks_whole_archive() {
    let (svc, _events) = service();
    let r = svc
        .open_archive(fx("plain.zip").to_string_lossy().into(), None)
        .unwrap();
    let dest = std::env::temp_dir().join(format!("zipnest-tall-{}", std::process::id()));
    let job = svc
        .extract(r.id, vec![], dest.to_string_lossy().into(), Some("overwrite".into()), None)
        .unwrap();
    wait_events(&_events, 5000, |v| v.iter().any(|(n, _)| n == "job_finished"));
    assert!(dest.join("a.txt").exists(), "whole archive should include a.txt");
    assert!(dest.join("c.txt").exists(), "whole archive should include c.txt");
    assert!(
        dest.join("sub").join("b.txt").exists(),
        "whole archive should recurse into sub/ (nested b.txt)"
    );
    assert!(!dest.join("sub").is_file(), "sub must stay a directory");
    let _ = job;
    std::fs::remove_dir_all(&dest).ok();
}

// c.txt is the only Deflate-compressed entry in plain.zip (8406 bytes).
// Store entries extracted fine while this one came out as 0 bytes through
// the UI/IPC path — pin the full byte count.
#[test]
fn extract_deflated_entry_writes_full_bytes() {
    let (svc, events) = service();
    let r = svc
        .open_archive(fx("plain.zip").to_string_lossy().into(), None)
        .unwrap();
    let dest = std::env::temp_dir().join(format!("zipnest-t4c-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    let job = svc
        .extract(r.id, vec!["c.txt".into()], dest.to_string_lossy().into(), Some("overwrite".into()), None)
        .unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, _)| n == "job_finished"));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["job_id"], job);
    assert_eq!(fin.1["ok"], true, "extract failed: {:?}", fin.1);
    drop(v);
    let len = std::fs::metadata(dest.join("c.txt"))
        .expect("c.txt must exist")
        .len();
    assert_eq!(len, 8406, "deflated entry must be fully written");
    std::fs::remove_dir_all(&dest).ok();
}

// The exact selection the UI made during smoke: a directory plus two files
// (3 non-dir indices of the 4-entry archive) — subset extract regressed to
// 0-byte output for the deflated entry even though whole-archive and
// single-entry extracts were fine.
#[test]
fn extract_ui_selection_subset_writes_full_bytes() {
    let (svc, events) = service();
    let r = svc
        .open_archive(fx("plain.zip").to_string_lossy().into(), None)
        .unwrap();
    let dest = std::env::temp_dir().join(format!("zipnest-t4d-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    let job = svc
        .extract(
            r.id,
            vec!["sub".into(), "a.txt".into(), "c.txt".into()],
            dest.to_string_lossy().into(),
            Some("overwrite".into()),
            None,
        )
        .unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, _)| n == "job_finished"));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["job_id"], job);
    assert_eq!(fin.1["ok"], true, "extract failed: {:?}", fin.1);
    drop(v);
    let len = std::fs::metadata(dest.join("c.txt"))
        .expect("c.txt must exist")
        .len();
    assert_eq!(len, 8406, "deflated entry must be fully written");
    assert_eq!(std::fs::read_to_string(dest.join("sub/b.txt")).unwrap().trim(), "nested content");
    std::fs::remove_dir_all(&dest).ok();
}

#[test]
fn extract_and_delete_removes_source_only_after_success() {
    let (svc, events) = service();
    let tmp = std::env::temp_dir().join(format!("zipnest-delete-ok-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let source = tmp.join("plain.zip");
    std::fs::copy(fx("plain.zip"), &source).unwrap();
    let dest = tmp.join("out");
    let r = svc.open_archive(source.to_string_lossy().into(), None).unwrap();
    let job = svc
        .extract_with_delete(
            r.id,
            vec![], // whole archive
            dest.to_string_lossy().into(),
            Some("overwrite".into()),
            None,
            true,
        )
        .unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, e)| n == "job_finished" && e["job_id"] == job));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["ok"], true, "extract failed: {:?}", fin.1);
    drop(v);

    assert!(dest.join("a.txt").exists(), "whole archive must be extracted");
    assert!(dest.join("c.txt").exists(), "whole archive must be extracted");
    assert!(!source.exists(), "source archive must be deleted");
    assert_eq!(svc.open_archive_count(), 0, "deleted archive must be closed");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn extract_and_delete_refuses_a_partial_selection() {
    let (svc, events) = service();
    let tmp = std::env::temp_dir().join(format!("zipnest-delete-part-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let source = tmp.join("plain.zip");
    std::fs::copy(fx("plain.zip"), &source).unwrap();
    let dest = tmp.join("out");
    let r = svc.open_archive(source.to_string_lossy().into(), None).unwrap();
    let job = svc
        .extract_with_delete(
            r.id,
            vec!["a.txt".into()],
            dest.to_string_lossy().into(),
            Some("overwrite".into()),
            None,
            true,
        )
        .unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, e)| n == "job_finished" && e["job_id"] == job));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["ok"], false);
    assert_eq!(fin.1["error_key"].as_str(), Some("error.delete_partial"));
    drop(v);

    assert!(source.exists(), "a partial extract must keep its archive");
    assert_eq!(svc.open_archive_count(), 1, "kept archive stays open");
    assert!(!dest.join("a.txt").exists(), "the refused run wrote nothing");
    svc.close_archive(r.id);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Deleting the source is only safe when the archive is not the last copy of
/// anything. A `skip` entry leaves the archive's version of that file on no
/// disk at all — the destination kept its own — so the source must survive.
#[test]
fn extract_and_delete_refuses_when_entries_were_skipped() {
    let (svc, events) = service();
    let tmp = std::env::temp_dir().join(format!("zipnest-delete-skip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let source = tmp.join("plain.zip");
    std::fs::copy(fx("plain.zip"), &source).unwrap();
    let dest = tmp.join("out");
    std::fs::create_dir_all(&dest).unwrap();
    // `a.txt` is already there, so the `skip` policy leaves it alone and the
    // archive's copy of it never gets written.
    std::fs::write(dest.join("a.txt"), b"OLDER").unwrap();
    let r = svc.open_archive(source.to_string_lossy().into(), None).unwrap();
    let job = svc
        .extract_with_delete(
            r.id,
            vec![], // the whole archive: the partial-selection check does not apply
            dest.to_string_lossy().into(),
            Some("skip".into()),
            None,
            true,
        )
        .unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, e)| n == "job_finished" && e["job_id"] == job));
    let v = events.lock().unwrap();
    let skipped = v.iter().any(|(n, _)| n == "extract_skipped");
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert!(skipped, "the run must report the skipped entry");
    assert_eq!(fin.1["ok"], false, "deleting past a skip is not a success");
    assert_eq!(fin.1["error_key"].as_str(), Some("error.delete_skipped"));
    drop(v);

    assert!(source.exists(), "the archive is the only copy of a.txt's new content");
    assert_eq!(
        std::fs::read(dest.join("a.txt")).unwrap(),
        b"OLDER",
        "the skip policy still leaves the existing file alone"
    );
    assert!(dest.join("c.txt").exists(), "the rest of the archive did extract");
    svc.close_archive(r.id);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn failed_extract_with_delete_requested_keeps_source() {
    let (svc, events) = service();
    let tmp = std::env::temp_dir().join(format!("zipnest-delete-fail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let source = tmp.join("enc.7z");
    std::fs::copy(fx("enc.7z"), &source).unwrap();
    let dest = tmp.join("out");
    let r = svc
        .open_archive(source.to_string_lossy().into(), Some("secret".into()))
        .unwrap();
    let job = svc
        .extract_with_delete(
            r.id,
            vec!["a.txt".into()],
            dest.to_string_lossy().into(),
            Some("overwrite".into()),
            Some("wrong".into()),
            true,
        )
        .unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, e)| n == "job_finished" && e["job_id"] == job));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["ok"], false);
    assert_eq!(fin.1["error_key"].as_str(), Some("error.password_incorrect"));
    drop(v);

    assert!(source.exists(), "a failed extraction must keep its archive");
    assert_eq!(svc.open_archive_count(), 1, "failed archive stays available");
    svc.close_archive(r.id);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn extract_wrong_password_reports_key() {
    let (svc, events) = service();
    let r = svc
        .open_archive(fx("enc.7z").to_string_lossy().into(), Some("secret".into()))
        .unwrap();
    let dest = std::env::temp_dir().join(format!("zipnest-t4b-{}", std::process::id()));
    let job = svc
        .extract(r.id, vec!["a.txt".into()], dest.to_string_lossy().into(), Some("overwrite".into()), Some("wrong".into()))
        .unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, _)| n == "job_finished"));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["job_id"], job);
    assert_eq!(fin.1["ok"], false);
    assert_eq!(fin.1["error_key"].as_str(), Some("error.password_incorrect"));
    std::fs::remove_dir_all(&dest).ok();
}

/// 加密 ZIP 内容坏了 + 密码正确：必须报 `error.corrupt`，并且把没写出来的
/// 条目随事件交出去。
///
/// 用户侧的差别很大：「密码错误」意味着再输一次就有救，`error.password_incorrect`
/// 也确实会被 UI 变成第二个密码框；「已损坏」才是真的，再输多少次也没用。
/// 同时这条致命路径以前会丢掉 failed 名单 —— 用户只知道少了东西，不知道少了什么。
#[test]
fn extract_damaged_encrypted_zip_reports_corrupt_with_entry_list() {
    let (svc, events) = service();
    // 在测试里现场改坏密文字节（AES 密文区从偏移 64 开始），不依赖额外的 fixture。
    let tmp = std::env::temp_dir().join(format!("zipnest-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let damaged = tmp.join("damaged.zip");
    let mut bytes = std::fs::read(fx("enc.zip")).unwrap();
    bytes[70] ^= 0xFF;
    std::fs::write(&damaged, &bytes).unwrap();

    let r = svc
        .open_archive(damaged.to_string_lossy().into(), None)
        .unwrap();
    let dest = tmp.join("out");
    let job = svc
        .extract(
            r.id,
            vec!["a.txt".into()],
            dest.to_string_lossy().into(),
            Some("overwrite".into()),
            Some("secret".into()),
        )
        .unwrap();
    wait_events(&events, 5000, |v| {
        v.iter().any(|(n, e)| n == "job_finished" && e["job_id"] == job)
    });
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["ok"], false);
    assert_eq!(
        fin.1["error_key"].as_str(),
        Some("error.corrupt"),
        "密码对了、数据坏了，不能报成密码错误"
    );
    // 没写出来的条目要能列出来。
    let failed = v
        .iter()
        .find(|(n, _)| n == "extract_failed")
        .expect("损坏时也要发出 extract_failed，带上没法写出来的条目");
    assert_eq!(failed.1["count"].as_u64(), Some(1));
    assert_eq!(failed.1["entries"][0]["path"].as_str(), Some("a.txt"));
    assert_eq!(failed.1["entries"][0]["reason"].as_str(), Some("error.corrupt"));
    drop(v);

    assert!(!dest.join("a.txt").exists(), "坏掉的数据不该落盘");
    svc.close_archive(r.id);
    let _ = std::fs::remove_dir_all(&tmp);
}

// The password given at open time must carry over to extract — the UI has
// no second prompt for the happy path (desktop archiver session semantics).
#[test]
fn open_with_password_then_extract_without_it_reuses_open_password() {
    let (svc, events) = service();
    let r = svc
        .open_archive(fx("enc.7z").to_string_lossy().into(), Some("secret".into()))
        .unwrap();
    let dest = std::env::temp_dir().join(format!("zipnest-t4e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    let job = svc
        .extract(r.id, vec!["a.txt".into()], dest.to_string_lossy().into(), Some("overwrite".into()), None)
        .unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, _)| n == "job_finished"));
    {
        let v = events.lock().unwrap();
        let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
        assert_eq!(fin.1["job_id"], job);
        assert_eq!(fin.1["ok"], true, "extract failed: {:?}", fin.1);
    }
    let len = std::fs::metadata(dest.join("a.txt"))
        .expect("a.txt must exist")
        .len();
    assert_eq!(len, 19, "encrypted entry must be fully written");
    std::fs::remove_dir_all(&dest).ok();
}

#[test]
fn create_archive_emits_finished_ok() {
    let (svc, events) = service();
    let tmp = std::env::temp_dir().join(format!("zn-ipc-create-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let src = tmp.join("a.txt"); std::fs::write(&src, b"ipc create").unwrap();
    let dest = tmp.join("out.zip");
    let job = svc.create_archive(vec![src.to_string_lossy().into()], dest.to_string_lossy().into(),
        serde_json::from_value(serde_json::json!({"format":"zip","level":"normal","method":"auto"})).unwrap()).unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, e)| n == "job_finished" && e["job_id"] == job));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["ok"], true, "{:?}", fin.1);
    drop(v);
    assert!(dest.exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

// Unknown DTO enum strings are rejected synchronously with a stable key,
// before any job is queued (the DTO is the untrusted frontend boundary).
#[test]
fn create_archive_invalid_format_is_rejected() {
    let (svc, _) = service();
    let tmp = std::env::temp_dir().join(format!("zn-ipc-create-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let src = tmp.join("a.txt");
    std::fs::write(&src, b"x").unwrap();
    let dest = tmp.join("out.rar");
    let err = svc
        .create_archive(
            vec![src.to_string_lossy().into()],
            dest.to_string_lossy().into(),
            serde_json::from_value(serde_json::json!({"format":"rar","level":"normal","method":"auto"})).unwrap(),
        )
        .unwrap_err();
    assert_eq!(err.key, "error.engine");
    assert!(!dest.exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

// A missing source fails the job (not a panic, not a hang) with the engine's
// I/O key surfaced through `job_finished`.
#[test]
fn create_archive_missing_source_finishes_failed() {
    let (svc, events) = service();
    let tmp = std::env::temp_dir().join(format!("zn-ipc-create-miss-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let dest = tmp.join("out.zip");
    let missing = tmp.join("nope.txt");
    let job = svc
        .create_archive(
            vec![missing.to_string_lossy().into()],
            dest.to_string_lossy().into(),
            serde_json::from_value(serde_json::json!({"format":"zip","level":"normal","method":"auto"})).unwrap(),
        )
        .unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, e)| n == "job_finished" && e["job_id"] == job));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["ok"], false);
    assert_eq!(fin.1["error_key"].as_str(), Some("error.io"));
    drop(v);
    assert!(!dest.exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

// Encrypted entries with no password from either source must fail with a
// re-promptable key, not an opaque engine error.
#[test]
fn extract_encrypted_without_any_password_prompts() {
    let (svc, events) = service();
    // enc.zip: entry-encrypted, header readable — opens without a password.
    let r = svc
        .open_archive(fx("enc.zip").to_string_lossy().into(), None)
        .unwrap();
    let dest = std::env::temp_dir().join(format!("zipnest-t4f-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    let job = svc
        .extract(r.id, vec!["a.txt".into()], dest.to_string_lossy().into(), Some("overwrite".into()), None)
        .unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, _)| n == "job_finished"));
    {
        let v = events.lock().unwrap();
        let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
        assert_eq!(fin.1["job_id"], job);
        assert_eq!(fin.1["ok"], false);
        assert_eq!(
            fin.1["error_key"].as_str(),
            Some("error.password_required"),
            "expected password_required, got {:?}",
            fin.1
        );
    }
    assert!(!dest.exists() || std::fs::read_dir(&dest).unwrap().next().is_none());
    std::fs::remove_dir_all(&dest).ok();
}

// Preview has no dialog to fall back on, so the same "ask for a password" key
// has to come back from the read itself; the engine would only say "failed".
#[test]
fn read_entry_encrypted_without_password_prompts() {
    let (svc, _) = service();
    let r = svc
        .open_archive(fx("enc.zip").to_string_lossy().into(), None)
        .unwrap();
    let err = svc
        .read_entry_bytes(r.id, "a.txt".into(), 1024)
        .unwrap_err();
    assert_eq!(err.key, "error.password_required");
}

#[test]
fn read_entry_encrypted_reuses_open_time_password() {
    let (svc, _) = service();
    let r = svc
        .open_archive(fx("enc.zip").to_string_lossy().into(), Some("secret".into()))
        .unwrap();
    let bytes = svc
        .read_entry_bytes(r.id, "a.txt".into(), 1024)
        .expect("the password given at open time unlocks the entry");
    assert!(!bytes.is_empty());
}

#[test]
fn open_encrypted_7z_wrong_password_reports_incorrect() {
    let (svc, _) = service();
    let err = svc
        .open_archive(fx("enc.7z").to_string_lossy().into(), Some("wrong".into()))
        .unwrap_err();
    assert_eq!(err.key, "error.password_incorrect");
}
