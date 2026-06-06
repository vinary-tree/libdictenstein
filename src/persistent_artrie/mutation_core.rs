//! Core mutation implementation for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~439-782, ~344 LOC) as
//! the twenty-sixth Phase-5 byte sub-module. These methods form the
//! heart of the trie's write path:
//!
//! - `insert_impl` — WAL-logged insert wrapper (called from
//!   `mutation_api`, `parallel_merge`, `atomic_ops`)
//! - `insert_impl_core` — actual mutation against the in-memory
//!   `TrieRoot::Bucket` / `TrieRoot::ArtNode`
//! - `remove_impl` — WAL-logged remove wrapper
//! - `remove_impl_core` — actual remove against the in-memory state
//! - `convert_bucket_to_art` — bucket-to-ART promotion when a bucket
//!   exceeds `MAX_BUCKET_ENTRIES`
//! - `insert_impl_no_wal` / `remove_impl_no_wal` /
//!   `upsert_impl_no_wal` — recovery-replay variants that skip WAL
//!   logging
//!
//! All methods are `pub(super)` so the sibling modules
//! (`mutation_api`, `atomic_ops`, `shared_trait_impl`,
//! `io_uring_ctor`, `mmap_ctor`, `parallel_merge`, `document_tx`,
//! `disk_load`) can call them.

use std::sync::atomic::Ordering as AtomicOrdering;

use log::warn;

use crate::value::DictionaryValue;

use super::block_storage::BlockStorage;
use super::bucket::StringBucket;
use super::dict_impl::{resolve_child_for_mutation_with_bm, PersistentARTrie, TrieRoot};
use super::nodes::{ArtNode, Node};
use super::swizzled_ptr::SwizzledPtr;
use super::transitions::{bucket_to_art_node, ChildNode};

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Insert implementation with WAL logging (for persistent mode).
    ///
    /// `pub(super)` so the parallel-merge extension trait in
    /// `crate::persistent_artrie::parallel_merge` (gated on the
    /// `parallel-merge` feature) can call it during the
    /// sequential-write phase of `merge_from_parallel`.
    pub(super) fn insert_impl(&mut self, term: &[u8], value: Option<V>) -> bool {
        let serialized_value = match value.as_ref() {
            Some(v) => match crate::serialization::bincode_compat::serialize(v) {
                Ok(bytes) => Some(bytes),
                Err(e) => {
                    warn!("Failed to serialize value for WAL: {:?}", e);
                    return false;
                }
            },
            None => None,
        };

        // Log new terms and value updates before applying them in memory.
        // Duplicate term-only inserts are no-ops and do not need WAL traffic.
        let needs_wal = value.is_some() || !self.contains_impl(term);
        if needs_wal {
            use super::wal::WalRecord;

            let record = WalRecord::Insert {
                term: term.to_vec(),
                value: serialized_value,
            };
            if let Err(e) = self.append_mutation_wal_record(record, "insert") {
                warn!("Failed to log insert to WAL: {:?}", e);
                return false;
            }
        }

        self.insert_impl_core(term, value)
    }

    /// Core insert implementation without WAL logging.
    pub(super) fn insert_impl_core(&mut self, term: &[u8], value: Option<V>) -> bool {
        // Clone buffer manager reference before mutable borrow of self.root
        // This is needed to resolve DiskRef nodes during mutation
        let buffer_manager = self.buffer_manager.clone();

        let inserted = match &mut self.root {
            TrieRoot::Bucket(bucket) => {
                // Clone value here in case we need to retry after bucket conversion
                let value_for_retry = value.clone();

                // Serialize value for bucket storage
                let serialized_value: Option<Vec<u8>> =
                    value.and_then(|v| crate::serialization::bincode_compat::serialize(&v).ok());

                let result = if let Some(ref val_bytes) = serialized_value {
                    bucket.insert(term, val_bytes)
                } else {
                    bucket.insert_key(term)
                };

                match result {
                    Ok(inserted) => {
                        // Check if bucket needs to be converted to ART
                        if bucket.header().should_split() {
                            self.convert_bucket_to_art();
                        }
                        inserted
                    }
                    Err(_) => {
                        // Bucket is full, convert to ART and retry
                        self.convert_bucket_to_art();
                        // Retry insert in the new ART structure (no double WAL logging)
                        self.insert_impl_core(term, value_for_retry);
                        true
                    }
                }
            }
            TrieRoot::ArtNode {
                node,
                children,
                is_final,
                value: root_value,
            } => {
                // Serialize value for bucket storage (same as root bucket case)
                let serialized_value: Option<Vec<u8>> = value
                    .clone()
                    .and_then(|v| crate::serialization::bincode_compat::serialize(&v).ok());

                if term.is_empty() {
                    // Inserting empty string
                    if *is_final {
                        if value.is_some() {
                            *root_value = value;
                        }
                        false
                    } else {
                        *is_final = true;
                        *root_value = value;
                        true
                    }
                } else {
                    // Find or create child for first byte
                    let first_byte = term[0];
                    let remaining = &term[1..];

                    // Find existing child
                    let child_idx = children.iter().position(|(b, _)| *b == first_byte);

                    if let Some(idx) = child_idx {
                        // Resolve DiskRef if needed before mutation
                        if !resolve_child_for_mutation_with_bm(
                            &mut children[idx].1,
                            buffer_manager.as_ref(),
                        ) {
                            return false; // Resolution failed (logged in resolve_child_for_mutation_with_bm)
                        }
                        // Use insert_with_value which handles bucket overflow recursively
                        children[idx]
                            .1
                            .insert_with_value(remaining, serialized_value.as_deref())
                    } else {
                        // Create new child bucket
                        let mut bucket = StringBucket::with_values();
                        // Insert with value if provided
                        if let Some(ref val_bytes) = serialized_value {
                            let _ = bucket.insert(remaining, val_bytes);
                        } else {
                            let _ = bucket.insert_key(remaining);
                        }

                        // Add child to ART node, growing the node if it's full
                        let ptr = SwizzledPtr::null();
                        let add_result = match node {
                            Node::N4(n) => n.add_child(first_byte, ptr.clone()),
                            Node::N16(n) => n.add_child(first_byte, ptr.clone()),
                            Node::N48(n) => n.add_child(first_byte, ptr.clone()),
                            Node::N256(n) => n.add_child(first_byte, ptr.clone()),
                        };

                        // If node is full, grow it and retry
                        if let Err(super::nodes::AddChildError::NodeFull) = add_result {
                            // Grow the node to a larger type
                            let grown_node = match node {
                                Node::N4(n) => Node::N16(Box::new(n.grow())),
                                Node::N16(n) => Node::N48(Box::new(n.grow())),
                                Node::N48(n) => Node::N256(Box::new(n.grow())),
                                Node::N256(_) => {
                                    // Node256 can't grow further, this shouldn't happen
                                    // since Node256 can hold all 256 children
                                    log::error!("Cannot grow Node256 - this should never happen");
                                    children.push((first_byte, ChildNode::Bucket(bucket)));
                                    return true;
                                }
                            };
                            *node = grown_node;

                            // Retry add_child on the grown node
                            let _ = match node {
                                Node::N4(n) => n.add_child(first_byte, ptr),
                                Node::N16(n) => n.add_child(first_byte, ptr),
                                Node::N48(n) => n.add_child(first_byte, ptr),
                                Node::N256(n) => n.add_child(first_byte, ptr),
                            };
                        }

                        children.push((first_byte, ChildNode::Bucket(bucket)));
                        true
                    }
                }
            }
        };

        if inserted {
            self.term_count.fetch_add(1, AtomicOrdering::Relaxed);
            self.dirty.store(true, AtomicOrdering::Release);
            // Record the path as dirty for selective persistence
            self.record_dirty_path(term);
            self.propagate_dirty_to_root();
        }

        inserted
    }

    /// Remove implementation with WAL logging (for persistent mode).
    pub(super) fn remove_impl(&mut self, term: &[u8]) -> bool {
        if !self.contains_impl(term) {
            return false;
        }

        use super::wal::WalRecord;
        let record = WalRecord::Remove {
            term: term.to_vec(),
        };
        if let Err(e) = self.append_mutation_wal_record(record, "remove") {
            warn!("Failed to log remove to WAL: {:?}", e);
            return false;
        }

        self.remove_impl_core(term)
    }

    /// Core remove implementation without WAL logging.
    pub(super) fn remove_impl_core(&mut self, term: &[u8]) -> bool {
        // Clone buffer manager reference before mutable borrow of self.root
        // This is needed to resolve DiskRef nodes during mutation
        let buffer_manager = self.buffer_manager.clone();

        let removed = match &mut self.root {
            TrieRoot::Bucket(bucket) => bucket.remove(term).is_some(),
            TrieRoot::ArtNode {
                node: _,
                children,
                is_final,
                value,
            } => {
                if term.is_empty() {
                    if *is_final {
                        *is_final = false;
                        *value = None;
                        true
                    } else {
                        false
                    }
                } else {
                    let first_byte = term[0];
                    let remaining = &term[1..];

                    let child_idx = children.iter().position(|(b, _)| *b == first_byte);

                    if let Some(idx) = child_idx {
                        // Resolve DiskRef if needed before mutation
                        if !resolve_child_for_mutation_with_bm(
                            &mut children[idx].1,
                            buffer_manager.as_ref(),
                        ) {
                            return false; // Resolution failed (logged in resolve_child_for_mutation_with_bm)
                        }
                        match &mut children[idx].1 {
                            ChildNode::Bucket(bucket) => bucket.remove(remaining).is_some(),
                            ChildNode::ArtNode {
                                is_final: child_is_final,
                                value: child_value,
                                children: child_children,
                                ..
                            } => {
                                // Recursive remove from child ART
                                if remaining.is_empty() {
                                    if *child_is_final {
                                        *child_is_final = false;
                                        *child_value = None;
                                        true
                                    } else {
                                        false
                                    }
                                } else {
                                    let first = remaining[0];
                                    let rest = &remaining[1..];

                                    // Find child with matching byte
                                    for (b, c) in child_children.iter_mut() {
                                        if *b == first {
                                            // Use recursive remove_key for all child types
                                            return c.remove_key(rest);
                                        }
                                    }
                                    false
                                }
                            }
                            ChildNode::DiskRef { .. } => {
                                // DiskRef should have been resolved above
                                unreachable!("DiskRef should have been resolved by resolve_child_for_mutation_with_bm")
                            }
                        }
                    } else {
                        false
                    }
                }
            }
        };

        if removed {
            self.term_count.fetch_sub(1, AtomicOrdering::Relaxed);
            self.dirty.store(true, AtomicOrdering::Release);
            // Record the path as dirty for selective persistence
            self.record_dirty_path(term);
            self.propagate_dirty_to_root();
        }

        removed
    }

    /// Convert root bucket to ART node structure
    fn convert_bucket_to_art(&mut self) {
        if let TrieRoot::Bucket(bucket) = &self.root {
            if let Ok(result) = bucket_to_art_node(bucket) {
                // bucket_to_art_node now returns ChildNode directly (which may be
                // buckets or nested ART nodes for overflowed children).
                //
                // Empty-string support (H7): preserve the empty term's root value across
                // the bucket→ART split (reachable on the WAL-replay recovery path).
                // `final_value` is bincode(V) bytes (the bucket stores values as Vec<u8>);
                // deserialize back to Option<V>. A deserialize failure (corruption only)
                // is log::warn'd, NOT silently dropped — membership survives via `is_final`
                // regardless, and the loud log avoids a silent data loss.
                let value: Option<V> = match result.final_value {
                    Some(vb) => match crate::serialization::bincode_compat::deserialize(&vb) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            log::warn!(
                                "convert_bucket_to_art: failed to deserialize empty-term root \
                                 value ({e}); membership preserved, value dropped"
                            );
                            None
                        }
                    },
                    None => None,
                };
                self.root = TrieRoot::ArtNode {
                    node: result.node,
                    children: result.children,
                    is_final: result.is_final,
                    value,
                };
            }
        }
    }

    /// Insert implementation without WAL logging (for recovery replay).
    ///
    /// This is used during WAL recovery to avoid re-logging operations
    /// that are already in the WAL.
    pub(super) fn insert_impl_no_wal(&mut self, term: &[u8], value: Option<V>) -> bool {
        // Call core implementation directly to skip WAL logging
        self.insert_impl_core(term, value)
    }

    /// Remove implementation without WAL logging (for recovery replay).
    ///
    /// This is used during WAL recovery to avoid re-logging operations
    /// that are already in the WAL.
    pub(super) fn remove_impl_no_wal(&mut self, term: &[u8]) -> bool {
        // Call core implementation directly to skip WAL logging
        self.remove_impl_core(term)
    }

    /// Upsert implementation without WAL logging (for recovery replay).
    ///
    /// This updates the value if the term exists, or inserts if it doesn't.
    /// Used during WAL recovery to replay Upsert, Increment, and CAS operations.
    pub(super) fn upsert_impl_no_wal(&mut self, term: &[u8], value: V) {
        // First remove existing entry (if any) to allow update
        self.remove_impl_core(term);
        // Then insert with new value
        self.insert_impl_core(term, Some(value));
    }

    /// **M2d — regime-aware crash-recovery replay (the byte twin of char's
    /// `replay_records_lww`).** Thread the ALREADY-SHARED A2 reconcile
    /// (`crate::persistent_artrie_core::recovery::reconcile_lww`) through byte's
    /// single-segment `open` recovery sinks so that, once byte writes an Overlay
    /// file (M2b `CommitRank`), post-recovery same-term last-writer-wins is
    /// ordered by commit GENERATION (not physical LSN) and Overlay-regime UNRANKED
    /// orphans are DROPPED (red-team defect H3). EVERY single-segment owned-tree
    /// replay sink — the two `mmap_ctor::open*` ctors AND the `io_uring_ctor` ctor
    /// — routes through THIS one function so they cannot drift (design risk 7).
    ///
    /// # Back-compat (the critical inertness proof)
    ///
    /// Returns the count of SUCCESSFULLY-applied winners (`replayed_count`) so each
    /// sink's post-replay WAL-truncation decision (`was_loaded_from_disk &&
    /// replayed_count == 0`) is byte-for-byte unchanged, and stops at the first
    /// failed apply ("durable prefix") exactly as the pre-M2d inline loops did.
    ///
    /// The dispatch is regime-gated (mirroring char's corruption-rebuild
    /// `any_overlay` gate):
    /// * **`Owned`** (every legacy/base/vocab/un-flipped byte file — and ANY
    ///   rank-less WAL, since the WAL header defaults to `Owned` and only the flip
    ///   stamps `Overlay`): replay the `RecoveryManager`-produced, TRANSACTION-
    ///   FILTERED `tx_filtered_ops` in LSN order — i.e. the LITERAL pre-M2d loop.
    ///   This preserves byte's transaction semantics (aborted-tx data records are
    ///   already dropped by `RecoveryManager`), which the shared reconcile — being
    ///   transaction-unaware — would NOT. So for a rank-less WAL the behavior is
    ///   byte-for-byte the current in-order replay (INERT).
    /// * **`Overlay`** (a flipped/overlay file, M2b `CommitRank` present, and —
    ///   by construction — NO transactions, since the durable overlay-write path
    ///   never runs inside a `document_tx`): reconcile the RAW WAL records through
    ///   the shared `reconcile_lww` (which builds the `lsn→generation` map from the
    ///   `CommitRank` markers, orders same-term ops by `(generation, lsn)`, and
    ///   DROPS unranked two-append-window orphans), then apply the winners. This is
    ///   what closes H3.
    ///
    /// `checkpoint_lsn` is byte's native `Option<Lsn>` (the recovered checkpoint
    /// frontier, `None` when nothing loaded from disk); it is mapped to the
    /// reconcile's `(loaded_from_disk, checkpoint_lsn: Lsn)` form internally — the
    /// `Owned` skip-threshold (`op.lsn() <= threshold` ⟺ `was_loaded_from_disk`)
    /// and the reconcile's `loaded_from_disk && checkpoint_lsn > 0 && lsn <=
    /// checkpoint_lsn` guard are equivalent (a `Some(0)` threshold drops only
    /// `lsn <= 0`, i.e. nothing, exactly as `0 > 0 == false`).
    ///
    /// Generic over `S: BlockStorage` so both the `MmapDiskManager` ctors and the
    /// `IoUringDiskManager` ctor share this one body.
    pub(super) fn replay_records_lww(
        &mut self,
        tx_filtered_ops: Vec<super::recovery::RecoveredOperation>,
        raw_records: Vec<(super::wal::Lsn, super::wal::WalRecord)>,
        loaded_from_disk: bool,
        checkpoint_lsn: Option<super::wal::Lsn>,
        rank_regime: crate::persistent_artrie_core::wal::RankRegime,
    ) -> usize {
        match rank_regime {
            // Owned (back-compat / INERT): the LITERAL pre-M2d in-order replay of
            // the transaction-filtered `RecoveryManager` ops. `raw_records` is
            // intentionally unused here — using the (tx-unaware) reconcile would
            // resurrect aborted-tx data records (a behavior change byte must NOT
            // make). For a rank-less WAL this is byte-for-byte the old loop.
            crate::persistent_artrie_core::wal::RankRegime::Owned => {
                let _ = raw_records;
                let skip_threshold = if loaded_from_disk {
                    checkpoint_lsn
                } else {
                    None
                };
                let mut replayed_count = 0usize;
                for op in tx_filtered_ops.into_iter() {
                    if let Some(threshold) = skip_threshold {
                        if op.lsn() <= threshold {
                            continue;
                        }
                    }
                    if self.apply_recovered_operation_no_wal(op) {
                        replayed_count += 1;
                    } else {
                        warn!(
                            "Recovered operation failed during replay; stopping at durable prefix"
                        );
                        break;
                    }
                }
                replayed_count
            }
            // Overlay (H3 close): reconcile the RAW records through the SHARED A2
            // reconcile, then apply the winners. The reconcile builds the
            // commit-generation map from `CommitRank`, orders same-term ops by
            // `(generation, lsn)`, and DROPS unranked Overlay orphans. The durable
            // overlay-write path is never transactional, so the tx-filtered op
            // stream and the raw records carry the SAME data ops (no aborted-tx
            // divergence) — the raw form is required only so the reconcile can see
            // the `CommitRank` markers (which `RecoveryManager` strips).
            crate::persistent_artrie_core::wal::RankRegime::Overlay => {
                let _ = tx_filtered_ops;
                let winners = crate::persistent_artrie_core::recovery::reconcile_lww(
                    raw_records,
                    loaded_from_disk,
                    checkpoint_lsn.unwrap_or(0),
                    rank_regime,
                );
                let mut replayed_count = 0usize;
                for op in winners.into_iter() {
                    if self.apply_recovered_operation_no_wal(op) {
                        replayed_count += 1;
                    } else {
                        warn!(
                            "Recovered operation failed during replay; stopping at durable prefix"
                        );
                        break;
                    }
                }
                replayed_count
            }
        }
    }

    /// Apply a recovered WAL operation without writing a new WAL record.
    pub(super) fn apply_recovered_operation_no_wal(
        &mut self,
        op: super::recovery::RecoveredOperation,
    ) -> bool {
        match op {
            super::recovery::RecoveredOperation::Insert { term, value, .. } => {
                let deserialized =
                    value.and_then(
                        |bytes| match crate::serialization::bincode_compat::deserialize(&bytes) {
                            Ok(value) => Some(value),
                            Err(error) => {
                                warn!("Failed to deserialize recovered insert value: {:?}", error);
                                None
                            }
                        },
                    );
                self.insert_impl_no_wal(&term, deserialized);
                true
            }
            super::recovery::RecoveredOperation::Remove { term, .. } => {
                self.remove_impl_no_wal(&term);
                true
            }
            super::recovery::RecoveredOperation::Increment {
                term,
                delta,
                result,
                ..
            } => {
                // D6: a delta op (`None`, from BatchIncrement) ⇒ ACCUMULATE onto the
                // current value; an absolute op (`Some(v)`, single Increment) ⇒ the
                // value IS `v` (including 0 — the old `result == 0` arm mis-classified
                // an absolute-set-to-0 as a delta and accumulated it).
                let computed = match result {
                    None => self.recompute_recovered_increment(&term, delta),
                    Some(v) => Some(v),
                };
                let final_value = match computed {
                    Some(value) => value,
                    None => {
                        warn!(
                            "Recovered increment overflow for term {:?}; stopping replay at durable prefix",
                            String::from_utf8_lossy(&term)
                        );
                        return false;
                    }
                };

                match Self::value_from_i64(final_value) {
                    Some(value) => {
                        self.upsert_impl_no_wal(&term, value);
                        true
                    }
                    None => false,
                }
            }
            super::recovery::RecoveredOperation::Upsert { term, value, .. } => {
                match crate::serialization::bincode_compat::deserialize(&value) {
                    Ok(value) => {
                        self.upsert_impl_no_wal(&term, value);
                        true
                    }
                    Err(error) => {
                        warn!("Failed to deserialize recovered upsert value: {:?}", error);
                        false
                    }
                }
            }
            super::recovery::RecoveredOperation::CompareAndSwap {
                term,
                new_value,
                success,
                ..
            } => {
                if !success {
                    return false;
                }

                match crate::serialization::bincode_compat::deserialize(&new_value) {
                    Ok(value) => {
                        self.upsert_impl_no_wal(&term, value);
                        true
                    }
                    Err(error) => {
                        warn!("Failed to deserialize recovered CAS value: {:?}", error);
                        false
                    }
                }
            }
        }
    }

    fn recompute_recovered_increment(&self, term: &[u8], delta: i64) -> Option<i64> {
        let current = self
            .get_value_impl(term)
            .and_then(|value| Self::i64_from_value(&value))
            .unwrap_or(0);
        current.checked_add(delta)
    }

    fn i64_from_value(value: &V) -> Option<i64> {
        let bytes = crate::serialization::bincode_compat::serialize(value).ok()?;
        if bytes.len() == 8 {
            let raw: [u8; 8] = bytes.try_into().ok()?;
            Some(i64::from_le_bytes(raw))
        } else {
            crate::serialization::bincode_compat::deserialize::<i64>(&bytes).ok()
        }
    }

    fn value_from_i64(value: i64) -> Option<V> {
        let bytes = crate::serialization::bincode_compat::serialize(&value).ok()?;
        crate::serialization::bincode_compat::deserialize(&bytes).ok()
    }
}
