//! `Dictionary` / `MappedDictionary` / `Debug` trait implementations
//! for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~4890-4927, ~38 LOC) as
//! the thirteenth Phase-5 byte sub-module. These are thin trait
//! adapters that delegate to inherent methods (`contains_impl` /
//! `get_value_impl` / `get_root_node`); the heavy lifting stays in
//! `dict_impl.rs`.

use std::sync::atomic::Ordering as AtomicOrdering;

use crate::value::DictionaryValue;
use crate::{Dictionary, MappedDictionary, SyncStrategy};

use super::block_storage::BlockStorage;
use super::dict_impl::PersistentARTrie;
use super::node_impl::PersistentARTrieNode;

impl<V: DictionaryValue, S: BlockStorage> Dictionary for PersistentARTrie<V, S> {
    type Node = PersistentARTrieNode<V>;

    fn root(&self) -> Self::Node {
        // M3 DEFER (D8 / E1-iter-B boundary): under the overlay regime this walks the
        // OWNED tree (empty), so zipper / transducer / fuzzy traversal over a flipped
        // trie sees an empty dictionary. Overlay-backed `DictionaryNode` traversal is
        // a follow-on; the warning makes the boundary observable rather than silent.
        if self.route_overlay() {
            log::warn!(
                "root()/zipper traversal under the lock-free overlay returns an EMPTY owned \
                 tree (M3 DEFER / E1-iter-B: overlay-backed DictionaryNode traversal is not yet \
                 implemented); use contains / get_value / iter_prefix for overlay reads"
            );
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
