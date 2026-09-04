//! PathMap-backed dictionary family (feature `pathmap-backend`).
//!
//! - [`ascii`] — byte-level (`u8`) [`PathMapDictionary`] (mutable, atomic snapshot).
//! - [`mod@char`] — Unicode (`char`) [`PathMapDictionaryChar`].
//! - [`zipper`] — [`PathMapZipper`] navigator.
//! - [`core`] — the lock-free `TrieRef` substrate ([`TrieRefLike`], [`TrieRefNode`],
//!   [`TrieRefNodeChar`]) that all PathMap nodes/zippers descend through.
//! - [`snapshot`] — zero-plumbing, MORK-facing dictionaries ([`PathMapSnapshot`],
//!   [`PathMapRef`], and their `Char` variants) for querying a borrowed or
//!   `𝒪(1)`-snapshotted `PathMap` directly.

pub mod ascii;
pub mod char;
pub mod core;
pub mod snapshot;
pub mod zipper;

use crate::{Dictionary, DictionaryEntries};

pub use self::core::{
    trie_ref_root, trie_ref_root_borrowed, TrieRefLike, TrieRefNode, TrieRefNodeChar,
};
pub use ascii::{PathMapDictionary, PathMapNode};
pub use char::{PathMapDictionaryChar, PathMapNodeChar};
pub use snapshot::{PathMapRef, PathMapRefChar, PathMapSnapshot, PathMapSnapshotChar};
pub use zipper::PathMapZipper;

/// PathMap adapter boundary for canonical variable-width ULEB128 sequences.
/// The third-party map remains byte-backed; this type exposes only complete
/// logical sequences and never publishes continuation bytes as symbols.
#[derive(Clone)]
pub struct PathMapDictionaryUleb128<V: crate::DictionaryValue = ()> {
    inner: PathMapDictionary<V>,
}

/// PathMap adapter boundary for variable-width UTF-8 strings.
#[derive(Clone)]
pub struct PathMapDictionaryUtf8<V: crate::DictionaryValue = ()> {
    inner: PathMapDictionary<V>,
}

impl<V: crate::DictionaryValue> Default for PathMapDictionaryUtf8<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: crate::DictionaryValue> PathMapDictionaryUtf8<V> {
    pub fn new() -> Self {
        Self {
            inner: PathMapDictionary::new(),
        }
    }
    pub fn from_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let dictionary = Self::new();
        for term in terms {
            dictionary.insert(term.as_ref());
        }
        dictionary
    }
    pub fn from_terms_with_values<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        let dictionary = Self::new();
        for (term, value) in entries {
            dictionary.insert_with_value(term.as_ref(), value);
        }
        dictionary
    }
    #[inline]
    pub fn insert(&self, term: &str) -> bool {
        self.inner.insert_bytes(term.as_bytes())
    }
    #[inline]
    pub fn insert_with_value(&self, term: &str, value: V) -> bool {
        self.inner.insert_bytes_with_value(term.as_bytes(), value)
    }
    #[inline]
    pub fn contains(&self, term: &str) -> bool {
        self.inner.contains_bytes(term.as_bytes())
    }
    #[inline]
    pub fn get_value(&self, term: &str) -> Option<V> {
        self.inner.get_bytes_value(term.as_bytes())
    }
    #[inline]
    pub fn remove(&self, term: &str) -> bool {
        self.inner.remove_bytes(term.as_bytes())
    }
    #[inline]
    pub fn term_count(&self) -> usize {
        self.inner.len().unwrap_or(0)
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.term_count() == 0
    }
    pub fn contains_encoded(&self, encoded: &[u8]) -> Result<bool, std::str::Utf8Error> {
        std::str::from_utf8(encoded)?;
        Ok(self.inner.contains_bytes(encoded))
    }

    pub fn remove_encoded(&self, encoded: &[u8]) -> Result<bool, std::str::Utf8Error> {
        std::str::from_utf8(encoded)?;
        Ok(self.inner.remove_bytes(encoded))
    }
    pub fn visible_entries(&self) -> Result<Vec<(String, Option<V>)>, std::str::Utf8Error> {
        self.inner
            .entries()
            .map(|entry| std::str::from_utf8(&entry.key).map(|s| (s.to_owned(), entry.value)))
            .collect()
    }
}

impl<V: crate::DictionaryValue> PathMapDictionaryUleb128<V> {
    /// Construct an empty ULEB128 PathMap adapter.
    pub fn new() -> Self {
        Self {
            inner: PathMapDictionary::new(),
        }
    }

    /// Build a value-bearing adapter from complete canonical ULEB sequences.
    pub fn from_sequences_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (crate::Uleb128Sequence, V)>,
    {
        let dictionary = Self::new();
        for (sequence, value) in entries {
            dictionary.insert_with_value(&sequence, value);
        }
        dictionary
    }

    /// Build an unvalued adapter from complete canonical ULEB sequences.
    pub fn from_sequences<I>(sequences: I) -> Self
    where
        I: IntoIterator<Item = crate::Uleb128Sequence>,
    {
        let dictionary = Self::new();
        for sequence in sequences {
            dictionary.insert(&sequence);
        }
        dictionary
    }

    /// Insert one complete ULEB128 sequence.
    #[inline]
    pub fn insert(&self, sequence: &crate::Uleb128Sequence) -> bool {
        self.inner.insert_bytes(&sequence.to_encoded())
    }

    /// Insert one complete ULEB128 sequence with a mapped value.
    #[inline]
    pub fn insert_with_value(&self, sequence: &crate::Uleb128Sequence, value: V) -> bool {
        self.inner
            .insert_bytes_with_value(&sequence.to_encoded(), value)
    }

    /// Test one complete ULEB128 sequence.
    #[inline]
    pub fn contains(&self, sequence: &crate::Uleb128Sequence) -> bool {
        self.inner.contains_bytes(&sequence.to_encoded())
    }

    /// Test a complete canonical encoded sequence without materializing its
    /// decoded atoms.  Malformed or non-canonical images are rejected.
    pub fn contains_encoded(&self, encoded: &[u8]) -> Result<bool, crate::Uleb128Error> {
        crate::Uleb128Sequence::from_encoded(encoded)?;
        Ok(self.inner.contains_bytes(encoded))
    }

    /// Read a mapped value for one complete ULEB128 sequence.
    #[inline]
    pub fn get_value(&self, sequence: &crate::Uleb128Sequence) -> Option<V> {
        self.inner.get_bytes_value(&sequence.to_encoded())
    }

    /// Read a value for a complete canonical encoded sequence without
    /// materializing its decoded atoms.
    pub fn get_encoded_value(&self, encoded: &[u8]) -> Result<Option<V>, crate::Uleb128Error> {
        crate::Uleb128Sequence::from_encoded(encoded)?;
        Ok(self.inner.get_bytes_value(encoded))
    }

    /// Remove one complete ULEB128 sequence.
    #[inline]
    pub fn remove(&self, sequence: &crate::Uleb128Sequence) -> bool {
        self.inner.remove_bytes(&sequence.to_encoded())
    }

    /// Remove a complete canonical encoded sequence without decoding it.
    pub fn remove_encoded(&self, encoded: &[u8]) -> Result<bool, crate::Uleb128Error> {
        crate::Uleb128Sequence::from_encoded(encoded)?;
        Ok(self.inner.remove_bytes(encoded))
    }

    /// Number of visible logical sequences in the current snapshot.
    #[inline]
    pub fn term_count(&self) -> usize {
        self.inner.len().unwrap_or(0)
    }

    /// Whether the current snapshot contains no logical sequences.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.term_count() == 0
    }

    /// Export complete logical sequences from one immutable PathMap snapshot.
    /// Continuation bytes are decoded only at this boundary and never exposed
    /// as semantic transitions.
    pub fn visible_entries(
        &self,
    ) -> Result<Vec<(crate::Uleb128Sequence, Option<V>)>, crate::Uleb128Error> {
        self.inner
            .entries()
            .map(|entry| {
                crate::Uleb128Sequence::from_encoded(&entry.key)
                    .map(|sequence| (sequence, entry.value))
            })
            .collect()
    }
}

impl<V: crate::DictionaryValue> Default for PathMapDictionaryUleb128<V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "pathmap-backend"))]
mod profile_tests {
    use super::{
        PathMapDictionary, PathMapDictionaryChar, PathMapDictionaryUleb128, PathMapDictionaryUtf8,
    };
    use crate::{AtomSequence, Bytes, Dictionary, UnicodeScalar};

    #[test]
    fn uleb_adapter_preserves_logical_sequences() {
        let sequence = crate::Uleb128Sequence::from_atoms([
            crate::Uleb128::from_u64(624_485),
            crate::Uleb128::from_u64(7),
        ]);
        let dictionary = PathMapDictionaryUleb128::<u16>::new();
        assert!(dictionary.insert_with_value(&sequence, 19));
        assert!(dictionary.contains(&sequence));
        assert_eq!(dictionary.get_value(&sequence), Some(19));
        assert!(dictionary.remove(&sequence));
        assert!(!dictionary.contains(&sequence));
    }

    #[test]
    fn uleb_adapter_builds_unvalued_sequences_and_rejects_malformed_images() {
        let sequence = crate::Uleb128Sequence::from_atoms([crate::Uleb128::from_u64(9)]);
        let dictionary = PathMapDictionaryUleb128::<()>::from_sequences([sequence.clone()]);
        assert_eq!(
            dictionary.contains_encoded(sequence.to_encoded().as_slice()),
            Ok(true)
        );
        assert!(dictionary.contains_encoded(&[0x80]).is_err());
        assert_eq!(dictionary.term_count(), 1);
        assert_eq!(dictionary.visible_entries().unwrap().len(), 1);
        assert!(dictionary
            .remove_encoded(sequence.to_encoded().as_slice())
            .unwrap());
        assert!(dictionary.is_empty());
    }

    #[test]
    fn utf8_adapter_preserves_logical_entries() {
        let dictionary =
            PathMapDictionaryUtf8::<u16>::from_terms_with_values([("λ🎉", 9), ("a", 1)]);
        assert!(dictionary.contains("λ🎉"));
        assert_eq!(dictionary.get_value("λ🎉"), Some(9));
        assert_eq!(dictionary.visible_entries().unwrap().len(), 2);
        assert!(dictionary.contains_encoded("λ🎉".as_bytes()).unwrap());
        assert!(dictionary.contains_encoded(&[0x80]).is_err());
        assert!(!dictionary.is_empty());
        assert!(dictionary.remove_encoded("λ🎉".as_bytes()).unwrap());
        assert!(!dictionary.contains("λ🎉"));
    }

    #[test]
    fn byte_profile_constructor_preserves_membership_and_values() {
        let dictionary = PathMapDictionary::<u16>::from_atom_sequences_with_values::<Bytes, _>([(
            AtomSequence::<Bytes>::from_atoms([b'a', b'b']),
            13,
        )]);
        assert!(dictionary.contains("ab"));
        assert_eq!(dictionary.get_value("ab"), Some(13));
    }

    #[test]
    fn byte_adapter_supports_arbitrary_encoded_keys() {
        let dictionary = PathMapDictionary::<u16>::new();
        assert!(dictionary.insert_bytes_with_value(&[0, 255, 1], 34));
        assert!(dictionary.contains_bytes(&[0, 255, 1]));
        assert_eq!(dictionary.get_bytes_value(&[0, 255, 1]), Some(34));
        let sequence = AtomSequence::<Bytes>::from_atoms([0, 255, 1]);
        assert_eq!(dictionary.get_atom_sequence_value(&sequence), Some(34));
        assert!(dictionary.remove_bytes(&[0, 255, 1]));
        assert!(!dictionary.contains_bytes(&[0, 255, 1]));
    }

    #[test]
    fn unicode_profile_constructor_preserves_scalar_boundaries_and_values() {
        let dictionary = PathMapDictionaryChar::<u16>::from_atom_sequences_with_values::<
            UnicodeScalar,
            _,
        >([(AtomSequence::<UnicodeScalar>::from_atoms(['λ', 'x']), 21)]);
        assert!(dictionary.contains("λx"));
        assert!(!dictionary.contains("lx"));
        assert_eq!(dictionary.get_value("λx"), Some(21));
        let sequence = AtomSequence::<UnicodeScalar>::from_atoms(['λ', 'x']);
        assert_eq!(dictionary.get_atom_sequence_value(&sequence), Some(21));
    }
}
