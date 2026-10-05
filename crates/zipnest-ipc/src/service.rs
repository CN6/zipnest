//! The IPC facade: open/list/read today, extract/cancel in the next step.
//! Emits `job_*` / `password_required` events through the emit callback.

use crate::error::IpcError;
use crate::registry::{ArchiveRegistry, SharedArchive};
use crate::settings::{Settings, SettingsPatch, SettingsStore};
use archive_core::types::{
    CompressionLevel, CompressionMethod, CreateFormat, CreateOptions, SfxKind,
};
use archive_core::{Archive, ArchiveEntry, ArchiveOpenOptions};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// How often a running job is allowed to report progress to the UI.
///
/// The engines call back once per entry, and every report wakes the UI thread
/// and repaints the window: a folder with tens of thousands of small files spent
/// far longer repainting a progress bar than compressing. Throttling cannot
/// change the archive — it only decides how often the same numbers are drawn.
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(50);

/// Rate limiter for progress callbacks. The first report always goes out, so a
/// short job still shows something.
struct ProgressThrottle {
    last: std::time::Instant,
    interval: Duration,
}

impl ProgressThrottle {
    fn new(interval: Duration) -> Self {
        Self {
            last: std::time::Instant::now() - interval,
            interval,
        }
    }

    /// `true` when this progress report is due to be forwarded.
    fn due(&mut self) -> bool {
        if self.last.elapsed() >= self.interval {
            self.last = std::time::Instant::now();
            true
        } else {
            false
        }
    }
}

/// One archive row in the UI. Serialized camel-free: fields match the
/// frontend `EntryDto` interface verbatim.
#[derive(Debug, Clone, Serialize)]
pub struct EntryDto {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime_ms: Option<u64>,
    pub encrypted: bool,
}

/// Response of `open_archive`: metadata plus the archive's root entries.
#[derive(Debug, Clone, Serialize)]
pub struct OpenArchiveResult {
    pub id: u64,
    pub encrypted: bool,
    pub format: String,
    pub entries: Vec<EntryDto>,
}

/// Create wizard options, as sent by the frontend. Enum-like fields are
/// stable lowercase strings (never the Rust enum names) so the DTO can evolve
/// without breaking the UI:
///
/// - `format`: `"zip" | "7z" | "tar" | "tar.gz" | "tar.bz2" | "tar.xz"`
///   (aliases `"7zip"`, `"tgz"`, `"tbz2"`, `"txz"` accepted)
/// - `level`: `"store" | "fastest" | "normal" | "maximum" | "ultra"`
/// - `method`: `"auto" | "copy" | "deflate" | "lzma2" | "bzip2"`
/// - `sfx`: `"gui" | "console"` (7z-only; omitted when not building an SFX)
///
/// The password is memory-only; `Debug` redacts it and it is never logged.
#[derive(Clone, Deserialize)]
pub struct CreateRequest {
    pub format: String,
    pub level: String,
    pub method: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub encrypt_names: Option<bool>,
    #[serde(default)]
    pub volume_bytes: Option<u64>,
    #[serde(default)]
    pub sfx: Option<String>,
}

impl std::fmt::Debug for CreateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateRequest")
            .field("format", &self.format)
            .field("level", &self.level)
            .field("method", &self.method)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("encrypt_names", &self.encrypt_names)
            .field("volume_bytes", &self.volume_bytes)
            .field("sfx", &self.sfx)
            .finish()
    }
}

impl CreateRequest {
    /// Map the wire DTO onto the engine's typed [`CreateOptions`]. Unknown
    /// strings are rejected here (the untrusted frontend boundary) with a
    /// stable key instead of reaching the engine.
    fn into_engine_options(self) -> Result<CreateOptions, IpcError> {
        let format = match self.format.to_ascii_lowercase().as_str() {
            "zip" => CreateFormat::Zip,
            "7z" | "7zip" => CreateFormat::SevenZ,
            "tar" => CreateFormat::Tar,
            "tar.gz" | "tgz" | "targz" => CreateFormat::TarGz,
            "tar.bz2" | "tbz2" | "tarbz2" => CreateFormat::TarBz2,
            "tar.xz" | "txz" | "tarxz" => CreateFormat::TarXz,
            _ => return Err(IpcError::new("error.engine")),
        };
        let level = match self.level.to_ascii_lowercase().as_str() {
            "store" => CompressionLevel::Store,
            "fastest" => CompressionLevel::Fastest,
            "normal" => CompressionLevel::Normal,
            "maximum" => CompressionLevel::Maximum,
            "ultra" => CompressionLevel::Ultra,
            _ => return Err(IpcError::new("error.engine")),
        };
        let method = match self.method.to_ascii_lowercase().as_str() {
            "auto" => CompressionMethod::Auto,
            "copy" => CompressionMethod::Copy,
            "deflate" => CompressionMethod::Deflate,
            "lzma2" => CompressionMethod::Lzma2,
            "bzip2" => CompressionMethod::Bzip2,
            _ => return Err(IpcError::new("error.engine")),
        };
        let sfx = match self.sfx.as_deref() {
            None | Some("") => None,
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "gui" => Some(SfxKind::Gui),
                "console" => Some(SfxKind::Console),
                _ => return Err(IpcError::new("error.engine")),
            },
        };
        Ok(CreateOptions {
            format,
            level,
            method,
            password: self.password,
            encrypt_names: self.encrypt_names.unwrap_or(false),
            volume_bytes: self.volume_bytes,
            sfx,
        })
    }
}

fn to_dto(e: &ArchiveEntry) -> EntryDto {
    let name = e
        .path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&e.path)
        .to_string();
    EntryDto {
        path: e.path.clone(),
        name,
        is_dir: e.is_dir,
        size: e.size,
        mtime_ms: e
            .mtime
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64),
        encrypted: e.encrypted,
    }
}

/// Direct children of `dir` (`""` = root); tolerates trailing-slash forms.
/// Archives may store Windows-style backslash separators (`sub\b.txt`), so
/// matching normalizes to `/` without mutating the stored path (the UI and
/// `read_entry` see the engine's original path verbatim).
fn children_of<'a>(entries: &'a [ArchiveEntry], dir: &str) -> Vec<&'a ArchiveEntry> {
    fn norm(s: &str) -> String {
        s.replace('\\', "/")
    }
    let dir = norm(dir.trim_end_matches(['/', '\\']));
    entries
        .iter()
        .filter(|e| {
            let p = norm(e.path.trim_end_matches(['/', '\\']));
            if dir.is_empty() {
                !p.contains('/')
            } else {
                matches!(p.strip_prefix(&format!("{dir}/")), Some(rest) if !rest.contains('/'))
            }
        })
        .collect()
}

/// Expand the UI's selection into the engine's index list.
///
/// A selected directory contributes its subtree *and* the directory entry
/// itself, so empty folders land on disk. An empty selection means "the whole
/// archive". Directory membership is looked up in a set instead of rescanning
/// every entry once per selection (that inner scan made selecting a big
/// archive quadratic in the entry count). The result is sorted and deduped.
fn select_entries(entries: &[ArchiveEntry], paths: &[String]) -> Vec<u32> {
    fn norm(s: &str) -> String {
        s.replace('\\', "/").trim_end_matches('/').to_string()
    }
    let selection: Vec<String> = paths.iter().map(|p| norm(p)).collect();
    let mut wanted: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    if selection.is_empty() || selection.iter().any(|s| s.is_empty()) {
        wanted.extend(entries.iter().map(|e| e.index));
        return wanted.into_iter().collect();
    }
    let dirs: std::collections::HashSet<String> = entries
        .iter()
        .filter(|e| e.is_dir)
        .map(|e| norm(&e.path))
        .collect();
    let selected: std::collections::HashSet<&str> = selection.iter().map(|s| s.as_str()).collect();
    for e in entries {
        let path = norm(&e.path);
        let inside_selected_dir = selected.iter().any(|sel| {
            dirs.contains(*sel) && path.strip_prefix(sel).is_some_and(|rest| rest.starts_with('/'))
        });
        if selected.contains(path.as_str()) || inside_selected_dir {
            wanted.insert(e.index);
        }
    }
    wanted.into_iter().collect()
}

/// `emit(event_name, json_payload)` — thread-safe callback the glue layer
/// wires to Tauri events (tests wire a recorder).
pub type Emit = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

pub struct IpcService {
    /// Exposed read-only for the Tauri glue (resolve an entry for preview).
    pub registry: ArchiveRegistry,
    jobs: archive_jobs::JobManager,
    emit: Emit,
    settings: std::sync::Mutex<SettingsStore>,
}

impl IpcService {
    /// `emit` is called from job worker threads — the callback must be
    /// thread-safe. `throttle` controls progress event spacing (200ms in
    /// production, 0ms in tests).
    pub fn with_settings_path(
        emit: Emit,
        throttle: Duration,
        settings_path: std::path::PathBuf,
    ) -> Self {
        let emit2 = Arc::clone(&emit);
        let sink = Box::new(move |ev: archive_jobs::JobEvent| {
            use archive_jobs::JobEvent;
            match ev {
                JobEvent::Progress {
                    job_id,
                    done_items,
                    total_items,
                    done_bytes,
                    total_bytes,
                    speed_bps,
                    eta_secs,
                } => emit2(
                    "job_progress",
                    serde_json::json!({
                        "job_id": job_id,
                        "done_items": done_items,
                        "total_items": total_items,
                        "done_bytes": done_bytes,
                        "total_bytes": total_bytes,
                        "speed_bps": speed_bps,
                        "eta_secs": eta_secs,
                    }),
                ),
                JobEvent::Finished { job_id, ok, error_key } => emit2(
                    "job_finished",
                    serde_json::json!({
                        "job_id": job_id,
                        "ok": ok,
                        "error_key": error_key,
                    }),
                ),
            }
        });
        IpcService {
            registry: ArchiveRegistry::default(),
            jobs: archive_jobs::JobManager::with_throttle(sink, throttle),
            emit,
            settings: std::sync::Mutex::new(SettingsStore::new(settings_path)),
        }
    }

    /// Production constructor: settings live in the per-user config dir.
    pub fn new(emit: Emit, throttle: Duration) -> Self {
        Self::with_settings_path(emit, throttle, crate::settings::default_path())
    }

    /// Current settings (never fails: defaults when the file is absent).
    pub fn settings_get(&self) -> Result<Settings, IpcError> {
        Ok(self.lock_settings()?.get().clone())
    }

    /// Validate + persist a settings patch, returning the resulting settings.
    pub fn settings_set(&self, patch: SettingsPatch) -> Result<Settings, IpcError> {
        self.lock_settings()?.set(patch)
    }

    fn lock_settings(&self) -> Result<std::sync::MutexGuard<'_, SettingsStore>, IpcError> {
        self.settings.lock().map_err(|_| IpcError::new("error.engine"))
    }

    /// Register or unregister the per-user Explorer integration for `exe`,
    /// then remember the choice. Each component (file associations, and the
    /// three context-menu targets) is applied independently: a hardened OS
    /// can deny one injection point while the rest still succeed, so the
    /// outcome carries a warning key per denied component instead of failing
    /// the whole save. The registry side is injected through `applier` so
    /// tests never touch the real registry.
    pub fn shell_register(
        &self,
        exe: &std::path::Path,
        opts: crate::shell::ShellOptions,
        applier: &dyn crate::shell::ShellApplier,
    ) -> Result<crate::shell::ShellRegisterResult, IpcError> {
        use crate::shell;
        let mut warnings = Vec::new();

        // Guard: cargo build artifacts (target/debug, target/release) must
        // never become the system default handler. Registering them pollutes
        // Explorer associations with paths that only exist on a dev machine
        // and with console-subsystem debug builds. Packaged apps run from
        // Program Files / the portable folder and pass this check.
        let exe_norm = exe.to_string_lossy().replace('\\', "/");
        if exe_norm.contains("/target/") {
            return Err(IpcError::new("error.shell.dev_path"));
        }

        let associate_ok = if opts.associate {
            let ok = applier.run(&shell::assoc_ops(exe)).is_ok();
            if !ok {
                warnings.push("error.shell.associate".into());
            }
            ok
        } else {
            let _ = applier.run(&shell::assoc_removals());
            false
        };

        let mut menu_ok = false;
        if opts.context_menu {
            for (key, ops) in [
                ("error.shell.file_menu", shell::file_menu_ops(exe)),
                ("error.shell.directory_menu", shell::directory_menu_ops(exe)),
                ("error.shell.background_menu", shell::background_menu_ops(exe)),
            ] {
                if applier.run(&ops).is_ok() {
                    menu_ok = true;
                } else {
                    warnings.push(key.into());
                }
            }
        } else {
            let _ = applier.run(&shell::context_menu_removals());
        }

        let settings = self.lock_settings()?.set(SettingsPatch {
            associate: Some(opts.associate && associate_ok),
            context_menu: Some(opts.context_menu && menu_ok),
            ..Default::default()
        })?;
        Ok(crate::shell::ShellRegisterResult {
            associate: settings.associate,
            context_menu: settings.context_menu,
            warnings,
        })
    }

    pub fn open_archive(&self, path: String, password: Option<String>) -> Result<OpenArchiveResult, IpcError> {
        let archive = Archive::open(
            std::path::Path::new(&path),
            ArchiveOpenOptions { password: password.clone() },
        )?;
        let entries = archive.entries()?;
        let encrypted = entries.iter().any(|e| e.encrypted);
        let format = std::path::Path::new(&path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let root = children_of(&entries, "").into_iter().map(to_dto).collect();
        let (id, _) = self.registry.insert(archive, password);
        Ok(OpenArchiveResult { id, encrypted, format, entries: root })
    }

    pub fn list_children(&self, id: u64, dir: String) -> Result<Vec<EntryDto>, IpcError> {
        let shared = self.lock_archive(id)?;
        let guard = shared.lock().map_err(|_| IpcError::new("error.engine"))?;
        let entries = guard.0.entries()?;
        Ok(children_of(&entries, &dir).into_iter().map(to_dto).collect())
    }

    pub fn read_entry_bytes(&self, id: u64, path: String, max_bytes: u64) -> Result<Vec<u8>, IpcError> {
        let shared = self.lock_archive(id)?;
        let guard = shared.lock().map_err(|_| IpcError::new("error.engine"))?;
        let entries = guard.0.entries()?;
        let idx = entries
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.index)
            .ok_or_else(|| IpcError::new("error.not_an_archive"))?;
        // Preview reuses the open-time password for encrypted entries.
        let opts = ArchiveOpenOptions {
            password: self.registry.password(id),
        };
        Ok(guard.0.read_entry(idx, &opts, Some(max_bytes))?)
    }

    fn lock_archive(&self, id: u64) -> Result<SharedArchive, IpcError> {
        self.registry
            .get(id)
            .ok_or_else(|| IpcError::new("error.not_an_archive"))
    }

    /// Cooperative cancel of a queued/running job. Unknown ids return false.
    pub fn cancel(&self, job_id: u64) -> bool {
        self.jobs.cancel(job_id)
    }

    /// Queue an extract job (behind any active one) and return its id.
    ///
    /// The runner holds the archive's mutex for the whole extraction —
    /// other commands on the same archive wait (single-archive-at-a-time
    /// is the M2 model; the UI only ever has one heavy job running).
    ///
    /// `on_conflict` carries the user's conflict policy (`"overwrite" | "skip" |
    /// "rename"`); `"ask"` is resolved by the UI before calling and unknown
    /// values fall back to overwriting. The zip-bomb guard is read from
    /// settings here because the job closure runs on a worker thread.
    pub fn extract(
        &self,
        id: u64,
        paths: Vec<String>,
        dest: String,
        on_conflict: Option<String>,
        password: Option<String>,
    ) -> Result<u64, IpcError> {
        if dest.trim().is_empty() {
            return Err(IpcError::new("error.io"));
        }
        let shared = self.lock_archive(id)?;
        // Callers may omit the password; fall back to the one supplied at
        // open time (session-scoped, memory only).
        let password = password.or_else(|| self.registry.password(id));
        let emit = Arc::clone(&self.emit);
        let max_total_bytes = match self.settings.lock() {
            Ok(s) => s.get().max_extract_bytes,
            Err(p) => p.into_inner().get().max_extract_bytes,
        };
        let on_conflict = archive_core::OnConflict::from_policy(on_conflict.as_deref());

        let job_id = self.jobs.submit("extract", Box::new(move |ctx| {
            let guard = shared.lock().map_err(|_| "error.engine".to_string())?;
            let entries = guard.0.entries().map_err(|e| e.error_key().to_string())?;

            // Expand the selection: a directory contributes its subtree *and*
            // the directory entry itself (so empty folders survive). An empty
            // selection means "extract the whole archive".
            let wanted = select_entries(&entries, &paths);
            if wanted.is_empty() {
                return Err("error.not_an_archive".into());
            }
            let wanted_set: std::collections::HashSet<u32> = wanted.iter().copied().collect();
            // Encrypted entries with no password anywhere (argument or
            // open-time): fail with a re-promptable key instead of letting
            // the engine surface an opaque data error.
            let needs_password = entries
                .iter()
                .any(|e| wanted_set.contains(&e.index) && e.encrypted);
            if needs_password && password.is_none() {
                return Err("error.password_required".into());
            }
            let total_items = wanted.len() as u64;
            let total_bytes: u64 = entries
                .iter()
                .filter(|e| wanted_set.contains(&e.index))
                .map(|e| e.size)
                .sum();

            let opts = archive_core::ExtractOptions {
                dest: std::path::PathBuf::from(&dest),
                entries: wanted,
                // Zip-bomb guard: writes are bounded by the user's limit.
                max_total_bytes,
                on_conflict,
            };
            let mut seen_paths = std::collections::HashSet::new();
            let mut throttle = ProgressThrottle::new(PROGRESS_MIN_INTERVAL);
            let result = guard.0.extract(&opts, password.as_deref(), &mut |d| {
                if ctx.cancelled() {
                    return false;
                }
                // `ExtractProgress` has no item counter; count distinct
                // paths observed as an approximation of files touched.
                seen_paths.insert(d.current_path.clone());
                if throttle.due() {
                    ctx.report(
                        seen_paths.len() as u64,
                        total_items,
                        d.done_bytes,
                        total_bytes,
                    );
                }
                true
            });
            drop(guard);
            let stats = result.map_err(|e| {
                let key = e.error_key().to_string();
                if key == "error.password_required" {
                    emit(
                        "password_required",
                        serde_json::json!({ "archive_id": id }),
                    );
                }
                key
            })?;
            if !stats.skipped.is_empty() {
                // Some entries could not be written safely (hostile/Windows-
                // reserved names). The safe ones are all extracted; tell the UI
                // so the skips are visible instead of looking like data loss.
                emit(
                    "extract_skipped",
                    serde_json::json!({
                        "count": stats.skipped.len(),
                        "names": stats.skipped,
                    }),
                );
            }
            Ok(())
        }));
        Ok(job_id)
    }

    /// Queue a create job and return its id.
    ///
    /// `sources` are on-disk paths (files or directories); each keeps its own
    /// basename inside the archive, and directories recurse (empty ones are
    /// preserved). The DTO is validated and mapped to [`CreateOptions`] before
    /// the job is queued, so a malformed request fails synchronously with an
    /// `IpcError` instead of producing a queued-but-doomed job. Progress is
    /// reported from the engine callback (return `false` on cancel), and the
    /// destination's parent must be writable — enforced by the engine.
    pub fn create_archive(
        &self,
        sources: Vec<String>,
        dest: String,
        options: CreateRequest,
    ) -> Result<u64, IpcError> {
        if sources.is_empty() || dest.trim().is_empty() {
            return Err(IpcError::new("error.io"));
        }
        let opts = options.into_engine_options()?;
        let inputs: Vec<std::path::PathBuf> =
            sources.into_iter().map(std::path::PathBuf::from).collect();

        let job_id = self.jobs.submit("create", Box::new(move |ctx| {
            // Each input is expanded relative to its own parent so the archive
            // node is the item's basename (a directory keeps its tree).
            let mut engine_sources = Vec::new();
            for input in &inputs {
                let base = input
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                let mut part =
                    archive_core::collect_sources(std::slice::from_ref(input), &base)
                        .map_err(|e| e.error_key().to_string())?;
                engine_sources.append(&mut part);
            }
            if engine_sources.is_empty() {
                return Err("error.io".into());
            }

            let total_items = engine_sources.len() as u64;
            let dest_path = std::path::PathBuf::from(&dest);
            // `CreateProgress` has no item counter; count distinct paths
            // observed as an approximation of entries written.
            let mut seen = std::collections::HashSet::new();
            let mut throttle = ProgressThrottle::new(PROGRESS_MIN_INTERVAL);
            archive_core::create_archive(&engine_sources, &dest_path, &opts, &mut |p| {
                if ctx.cancelled() {
                    return false;
                }
                seen.insert(p.current_path.clone());
                if throttle.due() {
                    ctx.report(seen.len() as u64, total_items, p.done_bytes, p.total_bytes);
                }
                true
            })
            .map_err(|e| e.error_key().to_string())?;
            Ok(())
        }));
        Ok(job_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::{RegOp, ShellApplier, ShellOptions};
    use std::sync::Mutex;

    /// Records the ops the service would apply instead of touching the registry.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<RegOp>>);

    impl ShellApplier for Recorder {
        fn run(&self, ops: &[RegOp]) -> std::io::Result<()> {
            self.0.lock().unwrap().extend_from_slice(ops);
            Ok(())
        }
    }

    /// Fails exactly the all-files context-menu target, like a hardened OS.
    struct PartialFail;

    impl ShellApplier for PartialFail {
        fn run(&self, ops: &[RegOp]) -> std::io::Result<()> {
            let blocked = ops.iter().any(|o| match o {
                RegOp::SetValue { key, .. }
                | RegOp::DeleteKey { key }
                | RegOp::DeleteValueIfEquals { key, .. } => key.contains(r"\*\shell"),
                RegOp::DeleteValue { .. } => false,
            });
            if blocked {
                Err(std::io::Error::other("access denied"))
            } else {
                Ok(())
            }
        }
    }

    fn service_with_settings(name: &str) -> IpcService {
        crate::sweep_stale_test_temp("zipnest-svc-");
        let root = std::env::temp_dir().join(format!("zipnest-svc-{}-{name}", std::process::id()));
        IpcService::with_settings_path(
            Arc::new(|_, _| {}),
            Duration::ZERO,
            root.join("settings.json"),
        )
    }

    #[test]
    fn shell_register_applies_ops_and_persists_flags() {
        let svc = service_with_settings("register");
        let exe = std::path::Path::new(r"C:\Apps\ZipNest.exe");

        let rec = Recorder::default();
        let out = svc
            .shell_register(exe, ShellOptions { associate: true, context_menu: false }, &rec)
            .unwrap();
        assert!(out.associate && !out.context_menu);
        assert!(rec
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|o| matches!(o, RegOp::SetValue { key, .. } if key.contains(r"ZipNest.zip"))));

        // Turning everything off emits removals only.
        let rec2 = Recorder::default();
        let out2 = svc.shell_register(exe, ShellOptions::default(), &rec2).unwrap();
        assert!(!out2.associate && !out2.context_menu);
        assert!(rec2
            .0
            .lock()
            .unwrap()
            .iter()
            .all(|o| matches!(
                o,
                RegOp::DeleteKey { .. }
                    | RegOp::DeleteValue { .. }
                    | RegOp::DeleteValueIfEquals { .. }
            )));
    }

    #[test]
    fn shell_register_survives_partial_denial() {
        let svc = service_with_settings("partial");
        let exe = std::path::Path::new(r"C:\Apps\ZipNest.exe");
        let out = svc
            .shell_register(
                exe,
                ShellOptions { associate: true, context_menu: true },
                &PartialFail,
            )
            .unwrap();
        // Associations and the two allowed menu targets applied; the
        // all-files entry was denied and surfaced as a warning.
        assert!(out.associate);
        assert!(out.context_menu);
        assert_eq!(out.warnings, vec!["error.shell.file_menu".to_string()]);
    }
}
