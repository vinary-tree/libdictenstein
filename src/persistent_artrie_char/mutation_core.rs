//! Core mutation implementations for `PersistentARTrieChar<V, S>`.
//!
//! Split out of char `dict_impl_char.rs` (lines ~143-273, ~131 LOC)
//! as the twenty-fourth and final Phase-6 char sub-module.
//! These are the pub(super) primitives that the public mutation
//! API (`mutation_api`) and recovery replay (`mmap_ctor` /
//! `io_uring_ctor`) call without WAL logging:
//!
//! - `insert_impl_no_wal` — insert without value
//! - `insert_impl_no_wal_with_value` — insert with value
//! - `remove_impl_no_wal` — remove
//!
//! These manage in-memory `CharTrieNodeInner<V>` directly, including
//! the path-compression / node-growth logic.

use std::sync::atomic::Ordering as AtomicOrdering;

use log::warn;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::error::Result;
use crate::persistent_artrie_core::counter_codec;
use crate::value::DictionaryValue;

use super::types::{CharTrieNodeInner, CharTrieRoot};

impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// Load root from disk given the root descriptor pointer
    ///
    /// This function:
    /// 1. Reads the root descriptor block
    /// 2. Loads arena block IDs and populates the arena manager
    /// 3. Loads the root node (which can now read from arenas)
    ///
    /// # Arguments
    /// * `buffer_manager` - The buffer manager for disk I/O
    /// * `root_desc_ptr` - Pointer to the root descriptor block
    /// * `eager_depth` - Controls loading strategy:
    ///   - `None`: Fully lazy loading (only root node loaded)
    ///   - `Some(0)`: Same as None (lazy loading)
    ///   - `Some(n)`: Load n levels eagerly, rest lazy
    ///   - `Some(usize::MAX)`: Fully eager loading (all levels)

    fn preflight_existing_terminal_is_final(&self, term: &str) -> Result<Option<bool>> {
        // **F4:** `&self` taking the OR write lock — the lock is the exclusivity
        // anchor that replaces the old `&mut self` for the raw-pointer walk (no NEW
        // unsafe; the same raw-pointer pattern, now under the OR guard).
        let mut root_guard = self.root.write();
        let root = match &mut *root_guard {
            CharTrieRoot::Node(node) => node.as_mut(),
            CharTrieRoot::Empty => return Ok(None),
        };

        let mut current = root as *mut CharTrieNodeInner<V>;
        let mut terminal_is_final = root.is_final();
        for c in term.chars() {
            // Safety: current is valid and we hold exclusive access via the OR write
            // guard (single owned writer).
            let node = unsafe { &mut *current };
            let Some(ptr) = node.node.find_child(c as u32) else {
                return Ok(None);
            };
            if ptr.is_null() {
                return Ok(None);
            }
            let child = self.resolve_swizzled_ptr_mut(ptr)?;
            terminal_is_final = child.is_final();
            current = child as *mut CharTrieNodeInner<V>;
        }

        Ok(Some(terminal_is_final))
    }

    pub(super) fn preflight_insert_no_wal(&self, term: &str) -> Result<bool> {
        match self.preflight_existing_terminal_is_final(term)? {
            Some(is_final) => Ok(!is_final),
            None => Ok(true),
        }
    }

    pub(super) fn preflight_insert_with_value_no_wal(&self, term: &str) -> Result<()> {
        let _ = self.preflight_existing_terminal_is_final(term)?;
        Ok(())
    }

    pub(super) fn preflight_remove_no_wal(&self, term: &str) -> Result<bool> {
        match self.preflight_existing_terminal_is_final(term)? {
            Some(is_final) => Ok(is_final),
            None => Ok(false),
        }
    }

    /// Insert a term (internal, no WAL logging)
    ///
    /// **F4:** `&self` taking the OR write lock once. The guard is held for the
    /// whole raw-pointer walk (its target owns the nodes the pointers reference);
    /// the lock provides the single-owned-writer exclusivity that replaces the old
    /// `&mut self` for the unsafe walk (no NEW unsafe).
    pub(super) fn try_insert_impl_no_wal(&self, term: &str) -> Result<bool> {
        let mut root_guard = self.root.write();
        // Ensure we have a root node
        if matches!(&*root_guard, CharTrieRoot::Empty) {
            *root_guard = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }

        // Navigate to the insertion point using raw pointer for traversal.
        let root = match &mut *root_guard {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => unreachable!(),
        };

        let mut current = root;
        for c in term.chars() {
            // Safety: current is valid; exclusivity via the held OR write guard.
            let node = unsafe { &mut *current };
            current = self.get_or_create_child_lazy_ptr(node, c)?;
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if already final
        if node.is_final() {
            return Ok(false);
        }

        // Mark as final
        node.set_final(true);
        self.len.fetch_add(1, AtomicOrdering::Relaxed);
        self.dirty.store(true, AtomicOrdering::Release);
        Ok(true)
    }

    /// Insert a term with value (internal, no WAL logging)
    pub(super) fn try_insert_impl_no_wal_with_value(&self, term: &str, value: V) -> Result<bool> {
        // **F4:** `&self` + OR write guard (see `try_insert_impl_no_wal`).
        let mut root_guard = self.root.write();
        // Ensure we have a root node
        if matches!(&*root_guard, CharTrieRoot::Empty) {
            *root_guard = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }

        // Navigate to the insertion point using raw pointer for traversal
        let root = match &mut *root_guard {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => unreachable!(),
        };

        let mut current = root;
        for c in term.chars() {
            // Safety: current is valid; exclusivity via the held OR write guard.
            let node = unsafe { &mut *current };
            current = self.get_or_create_child_lazy_ptr(node, c)?;
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if already final
        if node.is_final() {
            // Update value if already exists
            node.value = Some(value);
            return Ok(false);
        }

        // Mark as final with value
        node.set_final(true);
        node.value = Some(value);
        self.len.fetch_add(1, AtomicOrdering::Relaxed);
        self.dirty.store(true, AtomicOrdering::Release);
        Ok(true)
    }

    /// Insert a term with value (internal, no WAL logging)

    /// Remove a term (internal, no WAL logging)
    pub(super) fn try_remove_impl_no_wal(&self, term: &str) -> Result<bool> {
        // **F4:** `&self` + OR write guard (see `try_insert_impl_no_wal`).
        let mut root_guard = self.root.write();
        let root = match &mut *root_guard {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => return Ok(false),
        };

        // Navigate to the node using raw pointer for traversal
        let chars: Vec<char> = term.chars().collect();
        let mut current = root;
        for &c in &chars {
            // Safety: current is valid; exclusivity via the held OR write guard.
            let node = unsafe { &*current };
            match self.get_child_mut_lazy(node, c) {
                Ok(Some(child)) => current = child as *mut CharTrieNodeInner<V>,
                Ok(None) => return Ok(false), // Term not found
                Err(error) => return Err(error),
            }
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if this node is final
        if !node.is_final() {
            return Ok(false);
        }

        // Mark as not final
        node.set_final(false);
        node.value = None;
        self.len.fetch_sub(1, AtomicOrdering::Relaxed);
        self.dirty.store(true, AtomicOrdering::Release);
        Ok(true)
    }

    pub(super) fn insert_impl_no_wal(&self, term: &str) -> bool {
        self.try_insert_impl_no_wal(term).unwrap_or_else(|error| {
            warn!(
                "I/O error during lazy loading in insert replay: {:?}",
                error
            );
            false
        })
    }

    pub(super) fn insert_impl_no_wal_with_value(&self, term: &str, value: V) -> bool {
        self.try_insert_impl_no_wal_with_value(term, value)
            .unwrap_or_else(|error| {
                warn!(
                    "I/O error during lazy loading in value insert replay: {:?}",
                    error
                );
                false
            })
    }

    pub(super) fn remove_impl_no_wal(&self, term: &str) -> bool {
        self.try_remove_impl_no_wal(term).unwrap_or_else(|error| {
            warn!(
                "I/O error during lazy loading in remove replay: {:?}",
                error
            );
            false
        })
    }

    /// **Shared owned-tree WAL replay (Order-A replay-order fix, design C′ — the
    /// single `replay_records_lww` consumer).** Reconcile the recovered records
    /// into per-term last-writer-wins by commit generation
    /// ([`crate::persistent_artrie_core::recovery::reconcile_lww`]), then apply
    /// each winning operation in commit-visibility order via
    /// [`Self::apply_core_recovered_operation_no_wal`].
    ///
    /// Generic over `S: BlockStorage` so EVERY owned-tree replay site — the two
    /// `mmap_ctor::open*` ctors (`MmapDiskManager`) AND the `io_uring_ctor` ctor
    /// (`IoUringDiskManager`) — routes through THIS one function and cannot drift
    /// apart (design risk 7). Returns whether at least one record was in scope
    /// (`false` when every record was skipped by the checkpoint guard — the
    /// pre-fix `skipped_all` signal the ctors use to consider WAL truncation /
    /// clearing the dirty flag).
    ///
    /// For a WAL with no `CommitRank` records (any pre-fix log) this is
    /// byte-for-byte the pre-fix in-order replay: `generation_of = lsn`, so the
    /// per-term winner is the highest-LSN op, applied in LSN order.
    pub(super) fn replay_records_lww(
        &mut self,
        recovered_ops: Vec<(
            crate::persistent_artrie::wal::Lsn,
            crate::persistent_artrie::wal::WalRecord,
        )>,
        loaded_from_disk: bool,
        checkpoint_lsn: crate::persistent_artrie::wal::Lsn,
        rank_regime: crate::persistent_artrie_core::wal::RankRegime,
    ) -> bool {
        // Was anything in scope at all (i.e. not entirely below the checkpoint)?
        // Mirrors the pre-fix `skipped_all` flag: true iff every record was
        // skipped by the checkpoint guard.
        let any_in_scope = recovered_ops
            .iter()
            .any(|(lsn, _)| !(loaded_from_disk && checkpoint_lsn > 0 && *lsn <= checkpoint_lsn));

        let winners = crate::persistent_artrie_core::recovery::reconcile_lww(
            recovered_ops,
            loaded_from_disk,
            checkpoint_lsn,
            // S4 (DG-RECON): the file's ACTUAL regime (`header.regime()`, threaded from
            // the ctor). Owned ⇒ KEEP-unranked-@-lsn (legacy/base/vocab/un-flipped char);
            // Overlay ⇒ DROP unranked two-append orphans — the idempotent NO-RANK records
            // the producers leave on the rare present-hoist miss.
            rank_regime,
        );
        for op in winners {
            // The applier logs+returns false on a value-deserialize failure
            // (same best-effort semantics the pre-fix inline loop had, which
            // simply skipped on a deserialize `Err`).
            let _ = self.apply_core_recovered_operation_no_wal(op);
        }
        any_in_scope
    }

    /// Apply ONE reconciled [`crate::persistent_artrie::recovery::RecoveredOperation`]
    /// to the owned tree without WAL logging. The per-term winners chosen by
    /// [`Self::replay_records_lww`] are applied through here; also reused by the
    /// archive-segment recovery path. Generic over `S` (see `replay_records_lww`).
    pub(super) fn apply_core_recovered_operation_no_wal(
        &mut self,
        op: crate::persistent_artrie::recovery::RecoveredOperation,
    ) -> bool {
        match op {
            crate::persistent_artrie::recovery::RecoveredOperation::Insert {
                term, value, ..
            } => {
                let term_str = String::from_utf8_lossy(&term);
                if let Some(value_bytes) = value {
                    match crate::serialization::bincode_compat::deserialize::<V>(&value_bytes) {
                        Ok(value) => {
                            self.insert_impl_no_wal_with_value(&term_str, value);
                            true
                        }
                        Err(error) => {
                            log::warn!(
                                "Failed to deserialize recovered char insert value: {:?}",
                                error
                            );
                            false
                        }
                    }
                } else {
                    self.insert_impl_no_wal(&term_str);
                    true
                }
            }
            crate::persistent_artrie::recovery::RecoveredOperation::Remove { term, .. } => {
                let term_str = String::from_utf8_lossy(&term);
                self.remove_impl_no_wal(&term_str);
                true
            }
            crate::persistent_artrie::recovery::RecoveredOperation::Increment {
                term,
                delta,
                result,
                ..
            } => {
                let term_str = String::from_utf8_lossy(&term);
                match result {
                    // Delta (from BatchIncrement) ⇒ ACCUMULATE `delta` (D6 — was the
                    // `result == 0` arm, which mis-classified an absolute-set-to-0).
                    None => self.try_increment_impl_no_wal(&term_str, delta).is_ok(),
                    // Absolute (single Increment) ⇒ SET the term to `v` (including 0).
                    Some(v) => match Self::value_from_recovered_i64(v) {
                        Some(value) => {
                            self.insert_impl_no_wal_with_value(&term_str, value);
                            true
                        }
                        None => false,
                    },
                }
            }
            crate::persistent_artrie::recovery::RecoveredOperation::Upsert {
                term, value, ..
            } => {
                let term_str = String::from_utf8_lossy(&term);
                match crate::serialization::bincode_compat::deserialize::<V>(&value) {
                    Ok(value) => {
                        self.insert_impl_no_wal_with_value(&term_str, value);
                        true
                    }
                    Err(error) => {
                        log::warn!(
                            "Failed to deserialize recovered char upsert value: {:?}",
                            error
                        );
                        false
                    }
                }
            }
            crate::persistent_artrie::recovery::RecoveredOperation::CompareAndSwap {
                term,
                new_value,
                success,
                ..
            } => {
                if !success {
                    return false;
                }

                let term_str = String::from_utf8_lossy(&term);
                match crate::serialization::bincode_compat::deserialize::<V>(&new_value) {
                    Ok(value) => {
                        self.insert_impl_no_wal_with_value(&term_str, value);
                        true
                    }
                    Err(error) => {
                        log::warn!(
                            "Failed to deserialize recovered char CAS value: {:?}",
                            error
                        );
                        false
                    }
                }
            }
        }
    }

    /// Decode a recovered ABSOLUTE counter (the WAL `Increment.result` i64 field,
    /// which carries the COUNTER LEAF BIT-PATTERN written via `counter_return_i64`)
    /// back into the typed counter `V`, routed through the `counter_codec` i128
    /// substrate (the v6 gate). The i64 is reinterpreted as the 8-byte leaf
    /// (`counter_leaf_to_i128` of its LE bytes) so a `u64` count > i64::MAX (whose
    /// i64 field is negative) recovers its true magnitude — a naive `value as i128`
    /// widen would wrongly stay negative and reject. `None` on a non-counter `V`.
    fn value_from_recovered_i64(value: i64) -> Option<V> {
        let n = counter_codec::counter_leaf_to_i128::<V>(&value.to_le_bytes())?;
        counter_codec::i128_to_counter_value::<V>(n)
    }
}
