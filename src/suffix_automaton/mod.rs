//! Suffix-Automaton dictionary family — substring (infix) search.
//!
//! - [`ascii`] — byte-level (`u8`) [`SuffixAutomaton`].
//! - [`char`] — Unicode (`char`) [`SuffixAutomatonChar`].
//! - [`zipper`] / [`char_zipper`] — zipper navigators for each.
//! - [`core`] — the unit-generic suffix-automaton core shared by both.

pub mod ascii;
pub mod char;
pub mod char_zipper;
pub mod core;
pub(crate) mod lockfree;
pub mod zipper;

pub use ascii::{SuffixAutomaton, SuffixNodeHandle};
pub use char::{SuffixAutomatonChar, SuffixNodeCharHandle};
pub use char_zipper::SuffixAutomatonCharZipper;
pub use zipper::SuffixAutomatonZipper;

#[cfg(feature = "persistent-artrie")]
pub use crate::persistent_artrie::{
    PersistentSuffixAutomaton, PersistentSuffixAutomatonChar, PersistentSuffixAutomatonCharNode,
    PersistentSuffixAutomatonNode,
};
