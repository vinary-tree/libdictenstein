//! `SyncHandle` — caller-side completion handle for async WAL sync.
//!
//! Split out of the monolithic `wal.rs` (lines ~207-291) as part of the
//! async-writer-cluster decomposition. `SyncHandle` is returned by
//! `AsyncWalWriter::sync_async()` and lets callers either block, check,
//! or timeout-wait on a target LSN's durability.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use super::{AsyncWalError, Lsn, SegmentSyncManager};

/// Handle to track completion of an async sync operation.
///
/// Returned by `AsyncWalWriter::sync_async()`. The caller can either:
/// - Call `wait()` to block until the target LSN is durable
/// - Call `is_synced()` to check status without blocking
/// - Call `wait_timeout()` to wait with a timeout
///
/// # Example
///
/// ```rust,ignore
/// let handle = wal.sync_async()?;
///
/// // Non-blocking check
/// if !handle.is_synced() {
///     // Do other work while sync happens in background
///     process_other_tasks();
/// }
///
/// // Block until durable
/// handle.wait()?;
/// ```
pub struct SyncHandle {
    /// The LSN that must be synced for this handle to be complete.
    target_lsn: Lsn,
    /// Reference to the sync manager for checking/waiting.
    sync_manager: Arc<SegmentSyncManager>,
}

impl SyncHandle {
    /// Create a new sync handle.
    pub(super) fn new(target_lsn: Lsn, sync_manager: Arc<SegmentSyncManager>) -> Self {
        Self {
            target_lsn,
            sync_manager,
        }
    }

    /// Create a handle that is already synced.
    pub(super) fn already_synced(target_lsn: Lsn, sync_manager: Arc<SegmentSyncManager>) -> Self {
        Self {
            target_lsn,
            sync_manager,
        }
    }

    /// Get the target LSN this handle is waiting for.
    pub fn target_lsn(&self) -> Lsn {
        self.target_lsn
    }

    /// Check if the target LSN is now durable (non-blocking).
    pub fn is_synced(&self) -> bool {
        self.sync_manager.global_synced_lsn.load(Ordering::Acquire) >= self.target_lsn
    }

    /// Block until the target LSN is durable.
    ///
    /// # Errors
    ///
    /// Returns `AsyncWalError::SyncThreadPanicked` if the background sync
    /// thread has crashed.
    pub fn wait(&self) -> Result<(), AsyncWalError> {
        self.sync_manager.wait_for_lsn(self.target_lsn)
    }

    /// Block until the target LSN is durable, with timeout.
    ///
    /// # Returns
    ///
    /// - `Ok(true)` if the LSN was synced within the timeout
    /// - `Ok(false)` if the timeout elapsed before sync completed
    /// - `Err(...)` if the sync thread panicked
    pub fn wait_timeout(&self, timeout: Duration) -> Result<bool, AsyncWalError> {
        self.sync_manager.wait_for_lsn_timeout(self.target_lsn, timeout)
    }
}

impl std::fmt::Debug for SyncHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncHandle")
            .field("target_lsn", &self.target_lsn)
            .field("is_synced", &self.is_synced())
            .finish()
    }
}
