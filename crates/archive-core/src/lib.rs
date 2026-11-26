//! archive-core — process-internal 7z.dll COM wrapper.
//!
//! Public surface:
//! - [`Archive::open`] / [`Archive::len`] / [`Archive::entries`]
//! - [`Archive::read_entry`] / [`Archive::extract`]
//! - [`ZipnestError`] with stable `error_key()` values for the UI layer.

pub mod archive;
pub mod create;
pub mod dll;
pub mod error;
pub mod format;
pub mod extract;
pub mod types;
pub mod update;

/// COM FFI internals — exposed for tooling/probes, not a stable API.
#[doc(hidden)]
pub mod com;

pub use archive::Archive;
pub use create::{collect_sources, collect_sources_detailed, create_archive, CollectedSources};
pub use error::{map_hresult, ZipnestError};
pub use format::{all_extensions, looks_like_archive, Claim, FormatRow, FORMATS};
pub use extract::{conflicts_in, conflicts_on_disk};
pub use update::{update_archive, update_format_for, UpdateFormat, UpdateOptions, UpdatePlan};
pub use types::{
    ArchiveEntry, ArchiveOpenOptions, CompressionLevel, CompressionMethod, CreateFormat,
    CreateOptions, CreateProgress, CreateSource, CreateStats, ExtractOptions, ExtractProgress,
    ExtractStats, ExtractedEditStats, OnConflict, SfxKind, SkippedLink,
};
