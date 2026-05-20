//! Public read-path API for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~278-468, ~191 LOC)
//! as a Phase-6 char sub-module. Methods covered:
//!
//! - `contains` / `try_contains` / `get` / `try_get` — fail-fast read path
//! - `contains_optimistic` / `try_contains_optimistic` /
//!   `get_optimistic` / `try_get_optimistic` — optimistic concurrency
//!   variants with bounded retry on epoch-version conflicts
//! - `enter_epoch` / `current_epoch` / `advance_epoch` / `active_readers`
//!   — epoch-based reclamation interface
//! - `retry_stats_snapshot` / `is_write_locked` / `current_version`
//!   — observability accessors

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::concurrency::{EpochGuard, OptimisticReadGuard};
use crate::persistent_artrie::error::Result;
use crate::value::DictionaryValue;

use super::types::CharTrieRoot;

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    ///
    /// For persistent tries with lazy loading, this will load nodes on-demand.
    /// I/O errors during lazy loading will cause a panic. Use `try_contains()`
    /// for explicit error handling.
    pub fn contains(&self, term: &str) -> bool {
        {
            self.try_contains(term)
                .expect("I/O error during lazy loading in contains()")
        }
    }

    /// Check if a term exists in the trie with explicit error handling.
    ///
    /// This version returns a `Result` for lazy loading I/O errors.
    /// For disk-backed tries, prefetches children at each level for improved I/O performance.
    pub fn try_contains(&self, term: &str) -> Result<bool> {
        let root = match &self.root {
            CharTrieRoot::Node(node) => node.as_ref(),
            CharTrieRoot::Empty => return Ok(false),
        };

        let mut current = root;
        let mut depth = 0u16;
        for c in term.chars() {
            // Prefetch siblings before descending (multi-level prefetch)
            self.prefetch_disk_refs_bounded(current.node.iter_children(), depth);

            match self.get_child_lazy(current, c)? {
                Some(child) => {
                    current = child;
                    depth = depth.saturating_add(1);
                }
                None => return Ok(false),
            }
        }

        Ok(current.is_final())
    }

    /// Get a value by term
    ///
    /// For persistent tries with lazy loading, this will load nodes on-demand.
    /// I/O errors during lazy loading will cause a panic. Use `try_get()`
    /// for explicit error handling.
    pub fn get(&self, term: &str) -> Option<&V> {
        {
            self.try_get(term)
                .expect("I/O error during lazy loading in get()")
        }
    }

    /// Get a value by term with explicit error handling.
    ///
    /// This version returns a `Result` for lazy loading I/O errors.
    /// For disk-backed tries, prefetches children at each level for improved I/O performance.
    pub fn try_get(&self, term: &str) -> Result<Option<&V>> {
        let root = match &self.root {
            CharTrieRoot::Node(node) => node.as_ref(),
            CharTrieRoot::Empty => return Ok(None),
        };

        let mut current = root;
        let mut depth = 0u16;
        for c in term.chars() {
            // Prefetch siblings before descending (multi-level prefetch)
            self.prefetch_disk_refs_bounded(current.node.iter_children(), depth);

            match self.get_child_lazy(current, c)? {
                Some(child) => {
                    current = child;
                    depth = depth.saturating_add(1);
                }
                None => return Ok(None),
            }
        }

        if current.is_final() {
            Ok(current.value.as_ref())
        } else {
            Ok(None)
        }
    }

    // ==================== Optimistic Concurrency Methods ====================

    /// Try an optimistic read for contains.
    ///
    /// Returns `Some(result)` if the read was consistent, `None` if a concurrent
    /// write occurred and the read should be retried.
    pub fn try_contains_optimistic(&self, term: &str) -> Option<bool> {
        // Record the version before reading
        let guard = OptimisticReadGuard::new(&self.version);

        // Perform the read
        let result = self.contains(term);

        // Validate the version - if it changed, return None to signal retry
        if guard.validate() {
            Some(result)
        } else {
            None
        }
    }

    /// Optimistic contains with automatic retry.
    ///
    /// Retries up to `max_retries` times if concurrent writes occur.
    /// Returns the result if successful within retry limit.
    pub fn contains_optimistic(&self, term: &str, max_retries: usize) -> Option<bool> {
        let mut retries = 0u64;
        for _ in 0..max_retries {
            if let Some(result) = self.try_contains_optimistic(term) {
                self.retry_stats.record_success(retries);
                return Some(result);
            }
            retries += 1;
            std::hint::spin_loop();
        }
        None
    }

    /// Try an optimistic read for get.
    ///
    /// Returns `Some(result)` if the read was consistent, `None` if retry needed.
    /// Note: Returns Option<Option<V>> - outer Option for consistency, inner for value.
    pub fn try_get_optimistic(&self, term: &str) -> Option<Option<V>> {
        let guard = OptimisticReadGuard::new(&self.version);

        // Clone the value if found (to avoid holding reference during validation)
        let result = self.get(term).cloned();

        if guard.validate() {
            Some(result)
        } else {
            None
        }
    }

    /// Optimistic get with automatic retry.
    pub fn get_optimistic(&self, term: &str, max_retries: usize) -> Option<Option<V>> {
        let mut retries = 0u64;
        for _ in 0..max_retries {
            if let Some(result) = self.try_get_optimistic(term) {
                self.retry_stats.record_success(retries);
                return Some(result);
            }
            retries += 1;
            std::hint::spin_loop();
        }
        None
    }

    /// Enter an epoch-protected read section.
    ///
    /// Returns an EpochGuard that must be held while reading. This ensures
    /// memory accessed during the read won't be reclaimed until the guard is dropped.
    pub fn enter_epoch(&self) -> EpochGuard<'_> {
        EpochGuard::new(&self.epoch_manager)
    }

    /// Get the current read epoch.
    pub fn current_epoch(&self) -> u64 {
        self.epoch_manager.current_epoch()
    }

    /// Advance the epoch (should be called periodically by a background task).
    pub fn advance_epoch(&self) -> u64 {
        self.epoch_manager.advance()
    }

    /// Get the number of active readers.
    pub fn active_readers(&self) -> usize {
        self.epoch_manager.active_reader_count()
    }

    /// Get retry statistics snapshot.
    pub fn retry_stats_snapshot(&self) -> crate::persistent_artrie::concurrency::RetryStatsSnapshot {
        self.retry_stats.snapshot()
    }

    /// Check if the trie is currently being written to.
    pub fn is_write_locked(&self) -> bool {
        !self.version.is_stable()
    }

    /// Get the current version (for debugging/monitoring).
    pub fn current_version(&self) -> u64 {
        self.version.get()
    }

    // ==================== End Optimistic Concurrency Methods ====================
}
