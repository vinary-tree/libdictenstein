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
        self.get_root_node()
    }

    fn contains(&self, term: &str) -> bool {
        self.contains_impl(term.as_bytes())
    }

    #[inline]
    fn len(&self) -> Option<usize> {
        Some(self.term_count.load(AtomicOrdering::Acquire))
    }

    fn sync_strategy(&self) -> SyncStrategy {
        SyncStrategy::InternalSync
    }
}

impl<V: DictionaryValue, S: BlockStorage> MappedDictionary for PersistentARTrie<V, S> {
    type Value = V;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        self.get_value_impl(term.as_bytes())
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
