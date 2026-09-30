//! Stable cross-layer error: key only, UI maps key → localized text.

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

/// Shape over the wire: `{"key":"error.*"}`. The key set is the contract
/// with the frontend i18n table — never rename without updating locales.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcError {
    pub key: String,
}

impl IpcError {
    pub fn new(key: &str) -> Self {
        IpcError { key: key.to_string() }
    }
}

impl Serialize for IpcError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("IpcError", 1)?;
        st.serialize_field("key", &self.key)?;
        st.end()
    }
}

impl From<archive_core::ZipnestError> for IpcError {
    fn from(e: archive_core::ZipnestError) -> Self {
        IpcError::new(e.error_key())
    }
}
