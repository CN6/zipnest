//! Extraction paths: `read_entry` (Task 7) and `extract_to_disk` (Task 8).
//!
//! T5 scaffold: open/list only; bodies are implemented by later tasks.

use crate::archive::Archive;
use crate::error::ZipnestError;
use crate::types::{ExtractOptions, ExtractProgress, ExtractStats};
use crate::com::E_NOTIMPL;

pub(crate) fn read_entry(
    _arc: &Archive,
    _index: u32,
    _max_bytes: Option<u64>,
) -> Result<Vec<u8>, ZipnestError> {
    Err(ZipnestError::Engine(E_NOTIMPL))
}

pub(crate) fn extract_to_disk(
    _arc: &Archive,
    _opts: &ExtractOptions,
    _password: Option<&str>,
    _progress: &mut dyn FnMut(&ExtractProgress) -> bool,
) -> Result<ExtractStats, ZipnestError> {
    Err(ZipnestError::Engine(E_NOTIMPL))
}
