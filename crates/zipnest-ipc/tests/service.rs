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
        .extract(r.id, vec!["a.txt".into()], dest.to_string_lossy().into(), true, None)
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

#[test]
fn extract_wrong_password_reports_key() {
    let (svc, events) = service();
    let r = svc
        .open_archive(fx("enc.7z").to_string_lossy().into(), Some("secret".into()))
        .unwrap();
    let dest = std::env::temp_dir().join(format!("zipnest-t4b-{}", std::process::id()));
    let job = svc
        .extract(r.id, vec!["a.txt".into()], dest.to_string_lossy().into(), true, Some("wrong".into()))
        .unwrap();
    wait_events(&events, 5000, |v| v.iter().any(|(n, _)| n == "job_finished"));
    let v = events.lock().unwrap();
    let fin = v.iter().find(|(n, _)| n == "job_finished").unwrap();
    assert_eq!(fin.1["job_id"], job);
    assert_eq!(fin.1["ok"], false);
    assert_eq!(fin.1["error_key"].as_str(), Some("error.password_incorrect"));
    std::fs::remove_dir_all(&dest).ok();
}
