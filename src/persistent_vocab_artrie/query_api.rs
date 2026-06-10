//! Public query API for `PersistentVocabARTrie<S>` — OVERLAY-ONLY (V6).
//!
//! - `get_index` (term → u64 index) — lock-free overlay lookup
//! - `get_term` (u64 index → term) — the in-memory reverse map (id → term)
//! - `contains` / `contains_index`
//! - `len` / `is_empty`
//! - `start_index` / `next_index`

use std::sync::atomic::Ordering;

use crate::persistent_artrie::block_storage::BlockStorage;

impl<S: BlockStorage> super::dict_impl::PersistentVocabARTrie<S> {
    /// Get the vocabulary index for a term (lock-free overlay lookup).
    pub fn get_index(&self, term: &str) -> Option<u64> {
        self.get_index_lockfree(term)
    }

    /// Get the term for a vocabulary index via the in-memory reverse map (id → term).
    ///
    /// The reverse map is populated on every insert and rebuilt from the image on reopen, so
    /// it covers every assigned id (the owned parent-pointer/reverse-index machinery is deleted).
    pub fn get_term(&self, index: u64) -> Option<String> {
        self.reverse_term_map
            .as_ref()
            .and_then(|m| m.get(&index).map(|e| e.value().clone()))
    }

    /// Check if a term exists in the vocabulary.
    #[inline]
    pub fn contains(&self, term: &str) -> bool {
        self.get_index(term).is_some()
    }

    /// Check if an index exists in the vocabulary.
    #[inline]
    pub fn contains_index(&self, index: u64) -> bool {
        self.get_term(index).is_some()
    }

    /// Get the number of vocabulary entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.entry_count.load(Ordering::Acquire)
    }

    /// Check if the vocabulary is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the starting index.
    #[inline]
    pub fn start_index(&self) -> u64 {
        self.start_index
    }

    /// Get the next index to be assigned.
    #[inline]
    pub fn next_index(&self) -> u64 {
        self.next_index.load(Ordering::Acquire)
    }
}
