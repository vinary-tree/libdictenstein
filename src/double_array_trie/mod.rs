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

#[cfg(test)]
mod profile_tests {
    use super::{DoubleArrayTrie, DoubleArrayTrieChar};
    use crate::{AtomSequence, Bytes, Dictionary, UnicodeScalar};

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
