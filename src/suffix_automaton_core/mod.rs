//! Generic suffix automaton core shared between
//! [`crate::suffix_automaton::SuffixAutomaton`] (byte-keyed) and
//! [`crate::suffix_automaton_char::SuffixAutomatonChar`] (char-keyed).
//!
//! # Background
//!
//! The byte and char suffix-automaton variants used to ship two
//! byte-for-byte-identical `SuffixNode` struct definitions plus identical
//! 5-method `impl` blocks (root/new/find_edge/add_edge/update_edge),
//! differing only in the edge-label type (`u8` vs `char`). This module
//! exposes a single generic [`SuffixNode<U, V>`] (and its impl) so the
//! variants share the node-shape and node-API entirely.
//!
//! The larger [`SuffixAutomatonInner<V>`] (each variant's mutable state
//! machine wrapped in an `Arc<RwLock<…>>`) and the on-line `extend()`
//! construction algorithm stay per-variant for now — see
//! `docs/benchmarks/c3-suffix-automaton-core-handoff.md` for the full
//! generification plan.

pub mod node;

pub use node::SuffixNode;
