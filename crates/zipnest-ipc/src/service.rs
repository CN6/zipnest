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

/// `/`-separated form of an archive path, with trailing separators trimmed.
///
/// Borrows when the path already uses `/` — the common case, and the one that
/// matters: this runs over *every* entry of the archive on each listing or
/// extraction, so allocating a `String` per entry was pure churn. Only the
/// Windows-style `\` paths some archivers store are copied.
fn normalized(s: &str) -> std::borrow::Cow<'_, str> {
    let trimmed = s.trim_end_matches(['/', '\\']);
    if trimmed.contains('\\') {
        std::borrow::Cow::Owned(trimmed.replace('\\', "/"))
    } else {
        std::borrow::Cow::Borrowed(trimmed)
    }
}

/// Direct children of `dir` (`""` = root); tolerates trailing-slash forms.
/// Archives may store Windows-style backslash separators (`sub\b.txt`), so
/// matching normalizes to `/` without mutating the stored path (the UI and
/// `read_entry` see the engine's original path verbatim).
///
/// Folders that exist only as a prefix of deeper entries (`dir/sub/file`) are
/// listed too, as synthetic `EntryDto`s. Some archivers write no directory
/// items at all, so without this the root of such an archive looked empty even
/// though extraction produced the files.
fn children_of(entries: &[ArchiveEntry], dir: &str) -> Vec<EntryDto> {
    let dir = normalized(dir);
    // Built once per call. It used to be `format!("{dir}/")` *inside* the
    // filter closure, so it was allocated once per entry instead of once here.
    let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    let mut out: Vec<EntryDto> = Vec::new();
    // Windows treats names case-insensitively; a synthetic folder must not be
    // added next to a real entry for the same folder.
    let mut listed: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut synthetic: Vec<(String, String)> = Vec::new();
    let mut synthetic_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in entries {
        let p = normalized(&e.path);
        let rest = if dir.is_empty() {
            p.as_ref()
        } else {
            match p.strip_prefix(prefix.as_str()) {
                Some(rest) => rest,
                None => continue,
            }
        };
        if rest.is_empty() {
            continue;
        }
        match rest.split_once('/') {
            // A doubled separator (`root//b.txt`) names no folder to show.
            Some(("", _)) => {}
            Some((seg, _)) => {
                let full = if dir.is_empty() { seg.to_string() } else { format!("{dir}/{seg}") };
                if synthetic_seen.insert(full.to_lowercase()) {
                    synthetic.push((full, seg.to_string()));
                }
            }
            None => {
                listed.insert(p.to_lowercase());
                out.push(to_dto(e));
            }
        }
    }
    // A real entry outranks a synthetic folder for the same name, whatever
    // order the archive stores them in (a folder item may follow its files).
    out.extend(
        synthetic
            .into_iter()
            .filter(|(path, _)| !listed.contains(&path.to_lowercase()))
            .map(|(path, name)| EntryDto {
                path,
                name,
                is_dir: true,
                size: 0,
                mtime_ms: None,
                encrypted: false,
            }),
    );
    out
}

/// Entries anywhere in the archive whose *name* contains `query`, ignoring
/// case, capped at `limit`.
///
/// Matches the name only: a hit has to be something the user can recognise in
/// the list, and matching whole paths turned a search for `a` into every file
/// under a folder whose name happened to contain one.
fn search_entries_in(entries: &[ArchiveEntry], query: &str, limit: usize) -> Vec<EntryDto> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for e in entries {
        let dto = to_dto(e);
        // `to_lowercase` allocates; a big archive being searched on every
        // keystroke would otherwise lowercase every name on every frame.
        if dto.name.to_lowercase().contains(&needle) {
            out.push(dto);
            if out.len() == limit {
                break;
            }
        }
    }
    out
}

/// Expand the UI's selection into the engine's index list.
///
/// A selected directory contributes its subtree *and* the directory entry
/// itself, so empty folders land on disk. An empty selection means "the whole
/// archive".
///
/// Membership is decided by walking each entry's own ancestors, not by testing
/// every selected path against every entry. The old nested scan was
/// `O(entries × selection)` — selecting a few thousand siblings and pressing
/// extract turned into billions of string comparisons on the worker thread,
/// before the first progress report, so the progress bar simply sat there.
/// Depth is tiny, so this is linear in entries.
fn select_entries(entries: &[ArchiveEntry], paths: &[String]) -> Vec<u32> {
    let selection: Vec<std::borrow::Cow<'_, str>> = paths.iter().map(|p| normalized(p)).collect();
    if selection.is_empty() || selection.iter().any(|s| s.is_empty()) {
        let mut all: Vec<u32> = entries.iter().map(|e| e.index).collect();
        all.sort_unstable();
        all.dedup();
        return all;
    }

    let selected: std::collections::HashSet<&str> = selection.iter().map(|s| s.as_ref()).collect();
    // A selected path is a directory if an entry names it OR any entry lives
    // beneath it. The second case matters for archives that store no directory
    // items: their folders exist only as path prefixes, and selecting one of
    // those from the listing must still extract its contents.
    let dirs: std::collections::HashSet<String> = {
        let mut dirs = std::collections::HashSet::new();
        for e in entries {
            let path = normalized(&e.path);
            if e.is_dir {
                dirs.insert(path.to_string());
            }
            let mut rest = path.as_ref();
            while let Some(cut) = rest.rfind('/') {
                rest = &rest[..cut];
                dirs.insert(rest.to_string());
            }
        }
        dirs
    };
    let selected_dirs: std::collections::HashSet<&str> = selected
        .iter()
        .copied()
        .filter(|sel| dirs.contains(*sel))
        .collect();

    let mut wanted: Vec<u32> = Vec::new();
    for e in entries {
        let path = normalized(&e.path);
        let mut inside_selected_dir = false;
        let mut rest = path.as_ref();
        while let Some(cut) = rest.rfind('/') {
            rest = &rest[..cut];
            if selected_dirs.contains(rest) {
                inside_selected_dir = true;
                break;
            }
        }
        if inside_selected_dir || selected.contains(path.as_ref()) {
            wanted.push(e.index);
        }
    }
    wanted.sort_unstable();
    wanted.dedup();
    wanted
}

/// `emit(event_name, json_payload)` — thread-safe callback the glue layer
/// wires to Tauri events (tests wire a recorder).
pub type Emit = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

pub struct IpcService {
    /// Exposed read-only for the Tauri glue (resolve an entry for preview).
    pub registry: Arc<ArchiveRegistry>,
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
            registry: Arc::new(ArchiveRegistry::default()),
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

    /// Whether the settings file had to be quarantined at startup, and where the
    /// unreadable original was kept.
    ///
    /// The UI asks once, on the first frame, so the user is told that their
    /// preferences were not read instead of finding every toggle back at its
    /// default with no explanation. `None` on a normal start.
    pub fn corrupt_settings(&self) -> Option<crate::settings::CorruptSettings> {
        self.lock_settings().ok()?.corrupt_settings().cloned()
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
    /// tests never touch the real registry, and Windows' own per-extension
    /// choice is read through `probe`.
    ///
    /// `blocked` in the result names the extensions Windows has already
    /// assigned to another program: those writes succeed but the OS keeps its
    /// locked-in handler, and no program can change that without the user.
    pub fn shell_register(
        &self,
        exe: &std::path::Path,
        opts: crate::shell::ShellOptions,
        applier: &dyn crate::shell::ShellApplier,
        probe: &dyn crate::shell::AssocProbe,
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

        let mut blocked = Vec::new();
        let associate_ok = if opts.associate {
            // The ticked set decides what we claim; "everything we ship a handler
            // for" is the default (installer, first run, fresh profile). An
            // explicitly empty list is a real state: claim nothing, but stay
            // listed in 打开方式 so the user can pick ZipNest by hand.
            let wanted = shell::normalise_extensions(&opts.extensions);
            let wanted: Vec<&str> = wanted.iter().map(|e| e.as_str()).collect();
            let ok = applier.run(&shell::assoc_ops_for(exe, &wanted)).is_ok();
            if !ok {
                warnings.push("error.shell.associate".into());
            }
            blocked = shell::blocked_extensions(probe);
            ok
        } else {
            // `assoc_removals` only clears `HKCU\Software\Classes`; the record
            // Windows actually honours lives in `FileExts\<ext>\UserChoice` and
            // is released by `shell::release_defaults` at the call sites that own
            // that decision (the uninstaller and the settings dialog) — not here,
            // because this layer must stay a pure applier for the tests.
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
            // Whatever happened, the one-time startup bootstrap must not run
            // again: an explicit opt-out has to survive a restart.
            integration_applied: Some(true),
            ..Default::default()
        })?;
        Ok(crate::shell::ShellRegisterResult {
            associate: settings.associate,
            context_menu: settings.context_menu,
            warnings,
            blocked,
        })
    }

    pub fn open_archive(&self, path: String, password: Option<String>) -> Result<OpenArchiveResult, IpcError> {
        let archive = Archive::open(
            std::path::Path::new(&path),
            ArchiveOpenOptions { password: password.clone() },
        )?;
        // The listing is cached on the `Archive` itself: opening already pays
        // for it, and every later navigation/preview/extract reuses it.
        let entries = archive.entries_arc()?;
        let encrypted = entries.iter().any(|e| e.encrypted);
        let format = std::path::Path::new(&path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let root = children_of(&entries, "");
        let (id, _) = self.registry.insert(archive, std::path::PathBuf::from(&path), password);
        Ok(OpenArchiveResult { id, encrypted, format, entries: root })
    }

    /// Release an archive the UI has finished with.
    ///
    /// Every call to [`IpcService::open_archive`] used to pin an archive (7z
    /// handler + source file handle + plaintext password) in the registry for
    /// the rest of the process. The UI presents one archive at a time, so it
    /// closes the previous one here as it opens the next.
    ///
    /// Idempotent, and never fails: an unknown id means it was already closed.
    pub fn close_archive(&self, id: u64) {
        if let Some(shared) = self.registry.remove(id) {
            // Drop the last reference we hold outside the map here rather than
            // at the end of the caller's scope, so the engine is released even
            // if the caller keeps going.
            drop(shared);
        }
    }

    /// How many archives are currently held open.
    ///
    /// The UI keeps one at a time; this exists so the "closing really releases
    /// it" contract can be pinned by a test instead of trusted.
    pub fn open_archive_count(&self) -> usize {
        self.registry.len()
    }

    pub fn list_children(&self, id: u64, dir: String) -> Result<Vec<EntryDto>, IpcError> {
        let shared = self.lock_archive(id)?;
        let guard = shared.lock().unwrap_or_else(|e| e.into_inner());
        let entries = guard.0.entries_arc()?;
        Ok(children_of(&entries, &dir))
    }

    /// Entries anywhere in the archive whose *name* contains `query`
    /// (case-insensitive), capped at `limit`.
    ///
    /// The browser can only show one directory at a time, so a file three
    /// folders down is unfindable by eye in a big archive. This searches the
    /// whole entry list and returns real entry paths, which means a hit can be
    /// selected and extracted without navigating to it first.
    pub fn search_entries(
        &self,
        id: u64,
        query: String,
        limit: usize,
    ) -> Result<Vec<EntryDto>, IpcError> {
        let shared = self.lock_archive(id)?;
        let guard = shared.lock().unwrap_or_else(|e| e.into_inner());
        let entries = guard.0.entries_arc()?;
        Ok(search_entries_in(&entries, &query, limit))
    }

    pub fn read_entry_bytes(&self, id: u64, path: String, max_bytes: u64) -> Result<Vec<u8>, IpcError> {
        let shared = self.lock_archive(id)?;
        // A poisoned lock means a job runner panicked while holding it. The
        // archive itself is still usable, so recover the guard instead of
        // turning one panic into a permanent error for that archive.
        let guard = shared.lock().unwrap_or_else(|e| e.into_inner());
        let entries = guard.0.entries_arc()?;
        let entry = entries
            .iter()
            .find(|e| e.path == path)
            .ok_or_else(|| IpcError::new("error.not_an_archive"))?;
        // Preview reuses the open-time password for encrypted entries.
        let password = self.registry.password(id);
        // Same pre-flight the extract path does: an encrypted entry read with
        // no password at all comes back from the engine as a generic data
        // error, which reads as "operation failed". The key that means "ask for
        // a password" is only useful if we look for the case first.
        if entry.encrypted && password.is_none() {
            return Err(IpcError::new("error.password_required"));
        }
        let opts = ArchiveOpenOptions { password };
        Ok(guard.0.read_entry(entry.index, &opts, Some(max_bytes))?)
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
        self.extract_with_delete(id, paths, dest, on_conflict, password, false)
    }

    /// Same as [`IpcService::extract`], with an optional source deletion after
    /// every success check passes. The handle is released before deleting.
    pub fn extract_with_delete(
        &self,
        id: u64,
        paths: Vec<String>,
        dest: String,
        on_conflict: Option<String>,
        password: Option<String>,
        delete_archive: bool,
    ) -> Result<u64, IpcError> {
        // A relative destination is resolved by the OS against whatever working
        // directory the process was started with — for an Explorer launch that
        // is nowhere the user would look. The archive would be unpacked, the
        // progress bar would say 完成, and the files would be somewhere else
        // entirely. The create side has always required an absolute path; this
        // is the same rule, enforced here so no caller can skip it.
        if !std::path::Path::new(dest.trim()).is_absolute() {
            return Err(IpcError::new("error.io"));
        }
        let shared = self.lock_archive(id)?;
        let source_path = if delete_archive {
            self.registry.path(id).ok_or_else(|| IpcError::new("error.not_an_archive"))?
        } else {
            std::path::PathBuf::new()
        };
        // Callers may omit the password; fall back to the one supplied at
        // open time (session-scoped, memory only).
        let password = password.or_else(|| self.registry.password(id));
        let emit = Arc::clone(&self.emit);
        let registry = Arc::clone(&self.registry);
        let max_total_bytes = match self.settings.lock() {
            Ok(s) => s.get().max_extract_bytes,
            Err(p) => p.into_inner().get().max_extract_bytes,
        };
        let on_conflict = archive_core::OnConflict::from_policy(on_conflict.as_deref());

        let job_id = self.jobs.submit("extract", Box::new(move |ctx| {
            // Poison-tolerant: a panic in an earlier job must not make this
            // archive permanently unusable (the settings lock above already
            // does the same).
            let guard = shared.lock().unwrap_or_else(|e| e.into_inner());
            let entries = guard.0.entries_arc().map_err(|e| e.error_key().to_string())?;

            // Expand the selection: a directory contributes its subtree *and*
            // the directory entry itself (so empty folders survive). An empty
            // selection means "extract the whole archive".
            let wanted = select_entries(&entries, &paths);
            if wanted.is_empty() {
                return Err("error.not_an_archive".into());
            }
            // Deleting the source is only safe once every entry it holds is
            // being extracted; a partial run would lose the rest.
            if delete_archive && wanted.len() < entries.len() {
                return Err("error.delete_partial".into());
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
                // A fatal local I/O error (disk full, device failure) carries
                // the operating system's own words. Pass them along so the
                // failure dialog can say *why*, not just "读写失败".
                if let archive_core::ZipnestError::Io(io) = &e {
                    emit(
                        "extract_failed",
                        serde_json::json!({
                            "count": 0,
                            "detail": io.to_string(),
                            "entries": [],
                        }),
                    );
                }
                key
            })?;
            if !stats.renamed.is_empty() {
                // Weird names were rewritten so the entries could be written at
                // all. Every one of them is on disk; tell the user which name
                // changed instead of letting it look like a different file.
                emit(
                    "extract_renamed",
                    serde_json::json!({
                        "count": stats.renamed.len(),
                        "entries": stats.renamed
                            .iter()
                            .map(|r| serde_json::json!({ "from": r.from, "to": r.to }))
                            .collect::<Vec<_>>(),
                    }),
                );
            }
            if !stats.skipped.is_empty() {
                // Entries the user's own conflict policy left alone (an
                // existing file under "skip"). Deliberate, so this is not a
                // failure — but it is counted out loud.
                emit(
                    "extract_skipped",
                    serde_json::json!({
                        "count": stats.skipped.len(),
                        "names": stats.skipped,
                    }),
                );
            }
            if !stats.failed.is_empty() {
                // Files are missing. This run must NOT come back as success:
                // the UI shows these entries and their reasons in a dialog.
                emit(
                    "extract_failed",
                    serde_json::json!({
                        "count": stats.failed.len(),
                        "detail": serde_json::Value::Null,
                        "entries": stats.failed
                            .iter()
                            .map(|f| {
                                serde_json::json!({
                                    "path": f.path,
                                    "reason": f.reason,
                                    "detail": f.detail,
                                })
                            })
                            .collect::<Vec<_>>(),
                    }),
                );
                return Err("error.entries_failed".into());
            }
            if delete_archive {
                // A skipped entry means the destination already held a file of
                // that name, so the archive's version of it never reached the
                // disk — and the archive is this run's only other copy. Deleting
                // it would lose that content for good.
                if !stats.skipped.is_empty() {
                    return Err("error.delete_skipped".into());
                }
                // Windows cannot unlink a file while Archive owns its read
                // handle, so release both the worker's and registry's refs.
                drop(shared);
                drop(registry.remove(id));
                if let Err(e) = std::fs::remove_file(&source_path) {
                    emit(
                        "extract_failed",
                        serde_json::json!({
                            "count": 0,
                            "detail": e.to_string(),
                            "entries": [],
                        }),
                    );
                    return Err("error.archive_delete".into());
                }
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
    use crate::shell::{AssocProbe, RegOp, ShellApplier, ShellOptions};
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

    /// A machine where Windows has not recorded a choice: our registration is
    /// what Explorer will use.
    struct NoChoice;

    impl AssocProbe for NoChoice {
        fn user_choice_progid(&self, _ext: &str) -> Option<String> {
            None
        }
        fn classes_default(&self, ext: &str) -> Option<String> {
            // Pretend we registered everything, so `registered` is true and the
            // rows differ only by `UserChoice`.
            Some(format!("ZipNest.{ext}"))
        }
    }

    /// A machine where `.zip` is locked to another archiver, the situation the
    /// app cannot fix and has to report.
    struct ZipLockedToAnother;

    impl AssocProbe for ZipLockedToAnother {
        fn user_choice_progid(&self, ext: &str) -> Option<String> {
            (ext == "zip").then(|| "Bandizip.zip".to_string())
        }
        fn classes_default(&self, ext: &str) -> Option<String> {
            Some(format!("ZipNest.{ext}"))
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
            .shell_register(
                exe,
                // The ticked set decides what is claimed; this test wants the
                // usual "all archives" behaviour.
                ShellOptions {
                    associate: true,
                    context_menu: false,
                    extensions: crate::shell::SUPPORTED_EXTENSIONS
                        .iter()
                        .map(|e| (*e).to_string())
                        .collect(),
                },
                &rec,
                &NoChoice,
            )
            .unwrap();
        assert!(out.associate && !out.context_menu);
        assert!(out.blocked.is_empty());
        assert!(rec
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|o| matches!(o, RegOp::SetValue { key, .. } if key.contains(r"ZipNest.zip"))));

        // Turning everything off emits removals only.
        let rec2 = Recorder::default();
        let out2 = svc
            .shell_register(exe, ShellOptions::default(), &rec2, &NoChoice)
            .unwrap();
        assert!(!out2.associate && !out2.context_menu);
        assert!(out2.blocked.is_empty(), "nothing is claimed, so nothing is blocked");
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
    fn shell_register_reports_extensions_windows_will_not_hand_over() {
        // Registering succeeds, `.zip` still opens with the other program, and
        // the UI needs to know which extensions those are.
        let svc = service_with_settings("blocked");
        let exe = std::path::Path::new(r"C:\Apps\ZipNest.exe");
        let rec = Recorder::default();
        let out = svc
            .shell_register(
                exe,
                ShellOptions { associate: true, context_menu: false, extensions: Vec::new() },
                &rec,
                &ZipLockedToAnother,
            )
            .unwrap();
        assert!(out.associate, "the registry writes themselves succeeded");
        assert!(out.warnings.is_empty());
        assert_eq!(out.blocked, vec!["zip".to_string()]);
    }

    #[test]
    fn shell_register_survives_partial_denial() {
        let svc = service_with_settings("partial");
        let exe = std::path::Path::new(r"C:\Apps\ZipNest.exe");
        let out = svc
            .shell_register(
                exe,
                ShellOptions { associate: true, context_menu: true, extensions: Vec::new() },
                &PartialFail,
                &NoChoice,
            )
            .unwrap();
        // Associations and the two allowed menu targets applied; the
        // all-files entry was denied and surfaced as a warning.
        assert!(out.associate);
        assert!(out.context_menu);
        assert_eq!(out.warnings, vec!["error.shell.file_menu".to_string()]);
    }

    #[test]
    fn search_matches_names_anywhere_in_the_archive() {
        let entries = vec![
            entry(0, "readme.txt", false),
            entry(1, "photos/", true),
            entry(2, "photos/holiday.jpg", false),
            entry(3, "photos/README.MD", false),
            entry(4, "docs/notes.txt", false),
        ];
        let hits = |q: &str| -> Vec<String> {
            search_entries_in(&entries, q, 100).into_iter().map(|e| e.path).collect()
        };
        // Case does not matter, and the match is on the name, not the folder:
        // "readme" must not drag in everything under a "readme"-ish folder.
        assert_eq!(hits("README"), vec!["readme.txt", "photos/README.MD"]);
        assert_eq!(hits("  readme  "), vec!["readme.txt", "photos/README.MD"]);
        // A name from a folder the browser is not showing is still found.
        assert_eq!(hits("holiday"), vec!["photos/holiday.jpg"]);
        assert_eq!(hits("photos"), vec!["photos/"]);
        assert!(hits("").is_empty());
        assert!(hits("   ").is_empty());
        assert!(hits("no such file").is_empty());
        // The cap is a hard stop, not a hint.
        assert_eq!(search_entries_in(&entries, "t", 2).len(), 2);
    }

    // --- listing / selection planning -------------------------------------

    fn entry(index: u32, path: &str, is_dir: bool) -> ArchiveEntry {
        ArchiveEntry {
            index,
            path: path.to_string(),
            is_dir,
            size: 0,
            crc: None,
            mtime: None,
            encrypted: false,
        }
    }

    fn listing(entries: &[ArchiveEntry], dir: &str) -> Vec<String> {
        children_of(entries, dir).iter().map(|e| e.path.clone()).collect()
    }

    #[test]
    fn children_of_lists_only_direct_children() {
        let entries = vec![
            entry(0, "a.txt", false),
            entry(1, "sub/", true),
            entry(2, "sub/b.txt", false),
            entry(3, "sub/deep/", true),
            entry(4, "sub/deep/c.txt", false),
        ];
        assert_eq!(listing(&entries, ""), vec!["a.txt", "sub/"]);
        assert_eq!(listing(&entries, "sub"), vec!["sub/b.txt", "sub/deep/"]);
        // The same directory spelled with a trailing slash.
        assert_eq!(listing(&entries, "sub/"), vec!["sub/b.txt", "sub/deep/"]);
        // Archives that store Windows separators still list correctly.
        let backslash = vec![entry(0, r"sub\b.txt", false)];
        assert_eq!(children_of(&backslash, "sub").len(), 1);
    }

    #[test]
    fn implicit_directories_are_listed_and_selectable() {
        // Some archivers store no directory items at all (360压缩箱 does this):
        // folders exist only as prefixes of deeper paths. The root must still
        // list them, and selecting one must extract what is beneath it.
        let entries = vec![
            entry(0, r"root\a.txt", false),
            entry(1, r"root\sub\b.txt", false),
            entry(2, r"root\sub\deep\c.txt", false),
        ];
        let root = children_of(&entries, "");
        assert_eq!(root.len(), 1);
        assert!(root[0].is_dir, "implicit folder must list as a directory");
        assert_eq!(root[0].path, "root");
        assert_eq!(root[0].name, "root");

        let inner = children_of(&entries, "root");
        let listed: Vec<&str> = inner.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(listed, vec!["root\\a.txt", "root/sub"]);
        assert!(inner[1].is_dir);

        // Selecting the synthetic folder takes its whole subtree.
        assert_eq!(select_entries(&entries, &["root/sub".into()]), vec![1, 2]);
    }

    #[test]
    fn implicit_directory_is_not_duplicated_by_a_later_real_dir_entry() {
        // Entry order is whatever the archiver wrote: a real folder item can
        // follow files under it, and then both the synthetic and the real row
        // would name the same folder.
        let entries = vec![
            entry(0, "root/a.txt", false),
            entry(1, "root/sub/b.txt", false),
            entry(2, "root/sub/", true),
        ];
        let dirs: Vec<String> = children_of(&entries, "root")
            .into_iter()
            .filter(|e| e.is_dir)
            .map(|e| normalized(&e.path).into_owned())
            .collect();
        assert_eq!(dirs, vec!["root/sub"]);
    }

    #[test]
    fn select_entries_expands_directories_and_dedupes() {
        let entries = vec![
            entry(0, "a.txt", false),
            entry(1, "sub/", true),
            entry(2, "sub/b.txt", false),
            entry(3, "sub/deep/", true),
            entry(4, "sub/deep/c.txt", false),
            entry(5, "z.txt", false),
        ];
        // A directory contributes itself plus its whole subtree.
        assert_eq!(select_entries(&entries, &["sub".to_string()]), vec![1, 2, 3, 4]);
        // A file contributes exactly itself, never its siblings.
        assert_eq!(select_entries(&entries, &["a.txt".to_string()]), vec![0]);
        // Overlapping picks collapse into one sorted list.
        assert_eq!(
            select_entries(&entries, &["sub".into(), "sub/b.txt".into(), "a.txt".into()]),
            vec![0, 1, 2, 3, 4]
        );
        // A backslash-spelled directory is the same directory.
        assert_eq!(select_entries(&entries, &[r"sub\deep".into()]), vec![3, 4]);
        // An empty selection means "the whole archive" — and so does an empty
        // path inside it.
        assert_eq!(select_entries(&entries, &[]), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(select_entries(&entries, &[String::new()]), vec![0, 1, 2, 3, 4, 5]);
    }

    /// Selecting thousands of siblings used to be `O(entries × selection)`:
    /// 20 000 picks against 20 000 entries is 400 million comparisons on the
    /// worker thread, before the first progress event. This pins the linear
    /// behaviour — and that every pick survives.
    #[test]
    fn select_entries_scales_to_wide_selections() {
        let n = 20_000u32;
        let entries: Vec<ArchiveEntry> =
            (0..n).map(|i| entry(i, &format!("f{i}.txt"), false)).collect();
        let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
        let wanted = select_entries(&entries, &paths);
        assert_eq!(wanted.len(), n as usize);
        assert_eq!(wanted.first(), Some(&0));
        assert_eq!(wanted.last(), Some(&(n - 1)));
    }
}
