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

#[allow(dead_code)] // used by the extract tests added in the next step
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
