//! Double-Array Trie (DAT) dictionary family — fast read-only static dictionaries.
//!
//! - [`ascii`] — byte-level (`u8`) [`DoubleArrayTrie`] (+ [`DoubleArrayTrieBuilder`]).
//! - [`char`] — Unicode (`char`) [`DoubleArrayTrieChar`].
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
