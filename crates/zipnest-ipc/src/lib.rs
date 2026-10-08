//! IPC service layer between the Tauri glue and the archive engine.
//!
//! Framework-free on purpose: the Tauri app wires an emit callback, tests
//! wire a recording vector. No file-system access beyond what the engine
//! performs through [`IpcService`].

pub mod diagnostics;
pub mod error;
pub mod registry;
pub mod service;
pub mod settings;
pub mod shell;

pub use diagnostics::{mailto_url, Diagnostics};
pub use error::IpcError;
pub use service::{CreateRequest, EntryDto, IpcService, OpenArchiveResult};
pub use settings::{Settings, SettingsPatch, SettingsStore};
pub use shell::{ShellApplier, ShellOptions, ShellRegisterResult};

/// Whether the unpacking engine can be loaded right now.
///
/// Used by the diagnostics report in Settings. It runs the same load the engine
/// would (cached after the first call) and never logs or transmits anything:
/// "7z.dll missing" is otherwise invisible until a user tries to open an
/// archive.
pub fn engine_available() -> bool {
    archive_core::dll::load().is_ok()
}

/// Test-only: remove temp files and directories left behind by *earlier* runs.
///
/// The test helpers name their scratch space after the process id, so every run
/// creates a fresh set and nothing ever reuses or deletes the previous one —
/// `cargo test` was quietly leaving tens of megabytes in `%TEMP%`. Cleanup
/// cannot simply run at the end either: a failing assertion skips it. So each
/// helper sweeps on the way in instead, and only touches entries older than an
/// hour so a second test run in another terminal is never disturbed.
#[cfg(test)]
pub(crate) fn sweep_stale_test_temp(prefix: &str) {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(3600))
        .unwrap_or(std::time::UNIX_EPOCH);
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(prefix) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .map(|modified| modified < cutoff)
            .unwrap_or(false);
        if !stale {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}
