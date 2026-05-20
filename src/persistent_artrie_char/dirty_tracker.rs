//! DirtyTracker for char trie — re-export from core.
//!
//! The original char-local copy was a strict subset of
//! `persistent_artrie_core::dirty_tracker` (8 tests vs core's 11
//! — the 8 are a subset, and core has an additional
//! `mark_arenas_dirty` bulk API plus stronger memory ordering).
//! Phase-3 Move-2: deleted that 439-LOC duplicate per audit T2-2
//! ("share as-is, pure deletion of char copy") and replaced it
//! with a re-export so the existing public API and tests at
//! `crate::persistent_artrie_char::dirty_tracker::*` still resolve.

pub use crate::persistent_artrie_core::dirty_tracker::*;
