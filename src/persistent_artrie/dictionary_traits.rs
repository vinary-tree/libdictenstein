//! `Dictionary` / `MappedDictionary` / `Debug` trait implementations
//! for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~4890-4927, ~38 LOC) as
//! the thirteenth Phase-5 byte sub-module. These are thin trait
//! adapters that delegate to inherent methods (`contains_impl` /
//! `get_value_impl` / `get_root_node`); the heavy lifting stays in
//! `dict_impl.rs`.

use std::sync::atomic::Ordering as AtomicOrdering;

use crate::persistent_artrie::core::key_encoding::ByteKey;
use crate::value::DictionaryValue;
use crate::{Dictionary, MappedDictionary, MutableMappedDictionary, SyncStrategy};

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::node_impl::PersistentARTrieNode;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Capture a traversal root and exact cardinality from one atomic revision.
    pub(crate) fn root_with_term_count(&self) -> (PersistentARTrieNode<V>, usize) {
        use crate::persistent_artrie::core::overlay::flip::LockFreeOverlay;
        let (root, term_count) = <Self as LockFreeOverlay<ByteKey, V, S>>::lockfree_root(self)
            .and_then(|slot| slot.load_with_term_count())
            .unwrap_or_else(|| {
                (
                    std::sync::Arc::new(crate::persistent_artrie::core::overlay::OverlayNode::<
                        ByteKey,
                        V,
                    >::new()),
                    0,
                )
            });
        (
            PersistentARTrieNode::from_overlay_root(root, None),
            term_count,
        )
    }
}

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentARTrie<V, S> {
    type Node = PersistentARTrieNode<V>;

    fn root(&self) -> Self::Node {
        // F7 BLOCKER-1 / L3.3: return an OVERLAY-backed `DictionaryNode` that navigates
        // the lock-free overlay lazily (the owned tree is gone), so zipper / transducer /
        // fuzzy traversal works. `overlay_root_node()` is the hazard-protected immutable
        // root snapshot; an empty/absent overlay yields a fresh empty node (a childless,
        // non-final root — the correct empty-dictionary view).
        self.root_with_term_count().0
    }

    fn contains(&self, term: &str) -> bool {
        // M3 (C6): delegate to the routed `contains_bytes` (this trait body read
        // `contains_impl` directly, bypassing the overlay route).
        self.contains_bytes(term.as_bytes())
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        // L3.3: the overlay is the sole representation; count its resident finals.
        Some(self.overlay_len())
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentARTrie<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        // M3 (C6): delegate to the routed `get_value_bytes` (value-routes to the
        // overlay, incl. the empty-term owned exception), NOT `get_value_impl`
        // directly (which reads the empty owned tree under the flip).
        self.get_value_bytes(term.as_bytes())
    }
}

impl<V: DictionaryValue, S: BlockStorage> MutableMappedDictionary for PersistentARTrie<V, S> {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        PersistentARTrie::insert_with_value(self, term, value)
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let entries = match other.iter_prefix_with_values_and_arena(b"") {
            Ok(Some(terms)) => terms
                .into_iter()
                .map(|term| (term.term, term.value))
                .collect(),
            Ok(None) => return 0,
            Err(error) => {
                log::warn!("PersistentARTrie::union_with source iteration failed: {error}");
                return 0;
            }
        };
        self.merge_entries_overlay(entries, merge_fn)
            .unwrap_or_else(|error| {
                log::warn!("PersistentARTrie::union_with merge failed: {error}");
                0
            })
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: Fn(&mut Self::Value),
    {
        // Delegate to the atomic byte-keyed CAS loop so this shares the lock-free
        // overlay CX + WAL + CAS path (no lost updates). The previous body was a
        // read-then-`upsert` with a lost-update window under concurrent `&self`
        // callers; `update_or_insert_bytes` closes it via a value-CAS retry loop.
        // Errors are swallowed to `false` to preserve this trait method's
        // infallible signature, matching the previous body's `unwrap_or_else`
        // convention.
        PersistentARTrie::update_or_insert_bytes(self, term.as_bytes(), default_value, update_fn)
            .unwrap_or_else(|error| {
                log::warn!("PersistentARTrie::update_or_insert failed: {error}");
                false
            })
    }
}

impl<V: DictionaryValue, S: BlockStorage> std::fmt::Debug for PersistentARTrie<V, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentARTrie")
            .field("term_count", &self.term_count.load(AtomicOrdering::Relaxed))
            .field("dirty", &self.dirty.load(AtomicOrdering::Relaxed))
            .finish()
    }
}
