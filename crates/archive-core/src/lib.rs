//! archive-core — process-internal 7z.dll COM wrapper.
//!
//! Public surface:
//! - [`Archive::open`] / [`Archive::len`] / [`Archive::entries`]
//! - [`Archive::read_entry`] / [`Archive::extract`]
//! - [`ZipnestError`] with stable `error_key()` values for the UI layer.

pub mod archive;
pub mod dll;
pub mod error;
pub mod extract;
pub mod types;

/// COM FFI internals — exposed for tooling/probes, not a stable API.
#[doc(hidden)]
pub mod com;

pub use archive::Archive;
pub use error::{map_hresult, ZipnestError};
pub use types::{
    ArchiveEntry, ArchiveOpenOptions, ExtractOptions, ExtractProgress, ExtractStats,
};
