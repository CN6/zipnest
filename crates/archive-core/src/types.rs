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

/// Summary of a completed extraction.
#[derive(Debug, Default, Clone)]
pub struct ExtractStats {
    pub files: u32,
    pub bytes: u64,
    /// Archive paths that were not written: unsafe names (path escape, Windows
    /// reserved name, …), entries whose destination already existed under the
    /// `skip` policy, and entries whose destination could not be created.
    /// Everything else still extracts; the caller surfaces these so nothing
    /// looks silently lost.
    pub skipped: Vec<String>,
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
}
