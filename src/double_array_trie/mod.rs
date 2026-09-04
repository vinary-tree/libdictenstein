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

impl<V: crate::DictionaryValue> DoubleArrayTrieUleb128<V> {
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

    /// Test membership of one complete ULEB128 sequence.
    #[inline]
    pub fn contains(&self, sequence: &crate::Uleb128Sequence) -> bool {
        self.inner.contains_bytes(&sequence.to_encoded())
    }

    /// Read a mapped value for one complete ULEB128 sequence.
    #[inline]
    pub fn get_value(&self, sequence: &crate::Uleb128Sequence) -> Option<V> {
        self.inner.get_bytes_value(&sequence.to_encoded())
    }

    /// Number of visible logical sequences.
    #[inline]
    pub fn term_count(&self) -> usize {
        self.inner.len().unwrap_or(0)
    }
}

#[cfg(test)]
mod profile_tests {
    use super::{DoubleArrayTrie, DoubleArrayTrieChar, DoubleArrayTrieUleb128};
    use crate::{AtomSequence, Bytes, Dictionary, UnicodeScalar};

    #[test]
    fn uleb_wrapper_preserves_logical_sequences() {
        let sequence = crate::Uleb128Sequence::from_atoms([
            crate::Uleb128::from_u64(624_485),
            crate::Uleb128::from_u64(7),
        ]);
        let dictionary =
            DoubleArrayTrieUleb128::<u16>::from_sequences_with_values([(sequence.clone(), 19)]);
        assert!(dictionary.contains(&sequence));
        assert_eq!(dictionary.get_value(&sequence), Some(19));
        assert_eq!(dictionary.term_count(), 1);
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
