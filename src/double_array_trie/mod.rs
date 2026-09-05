//! Double-Array Trie (DAT) dictionary family — fast read-only static dictionaries.
//!
//! - [`ascii`] — byte-level (`u8`) [`DoubleArrayTrie`] (+ [`DoubleArrayTrieBuilder`]).
//! - [`mod@char`] — Unicode (`char`) [`DoubleArrayTrieChar`].
//! - [`zipper`] / [`char_zipper`] — zipper navigators for each.
//! - [`core`] — the unit-generic double-array storage shared by both.

pub mod ascii;
pub mod char;
pub mod char_zipper;
pub mod core;
pub mod zipper;

use crate::DictionaryEntries;

pub use ascii::{DoubleArrayTrie, DoubleArrayTrieBuilder, DoubleArrayTrieNode};
pub use char::{DoubleArrayTrieChar, DoubleArrayTrieCharNode};
pub use char_zipper::DoubleArrayTrieCharZipper;
pub use zipper::DoubleArrayTrieZipper;

/// Immutable DAT boundary for canonical variable-width ULEB128 sequences.
/// Physical byte edges remain private; callers observe complete logical
/// sequences only.
#[derive(Clone, Debug)]
pub struct DoubleArrayTrieUleb128<V: crate::DictionaryValue = ()> {
    inner: DoubleArrayTrie<V>,
}

/// Immutable byte-backed DAT boundary for variable-width UTF-8 strings.
#[derive(Clone, Debug)]
pub struct DoubleArrayTrieUtf8<V: crate::DictionaryValue = ()> {
    inner: DoubleArrayTrie<V>,
}

impl<V: crate::DictionaryValue> Default for DoubleArrayTrieUtf8<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: crate::DictionaryValue> DoubleArrayTrieUtf8<V> {
    pub fn new() -> Self {
        Self {
            inner: DoubleArrayTrie::new(),
        }
    }
    pub fn from_terms<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            inner: terms
                .into_iter()
                .map(|s| s.as_ref().as_bytes().to_vec())
                .collect(),
        }
    }
    pub fn from_terms_with_values<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, V)>,
        S: AsRef<str>,
    {
        Self {
            inner: entries
                .into_iter()
                .map(|(s, v)| (s.as_ref().as_bytes().to_vec(), v))
                .collect(),
        }
    }

    /// Build from shared logical UTF-8 scalar profile sequences.
    pub fn from_atom_sequences<I>(sequences: I) -> Self
    where
        I: IntoIterator<Item = crate::AtomSequence<crate::Utf8>>,
    {
        Self {
            inner: sequences
                .into_iter()
                .map(|sequence| sequence.to_encoded())
                .collect(),
        }
    }

    /// Build a value-bearing DAT from shared logical UTF-8 scalar profile
    /// sequences.
    pub fn from_atom_sequences_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (crate::AtomSequence<crate::Utf8>, V)>,
    {
        Self {
            inner: entries
                .into_iter()
                .map(|(sequence, value)| (sequence.to_encoded(), value))
                .collect(),
        }
    }
    #[inline]
    pub fn contains(&self, term: &str) -> bool {
        self.inner.contains_bytes(term.as_bytes())
    }

    /// Test membership of one shared logical UTF-8 scalar profile sequence.
    #[inline]
    pub fn contains_atom_sequence(&self, sequence: &crate::AtomSequence<crate::Utf8>) -> bool {
        self.inner.contains_bytes(&sequence.to_encoded())
    }
    #[inline]
    pub fn get_value(&self, term: &str) -> Option<V> {
        self.inner.get_bytes_value(term.as_bytes())
    }

    /// Read a mapped value for one shared logical UTF-8 scalar profile
    /// sequence.
    #[inline]
    pub fn get_atom_sequence_value(
        &self,
        sequence: &crate::AtomSequence<crate::Utf8>,
    ) -> Option<V> {
        self.inner.get_bytes_value(&sequence.to_encoded())
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
    pub fn visible_entries(&self) -> Result<Vec<(String, Option<V>)>, std::str::Utf8Error> {
        self.inner
            .entries()
            .map(|entry| std::str::from_utf8(&entry.key).map(|s| (s.to_owned(), entry.value)))
            .collect()
    }
}

impl<V: crate::DictionaryValue> DoubleArrayTrieUleb128<V> {
    /// Construct an empty ULEB128 DAT.
    pub fn new() -> Self {
        Self {
            inner: DoubleArrayTrie::new(),
        }
    }

    /// Build from complete canonical ULEB128 sequences.
    pub fn from_sequences<I>(sequences: I) -> Self
    where
        I: IntoIterator<Item = crate::Uleb128Sequence>,
    {
        Self {
            inner: sequences
                .into_iter()
                .map(|sequence| sequence.to_encoded())
                .collect(),
        }
    }

    /// Build from the shared logical ULEB profile sequence representation.
    pub fn from_atom_sequences<I>(sequences: I) -> Self
    where
        I: IntoIterator<Item = crate::AtomSequence<crate::Uleb128Atom>>,
    {
        Self::from_sequences(sequences.into_iter().map(Into::into))
    }

    /// Build a value-bearing DAT from complete canonical ULEB128 sequences.
    pub fn from_sequences_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (crate::Uleb128Sequence, V)>,
    {
        Self {
            inner: entries
                .into_iter()
                .map(|(sequence, value)| (sequence.to_encoded(), value))
                .collect(),
        }
    }

    /// Build a value-bearing DAT from shared logical ULEB profile sequences.
    pub fn from_atom_sequences_with_values<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (crate::AtomSequence<crate::Uleb128Atom>, V)>,
    {
        Self::from_sequences_with_values(
            entries
                .into_iter()
                .map(|(sequence, value)| (sequence.into(), value)),
        )
    }

    /// Test membership of one complete ULEB128 sequence.
    #[inline]
    pub fn contains(&self, sequence: &crate::Uleb128Sequence) -> bool {
        self.inner.contains_bytes(&sequence.to_encoded())
    }

    /// Test membership of one shared logical ULEB profile sequence.
    #[inline]
    pub fn contains_atom_sequence(
        &self,
        sequence: &crate::AtomSequence<crate::Uleb128Atom>,
    ) -> bool {
        self.inner.contains_bytes(&sequence.to_encoded())
    }

    /// Test a complete canonical encoded sequence without materializing its
    /// decoded atoms.  Malformed or non-canonical images are rejected.
    pub fn contains_encoded(&self, encoded: &[u8]) -> Result<bool, crate::Uleb128Error> {
        crate::validate_uleb128_sequence(encoded)?;
        Ok(self.inner.contains_bytes(encoded))
    }

    /// Read a mapped value for one complete ULEB128 sequence.
    #[inline]
    pub fn get_value(&self, sequence: &crate::Uleb128Sequence) -> Option<V> {
        self.inner.get_bytes_value(&sequence.to_encoded())
    }

    /// Read a mapped value for one shared logical ULEB profile sequence.
    #[inline]
    pub fn get_atom_sequence_value(
        &self,
        sequence: &crate::AtomSequence<crate::Uleb128Atom>,
    ) -> Option<V> {
        self.inner.get_bytes_value(&sequence.to_encoded())
    }

    /// Read a value for a complete canonical encoded sequence without
    /// materializing its decoded atoms.
    pub fn get_encoded_value(&self, encoded: &[u8]) -> Result<Option<V>, crate::Uleb128Error> {
        crate::validate_uleb128_sequence(encoded)?;
        Ok(self.inner.get_bytes_value(encoded))
    }

    /// Export complete logical sequences from the immutable DAT.
    /// Traversal remains iterative in the byte-backed core; decoding occurs
    /// only at this boundary and malformed images are rejected.
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

    /// Number of visible logical sequences.
    #[inline]
    pub fn term_count(&self) -> usize {
        self.inner.len().unwrap_or(0)
    }

    /// Whether no logical ULEB sequences are present.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.term_count() == 0
    }
}

impl<V: crate::DictionaryValue> Default for DoubleArrayTrieUleb128<V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod profile_tests {
    use super::{
        DoubleArrayTrie, DoubleArrayTrieChar, DoubleArrayTrieUleb128, DoubleArrayTrieUtf8,
    };
    use crate::{AtomSequence, Bytes, Dictionary, UnicodeScalar};

    #[test]
    fn uleb_wrapper_preserves_logical_sequences() {
        assert!(DoubleArrayTrieUleb128::<u16>::new().is_empty());
        let sequence = crate::Uleb128Sequence::from_atoms([
            crate::Uleb128::from_u64(624_485),
            crate::Uleb128::from_u64(7),
        ]);
        let dictionary =
            DoubleArrayTrieUleb128::<u16>::from_sequences_with_values([(sequence.clone(), 19)]);
        assert!(dictionary.contains(&sequence));
        assert_eq!(dictionary.get_value(&sequence), Some(19));
        assert_eq!(dictionary.term_count(), 1);
        assert_eq!(
            dictionary.contains_encoded(sequence.to_encoded().as_slice()),
            Ok(true)
        );
        assert!(dictionary.get_encoded_value(&[0x80]).is_err());
        assert_eq!(dictionary.visible_entries().unwrap().len(), 1);
    }

    #[test]
    fn uleb_wrapper_accepts_shared_profile_sequences() {
        let sequence = crate::AtomSequence::<crate::Uleb128Atom>::from_atoms([
            crate::Uleb128::from_u64(624_485),
            crate::Uleb128::from_u64(1u64 << 63),
        ]);
        let dictionary = DoubleArrayTrieUleb128::<u16>::from_atom_sequences_with_values([(
            sequence.clone(),
            23,
        )]);
        assert!(dictionary.contains_atom_sequence(&sequence));
        assert_eq!(dictionary.get_atom_sequence_value(&sequence), Some(23));
    }

    #[test]
    fn utf8_wrapper_accepts_shared_profile_sequences() {
        let sequence = crate::AtomSequence::<crate::Utf8>::from_atoms(['λ', '🎉']);
        let dictionary =
            DoubleArrayTrieUtf8::<u16>::from_atom_sequences_with_values([(sequence.clone(), 29)]);
        assert!(dictionary.contains_atom_sequence(&sequence));
        assert_eq!(dictionary.get_atom_sequence_value(&sequence), Some(29));
    }

    #[test]
    fn utf8_wrapper_preserves_logical_entries() {
        let dictionary = DoubleArrayTrieUtf8::<u16>::from_terms_with_values([("λ🎉", 9), ("a", 1)]);
        assert!(dictionary.contains("λ🎉"));
        assert_eq!(dictionary.get_value("λ🎉"), Some(9));
        assert_eq!(dictionary.visible_entries().unwrap().len(), 2);
        assert!(dictionary.contains_encoded("λ🎉".as_bytes()).unwrap());
        assert!(dictionary.contains_encoded(&[0x80]).is_err());
    }

    #[test]
    fn byte_profile_sequences_use_the_existing_dat_builder() {
        let dictionary: DoubleArrayTrie = DoubleArrayTrie::from_atom_sequences::<Bytes, _>([
            AtomSequence::<Bytes>::from_atoms([b'a', b'b']),
            AtomSequence::<Bytes>::from_atoms([b'a', b'c']),
        ]);
        assert!(dictionary.contains("ab"));
        assert!(dictionary.contains("ac"));
    }

    #[test]
    fn byte_profile_sequences_preserve_values() {
        let dictionary = DoubleArrayTrie::<u16>::from_atom_sequences_with_values::<Bytes, _>([(
            AtomSequence::<Bytes>::from_atoms([0, 255]),
            9,
        )]);
        assert_eq!(dictionary.get_bytes_value(&[0, 255]), Some(9));
        let sequence = AtomSequence::<Bytes>::from_atoms([0, 255]);
        assert_eq!(dictionary.get_atom_sequence_value(&sequence), Some(9));
    }

    #[test]
    fn unicode_profile_sequences_preserve_scalar_boundaries() {
        let dictionary: DoubleArrayTrieChar = DoubleArrayTrieChar::from_atom_sequences::<
            UnicodeScalar,
            _,
        >([
            AtomSequence::<UnicodeScalar>::from_atoms(['λ', 'x']),
        ]);
        assert!(dictionary.contains("λx"));
        assert!(!dictionary.contains("lx"));
    }

    #[test]
    fn unicode_profile_sequences_preserve_values() {
        let dictionary = DoubleArrayTrieChar::<u16>::from_atom_sequences_with_values::<
            UnicodeScalar,
            _,
        >([(AtomSequence::<UnicodeScalar>::from_atoms(['λ']), 7)]);
        assert_eq!(dictionary.get_value("λ"), Some(7));
        assert_eq!(dictionary.get_chars_value(&['λ']), Some(7));
        let sequence = AtomSequence::<UnicodeScalar>::from_atoms(['λ']);
        assert_eq!(dictionary.get_atom_sequence_value(&sequence), Some(7));
    }
}
