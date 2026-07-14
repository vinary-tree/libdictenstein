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
        let terms: Vec<_> = self
            .iter_prefix_with_arena(b"")
            .ok()
            .flatten()
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.term)
            .collect();
        TermIterator::from_terms(terms)
    }

    /// Iterate over all terms with their values.
    ///
    /// Returns an iterator yielding `(term, Option<value>)` pairs in lexicographic order.
    ///
    /// **The MIXED-read iterator (the audit's §C.2).** The overlay is the sole
    /// representation, so this ENUMERATES every term (membership-complete via
    /// `iter_prefix_with_arena`, including value-less "term-only" members) and then
    /// looks up the value PER TERM (`get_value_bytes`, overlay-routed) — NOT the
    /// value-CARRYING enumerator, which cannot represent a value-less final and would
    /// silently drop term-only members.
    pub fn iter_with_values(&self) -> TermValueIterator<V> {
        // **F7 fix (term-only membership preservation).** ENUMERATE every term
        // (membership-complete via `iter_prefix_with_arena`, which includes value-less
        // "term-only" members) and then look the value up PER TERM (`get_value_bytes`,
        // overlay-routed), yielding `(term, None)` for a term-only member. The previous
        // value-CARRYING `iter_prefix_with_values_and_arena` enumerator, whose
        // `PrefixTermWithValueAndArena` cannot represent a value-less final, SILENTLY
        // DROPPED term-only members on a mixed valued/value-less trie (the
        // data-loss-in-observation F7's converter exposed when an Owned mixed-usage file
        // now reopens INTO the overlay).
        let entries: Vec<(Vec<u8>, Option<V>)> = self
            .iter_prefix_with_arena(b"")
            .ok()
            .flatten()
            .unwrap_or_default()
            .into_iter()
            .map(|entry| {
                // `get_value_bytes` reads the overlay value (the sole representation), so a
                // term-only member yields `None` and a valued term yields `Some(v)`.
                let value = self.get_value_bytes(&entry.term);
                (entry.term, value)
            })
            .collect();
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
        let terms = self.iter_prefix_with_arena(prefix).ok()??;
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
        let terms = self.iter_prefix_with_values_and_arena(prefix).ok()??;
        Some(terms.into_iter().map(|t| (t.term, t.value)))
    }

    /// Iterate over all `(term, value)` pairs as raw byte vectors.
    ///
    /// Yields `(Vec<u8>, V)` over the whole trie with lossless raw-byte keys — no
    /// stringification, so non-UTF-8 keys (high bytes `0x80..=0xFF`, `0x00`)
    /// round-trip intact, unlike [`iter_strings`](Self::iter_strings). Value-less
    /// "term-only" members (finals with no value) are skipped; use
    /// [`iter_with_values`](Self::iter_with_values) to observe them as
    /// `(term, None)`. Uniform-named twin of the in-memory DAWG's
    /// `iter_bytes_with_values` for generic byte-backend code.
    pub fn iter_bytes_with_values(&self) -> impl Iterator<Item = (Vec<u8>, V)> + '_
    where
        V: Clone,
    {
        // `b""` (the empty prefix) is the overlay root and always exists, so
        // `iter_prefix_with_values(b"")` is always `Some`; flatten the Option away.
        self.iter_prefix_with_values(b"").into_iter().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(prefix: &str) -> tempfile::TempDir {
        std::fs::create_dir_all("target/test-tmp").expect("create target/test-tmp");
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("target/test-tmp")
            .expect("scratch tempdir under target/test-tmp")
    }

    #[test]
    fn public_iter_uses_overlay_lexicographic_order_without_resort() {
        let dir = scratch("byte-public-iter-order-");
        let dict: PersistentARTrie<()> =
            PersistentARTrie::create(dir.path().join("dict.part")).expect("create trie");
        for term in ["z", "aa", "a", "ab", "b", ""] {
            dict.insert(term);
        }

        let terms: Vec<String> = dict.iter_strings().collect();

        assert_eq!(terms, vec!["", "a", "aa", "ab", "b", "z"]);
    }

    #[test]
    fn public_iter_with_values_preserves_overlay_lexicographic_order() {
        let dir = scratch("byte-public-iter-values-order-");
        let dict: PersistentARTrie<u64> =
            PersistentARTrie::create(dir.path().join("dict.part")).expect("create trie");
        for (term, value) in [("z", 6), ("aa", 3), ("a", 2), ("ab", 4), ("b", 5)] {
            dict.insert_with_value(term, value);
        }

        let entries: Vec<_> = dict
            .iter_with_values()
            .map(|(term, value)| (String::from_utf8(term).expect("utf8"), value))
            .collect();

        assert_eq!(
            entries,
            vec![
                ("a".to_string(), Some(2)),
                ("aa".to_string(), Some(3)),
                ("ab".to_string(), Some(4)),
                ("b".to_string(), Some(5)),
                ("z".to_string(), Some(6)),
            ]
        );
    }
}
