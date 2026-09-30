//! Extraction size accounting — zip-bomb guard.

use std::fmt;

/// Total extracted bytes would exceed the configured limit.
#[derive(Debug, PartialEq, Eq)]
pub struct QuotaExceeded;

impl fmt::Display for QuotaExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "extract quota exceeded")
    }
}

impl std::error::Error for QuotaExceeded {}

/// Cumulative byte counter for a single extraction operation.
///
/// A failed [`charge`](ExtractQuota::charge) does NOT consume budget,
/// so callers can safely keep charging subsequent entries until the
/// point where the limit would be crossed.
pub struct ExtractQuota {
    max: u64,
    used: u64,
}

impl ExtractQuota {
    pub fn new(max_bytes: u64) -> Self {
        Self { max: max_bytes, used: 0 }
    }

    /// Charge `bytes` against the budget; error (without charging) if over.
    pub fn charge(&mut self, bytes: u64) -> Result<(), QuotaExceeded> {
        match self.used.checked_add(bytes) {
            Some(total) if total <= self.max => {
                self.used = total;
                Ok(())
            }
            _ => Err(QuotaExceeded),
        }
    }

    pub fn remaining(&self) -> u64 {
        self.max - self.used
    }

    pub fn used(&self) -> u64 {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_within_limit() {
        let mut q = ExtractQuota::new(100);
        assert!(q.charge(60).is_ok());
        assert_eq!(q.remaining(), 40);
        assert!(q.charge(40).is_ok());
        assert_eq!(q.remaining(), 0);
    }

    #[test]
    fn rejects_over_limit() {
        let mut q = ExtractQuota::new(100);
        assert!(q.charge(101).is_err());
        assert!(q.charge(60).is_ok());
        assert!(q.charge(41).is_err());
        assert_eq!(q.remaining(), 40);
    }

    #[test]
    fn zero_limit_blocks_everything() {
        let mut q = ExtractQuota::new(0);
        assert!(q.charge(0).is_ok());
        assert!(q.charge(1).is_err());
    }

    #[test]
    fn charge_overflow_safe() {
        let mut q = ExtractQuota::new(u64::MAX);
        assert!(q.charge(u64::MAX).is_ok());
        assert!(q.charge(1).is_err());
    }
}
