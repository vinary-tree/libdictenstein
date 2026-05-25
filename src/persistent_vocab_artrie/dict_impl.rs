//! Disk-backed implementation of PersistentVocabARTrie.
//!
//! This module provides the core disk-backed vocabulary trie implementation
//! with parent pointers for O(k) reverse lookups, using the base persistence
//! infrastructure from `persistent_artrie` (WAL, BufferManager, etc.).
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                 PersistentVocabARTrie                        │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Uses base persistence layer from persistent_artrie:        │
//! │  - WalWriter/WalReader for WAL operations                   │
//! │  - BufferManager for page cache                             │
//! │  - DiskManager for raw block I/O                            │
//! │  - ArenaManager for node storage                            │
//! │                                                              │
//! │  Files:                                                      │
//! │  - vocabulary.vocab      # Main trie (nodes with parents)   │
//! │  - vocabulary.vocab.wal  # Write-ahead log                  │
//! │  - vocabulary.vocab.idx  # Reverse index (u64 → NodeRef)    │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # File Layout
//!
//! ```text
//! vocabulary.vocab:
//! ┌─────────────────────────────────────────────────────────────┐
//! │ VocabTrieFileHeader (96 bytes)                              │
//! │ - Magic: "VOCB"                                             │
//! │ - Version: u8                                               │
//! │ - Root pointer: u64                                         │
//! │ - Entry count: u64                                          │
//! │ - Start/Next index: u64                                     │
//! └─────────────────────────────────────────────────────────────┘
//! │ VocabTrieNode entries (arenas)                              │
//! └─────────────────────────────────────────────────────────────┘
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
#[allow(unused_imports)]
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Arc;
#[allow(unused_imports)]
use std::time::Duration;
use xxhash_rust::xxh3::Xxh3DefaultBuilder;

use parking_lot::RwLock;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::dict_impl::DurabilityPolicy;
use crate::persistent_artrie::disk_manager::MmapDiskManager;
use crate::persistent_artrie::wal::AsyncWalWriter;
#[allow(unused_imports)]
use crate::persistent_artrie::wal::{WalConfig, WalReader};
use crate::persistent_artrie::wal_managed::WalManaged;
use crate::persistent_artrie_char::arena_manager::ArenaManager;
use crate::persistent_artrie_char::nodes::AtomicNodePtr;
use crate::persistent_artrie_char::types::NodeRef;
use dashmap::DashMap;

use super::reverse_cache::VocabReverseCache;
use super::reverse_index::VocabReverseIndex;
use super::types::{VocabTrieNode, VocabTrieRoot, DEFAULT_REVERSE_CACHE_SIZE};
use crate::bloom_filter::BloomFilter;

/// Default buffer pool size for vocabulary trie
const DEFAULT_VOCAB_BUFFER_POOL_SIZE: usize = 64;

// `VocabSyncHandle` was relocated to `super::sync_handle`; re-exported here
// under its original path.
pub use super::sync_handle::VocabSyncHandle;

/// Persistent vocabulary ARTrie with parent pointers for O(k) reverse lookups.
///
/// This struct uses the base persistence layer from `persistent_artrie` for
/// WAL-based crash recovery and durability, with full disk-backed node storage
/// via ArenaManager.
///
/// # Thread Safety
///
/// Thread safety is provided via external wrapping with `Arc<RwLock<...>>`.
/// Use the type alias [`SharedVocabARTrie`] for thread-safe access.
///
/// # Example
///
/// ```rust,no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
///
/// // Create a new vocabulary
/// let mut vocab = PersistentVocabARTrie::create("vocab.vocab")?;
///
/// // Insert terms
/// let idx1 = vocab.insert("hello")?; // Returns 0
/// let idx2 = vocab.insert("world")?; // Returns 1
///
/// // Forward lookup
/// assert_eq!(vocab.get_index("hello"), Some(0));
///
/// // Reverse lookup (O(k) via parent backtracking)
/// assert_eq!(vocab.get_term(0), Some("hello".to_string()));
///
/// // Checkpoint to disk
/// vocab.checkpoint()?;
///
/// // Close and reopen - data is preserved!
/// drop(vocab);
/// let (vocab, _) = PersistentVocabARTrie::open_with_recovery("vocab.vocab")?;
/// assert_eq!(vocab.get_index("hello"), Some(0));
/// # Ok(())
/// # }
/// ```
pub struct PersistentVocabARTrie<S: BlockStorage = MmapDiskManager> {
    // === Vocab-specific fields ===
    /// Path to the main trie file
    pub(super) path: PathBuf,

    /// Root node of the trie
    pub(crate) root: VocabTrieRoot,

    /// Number of vocabulary entries (atomic for lock-free access)
    pub(super) entry_count: AtomicUsize,

    /// Starting vocabulary index
    pub(super) start_index: u64,

    /// Next index to assign (atomic for lock-free CAS operations)
    pub(super) next_index: AtomicU64,

    /// Dirty flag (atomic for lock-free access)
    pub(super) dirty: AtomicBool,

    /// Reverse index for O(1) node lookup by vocabulary index
    pub(super) reverse_index: Option<VocabReverseIndex>,

    /// LRU cache for hot reverse lookups
    pub(super) reverse_cache: VocabReverseCache,

    /// Map from NodeRef to in-memory node for lookups.
    /// This is used for term reconstruction via parent pointers.
    /// Uses xxh3 hasher instead of SipHash for ~3-5x faster hashing on
    /// non-adversarial input (vocabulary node references).
    pub(super) node_map: HashMap<NodeRef, *const VocabTrieNode, Xxh3DefaultBuilder>,

    /// Next available slot for NodeRef assignment
    pub(super) next_slot: u64,

    // === Base persistence layer (from persistent_artrie) ===
    /// WAL writer for durability (using AsyncWalWriter via WalManaged trait)
    pub(super) wal_writer: Option<Arc<AsyncWalWriter>>,

    /// WAL configuration
    pub(super) wal_config: WalConfig,

    /// Next LSN to assign (atomic for lock-free access)
    pub(super) next_lsn: AtomicU64,

    /// Last synced LSN (atomic for lock-free access)
    pub(super) synced_lsn: AtomicU64,

    /// Durability policy for WAL synchronization
    pub(super) durability_policy: DurabilityPolicy,

    // === Storage layer for disk-backed persistence ===
    /// Arena manager for node storage (shared with buffer manager)
    pub(super) arena_manager: Option<Arc<RwLock<ArenaManager<S>>>>,

    /// Buffer manager for disk I/O
    pub(super) buffer_manager: Option<Arc<RwLock<BufferManager<S>>>>,

    // === Eviction Support ===
    /// Eviction coordinator for memory pressure-driven eviction
    pub(crate) eviction_coordinator:
        Option<Arc<crate::persistent_artrie::eviction::EvictionCoordinator>>,

    // === BloomFilter Support ===
    /// Optional BloomFilter for O(1) negative lookups.
    /// Provides 5-10x faster rejection for OOV words.
    pub(super) bloom_filter: Option<BloomFilter>,

    // === Lock-Free Infrastructure (per plan Phase 4-5) ===
    /// Lock-free root using PersistentCharNode with im::Vector for CAS operations.
    /// When present, `insert_cas()` uses this for lock-free concurrent inserts.
    pub(super) lockfree_root: Option<AtomicNodePtr>,

    /// Lock-free cache for term → index lookups (DashMap for O(1) sharded access).
    pub(super) lockfree_cache: Option<DashMap<String, u64>>,

    /// Statistics: CAS retries for monitoring contention.
    pub(super) cas_retries: AtomicU64,
}

// ============================================================================
// WalManaged trait implementation
// ============================================================================

impl<S: BlockStorage> WalManaged for PersistentVocabARTrie<S> {
    fn wal_writer(&self) -> Option<&Arc<AsyncWalWriter>> {
        self.wal_writer.as_ref()
    }
}

// Safety: The raw pointers in node_map are managed carefully and only accessed
// through methods that ensure proper synchronization.
unsafe impl<S: BlockStorage> Send for PersistentVocabARTrie<S> {}
unsafe impl<S: BlockStorage> Sync for PersistentVocabARTrie<S> {}

/// Thread-safe shared vocabulary ARTrie.
///
/// This is the recommended type for concurrent access to the vocabulary trie.
pub type SharedVocabARTrie<S = MmapDiskManager> = Arc<RwLock<PersistentVocabARTrie<S>>>;

// `load_trie_from_disk` and related disk-loading + persistence helpers
// moved to sibling `disk_io.rs` in Phase-6 decomposition.

impl<S: BlockStorage> Drop for PersistentVocabARTrie<S> {
    fn drop(&mut self) {
        // Try to checkpoint on drop
        let _ = self.checkpoint();
    }
}

impl<S: BlockStorage> Clone for PersistentVocabARTrie<S> {
    fn clone(&self) -> Self {
        // Deep clone the root
        let cloned_root = self.root.clone();

        // Clone node_map with new pointers
        let mut new_node_map = HashMap::with_hasher(Xxh3DefaultBuilder);
        if let VocabTrieRoot::Node(ref root_box) = cloned_root {
            let root_ref = NodeRef::new(0, 0);
            new_node_map.insert(root_ref, root_box.as_ref() as *const VocabTrieNode);
        }

        Self {
            path: self.path.clone(),
            root: cloned_root,
            entry_count: AtomicUsize::new(self.entry_count.load(Ordering::Acquire)),
            start_index: self.start_index,
            next_index: AtomicU64::new(self.next_index.load(Ordering::Acquire)),
            dirty: AtomicBool::new(self.dirty.load(Ordering::Acquire)),
            reverse_index: None, // Cannot clone mmap'd index
            reverse_cache: VocabReverseCache::new(DEFAULT_REVERSE_CACHE_SIZE),
            node_map: new_node_map,
            next_slot: self.next_slot,
            wal_writer: self.wal_writer.clone(),
            wal_config: self.wal_config.clone(),
            next_lsn: AtomicU64::new(self.next_lsn.load(Ordering::Acquire)),
            synced_lsn: AtomicU64::new(self.synced_lsn.load(Ordering::Acquire)),
            durability_policy: self.durability_policy,
            arena_manager: None,        // Cannot clone arena manager
            buffer_manager: None,       // Cannot clone buffer manager
            eviction_coordinator: None, // Cannot clone eviction coordinator
            bloom_filter: self.bloom_filter.clone(),
            lockfree_root: None,  // Cannot clone lock-free root
            lockfree_cache: None, // Cannot clone lock-free cache
            cas_retries: AtomicU64::new(0),
        }
    }
}

impl<S: BlockStorage> std::fmt::Debug for PersistentVocabARTrie<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentVocabARTrie")
            .field("path", &self.path)
            .field("len", &self.entry_count)
            .field("start_index", &self.start_index)
            .field("next_index", &self.next_index)
            .field("is_dirty", &self.dirty)
            .field("next_lsn", &self.next_lsn)
            .field("synced_lsn", &self.synced_lsn)
            .field("durability_policy", &self.durability_policy)
            .field("has_arena_manager", &self.arena_manager.is_some())
            .field("has_buffer_manager", &self.buffer_manager.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_create_and_insert() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

        // Insert terms
        let idx1 = vocab.insert("hello").expect("insert hello");
        let idx2 = vocab.insert("world").expect("insert world");
        let idx3 = vocab.insert("hello").expect("insert duplicate hello"); // Duplicate

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 0); // Returns existing index

        assert_eq!(vocab.len(), 2);
    }

    #[test]
    fn test_forward_lookup() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
        vocab.insert("apple");
        vocab.insert("banana");
        vocab.insert("cherry");

        assert_eq!(vocab.get_index("apple"), Some(0));
        assert_eq!(vocab.get_index("banana"), Some(1));
        assert_eq!(vocab.get_index("cherry"), Some(2));
        assert_eq!(vocab.get_index("durian"), None);
    }

    #[test]
    fn test_reverse_lookup() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
        vocab.insert("apple");
        vocab.insert("banana");
        vocab.insert("cherry");

        assert_eq!(vocab.get_term(0), Some("apple".to_string()));
        assert_eq!(vocab.get_term(1), Some("banana".to_string()));
        assert_eq!(vocab.get_term(2), Some("cherry".to_string()));
        assert_eq!(vocab.get_term(999), None);
    }

    #[test]
    fn test_unicode_terms() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

        let idx1 = vocab.insert("日本語").expect("insert Japanese term");
        let idx2 = vocab.insert("中文").expect("insert Chinese term");
        let idx3 = vocab.insert("한글").expect("insert Korean term");

        assert_eq!(vocab.get_index("日本語"), Some(idx1));
        assert_eq!(vocab.get_index("中文"), Some(idx2));
        assert_eq!(vocab.get_index("한글"), Some(idx3));

        assert_eq!(vocab.get_term(idx1), Some("日本語".to_string()));
        assert_eq!(vocab.get_term(idx2), Some("中文".to_string()));
        assert_eq!(vocab.get_term(idx3), Some("한글".to_string()));
    }

    #[test]
    fn test_custom_start_index() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create_with_start_index(&path, 100).unwrap();

        let idx1 = vocab.insert("first").expect("insert first");
        let idx2 = vocab.insert("second").expect("insert second");

        assert_eq!(idx1, 100);
        assert_eq!(idx2, 101);
        assert_eq!(vocab.start_index(), 100);
    }

    #[test]
    fn test_checkpoint_and_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Create and populate
        {
            let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
            vocab.insert("hello");
            vocab.insert("world");
            vocab.insert("test");
            vocab.checkpoint().unwrap();
        }

        // Reopen with recovery and verify data is preserved
        {
            let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();
            assert!(report.mode.is_normal()); // No WAL replay needed

            // Verify data was loaded from disk
            assert_eq!(vocab.len(), 3);
            assert_eq!(vocab.get_index("hello"), Some(0));
            assert_eq!(vocab.get_index("world"), Some(1));
            assert_eq!(vocab.get_index("test"), Some(2));
        }
    }

    #[test]
    fn test_checkpoint_reopen_modify_checkpoint() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Phase 1: Create, insert, checkpoint
        {
            let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
            vocab.insert("apple");
            vocab.insert("banana");
            vocab.checkpoint().unwrap();
        }

        // Phase 2: Reopen, insert more, checkpoint
        {
            let (mut vocab, _) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();
            assert_eq!(vocab.len(), 2);
            vocab.insert("cherry");
            vocab.insert("durian");
            vocab.checkpoint().unwrap();
        }

        // Phase 3: Reopen and verify all data
        {
            let (vocab, _) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();
            assert_eq!(vocab.len(), 4);
            assert_eq!(vocab.get_index("apple"), Some(0));
            assert_eq!(vocab.get_index("banana"), Some(1));
            assert_eq!(vocab.get_index("cherry"), Some(2));
            assert_eq!(vocab.get_index("durian"), Some(3));
        }
    }

    #[test]
    fn test_contains() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
        vocab.insert("present");

        assert!(vocab.contains("present"));
        assert!(!vocab.contains("absent"));

        assert!(vocab.contains_index(0));
        assert!(!vocab.contains_index(1));
    }

    #[test]
    fn test_lsn_tracking() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

        // Initial LSN
        let initial_lsn = vocab.current_lsn();
        assert!(initial_lsn > 0);
        assert!(vocab.synced_lsn().is_none());

        // After insert
        vocab.insert("test");
        assert!(vocab.current_lsn() > initial_lsn);

        // After sync
        vocab.sync().unwrap();
        assert!(vocab.synced_lsn().is_some());
    }

    #[test]
    fn test_durability_policy() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

        // Default is Immediate
        assert_eq!(vocab.durability_policy(), DurabilityPolicy::Immediate);

        // Change to Periodic
        vocab.set_durability_policy(DurabilityPolicy::Periodic);
        assert_eq!(vocab.durability_policy(), DurabilityPolicy::Periodic);
    }

    #[test]
    fn test_wal_recovery() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Create and insert some terms, then drop without checkpoint
        {
            let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
            vocab.insert("term1");
            vocab.insert("term2");
            vocab.insert("term3");
            // No checkpoint - simulate crash
            std::mem::forget(vocab); // Prevent Drop from running
        }

        // Recover via WAL replay
        let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();

        // Terms should be recovered via WAL replay
        assert!(report.records_replayed > 0);
        assert_eq!(vocab.len(), 3);
        assert_eq!(vocab.get_index("term1"), Some(0));
        assert_eq!(vocab.get_index("term2"), Some(1));
        assert_eq!(vocab.get_index("term3"), Some(2));
    }

    #[test]
    fn test_partial_wal_recovery() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Phase 1: Create, insert, checkpoint
        {
            let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
            vocab.insert("apple");
            vocab.insert("banana");
            vocab.checkpoint().unwrap();
        }

        // Phase 2: Reopen, insert without checkpoint (simulate crash)
        {
            let (mut vocab, _) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();
            vocab.insert("cherry");
            vocab.insert("durian");
            // No checkpoint - simulate crash
            std::mem::forget(vocab);
        }

        // Phase 3: Recover - should have checkpointed data + WAL replay
        let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();

        // Should have replayed 2 records (cherry, durian)
        assert!(report.records_replayed >= 2);
        assert_eq!(vocab.len(), 4);
        assert_eq!(vocab.get_index("apple"), Some(0));
        assert_eq!(vocab.get_index("banana"), Some(1));
        assert_eq!(vocab.get_index("cherry"), Some(2));
        assert_eq!(vocab.get_index("durian"), Some(3));
    }

    // ========================================================================
    // Regression tests for bug fixes (Phase 2)
    // ========================================================================

    /// Regression test for Bug #1 and #2: Node growth during load and correct NodeRef tracking.
    ///
    /// Bug #1: add_child_growing returns Ok(Some(grown)) for growth, not Err(_)
    /// Bug #2: child_ref must be stored with each child, not computed from next_slot-1
    ///
    /// This test creates a trie with >4 children at the root to trigger Node4 → Node16 growth
    /// during disk loading, then verifies all children are correctly loaded and reverse
    /// lookups work (which requires node_map to have correct NodeRefs).
    #[test]
    fn test_regression_node_growth_during_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Create trie with >4 single-char terms to trigger node growth
        // Node4 holds 4 children, so 5+ children triggers growth to Node16
        let terms = ["a", "b", "c", "d", "e", "f", "g", "h"];

        {
            let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
            for (i, term) in terms.iter().enumerate() {
                let idx = vocab.insert(term).expect("insert term");
                assert_eq!(idx, i as u64, "Term '{}' should have index {}", term, i);
            }
            vocab.checkpoint().unwrap();
        }

        // Reopen and verify all terms are present
        {
            let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();
            assert!(
                report.mode.is_normal(),
                "Should load from disk without WAL replay"
            );
            assert_eq!(vocab.len(), terms.len());

            // Verify forward lookups
            for (i, term) in terms.iter().enumerate() {
                assert_eq!(
                    vocab.get_index(term),
                    Some(i as u64),
                    "Forward lookup failed for term '{}'",
                    term
                );
            }

            // Verify reverse lookups - this exercises node_map correctness (Bug #2)
            for (i, term) in terms.iter().enumerate() {
                assert_eq!(
                    vocab.get_term(i as u64),
                    Some(term.to_string()),
                    "Reverse lookup failed for index {} (expected '{}')",
                    i,
                    term
                );
            }
        }
    }

    /// Regression test for Bug #3: Node growth during serialization.
    ///
    /// Bug #3: build_disk_char_node_static must handle Ok(Some(grown)) from add_child_growing
    ///
    /// This test creates a trie with many children that may trigger node growth during
    /// the serialization phase in build_disk_char_node_static.
    #[test]
    fn test_regression_node_growth_during_serialization() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Create trie with many terms sharing a common prefix to create deep structure
        // with multiple children at internal nodes
        let prefixes = ["aa", "ab", "ac", "ad", "ae", "af", "ag", "ah", "ai", "aj"];

        {
            let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
            for (i, term) in prefixes.iter().enumerate() {
                let idx = vocab.insert(term).expect("insert term");
                assert_eq!(idx, i as u64);
            }
            // This checkpoint triggers serialization with potential node growth
            vocab.checkpoint().unwrap();
        }

        // Verify data survived serialization
        {
            let (vocab, _) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();
            assert_eq!(vocab.len(), prefixes.len());

            for (i, term) in prefixes.iter().enumerate() {
                assert_eq!(
                    vocab.get_index(term),
                    Some(i as u64),
                    "Term '{}' not found after serialization",
                    term
                );
            }
        }
    }

    /// Regression test for Bug #1, #2, #3 combined: Large trie with deep structure.
    ///
    /// This stress test creates a larger vocabulary that exercises:
    /// - Node growth during loading (Bug #1, #2)
    /// - Node growth during serialization (Bug #3)
    /// - Correct NodeRef tracking for reverse lookups (Bug #2)
    #[test]
    fn test_regression_large_trie_checkpoint_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Generate terms that will create a trie with varied structure
        let terms: Vec<String> = (0..50).map(|i| format!("term_{:03}", i)).collect();

        {
            let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
            for (i, term) in terms.iter().enumerate() {
                let idx = vocab.insert(term).expect("insert term");
                assert_eq!(idx, i as u64);
            }
            vocab.checkpoint().unwrap();
        }

        // Reopen and verify
        {
            let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();
            assert!(report.mode.is_normal());
            assert_eq!(vocab.len(), terms.len());

            // Verify all forward and reverse lookups
            for (i, term) in terms.iter().enumerate() {
                assert_eq!(
                    vocab.get_index(term),
                    Some(i as u64),
                    "Forward lookup failed for '{}'",
                    term
                );
                assert_eq!(
                    vocab.get_term(i as u64),
                    Some(term.clone()),
                    "Reverse lookup failed for index {}",
                    i
                );
            }
        }

        // Reopen again, add more terms, checkpoint again
        {
            let (mut vocab, _) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();

            let more_terms: Vec<String> = (50..75).map(|i| format!("term_{:03}", i)).collect();

            for (i, term) in more_terms.iter().enumerate() {
                let idx = vocab.insert(term).expect("insert term");
                assert_eq!(idx, (50 + i) as u64);
            }
            vocab.checkpoint().unwrap();
        }

        // Final verification
        {
            let (vocab, _) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();
            assert_eq!(vocab.len(), 75);

            for i in 0..75 {
                let expected_term = format!("term_{:03}", i);
                assert_eq!(vocab.get_index(&expected_term), Some(i as u64));
                assert_eq!(vocab.get_term(i as u64), Some(expected_term));
            }
        }
    }

    /// Regression test for Bug #4: Corrupted data detection.
    ///
    /// Bug #4: If has_value=1 but data is too short, should error instead of silently
    /// dropping the value.
    ///
    /// Note: This is a defensive check for data corruption. We test indirectly by
    /// verifying that valid data with values is preserved correctly. Direct testing
    /// of the error path would require crafting invalid binary data.
    #[test]
    fn test_regression_value_preservation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Create terms and verify their values (indices) are preserved
        {
            let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

            // Insert terms that will have values
            vocab.insert("with_value_1");
            vocab.insert("with_value_2");
            vocab.insert("with_value_3");

            // Verify values before checkpoint
            assert_eq!(vocab.get_index("with_value_1"), Some(0));
            assert_eq!(vocab.get_index("with_value_2"), Some(1));
            assert_eq!(vocab.get_index("with_value_3"), Some(2));

            vocab.checkpoint().unwrap();
        }

        // Reopen and verify values survived
        {
            let (vocab, _) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();

            // If Bug #4 existed, values might be silently dropped
            assert_eq!(
                vocab.get_index("with_value_1"),
                Some(0),
                "Value for 'with_value_1' was lost"
            );
            assert_eq!(
                vocab.get_index("with_value_2"),
                Some(1),
                "Value for 'with_value_2' was lost"
            );
            assert_eq!(
                vocab.get_index("with_value_3"),
                Some(2),
                "Value for 'with_value_3' was lost"
            );

            // Also verify via reverse lookup
            assert_eq!(vocab.get_term(0), Some("with_value_1".to_string()));
            assert_eq!(vocab.get_term(1), Some("with_value_2".to_string()));
            assert_eq!(vocab.get_term(2), Some("with_value_3".to_string()));
        }
    }

    // ========================================================================
    // sync_to_disk tests
    // ========================================================================

    #[test]
    fn test_sync_to_disk_async_non_blocking() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let vocab = Arc::new(RwLock::new(
            PersistentVocabARTrie::create(&path).expect("Failed to create vocab"),
        ));

        // Insert some data
        vocab.write().insert("hello");

        // Start async sync
        let handle = vocab
            .read()
            .sync_to_disk_async()
            .expect("Failed to start async sync");

        // Reads continue during sync
        assert!(vocab.read().contains("hello"));

        // Writes continue during sync
        vocab.write().insert("world");

        // Wait for sync completion
        handle.wait().expect("Sync failed");

        // Verify both words present
        assert!(vocab.read().contains("hello"));
        assert!(vocab.read().contains("world"));
    }

    #[test]
    fn test_sync_to_disk_async_multiple_calls() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");
        vocab.insert("hello");

        // Start first sync
        let handle1 = vocab
            .sync_to_disk_async()
            .expect("Failed to start first async sync");

        // Add more data
        vocab.insert("world");

        // Start second sync (independent of first)
        let handle2 = vocab
            .sync_to_disk_async()
            .expect("Failed to start second async sync");

        // Wait for both handles
        handle1.wait().expect("First sync failed");
        handle2.wait().expect("Second sync failed");

        // Both should complete successfully
        assert!(handle1.is_synced());
        assert!(handle2.is_synced());
    }

    #[test]
    fn test_sync_to_disk_no_fragmentation() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        {
            let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");
            for i in 0..100 {
                vocab.insert(&format!("word{}", i));
            }
            vocab.sync_to_disk().expect("First sync failed");
            let size_after_first = std::fs::metadata(&path)
                .expect("Failed to get metadata")
                .len();

            vocab.sync_to_disk().expect("Second sync failed"); // No new data
            let size_after_second = std::fs::metadata(&path)
                .expect("Failed to get metadata")
                .len();

            // File size should not increase without new data
            assert_eq!(
                size_after_first, size_after_second,
                "File grew without new data (fragmentation detected)"
            );
        }
    }

    #[test]
    fn test_sync_to_disk_then_checkpoint() {
        // This test verifies the intended usage pattern:
        // sync_to_disk() can be called multiple times during work,
        // but checkpoint() is needed for proper persistence
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");
        vocab.insert("hello");
        // sync_to_disk is a no-op for now (dirty arenas flushed)
        vocab.sync_to_disk().expect("First sync failed");
        vocab.insert("world");
        vocab.sync_to_disk().expect("Second sync failed");

        // Data should still be accessible in the same session
        assert!(vocab.contains("hello"), "Missing 'hello' after sync");
        assert!(vocab.contains("world"), "Missing 'world' after sync");
        assert_eq!(vocab.len(), 2);

        // Checkpoint for final persistence
        vocab.checkpoint().expect("Checkpoint failed");
        drop(vocab);

        // Now reopen and verify
        let (vocab, report) =
            PersistentVocabARTrie::open_with_recovery(&path).expect("Failed to open vocab");
        assert!(
            report.mode.is_normal(),
            "Should not need WAL replay after checkpoint"
        );
        assert!(vocab.contains("hello"), "Missing 'hello' after reopen");
        assert!(vocab.contains("world"), "Missing 'world' after reopen");
    }

    #[test]
    fn test_sync_to_disk_crash_recovery_via_wal() {
        // This test verifies that sync_to_disk + WAL provides crash recovery
        // even without a final checkpoint. The data is recovered via WAL replay.
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        {
            let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");
            vocab.insert("hello");
            vocab.insert("world");
            // Sync the WAL to ensure records are written
            vocab.sync().expect("WAL sync failed");
            // Intentionally forget without checkpoint to simulate crash
            std::mem::forget(vocab);
        }

        {
            let (vocab, report) =
                PersistentVocabARTrie::open_with_recovery(&path).expect("Failed to open vocab");
            // WAL replay should recover the data
            assert!(report.records_replayed > 0, "Expected WAL replay");
            assert!(
                vocab.contains("hello"),
                "Missing 'hello' after WAL recovery"
            );
            assert!(
                vocab.contains("world"),
                "Missing 'world' after WAL recovery"
            );
        }
    }

    #[test]
    fn test_sync_to_disk_concurrent_reads_writes() {
        use std::thread;

        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let vocab = Arc::new(RwLock::new(
            PersistentVocabARTrie::create(&path).expect("Failed to create vocab"),
        ));

        // Insert initial data
        for i in 0..50 {
            vocab.write().insert(&format!("initial_{}", i));
        }

        // Start async sync
        let handle = vocab
            .read()
            .sync_to_disk_async()
            .expect("Failed to start async sync");

        // Spawn readers while sync is in progress
        let vocab_clone = Arc::clone(&vocab);
        let reader_handle = thread::spawn(move || {
            for i in 0..50 {
                let _found = vocab_clone.read().contains(&format!("initial_{}", i));
            }
        });

        // Spawn writers while sync is in progress
        let vocab_clone2 = Arc::clone(&vocab);
        let writer_handle = thread::spawn(move || {
            for i in 50..100 {
                vocab_clone2.write().insert(&format!("concurrent_{}", i));
            }
        });

        // Wait for all threads
        reader_handle.join().expect("Reader thread panicked");
        writer_handle.join().expect("Writer thread panicked");
        handle.wait().expect("Sync failed");

        // Verify all data is present
        let vocab_guard = vocab.read();
        for i in 0..50 {
            assert!(
                vocab_guard.contains(&format!("initial_{}", i)),
                "Missing initial_{}",
                i
            );
        }
        for i in 50..100 {
            assert!(
                vocab_guard.contains(&format!("concurrent_{}", i)),
                "Missing concurrent_{}",
                i
            );
        }
    }

    #[test]
    fn test_sync_to_disk_wait_timeout() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");
        vocab.insert("test");

        let handle = vocab
            .sync_to_disk_async()
            .expect("Failed to start async sync");

        // Wait with generous timeout (sync should complete quickly for small data)
        let completed = handle
            .wait_timeout(Duration::from_secs(10))
            .expect("Sync failed");

        assert!(completed, "Sync should complete within timeout");
        assert!(
            handle.is_synced(),
            "Handle should report synced after wait_timeout"
        );
    }

    // =========================================================================
    // Additional Edge Case / Error Path Tests
    // =========================================================================

    #[test]
    fn test_empty_string_insert() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

        // Empty string should be insertable
        let idx = vocab.insert("").expect("insert empty term");
        assert_eq!(idx, 0);
        assert!(vocab.contains(""));
        assert_eq!(vocab.get_index(""), Some(0));
        assert_eq!(vocab.get_term(0), Some("".to_string()));
    }

    #[test]
    fn test_long_string_insert() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

        // Long string
        let long_term: String = "a".repeat(1000);
        let idx = vocab.insert(&long_term).expect("insert long term");
        assert_eq!(idx, 0);
        assert!(vocab.contains(&long_term));
        assert_eq!(vocab.get_index(&long_term), Some(0));
        assert_eq!(vocab.get_term(0), Some(long_term.clone()));
    }

    #[test]
    fn test_special_characters() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

        let special_chars = vec![
            "\0",          // Null byte
            "\t\n\r",      // Whitespace
            "a\0b",        // Embedded null
            "🎉🎊🎁",      // Emoji
            "αβγδε",       // Greek
            "מְזָלֵל",        // Hebrew with diacritics
            "\u{FEFF}BOM", // BOM character
        ];

        for (i, term) in special_chars.iter().enumerate() {
            let idx = vocab.insert(term).expect("insert special term");
            assert_eq!(idx, i as u64, "Failed for term: {:?}", term);
            assert!(vocab.contains(term), "Not found: {:?}", term);
            assert_eq!(
                vocab.get_index(term),
                Some(i as u64),
                "Index mismatch: {:?}",
                term
            );
            assert_eq!(
                vocab.get_term(i as u64),
                Some(term.to_string()),
                "Reverse lookup failed: {:?}",
                term
            );
        }
    }

    #[test]
    fn test_open_nonexistent_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.vocab");

        let result = PersistentVocabARTrie::open(&path);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_nested_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("deeply/nested/path/test.vocab");

        // Should create parent directories
        let vocab = PersistentVocabARTrie::create(&path);
        assert!(vocab.is_ok(), "Should create nested directories");
    }

    #[test]
    fn test_serialization_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Create and populate
        {
            let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

            // Insert various types of strings
            vocab.insert("simple");
            vocab.insert("日本語");
            vocab.insert("");
            vocab.insert("with spaces and punctuation!");
            vocab.insert(&"x".repeat(100));

            vocab.checkpoint().unwrap();
        }

        // Reopen and verify serialization roundtrip
        {
            let (vocab, _) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();

            assert_eq!(vocab.len(), 5);
            assert_eq!(vocab.get_index("simple"), Some(0));
            assert_eq!(vocab.get_index("日本語"), Some(1));
            assert_eq!(vocab.get_index(""), Some(2));
            assert_eq!(vocab.get_index("with spaces and punctuation!"), Some(3));
            assert_eq!(vocab.get_index(&"x".repeat(100)), Some(4));

            // Verify reverse lookups
            assert_eq!(vocab.get_term(0), Some("simple".to_string()));
            assert_eq!(vocab.get_term(1), Some("日本語".to_string()));
            assert_eq!(vocab.get_term(2), Some("".to_string()));
            assert_eq!(
                vocab.get_term(3),
                Some("with spaces and punctuation!".to_string())
            );
            assert_eq!(vocab.get_term(4), Some("x".repeat(100)));
        }
    }

    #[test]
    fn test_large_vocabulary_serialization() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Create large vocabulary
        {
            let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

            for i in 0..1000 {
                vocab.insert(&format!("term_{:05}", i));
            }

            vocab.checkpoint().unwrap();
        }

        // Reopen and verify
        {
            let (vocab, _) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();

            assert_eq!(vocab.len(), 1000);

            // Verify some entries
            for i in [0, 100, 500, 999] {
                let term = format!("term_{:05}", i);
                assert_eq!(vocab.get_index(&term), Some(i as u64));
                assert_eq!(vocab.get_term(i as u64), Some(term));
            }
        }
    }

    #[test]
    fn test_get_value_trait() {
        use crate::MappedDictionary;

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
        vocab.insert("test");

        // MappedDictionary::get_value should return the index
        assert_eq!(MappedDictionary::get_value(&vocab, "test"), Some(0));
        assert_eq!(MappedDictionary::get_value(&vocab, "missing"), None);
    }

    #[test]
    fn test_checkpoint_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
        vocab.insert("test");

        // Multiple checkpoints should be safe
        vocab.checkpoint().unwrap();
        vocab.checkpoint().unwrap();
        vocab.checkpoint().unwrap();

        // Verify data is still correct
        assert_eq!(vocab.len(), 1);
        assert!(vocab.contains("test"));
    }

    #[test]
    fn test_sync_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();
        vocab.insert("test");

        // Multiple syncs should be safe
        vocab.sync().unwrap();
        vocab.sync().unwrap();
        vocab.sync().unwrap();

        // Verify data is still correct
        assert_eq!(vocab.len(), 1);
        assert!(vocab.contains("test"));
    }

    #[test]
    fn test_next_index_tracking() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).unwrap();

        assert_eq!(vocab.next_index(), 0);

        vocab.insert("first");
        assert_eq!(vocab.next_index(), 1);

        vocab.insert("second");
        assert_eq!(vocab.next_index(), 2);

        // Duplicate insert shouldn't change next_index
        vocab.insert("first");
        assert_eq!(vocab.next_index(), 2);
    }

    #[test]
    fn test_custom_start_index_serialization() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.vocab");

        // Create with custom start index
        {
            let mut vocab = PersistentVocabARTrie::create_with_start_index(&path, 1000).unwrap();
            vocab.insert("test");
            assert_eq!(vocab.get_index("test"), Some(1000));
            vocab.checkpoint().unwrap();
        }

        // Reopen and verify start index is preserved
        {
            let (vocab, _) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();
            assert_eq!(vocab.start_index(), 1000);
            assert_eq!(vocab.get_index("test"), Some(1000));
        }
    }

    // ========================================================================
    // insert_batch tests
    // ========================================================================

    #[test]
    fn test_insert_batch_basic() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");

        // Batch insert multiple terms
        let indices = vocab
            .insert_batch(&["apple", "banana", "cherry"])
            .expect("insert batch");

        // Verify sequential indices assigned
        assert_eq!(indices, vec![0, 1, 2]);
        assert_eq!(vocab.len(), 3);

        // Verify forward lookups
        assert_eq!(vocab.get_index("apple"), Some(0));
        assert_eq!(vocab.get_index("banana"), Some(1));
        assert_eq!(vocab.get_index("cherry"), Some(2));

        // Verify reverse lookups
        assert_eq!(vocab.get_term(0), Some("apple".to_string()));
        assert_eq!(vocab.get_term(1), Some("banana".to_string()));
        assert_eq!(vocab.get_term(2), Some("cherry".to_string()));
    }

    #[test]
    fn test_insert_batch_with_duplicates() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");

        // Insert some terms first
        vocab.insert("apple");
        vocab.insert("banana");

        // Batch insert with some duplicates
        let indices = vocab
            .insert_batch(&["apple", "cherry", "banana", "date"])
            .expect("insert batch with duplicates");

        // Duplicates should return existing indices
        assert_eq!(indices, vec![0, 2, 1, 3]);
        assert_eq!(vocab.len(), 4);
    }

    #[test]
    fn test_insert_batch_empty() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");

        // Empty batch should return empty vec
        let indices = vocab.insert_batch(&[]).expect("insert empty batch");
        assert!(indices.is_empty());
        assert_eq!(vocab.len(), 0);
    }

    #[test]
    fn test_insert_batch_wal_recovery() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        // Phase 1: Batch insert and sync WAL (no checkpoint)
        {
            let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");
            let indices = vocab
                .insert_batch(&["apple", "banana", "cherry"])
                .expect("insert batch");
            assert_eq!(indices, vec![0, 1, 2]);
            vocab.sync().expect("Sync failed");
            // No checkpoint - data only in WAL
        }

        // Phase 2: Reopen and verify WAL recovery replayed batch insert
        {
            let (vocab, report) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();

            // Should have recovered all 3 terms from WAL
            assert_eq!(vocab.len(), 3, "WAL recovery should restore all 3 terms");
            assert_eq!(vocab.get_index("apple"), Some(0));
            assert_eq!(vocab.get_index("banana"), Some(1));
            assert_eq!(vocab.get_index("cherry"), Some(2));

            // Verify reverse lookups
            assert_eq!(vocab.get_term(0), Some("apple".to_string()));
            assert_eq!(vocab.get_term(1), Some("banana".to_string()));
            assert_eq!(vocab.get_term(2), Some("cherry".to_string()));
        }
    }

    // ========================================================================
    // rotate_wal tests
    // ========================================================================

    #[test]
    fn test_rotate_wal_basic() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");
        vocab.enable_slot_tracking();

        // Insert data
        vocab.insert("hello");
        vocab.insert("world");
        assert!(vocab.is_dirty());

        // Rotate WAL (should sync but not full checkpoint)
        vocab.rotate_wal().expect("rotate_wal failed");

        // Data should still be accessible
        assert!(vocab.contains("hello"));
        assert!(vocab.contains("world"));
        assert_eq!(vocab.len(), 2);
    }

    #[test]
    fn test_rotate_wal_recovery() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        // Phase 1: Insert and rotate WAL (no full checkpoint)
        {
            let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");
            vocab.enable_slot_tracking();
            vocab.insert("apple");
            vocab.insert("banana");
            vocab.rotate_wal().expect("rotate_wal failed");
            // Note: rotate_wal does NOT truncate WAL, so data is recoverable
        }

        // Phase 2: Reopen and verify WAL recovery
        {
            let (vocab, _report) = PersistentVocabARTrie::open_with_recovery(&path).unwrap();

            // Should have recovered from WAL
            assert_eq!(vocab.len(), 2, "WAL recovery should restore 2 terms");
            assert_eq!(vocab.get_index("apple"), Some(0));
            assert_eq!(vocab.get_index("banana"), Some(1));
        }
    }

    #[test]
    fn test_rotate_wal_multiple_batches() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");
        vocab.enable_slot_tracking();

        // Multiple insert batches with WAL rotation between them
        vocab.insert_batch(&["apple", "banana"]);
        vocab.rotate_wal().expect("First rotate_wal failed");

        vocab.insert_batch(&["cherry", "date"]);
        vocab.rotate_wal().expect("Second rotate_wal failed");

        vocab.insert_batch(&["elderberry"]);
        vocab.rotate_wal().expect("Third rotate_wal failed");

        // All 5 terms should be present
        assert_eq!(vocab.len(), 5);
        assert_eq!(vocab.get_index("apple"), Some(0));
        assert_eq!(vocab.get_index("elderberry"), Some(4));
    }

    #[test]
    fn test_insert_cas_basic() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");
        vocab.enable_lockfree();

        // Insert using CAS
        let idx1 = vocab.insert_cas("hello");
        let idx2 = vocab.insert_cas("world");
        let idx3 = vocab.insert_cas("hello"); // Duplicate

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 0); // Should return existing index

        // Verify with get_index via cache
        assert_eq!(vocab.insert_cas("hello"), 0);
        assert_eq!(vocab.insert_cas("world"), 1);
    }

    #[test]
    fn test_insert_cas_concurrent() {
        use std::thread;

        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");
        vocab.enable_lockfree();

        let vocab = Arc::new(vocab);
        let num_threads = 4;
        let terms_per_thread = 100;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let v = Arc::clone(&vocab);
                thread::spawn(move || {
                    let mut indices = Vec::new();
                    for i in 0..terms_per_thread {
                        let term = format!("thread{}_{}", t, i);
                        let idx = v.insert_cas(&term);
                        indices.push(idx);
                    }
                    indices
                })
            })
            .collect();

        let all_indices: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().expect("thread"))
            .collect();

        // All indices should be in valid range
        let max_expected = (num_threads * terms_per_thread) as u64;
        for &idx in &all_indices {
            assert!(idx < max_expected + 100, "index {} out of range", idx);
        }

        // Next index should be at least num_threads * terms_per_thread
        assert!(vocab.next_index() >= (num_threads * terms_per_thread) as u64);
    }

    #[test]
    fn test_insert_cas_merge_to_persistent() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("vocab.vocab");

        let mut vocab = PersistentVocabARTrie::create(&path).expect("Failed to create vocab");
        vocab.enable_lockfree();

        // Insert using CAS (lock-free)
        vocab.insert_cas("apple");
        vocab.insert_cas("banana");
        vocab.insert_cas("cherry");

        // Merge to persistent trie
        let merged = vocab.merge_lockfree_to_persistent().expect("merge failed");
        assert_eq!(merged, 3);

        // Checkpoint and reopen
        vocab.checkpoint().expect("checkpoint failed");
        drop(vocab);

        let (vocab, _) =
            PersistentVocabARTrie::open_with_recovery(&path).expect("Failed to open vocab");

        // Data should be persisted
        assert_eq!(vocab.get_index("apple"), Some(0));
        assert_eq!(vocab.get_index("banana"), Some(1));
        assert_eq!(vocab.get_index("cherry"), Some(2));
    }
}
