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
    pub(super) fn insert_impl(&self, term: &[u8], value: Option<V>) -> bool {
        // **F4:** `&self` (the kill-switched-owned runtime arm of the trait
        // `insert`; also reachable from the pre-share WAL-replay `&mut self`
        // callers — a `&mut self` caller may call this `&self` method). The owned
        // mutation takes the inner OR write lock inside `insert_impl_core`.
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
        // (`contains_impl` takes + drops a short OR read guard; released before the
        // OR write guard in `insert_impl_core` — no nested self-lock.)
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
    ///
    /// **F4:** `&self` — acquires the **OR** write lock ONCE and delegates to the
    /// reentrancy-safe [`Self::insert_into_root`], which takes `root: &mut
    /// TrieRoot<V>` so its bucket→ART conversion + retry recursion all operate on
    /// the single held guard (parking_lot is non-reentrant — re-locking inside the
    /// recursion would self-deadlock).
    pub(super) fn insert_impl_core(&self, term: &[u8], value: Option<V>) -> bool {
        let buffer_manager = self.buffer_manager.clone();
        let mut root = self.root.write();
        let inserted = self.insert_into_root(&mut root, buffer_manager.as_ref(), term, value);
        // `propagate_dirty_to_root` mutates the SAME root under the held guard.
        if inserted {
            if let TrieRoot::ArtNode { node, .. } = &mut *root {
                node.header_mut().set_has_dirty_descendants(true);
            }
        }
        drop(root); // release OR before touching the dirty-prefix Mutex (lock order)
        if inserted {
            self.term_count.fetch_add(1, AtomicOrdering::Relaxed);
            self.dirty.store(true, AtomicOrdering::Release);
            self.record_dirty_path(term);
        }
        inserted
    }

    /// Reentrancy-safe core of [`Self::insert_impl_core`]: insert `term` into an
    /// explicitly-borrowed `root` (the held OR guard's target). Recurses + converts
    /// bucket→ART without re-locking. Returns whether a NEW term was inserted (the
    /// caller does the dirty/count bookkeeping).
    fn insert_into_root(
        &self,
        root: &mut TrieRoot<V>,
        buffer_manager: Option<
            &std::sync::Arc<crate::sync_compat::RwLock<super::buffer_manager::BufferManager<S>>>,
        >,
        term: &[u8],
        value: Option<V>,
    ) -> bool {
        match root {
            TrieRoot::Bucket(_) => {
                // Clone value here in case we need to retry after bucket conversion
                let value_for_retry = value.clone();

                // Serialize value for bucket storage
                let serialized_value: Option<Vec<u8>> =
                    value.and_then(|v| crate::serialization::bincode_compat::serialize(&v).ok());

                // Insert into the bucket in an inner scope so the `&mut bucket`
                // borrow of `*root` ENDS before we (possibly) convert `root`
                // bucket→ART and retry — keeping a single OR guard, no re-lock.
                let (result, should_split) = {
                    let TrieRoot::Bucket(bucket) = &mut *root else {
                        unreachable!("matched Bucket above");
                    };
                    let result = if let Some(ref val_bytes) = serialized_value {
                        bucket.insert(term, val_bytes)
                    } else {
                        bucket.insert_key(term)
                    };
                    let should_split = result.is_ok() && bucket.header().should_split();
                    (result, should_split)
                };

                match result {
                    Ok(inserted) => {
                        if should_split {
                            Self::convert_root_bucket_to_art(root);
                        }
                        inserted
                    }
                    Err(_) => {
                        // Bucket is full, convert to ART and retry
                        Self::convert_root_bucket_to_art(root);
                        // Retry insert in the new ART structure (no double WAL logging,
                        // same held OR guard via the `root` reborrow).
                        self.insert_into_root(root, buffer_manager, term, value_for_retry);
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
                        if !resolve_child_for_mutation_with_bm(&mut children[idx].1, buffer_manager)
                        {
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
        }
    }

    /// Remove implementation with WAL logging (for persistent mode).
    ///
    /// **F4:** `&self` (kill-switched-owned runtime arm + pre-share replay caller).
    pub(super) fn remove_impl(&self, term: &[u8]) -> bool {
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
    ///
    /// **F4:** `&self` — takes the OR write lock once and delegates to the
    /// reentrancy-safe [`Self::remove_from_root`] (`root: &mut TrieRoot<V>`).
    pub(super) fn remove_impl_core(&self, term: &[u8]) -> bool {
        let buffer_manager = self.buffer_manager.clone();
        let mut root = self.root.write();
        let removed = Self::remove_from_root(&mut root, buffer_manager.as_ref(), term);
        if removed {
            if let TrieRoot::ArtNode { node, .. } = &mut *root {
                node.header_mut().set_has_dirty_descendants(true);
            }
        }
        drop(root);
        if removed {
            self.term_count.fetch_sub(1, AtomicOrdering::Relaxed);
            self.dirty.store(true, AtomicOrdering::Release);
            self.record_dirty_path(term);
        }
        removed
    }

    /// Reentrancy-safe core of [`Self::remove_impl_core`].
    fn remove_from_root(
        root: &mut TrieRoot<V>,
        buffer_manager: Option<
            &std::sync::Arc<crate::sync_compat::RwLock<super::buffer_manager::BufferManager<S>>>,
        >,
        term: &[u8],
    ) -> bool {
        match root {
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
                        if !resolve_child_for_mutation_with_bm(&mut children[idx].1, buffer_manager)
                        {
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
        }
    }

    /// Convert root bucket to ART node structure (operates on an explicitly-held
    /// `root`, so the owned-mutator recursion never re-locks the OR `RwLock`).
    fn convert_root_bucket_to_art(root: &mut TrieRoot<V>) {
        if let TrieRoot::Bucket(bucket) = &*root {
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
                *root = TrieRoot::ArtNode {
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
    /// that are already in the WAL. **F4:** `&self` (delegates to the `&self`
    /// core; callable from the `&mut self` pre-share replay sinks).
    pub(super) fn insert_impl_no_wal(&self, term: &[u8], value: Option<V>) -> bool {
        // Call core implementation directly to skip WAL logging
        self.insert_impl_core(term, value)
    }

    /// Remove implementation without WAL logging (for recovery replay).
    ///
    /// This is used during WAL recovery to avoid re-logging operations
    /// that are already in the WAL. **F4:** `&self`.
    #[allow(dead_code)] // L1.3: production-dead (the recovery appliers that called it are gone); retained for the in-crate owned white-box tests + L2/L3 owned-staging; removed with the owned path at L3.3
    pub(super) fn remove_impl_no_wal(&self, term: &[u8]) -> bool {
        // Call core implementation directly to skip WAL logging
        self.remove_impl_core(term)
    }

    /// Upsert implementation without WAL logging (for recovery replay).
    ///
    /// This updates the value if the term exists, or inserts if it doesn't.
    /// Used during WAL recovery to replay Upsert, Increment, and CAS operations.
    /// **F4:** `&self`. (Two sequential OR critical sections — remove then insert
    /// — exactly as before; the owned path is single-writer under the kill-switch.)
    pub(super) fn upsert_impl_no_wal(&self, term: &[u8], value: V) {
        // First remove existing entry (if any) to allow update
        self.remove_impl_core(term);
        // Then insert with new value
        self.insert_impl_core(term, Some(value));
    }
}
