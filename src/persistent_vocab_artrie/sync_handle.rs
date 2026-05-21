//! Handle for tracking completion of an async vocabulary sync operation.
//!
//! Split out of `persistent_vocab_artrie::dict_impl` (lines ~81-182) as part
//! of the Phase-6 decomposition.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

/// Handle for tracking completion of an async vocabulary sync operation.
///
/// Returned by [`crate::persistent_vocab_artrie::PersistentVocabARTrie::sync_to_disk_async`].
/// The caller can:
/// - Call `wait()` to block until sync completes
/// - Call `is_synced()` to check status without blocking
/// - Call `wait_timeout()` to wait with a timeout
///
/// # Example
///
/// ```text
/// let handle = vocab.sync_to_disk_async()?;
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
#[derive(Debug)]
pub struct VocabSyncHandle {
    /// Whether the sync has completed.
    completed: Arc<AtomicBool>,
    /// Error message if sync failed.
    error: Arc<Mutex<Option<String>>>,
}

impl VocabSyncHandle {
    /// Create a handle that is already synced (used when no work needed).
    pub(super) fn already_synced() -> Self {
        Self {
            completed: Arc::new(AtomicBool::new(true)),
            error: Arc::new(Mutex::new(None)),
        }
    }

    /// Check if the sync has completed (non-blocking).
    pub fn is_synced(&self) -> bool {
        self.completed.load(Ordering::Acquire)
    }

    /// Block until sync completes.
    ///
    /// # Errors
    ///
    /// Returns an error message if the sync failed.
    pub fn wait(&self) -> std::result::Result<(), String> {
        // Spin-wait with backoff for completion
        let mut backoff_us = 10;
        while !self.completed.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_micros(backoff_us));
            backoff_us = (backoff_us * 2).min(10_000); // Cap at 10ms
        }

        // Check for error
        let error_guard = self.error.lock();
        if let Some(ref e) = *error_guard {
            Err(e.clone())
        } else {
            Ok(())
        }
    }

    /// Block until sync completes, with timeout.
    ///
    /// # Returns
    ///
    /// - `Ok(true)` if sync completed within timeout
    /// - `Ok(false)` if timeout elapsed before sync completed
    /// - `Err(...)` if sync completed with an error
    pub fn wait_timeout(&self, timeout: Duration) -> std::result::Result<bool, String> {
        let start = std::time::Instant::now();
        let mut backoff_us = 10;

        while !self.completed.load(Ordering::Acquire) {
            if start.elapsed() >= timeout {
                return Ok(false); // Timeout
            }
            std::thread::sleep(Duration::from_micros(backoff_us));
            backoff_us = (backoff_us * 2).min(10_000);
        }

        // Check for error
        let error_guard = self.error.lock();
        if let Some(ref e) = *error_guard {
            Err(e.clone())
        } else {
            Ok(true)
        }
    }
}

impl Clone for VocabSyncHandle {
    fn clone(&self) -> Self {
        Self {
            completed: Arc::clone(&self.completed),
            error: Arc::clone(&self.error),
        }
    }
}
