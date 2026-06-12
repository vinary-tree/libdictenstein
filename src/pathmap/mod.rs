//! PathMap-backed dictionary family (feature `pathmap-backend`).
//!
//! - [`ascii`] — byte-level (`u8`) [`PathMapDictionary`] (mutable, `RwLock`-guarded).
//! - [`char`] — Unicode (`char`) [`PathMapDictionaryChar`].
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
