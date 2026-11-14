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

/// 条目的显示名：路径最后一段，忽略结尾的分隔符。借出而不复制，列目录和搜索
/// 都会对每个条目调用它。
fn entry_name(e: &ArchiveEntry) -> &str {
    e.path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&e.path)
}

fn to_dto(e: &ArchiveEntry) -> EntryDto {
    let name = entry_name(e).to_string();
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
///
/// 先用不分配的方式判断命中，命中了才构造 DTO：搜索框每敲一个字都会走一遍全部
/// 条目，从前每条都要 `path.clone()` + `to_lowercase()`，即使绝大多数条目不匹配。
/// `to_lowercase` 仍然只在条目本身带大写时才分配（见 [`contains_ignore_case`]）。
fn search_entries_in(entries: &[ArchiveEntry], query: &str, limit: usize) -> Vec<EntryDto> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for e in entries {
        if contains_ignore_case(entry_name(e), &needle) {
            out.push(to_dto(e));
            if out.len() == limit {
                break;
            }
        }
    }
    out
}

/// `haystack` 是否（忽略大小写）包含已经小写化的 `needle`。
///
/// 全小写的名字——绝大部分下载和打包工具写出来的都是——先在原串上逐字节滑窗比
/// 一遍就能命中，完全不分配；只有带大写的名字才付一次 `to_lowercase`。等价于
/// `haystack.to_lowercase().contains(needle)`，并且 `needle` 必须是已经
/// `trim` + `to_lowercase` 过的。
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        // 与 `contains("")` 一致。`windows(0)` 会 panic，所以必须先挡掉。
        return true;
    }
    if haystack.as_bytes().windows(needle.len()).any(|w| w == needle.as_bytes()) {
        return true;
    }
    if haystack.is_ascii() && !haystack.bytes().any(|b| b.is_ascii_uppercase()) {
        // 纯 ASCII 且全小写：快路径没中，逐字节比较就是最终答案。
        return false;
    }
    // 带大写或带非 ASCII 字符：只有这条路才能保证与 `to_lowercase` 等价
    // （例如 `É` 会小写成 `é`，快路径的逐字节比较看不到）。
    haystack.to_lowercase().contains(needle)
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

/// 创建前把输入展开成进包的条目表，并把「因为是指向别处的链接而没跟进」的
/// 那些项以 `create_skipped` 事件报给界面。
///
/// 两者必须一起处理：只拿条目表的话，用户拖进来的若是个 junction，结果就是一个
/// 空包加一句「完成」。多输入各自相对自己的父目录展开（归档内的名字取条目自己
/// 的 basename），用 `CollectedSources` 的 `Extend` 合并。
///
/// 返回的空表有两种来源，分别给不同的 key：什么都没收到 → `error.io`；收到的
/// 全是指向别处的链接 → `error.create_only_links`（一个用户看得懂、也改得动的
/// 原因，不能笼统地叫 I/O 失败）。事件在返回错误之前发出，界面才说得出是哪些
/// 路径、为什么没打进去。
fn collect_create_sources(
    inputs: &[std::path::PathBuf],
    emit: &Emit,
) -> Result<Vec<archive_core::CreateSource>, String> {
    let mut collected = archive_core::CollectedSources::default();
    for input in inputs {
        let base = input
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let part = archive_core::collect_sources_detailed(std::slice::from_ref(input), &base)
            .map_err(|e| e.error_key().to_string())?;
        collected.extend([part]);
    }
    // emit 是线程安全的，worker 线程上直接发即可（与 extract_renamed 同）。
    if !collected.skipped_links.is_empty() {
        emit(
            "create_skipped",
            serde_json::json!({
                "count": collected.skipped_links.len(),
                "entries": collected.skipped_links
                    .iter()
                    .map(|l| {
                        serde_json::json!({
                            "path": l.path.to_string_lossy(),
                            "is_dir": l.is_dir,
                        })
                    })
                    .collect::<Vec<_>>(),
            }),
        );
    }
    if collected.sources.is_empty() {
        return Err(if collected.skipped_links.is_empty() {
            "error.io"
        } else {
            "error.create_only_links"
        }
        .to_string());
    }
    Ok(collected.sources)
}

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
        let (id, _) = self.registry.insert(
            archive,
            std::path::PathBuf::from(&path),
            password,
            Arc::clone(&entries),
        );
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

    /// 归档里 `dir` 的直接子项（`""` 为根）。
    ///
    /// 只读注册表里那份打开时缓存的条目列表，**不取归档锁**：提取的 worker
    /// 线程会把 `Mutex<SendArchive>` 一直握到解压结束，从前这里跟着阻塞，UI 线程
    /// 上翻一个目录就能把窗口整块冻住，连取消都点不了。条目列表在归档打开后不再
    /// 变化，缓存不会过期。
    pub fn list_children(&self, id: u64, dir: String) -> Result<Vec<EntryDto>, IpcError> {
        let entries = self.cached_entries(id)?;
        Ok(children_of(&entries, &dir))
    }

    /// Entries anywhere in the archive whose *name* contains `query`
    /// (case-insensitive), capped at `limit`.
    ///
    /// The browser can only show one directory at a time, so a file three
    /// folders down is unfindable by eye in a big archive. This searches the
    /// whole entry list and returns real entry paths, which means a hit can be
    /// selected and extracted without navigating to it first.
    ///
    /// 和 [`IpcService::list_children`] 一样只走条目缓存，因此提取进行中也能搜索。
    pub fn search_entries(
        &self,
        id: u64,
        query: String,
        limit: usize,
    ) -> Result<Vec<EntryDto>, IpcError> {
        let entries = self.cached_entries(id)?;
        Ok(search_entries_in(&entries, &query, limit))
    }

    /// 打开时缓存下来的条目列表。归档不存在（或已关闭）时报
    /// `error.not_an_archive`，和 `lock_archive` 的失败语义一致。
    fn cached_entries(&self, id: u64) -> Result<Arc<Vec<ArchiveEntry>>, IpcError> {
        self.registry
            .entries(id)
            .ok_or_else(|| IpcError::new("error.not_an_archive"))
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
    /// The runner holds the archive's mutex for the whole extraction, so the
    /// one command that still waits on it is [`IpcService::read_entry_bytes`]
    /// (previewing an entry while it is being unpacked). Listings and searches
    /// read the cached entry list instead and never block; the UI only ever has
    /// one heavy job running.
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
                // 数据损坏是致命错误，但它同样有「哪些条目没写出来」的名单，
                // 而且这份名单只在这里能拿到 —— 错误本身不经过 `ExtractStats`。
                // 丢掉它，用户就只剩一句「已损坏」，不知道自己少了哪些文件。
                if let archive_core::ZipnestError::Corrupt(entries) = &e {
                    if !entries.is_empty() {
                        emit(
                            "extract_failed",
                            serde_json::json!({
                                "count": entries.len(),
                                "detail": serde_json::Value::Null,
                                "entries": entries
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
                    }
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
    ///
    /// 符号链接与目录联接不跟进（见 `archive_core::SkippedLink`），被跳过的那
    /// 些以 `create_skipped` 事件报给界面；若一个能打包的条目都不剩，这条 job
    /// 以 `error.create_only_links` 失败，而不是交出一个空包。
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

        let emit = Arc::clone(&self.emit);
        let job_id = self.jobs.submit(
            "create",
            Box::new(move |ctx| {
                let engine_sources = collect_create_sources(&inputs, &emit)?;

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
            }),
        );
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

    /// 测试里收集 worker 线程发出的事件：`(事件名, 载荷)`。
    type Events = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    /// 和 [`service_with_settings`] 一样起一个服务，但把 emit 回调留下来，
    /// 好在测试里检查 worker 线程发出去的事件。
    fn service_with_events(name: &str) -> (IpcService, Events) {
        crate::sweep_stale_test_temp("zipnest-svc-");
        let root = std::env::temp_dir().join(format!("zipnest-svc-{}-{name}", std::process::id()));
        let events: Events = Arc::default();
        let sink = Arc::clone(&events);
        let svc = IpcService::with_settings_path(
            Arc::new(move |name: &str, payload: serde_json::Value| {
                sink.lock().unwrap().push((name.to_string(), payload));
            }),
            Duration::ZERO,
            root.join("settings.json"),
        );
        (svc, events)
    }

    /// 等一个叫 `what` 的事件出现（worker 线程是异步的）。返回那一刻的整份
    /// 事件表，让调用方自己再断言细节。
    fn wait_for_event(events: &Events, what: &str) -> Vec<(String, serde_json::Value)> {
        let t0 = std::time::Instant::now();
        loop {
            let snapshot = events.lock().unwrap().clone();
            if snapshot.iter().any(|(n, _)| n == what) {
                return snapshot;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "事件 {what} 没等到");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 「源目录里只有链接」：既要报出 `create_skipped`，又不能交出空包。
    ///
    /// 用 junction 构造正向用例 —— `mklink /J` 不需要 `SeCreateSymbolicLinkPrivilege`，
    /// 所以在普通账户下也能跑。真建不出来（受限环境）就打印一行提示跳过，
    /// 而不是误报失败。
    #[test]
    fn create_with_only_links_fails_loudly_and_reports_them() {
        let (svc, events) = service_with_events("create-links");

        crate::sweep_stale_test_temp("zipnest-links-");
        let base = std::env::temp_dir().join(format!("zipnest-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let src = base.join("src");
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("inside.txt"), b"inside").unwrap();
        // 相对路径 + 在链接的父目录里执行（与 archive-core 的 junction 测试同一
        // 写法）：绝对路径经 MSYS 转换后 mklink 会报「找不到路径」。
        let made = std::process::Command::new("cmd")
            .current_dir(&src)
            .args(["/c", "mklink", "/J", "only", "..\\elsewhere"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !made {
            eprintln!("note: 这个环境建不出 junction，只有链接的用例跳过");
            let _ = std::fs::remove_dir_all(&base);
            return;
        }

        let dest = base.join("out.zip");
        let job = svc
            .create_archive(
                vec![src.join("only").to_string_lossy().into()],
                dest.to_string_lossy().into(),
                serde_json::from_value(
                    serde_json::json!({"format":"zip","level":"normal","method":"auto"}),
                )
                .unwrap(),
            )
            .unwrap();
        let snapshot = wait_for_event(&events, "job_finished");

        // 事件在失败之前发出，且带得回路径与 is_dir。
        let (_, skipped) = snapshot
            .iter()
            .find(|(n, _)| n == "create_skipped")
            .expect("只装着链接时必须发 create_skipped");
        assert_eq!(skipped["count"], 1);
        let entry = &skipped["entries"][0];
        assert!(
            entry["path"].as_str().unwrap_or_default().ends_with("only"),
            "事件里要带上被跳过的路径，实际 {:?}",
            entry
        );
        assert_eq!(entry["is_dir"], true, "junction 指向目录");

        // 不是 I/O 失败，也不是「完成」：给的是一个说得清原因的 key。
        let (_, fin) = snapshot
            .iter()
            .find(|(n, e)| n == "job_finished" && e["job_id"] == job)
            .expect("job_finished 必须发出");
        assert_eq!(fin["ok"], false);
        assert_eq!(
            fin["error_key"].as_str(),
            Some("error.create_only_links"),
            "只有链接时要报出专门的原因，实际 {:?}",
            fin
        );
        assert!(!dest.exists(), "没有可打包的条目就不该留下一个空包");

        // 第二种输入形态：选中的是「只装着那个 junction 的文件夹」本身。文件夹
        // 自己的条目能进包，所以这条 job 是成功的 —— 但它必须照样报出跳过项，
        // 否则用户会以为 junction 后面的东西已经打进去了。
        events.lock().unwrap().clear();
        let dest2 = base.join("out2.zip");
        let job2 = svc
            .create_archive(
                vec![src.to_string_lossy().into()],
                dest2.to_string_lossy().into(),
                serde_json::from_value(
                    serde_json::json!({"format":"zip","level":"normal","method":"auto"}),
                )
                .unwrap(),
            )
            .unwrap();
        let snapshot = wait_for_event(&events, "job_finished");
        let (_, skipped) = snapshot
            .iter()
            .find(|(n, _)| n == "create_skipped")
            .expect("文件夹里只有链接时同样要发 create_skipped");
        assert_eq!(skipped["count"], 1);
        let (_, fin) = snapshot
            .iter()
            .find(|(n, e)| n == "job_finished" && e["job_id"] == job2)
            .expect("job_finished 必须发出");
        assert_eq!(fin["ok"], true, "文件夹本身是可打包的，这条应当成功");
        assert!(dest2.exists());

        let _ = std::fs::remove_dir_all(&base);
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

    #[test]
    fn case_insensitive_match_matches_lowercase_contains() {
        // 快路径（纯 ASCII、已经小写）与慢路径（带大写或非 ASCII）必须与
        // `to_lowercase().contains` 完全一致，否则搜索会随文件名的写法悄悄
        // 改变结果。
        let cases = [
            ("readme.txt", "readme", true),
            ("README.TXT", "readme", true),
            ("readme.txt", "me.t", true),
            ("readme.txt", "readme.txt", true),
            ("readme.txt", "readme.txtx", false),
            ("readme.txt", "", true),
            ("Été.txt", "été", true),
            ("ÉTÉ.TXT", "été", true),
            ("naïve.txt", "naïve", true),
            ("photo.JPG", "photo.jpg", true),
            ("photo.jpg", "photo.JPG", false), // needle 永远是小写化的
            ("中文名字.txt", "中文", true),
        ];
        for (hay, needle, want) in cases {
            assert_eq!(
                contains_ignore_case(hay, needle),
                want,
                "{hay:?} 里找 {needle:?}"
            );
            // 与直接小写再 contains 的参考实现对照。
            assert_eq!(
                contains_ignore_case(hay, needle),
                hay.to_lowercase().contains(needle),
                "与参考实现不一致：{hay:?} / {needle:?}"
            );
        }
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
