// MSVC linker emits info lines (lib/exp paths); not a code issue.
#![allow(linker_messages)]

mod commands;
mod launch;

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager};
use zipnest_ipc::IpcService;

/// Archive path handed in on the command line, consumed once by the frontend.
pub(crate) struct LaunchFile(pub(crate) std::sync::Mutex<Option<String>>);

fn focus_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be the first plugin so the second instance exits before the
        // webview spins up.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if let Some(path) = launch::archive_arg(&argv) {
                let _ = app.emit("open_file_request", serde_json::json!({ "path": path }));
            }
            focus_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let svc = IpcService::new(
                std::sync::Arc::new(move |name: &str, payload: serde_json::Value| {
                    let _ = handle.emit(name, payload);
                }),
                std::time::Duration::from_millis(200),
            );
            app.manage(svc);
            app.manage(LaunchFile(std::sync::Mutex::new(launch::archive_arg(
                &std::env::args().collect::<Vec<_>>(),
            ))));

            // System tray: bring the main window back or quit. Native labels
            // are fixed for now; they do not switch with the in-app locale.
            let show = MenuItemBuilder::with_id("show", "打开主界面").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&show, &quit]).build()?;
            let mut tray = TrayIconBuilder::with_id("main")
                .menu(&menu)
                .tooltip("ZipNest")
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => focus_main(app),
                    "quit" => app.exit(0),
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_archive,
            commands::list_children,
            commands::read_entry_bytes,
            commands::extract,
            commands::create_archive,
            commands::job_cancel,
            commands::settings_get,
            commands::settings_set,
            commands::shell_register,
            commands::reveal_in_explorer,
            commands::open_entry,
            commands::launch_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
