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
    ///
    /// **M3 read-flip (the audit's §C.2 — byte's MIXED-read iterator).** The owned
    /// body enumerates the terms via the arena iter then re-reads each value via
    /// `get_value_impl` (owned). Under `route_overlay()` the owned tree is empty, so
    /// that mixed read would emit every value as `None`. The flip routes through the
    /// VALUE-CARRYING overlay enumerator
    /// [`iter_prefix_with_values_and_arena`](Self::iter_prefix_with_values_and_arena)
    /// (itself overlay-routed to `overlay_iter_prefix_with_values`), NOT
    /// enumerate-overlay-then-value-owned. The owned arm below is the verbatim
    /// pre-flip mixed read (INERT until the flip).
    pub fn iter_with_values(&self) -> TermValueIterator<V> {
        // **F7 fix (term-only membership preservation).** BOTH the overlay and owned arms
        // ENUMERATE every term (membership-complete via `iter_prefix_with_arena`, which
        // includes value-less "term-only" members) and then look the value up PER TERM
        // (`get_value_impl`, overlay-routed under `route_overlay()`), yielding `(term,
        // None)` for a term-only member. The previous overlay arm used the value-CARRYING
        // `iter_prefix_with_values_and_arena` enumerator, whose `PrefixTermWithValueAndArena`
        // cannot represent a value-less final, so it SILENTLY DROPPED term-only members on a
        // mixed valued/value-less trie that routes the overlay (the data-loss-in-observation
        // F7's converter exposed when an Owned mixed-usage trie now reopens INTO the overlay).
        // The enumerate-then-lookup shape matches the proven owned arm exactly.
        let mut entries: Vec<(Vec<u8>, Option<V>)> = self
            .iter_prefix_with_arena(b"")
            .ok()
            .flatten()
            .unwrap_or_default()
            .into_iter()
            .map(|entry| {
                // `get_value_bytes` is overlay-ROUTED (reads the overlay value under
                // `route_overlay()`, falls back to the owned tree otherwise), so a term-only
                // member yields `None` and a valued term yields `Some(v)` on BOTH paths.
                let value = self.get_value_bytes(&entry.term);
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
