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

pub use self::core::{
    trie_ref_root, trie_ref_root_borrowed, TrieRefLike, TrieRefNode, TrieRefNodeChar,
};
pub use ascii::{PathMapDictionary, PathMapNode};
pub use char::{PathMapDictionaryChar, PathMapNodeChar};
pub use snapshot::{PathMapRef, PathMapRefChar, PathMapSnapshot, PathMapSnapshotChar};
pub use zipper::PathMapZipper;

#[cfg(all(test, feature = "pathmap-backend"))]
mod profile_tests {
    use super::{PathMapDictionary, PathMapDictionaryChar};
    use crate::{AtomSequence, Bytes, Dictionary, UnicodeScalar};

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
    }
}
