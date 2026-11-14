//! Public value types returned by the engine.

use std::fmt;
use std::path::PathBuf;

/// Options accepted by [`crate::Archive::open`].
///
/// The password (if any) is held only in memory and never logged or
/// included in `Debug` output.
#[derive(Default)]
pub struct ArchiveOpenOptions {
    pub password: Option<String>,
}

impl fmt::Debug for ArchiveOpenOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArchiveOpenOptions")
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// One entry inside an archive (see Task 6 for construction).
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    pub index: u32,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub crc: Option<u32>,
    pub mtime: Option<std::time::SystemTime>,
    pub encrypted: bool,
}

/// What to do when the destination already holds a file with the entry's name.
///
/// The user-facing `"ask"` policy is resolved by the UI (it is the dialog
/// itself) into one of these before the engine is called: the engine never
/// prompts. Parsing lives in [`OnConflict::from_policy`] so the wire strings
/// stay in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnConflict {
    /// Replace the existing file. The replacement only happens after the new
    /// entry has been written completely, so a failed or cancelled run cannot
    /// truncate the file that was there before.
    #[default]
    Overwrite,
    /// Leave the existing file untouched; the entry is reported as skipped.
    Skip,
    /// Write next to the existing file as `name (2).ext`, `name (3).ext`, …
    Rename,
}

impl OnConflict {
    /// Map a settings/IPC policy string (`"overwrite" | "skip" | "rename"`,
    /// `"ask"`, `None`, or anything unknown) onto engine behavior. Unknown
    /// values fall back to the safe default rather than failing the call.
    pub fn from_policy(policy: Option<&str>) -> Self {
        match policy.map(|p| p.trim().to_ascii_lowercase()) {
            Some(ref p) if p == "skip" => OnConflict::Skip,
            Some(ref p) if p == "rename" => OnConflict::Rename,
            _ => OnConflict::Overwrite,
        }
    }
}

/// Extraction request (see Task 8).
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub dest: PathBuf,
    pub entries: Vec<u32>,
    pub max_total_bytes: u64,
    pub on_conflict: OnConflict,
}

/// Progress snapshot handed to the extract callback.
#[derive(Debug, Clone)]
pub struct ExtractProgress {
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub current_path: String,
}

/// An entry that was written under a different name because the archived name
/// is not usable on this filesystem (device name, illegal characters, `..`,
/// over-long component, a collision after cleaning). The content is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamedEntry {
    /// The name as the archive carried it.
    pub from: String,
    /// The name it was written as, relative to the destination.
    pub to: String,
}

/// An entry that could **not** be written at all.
///
/// A non-empty `failed` list means the extraction did *not* succeed: the caller
/// must report a failure, never a success, because files are missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryFailure {
    /// The name as the archive carried it.
    pub path: String,
    /// Stable reason key (`error.*`) for the UI.
    pub reason: &'static str,
    /// The operating system's own words, when it gave any (never localized).
    pub detail: Option<String>,
}

/// Summary of a completed extraction.
#[derive(Debug, Default, Clone)]
pub struct ExtractStats {
    pub files: u32,
    pub bytes: u64,
    /// Entries the user's own conflict policy asked to leave alone (an existing
    /// destination under `OnConflict::Skip`, or one that cannot be renamed
    /// because the policy forbids overwriting). Deliberate, not a defect.
    pub skipped: Vec<String>,
    /// Entries that were written under a changed name. Everything in here is on
    /// disk with complete content.
    pub renamed: Vec<RenamedEntry>,
    /// Entries that are *not* on disk. Non-empty ⇒ the run failed.
    pub failed: Vec<EntryFailure>,
}

// ---------------------------------------------------------------------------
// Archive creation (Task 1 / M3).
// ---------------------------------------------------------------------------

/// Output container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateFormat {
    Zip,
    SevenZ,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
}

/// Compression effort. `engine_x()` is the numeric value handed to 7-Zip's
/// `x` property (0..9) via `ISetProperties` (a `VT_UI4`, as the engine's
/// `ParsePropToUInt32` requires — a `VT_BSTR` is rejected with `E_INVALIDARG`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionLevel {
    Store,
    Fastest,
    Normal,
    Maximum,
    Ultra,
}

impl CompressionLevel {
    pub fn engine_x(&self) -> u32 {
        match self {
            CompressionLevel::Store => 0,
            CompressionLevel::Fastest => 1,
            CompressionLevel::Normal => 5,
            CompressionLevel::Maximum => 7,
            CompressionLevel::Ultra => 9,
        }
    }
}

/// Per-format compression method. `Auto` leaves the choice to the handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionMethod {
    Auto,
    Copy,
    Deflate,
    Lzma2,
    Bzip2,
}

impl CompressionMethod {
    /// 7-Zip method name (`m` property), or `None` for `Auto`.
    pub fn engine_name(&self) -> Option<&'static str> {
        match self {
            CompressionMethod::Auto => None,
            CompressionMethod::Copy => Some("Copy"),
            CompressionMethod::Deflate => Some("Deflate"),
            CompressionMethod::Lzma2 => Some("LZMA2"),
            CompressionMethod::Bzip2 => Some("BZip2"),
        }
    }
}

/// Self-extracting archive stub flavor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SfxKind {
    Gui,
    Console,
}

/// Options for [`crate::create::create_archive`].
///
/// The password (if any) is held only in memory; `Debug` redacts it and it is
/// never logged.
#[derive(Clone)]
pub struct CreateOptions {
    pub format: CreateFormat,
    pub level: CompressionLevel,
    pub method: CompressionMethod,
    pub password: Option<String>,
    pub encrypt_names: bool,
    pub volume_bytes: Option<u64>,
    pub sfx: Option<SfxKind>,
}

impl fmt::Debug for CreateOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateOptions")
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

/// One input to a create operation: an on-disk path plus the node name it
/// should carry inside the archive.
#[derive(Debug, Clone)]
pub struct CreateSource {
    pub path: PathBuf,
    pub node: String,
}

/// 创建时被跳过的符号链接 / 目录联接（junction）。
///
/// 打包只跟随真实目录树，不跟随链接：跟随会让同一个文件在包里出现两次
/// （链接 + 目标），也可能把用户没选的目录卷进来。跳过是既定策略，但必须
/// 让对方看得见 —— 否则一个只装着链接的目录会得到「完成」，而实际什么都没
/// 进包。进程内的 junction 在 `symlink_metadata` 里同样算 symlink，所以它
/// 连同整棵子树都会被记在这里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedLink {
    /// 链接本身的路径（与传给 `collect_sources` 的输入同一形式）。
    pub path: PathBuf,
    /// 链接指向目录（目录符号链接 / junction）时为 `true`。
    ///
    /// 由跟随链接后的元数据判定；悬空链接读不到目标，按文件计。
    pub is_dir: bool,
}

/// Progress snapshot handed to the create callback.
#[derive(Debug, Clone)]
pub struct CreateProgress {
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub current_path: String,
}

/// Summary of a completed archive creation.
#[derive(Debug, Default, Clone, Copy)]
pub struct CreateStats {
    pub files: u32,
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// 分卷覆盖后删不掉的旧卷备份数（`<dest>.old-<tag>.NNN`）。
    ///
    /// 备份删除是收尾的 best-effort：文件被别的程序占住时删不掉，但**新卷已经
    /// 就位、旧内容也还在磁盘上**，所以这不是失败。非零时调用方应当如实告诉
    /// 用户「旧卷留在 `<name>.old-…` 里，可以手动删」，而不是一声不吭。
    pub kept_backup_files: u32,
}
