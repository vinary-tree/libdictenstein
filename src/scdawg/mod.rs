//! Compressed Suffix-DAWG (SCDAWG) dictionary family.
//!
//! - [`ascii`] — byte-level (`u8`) [`Scdawg`].
//! - [`char`] — Unicode (`char`) [`ScdawgChar`].
//! - [`core`] — the unit-generic substring-automaton core shared by both.

pub mod ascii;
pub mod char;
pub mod core;

pub use ascii::{Scdawg, ScdawgNodeHandle};
pub use char::{ScdawgChar, ScdawgCharNodeHandle};
