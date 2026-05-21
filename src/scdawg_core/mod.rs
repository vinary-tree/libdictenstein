//! Generic SCDAWG (compact suffix DAWG) core shared between
//! [`crate::scdawg::Scdawg`] (byte-keyed) and
//! [`crate::scdawg_char::ScdawgChar`] (char-keyed).
//!
//! # Background
//!
//! The byte and char SCDAWG variants used to ship two
//! byte-for-byte-identical `ScdawgNode` struct definitions plus
//! identical 4-method `impl` blocks (root/new/get_edge/set_edge),
//! differing only in the label type (`u8` vs `char`). This module
//! exposes a single generic [`ScdawgNode<U, V>`] (and its impl) so the
//! variants share the node-shape and node-API entirely.
//!
//! The larger [`ScdawgInner<V>`] (each variant's mutable state plus the
//! batch-construction algorithm and the IS-features find /
//! match_positions / count_substring) stays per-variant for now — see
//! `docs/benchmarks/c4-scdawg-core-handoff.md` for the full
//! generification plan.

pub mod node;

pub use node::{ScdawgNode, NIL};
