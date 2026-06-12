//! Generic SCDAWG (compact suffix DAWG) core shared between
//! [`super::ascii::Scdawg`] (byte-keyed) and
//! [`super::char::ScdawgChar`] (char-keyed).
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
//! The larger [`ScdawgCoreInner<U, V>`] hosts the on-line construction
//! state machine (Blumer et al. 1987's `sa_extend`), the post-pass
//! `compute_left_edges`, and the IS-features (`find_substring_fast`,
//! `contains_substring`, `find_exact_substring`, `frequency`,
//! `count_occurrences`). Both byte and char SCDAWG variants alias to it
//! after the C4a/C4b/C4c migration.

pub mod inner;
pub mod node;

pub use inner::ScdawgCoreInner;
pub use node::{ScdawgNode, NIL};
