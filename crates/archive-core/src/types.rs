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

/// Extraction request (see Task 8).
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub dest: PathBuf,
    pub entries: Vec<u32>,
    pub max_total_bytes: u64,
    pub overwrite: bool,
}

/// Progress snapshot handed to the extract callback.
#[derive(Debug, Clone)]
pub struct ExtractProgress {
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub current_path: String,
}

/// Summary of a completed extraction.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExtractStats {
    pub files: u32,
    pub bytes: u64,
}
