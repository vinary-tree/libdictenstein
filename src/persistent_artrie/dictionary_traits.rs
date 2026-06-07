//! `Dictionary` / `MappedDictionary` / `Debug` trait implementations
//! for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~4890-4927, ~38 LOC) as
//! the thirteenth Phase-5 byte sub-module. These are thin trait
//! adapters that delegate to inherent methods (`contains_impl` /
//! `get_value_impl` / `get_root_node`); the heavy lifting stays in
//! `dict_impl.rs`.

use std::sync::atomic::Ordering as AtomicOrdering;

use crate::persistent_artrie_core::key_encoding::ByteKey;
use crate::value::DictionaryValue;
use crate::{Dictionary, MappedDictionary, SyncStrategy};

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::node_impl::PersistentARTrieNode;

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentARTrie<V, S> {
    type Node = PersistentARTrieNode<V>;

    fn root(&self) -> Self::Node {
        // F7 BLOCKER-1: under the overlay regime, return an OVERLAY-backed
        // `DictionaryNode` that navigates the lock-free overlay lazily, so zipper /
        // transducer / fuzzy traversal works on a flipped trie (was: an EMPTY owned
        // tree + a `log::warn!` deferral). Additive + reversible — the owned arm is
        // unchanged and returned whenever `!route_overlay()`.
        if self.route_overlay() {
            // `overlay_root_node()` is the hazard-protected immutable root snapshot;
            // an empty/absent overlay yields a fresh empty node (a childless,
            // non-final root — the correct empty-dictionary view).
            use crate::persistent_artrie_core::overlay::flip::LockFreeOverlay;
            let root = <Self as LockFreeOverlay<ByteKey, V, S>>::overlay_root_node(self)
                .unwrap_or_else(|| {
                    std::sync::Arc::new(crate::persistent_artrie_core::overlay::OverlayNode::<
                        ByteKey,
                        V,
                    >::new())
                });
            // Faulter is `None` on the inherent `&self` root path: eviction (the only
            // source of an `OnDisk` overlay child) is impossible on a non-`Shared`
            // owned trie, so the overlay handed out here is fully `Child::InMem`.
            return PersistentARTrieNode::new_overlay(root, None);
        }
        self.get_root_node()
    }

    fn contains(&self, term: &str) -> bool {
        // M3 (C6): delegate to the routed `contains_bytes` (this trait body read
        // `contains_impl` directly, bypassing the overlay route).
        self.contains_bytes(term.as_bytes())
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        // M3 (C6): under the overlay the owned `term_count` is cleared on reopen;
        // count the overlay's resident finals instead (this read `term_count` direct).
        if self.route_overlay() {
            return Some(self.overlay_len());
        }
        Some(self.term_count.load(AtomicOrdering::Acquire))
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

impl<V: DictionaryValue, S: BlockStorage> std::fmt::Debug for PersistentARTrie<V, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentARTrie")
            .field("term_count", &self.term_count.load(AtomicOrdering::Relaxed))
            .field("dirty", &self.dirty.load(AtomicOrdering::Relaxed))
            .finish()
    }
}
