//! Shared substrate for the persistent ARTrie variants.
//!
//! `persistent_artrie_core` hosts the unit-agnostic infrastructure
//! shared by the three persistent ARTrie variants:
//!
//! - [`crate::persistent_artrie`] — byte / `u8` keys
//! - [`crate::persistent_artrie_char`] — UTF-8 / `u32` keys
//! - [`crate::persistent_vocab_artrie`] — vocabulary-specific, builds on char
//!
//! # Layering invariant
//!
//! `persistent_artrie_core` has **zero** upward dependencies on the variant
//! modules. Variants depend on core; core never depends on variants. This
//! invariant is verified by:
//!
//! ```text
//! grep -rn "crate::persistent_artrie_char\|crate::persistent_vocab" src/persistent_artrie_core/
//! ```
//!
//! which must return empty.
//!
//! # Migration
//!
//! Sub-modules are added incrementally as the multi-phase
//! `persistent_artrie_core` extraction proceeds. Until the extraction is
//! complete, the variant modules continue to re-export their original symbols
//! so existing call-sites need not change paths in lock-step with moves.

pub mod adaptive_pool;
pub mod arena_slot;
pub mod block_storage;
pub mod buffer_manager;
pub mod compact_encoding;
pub mod concurrency;
pub mod dirty_tracker;
pub mod disk_manager;
pub mod durability;
pub mod epoch;
pub mod error;
pub mod eviction;
#[cfg(feature = "group-commit")]
pub mod group_commit;
#[cfg(feature = "io-uring-backend")]
pub mod io_uring_disk_manager;
pub mod key_encoding;
pub mod memory_monitor;
pub mod mvcc;
/// G4: the shared lock-free `OverlayNode<K, V>` / `AtomicNodePtr<K, V>` aliased by
/// the byte/char overlays. Gated on `persistent-artrie` like the variant node
/// modules (it depends on `arc_swap` and the overlay machinery).
#[cfg(feature = "persistent-artrie")]
pub mod overlay;
pub mod prefetch;
pub mod recovery;
pub mod swizzled_ptr;
pub mod traversal_context;
pub mod version_checkpoint;
pub mod version_gc;
pub mod wal;
pub mod wal_managed;

// Top-level convenience re-exports used by sub-modules via `super::`.
pub use error::{PersistentARTrieError, Result, SwizzleError};
