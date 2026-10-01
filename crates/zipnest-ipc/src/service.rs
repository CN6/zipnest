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
    pub fn extract(
        &self,
        id: u64,
        paths: Vec<String>,
        dest: String,
        overwrite: bool,
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

        let job_id = self.jobs.submit("extract", Box::new(move |ctx| {
            let guard = shared.lock().map_err(|_| "error.engine".to_string())?;
            let entries = guard.0.entries().map_err(|e| e.error_key().to_string())?;

            // Expand the selection: directories contribute all nested files.
            let sel_norm: Vec<String> = paths.iter().map(|p| p.replace('\\', "/")).collect();
            let mut wanted: Vec<u32> = Vec::new();
            for sel in &sel_norm {
                let sel = sel.trim_end_matches('/');
                let is_dir = entries
                    .iter()
                    .any(|e| e.path.replace('\\', "/").trim_end_matches('/') == sel && e.is_dir);
                for e in &entries {
                    let p = e.path.replace('\\', "/");
                    let p = p.trim_end_matches('/');
                    let hit = if is_dir {
                        p.starts_with(&format!("{sel}/"))
                    } else {
                        p == sel
                    };
                    if hit && !e.is_dir && !wanted.contains(&e.index) {
                        wanted.push(e.index);
                    }
                }
            }
            if wanted.is_empty() {
                return Err("error.not_an_archive".into());
            }
            // Encrypted entries with no password anywhere (argument or
            // open-time): fail with a re-promptable key instead of letting
            // the engine surface an opaque data error.
            let needs_password = entries
                .iter()
                .filter(|e| wanted.contains(&e.index))
                .any(|e| e.encrypted);
            if needs_password && password.is_none() {
                return Err("error.password_required".into());
            }
            let total_items = wanted.len() as u64;
            let total_bytes: u64 = entries
                .iter()
                .filter(|e| wanted.contains(&e.index))
                .map(|e| e.size)
                .sum();

            let opts = archive_core::ExtractOptions {
                dest: std::path::PathBuf::from(&dest),
                entries: wanted,
                // No quota in M2; the security layer still sanitizes paths.
                max_total_bytes: u64::MAX,
                overwrite,
            };
            let mut seen_paths = std::collections::HashSet::new();
            let result = guard.0.extract(&opts, password.as_deref(), &mut |d| {
                if ctx.cancelled() {
                    return false;
                }
                // `ExtractProgress` has no item counter; count distinct
                // paths observed as an approximation of files touched.
                seen_paths.insert(d.current_path.clone());
                ctx.report(
                    seen_paths.len() as u64,
                    total_items,
                    d.done_bytes,
                    total_bytes,
                );
                true
            });
            drop(guard);
            result.map_err(|e| {
                let key = e.error_key().to_string();
                if key == "error.password_required" {
                    emit(
                        "password_required",
                        serde_json::json!({ "archive_id": id }),
                    );
                }
                key
            })?;
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
            archive_core::create_archive(&engine_sources, &dest_path, &opts, &mut |p| {
                if ctx.cancelled() {
                    return false;
                }
                seen.insert(p.current_path.clone());
                ctx.report(seen.len() as u64, total_items, p.done_bytes, p.total_bytes);
                true
            })
            .map_err(|e| e.error_key().to_string())?;
            Ok(())
        }));
        Ok(job_id)
    }
}
