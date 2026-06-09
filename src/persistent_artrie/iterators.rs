//! DFS iterators over a byte `PersistentARTrie`.
//!
//! Split out of byte `dict_impl.rs` (Phase-5 decomposition). The public
//! `TermIterator<V>` / `TermValueIterator<V>` types yield a pre-collected
//! snapshot of (term[, value]) pairs.
//!
//! **L3.3c:** the owned tree is gone, so these iterators are driven solely by the
//! pre-collected `from_terms` snapshot (produced from the overlay enumeration by
//! `public_iter::iter` / `iter_with_values`). The owned stack-based DFS (the
//! `IterState` enum + the `::new(&TrieRoot)` constructors that walked the owned
//! `TrieRoot` / `ChildNode` representation) was deleted.

use crate::value::DictionaryValue;

/// Iterator over all terms in a PersistentARTrie.
///
/// Yields the pre-collected snapshot in lexicographic order.
///
/// # Example
///
/// ```text
/// use libdictenstein::persistent_artrie::PersistentARTrie;
///
/// let mut dict = PersistentARTrie::new();
/// dict.insert("apple");
/// dict.insert("banana");
///
/// for term in dict.iter() {
///     println!("{}", String::from_utf8_lossy(&term));
/// }
/// ```
pub struct TermIterator<V: DictionaryValue> {
    /// Pre-collected snapshot used by disk-aware public iteration.
    precollected: std::vec::IntoIter<Vec<u8>>,
    /// Marker for value type
    _marker: std::marker::PhantomData<V>,
}

impl<V: DictionaryValue> TermIterator<V> {
    /// Create an iterator from a pre-collected snapshot.
    pub(super) fn from_terms(terms: Vec<Vec<u8>>) -> Self {
        Self {
            precollected: terms.into_iter(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<V: DictionaryValue> Iterator for TermIterator<V> {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        self.precollected.next()
    }
}

/// Iterator over all terms with their values in a PersistentARTrie.
///
/// Yields the pre-collected (term, value) snapshot in lexicographic order.
pub struct TermValueIterator<V: DictionaryValue> {
    /// Pre-collected snapshot used by disk-aware public iteration.
    precollected: std::vec::IntoIter<(Vec<u8>, Option<V>)>,
    /// Marker for value type
    _marker: std::marker::PhantomData<V>,
}

impl<V: DictionaryValue> TermValueIterator<V> {
    /// Create an iterator from a pre-collected snapshot.
    pub(super) fn from_terms(terms: Vec<(Vec<u8>, Option<V>)>) -> Self {
        Self {
            precollected: terms.into_iter(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<V: DictionaryValue> Iterator for TermValueIterator<V> {
    type Item = (Vec<u8>, Option<V>);

    fn next(&mut self) -> Option<Self::Item> {
        self.precollected.next()
    }
}
