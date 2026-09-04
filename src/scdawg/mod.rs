//! Compressed Suffix-DAWG (SCDAWG) dictionary family.
//!
//! - [`ascii`] — byte-level (`u8`) [`Scdawg`].
//! - [`mod@char`] — Unicode (`char`) [`ScdawgChar`].
//! - [`core`] — the unit-generic substring-automaton core shared by both.

pub mod ascii;
pub mod char;
pub mod core;
pub(crate) mod lockfree;

pub use ascii::{Scdawg, ScdawgNodeHandle};
pub use char::{ScdawgChar, ScdawgCharNodeHandle};

#[cfg(test)]
mod profile_tests {
    use super::ScdawgChar;
    use crate::{AtomSequence, UnicodeScalar};

    #[test]
    fn unicode_profile_sequences_preserve_suffix_units() {
        let dictionary: ScdawgChar = ScdawgChar::from_atom_sequences::<UnicodeScalar, _>([
            AtomSequence::<UnicodeScalar>::from_atoms(['λ', 'x', 'y']),
        ]);
        assert!(dictionary.contains_substring("λx"));
        assert!(dictionary.contains_substring("xy"));
    }

    #[test]
    fn unicode_profile_sequences_preserve_mapped_values() {
        let dictionary =
            ScdawgChar::<u16>::from_atom_sequences_with_values::<UnicodeScalar, _>([(
                AtomSequence::<UnicodeScalar>::from_atoms(['λ', 'x']),
                8,
            )]);
        assert_eq!(dictionary.get_value("λx"), Some(8));
    }
}
