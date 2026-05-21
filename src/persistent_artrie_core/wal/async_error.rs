//! Error types specific to the async / background-sync WAL writer.
//!
//! Split out of the monolithic `wal.rs` (lines ~1642-1717) as part of the
//! Phase-4 wal decomposition. Pairs with `wal::error::WalError` (sync
//! errors); `AsyncWalError` carries `From<WalError>` so synchronous errors
//! lift naturally into the async surface.

use std::io;
use std::path::PathBuf;

use super::error::WalError;
use super::Lsn;

/// Error types specific to async WAL operations.
#[derive(Debug)]
pub enum AsyncWalError {
    /// Underlying WAL error.
    Wal(WalError),
    /// Segment sync failed after retries.
    SegmentSyncFailed {
        path: PathBuf,
        attempts: u32,
        last_error: io::Error,
    },
    /// Rotation failed.
    RotationFailed {
        reason: String,
        source: Option<io::Error>,
    },
    /// Sync wait timed out.
    SyncTimeout {
        target_lsn: Lsn,
        current_synced: Lsn,
        timeout_ms: u64,
    },
    /// Background sync thread panicked.
    SyncThreadPanicked,
}

impl std::fmt::Display for AsyncWalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AsyncWalError::Wal(e) => write!(f, "WAL error: {}", e),
            AsyncWalError::SegmentSyncFailed {
                path,
                attempts,
                last_error,
            } => {
                write!(
                    f,
                    "Segment sync failed after {} attempts at {}: {}",
                    attempts,
                    path.display(),
                    last_error
                )
            }
            AsyncWalError::RotationFailed { reason, source } => {
                if let Some(e) = source {
                    write!(f, "Rotation failed ({}): {}", reason, e)
                } else {
                    write!(f, "Rotation failed: {}", reason)
                }
            }
            AsyncWalError::SyncTimeout {
                target_lsn,
                current_synced,
                timeout_ms,
            } => {
                write!(
                    f,
                    "Sync timeout: target LSN {} not reached (current synced: {}) after {}ms",
                    target_lsn, current_synced, timeout_ms
                )
            }
            AsyncWalError::SyncThreadPanicked => {
                write!(f, "Background sync thread panicked")
            }
        }
    }
}

impl std::error::Error for AsyncWalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AsyncWalError::Wal(e) => Some(e),
            AsyncWalError::SegmentSyncFailed { last_error, .. } => Some(last_error),
            AsyncWalError::RotationFailed {
                source: Some(e), ..
            } => Some(e),
            _ => None,
        }
    }
}

impl From<WalError> for AsyncWalError {
    fn from(e: WalError) -> Self {
        AsyncWalError::Wal(e)
    }
}
