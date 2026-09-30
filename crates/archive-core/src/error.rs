//! Unified error type for archive operations.

use archive_security::SecurityViolation;
use std::fmt;

/// Every error that crosses the crate boundary.
///
/// `error_key()` returns a stable `"error.*"` constant for the UI layer.
/// Passwords are never stored here.
#[derive(Debug)]
#[non_exhaustive]
pub enum ZipnestError {
    /// 7z.dll could not be located/loaded; carries the tried paths.
    DllMissing(String),
    /// Archive signature/header check failed.
    NotAnArchive,
    /// Encryption present, no password supplied.
    PasswordRequired,
    /// Password supplied but rejected.
    PasswordIncorrect,
    /// User cancelled a progress-reporting operation.
    Cancelled,
    /// Raw engine HRESULT (see `map_hresult`).
    Engine(i32),
    /// Entry path failed sanitization (zip-slip etc.).
    Security(SecurityViolation),
    /// Extracted bytes exceeded the configured quota.
    QuotaExceeded,
    /// Local filesystem error.
    Io(std::io::Error),
}

impl ZipnestError {
    /// Stable machine-readable key for the UI/i18n layer.
    pub fn error_key(&self) -> &'static str {
        match self {
            ZipnestError::DllMissing(_) => "error.dll_missing",
            ZipnestError::NotAnArchive => "error.not_an_archive",
            ZipnestError::PasswordRequired => "error.password_required",
            ZipnestError::PasswordIncorrect => "error.password_incorrect",
            ZipnestError::Cancelled => "error.cancelled",
            ZipnestError::Engine(_) => "error.engine",
            ZipnestError::Security(_) => "error.security_blocked",
            ZipnestError::QuotaExceeded => "error.quota_exceeded",
            ZipnestError::Io(_) => "error.io",
        }
    }
}

impl fmt::Display for ZipnestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // NOTE: never include secrets here.
        match self {
            ZipnestError::DllMissing(p) => write!(f, "7z.dll not found (tried: {p})"),
            ZipnestError::Engine(hr) => write!(f, "engine error HRESULT=0x{:08X}", *hr as u32),
            other => write!(f, "{}", other.error_key()),
        }
    }
}

impl std::error::Error for ZipnestError {}

impl From<std::io::Error> for ZipnestError {
    fn from(e: std::io::Error) -> Self {
        ZipnestError::Io(e)
    }
}

impl From<SecurityViolation> for ZipnestError {
    fn from(v: SecurityViolation) -> Self {
        ZipnestError::Security(v)
    }
}

/// Map a raw HRESULT to a [`ZipnestError`].
///
/// Central choke point for all engine HRESULTs (see vendor notes §7).
pub fn map_hresult(hr: i32) -> ZipnestError {
    match hr as u32 {
        0x8000_4004 => ZipnestError::Cancelled, // E_ABORT
        _ => ZipnestError::Engine(hr),
    }
}
