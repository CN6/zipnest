// MSVC linker emits info lines (lib/exp paths); not a code issue.
#![allow(linker_messages)]

mod commands;

use tauri::{Emitter, Manager};
use zipnest_ipc::IpcService;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // The emit callback bridges job events from worker threads to
            // the webview; the 200ms throttle lives in the IPC service.
            let handle = app.handle().clone();
            let svc = IpcService::new(
                std::sync::Arc::new(move |name: &str, payload: serde_json::Value| {
                    let _ = handle.emit(name, payload);
                }),
                std::time::Duration::from_millis(200),
            );
            app.manage(svc);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_archive,
            commands::list_children,
            commands::read_entry_bytes,
            commands::extract,
            commands::job_cancel,
            commands::reveal_in_explorer,
            commands::open_entry,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
