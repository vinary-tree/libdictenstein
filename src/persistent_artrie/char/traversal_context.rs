//! TraversalContext for char trie — re-export from core.
//!
//! The original char-local copy was a near-duplicate of byte's copy
//! (similarity 0.98 per audit, only differences were doc-comment
//! examples and import paths). Phase-3 Move-2: collapsed both
//! variants' local copies into a single shared implementation at
//! `persistent_artrie::core::traversal_context`.

pub use crate::persistent_artrie::core::traversal_context::*;
