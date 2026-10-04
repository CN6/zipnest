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
