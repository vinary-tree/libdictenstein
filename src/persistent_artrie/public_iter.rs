//! Public iteration API for `PersistentARTrie<V, S>`.
//!
//! Split out of byte `dict_impl.rs` (lines ~5263-5413, ~150 LOC) as
//! the eleventh Phase-5 byte sub-module. These are the thin wrappers
//! over `TermIterator` / `TermValueIterator` and the arena-aware
//! prefix iterators — the heavy lifting lives in
//! `super::iterators` (DFS state machines) and the
//! `iter_prefix_with_arena` / `iter_prefix_with_values_and_arena`
//! methods on `PersistentARTrie`.

use super::block_storage::BlockStorage;
use super::dict_impl::{PersistentARTrie, TermIterator, TermValueIterator};
use crate::value::DictionaryValue;

impl<V: DictionaryValue, S: BlockStorage> PersistentARTrie<V, S> {
    /// Iterate over all terms in the dictionary.
    ///
    /// Returns an iterator yielding terms as `Vec<u8>` in lexicographic order.
    pub fn iter(&self) -> TermIterator<V> {
        let mut terms: Vec<_> = self
            .iter_prefix_with_arena(b"")
            .ok()
            .flatten()
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.term)
            .collect();
        terms.sort();
        TermIterator::from_terms(terms)
    }

    /// Iterate over all terms with their values.
    ///
    /// Returns an iterator yielding `(term, Option<value>)` pairs in lexicographic order.
    pub fn iter_with_values(&self) -> TermValueIterator<V> {
        let mut entries: Vec<_> = self
            .iter_prefix_with_arena(b"")
            .ok()
            .flatten()
            .unwrap_or_default()
            .into_iter()
            .map(|entry| {
                let value = self.get_value_impl(&entry.term);
                (entry.term, value)
            })
            .collect();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        TermValueIterator::from_terms(entries)
    }

    /// Iterate over all terms as strings.
    ///
    /// This is a convenience method that converts terms to UTF-8 strings,
    /// skipping any terms that contain invalid UTF-8.
    pub fn iter_strings(&self) -> impl Iterator<Item = String> + '_ {
        self.iter()
            .filter_map(|bytes| String::from_utf8(bytes).ok())
    }

    /// Iterate over all terms with the given prefix.
    ///
    /// Returns `None` if the prefix path doesn't exist in the trie.
    /// Returns `Some(iterator)` that yields all terms starting with the prefix.
    pub fn iter_prefix(&self, prefix: &[u8]) -> Option<impl Iterator<Item = Vec<u8>> + '_> {
        self.iter_prefix_direct(prefix)
    }

    /// Direct prefix iteration implementation (non-zipper based).
    fn iter_prefix_direct(&self, prefix: &[u8]) -> Option<impl Iterator<Item = Vec<u8>> + '_> {
        let mut terms = self.iter_prefix_with_arena(prefix).ok()??;
        terms.sort_by(|left, right| left.term.cmp(&right.term));
        Some(terms.into_iter().map(|t| t.term))
    }

    /// Iterate over all (term, value) pairs with the given prefix.
    ///
    /// Returns `None` if the prefix path doesn't exist in the trie.
    /// Returns `Some(iterator)` that yields all (term, value) pairs where term
    /// starts with prefix.
    pub fn iter_prefix_with_values(
        &self,
        prefix: &[u8],
    ) -> Option<impl Iterator<Item = (Vec<u8>, V)> + '_>
    where
        V: Clone,
    {
        let mut terms = self.iter_prefix_with_values_and_arena(prefix).ok()??;
        terms.sort_by(|left, right| left.term.cmp(&right.term));
        Some(terms.into_iter().map(|t| (t.term, t.value)))
    }
}
