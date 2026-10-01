//! Tauri command surface. Thin glue only: every archive decision lives in
//! `zipnest-ipc`; errors cross the boundary as `IpcError { key }` strings
//! the frontend maps through i18n. Command names are the JS contract.

use tauri::State;
use zipnest_ipc::{CreateRequest, EntryDto, IpcError, IpcService, OpenArchiveResult};

#[tauri::command]
pub fn open_archive(
    svc: State<'_, IpcService>,
    path: String,
    password: Option<String>,
) -> Result<OpenArchiveResult, IpcError> {
    svc.open_archive(path, password)
}

#[tauri::command]
pub fn list_children(
    svc: State<'_, IpcService>,
    id: u64,
    dir: String,
) -> Result<Vec<EntryDto>, IpcError> {
    svc.list_children(id, dir)
}

#[tauri::command]
pub fn read_entry_bytes(
    svc: State<'_, IpcService>,
    id: u64,
    path: String,
    max_bytes: u64,
) -> Result<Vec<u8>, IpcError> {
    svc.read_entry_bytes(id, path, max_bytes)
}

#[tauri::command]
pub fn extract(
    svc: State<'_, IpcService>,
    id: u64,
    paths: Vec<String>,
    dest: String,
    overwrite: bool,
    password: Option<String>,
) -> Result<u64, IpcError> {
    svc.extract(id, paths, dest, overwrite, password)
}

/// Queue a create job from on-disk `sources`; returns its id. The `options`
/// object is validated in `zipnest-ipc` (unknown format/level/method/sfx are
/// rejected with a stable key) before the job is queued.
#[tauri::command]
pub fn create_archive(
    svc: State<'_, IpcService>,
    sources: Vec<String>,
    dest: String,
    options: serde_json::Value,
) -> Result<u64, IpcError> {
    let options: CreateRequest =
        serde_json::from_value(options).map_err(|_| IpcError::new("error.engine"))?;
    svc.create_archive(sources, dest, options)
}

#[tauri::command]
pub fn job_cancel(svc: State<'_, IpcService>, job_id: u64) -> bool {
    svc.cancel(job_id)
}

/// Reveal an on-disk path in Explorer (used for extraction destinations).
#[tauri::command]
pub fn reveal_in_explorer(path: String) -> Result<(), String> {
    if !std::path::Path::new(&path).exists() {
        return Err("error.io".into());
    }
    std::process::Command::new("explorer")
        .args(["/select,", &path])
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Extract one entry to the temp dir and open it with the OS default app.
#[tauri::command]
pub fn open_entry(
    svc: State<'_, IpcService>,
    id: u64,
    path: String,
) -> Result<(), String> {
    let bytes = svc
        .read_entry_bytes(id, path.clone(), 256 * 1024 * 1024)
        .map_err(|e| e.key)?;
    let base = std::path::Path::new(&path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "entry.bin".into());
    let tmp = std::env::temp_dir().join("zipnest-open").join(base);
    std::fs::create_dir_all(tmp.parent().unwrap())
        .and_then(|()| std::fs::write(&tmp, &bytes))
        .map_err(|e| e.to_string())?;
    std::process::Command::new("cmd")
        .args(["/c", "start", "", &tmp.to_string_lossy()])
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
