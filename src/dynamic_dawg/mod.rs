//! Dynamic DAWG dictionary family — incrementally updatable automata.
//!
//! - [`ascii`] — byte-level (`u8`) [`DynamicDawg`].
//! - [`char`] — Unicode (`char`) [`DynamicDawgChar`].
//! - [`u64`] — `u64`-labeled [`DynamicDawgU64`] (time-series / sequence keys).
//! - [`zipper`] / [`char_zipper`] / [`u64_zipper`] — zipper navigators.
//! - [`core`] — the unit-generic minimization core ([`DawgCore`], [`DawgNode`])
//!   shared by all three variants.

pub mod ascii;
pub mod char;
pub mod char_zipper;
pub mod core;
pub mod u64;
pub mod u64_zipper;
pub mod zipper;

pub use ascii::{DynamicDawg, DynamicDawgNode};
pub use char::{DynamicDawgChar, DynamicDawgCharNode};
pub use char_zipper::DynamicDawgCharZipper;
// `self::` disambiguates the child module `core` from the `core` crate.
pub use self::core::{DawgCore, DawgNode};
pub use u64::{DynamicDawgU64, DynamicDawgU64Node};
pub use u64_zipper::DynamicDawgU64Zipper;
pub use zipper::DynamicDawgZipper;
