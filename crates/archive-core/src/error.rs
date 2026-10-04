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
    /// A password was requested for a format that cannot encrypt (TAR family).
    PasswordUnsupported,
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
            ZipnestError::PasswordUnsupported => "error.password_unsupported",
            ZipnestError::Cancelled => "error.cancelled",
            ZipnestError::Engine(_) => "error.engine",
            ZipnestError::Security(_) => "error.security_blocked",
            ZipnestError::QuotaExceeded => "error.quota_exceeded",
            ZipnestError::Io(_) => "error.io",
        }
    }

    /// Alias for [`Self::error_key`] — errors are not `Clone`, so callers
    /// compare by stable kind instead of by value.
    pub fn kind(&self) -> &'static str {
        self.error_key()
    }
}

impl fmt::Display for ZipnestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // NOTE: never include secrets here.
        match self {
            ZipnestError::DllMissing(p) => write!(f, "7z.dll not found (tried: {p})"),
            ZipnestError::Engine(hr) => write!(f, "engine error HRESULT=0x{:08X}", *hr as u32),
            ZipnestError::PasswordUnsupported => {
                write!(f, "this archive format does not support a password")
            }
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

/// `HRESULT_FROM_WIN32(ERROR_NEGATIVE_SEEK)` — the code the `IStream` contract
/// recommends for a seek to a position before the start of the stream
/// (`vendor/7zip-sdk/IStream.h`).
pub const E_NEGATIVE_SEEK: i32 = 0x8007_0083u32 as i32;

/// Is this HRESULT a failure?
///
/// Every non-negative value is success: `S_OK` (0) *and* `S_FALSE` (1). 7-Zip
/// uses `S_FALSE` for "finished, but not everything succeeded" on several
/// interfaces (`IArchiveExtractCallback::GetStream` reports a decoder data
/// error that way, `IArchiveOpenVolumeCallback::GetStream` reports "no such
/// volume"), so `hr != S_OK` must never be the failure test for those.
///
/// Contracts that strictly require `S_OK` keep hard-coding it; do not route
/// them through this helper. The ones that matter here:
/// `IUnknown::QueryInterface`, and the `Read`/`Write`/`Seek` results we return
/// ourselves (`vendor/7zip-sdk/IStream.h` allows only `S_OK` or an error).
pub fn hr_failed(hr: i32) -> bool {
    hr < 0
}

/// Resolve a seek `offset` against `base` (0 for `SeekFrom::Start`, the current
/// position for `Current`, the stream length for `End`).
///
/// `None` means the result is negative or overflows [`u64`]; the stream
/// implementations turn that into [`E_NEGATIVE_SEEK`] instead of wrapping a
/// negative offset into a ~1.8e19 forward seek.
pub(crate) fn seek_target_checked(base: u64, offset: i64) -> Option<u64> {
    if offset >= 0 {
        base.checked_add(offset as u64)
    } else {
        base.checked_sub(offset.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hr_failed_treats_s_false_as_success() {
        assert!(!hr_failed(0)); // S_OK
        assert!(!hr_failed(1)); // S_FALSE
        assert!(hr_failed(E_NEGATIVE_SEEK));
        assert!(hr_failed(0x8000_4004u32 as i32)); // E_ABORT
    }

    #[test]
    fn seek_target_checked_rejects_negative_and_overflow() {
        assert_eq!(seek_target_checked(0, 0), Some(0));
        assert_eq!(seek_target_checked(0, 7), Some(7));
        assert_eq!(seek_target_checked(10, -4), Some(6));
        // Before the start of the stream: must be rejected, never wrapped.
        assert_eq!(seek_target_checked(10, -11), None);
        assert_eq!(seek_target_checked(0, -1), None);
        assert_eq!(seek_target_checked(0, i64::MIN), None);
        assert_eq!(seek_target_checked(5, i64::MIN), None);
        // Overflow past the end of the addressable range.
        assert_eq!(seek_target_checked(u64::MAX, 1), None);
        assert_eq!(seek_target_checked(u64::MAX, i64::MAX), None);
    }

    #[test]
    fn map_hresult_keeps_its_contract() {
        assert_eq!(
            map_hresult(0x8000_4004u32 as i32).error_key(),
            "error.cancelled"
        );
        assert_eq!(map_hresult(0x8000_4005u32 as i32).error_key(), "error.engine");
        // S_FALSE is not a failure, but it is also not a mapped error kind.
        assert_eq!(map_hresult(1).error_key(), "error.engine");
    }
}
