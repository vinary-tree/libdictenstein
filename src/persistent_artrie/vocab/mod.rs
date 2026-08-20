//! Persistent Vocabulary ARTrie — a lock-free, overlay-only UTF-8 vocabulary (V6).
//!
//! This module provides [`PersistentVocabARTrie`], a specialized UTF-8 vocabulary trie whose
//! SOLE representation is the lock-free overlay (`PersistentCharNode` with structural sharing,
//! at `V = u64` = the vocabulary index). The owned parent-pointer tree and reverse lookup
//! side table were deleted in V6 (the single-lock-free transition).
//!
//! - **Forward lookup** (term → index): O(k) walk of the lock-free overlay
//! - **Reverse lookup** (index → term): O(1) via the in-memory `reverse_term_map` (id → term),
//!   rebuilt from the checkpoint image on reopen
//!
//! # Design
//!
//! The vocabulary IS the lock-free overlay (a `PersistentCharNode` trie with structural sharing,
//! at `V = u64`) plus an in-memory `reverse_term_map` (id → term) for reverse lookups. Inserts
//! are `&self`-concurrent durable Order-A operations (WAL `Insert{value:id}` → overlay root-CAS →
//! CommitRank → mark_committed) — many threads may insert through a shared `Arc` with no external
//! locking (the single lock-free impl: no `install_overlay` toggle, no `ConcurrentVocabARTrie`
//! wrapper). A checkpoint publishes the overlay as a dense char-arena image (`vocabulary.vocab`),
//! RETAINING the WAL (`vocabulary.vocab.wal`) for crash recovery; the reverse map is rebuilt from
//! the image on reopen.
//!
//! # Example
//!
//! ```rust,no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
//!
//! // Create a new vocabulary
//! let mut vocab = PersistentVocabARTrie::create("vocab.vocab")?;
//!
//! // Insert terms (auto-assigns indices)
//! let idx1 = vocab.insert("hello")?; // Returns 0
//! let idx2 = vocab.insert("world")?; // Returns 1
//!
//! // Forward lookup: term → index
//! assert_eq!(vocab.get_index("hello"), Some(0));
//! assert_eq!(vocab.get_index("world"), Some(1));
//!
//! // Reverse lookup: index → term through reverse_term_map
//! assert_eq!(vocab.get_term(0), Some("hello".to_string()));
//! assert_eq!(vocab.get_term(1), Some("world".to_string()));
//!
//! // Sync WAL for durability
//! vocab.sync()?;
//!
//! // Checkpoint to disk
//! vocab.checkpoint()?;
//!
//! // Reopen later with crash recovery
//! let (vocab, report) = PersistentVocabARTrie::open_with_recovery("vocab.vocab")?;
//! assert_eq!(vocab.get_term(0), Some("hello".to_string()));
//! # Ok(())
//! # }
//! ```
//!
//! # Performance
//!
//! | Operation | Complexity | Notes |
//! |-----------|------------|-------|
//! | Forward lookup (`get_index`) | O(k) | overlay walk, k = term length |
//! | Reverse lookup (`get_term`) | O(1) | in-memory `reverse_term_map` |
//! | Insert (`&self`, concurrent) | O(k) | durable Order-A, lock-free CAS |

// Core types
pub mod types;

// Vocabulary-specific file-header reader (relocated out of
// `persistent_artrie::core::block_storage` so core stays free of variant deps).
pub mod header;

// VocabSyncHandle (Phase-6 split out of dict_impl).
pub mod sync_handle;

// IoUringDiskManager-specific constructors (Phase-6 split out of dict_impl).
#[cfg(feature = "io-uring-backend")]
pub mod io_uring_ctor;

// MmapDiskManager-specific constructors (Phase-6 split out of dict_impl).
pub mod mmap_ctor;

// Lock-free CAS-based concurrent inserts (Phase-6 split out of dict_impl).
pub mod lockfree_cas;

// Vocab overlay-flip seam impls (V1 — the shared overlay traits at V=u64).
pub(crate) mod overlay_write_mode;

// Overlay → disk serializer (V2 — the char-arena image writer for the flip).
pub(crate) mod overlay_serialize;

// Persistence/durability/observability API (Phase-6 split out of dict_impl).
pub mod persistence_api;

// Public query API (get_index, get_term, contains, len) — Phase-6 split.
pub mod query_api;

// Public mutation API (insert / insert_batch / insert_with_index) — Phase-6 split.
pub mod mutation_api;

// Path-based queries + iter_terms wrappers — Phase-6 split.
pub mod path_query;

// Disk-backed implementation
pub mod dict_impl;

// Re-export main types
pub use types::{
    NodeRef, // Re-export from persistent_artrie::char
    VocabTrieFileHeader,
    DEFAULT_VOCAB_BUFFER_POOL_SIZE,
    VOCAB_FILE_HEADER_SIZE,
    VOCAB_HEADER_VERSION_V2,
    VOCAB_TRIE_MAGIC,
};

pub use dict_impl::{PersistentVocabARTrie, SharedVocabARTrie, VocabSyncHandle};

// Re-export DurabilityPolicy from base layer
pub use crate::persistent_artrie::dict_impl::DurabilityPolicy;

// Re-export eviction types from byte-level implementation (shared)
pub use crate::persistent_artrie::eviction::{
    AccessTracker, DiskLocationRegistry, EvictionConfig, EvictionCoordinator, EvictionStats,
    EvictionUrgency, LruRegistry,
};

// ============================================================================
// Trait Implementations
// ============================================================================

use crate::bijective::BijectiveDictionary;
use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::core::key_encoding::CharKey;
use crate::persistent_artrie::core::overlay::flip::LockFreeOverlay;
use crate::persistent_artrie::core::overlay::{OverlayDictionaryNode, OverlayNode};
use crate::persistent_artrie::error::Result;
use crate::persistent_artrie::recovery::RecoveryReport;
use crate::{Dictionary, MappedDictionary, MutableMappedDictionary};
use std::path::Path;

// Dictionary trait implementation
impl Dictionary for PersistentVocabARTrie {
    type Node = VocabTrieNodeRef;

    fn root(&self) -> Self::Node {
        // Overlay-backed root: navigate the lock-free overlay lazily so the returned node
        // — and EVERY descendant reached via `transition`/`edges` — can descend the FULL
        // depth of the trie. Mirrors the char trie's inherent `root()`
        // (`persistent_artrie::char::PersistentARTrieChar::root`). `None` faulter: vocab
        // never evicts, so every overlay child is `Child::InMem` (no `OnDisk` slot to
        // fault in — `OverlayFaulter::fault_overlay_slot` returns `None` for vocab).
        let overlay_root = self
            .overlay_root_node()
            .unwrap_or_else(|| Arc::new(OverlayNode::<CharKey, u64>::new()));
        VocabTrieNodeRef::from_overlay_root(overlay_root, None)
    }

    fn contains(&self, term: &str) -> bool {
        PersistentVocabARTrie::contains(self, term)
    }

    fn len(&self) -> Option<usize> {
        Some(PersistentVocabARTrie::len(self))
    }
}

impl<S: BlockStorage> PersistentVocabARTrie<S> {
    /// Capture a traversal root and exact cardinality from one atomic revision.
    pub(crate) fn root_with_term_count(&self) -> (VocabTrieNodeRef, usize) {
        let (root, term_count) = self
            .lockfree_root()
            .and_then(|slot| slot.load_with_term_count())
            .unwrap_or_else(|| (Arc::new(OverlayNode::<CharKey, u64>::new()), 0));
        (VocabTrieNodeRef::from_overlay_root(root, None), term_count)
    }
}

/// Overlay-backed node handle for the vocab trie's [`Dictionary`] traversal.
///
/// Alias of the shared, `Arc`-holding
/// [`OverlayDictionaryNode<CharKey, u64>`](crate::persistent_artrie::core::overlay::OverlayDictionaryNode)
/// from the canonical `persistent_artrie::core::overlay` layer — the SAME handle the byte
/// and char tries expose (byte: `PersistentARTrieNode<V>`; char:
/// `PersistentARTrieCharNode<V>`). The vocab overlay node IS `PersistentCharNode<u64>` =
/// `OverlayNode<CharKey, u64>`, so this handle navigates it directly: `is_final` /
/// `transition` / `edges` read the real overlay node, and each child it hands back keeps
/// its own `Arc<OverlayNode>` — so a caller can descend the FULL depth of the trie. It
/// also gains [`MappedDictionaryNode::value`](crate::MappedDictionaryNode) → `Option<u64>`
/// (the vocabulary index) for free. `Unit = char` (via `CharKey::Token`), preserved
/// exactly; the type auto-derives `Clone + Send + Sync`.
///
/// # History
///
/// This SUPERSEDED a former one-level snapshot struct
/// (`struct VocabTrieNodeRef { is_final: bool, children: Vec<(char, bool)>, path: Vec<char> }`
/// with a private `new`) whose `transition` / `edges` stamped every returned child with an
/// EMPTY `children` vector — so navigation died at depth 1 while the snapshot held no
/// handle to the trie to fetch a child's children. That truncated any generic
/// `DictionaryNode` walk (liblevenshtein transducers, libgrammstein DFS) to 1-character
/// terms — the "returns childless nodes" bug reported by libgrammstein. The shared
/// `OverlayDictionaryNode` holds the child `Arc` and re-resolves children on demand, so
/// the truncation is gone. The removed struct + its `DictionaryNode` impl are recoverable
/// via git history.
pub type VocabTrieNodeRef = OverlayDictionaryNode<CharKey, u64>;

// MappedDictionary trait implementation
impl MappedDictionary for PersistentVocabARTrie {
    type Value = u64;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        self.get_index(term)
    }
}

// MutableMappedDictionary trait implementation.
//
// The mapped value is the vocabulary index. New terms honor caller-supplied
// values via `insert_with_index`; existing terms keep their assigned index so
// the term <-> index bijection remains stable.
impl MutableMappedDictionary for PersistentVocabARTrie {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        match self.insert_with_index(term, value) {
            Ok(inserted) => inserted,
            Err(error) => {
                log::warn!(
                    "PersistentVocabARTrie::insert_with_value({term:?}, {value}) failed: {error}"
                );
                false
            }
        }
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let other_terms: Vec<(String, u64)> = other
            .iter_terms()
            .filter_map(|term| other.get_index(&term).map(|index| (term, index)))
            .collect();

        let mut inserted = 0;
        for (term, other_index) in other_terms {
            if let Some(existing_index) = self.get_index(&term) {
                let merged_index = merge_fn(&existing_index, &other_index);
                if merged_index != existing_index {
                    log::warn!(
                        "PersistentVocabARTrie::union_with cannot remap existing term \
                         {term:?} from index {existing_index} to {merged_index}; \
                         vocabulary indices are immutable"
                    );
                }
                continue;
            }

            match self.insert_with_index(&term, other_index) {
                Ok(true) => inserted += 1,
                Ok(false) => {}
                Err(error) => {
                    log::warn!(
                        "PersistentVocabARTrie::union_with failed for {term:?} at \
                         index {other_index}: {error}"
                    );
                }
            }
        }
        inserted
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: Fn(&mut Self::Value),
    {
        if let Some(existing_index) = self.get_index(term) {
            let mut proposed_index = existing_index;
            update_fn(&mut proposed_index);
            if proposed_index != existing_index {
                log::warn!(
                    "PersistentVocabARTrie::update_or_insert({term:?}) cannot remap \
                     existing index {existing_index} to {proposed_index}; vocabulary \
                     indices are immutable"
                );
            }
            return false;
        }

        match self.insert_with_index(term, default_value) {
            Ok(inserted) => inserted,
            Err(error) => {
                log::warn!(
                    "PersistentVocabARTrie::update_or_insert({term:?}, {default_value}, _) \
                     failed: {error}"
                );
                false
            }
        }
    }
}

// BijectiveDictionary trait implementation.
//
// Reverse lookup clones the term from `reverse_term_map`
// (`PersistentVocabARTrie::get_term(index)`), then wraps the resulting
// `String` in `Cow::Owned`. The previous `Option<&str>` trait signature
// couldn't be honored honestly because there is no stable borrowed `str`
// lifetime exposed by the concurrent map. Cow lets the caller see the actual
// term.
impl BijectiveDictionary for PersistentVocabARTrie {
    fn get_term(&self, value: &Self::Value) -> Option<std::borrow::Cow<'_, str>> {
        // Delegate to the inherent method, which returns Option<String>.
        Self::get_term(self, *value).map(std::borrow::Cow::Owned)
    }

    fn contains_value(&self, value: &Self::Value) -> bool {
        self.contains_index(*value)
    }

    fn bijection_len(&self) -> usize {
        self.len()
    }
}

// F4 lock-collapse: `SharedVocabARTrie` is now a bare `Arc<PersistentVocabARTrie>`. The inner
// trie is fully `&self` + lock-free (lock-free overlay root, DashMap forward/reverse caches,
// atomic counters, epoch-pinned reads), so the outer `RwLock` was removed. The trait impls below
// run against the lock-free handle; `.read()`/`.write()` are the no-lock `SharedTrieAccess` shim
// (both hand back `&T`, no lock). The bare `PersistentVocabARTrie` still does not implement
// `ARTrie` directly — the impl lives on the `Arc` handle, whose `Clone` satisfies the
// `ARTrie: Clone` bound. Only concurrent `checkpoint()` needs mutual exclusion, via the internal
// `checkpoint_lock` (never the handle).

// ============================================================================
// SharedVocabARTrie Trait Implementations
// ============================================================================

use crate::persistent_artrie::core::shared_access::SharedTrieAccess;
use std::sync::Arc;

impl Dictionary for SharedVocabARTrie {
    type Node = VocabTrieNodeRef;

    fn root(&self) -> Self::Node {
        // Overlay-backed root (see `PersistentVocabARTrie`'s `Dictionary::root`). The
        // returned node OWNS the overlay-root `Arc`, so it outlives the transient no-lock
        // `read()` shim guard (`SharedTrieAccess` hands back `&PersistentVocabARTrie`).
        let guard = self.read();
        let overlay_root = guard
            .overlay_root_node()
            .unwrap_or_else(|| Arc::new(OverlayNode::<CharKey, u64>::new()));
        VocabTrieNodeRef::from_overlay_root(overlay_root, None)
    }

    fn contains(&self, term: &str) -> bool {
        let guard = self.read();
        guard.contains(term)
    }

    fn len(&self) -> Option<usize> {
        let guard = self.read();
        Some(guard.len())
    }
}

impl MappedDictionary for SharedVocabARTrie {
    type Value = u64;

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        let guard = self.read();
        guard.get_index(term)
    }
}

// `SharedVocabARTrie` accepts mutations through its read/write guards. New
// terms honor the value-shaped API by treating values as explicit vocabulary
// indices; existing terms keep their assigned index.
impl MutableMappedDictionary for SharedVocabARTrie {
    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        let guard = self.write();
        match guard.insert_with_index(term, value) {
            Ok(inserted) => inserted,
            Err(error) => {
                log::warn!(
                    "SharedVocabARTrie::insert_with_value({term:?}, {value}) failed: {error}"
                );
                false
            }
        }
    }

    fn union_with<F>(&self, other: &Self, merge_fn: F) -> usize
    where
        F: Fn(&Self::Value, &Self::Value) -> Self::Value,
        Self::Value: Clone,
    {
        let other_terms: Vec<(String, u64)> = {
            let other_guard = other.read();
            other_guard
                .iter_terms()
                .filter_map(|term| other_guard.get_index(&term).map(|index| (term, index)))
                .collect()
        };

        let mut conflicts = Vec::new();
        let inserted = {
            let self_guard = self.write();
            let mut inserted = 0;
            for (term, other_index) in other_terms {
                if let Some(existing_index) = self_guard.get_index(&term) {
                    conflicts.push((term, existing_index, other_index));
                    continue;
                }

                match self_guard.insert_with_index(&term, other_index) {
                    Ok(true) => inserted += 1,
                    Ok(false) => {}
                    Err(error) => {
                        log::warn!(
                            "SharedVocabARTrie::union_with failed for {term:?} at \
                             index {other_index}: {error}"
                        );
                    }
                }
            }
            inserted
        };

        for (term, existing_index, other_index) in conflicts {
            let merged_index = merge_fn(&existing_index, &other_index);
            if merged_index != existing_index {
                log::warn!(
                    "SharedVocabARTrie::union_with cannot remap existing term \
                     {term:?} from index {existing_index} to {merged_index}; \
                     vocabulary indices are immutable"
                );
            }
        }
        inserted
    }

    fn update_or_insert<F>(&self, term: &str, default_value: Self::Value, update_fn: F) -> bool
    where
        F: Fn(&mut Self::Value),
    {
        let guard = self.write();
        if let Some(existing_index) = guard.get_index(term) {
            // F4: `guard` is a no-lock `TrieAccessGuard` — there is no write lock to release
            // before invoking `update_fn`, so the former `drop(guard)` (now a no-op) is gone.
            let mut proposed_index = existing_index;
            update_fn(&mut proposed_index);
            if proposed_index != existing_index {
                log::warn!(
                    "SharedVocabARTrie::update_or_insert({term:?}) cannot remap \
                     existing index {existing_index} to {proposed_index}; vocabulary \
                     indices are immutable"
                );
            }
            return false;
        }

        match guard.insert_with_index(term, default_value) {
            Ok(inserted) => inserted,
            Err(error) => {
                log::warn!(
                    "SharedVocabARTrie::update_or_insert({term:?}, {default_value}, _) \
                     failed: {error}"
                );
                false
            }
        }
    }
}

impl BijectiveDictionary for SharedVocabARTrie {
    fn get_term(&self, value: &Self::Value) -> Option<std::borrow::Cow<'_, str>> {
        // Acquire the read guard, reconstruct the term, drop the guard, return
        // the owned String wrapped as Cow::Owned. The Cow doesn't borrow from
        // self because the underlying String is owned outright.
        let guard = self.read();
        guard.get_term(*value).map(std::borrow::Cow::Owned)
    }

    fn contains_value(&self, value: &Self::Value) -> bool {
        let guard = self.read();
        guard.contains_index(*value)
    }

    fn bijection_len(&self) -> usize {
        let guard = self.read();
        guard.len()
    }
}

impl crate::artrie_trait::ARTrie for SharedVocabARTrie {
    type Unit = char;
    type Value = u64;

    fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentVocabARTrie::create(path).map(Arc::new)
    }

    fn create_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentVocabARTrie::create(path).map(Arc::new)
    }

    fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentVocabARTrie::open(path).map(Arc::new)
    }

    fn open_with_slot_tracking<P: AsRef<Path>>(path: P) -> Result<Self> {
        PersistentVocabARTrie::open(path).map(Arc::new)
    }

    fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport)> {
        PersistentVocabARTrie::open_with_recovery(path).map(|(t, r)| (Arc::new(t), r))
    }

    fn open_with_recovery_and_slot_tracking<P: AsRef<Path>>(
        path: P,
    ) -> Result<(Self, RecoveryReport)> {
        let (trie, report) = PersistentVocabARTrie::open_with_recovery(path)?;
        trie.enable_slot_tracking();
        Ok((Arc::new(trie), report))
    }

    fn enable_slot_tracking(&self) {
        self.write().enable_slot_tracking();
    }

    fn flush_sequential(&self) -> Result<()> {
        self.write().flush_sequential()
    }

    fn insert(&self, term: &str) -> bool
    where
        Self::Value: Default,
    {
        let guard = self.write();
        let old_count = guard.len();
        // Explicitly call the inherent `&self` struct method (not any trait method).
        if let Err(error) = PersistentVocabARTrie::insert(&*guard, term) {
            log::warn!("SharedVocabARTrie::insert failed: {error}");
            return false;
        }
        // Return true if a new term was added (count increased)
        guard.len() > old_count
    }

    fn insert_with_value(&self, term: &str, value: Self::Value) -> bool {
        let guard = self.write();
        match guard.insert_with_index(term, value) {
            Ok(inserted) => inserted,
            Err(error) => {
                log::warn!(
                    "SharedVocabARTrie::insert_with_value({term:?}, {value}) failed: {error}"
                );
                false
            }
        }
    }

    fn contains(&self, term: &str) -> bool {
        let guard = self.read();
        guard.contains(term)
    }

    fn get_value(&self, term: &str) -> Option<Self::Value> {
        let guard = self.read();
        guard.get_index(term)
    }

    fn remove(&self, term: &str) -> bool {
        log::warn!(
            "SharedVocabARTrie::remove({term:?}) is unsupported — vocab tries \
             are append-only to preserve the term ↔ index bijection. Returns \
             false unconditionally."
        );
        false
    }

    fn len(&self) -> usize {
        let guard = self.read();
        guard.len()
    }

    fn checkpoint(&self) -> Result<()> {
        // F4: serialize concurrent checkpoints via the internal `checkpoint_lock` (the outer
        // write lock that used to do this was removed with the `Arc<RwLock>` → `Arc` collapse).
        // Clone the `Arc<Mutex>` out of a brief read guard so the trie handle isn't held while
        // acquiring CK. Byte/char twin (`ConcurrentCheckpointSerialization.tla`).
        let ckpt_lock = self.read().checkpoint_lock.clone();
        let _ckpt_guard = ckpt_lock.lock();
        let guard = self.write();
        guard.checkpoint()
    }

    fn is_dirty(&self) -> bool {
        let guard = self.read();
        guard.is_dirty()
    }

    fn remove_prefix(&self, prefix: &str) -> usize {
        log::warn!(
            "SharedVocabARTrie::remove_prefix({prefix:?}) is unsupported — \
             vocab tries are append-only. Returns 0 unconditionally."
        );
        0
    }

    fn iter_prefix(&self, prefix: &str) -> Option<Box<dyn Iterator<Item = String> + '_>> {
        // For SharedVocabARTrie, we need to collect terms to avoid holding lock
        // during iteration. This collects all matching terms upfront.
        let guard = self.read();
        let prefix_chars: Vec<char> = prefix.chars().collect();

        // Check if prefix exists
        let prefix_exists = if prefix.is_empty() {
            true
        } else {
            guard.get_index(prefix).is_some()
                || !guard.get_children_at_path(&prefix_chars).is_empty()
        };

        if prefix_exists {
            // Collect terms while holding lock, then return iterator over collected Vec
            let terms: Vec<String> = guard.iter_terms_with_prefix(prefix).collect();
            Some(Box::new(terms.into_iter()))
        } else {
            None
        }
    }

    fn sync(&self) -> Result<()> {
        let guard = self.write();
        guard.sync()
    }

    fn current_lsn(&self) -> u64 {
        let guard = self.read();
        guard.current_lsn()
    }

    fn synced_lsn(&self) -> Option<u64> {
        let guard = self.read();
        guard.synced_lsn()
    }

    fn durability_policy(&self) -> DurabilityPolicy {
        let guard = self.read();
        guard.durability_policy()
    }

    fn upsert(&self, term: &str, value: Self::Value) -> Result<bool> {
        let guard = self.write();
        guard.insert_with_index(term, value)
    }

    // C1: `increment` removed from the `ARTrie` trait. Vocab never supported it
    // (indices are auto-assigned); the former runtime reject is now simply the
    // method's ABSENCE (more honest than a runtime Err). Commented out (not deleted)
    // per convention.
    // fn increment(&self, _term: &str, _delta: i64) -> Result<i64> {
    //     Err(crate::persistent_artrie::error::PersistentARTrieError::InvalidOperation(
    //         "PersistentVocabARTrie does not support increment - indices are auto-assigned".into(),
    //     ))
    // }
}

// ============================================================================
// EvictableARTrie Trait Implementation (on SharedVocabARTrie)
// ============================================================================

impl crate::artrie_trait::EvictableARTrie for SharedVocabARTrie {
    fn enable_eviction(
        &self,
        config: crate::persistent_artrie::eviction::EvictionConfig,
    ) -> crate::persistent_artrie::error::Result<()> {
        use crate::persistent_artrie::error::PersistentARTrieError;

        config
            .validate()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        // F4 (EC leaf): check under a BRIEF EC lock; the coordinator is fully built + started
        // OUTSIDE the lock so EC is never held across a thread spawn or any other lock.
        if self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .is_some()
        {
            return Err(PersistentARTrieError::internal("Eviction already enabled"));
        }

        // Share THIS trie's own epoch manager with the coordinator (byte/char parity). The vocab
        // callback evicts nothing, so this is honest reader accounting, not a correctness change.
        let epoch_manager = Arc::clone(&self.epoch_manager);

        // Create the eviction coordinator
        let coordinator = crate::persistent_artrie::eviction::EvictionCoordinator::new(
            config.clone(),
            epoch_manager,
        );

        // Start the eviction coordinator with a no-op char callback. Overlay-only (V6): the
        // overlay never evicts finals to disk (OverlayFaulter::fault_overlay_slot -> None), so
        // there is nothing to unswizzle; the coordinator lifecycle is retained for
        // memory-pressure accounting + API parity with byte/char.
        coordinator
            .start_char(move |_nodes_to_evict| (0, 0))
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        // Start memory pressure monitor if configured
        coordinator
            .start_memory_monitor()
            .map_err(|e| PersistentARTrieError::internal(&e))?;

        // Install under a brief EC lock (re-check: first writer wins; a loser shuts its own
        // coordinator down OUTSIDE EC).
        let mut slot = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned");
        if slot.is_some() {
            drop(slot);
            coordinator.shutdown();
            return Err(PersistentARTrieError::internal("Eviction already enabled"));
        }
        *slot = Some(coordinator);
        Ok(())
    }

    fn disable_eviction(&self) -> crate::persistent_artrie::error::Result<()> {
        // Drop-before-join (live-deadlock fix; red-team R3-2 SWEEP C, the 8th site):
        // take the coordinator out and RELEASE the write guard BEFORE `shutdown()`
        // joins the eviction worker. The worker's reclaim callback re-enters via
        // `trie.write()` (the `enable_eviction` closure), so holding the write guard
        // across the join deadlocks (worker waits on the guard; the joining thread
        // waits on the worker). char/byte `disable_eviction` already use this
        // statement-temporary; vocab was the missed site.
        let coordinator = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .take();
        // EC guard dropped here (statement-temporary) — BEFORE the join in `shutdown()`.
        if let Some(coordinator) = coordinator {
            coordinator.shutdown();
        }
        Ok(())
    }

    fn eviction_enabled(&self) -> bool {
        self.eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .is_some()
    }

    fn eviction_stats(&self) -> crate::persistent_artrie::eviction::EvictionStats {
        self.eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
            .map(|c| c.stats())
            .unwrap_or_default()
    }

    fn force_eviction(
        &self,
        target_bytes: usize,
    ) -> crate::persistent_artrie::error::Result<(usize, usize)> {
        // Clone the coordinator Arc out under a BRIEF EC lock, then release EC before calling
        // force_eviction (vocab's no-op callback evicts nothing, so no overlay reclaim needed).
        let coordinator = {
            match self
                .eviction_coordinator
                .lock()
                .expect("eviction_coordinator mutex poisoned")
                .as_ref()
            {
                Some(c) => Arc::clone(c),
                None => return Ok((0, 0)),
            }
        };
        Ok(coordinator.force_eviction(target_bytes))
    }

    fn touch_node(&self, path: &[Self::Unit]) {
        if let Some(coordinator) = self
            .eviction_coordinator
            .lock()
            .expect("eviction_coordinator mutex poisoned")
            .as_ref()
        {
            use crate::persistent_artrie::eviction::lru_tracker::hash_char_path;
            coordinator.lru_registry().touch_hash(hash_char_path(path));
        }
    }
}

// ============================================================================
// Type Aliases
// ============================================================================

/// Type alias for vocabulary use cases.
///
/// This is the recommended type for embedding vocabularies, token-to-ID mappings,
/// and similar use cases that need sequential `u64` indices with persistent storage.
///
/// # Example
///
/// ```rust,no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use libdictenstein::persistent_artrie::vocab::IndexedVocabularyPersistent;
///
/// // Create new vocabulary
/// let mut vocab = IndexedVocabularyPersistent::create("vocab.vocab")?;
/// vocab.insert("hello")?; // Returns 0
///
/// // Checkpoint and reopen with recovery
/// vocab.checkpoint()?;
/// let (vocab, report) = IndexedVocabularyPersistent::open_with_recovery("vocab.vocab")?;
///
/// // Reverse lookup works immediately!
/// assert_eq!(vocab.get_term(0), Some("hello".to_string()));
/// # Ok(())
/// # }
/// ```
pub type IndexedVocabularyPersistent = PersistentVocabARTrie;

// Backwards compatibility alias (deprecated)
#[deprecated(since = "0.9.0", note = "Use SharedVocabARTrie instead")]
pub type SharedVocabTrie = SharedVocabARTrie;

// Also re-export DiskBackedVocabTrieInner as deprecated alias
#[deprecated(since = "0.9.0", note = "Use PersistentVocabARTrie directly instead")]
pub type DiskBackedVocabTrieInner = PersistentVocabARTrie;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_vocab_trie_basic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).unwrap();

        // Insert
        let idx1 = vocab.insert("apple").expect("insert apple");
        let idx2 = vocab.insert("banana").expect("insert banana");
        let idx3 = vocab.insert("cherry").expect("insert cherry");

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 2);
        assert_eq!(vocab.len(), 3);

        // Forward lookup
        assert_eq!(vocab.get_index("apple"), Some(0));
        assert_eq!(vocab.get_index("banana"), Some(1));
        assert_eq!(vocab.get_index("cherry"), Some(2));
        assert_eq!(vocab.get_index("durian"), None);

        // Reverse lookup
        assert_eq!(vocab.get_term(0), Some("apple".to_string()));
        assert_eq!(vocab.get_term(1), Some("banana".to_string()));
        assert_eq!(vocab.get_term(2), Some("cherry".to_string()));
        assert_eq!(vocab.get_term(999), None);
    }

    #[test]
    fn test_vocab_trie_unicode() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).unwrap();

        vocab.insert("日本語").expect("insert term failed");
        vocab.insert("中文").expect("insert term failed");
        vocab.insert("한글").expect("insert term failed");
        vocab.insert("العربية").expect("insert term failed");
        vocab.insert("emoji😀").expect("insert term failed");

        assert_eq!(vocab.get_index("日本語"), Some(0));
        assert_eq!(vocab.get_index("中文"), Some(1));
        assert_eq!(vocab.get_index("한글"), Some(2));
        assert_eq!(vocab.get_index("العربية"), Some(3));
        assert_eq!(vocab.get_index("emoji😀"), Some(4));

        assert_eq!(vocab.get_term(0), Some("日本語".to_string()));
        assert_eq!(vocab.get_term(4), Some("emoji😀".to_string()));
    }

    #[test]
    fn test_vocab_trie_custom_start() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Reserve 0-9 for special tokens
        let vocab = PersistentVocabARTrie::create_with_start_index(&path, 10).unwrap();

        let idx1 = vocab.insert("first").expect("insert first");
        let idx2 = vocab.insert("second").expect("insert second");

        assert_eq!(idx1, 10);
        assert_eq!(idx2, 11);
        assert_eq!(vocab.start_index(), 10);
    }

    #[test]
    fn test_vocab_trie_idempotent_insert() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).unwrap();

        let idx1 = vocab.insert("duplicate").expect("insert duplicate");
        let idx2 = vocab.insert("duplicate").expect("insert duplicate again");
        let idx3 = vocab
            .insert("duplicate")
            .expect("insert duplicate third time");

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 0);
        assert_eq!(idx3, 0);
        assert_eq!(vocab.len(), 1);
    }

    #[test]
    fn test_vocab_trie_traits() {
        use crate::Dictionary;
        use crate::MappedDictionary;

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).unwrap();
        vocab.insert("test").expect("insert term failed");

        // Dictionary trait
        assert!(Dictionary::contains(&vocab, "test"));
        assert!(!Dictionary::contains(&vocab, "missing"));
        assert_eq!(Dictionary::len(&vocab), Some(1));

        // MappedDictionary trait
        assert_eq!(MappedDictionary::get_value(&vocab, "test"), Some(0));
        assert_eq!(MappedDictionary::get_value(&vocab, "missing"), None);
    }

    #[test]
    fn test_vocab_trie_artrie_trait() {
        use crate::artrie_trait::ARTrie;

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab: SharedVocabARTrie = ARTrie::create(&path).unwrap();

        // ARTrie trait methods
        assert!(ARTrie::insert(&vocab, "hello"));
        assert!(!ARTrie::insert(&vocab, "hello")); // Already exists
        assert!(ARTrie::contains(&vocab, "hello"));
        assert_eq!(ARTrie::get_value(&vocab, "hello"), Some(0));
        assert_eq!(ARTrie::len(&vocab), 1);
    }

    #[test]
    fn test_vocab_trie_lsn_tracking() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).unwrap();

        // Initial state
        let initial_lsn = vocab.current_lsn();
        assert!(initial_lsn > 0);
        assert!(vocab.synced_lsn().is_none());

        // After insert
        vocab.insert("test").expect("insert term failed");
        assert!(vocab.current_lsn() > initial_lsn);

        // After sync
        vocab.sync().unwrap();
        assert!(vocab.synced_lsn().is_some());
    }

    #[test]
    fn test_vocab_trie_durability_policy() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).unwrap();

        // Default is Immediate
        assert_eq!(vocab.durability_policy(), DurabilityPolicy::Immediate);

        // Change to Periodic
        vocab.set_durability_policy(DurabilityPolicy::Periodic);
        assert_eq!(vocab.durability_policy(), DurabilityPolicy::Periodic);
    }

    #[test]
    fn test_shared_vocab_artrie() {
        use crate::artrie_trait::ARTrie;

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let vocab: SharedVocabARTrie = ARTrie::create(&path).unwrap();

        // Insert via trait
        assert!(ARTrie::insert(&vocab, "hello"));
        assert!(ARTrie::insert(&vocab, "world"));

        // Verify
        assert!(ARTrie::contains(&vocab, "hello"));
        assert!(ARTrie::contains(&vocab, "world"));
        assert_eq!(ARTrie::len(&vocab), 2);

        // Checkpoint
        ARTrie::checkpoint(&vocab).unwrap();
    }
}
