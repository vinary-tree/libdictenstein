//! Suffix-Automaton dictionary family — substring (infix) search.
//!
//! - [`ascii`] — byte-level (`u8`) [`SuffixAutomaton`].
//! - [`mod@char`] — Unicode (`char`) [`SuffixAutomatonChar`].
//! - [`zipper`] / [`char_zipper`] — zipper navigators for each.
//! - [`core`] — the unit-generic suffix-automaton core shared by both.

pub mod ascii;
pub mod char;
pub mod char_zipper;
pub mod core;
pub(crate) mod lockfree;
pub mod zipper;

pub use ascii::{SuffixAutomaton, SuffixNodeHandle};
pub use char::{SuffixAutomatonChar, SuffixAutomatonUtf8, SuffixNodeCharHandle};
pub use char_zipper::SuffixAutomatonCharZipper;
pub use zipper::SuffixAutomatonZipper;

#[cfg(test)]
mod profile_tests {
    use super::SuffixAutomatonChar;
    use crate::{AtomSequence, Dictionary, UnicodeScalar};

    #[test]
    fn unicode_profile_sequences_preserve_substring_boundaries() {
        let dictionary: SuffixAutomatonChar = SuffixAutomatonChar::from_atom_sequences::<
            UnicodeScalar,
            _,
        >([
            AtomSequence::<UnicodeScalar>::from_atoms(['λ', 'x', 'y']),
        ]);
        assert!(dictionary.contains("λx"));
        assert!(dictionary.contains("xy"));
    }
}

#[cfg(feature = "persistent-artrie")]
pub use crate::persistent_artrie::{
    PersistentSuffixAutomaton, PersistentSuffixAutomatonChar, PersistentSuffixAutomatonCharNode,
    PersistentSuffixAutomatonNode,
};
