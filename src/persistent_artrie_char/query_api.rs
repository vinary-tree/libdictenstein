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
    /// I/O errors during lazy loading fail closed as `false`. Use
    /// `try_contains()` for explicit error handling.
    pub fn contains(&self, term: &str) -> bool {
        match self.try_contains(term) {
            Ok(result) => result,
            Err(error) => {
                log::warn!("I/O error during lazy loading in contains(): {:?}", error);
                false
            }
        }
    }

    /// Check if a term exists in the trie with explicit error handling.
    ///
    /// This version returns a `Result` for lazy loading I/O errors.
    /// For disk-backed tries, prefetches children at each level for improved I/O performance.
    pub fn try_contains(&self, term: &str) -> Result<bool> {
        // E1 read-flip: under the overlay regime the owned tree is empty (cleared on
        // reopen), so route membership to the non-faulting lock-free read.
        if self.route_overlay() {
            return Ok(self.contains_lockfree(term));
        }
        self.owned_try_contains(term)
    }

    /// Owned-tree membership read (UN-routed). This is the E1 `false`-arm body AND
    /// the read the recovery/reestablish bootstrap must use directly: that bootstrap
    /// runs with `route_overlay()` already true yet must read the recovered OWNED tree
    /// (routing it would read the empty overlay — total data loss; D1).
    pub(crate) fn owned_try_contains(&self, term: &str) -> Result<bool> {
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
    /// I/O errors during lazy loading fail closed as `None`. Use `try_get()`
    /// for explicit error handling.
    pub fn get(&self, term: &str) -> Option<&V> {
        match self.try_get(term) {
            Ok(result) => result,
            Err(error) => {
                log::warn!("I/O error during lazy loading in get(): {:?}", error);
                None
            }
        }
    }

    /// Owned-tree value read returning a borrow (UN-routed). Mirrors `get` but always
    /// reads `self.root`. Used by the recovery/reestablish bootstrap (D1) and by
    /// `try_increment_impl_no_wal` — the `BatchIncrement` read-modify-write that runs
    /// during a corruption-recovery rebuild, which executes with `route_overlay()`
    /// already true (the trie was create-flipped) yet must read the OWNED tree it is
    /// rebuilding, or recovered counters silently accumulate from 0.
    pub(crate) fn owned_get(&self, term: &str) -> Option<&V> {
        match self.owned_try_get(term) {
            Ok(result) => result,
            Err(error) => {
                log::warn!("I/O error during lazy loading in owned_get(): {:?}", error);
                None
            }
        }
    }

    /// E1 read-flip: term → value as an owned `Option<V>` (unlike `get`'s borrow).
    /// Under `route_overlay()` it value-routes to the overlay (the `u64` counter via
    /// `get_lockfree`, or `()` membership) through the SAFE `Any` dispatch in
    /// `overlay_get_value`; otherwise it reads the owned tree. This is the canonical
    /// value getter the `MappedDictionary`/`ARTrie` trait bodies delegate to — the
    /// inherent method shadows the trait method of the same name in `.get_value()`
    /// call syntax, so `self.get_value(..)` from a trait body calls THIS (no recursion).
    pub fn get_value(&self, term: &str) -> Option<V> {
        if self.route_overlay() {
            if let Some(result) = self.overlay_get_value(term) {
                return result;
            }
        }
        self.owned_get(term).cloned()
    }

    /// Get a value by term with explicit error handling.
    ///
    /// This version returns a `Result` for lazy loading I/O errors.
    /// For disk-backed tries, prefetches children at each level for improved I/O performance.
    pub fn try_get(&self, term: &str) -> Result<Option<&V>> {
        // E1 read-flip: the overlay value is computed, not stored in `self.root`, so it
        // cannot be returned as a borrow `&V`. Under the overlay regime this reference-
        // returning getter reports absence; callers needing the value use `get_value`
        // (owned `Option<V>`), which value-routes to the overlay.
        if self.route_overlay() {
            return Ok(None);
        }
        self.owned_try_get(term)
    }

    /// Owned-tree value read returning a borrow (UN-routed). E1 `false`-arm + the
    /// recovery/reestablish bootstrap (D1 — must read the recovered owned tree even
    /// while `route_overlay()` is true).
    pub(crate) fn owned_try_get(&self, term: &str) -> Result<Option<&V>> {
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

        // Clone the value if found (to avoid holding reference during validation).
        // D4: value-route via `get_value` (owned `Option<V>`, no borrow) so the
        // optimistic getter reflects the overlay under the flip, instead of `get`
        // (which returns `None` under the overlay — the borrow limitation).
        let result = self.get_value(term);

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
    pub fn retry_stats_snapshot(
        &self,
    ) -> crate::persistent_artrie::concurrency::RetryStatsSnapshot {
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
