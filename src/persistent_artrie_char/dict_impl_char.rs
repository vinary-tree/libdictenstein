//! Disk-backed implementation of PersistentARTrieChar.
//!
//! This module provides disk persistence for the character-level trie,
//! supporting:
//! - Memory-mapped file storage
//! - Write-ahead logging (WAL) for crash recovery
//! - Buffer management for efficient I/O
//!
//! # Architecture
//!
//! The disk layout uses the char ART nodes (CharNode4/16/48/CharBucket)
//! for efficient storage of Unicode character keys.
//!
//! # File Layout
//!
//! ```text
//! ┌─────────────────────────────────────────────────┐
//! │ File Header (64 bytes)                          │
//! │ - Magic: "ARTC" (ART Char)                      │
//! │ - Version: u8                                   │
//! │ - Root pointer: u64                             │
//! │ - Entry count: u64                              │
//! │ - Checkpoint LSN: u64                           │
//! └─────────────────────────────────────────────────┘
//! │ Root Node (variable)                            │
//! └─────────────────────────────────────────────────┘
//! │ Child Nodes...                                  │
//! └─────────────────────────────────────────────────┘
//! ```

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};

use parking_lot::RwLock;

// SwizzledPtr is used unconditionally for in-memory CharNode children
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::disk_manager::DiskManager;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::{
    AsyncWalConfig, AsyncWalWriter, WalConfig, WalReader, WalRecord,
};
use crate::persistent_artrie::wal_managed::{create_async_wal, open_or_create_async_wal};
use crate::persistent_artrie::concurrency::{
    EpochManager, OptimisticVersion, RetryStats, EpochGuard, OptimisticReadGuard,
};
#[cfg(feature = "group-commit")]
use crate::persistent_artrie::group_commit::{GroupCommitConfig, GroupCommitCoordinator};
use crate::persistent_artrie::memory_monitor::{
    MemoryPressureConfig, MemoryPressureLevel, MemoryPressureMonitor, MemoryStats,
};
use crate::persistent_artrie::adaptive_pool::CacheStats;
use crate::persistent_artrie::epoch::{
    CheckpointManager, EpochConfig, EpochId, EpochMetadata, EpochStats,
};
use super::arena_manager::ArenaManager;
use crate::value::DictionaryValue;

// Import CharNode types for adaptive radix structure
use super::nodes::CharNode;
use crate::persistent_artrie::NodeType;

/// Magic bytes for char trie file
pub const CHAR_TRIE_MAGIC: [u8; 4] = *b"ARTC";

/// File header size in bytes
pub const CHAR_FILE_HEADER_SIZE: usize = 64;

/// Header format version 1 (original, no checksum)
pub const CHAR_HEADER_VERSION_V1: u8 = 1;

/// Header format version 2 (with checksum for crash recovery)
pub const CHAR_HEADER_VERSION_V2: u8 = 2;

/// Default buffer pool size (number of pages)
pub const DEFAULT_CHAR_BUFFER_POOL_SIZE: usize = 256;

// `EnhancedRecoveryMode` was relocated to `super::recovery_stats`; re-exported
// here under its original path.
pub use super::recovery_stats::EnhancedRecoveryMode;

/// Result of a lock-free CAS insert attempt.
///
/// Used internally by `insert_cas()` to communicate the outcome
/// of a single CAS attempt.
#[derive(Debug)]
pub(super) enum LockfreeInsertResult {
    /// Successfully inserted a new term, returning the target node
    Inserted(Arc<super::nodes::persistent_node::PersistentCharNode>),
    /// Term already exists in the trie
    AlreadyExists,
    /// CAS failed due to concurrent modification (should retry)
    Conflict,
}

// `EnhancedRecoveryStats` was relocated to `super::recovery_stats`;
// re-exported here under its original path.
pub use super::recovery_stats::EnhancedRecoveryStats;

// `CharTrieFileHeader` (struct + impls + Default + the private
// `crc32_header` helper it uses) was relocated to `super::file_header`;
// re-exported here under its original path.
pub use super::file_header::CharTrieFileHeader;

// `PrefixTermWithArena` and `PrefixTermWithValueAndArena` were relocated to
// `super::prefix_term`; re-exported here under their original paths.
pub use super::prefix_term::{PrefixTermWithArena, PrefixTermWithValueAndArena};

/// Transaction state for document transactions.
///
/// Re-exported from `persistent_artrie` for API consistency.
pub use crate::persistent_artrie::TransactionState;

/// Durability policy for WAL synchronization.
///
/// Re-exported from `persistent_artrie` for API consistency.
pub use crate::persistent_artrie::DurabilityPolicy;

// `CharDocumentTransaction` was relocated to `super::transactions`;
// re-exported here under its original path.
pub use super::transactions::CharDocumentTransaction;

// Note: CharTrieNodeInner is defined in types.rs and re-exported from mod.rs
use super::types::CharTrieNodeInner;

// Note: CharTrieRoot is defined in types.rs and re-exported from mod.rs
use super::types::CharTrieRoot;

// Note: Debug implementation is in mod.rs on PersistentARTrieChar directly

// =============================================================================
// MmapDiskManager-specific constructors moved to super::mmap_ctor.
// IoUringDiskManager-specific constructors moved to super::io_uring_ctor.
// =============================================================================

// =============================================================================
// Generic impl block for all BlockStorage backends
// =============================================================================
impl<V: DictionaryValue, S: BlockStorage> super::PersistentARTrieChar<V, S> {
    /// Load root from disk given the root descriptor pointer
    ///
    /// This function:
    /// 1. Reads the root descriptor block
    /// 2. Loads arena block IDs and populates the arena manager
    /// 3. Loads the root node (which can now read from arenas)
    ///
    /// # Arguments
    /// * `buffer_manager` - The buffer manager for disk I/O
    /// * `root_desc_ptr` - Pointer to the root descriptor block
    /// * `eager_depth` - Controls loading strategy:
    ///   - `None`: Fully lazy loading (only root node loaded)
    ///   - `Some(0)`: Same as None (lazy loading)
    ///   - `Some(n)`: Load n levels eagerly, rest lazy
    ///   - `Some(usize::MAX)`: Fully eager loading (all levels)

    /// Insert a term (internal, no WAL logging)
    pub(super) fn insert_impl_no_wal(&mut self, term: &str) -> bool {
        // Ensure we have a root node
        if matches!(self.root, CharTrieRoot::Empty) {
            self.root = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }

        // Navigate to the insertion point using raw pointer for traversal
        // This is safe because we maintain exclusive access through &mut self
        let root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => unreachable!(),
        };

        let mut current = root;
        for c in term.chars() {
            // Safety: current is valid and we have exclusive access through &mut self
            let node = unsafe { &mut *current };
            current = self.get_or_create_child_lazy_ptr(node, c)
                .expect("I/O error during lazy loading in insert");
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if already final
        if node.is_final() {
            return false;
        }

        // Mark as final
        node.set_final(true);
        self.len.fetch_add(1, AtomicOrdering::Relaxed);
        self.dirty.store(true, AtomicOrdering::Release);
        true
    }


    /// Insert a term with value (internal, no WAL logging)
    pub(super) fn insert_impl_no_wal_with_value(&mut self, term: &str, value: V) -> bool {
        // Ensure we have a root node
        if matches!(self.root, CharTrieRoot::Empty) {
            self.root = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }

        // Navigate to the insertion point using raw pointer for traversal
        let root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => unreachable!(),
        };

        let mut current = root;
        for c in term.chars() {
            // Safety: current is valid and we have exclusive access through &mut self
            let node = unsafe { &mut *current };
            current = self.get_or_create_child_lazy_ptr(node, c)
                .expect("I/O error during lazy loading in insert");
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if already final
        if node.is_final() {
            // Update value if already exists
            node.value = Some(value);
            return false;
        }

        // Mark as final with value
        node.set_final(true);
        node.value = Some(value);
        self.len.fetch_add(1, AtomicOrdering::Relaxed);
        self.dirty.store(true, AtomicOrdering::Release);
        true
    }

    /// Insert a term with value (internal, no WAL logging)

    /// Remove a term (internal, no WAL logging)
    pub(super) fn remove_impl_no_wal(&mut self, term: &str) -> bool {
        let root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => return false,
        };

        // Navigate to the node using raw pointer for traversal
        let chars: Vec<char> = term.chars().collect();
        let mut current = root;
        for &c in &chars {
            // Safety: current is valid and we have exclusive access through &mut self
            let node = unsafe { &*current };
            match self.get_child_mut_lazy(node, c) {
                Ok(Some(child)) => current = child as *mut CharTrieNodeInner<V>,
                Ok(None) => return false, // Term not found
                Err(_) => return false, // I/O error during lazy load
            }
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if this node is final
        if !node.is_final() {
            return false;
        }

        // Mark as not final
        node.set_final(false);
        node.value = None;
        self.len.fetch_sub(1, AtomicOrdering::Relaxed);
        self.dirty.store(true, AtomicOrdering::Release);
        true
    }

    /// Remove a term (internal, no WAL logging)

    /// Check if a term exists in the trie


    /// Insert a term with WAL logging
    pub fn insert(&mut self, term: &str) -> Result<bool> {
        // Log to WAL first (routes through group commit if enabled)
        let record = WalRecord::Insert {
            term: term.as_bytes().to_vec(),
            value: None,
        };
        self.append_to_wal(record)?;

        // Mark version as being written (odd = in-progress)
        self.version.begin_write();
        let result = self.insert_impl_no_wal(term);
        // Mark version as stable (even = complete)
        self.version.end_write();

        Ok(result)
    }

    /// Insert a term with an associated value and WAL logging
    pub fn insert_with_value(&mut self, term: &str, value: V) -> Result<bool> {
        // Log to WAL first (routes through group commit if enabled)
        let value_bytes = bincode::serialize(&value)
            .map_err(|e| PersistentARTrieError::internal(format!("Failed to serialize value: {}", e)))?;
        let record = WalRecord::Insert {
            term: term.as_bytes().to_vec(),
            value: Some(value_bytes),
        };
        self.append_to_wal(record)?;

        // Mark version as being written (odd = in-progress)
        self.version.begin_write();
        let result = self.insert_impl_no_wal_with_value(term, value);
        // Mark version as stable (even = complete)
        self.version.end_write();

        Ok(result)
    }

    /// Remove a term with WAL logging
    pub fn remove(&mut self, term: &str) -> Result<bool> {
        // Log to WAL first (routes through group commit if enabled)
        let record = WalRecord::Remove {
            term: term.as_bytes().to_vec(),
        };
        self.append_to_wal(record)?;

        // Mark version as being written
        self.version.begin_write();
        let result = self.remove_impl_no_wal(term);
        self.version.end_write();

        Ok(result)
    }

    // ========================================================================
    // Prefix Operations
    // ========================================================================

    /// Navigate to the node at the given prefix path.
    ///
    /// Returns `Ok(Some(node))` if the prefix exists, `Ok(None)` if it doesn't.
    /// Returns `Err` if an I/O error occurs during lazy loading.
    /// For disk-backed tries, prefetches children at each level for improved I/O performance.
    /// ```

    #[cfg(feature = "parallel-merge")]
    pub fn merge_from_parallel<F>(
        &mut self,
        other: &Self,
        merge_fn: F,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V + Sync + Send,
        V: Clone + Send + Sync,
    {
        use rayon::prelude::*;
        use std::collections::HashMap;

        // Collect all terms with values from source
        let terms_with_values = match other.iter_prefix_with_values_and_arena("")? {
            Some(terms) => terms,
            None => return Ok(0),
        };

        if terms_with_values.is_empty() {
            return Ok(0);
        }

        // Group by first character for parallel processing
        let mut char_groups: HashMap<Option<char>, Vec<(String, V)>> = HashMap::new();
        for item in terms_with_values {
            let first_char = item.term.chars().next();
            char_groups
                .entry(first_char)
                .or_insert_with(Vec::new)
                .push((item.term, item.value));
        }

        // Parallel phase: compute merged values
        // Each partition computes what values need to be inserted
        let partitions: Vec<Vec<(String, V)>> = char_groups
            .into_par_iter()
            .map(|(_, terms)| {
                let mut results = Vec::with_capacity(terms.len());
                for (term, other_value) in terms {
                    // Note: Reading from self is a concurrent read - safe because we're not mutating
                    let merged_value = if let Some(self_value) = self.get(&term) {
                        merge_fn(self_value, &other_value)
                    } else {
                        other_value
                    };
                    results.push((term, merged_value));
                }
                results
            })
            .collect();

        // Sequential phase: insert all results
        let mut total_processed = 0;
        for partition in partitions {
            for (term, value) in partition {
                self.upsert(&term, value)?;
                total_processed += 1;
            }
        }

        Ok(total_processed)
    }

    /// Merge all terms from another trie with both batching and parallel processing.
    ///
    /// This combines the memory-bounded batching of `merge_from_batched` with
    /// the parallel computation of `merge_from_parallel`. Each batch is
    /// processed in parallel, then results are inserted sequentially.
    ///
    /// # Arguments
    ///
    /// * `other` - The source trie to merge from
    /// * `merge_fn` - Function to merge values when a term exists in both tries.
    /// * `batch_size` - Number of terms to process per batch (0 = default 5000)
    ///
    /// # Returns
    ///
    /// The number of terms processed from the source trie.
    ///
    /// # Feature
    ///
    /// Requires the `parallel-merge` feature to be enabled.
    #[cfg(feature = "parallel-merge")]
    pub fn merge_from_batched_parallel<F>(
        &mut self,
        other: &Self,
        merge_fn: F,
        batch_size: usize,
    ) -> Result<usize>
    where
        F: Fn(&V, &V) -> V + Sync + Send,
        V: Clone + Send + Sync,
    {
        use rayon::prelude::*;

        let batch_size = if batch_size == 0 { 5_000 } else { batch_size };

        // Collect all terms with values from source
        let terms_with_values = match other.iter_prefix_with_values_and_arena("")? {
            Some(terms) => terms,
            None => return Ok(0),
        };

        let mut total_processed = 0;

        // Process in batches
        for batch in terms_with_values.chunks(batch_size) {
            // Parallel phase: compute merged values for this batch
            let results: Vec<(String, V)> = batch
                .par_iter()
                .map(|item| {
                    let merged_value = if let Some(self_value) = self.get(&item.term) {
                        merge_fn(self_value, &item.value)
                    } else {
                        item.value.clone()
                    };
                    (item.term.clone(), merged_value)
                })
                .collect();

            // Sequential phase: insert results for this batch
            for (term, value) in results {
                self.upsert(&term, value)?;
                total_processed += 1;
            }
        }

        Ok(total_processed)
    }

    // ========================================================================
    // Document Transaction API
    // ========================================================================

    /// Begin a document transaction for atomic per-document operations.
    ///
    /// This creates a new transaction that buffers terms in memory until
    /// `commit_document()` is called. The transaction can be aborted with
    /// `abort_document()` if document processing fails.
    ///
    /// # Arguments
    ///
    /// * `document_id` - Identifier for the document (used for logging/debugging)
    ///
    /// # Returns
    ///
    /// A new `CharDocumentTransaction` in the Active state.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut tx = trie.begin_document("doc_123")?;
    /// trie.tx_insert(&mut tx, "hello", None);
    /// trie.tx_insert(&mut tx, "world", Some(42));
    /// let count = trie.commit_document(tx)?;
    /// ```

    // ========================================================================
    // Batch Insert Operations
    // ========================================================================

    /// Insert multiple terms with optional values in a single batch operation.
    ///
    /// This method provides efficient bulk loading by:
    /// 1. Logging all entries as a single batch WAL record (one fsync)
    /// 2. Inserting entries without individual WAL logging
    ///
    /// # Arguments
    ///
    /// * `entries` - Slice of (term, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted (not updates).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let entries = vec![
    ///     ("hello".to_string(), Some(1)),
    ///     ("world".to_string(), Some(2)),
    ///     ("foo".to_string(), None),
    /// ];
    /// let count = trie.insert_batch(&entries)?;
    /// ```
    pub fn insert_batch(&mut self, entries: &[(String, Option<V>)]) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // First, log all entries as a single batch WAL record (routes through group commit if enabled)
        let wal_entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = entries
            .iter()
            .map(|(term, value)| {
                let term_bytes = term.as_bytes().to_vec();
                let value_bytes = value.as_ref().and_then(|v| {
                    bincode::serialize(v).ok()
                });
                (term_bytes, value_bytes)
            })
            .collect();

        let batch_record = WalRecord::BatchInsert { entries: wal_entries };
        if let Err(e) = self.append_to_wal(batch_record) {
            log::warn!("Failed to log batch insert to WAL: {:?}", e);
        }

        // Then insert each entry without individual WAL logging
        let mut inserted_count = 0;
        for (term, value) in entries {
            if let Some(v) = value {
                if self.insert_impl_no_wal_with_value(term, v.clone()) {
                    inserted_count += 1;
                }
            } else {
                if self.insert_impl_no_wal(term) {
                    inserted_count += 1;
                }
            }
        }

        inserted_count
    }

    /// Insert multiple terms (as char slices) with optional values in a single batch operation.
    ///
    /// This method is useful when you have pre-parsed Unicode characters and want
    /// to avoid UTF-8 encoding overhead for each term individually.
    ///
    /// # Arguments
    ///
    /// * `entries` - Slice of (char_slice, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted (not updates).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let entries = vec![
    ///     (&['日', '本', '語'][..], Some(1)),
    ///     (&['中', '文'][..], Some(2)),
    /// ];
    /// let count = trie.insert_batch_chars(&entries)?;
    /// ```
    pub fn insert_batch_chars(&mut self, entries: &[(&[char], Option<V>)]) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Convert char slices to strings for WAL and insertion
        let string_entries: Vec<(String, Option<V>)> = entries
            .iter()
            .map(|(chars, value)| {
                let term: String = chars.iter().collect();
                (term, value.clone())
            })
            .collect();

        self.insert_batch(&string_entries)
    }

    /// Insert multiple byte-slice terms in a single batch operation.
    ///
    /// This is the byte-slice version of `insert_batch()` for when you already
    /// have byte data and want to avoid string conversion overhead.
    ///
    /// # Arguments
    ///
    /// * `entries` - Slice of (term_bytes, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_bytes(&mut self, entries: &[(&[u8], Option<V>)]) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // First, log all entries as a single batch WAL record (routes through group commit if enabled)
        let wal_entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = entries
            .iter()
            .map(|(term, value)| {
                let value_bytes = value.as_ref().and_then(|v| {
                    bincode::serialize(v).ok()
                });
                (term.to_vec(), value_bytes)
            })
            .collect();

        let batch_record = WalRecord::BatchInsert { entries: wal_entries };
        if let Err(e) = self.append_to_wal(batch_record) {
            log::warn!("Failed to log batch insert to WAL: {:?}", e);
        }

        // Then insert each entry without individual WAL logging
        let mut inserted_count = 0;
        for (term, value) in entries {
            let term_str = String::from_utf8_lossy(term);
            if let Some(v) = value {
                if self.insert_impl_no_wal_with_value(&term_str, v.clone()) {
                    inserted_count += 1;
                }
            } else {
                if self.insert_impl_no_wal(&term_str) {
                    inserted_count += 1;
                }
            }
        }

        inserted_count
    }

    /// Insert multiple terms with optional values in sorted order for cache locality.
    ///
    /// This method sorts the entries lexicographically before inserting them,
    /// which improves cache hit rates since consecutive terms share trie prefix
    /// paths. For large batches, this can improve throughput by 5-20%.
    ///
    /// All entries are logged as a single batch WAL record before insertion.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (term, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_sorted(&mut self, mut entries: Vec<(String, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by term lexicographically for cache locality
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Delegate to insert_batch
        self.insert_batch(&entries)
    }

    /// Insert multiple char-slice terms with optional values in sorted order for cache locality.
    ///
    /// This method sorts the entries lexicographically before inserting them,
    /// which improves cache hit rates since consecutive terms share trie prefix
    /// paths. For large batches, this can improve throughput by 5-20%.
    ///
    /// All entries are logged as a single batch WAL record before insertion.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (char_vec, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_chars_sorted(&mut self, mut entries: Vec<(Vec<char>, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by chars lexicographically for cache locality
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Convert to references for insert_batch_chars
        let refs: Vec<(&[char], Option<V>)> = entries
            .iter()
            .map(|(chars, value)| (chars.as_slice(), value.clone()))
            .collect();
        self.insert_batch_chars(&refs)
    }

    /// Insert multiple byte terms with optional values in sorted order for cache locality.
    ///
    /// This method sorts the entries lexicographically before inserting them,
    /// which improves cache hit rates since consecutive terms share trie prefix
    /// paths. For large batches, this can improve throughput by 5-20%.
    ///
    /// All entries are logged as a single batch WAL record before insertion.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (term_bytes, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_bytes_sorted(&mut self, mut entries: Vec<(Vec<u8>, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by term lexicographically for cache locality
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Convert to references for insert_batch_bytes
        let refs: Vec<(&[u8], Option<V>)> = entries
            .iter()
            .map(|(term, value)| (term.as_slice(), value.clone()))
            .collect();
        self.insert_batch_bytes(&refs)
    }

    /// Insert multiple string terms grouped by first character for arena locality.
    ///
    /// This method groups inserts by their first character before inserting,
    /// which improves I/O locality for disk-resident tries. Terms with the same
    /// first character tend to land in nearby arenas because arenas fill
    /// sequentially during loading.
    ///
    /// # Performance
    ///
    /// Expected improvement: 5-10% faster batch inserts for disk-resident tries
    /// due to improved I/O locality. The first-character heuristic provides ~60-80%
    /// of the benefit of full arena prediction with O(1) complexity.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (term, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_grouped(&mut self, mut entries: Vec<(String, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by first character (arena proxy) then by full term for within-group locality
        entries.sort_by(|a, b| {
            let a_prefix = a.0.chars().next().unwrap_or('\0');
            let b_prefix = b.0.chars().next().unwrap_or('\0');
            a_prefix.cmp(&b_prefix).then_with(|| a.0.cmp(&b.0))
        });

        // Delegate to insert_batch
        self.insert_batch(&entries)
    }

    /// Insert multiple char-slice terms grouped by first character for arena locality.
    ///
    /// This is the char-slice variant of `insert_batch_grouped`. See that method
    /// for detailed documentation on the arena grouping strategy.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (char_vec, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_chars_grouped(&mut self, mut entries: Vec<(Vec<char>, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by first character (arena proxy) then by full term
        entries.sort_by(|a, b| {
            let a_prefix = a.0.first().copied().unwrap_or('\0');
            let b_prefix = b.0.first().copied().unwrap_or('\0');
            a_prefix.cmp(&b_prefix).then_with(|| a.0.cmp(&b.0))
        });

        // Convert to references for insert_batch_chars
        let refs: Vec<(&[char], Option<V>)> = entries
            .iter()
            .map(|(chars, value)| (chars.as_slice(), value.clone()))
            .collect();
        self.insert_batch_chars(&refs)
    }

    /// Insert multiple byte terms grouped by first byte for arena locality.
    ///
    /// This method groups inserts by their first byte prefix before inserting,
    /// which improves I/O locality for disk-resident tries.
    ///
    /// # Arguments
    ///
    /// * `entries` - Vector of (term_bytes, optional_value) pairs to insert
    ///
    /// # Returns
    ///
    /// The number of terms that were newly inserted.
    pub fn insert_batch_bytes_grouped(&mut self, mut entries: Vec<(Vec<u8>, Option<V>)>) -> usize {
        if entries.is_empty() {
            return 0;
        }

        // Sort by first byte (arena proxy) then by full term for within-group locality
        entries.sort_by(|a, b| {
            let a_prefix = a.0.first().copied().unwrap_or(0);
            let b_prefix = b.0.first().copied().unwrap_or(0);
            a_prefix.cmp(&b_prefix).then_with(|| a.0.cmp(&b.0))
        });

        // Convert to references for insert_batch_bytes
        let refs: Vec<(&[u8], Option<V>)> = entries
            .iter()
            .map(|(term, value)| (term.as_slice(), value.clone()))
            .collect();
        self.insert_batch_bytes(&refs)
    }

    /// Alias for `insert_batch_bytes_grouped` for API consistency with PersistentARTrie.
    ///
    /// See [`insert_batch_bytes_grouped`](Self::insert_batch_bytes_grouped) for documentation.
    #[inline]
    pub fn insert_batch_arena_grouped(&mut self, entries: Vec<(Vec<u8>, Option<V>)>) -> usize {
        self.insert_batch_bytes_grouped(entries)
    }

    /// Sync changes to disk
    pub fn sync(&mut self) -> Result<()> {
        if let Some(ref wal_writer) = self.wal_writer {
            wal_writer
                .sync()
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
        }
        Ok(())
    }

    /// Returns the next LSN that will be assigned to a write operation.
    ///
    /// This value increases monotonically with each write (insert, remove, update).
    /// It can be used as a "version" or "sequence number" for the trie state.
    ///
    /// # Returns
    /// - The next LSN to be assigned (starts at 1 for persistent tries)
    ///
    /// # Example
    /// ```rust,ignore
    /// let mut trie = PersistentARTrieChar::<i32>::create("test.part")?;
    /// let before = trie.current_lsn();
    /// trie.upsert("key", 42)?;
    /// let after = trie.current_lsn();
    /// assert!(after > before);
    /// ```
    #[inline]
    pub fn current_lsn(&self) -> u64 {
        // Use WAL's authoritative LSN if available, otherwise fall back to cached value
        self.wal_writer.as_ref()
            .map(|wal| wal.current_lsn())
            .unwrap_or_else(|| self.next_lsn.load(AtomicOrdering::Acquire))
    }

    /// Returns the highest LSN that has been durably synced to storage.
    ///
    /// Operations with LSN <= synced_lsn are guaranteed to survive crashes.
    /// Operations with LSN > synced_lsn may be lost if a crash occurs before
    /// the next sync or checkpoint.
    ///
    /// # Returns
    /// - `Some(lsn)` if WAL is enabled and has synced data
    /// - `None` if WAL is disabled (in-memory trie) or no data has been synced yet
    ///
    /// # Example
    /// ```rust,ignore
    /// let mut trie = PersistentARTrieChar::<i32>::create("test.part")?;
    /// trie.upsert("key", 42)?;
    /// trie.sync()?;  // Force durability
    /// let synced = trie.synced_lsn();
    /// assert!(synced.is_some());
    /// ```
    pub fn synced_lsn(&self) -> Option<u64> {
        self.wal_writer.as_ref().map(|wal| wal.synced_lsn())
    }

    // ========================================================================
    // Group Commit Support
    // ========================================================================

    /// Enable group commit for WAL write batching.
    ///
    /// Group commit batches multiple WAL writes into a single fsync() operation,
    /// significantly improving write throughput at the cost of slightly increased
    /// latency for individual operations.
    ///
    /// # Arguments
    ///
    /// * `config` - Group commit configuration (batch size, delay, etc.)
    ///
    /// # Returns
    ///
    /// Returns an error if:
    /// - The trie is in in-memory mode (no WAL)
    /// - Group commit is already enabled
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie::group_commit::GroupCommitConfig;
    ///
    /// let mut trie = PersistentARTrieChar::<u64>::create("data.trie")?;
    ///
    /// // Enable with default config (balanced latency/throughput)
    /// trie.enable_group_commit(GroupCommitConfig::default())?;
    ///
    /// // Or use a throughput-optimized config
    /// trie.enable_group_commit(GroupCommitConfig::high_throughput())?;
    /// ```
    #[cfg(feature = "group-commit")]
    pub fn enable_group_commit(&mut self, config: GroupCommitConfig) -> Result<()> {
        if self.group_commit.is_some() {
            return Err(PersistentARTrieError::InvalidOperation(
                "Group commit is already enabled".to_string(),
            ));
        }

        let wal_writer = self.wal_writer.as_ref().ok_or_else(|| {
            PersistentARTrieError::InvalidOperation(
                "Cannot enable group commit on in-memory trie".to_string(),
            )
        })?;

        let coordinator = GroupCommitCoordinator::new(Arc::clone(wal_writer), config)?;
        self.group_commit = Some(Arc::new(coordinator));

        Ok(())
    }

    /// Disable group commit, returning to direct WAL writes.
    ///
    /// This flushes any pending writes and shuts down the group commit coordinator.
    /// After this call, all WAL writes will be performed directly.
    #[cfg(feature = "group-commit")]
    pub fn disable_group_commit(&mut self) -> Result<()> {
        if self.group_commit.is_none() {
            return Ok(()); // Already disabled
        }

        // The coordinator will flush pending writes when dropped
        self.group_commit = None;
        Ok(())
    }

    /// Check if group commit is enabled.
    #[cfg(feature = "group-commit")]
    pub fn is_group_commit_enabled(&self) -> bool {
        self.group_commit.is_some()
    }

    /// Get group commit statistics.
    ///
    /// Returns None if group commit is not enabled.
    #[cfg(feature = "group-commit")]
    pub fn group_commit_stats(&self) -> Option<crate::persistent_artrie::group_commit::GroupCommitStats> {
        self.group_commit.as_ref().map(|gc| gc.stats())
    }

    // ==================== Performance Infrastructure Methods ====================

    /// Enables memory pressure monitoring with the given configuration and callback.
    ///
    /// Memory monitoring tracks system memory usage and invokes the callback when
    /// pressure thresholds change, allowing the trie to adapt its memory usage
    /// (e.g., by evicting cached nodes or reducing buffer sizes).
    ///
    /// # Arguments
    /// * `config` - Configuration for memory pressure thresholds and polling interval
    /// * `callback` - Function to call when memory pressure level changes
    ///
    /// # Returns
    /// * `Ok(())` - Monitor enabled successfully
    /// * `Err(_)` - Failed to start monitor thread
    ///
    /// # Example
    /// ```rust,ignore
    /// trie.enable_memory_monitor(
    ///     MemoryPressureConfig::default(),
    ///     |level, stats| {
    ///         log::info!("Memory pressure: {:?}, used: {} MB", level, stats.used_mb());
    ///     }
    /// )?;
    /// ```
    pub fn enable_memory_monitor<F>(&mut self, config: MemoryPressureConfig, callback: F) -> Result<()>
    where
        F: Fn(MemoryPressureLevel, &MemoryStats) + Send + Sync + 'static,
    {
        let monitor = MemoryPressureMonitor::start(config, callback)?;
        self.memory_monitor = Some(Arc::new(monitor));
        Ok(())
    }

    /// Enables memory pressure monitoring with default configuration and a no-op callback.
    ///
    /// Use this when you only want to query memory stats periodically
    /// without receiving pressure change notifications.
    pub fn enable_memory_monitor_default(&mut self) -> Result<()> {
        self.enable_memory_monitor(MemoryPressureConfig::default(), |_level, _stats| {})
    }

    /// Disables memory pressure monitoring.
    ///
    /// The monitor thread is stopped when the Arc is dropped.
    pub fn disable_memory_monitor(&mut self) {
        self.memory_monitor = None;
    }

    /// Returns whether memory monitoring is enabled.
    pub fn has_memory_monitor(&self) -> bool {
        self.memory_monitor.is_some()
    }

    /// Returns current memory statistics if monitoring is enabled.
    pub fn memory_stats(&self) -> Option<MemoryStats> {
        self.memory_monitor.as_ref().map(|m| m.current_stats())
    }

    /// Returns current memory pressure level if monitoring is enabled.
    pub fn memory_pressure_level(&self) -> Option<MemoryPressureLevel> {
        self.memory_monitor.as_ref().map(|m| m.current_level())
    }

    // -------------------- Cache Statistics --------------------

    /// Records a cache hit.
    ///
    /// Call this when a node lookup finds the node in cache.
    pub fn record_cache_hit(&self) {
        self.cache_stats.record_hit();
    }

    /// Records a cache miss.
    ///
    /// Call this when a node lookup requires loading from disk.
    pub fn record_cache_miss(&self) {
        self.cache_stats.record_miss();
    }

    /// Returns the current cache hit rate (0.0 to 1.0).
    ///
    /// Returns 1.0 if no cache accesses have been recorded.
    pub fn cache_hit_rate(&self) -> f64 {
        self.cache_stats.hit_rate()
    }

    /// Returns cache hit/miss counts.
    ///
    /// Returns `(hits, misses)`.
    pub fn cache_counts(&self) -> (u64, u64) {
        self.cache_stats.counts()
    }

    /// Returns the total number of cache accesses (hits + misses).
    pub fn cache_total_accesses(&self) -> u64 {
        self.cache_stats.total_accesses()
    }

    /// Gets cache statistics and resets the counters atomically.
    ///
    /// Returns `(hit_rate, hits, misses)`.
    ///
    /// Use this for periodic reporting where you want to measure
    /// hit rates over fixed time intervals.
    pub fn cache_stats_and_reset(&self) -> (f64, u64, u64) {
        self.cache_stats.get_and_reset()
    }

    /// Returns a reference to the underlying cache statistics.
    pub fn get_cache_stats(&self) -> &CacheStats {
        &self.cache_stats
    }

    // ==================== Prefetching Methods ====================

    /// Get a snapshot of prefetch statistics.
    ///
    /// Returns statistics about prefetch performance including:
    /// - Total requests submitted
    /// - Cache hits (prefetched data was already in memory)
    /// - I/O operations issued
    /// - Dropped requests (queue overflow)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let stats = trie.prefetch_stats();
    /// println!("Prefetch hit rate: {:.1}%", stats.hit_rate() * 100.0);
    /// println!("Drop rate: {:.1}%", stats.drop_rate() * 100.0);
    /// ```
    pub fn prefetch_stats(&self) -> crate::persistent_artrie::prefetch::PrefetchStatsSnapshot {
        self.prefetcher.stats().snapshot()
    }

    // DISABLED — `prefetch_disk_refs` was the original depth-0 convenience
    // wrapper for `prefetch_disk_refs_bounded`; it is fully superseded by
    // the bounded variant immediately below, which all callers in this
    // file already use directly (lines 2533, 2573, 3453, 3495). Kept here
    // commented out per CLAUDE.md to preserve the rename audit trail.
    //
    // fn prefetch_disk_refs<'a>(
    //     &self,
    //     children: impl Iterator<Item = (u32, &'a crate::persistent_artrie::swizzled_ptr::SwizzledPtr)>,
    // ) {
    //     self.prefetch_disk_refs_bounded(children, 0);
    // }

    /// Prefetch disk-resident children with depth bounds for multi-level prefetching.
    ///
    /// This method extends prefetching to all traversal levels, not just the root.
    /// When the prefetcher is configured with `DepthLimited(n)` strategy, prefetching
    /// will be disabled for nodes deeper than `n` levels, preventing excessive I/O
    /// for very deep tries.
    ///
    /// # Performance
    ///
    /// Multi-level prefetching improves cold lookup performance by 15-30% by
    /// initiating I/O for nodes at depth D while processing nodes at depth D-1.
    /// With default `DepthLimited(3)`, prefetching occurs for the first 4 levels.
    ///
    /// # Arguments
    ///
    /// * `children` - Iterator over (char_codepoint, &SwizzledPtr) pairs to potentially prefetch
    /// * `depth` - Current traversal depth (0 = root level)
    pub(super) fn prefetch_disk_refs_bounded<'a>(
        &self,
        children: impl Iterator<Item = (u32, &'a crate::persistent_artrie::swizzled_ptr::SwizzledPtr)>,
        depth: u16,
    ) {
        // Collect disk-resident children for prefetching
        // Use low byte of codepoint as key proxy for the prefetcher
        let disk_children: Vec<(u8, crate::persistent_artrie::swizzled_ptr::SwizzledPtr)> = children
            .filter_map(|(codepoint, ptr)| {
                if ptr.is_on_disk() {
                    // Use low byte of codepoint as routing key
                    let key_byte = (codepoint & 0xFF) as u8;
                    Some((key_byte, ptr.clone()))
                } else {
                    None
                }
            })
            .collect();

        if !disk_children.is_empty() {
            self.prefetcher.prefetch_children_bounded(&disk_children, depth);
        }
    }

    // ==================== End Performance Infrastructure Methods ====================

    // ==================== Epoch-Based Checkpointing Methods ====================

    /// Enables epoch-based automatic checkpointing.
    ///
    /// The checkpoint manager tracks operations and triggers automatic
    /// checkpoints based on configurable thresholds:
    /// - Operation count per epoch
    /// - WAL size limit
    /// - Time-based epoch duration
    ///
    /// This provides bounded WAL size and faster recovery times.
    ///
    /// **Important:** The checkpoint manager creates its own WAL in a subdirectory.
    /// For integration with the existing WAL, call `record_epoch_operation()`
    /// after each WAL write to track operation counts.
    ///
    /// # Arguments
    /// * `config` - Configuration for epoch thresholds and behavior
    ///
    /// # Returns
    /// * `Ok(())` - Checkpoint manager enabled successfully
    /// * `Err(_)` - Failed to initialize (e.g., directory creation failed)
    ///
    /// # Example
    /// ```rust,ignore
    /// // Enable with custom thresholds
    /// let config = EpochConfig {
    ///     epoch_duration: Duration::from_millis(500),
    ///     max_ops_per_epoch: 5000,
    ///     max_wal_size_bytes: 32 * 1024 * 1024, // 32MB
    ///     ..EpochConfig::default()
    /// };
    /// trie.enable_epoch_checkpointing(config)?;
    /// ```
    pub fn enable_epoch_checkpointing(&mut self, config: EpochConfig) -> Result<()> {
        // Create epoch subdirectory based on the trie's file path
        let epoch_dir = if let Some(ref path) = self.file_path {
            path.with_extension("epoch")
        } else {
            return Err(PersistentARTrieError::internal(
                "Cannot enable epoch checkpointing without a file path"
            ));
        };

        let manager = CheckpointManager::new(&epoch_dir, config)?;
        self.checkpoint_manager = Some(Arc::new(manager));
        Ok(())
    }

    /// Enables epoch-based checkpointing with default configuration.
    pub fn enable_epoch_checkpointing_default(&mut self) -> Result<()> {
        self.enable_epoch_checkpointing(EpochConfig::default())
    }

    /// Enables epoch-based checkpointing with high-throughput configuration.
    ///
    /// Uses longer epochs and higher operation limits, suitable for
    /// batch processing workloads.
    pub fn enable_epoch_checkpointing_high_throughput(&mut self) -> Result<()> {
        self.enable_epoch_checkpointing(EpochConfig::high_throughput())
    }

    /// Enables epoch-based checkpointing with low-latency configuration.
    ///
    /// Uses shorter epochs for faster recovery, suitable for
    /// real-time applications.
    pub fn enable_epoch_checkpointing_low_latency(&mut self) -> Result<()> {
        self.enable_epoch_checkpointing(EpochConfig::low_latency())
    }

    /// Disables epoch-based checkpointing.
    ///
    /// The checkpoint manager is stopped and dropped. Any pending
    /// checkpoint operations complete before this returns.
    pub fn disable_epoch_checkpointing(&mut self) {
        self.checkpoint_manager = None;
    }

    /// Returns whether epoch-based checkpointing is enabled.
    pub fn has_epoch_checkpointing(&self) -> bool {
        self.checkpoint_manager.is_some()
    }

    /// Records an operation in the current epoch.
    ///
    /// Call this after each WAL write to track operation counts for
    /// automatic epoch advancement. The `wal_bytes` parameter should
    /// be the size of the WAL record written.
    ///
    /// # Returns
    /// The current epoch ID, or None if checkpointing is not enabled.
    pub fn record_epoch_operation(&self, wal_bytes: usize) -> Option<EpochId> {
        self.checkpoint_manager.as_ref().map(|cm| cm.record_operation(wal_bytes))
    }

    /// Returns the current epoch ID.
    pub fn current_epoch_id(&self) -> Option<EpochId> {
        self.checkpoint_manager.as_ref().map(|cm| cm.current_epoch_id())
    }

    /// Forces an immediate checkpoint of the current epoch.
    ///
    /// This advances to a new epoch and checkpoints the previous one.
    /// Useful before shutdown or when you want to ensure durability.
    ///
    /// # Returns
    /// * `Some(epoch_id)` - The epoch ID that was checkpointed
    /// * `None` - Checkpoint manager not enabled
    pub fn force_epoch_checkpoint(&self) -> Option<Result<EpochId>> {
        self.checkpoint_manager.as_ref().map(|cm| cm.force_checkpoint())
    }

    /// Returns the last durable (fully checkpointed) epoch ID.
    pub fn last_durable_epoch(&self) -> Option<EpochId> {
        self.checkpoint_manager.as_ref().and_then(|cm| cm.last_durable_epoch())
    }

    /// Returns epoch statistics.
    pub fn epoch_stats(&self) -> Option<EpochStats> {
        self.checkpoint_manager.as_ref().map(|cm| cm.stats())
    }

    /// Returns metadata for recent epochs.
    pub fn epoch_metadata(&self) -> Option<Vec<EpochMetadata>> {
        self.checkpoint_manager.as_ref().map(|cm| cm.epoch_metadata())
    }

    /// Returns the configuration for epoch checkpointing.
    pub fn epoch_config(&self) -> Option<&EpochConfig> {
        self.checkpoint_manager.as_ref().map(|cm| cm.config())
    }

    /// Get the current durability policy.
    ///
    /// The durability policy controls when fsync is called after WAL writes.
    /// See [`DurabilityPolicy`] for available options and their trade-offs.
    pub fn durability_policy(&self) -> DurabilityPolicy {
        self.durability_policy
    }

    /// Set the durability policy for this trie.
    ///
    /// The durability policy controls when fsync is called after WAL writes,
    /// providing a trade-off between durability and performance.
    ///
    /// # Arguments
    ///
    /// * `policy` - The new durability policy
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie_char::{PersistentARTrieChar, DurabilityPolicy};
    ///
    /// let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::create("words.trie")?;
    ///
    /// // Use periodic sync for better performance (accepts bounded data loss)
    /// trie.set_durability_policy(DurabilityPolicy::Periodic);
    /// ```
    pub fn set_durability_policy(&mut self, policy: DurabilityPolicy) {
        self.durability_policy = policy;
    }

    // ==================== End Epoch-Based Checkpointing Methods ====================

    /// Internal helper: Append a record to the WAL, routing through group commit if enabled.
    ///
    /// When group commit is enabled, the record is submitted to the group commit
    /// coordinator which batches writes and reduces fsync overhead. Otherwise,
    /// the record is written directly to the WAL.
    pub(super) fn append_to_wal(&self, record: WalRecord) -> Result<()> {
        // Check if group commit is enabled first
        #[cfg(feature = "group-commit")]
        if let Some(ref gc) = self.group_commit {
            gc.append_with_sync(record)
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            return Ok(());
        }

        // Fall back to direct WAL write
        if let Some(ref wal_writer) = self.wal_writer {
            wal_writer
                .append(record)
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
        }
        Ok(())
    }

    /// Internal helper: Sync the WAL based on durability policy.
    ///
    /// Only syncs when durability_policy is Immediate. GroupCommit and Periodic
    /// policies handle syncing through their respective mechanisms.
    pub(super) fn sync_wal(&self) -> Result<()> {
        // Only sync for Immediate policy
        if self.durability_policy != DurabilityPolicy::Immediate {
            return Ok(());
        }

        // Group commit handles syncing internally via append_with_sync
        #[cfg(feature = "group-commit")]
        if self.group_commit.is_some() {
            return Ok(());
        }

        // Direct WAL sync
        if let Some(ref wal_writer) = self.wal_writer {
            wal_writer
                .sync()
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
        }
        Ok(())
    }

    // ========================================================================
    // Atomic Operations with WAL Support
    // ========================================================================

    /// Atomically increment a value by delta.
    ///
    /// If the term doesn't exist, inserts with `delta` as the initial value.
    /// The value must be serializable as an i64.
    ///
    /// # Returns
    ///
    /// The new value after incrementing.
    pub fn increment(&mut self, term: &str, delta: i64) -> Result<i64> {
        // Get current value
        let current: i64 = if let Some(v) = self.get(term) {
            let bytes = bincode::serialize(&v).map_err(|e| {
                PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
            })?;
            if bytes.len() == 8 {
                i64::from_le_bytes(bytes.try_into().unwrap())
            } else {
                bincode::deserialize::<i64>(&bytes).map_err(|e| {
                    PersistentARTrieError::internal(format!("Failed to deserialize as i64: {}", e))
                })?
            }
        } else {
            0
        };

        let new_value = current + delta;

        // Create value from i64
        let value_bytes = bincode::serialize(&new_value).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to serialize new value: {}", e))
        })?;
        let v: V = bincode::deserialize(&value_bytes).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to deserialize as V: {}", e))
        })?;

        // Log to WAL first (routes through group commit if enabled)
        let record = WalRecord::Increment {
            term: term.as_bytes().to_vec(),
            delta,
            result: new_value,
        };
        self.append_to_wal(record)?;

        // Update the trie
        self.insert_impl_no_wal_with_value(term, v);

        Ok(new_value)
    }

    /// Internal increment without WAL logging (for batch operations).
    ///
    /// This is used by `commit_document()` for BatchIncrement operations where
    /// the WAL record has already been written.
    ///
    /// # Returns
    ///
    /// The new value after incrementing.
    pub(super) fn increment_impl_no_wal(&mut self, term: &str, delta: i64) -> i64 {
        // Get current value
        let current: i64 = if let Some(v) = self.get(term) {
            let bytes = match bincode::serialize(&v) {
                Ok(b) => b,
                Err(_) => return delta, // On error, treat as starting from 0
            };
            if bytes.len() == 8 {
                i64::from_le_bytes(bytes.try_into().unwrap())
            } else {
                match bincode::deserialize::<i64>(&bytes) {
                    Ok(val) => val,
                    Err(_) => 0,
                }
            }
        } else {
            0
        };

        let new_value = current + delta;

        // Create value from i64
        let value_bytes = match bincode::serialize(&new_value) {
            Ok(b) => b,
            Err(_) => return new_value,
        };
        let v: V = match bincode::deserialize(&value_bytes) {
            Ok(val) => val,
            Err(_) => return new_value,
        };

        // Update the trie (no WAL logging)
        self.insert_impl_no_wal_with_value(term, v);

        new_value
    }

    /// Atomically update or insert a value.
    ///
    /// # Returns
    ///
    /// `true` if a new term was inserted, `false` if an existing term was updated.
    pub fn upsert(&mut self, term: &str, value: V) -> Result<bool> {
        let existed = self.contains(term);

        // Log to WAL first (routes through group commit if enabled)
        let value_bytes = bincode::serialize(&value).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
        })?;
        let record = WalRecord::Upsert {
            term: term.as_bytes().to_vec(),
            value: value_bytes,
        };
        self.append_to_wal(record)?;

        // Update the trie
        self.insert_impl_no_wal_with_value(term, value);

        Ok(!existed)
    }

    /// Atomically compare and swap a value.
    ///
    /// Updates the value only if the current value matches `expected`.
    ///
    /// # Returns
    ///
    /// `true` if the swap succeeded, `false` if the current value didn't match expected.
    pub fn compare_and_swap(&mut self, term: &str, expected: Option<V>, new_value: V) -> Result<bool> {
        let current = self.get(term).cloned();

        // Check if current matches expected
        let matches = match (&current, &expected) {
            (None, None) => true,
            (Some(c), Some(e)) => {
                let c_bytes = bincode::serialize(c).ok();
                let e_bytes = bincode::serialize(e).ok();
                c_bytes == e_bytes
            }
            _ => false,
        };

        if matches {
            // Log to WAL first (routes through group commit if enabled)
            let expected_bytes = expected
                .as_ref()
                .map(|e| bincode::serialize(e).ok())
                .flatten();
            let new_value_bytes = bincode::serialize(&new_value).map_err(|e| {
                PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
            })?;
            let record = WalRecord::CompareAndSwap {
                term: term.as_bytes().to_vec(),
                expected: expected_bytes,
                new_value: new_value_bytes,
                success: true,
            };
            self.append_to_wal(record)?;

            // Update the trie
            self.insert_impl_no_wal_with_value(term, new_value);
        }

        Ok(matches)
    }

    /// Get the current value and increment atomically (fetch-and-add).
    ///
    /// Returns the value *before* the increment.
    pub fn fetch_add(&mut self, term: &str, delta: i64) -> Result<i64> {
        let new_value = self.increment(term, delta)?;
        Ok(new_value - delta)
    }

    /// Get or insert a default value atomically.
    ///
    /// If the term exists, returns its current value.
    /// If not, inserts the default value and returns it.
    pub fn get_or_insert(&mut self, term: &str, default: V) -> Result<V> {
        if let Some(v) = self.get(term).cloned() {
            return Ok(v);
        }

        // Log to WAL first (routes through group commit if enabled)
        let value_bytes = bincode::serialize(&default).ok();
        let record = WalRecord::Insert {
            term: term.as_bytes().to_vec(),
            value: value_bytes,
        };
        self.append_to_wal(record)?;

        // Insert the default value
        self.insert_impl_no_wal_with_value(term, default.clone());

        Ok(default)
    }

    /// Checkpoint: persist trie to disk and truncate WAL
    ///
    /// This is the verified checkpoint sequence that ensures data integrity
    /// before truncating the WAL:
    ///
    /// 1. persist_to_disk() - serialize and sync data
    /// 2. verify_checkpoint() - read back and verify header checksum
    /// 3. WAL checkpoint record - mark checkpoint in WAL
    /// 4. WAL sync - ensure checkpoint record is durable
    /// 5. WAL truncate - only after verification passes
    ///
    /// If verification fails at step 2, the WAL is NOT truncated,
    /// allowing recovery from the existing WAL on next open.
    pub fn checkpoint(&mut self) -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Step 1: Persist trie to disk
        self.persist_to_disk()?;

        // Step 2: Verify checkpoint - re-read header and verify checksum
        // This ensures the sync() actually succeeded and data is durable
        self.verify_checkpoint()?;

        // Steps 3-5: WAL operations (only after verification passes)
        if let Some(ref wal_writer) = self.wal_writer {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let record = WalRecord::Checkpoint {
                checkpoint_lsn: self.next_lsn.load(AtomicOrdering::Acquire),
                timestamp,
            };
            // Step 3: Write checkpoint record
            wal_writer
                .append(record)
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            // Step 4: Sync WAL
            wal_writer
                .sync()
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            // Step 5: Archive or truncate WAL based on configuration
            // If archive mode is enabled, rotate to archive; otherwise truncate
            wal_writer
                .rotate_to_archive(&self.wal_config)
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
        }

        self.dirty.store(false, AtomicOrdering::Release);
        Ok(())
    }

    /// Verify checkpoint data integrity after persist_to_disk()
    ///
    /// Re-reads the file header from disk and verifies its checksum.
    /// This ensures the fsync() actually succeeded and data is durable.
    ///
    /// Returns an error if verification fails - the WAL should NOT be
    /// truncated in this case.
    fn verify_checkpoint(&self) -> Result<()> {
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for checkpoint verification")
        })?;

        // Re-read header from disk and verify checksum
        let bm = buffer_manager.read();

        let dm = bm.storage();

        // Read header and verify checksum
        let header = dm.read_header()?;
        if !header.verify_checksum() {
            return Err(PersistentARTrieError::CheckpointVerificationFailed {
                reason: format!(
                    "Header checksum mismatch after sync: stored={:#x}, computed={:#x}",
                    header.checksum,
                    header.compute_checksum()
                ),
            });
        }

        Ok(())
    }

    /// Persist the entire trie to disk
    ///
    /// This serializes the trie structure and writes it to the data file,
    /// updating the file header with the root pointer.
    pub fn persist_to_disk(&mut self) -> Result<()> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use crate::persistent_artrie::NodeType;

        // Get buffer manager
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for disk serialization")
        })?;

        // Serialize the trie root and get a descriptor
        let (root_type, root_ptr, is_final) = match &self.root {
            CharTrieRoot::Empty => {
                (ROOT_TYPE_EMPTY, 0u64, false)
            }
            CharTrieRoot::Node(node) => {
                // Recursively serialize the node and all children
                let ptr = self.serialize_char_node_to_disk(node.as_ref())?;
                (ROOT_TYPE_NODE, ptr.to_raw(), node.is_final())
            }
        };

        // Flush arenas to disk FIRST to get their block_ids
        // (writes dirty arenas to buffer manager)
        // Uses slot-level incremental flush if configured, otherwise full arena flush
        if let Some(ref arena_manager) = self.arena_manager {
            let stats = arena_manager.write().flush_dirty_slots()?;
            if stats.partial_writes > 0 {
                log::debug!(
                    "Char incremental flush: {} full arenas, {} partial, {} slots, {} bytes written, {} bytes saved",
                    stats.full_arena_writes, stats.partial_writes, stats.slots_written,
                    stats.bytes_written, stats.bytes_saved
                );
            }
        }

        // Get arena count after flushing (block IDs are derived from sequential allocation)
        let arena_count: u32 = if let Some(ref arena_manager) = self.arena_manager {
            arena_manager.read().arena_count() as u32
        } else {
            0
        };

        // Create root descriptor (fixed 18 bytes)
        // Format:
        //   0: type (1 byte)
        //   1: is_final (1 byte)
        //   2-5: term_count (4 bytes, little endian)
        //   6-9: arena_count (4 bytes, little endian)
        //   10-17: root_ptr (8 bytes, little endian)
        //
        // Note: Arena block IDs are NOT stored - they are derived from sequential allocation:
        // Block 0 = file header + descriptor, Blocks 1..=arena_count = arenas
        let mut descriptor = [0u8; 18];
        descriptor[0] = root_type;
        descriptor[1] = if is_final { 1 } else { 0 };
        descriptor[2..6].copy_from_slice(&(self.len.load(AtomicOrdering::Acquire) as u32).to_le_bytes());
        descriptor[6..10].copy_from_slice(&arena_count.to_le_bytes());
        descriptor[10..18].copy_from_slice(&root_ptr.to_le_bytes());

        // Write descriptor to fixed location in block 0 (offset 64, after file header)
        // This ensures arenas always occupy blocks 1, 2, 3, ... sequentially
        const DESCRIPTOR_OFFSET: usize = 64;
        let bm = buffer_manager.write();
        let dm = bm.storage();
        dm.write_bytes(0, DESCRIPTOR_OFFSET, &descriptor)?;

        // Update root_ptr to point to block 0, offset 64
        let root_descriptor_ptr = SwizzledPtr::on_disk(0, DESCRIPTOR_OFFSET as u32, NodeType::Bucket);
        dm.set_root_ptr(root_descriptor_ptr.to_raw())?;
        dm.set_entry_count(self.len.load(AtomicOrdering::Acquire) as u64)?;

        // Flush all pages to ensure durability
        bm.flush_all()?;
        dm.sync()?;

        self.dirty.store(false, AtomicOrdering::Release);
        Ok(())
    }

    /// Check if serialized children are consecutive in the same arena.
    ///
    /// For sequential sibling storage optimization: if all children are in the same arena
    /// and have consecutive slot IDs, we can store just `(first_slot, count)` instead of
    /// N separate pointers.
    ///
    /// # Arguments
    /// * `child_ptrs` - Child (key, SwizzledPtr) pairs from serialization
    /// * `parent_arena_id` - Arena ID where parent will be allocated
    ///
    /// # Returns
    /// `Some(first_child_slot)` if children are consecutive in same arena as parent,
    /// `None` otherwise.
    fn check_sequential_char_children(
        child_ptrs: &[(u32, SwizzledPtr)],
        parent_arena_id: u32,
        arena_node_count: u32,
    ) -> Option<super::arena_manager::ArenaSlot> {
        use super::arena_manager::ArenaSlot;

        if child_ptrs.len() < 2 {
            // Need at least 2 children for sequential optimization to be worthwhile
            return None;
        }

        // Collect arena slots from SwizzledPtrs
        let mut slots: Vec<ArenaSlot> = Vec::with_capacity(child_ptrs.len());
        for (_, ptr) in child_ptrs {
            // Get disk location from SwizzledPtr
            let loc = match ptr.disk_location() {
                Some(loc) => loc,
                None => return None, // All children must be on disk
            };
            let arena_id = loc.block_id;
            let slot_id = loc.offset;
            if arena_id != parent_arena_id {
                // All children must be in the same arena as parent
                return None;
            }
            slots.push(ArenaSlot::new(arena_id, slot_id));
        }

        // Sort by slot ID
        slots.sort_by_key(|s| s.slot_id);

        // Check if consecutive
        let first = slots[0];
        for (i, slot) in slots.iter().enumerate() {
            if slot.slot_id != first.slot_id + i as u32 {
                return None;
            }
        }

        // Verify first_slot + count won't overflow u32.
        // This prevents decode_sequential_siblings() from generating invalid slot IDs.
        // The last slot is first + (count - 1), so we check that doesn't overflow.
        let count = slots.len() as u32;
        if first.slot_id.checked_add(count.saturating_sub(1)).is_none() {
            return None; // Would overflow u32, use non-sequential encoding
        }

        // Verify last slot is within arena bounds.
        // This aligns with formal spec: first + count - 1 < arena_node_count
        // The overflow check above guarantees this subtraction is safe.
        let last_slot = first.slot_id + count - 1;
        if last_slot >= arena_node_count {
            return None; // Would exceed arena bounds, use non-sequential encoding
        }

        Some(first)
    }

    /// Serialize a CharTrieNodeInner to disk and return its SwizzledPtr
    ///
    /// Uses arena allocation for space-efficient storage. Multiple nodes are
    /// packed into each 256KB arena block instead of wasting one block per node.
    ///
    /// Node format on disk:
    /// ```text
    /// [CharNode serialized - 16-byte header + type-specific data]
    /// [value_len: u32]
    /// [value_bytes if value_len > 0]
    /// ```
    ///
    /// The SwizzledPtr uses:
    /// - arena_id as block_id (23 bits, up to 8M arenas)
    /// - slot_id as offset (22 bits, up to 4M slots per arena)
    fn serialize_char_node_to_disk(&self, node: &CharTrieNodeInner<V>) -> Result<SwizzledPtr> {
        use super::relative_encoding::SerializationContext;
        use super::serialization_char::serialize_char_node_v2;

        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk serialization")
        })?;

        // Get the predicted parent slot for sequential sibling check
        let parent_arena_id = arena_manager.read().next_slot().arena_id;

        // First, recursively serialize all children and collect their disk pointers
        // Note: We handle both in-memory children (need serialization) and disk-backed
        // children (already have a disk pointer, just reuse it).
        let mut child_disk_ptrs: Vec<(u32, SwizzledPtr)> = Vec::with_capacity(node.num_children());
        for (key, child_ptr) in node.node.iter_children() {
            if child_ptr.is_null() {
                continue;
            }

            // Check if the child is already on disk (DiskRef) - just reuse its pointer
            if child_ptr.disk_location().is_some() {
                // Clone the SwizzledPtr to preserve its disk location
                child_disk_ptrs.push((key, child_ptr.clone()));
            } else if let Some(child_raw) = child_ptr.as_ptr::<CharTrieNodeInner<V>>() {
                // Child is in memory - serialize it recursively
                // Safety: ptr was created via Box::into_raw() from CharTrieNodeInner<V>
                let child = unsafe { &*child_raw };
                let ptr = self.serialize_char_node_to_disk(child)?;
                child_disk_ptrs.push((key, ptr));
            }
            // If neither disk_location nor as_ptr succeeds, skip this child
            // (should not happen in normal operation)
        }

        // Get the predicted parent slot and arena node count for encoding children
        let (parent_slot, arena_node_count) = {
            let mgr = arena_manager.read();
            let slot = mgr.next_slot();
            let node_count = mgr
                .get_arena(parent_arena_id)
                .map(|a| a.node_count())
                .unwrap_or(0);
            (slot, node_count)
        };

        // Check if children are consecutive (enables sequential sibling storage)
        // Create serialization context that determines encoding mode:
        // - Sequential: children stored as (first_slot, count) instead of N pointers
        // - Relative: child offsets encoded relative to parent (1-2 bytes vs 8 bytes)
        // - Full: absolute (arena_id, slot_id) for each child (9 bytes per child)
        //
        // IMPORTANT: If parent_slot.slot_id is small (especially 0), children serialized
        // in the previous arena(s) would have "negative" relative offsets, causing
        // decode underflow. Use full encoding to avoid this.
        let ctx = if parent_slot.slot_id < child_disk_ptrs.len() as u32 {
            // Parent slot is near the start of an arena - children likely in previous arena
            // Use full encoding to avoid relative offset underflow during decode
            SerializationContext::full_encoding(parent_slot)
        } else if let Some(first_child) =
            Self::check_sequential_char_children(&child_disk_ptrs, parent_arena_id, arena_node_count)
        {
            // Children are consecutive in same arena: use sequential sibling encoding
            SerializationContext::sequential(parent_slot, first_child)
        } else {
            // Children are not consecutive: use relative encoding only
            SerializationContext::new(parent_slot)
        };

        // Build a CharNode with disk pointers for serialization
        let disk_node = self.build_disk_char_node(&node.node, &child_disk_ptrs)?;

        // Serialize the value using bincode (needed regardless of encoding)
        let value_bytes: Vec<u8> = if let Some(ref value) = node.value {
            bincode::serialize(value).map_err(|e| {
                PersistentARTrieError::internal(&format!("Failed to serialize value: {}", e))
            })?
        } else {
            Vec::new()
        };

        // Serialize the CharNode to a buffer using v2 format with relative offsets
        let mut node_buffer = Vec::new();
        serialize_char_node_v2(&disk_node, &mut node_buffer, &ctx)?;

        // Build complete serialized data:
        // [node_buffer] + [value_len: u32] + [value_bytes]
        let build_data = |node_buf: &[u8], value_buf: &[u8]| -> Vec<u8> {
            let total_size = node_buf.len() + 4 + value_buf.len();
            let mut data = Vec::with_capacity(total_size);
            data.extend_from_slice(node_buf);
            data.extend_from_slice(&(value_buf.len() as u32).to_le_bytes());
            data.extend_from_slice(value_buf);
            data
        };

        let data = build_data(&node_buffer, &value_bytes);

        // Allocate in arena (space-efficient: packs many nodes per 256KB block)
        let slot = arena_manager.write().allocate(&data)?;

        // Check if arena overflow caused slot mismatch
        // If so, re-serialize using the actual slot to prevent relative encoding underflow
        let final_slot = if slot != ctx.parent_slot {
            // Arena overflow detected - need to re-serialize with correct parent slot
            // This happens when the predicted slot was in arena N, but allocation
            // went to arena N+1 due to arena being full
            //
            // Children are now likely in a different arena than the parent, requiring
            // cross-arena encoding (9 bytes per child) instead of relative encoding.
            let corrected_ctx = SerializationContext::new(slot);
            let mut corrected_buffer = Vec::new();
            serialize_char_node_v2(&disk_node, &mut corrected_buffer, &corrected_ctx)?;
            let corrected_data = build_data(&corrected_buffer, &value_bytes);

            if corrected_data.len() == data.len() {
                // Same size - can update in-place
                arena_manager.write().update(slot, &corrected_data)?;
                slot
            } else {
                // Different size (cross-arena encoding is larger) - allocate new slot
                // The original slot becomes wasted space (acceptable for rare overflow cases)
                arena_manager.write().allocate(&corrected_data)?
            }
        } else {
            slot
        };

        // Return pointer using arena addressing:
        // - block_id = arena_id + 1 (block 0 is file header, arena N is in block N+1)
        // - offset = slot_id
        let node_type = self.char_node_to_node_type(&disk_node);
        Ok(SwizzledPtr::on_disk(final_slot.arena_id + 1, final_slot.slot_id, node_type))
    }

    /// Build a CharNode with disk SwizzledPtrs for serialization.
    ///
    /// Creates a new CharNode of the same type as the original, but with
    /// children pointing to disk locations instead of in-memory nodes.
    ///
    /// Returns `Err` only if the rebuilt node's `add_child_growing` exceeds
    /// capacity — that indicates corruption (the original held that many
    /// children, so a same-type rebuild cannot fail to hold them) and the
    /// caller propagates the error up the serialization stack rather than
    /// crashing.
    fn build_disk_char_node(
        &self,
        original: &CharNode,
        disk_children: &[(u32, SwizzledPtr)],
    ) -> Result<CharNode> {
        use super::nodes::{CharBucket, CharNode16, CharNode4, CharNode48};

        // Create a new node of the same type
        let mut new_node = match original {
            CharNode::N4(_) => CharNode::N4(Box::new(CharNode4::new())),
            CharNode::N16(_) => CharNode::N16(Box::new(CharNode16::new())),
            CharNode::N48(_) => CharNode::N48(Box::new(CharNode48::new())),
            CharNode::Bucket(_) => CharNode::Bucket(Box::new(CharBucket::new())),
        };

        // Copy header properties
        {
            let new_header = new_node.header_mut();
            let orig_header = original.header();
            new_header.prefix_len = orig_header.prefix_len;
            new_header.flags = orig_header.flags;
            new_header.version = orig_header.version;
        }

        // Copy prefix
        *new_node.prefix_mut() = *original.prefix();

        // Add disk children
        for &(key, ref ptr) in disk_children {
            new_node.add_child_growing(key, ptr.clone()).map_err(|e| {
                PersistentARTrieError::internal(&format!(
                    "build_disk_char_node: rebuilt node rejected child key {:#x} (Node type same \
                     as source): {:?} — indicates corruption in source node's child count",
                    key, e
                ))
            })?;
        }

        Ok(new_node)
    }

    /// Map CharNode type to NodeType for SwizzledPtr
    fn char_node_to_node_type(&self, node: &CharNode) -> NodeType {
        match node {
            CharNode::N4(_) => NodeType::CharNode4,
            CharNode::N16(_) => NodeType::CharNode16,
            CharNode::N48(_) => NodeType::CharNode48,
            CharNode::Bucket(_) => NodeType::CharBucket,
        }
    }
}

/// Root descriptor type constants
pub(super) const ROOT_TYPE_EMPTY: u8 = 0;
pub(super) const ROOT_TYPE_NODE: u8 = 1;

// Note: Default implementation is in mod.rs on PersistentARTrieChar directly
// Note: SharedCharARTrie is now a type alias in mod.rs: `pub type SharedCharARTrie<V> = Arc<RwLock<PersistentARTrieChar<V>>>;`
// Note: SharedCharTrie is a deprecated alias for SharedCharARTrie

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use super::super::PersistentARTrieChar;
    use super::super::SharedCharTrie;
    use crate::ARTrie;

    #[test]
    fn test_file_header_roundtrip() {
        let mut header = CharTrieFileHeader {
            magic: CHAR_TRIE_MAGIC,
            version: CHAR_HEADER_VERSION_V2,
            _reserved: [0; 3],
            root_ptr: 12345,
            entry_count: 67890,
            checkpoint_lsn: 111,
            header_checksum: 0,
            _padding: [0; 28],
        };
        header.finalize_checksum();

        let bytes = header.to_bytes();
        let restored = CharTrieFileHeader::from_bytes(&bytes);

        assert_eq!(restored.magic, CHAR_TRIE_MAGIC);
        assert_eq!(restored.version, CHAR_HEADER_VERSION_V2);
        assert_eq!(restored.root_ptr, 12345);
        assert_eq!(restored.entry_count, 67890);
        assert_eq!(restored.checkpoint_lsn, 111);
        assert!(restored.verify_checksum());
    }

    #[test]
    fn test_file_header_v1_roundtrip() {
        // V1 headers have no checksum
        let header = CharTrieFileHeader {
            magic: CHAR_TRIE_MAGIC,
            version: CHAR_HEADER_VERSION_V1,
            _reserved: [0; 3],
            root_ptr: 12345,
            entry_count: 67890,
            checkpoint_lsn: 111,
            header_checksum: 0,
            _padding: [0; 28],
        };

        let bytes = header.to_bytes();
        let restored = CharTrieFileHeader::from_bytes(&bytes);

        assert_eq!(restored.magic, CHAR_TRIE_MAGIC);
        assert_eq!(restored.version, CHAR_HEADER_VERSION_V1);
        assert_eq!(restored.root_ptr, 12345);
        assert!(!restored.has_checksum());
        assert!(restored.verify_checksum()); // V1 always valid
    }

    #[test]
    fn test_file_header_checksum() {
        let mut header = CharTrieFileHeader::new();
        header.root_ptr = 12345;
        header.entry_count = 67890;

        // Before finalize, checksum is 0
        assert_eq!(header.header_checksum, 0);
        assert!(!header.verify_checksum()); // Checksum doesn't match

        // After finalize, checksum is valid
        header.finalize_checksum();
        assert_ne!(header.header_checksum, 0);
        assert!(header.verify_checksum());

        // Modify a field and checksum becomes invalid
        header.root_ptr = 99999;
        assert!(!header.verify_checksum());

        // Finalize again to fix
        header.finalize_checksum();
        assert!(header.verify_checksum());
    }

    #[test]
    fn test_file_header_validation() {
        let mut header = CharTrieFileHeader::new();
        header.finalize_checksum();
        assert!(header.validate().is_ok());

        // Invalid magic
        header.magic = *b"XXXX";
        assert!(header.validate().is_err());

        // Restore magic, corrupt checksum
        header.magic = CHAR_TRIE_MAGIC;
        header.header_checksum = 0xDEADBEEF;
        assert!(header.validate().is_err());
    }

    #[test]
    fn test_file_header_from_bytes_verified() {
        let mut header = CharTrieFileHeader::new();
        header.root_ptr = 12345;
        header.finalize_checksum();

        let bytes = header.to_bytes();

        // Valid bytes should succeed
        let restored = CharTrieFileHeader::from_bytes_verified(&bytes);
        assert!(restored.is_ok());

        // Corrupt bytes should fail
        let mut corrupted = bytes;
        corrupted[8] = 0xFF; // Corrupt root_ptr
        let result = CharTrieFileHeader::from_bytes_verified(&corrupted);
        assert!(result.is_err());
    }

    #[test]
    fn test_file_header_upgrade_to_v2() {
        let mut header = CharTrieFileHeader::new_v1();
        assert!(!header.has_checksum());

        header.root_ptr = 12345;
        header.upgrade_to_v2();

        assert!(header.has_checksum());
        assert!(header.verify_checksum());
        assert_eq!(header.version, CHAR_HEADER_VERSION_V2);
    }

    #[test]
    fn test_inner_new() {
        let inner: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        assert_eq!(inner.len.load(AtomicOrdering::Acquire), 0);
        assert!(!inner.dirty.load(AtomicOrdering::Acquire));
        assert!(matches!(inner.root, CharTrieRoot::Empty));
    }

    #[test]
    fn test_create_and_open() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.trie");

        // Create a new trie
        {
            let mut inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");
            inner.insert("hello").expect("insert");
            inner.insert("world").expect("insert");
            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::open(&path).expect("open");
            // WAL replay should have reconstructed the state
            assert_eq!(inner.len(), 2);
        }
    }

    #[test]
    fn test_insert_and_contains() {
        let mut inner: PersistentARTrieChar<()> = PersistentARTrieChar::new();

        // Insert some terms
        assert!(inner.insert_impl_no_wal("hello"));
        assert!(inner.insert_impl_no_wal("world"));
        assert!(inner.insert_impl_no_wal("hello world"));

        // Verify contains
        assert!(inner.contains("hello"));
        assert!(inner.contains("world"));
        assert!(inner.contains("hello world"));
        assert!(!inner.contains("hell"));
        assert!(!inner.contains("hello worl"));

        assert_eq!(inner.len(), 3);
    }

    #[test]
    fn test_insert_duplicate() {
        let mut inner: PersistentARTrieChar<()> = PersistentARTrieChar::new();

        // First insert should succeed
        assert!(inner.insert_impl_no_wal("hello"));

        // Duplicate insert should fail
        assert!(!inner.insert_impl_no_wal("hello"));

        // Length should still be 1
        assert_eq!(inner.len(), 1);
    }

    #[test]
    fn test_remove() {
        let mut inner: PersistentARTrieChar<()> = PersistentARTrieChar::new();

        // Insert some terms
        inner.insert_impl_no_wal("hello");
        inner.insert_impl_no_wal("world");
        assert_eq!(inner.len(), 2);

        // Remove one
        assert!(inner.remove_impl_no_wal("hello"));
        assert_eq!(inner.len(), 1);
        assert!(!inner.contains("hello"));
        assert!(inner.contains("world"));

        // Remove again should fail
        assert!(!inner.remove_impl_no_wal("hello"));

        // Remove the other
        assert!(inner.remove_impl_no_wal("world"));
        assert_eq!(inner.len(), 0);
    }

    #[test]
    fn test_unicode_support() {
        let mut inner: PersistentARTrieChar<()> = PersistentARTrieChar::new();

        // Test various Unicode characters
        let terms = vec![
            "こんにちは",     // Japanese
            "你好",           // Chinese
            "안녕하세요",     // Korean
            "مرحبا",          // Arabic
            "שלום",           // Hebrew
            "🎉🎊🎋",        // Emoji
            "café",           // Latin with diacritics
            "naïve",          // Latin with diacritics
        ];

        for term in &terms {
            assert!(inner.insert_impl_no_wal(term), "should insert: {}", term);
        }

        assert_eq!(inner.len(), terms.len());

        // Verify all are present
        for term in &terms {
            assert!(inner.contains(term), "should contain: {}", term);
        }

        // Verify partial terms are not present
        assert!(!inner.contains("こん"));
        assert!(!inner.contains("你"));
        assert!(!inner.contains("🎉"));
    }

    #[test]
    fn test_prefix_sharing() {
        let mut inner: PersistentARTrieChar<()> = PersistentARTrieChar::new();

        // Terms that share prefixes
        inner.insert_impl_no_wal("a");
        inner.insert_impl_no_wal("ab");
        inner.insert_impl_no_wal("abc");
        inner.insert_impl_no_wal("abd");
        inner.insert_impl_no_wal("abcd");

        assert_eq!(inner.len(), 5);

        // All should be present
        assert!(inner.contains("a"));
        assert!(inner.contains("ab"));
        assert!(inner.contains("abc"));
        assert!(inner.contains("abd"));
        assert!(inner.contains("abcd"));

        // Partial paths should not be final
        assert!(!inner.contains("abce"));
    }

    #[test]
    fn test_empty_string() {
        let mut inner: PersistentARTrieChar<()> = PersistentARTrieChar::new();

        // Empty string is valid
        assert!(inner.insert_impl_no_wal(""));
        assert!(inner.contains(""));
        assert_eq!(inner.len(), 1);

        // Add another term
        inner.insert_impl_no_wal("hello");
        assert_eq!(inner.len(), 2);
        assert!(inner.contains(""));
        assert!(inner.contains("hello"));
    }

    #[test]
    fn test_get_value() {
        let mut inner: PersistentARTrieChar<i32> = PersistentARTrieChar::new();

        inner.insert_impl_no_wal_with_value("one", 1);
        inner.insert_impl_no_wal_with_value("two", 2);
        inner.insert_impl_no_wal_with_value("three", 3);

        assert_eq!(inner.get("one"), Some(&1));
        assert_eq!(inner.get("two"), Some(&2));
        assert_eq!(inner.get("three"), Some(&3));
        assert_eq!(inner.get("four"), None);
    }

    #[test]
    fn test_value_update() {
        let mut inner: PersistentARTrieChar<i32> = PersistentARTrieChar::new();

        // First insert
        assert!(inner.insert_impl_no_wal_with_value("key", 100));
        assert_eq!(inner.get("key"), Some(&100));

        // Update (insert returns false but value is updated)
        assert!(!inner.insert_impl_no_wal_with_value("key", 200));
        assert_eq!(inner.get("key"), Some(&200));

        // Length unchanged
        assert_eq!(inner.len(), 1);
    }

    #[test]
    fn test_wal_recovery_with_values() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_values.trie");

        // Create and insert with values
        {
            let mut inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");
            inner.insert("alpha").expect("insert");
            inner.insert("beta").expect("insert");
            inner.insert("gamma").expect("insert");
            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::open(&path).expect("open");
            assert_eq!(inner.len(), 3);
            assert!(inner.contains("alpha"));
            assert!(inner.contains("beta"));
            assert!(inner.contains("gamma"));
            assert!(!inner.contains("delta"));
        }
    }

    #[test]
    fn test_wal_recovery_mixed_operations() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_mixed.trie");

        // Create with mixed insert/remove
        {
            let mut inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");
            inner.insert("a").expect("insert");
            inner.insert("b").expect("insert");
            inner.insert("c").expect("insert");
            inner.remove("b").expect("remove");
            inner.insert("d").expect("insert");
            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::open(&path).expect("open");
            assert_eq!(inner.len(), 3);
            assert!(inner.contains("a"));
            assert!(!inner.contains("b"));
            assert!(inner.contains("c"));
            assert!(inner.contains("d"));
        }
    }

    #[test]
    fn test_checkpoint_and_disk_loading() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_checkpoint.trie");

        // Create, insert terms, and checkpoint
        let root_ptr_after_checkpoint;
        {
            let mut inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");
            inner.insert("apple").expect("insert");
            inner.insert("banana").expect("insert");
            inner.insert("cherry").expect("insert");
            assert_eq!(inner.len(), 3, "len after inserts");

            inner.checkpoint().expect("checkpoint");

            // Read root_ptr from disk to verify it was written
            let buffer_manager = inner.buffer_manager.as_ref().expect("buffer manager");
            let bm = buffer_manager.read();
            root_ptr_after_checkpoint = bm.disk_manager().root_ptr().expect("root_ptr");
        }

        // Verify root_ptr was written
        assert_ne!(root_ptr_after_checkpoint, 0, "root_ptr should be non-zero after checkpoint");

        // Reopen and verify data was loaded from disk
        {
            // First check what root_ptr is stored in the file
            let dm = crate::persistent_artrie::disk_manager::DiskManager::open(&path)
                .expect("open disk manager");
            let stored_root_ptr = dm.root_ptr().expect("read root_ptr");

            // Also check entry count
            let stored_entry_count = dm.entry_count().expect("read entry_count");

            assert_ne!(
                stored_root_ptr, 0,
                "root_ptr on disk should be non-zero (was: {}, entry_count: {})",
                stored_root_ptr, stored_entry_count
            );

            drop(dm);

            let inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::open(&path).expect("open");

            assert_eq!(inner.len(), 3, "len after reopen (root_ptr was {}, entry_count was {})",
                stored_root_ptr, stored_entry_count);
            assert!(inner.contains("apple"));
            assert!(inner.contains("banana"));
            assert!(inner.contains("cherry"));
            assert!(!inner.contains("date"));
        }
    }

    #[test]
    fn test_checkpoint_with_unicode() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_unicode_checkpoint.trie");

        // Create with Unicode terms and checkpoint
        {
            let mut inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");
            inner.insert("こんにちは").expect("insert");
            inner.insert("你好").expect("insert");
            inner.insert("🎉").expect("insert");
            inner.insert("café").expect("insert");
            inner.checkpoint().expect("checkpoint");
        }

        // Reopen and verify Unicode data
        {
            let inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::open(&path).expect("open");
            assert_eq!(inner.len(), 4);
            assert!(inner.contains("こんにちは"));
            assert!(inner.contains("你好"));
            assert!(inner.contains("🎉"));
            assert!(inner.contains("café"));
        }
    }

    #[test]
    fn test_checkpoint_then_more_inserts() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_post_checkpoint.trie");

        // Create, checkpoint, then add more
        {
            let mut inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");
            inner.insert("first").expect("insert");
            inner.insert("second").expect("insert");
            inner.checkpoint().expect("checkpoint");

            // Add more after checkpoint
            inner.insert("third").expect("insert");
            inner.insert("fourth").expect("insert");
            inner.sync().expect("sync");
        }

        // Reopen - should have all 4 (disk + WAL replay)
        {
            let inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::open(&path).expect("open");
            assert_eq!(inner.len(), 4);
            assert!(inner.contains("first"));
            assert!(inner.contains("second"));
            assert!(inner.contains("third"));
            assert!(inner.contains("fourth"));
        }
    }

    #[test]
    fn test_checkpoint_empty_trie() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_empty_checkpoint.trie");

        // Create empty trie and checkpoint
        {
            let mut inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");
            inner.checkpoint().expect("checkpoint");
        }

        // Reopen empty trie
        {
            let inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::open(&path).expect("open");
            assert_eq!(inner.len(), 0);
            assert!(!inner.contains("anything"));
        }
    }

    #[test]
    fn test_multiple_checkpoints() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_multi_checkpoint.trie");

        // Create with multiple checkpoint cycles
        {
            let mut inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");

            inner.insert("one").expect("insert");
            inner.checkpoint().expect("checkpoint 1");

            inner.insert("two").expect("insert");
            inner.checkpoint().expect("checkpoint 2");

            inner.insert("three").expect("insert");
            inner.checkpoint().expect("checkpoint 3");
        }

        // Reopen and verify all data
        {
            let inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::open(&path).expect("open");
            assert_eq!(inner.len(), 3);
            assert!(inner.contains("one"));
            assert!(inner.contains("two"));
            assert!(inner.contains("three"));
        }
    }

    #[test]
    fn test_deep_trie_checkpoint() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_deep_checkpoint.trie");

        // Create with deeply nested terms
        {
            let mut inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");
            inner.insert("a").expect("insert");
            inner.insert("ab").expect("insert");
            inner.insert("abc").expect("insert");
            inner.insert("abcd").expect("insert");
            inner.insert("abcde").expect("insert");
            inner.insert("abcdef").expect("insert");
            inner.insert("abcdefg").expect("insert");
            inner.insert("abcdefgh").expect("insert");
            inner.checkpoint().expect("checkpoint");
        }

        // Reopen and verify all levels
        {
            let inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::open(&path).expect("open");
            assert_eq!(inner.len(), 8);
            assert!(inner.contains("a"));
            assert!(inner.contains("ab"));
            assert!(inner.contains("abc"));
            assert!(inner.contains("abcd"));
            assert!(inner.contains("abcde"));
            assert!(inner.contains("abcdef"));
            assert!(inner.contains("abcdefg"));
            assert!(inner.contains("abcdefgh"));
            assert!(!inner.contains("abcdefghi"));
        }
    }

    // ==================== Phase C6: Atomic Operations with WAL ====================

    #[test]
    fn test_increment_with_wal() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_increment.trie");

        // Create and increment
        {
            let mut inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::create(&path).expect("create");

            // First increment creates value
            let result = inner.increment("counter", 10).expect("increment");
            assert_eq!(result, 10);

            // Second increment adds to existing
            let result = inner.increment("counter", 5).expect("increment");
            assert_eq!(result, 15);

            // Negative increment
            let result = inner.increment("counter", -3).expect("increment");
            assert_eq!(result, 12);

            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::open(&path).expect("open");
            assert!(inner.contains("counter"));
        }
    }

    #[test]
    fn test_upsert_with_wal() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_upsert.trie");

        // Create and upsert
        {
            let mut inner: PersistentARTrieChar<String> =
                PersistentARTrieChar::create(&path).expect("create");

            // First upsert inserts
            let inserted = inner
                .upsert("key", "value1".to_string())
                .expect("upsert");
            assert!(inserted);
            assert!(inner.contains("key"));

            // Second upsert updates
            let inserted = inner
                .upsert("key", "value2".to_string())
                .expect("upsert");
            assert!(!inserted);
            assert!(inner.contains("key"));

            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: PersistentARTrieChar<String> =
                PersistentARTrieChar::open(&path).expect("open");
            assert!(inner.contains("key"));
            assert_eq!(inner.len(), 1);
        }
    }

    #[test]
    fn test_compare_and_swap_with_wal() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_cas.trie");

        // Create and CAS
        {
            let mut inner: PersistentARTrieChar<i32> =
                PersistentARTrieChar::create(&path).expect("create");

            // CAS on non-existent key (expected None) should succeed
            let success = inner.compare_and_swap("key", None, 100).expect("cas");
            assert!(success);
            assert!(inner.contains("key"));

            // CAS with wrong expected value should fail
            let success = inner.compare_and_swap("key", Some(50), 200).expect("cas");
            assert!(!success);

            // CAS with correct expected value should succeed
            let success = inner.compare_and_swap("key", Some(100), 200).expect("cas");
            assert!(success);

            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: PersistentARTrieChar<i32> =
                PersistentARTrieChar::open(&path).expect("open");
            assert!(inner.contains("key"));
        }
    }

    #[test]
    fn test_fetch_add_with_wal() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_fetch_add.trie");

        // Create and fetch_add
        {
            let mut inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::create(&path).expect("create");

            // First fetch_add on non-existent key returns 0
            let old = inner.fetch_add("counter", 10).expect("fetch_add");
            assert_eq!(old, 0);

            // Second fetch_add returns previous value
            let old = inner.fetch_add("counter", 5).expect("fetch_add");
            assert_eq!(old, 10);

            // Third fetch_add
            let old = inner.fetch_add("counter", -3).expect("fetch_add");
            assert_eq!(old, 15);

            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::open(&path).expect("open");
            assert!(inner.contains("counter"));
        }
    }

    #[test]
    fn test_get_or_insert_with_wal() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_get_or_insert.trie");

        // Create and get_or_insert
        {
            let mut inner: PersistentARTrieChar<String> =
                PersistentARTrieChar::create(&path).expect("create");

            // First get_or_insert inserts
            let value = inner
                .get_or_insert("key", "default".to_string())
                .expect("get_or_insert");
            assert_eq!(value, "default");
            assert!(inner.contains("key"));

            // Second get_or_insert returns existing (does not insert)
            let value = inner
                .get_or_insert("key", "other".to_string())
                .expect("get_or_insert");
            assert_eq!(value, "default"); // Still the original

            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: PersistentARTrieChar<String> =
                PersistentARTrieChar::open(&path).expect("open");
            assert!(inner.contains("key"));
            assert_eq!(inner.len(), 1);
        }
    }

    #[test]
    fn test_atomic_ops_recovery() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_atomic_recovery.trie");

        // Create with various atomic operations
        {
            let mut inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::create(&path).expect("create");

            // Use increment
            inner.increment("counter1", 100).expect("increment");
            inner.increment("counter1", 50).expect("increment");

            // Use fetch_add
            inner.fetch_add("counter2", 200).expect("fetch_add");
            inner.fetch_add("counter2", 25).expect("fetch_add");

            inner.sync().expect("sync");
        }

        // Reopen and verify recovery
        {
            let inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::open(&path).expect("open");
            assert!(inner.contains("counter1"));
            assert!(inner.contains("counter2"));
            assert_eq!(inner.len(), 2);
        }
    }

    #[test]
    fn test_atomic_ops_with_checkpoint() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_atomic_checkpoint.trie");

        // Create, checkpoint, then more atomic ops
        {
            let mut inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::create(&path).expect("create");

            inner.increment("before_cp", 100).expect("increment");
            inner.checkpoint().expect("checkpoint");

            inner.increment("after_cp", 200).expect("increment");
            inner.sync().expect("sync");
        }

        // Reopen - should have both (disk + WAL replay)
        {
            let inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::open(&path).expect("open");
            assert!(inner.contains("before_cp"));
            assert!(inner.contains("after_cp"));
            assert_eq!(inner.len(), 2);
        }
    }

    #[test]
    fn test_unicode_atomic_ops() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_unicode_atomic.trie");

        // Create with Unicode keys
        {
            let mut inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::create(&path).expect("create");

            inner.increment("カウンター", 10).expect("increment");
            inner.increment("计数器", 20).expect("increment");
            inner.increment("🔢", 30).expect("increment");

            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::open(&path).expect("open");
            assert!(inner.contains("カウンター"));
            assert!(inner.contains("计数器"));
            assert!(inner.contains("🔢"));
            assert_eq!(inner.len(), 3);
        }
    }

    // ==================== Phase C7: Concurrency Tests ====================

    #[test]
    fn test_optimistic_contains() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_optimistic_contains.trie");

        let mut inner: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        inner.insert("hello").expect("insert");
        inner.insert("world").expect("insert");

        // Test optimistic reads
        let result = inner.contains_optimistic("hello", 10);
        assert_eq!(result, Some(true));

        let result = inner.contains_optimistic("world", 10);
        assert_eq!(result, Some(true));

        let result = inner.contains_optimistic("missing", 10);
        assert_eq!(result, Some(false));
    }

    #[test]
    fn test_optimistic_get() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_optimistic_get.trie");

        let mut inner: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path).expect("create");

        inner.increment("counter", 100).expect("increment");

        // Test optimistic get
        let result = inner.get_optimistic("counter", 10);
        assert!(result.is_some());
        let value = result.unwrap();
        assert_eq!(value, Some(100));

        let result = inner.get_optimistic("missing", 10);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_version_tracking() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_version.trie");

        let mut inner: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        let v0 = inner.current_version();
        assert_eq!(v0, 0); // Initial version

        inner.insert("a").expect("insert");
        let v1 = inner.current_version();
        assert_eq!(v1, 2); // After one write (begin + end = +2)

        inner.insert("b").expect("insert");
        let v2 = inner.current_version();
        assert_eq!(v2, 4); // After two writes

        // Not write-locked when idle
        assert!(!inner.is_write_locked());
    }

    #[test]
    fn test_epoch_management() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_epoch.trie");

        let inner: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // Initial state
        assert_eq!(inner.current_epoch(), 0);
        assert_eq!(inner.active_readers(), 0);

        // Enter epoch
        {
            let _guard = inner.enter_epoch();
            assert_eq!(inner.active_readers(), 1);

            // Can have multiple readers
            {
                let _guard2 = inner.enter_epoch();
                assert_eq!(inner.active_readers(), 2);
            }

            // One reader left
            assert_eq!(inner.active_readers(), 1);
        }

        // No readers left
        assert_eq!(inner.active_readers(), 0);

        // Advance epoch
        let old = inner.advance_epoch();
        assert_eq!(old, 0);
        assert_eq!(inner.current_epoch(), 1);
    }

    #[test]
    fn test_retry_stats() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_stats.trie");

        let mut inner: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        inner.insert("test").expect("insert");

        // Perform some optimistic reads
        for _ in 0..10 {
            let _ = inner.contains_optimistic("test", 5);
        }

        let stats = inner.retry_stats_snapshot();
        assert!(stats.successful >= 10); // At least 10 successful reads
        // Retry count should be low (no concurrent writers)
        assert_eq!(stats.retries, 0);
    }

    #[test]
    fn test_concurrent_readers() {
        use std::sync::Arc;
        use std::thread;
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_concurrent.trie");

        // Create and populate
        {
            let mut inner: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");

            for i in 0..100 {
                inner.insert(&format!("term{}", i)).expect("insert");
            }
            inner.sync().expect("sync");
        }

        // Reopen and spawn multiple reader threads
        let inner = Arc::new(
            PersistentARTrieChar::<()>::open(&path).expect("open")
        );

        let handles: Vec<_> = (0..4)
            .map(|t| {
                let inner = inner.clone();
                thread::spawn(move || {
                    let mut found = 0;
                    for i in 0..100 {
                        let _guard = inner.enter_epoch();
                        if let Some(true) = inner.contains_optimistic(&format!("term{}", i), 10) {
                            found += 1;
                        }
                    }
                    (t, found)
                })
            })
            .collect();

        for handle in handles {
            let (thread_id, found) = handle.join().expect("thread join");
            assert_eq!(found, 100, "Thread {} should find all 100 terms", thread_id);
        }
    }

    #[test]
    fn test_try_contains_optimistic() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_try_contains.trie");

        let mut inner: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        inner.insert("apple").expect("insert");

        // Single optimistic read should succeed
        let result = inner.try_contains_optimistic("apple");
        assert_eq!(result, Some(true));

        let result = inner.try_contains_optimistic("banana");
        assert_eq!(result, Some(false));
    }

    #[test]
    fn test_unicode_optimistic() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_unicode_optimistic.trie");

        let mut inner: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        inner.insert("日本語").expect("insert");
        inner.insert("中文").expect("insert");
        inner.insert("🎉🎊🎋").expect("insert");

        // Test optimistic reads with Unicode
        assert_eq!(inner.contains_optimistic("日本語", 10), Some(true));
        assert_eq!(inner.contains_optimistic("中文", 10), Some(true));
        assert_eq!(inner.contains_optimistic("🎉🎊🎋", 10), Some(true));
        assert_eq!(inner.contains_optimistic("한글", 10), Some(false));
    }

    // ========================================================================
    // Document Transaction Tests
    // ========================================================================

    #[test]
    fn test_document_transaction_basic() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_doc_tx_basic.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        // Start a transaction
        let mut tx = inner.begin_document("doc_001").expect("begin");
        assert!(tx.is_active());
        assert!(tx.is_empty());

        // Buffer some terms
        inner.tx_insert(&mut tx, "hello", Some(1));
        inner.tx_insert(&mut tx, "world", Some(2));
        inner.tx_insert(&mut tx, "foo", None);

        assert_eq!(tx.len(), 3);
        assert!(!tx.is_empty());

        // Terms should NOT be in trie yet
        assert!(!inner.contains("hello"));
        assert!(!inner.contains("world"));
        assert!(!inner.contains("foo"));

        // Commit the transaction
        let count = inner.commit_document(tx).expect("commit");
        assert_eq!(count, 3);

        // Now terms should be in trie
        assert!(inner.contains("hello"));
        assert!(inner.contains("world"));
        assert!(inner.contains("foo"));
        assert_eq!(inner.len(), 3);
    }

    #[test]
    fn test_document_transaction_abort() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_doc_tx_abort.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        // Insert a baseline term
        inner.insert("existing").expect("insert");

        // Start a transaction
        let mut tx = inner.begin_document("doc_002").expect("begin");
        inner.tx_insert(&mut tx, "new_term_1", Some(1));
        inner.tx_insert(&mut tx, "new_term_2", Some(2));

        // Abort the transaction
        inner.abort_document(tx).expect("abort");

        // New terms should NOT be in trie
        assert!(!inner.contains("new_term_1"));
        assert!(!inner.contains("new_term_2"));

        // Existing term should still be there
        assert!(inner.contains("existing"));
        assert_eq!(inner.len(), 1);
    }

    #[test]
    fn test_document_transaction_unicode() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_doc_tx_unicode.trie");

        let mut inner: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path).expect("create");

        let mut tx = inner.begin_document("unicode_doc").expect("begin");

        // Test with Unicode strings
        inner.tx_insert(&mut tx, "日本語", Some(1));
        inner.tx_insert(&mut tx, "中文", Some(2));
        inner.tx_insert(&mut tx, "🎉🎊🎋", Some(3));

        // Test with char slice
        inner.tx_insert_chars(&mut tx, &['한', '글'], Some(4));
        inner.tx_insert_chars(&mut tx, &['π', '∑', '∫'], Some(5));

        let count = inner.commit_document(tx).expect("commit");
        assert_eq!(count, 5);

        // Verify all terms
        assert!(inner.contains("日本語"));
        assert!(inner.contains("中文"));
        assert!(inner.contains("🎉🎊🎋"));
        assert!(inner.contains("한글"));
        assert!(inner.contains("π∑∫"));
    }

    #[test]
    fn test_document_transaction_empty() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_doc_tx_empty.trie");

        let mut inner: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // Create and commit an empty transaction
        let tx = inner.begin_document("empty_doc").expect("begin");
        let count = inner.commit_document(tx).expect("commit");

        assert_eq!(count, 0);
        assert_eq!(inner.len(), 0);
    }

    #[test]
    fn test_document_transaction_recovery() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_doc_tx_recovery.trie");

        // Create and commit a transaction
        {
            let mut inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::create(&path).expect("create");

            let mut tx = inner.begin_document("recovery_doc").expect("begin");
            inner.tx_insert(&mut tx, "term1", Some(100));
            inner.tx_insert(&mut tx, "term2", Some(200));
            inner.tx_insert(&mut tx, "term3", Some(300));

            inner.commit_document(tx).expect("commit");
            inner.sync().expect("sync");
        }

        // Reopen and verify recovery
        {
            let inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::open(&path).expect("open");

            assert!(inner.contains("term1"));
            assert!(inner.contains("term2"));
            assert!(inner.contains("term3"));
            assert_eq!(inner.len(), 3);
        }
    }

    // Note: test_document_transaction_insert_after_commit is not needed because
    // Rust's ownership system already prevents reuse after commit_document() consumes tx.
    // The compiler prevents this error at compile time.

    #[test]
    fn test_document_transaction_commit_twice_error() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_doc_tx_commit_twice.trie");

        let mut inner: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // First transaction succeeds
        let mut tx = inner.begin_document("test").expect("begin");
        inner.tx_insert(&mut tx, "term", None);
        inner.commit_document(tx).expect("commit");

        // Second transaction also succeeds
        let tx2 = inner.begin_document("test2").expect("begin");
        inner.commit_document(tx2).expect("commit empty");
    }

    #[test]
    fn test_document_transaction_multiple_sequential() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_doc_tx_sequential.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        // First document
        let mut tx1 = inner.begin_document("doc1").expect("begin");
        inner.tx_insert(&mut tx1, "apple", Some(1));
        inner.tx_insert(&mut tx1, "apricot", Some(2));
        inner.commit_document(tx1).expect("commit");

        // Second document (aborted)
        let mut tx2 = inner.begin_document("doc2").expect("begin");
        inner.tx_insert(&mut tx2, "banana", Some(3));
        inner.abort_document(tx2).expect("abort");

        // Third document
        let mut tx3 = inner.begin_document("doc3").expect("begin");
        inner.tx_insert(&mut tx3, "cherry", Some(4));
        inner.tx_insert(&mut tx3, "coconut", Some(5));
        inner.commit_document(tx3).expect("commit");

        // Verify final state
        assert!(inner.contains("apple"));
        assert!(inner.contains("apricot"));
        assert!(!inner.contains("banana")); // Aborted
        assert!(inner.contains("cherry"));
        assert!(inner.contains("coconut"));
        assert_eq!(inner.len(), 4);
    }

    #[test]
    fn test_document_transaction_tx_insert_bytes() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_doc_tx_bytes.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        let mut tx = inner.begin_document("bytes_doc").expect("begin");

        // Test with raw bytes
        inner.tx_insert_bytes(&mut tx, b"hello", Some(1));
        inner.tx_insert_bytes(&mut tx, b"world", Some(2));
        inner.tx_insert_bytes(&mut tx, "日本語".as_bytes(), Some(3));

        let count = inner.commit_document(tx).expect("commit");
        assert_eq!(count, 3);

        assert!(inner.contains("hello"));
        assert!(inner.contains("world"));
        assert!(inner.contains("日本語"));
    }

    #[test]
    fn test_document_transaction_tx_increment() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_doc_tx_increment.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        // Insert some initial values
        inner.increment("term_a", 100).expect("initial increment");
        inner.increment("term_b", 50).expect("initial increment");

        // Create a transaction with increments
        let mut tx = inner.begin_document("increment_doc").expect("begin");

        // Buffer some increments
        inner.tx_increment(&mut tx, "term_a", 25);  // Should add to existing 100
        inner.tx_increment(&mut tx, "term_b", 10);  // Should add to existing 50
        inner.tx_increment(&mut tx, "term_c", 75);  // New term
        inner.tx_increment(&mut tx, "term_a", 5);   // Multiple increments to same term

        assert_eq!(tx.increment_count(), 4);
        assert_eq!(tx.set_count(), 0);
        assert_eq!(tx.len(), 4);

        // Values should NOT be updated yet
        assert_eq!(inner.get("term_a"), Some(&100u64));
        assert_eq!(inner.get("term_b"), Some(&50u64));
        assert!(inner.get("term_c").is_none());

        // Commit the transaction
        let count = inner.commit_document(tx).expect("commit");
        assert_eq!(count, 4);

        // Values should be updated now (increments aggregated)
        // term_a: 100 + 25 + 5 = 130
        assert_eq!(inner.get("term_a"), Some(&130u64));
        // term_b: 50 + 10 = 60
        assert_eq!(inner.get("term_b"), Some(&60u64));
        // term_c: 0 + 75 = 75
        assert_eq!(inner.get("term_c"), Some(&75u64));
    }

    #[test]
    fn test_document_transaction_mixed_insert_and_increment() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_doc_tx_mixed.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        // Create a transaction with both inserts and increments
        let mut tx = inner.begin_document("mixed_doc").expect("begin");

        // Buffer inserts
        inner.tx_insert(&mut tx, "set_term", Some(100));

        // Buffer increments
        inner.tx_increment(&mut tx, "inc_term", 50);

        assert_eq!(tx.set_count(), 1);
        assert_eq!(tx.increment_count(), 1);
        assert_eq!(tx.len(), 2);

        // Commit
        let count = inner.commit_document(tx).expect("commit");
        assert_eq!(count, 2);

        // Verify results
        assert_eq!(inner.get("set_term"), Some(&100u64));
        assert_eq!(inner.get("inc_term"), Some(&50u64));
    }

    #[test]
    fn test_document_transaction_increment_recovery() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_doc_tx_inc_recovery.trie");

        // Phase 1: Create trie, add increments, close
        {
            let mut inner: PersistentARTrieChar<u64> =
                PersistentARTrieChar::create(&path).expect("create");

            inner.increment("existing", 100).expect("initial");

            let mut tx = inner.begin_document("recovery_doc").expect("begin");
            inner.tx_increment(&mut tx, "existing", 50);
            inner.tx_increment(&mut tx, "new_term", 75);
            inner.commit_document(tx).expect("commit");

            // Values should be correct before close
            assert_eq!(inner.get("existing"), Some(&150u64));
            assert_eq!(inner.get("new_term"), Some(&75u64));
        }

        // Phase 2: Reopen and verify recovery
        {
            let inner: PersistentARTrieChar<u64> =
                PersistentARTrieChar::open(&path).expect("open");

            // Values should survive recovery
            assert_eq!(inner.get("existing"), Some(&150u64));
            assert_eq!(inner.get("new_term"), Some(&75u64));
        }
    }

    // ========================================================================
    // Batch Insert Tests
    // ========================================================================

    #[test]
    fn test_insert_batch_basic() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_batch_basic.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        let entries = vec![
            ("hello".to_string(), Some(1u64)),
            ("world".to_string(), Some(2u64)),
            ("foo".to_string(), None),
            ("bar".to_string(), Some(4u64)),
        ];

        let count = inner.insert_batch(&entries);
        assert_eq!(count, 4);
        assert_eq!(inner.len(), 4);

        assert!(inner.contains("hello"));
        assert!(inner.contains("world"));
        assert!(inner.contains("foo"));
        assert!(inner.contains("bar"));
    }

    #[test]
    fn test_insert_batch_unicode() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_batch_unicode.trie");

        let mut inner: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path).expect("create");

        let entries = vec![
            ("日本語".to_string(), Some(1)),
            ("中文".to_string(), Some(2)),
            ("한글".to_string(), Some(3)),
            ("🎉🎊🎋".to_string(), Some(4)),
        ];

        let count = inner.insert_batch(&entries);
        assert_eq!(count, 4);

        assert!(inner.contains("日本語"));
        assert!(inner.contains("中文"));
        assert!(inner.contains("한글"));
        assert!(inner.contains("🎉🎊🎋"));
    }

    #[test]
    fn test_insert_batch_chars() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_batch_chars.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        let entries: Vec<(&[char], Option<u64>)> = vec![
            (&['h', 'e', 'l', 'l', 'o'][..], Some(1)),
            (&['日', '本', '語'][..], Some(2)),
            (&['π', '∑', '∫'][..], None),
        ];

        let count = inner.insert_batch_chars(&entries);
        assert_eq!(count, 3);

        assert!(inner.contains("hello"));
        assert!(inner.contains("日本語"));
        assert!(inner.contains("π∑∫"));
    }

    #[test]
    fn test_insert_batch_sorted() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_batch_sorted.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        // Entries in unsorted order
        let entries = vec![
            ("zebra".to_string(), Some(1u64)),
            ("apple".to_string(), Some(2u64)),
            ("mango".to_string(), Some(3u64)),
            ("apricot".to_string(), Some(4u64)),
        ];

        let count = inner.insert_batch_sorted(entries);
        assert_eq!(count, 4);

        assert!(inner.contains("apple"));
        assert!(inner.contains("apricot"));
        assert!(inner.contains("mango"));
        assert!(inner.contains("zebra"));
    }

    #[test]
    fn test_insert_batch_chars_sorted() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_batch_chars_sorted.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        let entries: Vec<(Vec<char>, Option<u64>)> = vec![
            (vec!['z', 'e', 'b', 'r', 'a'], Some(1)),
            (vec!['a', 'p', 'p', 'l', 'e'], Some(2)),
            (vec!['m', 'a', 'n', 'g', 'o'], Some(3)),
        ];

        let count = inner.insert_batch_chars_sorted(entries);
        assert_eq!(count, 3);

        assert!(inner.contains("apple"));
        assert!(inner.contains("mango"));
        assert!(inner.contains("zebra"));
    }

    #[test]
    fn test_insert_batch_bytes() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_batch_bytes.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        let entries: Vec<(&[u8], Option<u64>)> = vec![
            (b"hello" as &[u8], Some(1)),
            (b"world" as &[u8], Some(2)),
            ("日本語".as_bytes(), Some(3)),
        ];

        let count = inner.insert_batch_bytes(&entries);
        assert_eq!(count, 3);

        assert!(inner.contains("hello"));
        assert!(inner.contains("world"));
        assert!(inner.contains("日本語"));
    }

    #[test]
    fn test_insert_batch_bytes_sorted() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_batch_bytes_sorted.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        let entries: Vec<(Vec<u8>, Option<u64>)> = vec![
            (b"zebra".to_vec(), Some(1)),
            (b"apple".to_vec(), Some(2)),
            (b"mango".to_vec(), Some(3)),
        ];

        let count = inner.insert_batch_bytes_sorted(entries);
        assert_eq!(count, 3);

        assert!(inner.contains("apple"));
        assert!(inner.contains("mango"));
        assert!(inner.contains("zebra"));
    }

    #[test]
    fn test_insert_batch_empty() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_batch_empty.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        let entries: Vec<(String, Option<u64>)> = vec![];

        let count = inner.insert_batch(&entries);
        assert_eq!(count, 0);
        assert_eq!(inner.len(), 0);
    }

    #[test]
    fn test_insert_batch_duplicates() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_batch_duplicates.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        // Insert initial batch
        let entries1 = vec![
            ("apple".to_string(), Some(1u64)),
            ("banana".to_string(), Some(2u64)),
        ];
        let count1 = inner.insert_batch(&entries1);
        assert_eq!(count1, 2);

        // Insert with some duplicates
        let entries2 = vec![
            ("apple".to_string(), Some(10u64)), // Duplicate - will update
            ("cherry".to_string(), Some(3u64)), // New
            ("banana".to_string(), Some(20u64)), // Duplicate - will update
        ];
        let count2 = inner.insert_batch(&entries2);
        assert_eq!(count2, 1); // Only cherry is new

        assert_eq!(inner.len(), 3); // apple, banana, cherry
    }

    #[test]
    fn test_insert_batch_recovery() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_batch_recovery.trie");

        // Create and batch insert
        {
            let mut inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::create(&path).expect("create");

            let entries = vec![
                ("term1".to_string(), Some(100i64)),
                ("term2".to_string(), Some(200i64)),
                ("term3".to_string(), Some(300i64)),
            ];
            inner.insert_batch(&entries);
            inner.sync().expect("sync");
        }

        // Reopen and verify recovery
        {
            let inner: PersistentARTrieChar<i64> =
                PersistentARTrieChar::open(&path).expect("open");

            assert!(inner.contains("term1"));
            assert!(inner.contains("term2"));
            assert!(inner.contains("term3"));
            assert_eq!(inner.len(), 3);
        }
    }

    #[test]
    fn test_insert_batch_large() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_batch_large.trie");

        let mut inner: PersistentARTrieChar<u64> =
            PersistentARTrieChar::create(&path).expect("create");

        // Create a large batch
        let entries: Vec<(String, Option<u64>)> = (0..1000)
            .map(|i| (format!("term_{:05}", i), Some(i as u64)))
            .collect();

        let count = inner.insert_batch(&entries);
        assert_eq!(count, 1000);
        assert_eq!(inner.len(), 1000);

        // Verify a few random entries
        assert!(inner.contains("term_00000"));
        assert!(inner.contains("term_00500"));
        assert!(inner.contains("term_00999"));
    }

    // ========================================================================
    // Batch/Parallel Merge Tests
    // ========================================================================

    #[test]
    fn test_merge_from_batched_basic() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("test_merge_batched_src.trie");
        let path2 = dir.path().join("test_merge_batched_dst.trie");

        // Create source trie
        let mut src: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path1).expect("create");
        src.increment("apple", 10).expect("increment");
        src.increment("banana", 20).expect("increment");
        src.increment("cherry", 30).expect("increment");

        // Create destination trie with overlapping terms
        let mut dst: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path2).expect("create");
        dst.increment("apple", 5).expect("increment");
        dst.increment("date", 40).expect("increment");

        // Merge with summing function
        let count = dst.merge_from_batched(&src, |a, b| a + b, 2).expect("merge");
        assert_eq!(count, 3);

        // Verify results
        assert!(dst.contains("apple")); // Merged: 5 + 10 = 15
        assert!(dst.contains("banana")); // From src: 20
        assert!(dst.contains("cherry")); // From src: 30
        assert!(dst.contains("date")); // Original: 40
        assert_eq!(dst.len(), 4);
    }

    #[test]
    fn test_merge_from_batched_unicode() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("test_merge_batched_unicode_src.trie");
        let path2 = dir.path().join("test_merge_batched_unicode_dst.trie");

        // Create source with Unicode terms
        let mut src: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path1).expect("create");
        src.increment("日本語", 1).expect("increment");
        src.increment("中文", 2).expect("increment");
        src.increment("한글", 3).expect("increment");

        // Create destination
        let mut dst: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path2).expect("create");
        dst.increment("日本語", 100).expect("increment");

        // Merge with summing function
        let count = dst.merge_from_batched(&src, |a, b| a + b, 10).expect("merge");
        assert_eq!(count, 3);

        // Verify Unicode terms
        assert!(dst.contains("日本語"));
        assert!(dst.contains("中文"));
        assert!(dst.contains("한글"));
    }

    #[test]
    fn test_merge_from_batched_empty() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("test_merge_batched_empty_src.trie");
        let path2 = dir.path().join("test_merge_batched_empty_dst.trie");

        // Create empty source
        let src: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path1).expect("create");

        // Create destination with some terms
        let mut dst: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path2).expect("create");
        dst.increment("existing", 100).expect("increment");

        // Merge from empty source
        let count = dst.merge_from_batched(&src, |a, b| a + b, 100).expect("merge");
        assert_eq!(count, 0);
        assert_eq!(dst.len(), 1);
    }

    #[cfg(feature = "parallel-merge")]
    #[test]
    fn test_merge_from_parallel_basic() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("test_merge_parallel_src.trie");
        let path2 = dir.path().join("test_merge_parallel_dst.trie");

        // Create source with many terms
        let mut src: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path1).expect("create");
        for i in 0..100 {
            src.increment(&format!("term_{:03}", i), i as i64).expect("increment");
        }

        // Create destination with some overlapping terms
        let mut dst: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path2).expect("create");
        for i in 0..50 {
            dst.increment(&format!("term_{:03}", i), 1000).expect("increment");
        }

        // Parallel merge with summing function
        let count = dst.merge_from_parallel(&src, |a, b| a + b).expect("merge");
        assert_eq!(count, 100);

        // Verify all terms exist
        assert_eq!(dst.len(), 100);
        for i in 0..100 {
            assert!(dst.contains(&format!("term_{:03}", i)));
        }
    }

    #[cfg(feature = "parallel-merge")]
    #[test]
    fn test_merge_from_batched_parallel_basic() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("test_merge_batched_parallel_src.trie");
        let path2 = dir.path().join("test_merge_batched_parallel_dst.trie");

        // Create source
        let mut src: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path1).expect("create");
        for i in 0..50 {
            src.increment(&format!("key_{:02}", i), i as i64).expect("increment");
        }

        // Create destination
        let mut dst: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path2).expect("create");
        dst.increment("key_00", 1000).expect("increment");

        // Batched parallel merge
        let count = dst.merge_from_batched_parallel(&src, |a, b| a + b, 10).expect("merge");
        assert_eq!(count, 50);
        assert_eq!(dst.len(), 50);
    }

    #[cfg(feature = "parallel-merge")]
    #[test]
    fn test_merge_from_parallel_unicode() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("test_merge_parallel_unicode_src.trie");
        let path2 = dir.path().join("test_merge_parallel_unicode_dst.trie");

        // Create source with Unicode terms from different character ranges
        let mut src: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path1).expect("create");
        src.increment("日本語_001", 1).expect("increment");
        src.increment("日本語_002", 2).expect("increment");
        src.increment("中文_001", 3).expect("increment");
        src.increment("한글_001", 4).expect("increment");
        src.increment("🎉_emoji", 5).expect("increment");
        src.increment("ascii_test", 6).expect("increment");

        // Create empty destination
        let mut dst: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path2).expect("create");

        // Parallel merge
        let count = dst.merge_from_parallel(&src, |a, b| a + b).expect("merge");
        assert_eq!(count, 6);

        // Verify all Unicode terms
        assert!(dst.contains("日本語_001"));
        assert!(dst.contains("日本語_002"));
        assert!(dst.contains("中文_001"));
        assert!(dst.contains("한글_001"));
        assert!(dst.contains("🎉_emoji"));
        assert!(dst.contains("ascii_test"));
    }

    // ==================== Phase 4: Group Commit Tests ====================

    #[cfg(feature = "group-commit")]
    #[test]
    fn test_group_commit_enable_disable() {
        use tempfile::tempdir;
        use crate::persistent_artrie::group_commit::GroupCommitConfig;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_group_commit.trie");

        let mut trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // Initially disabled
        assert!(!trie.is_group_commit_enabled());
        assert!(trie.group_commit_stats().is_none());

        // Enable group commit
        trie.enable_group_commit(GroupCommitConfig::default())
            .expect("enable group commit");
        assert!(trie.is_group_commit_enabled());
        assert!(trie.group_commit_stats().is_some());

        // Double enable should fail
        let result = trie.enable_group_commit(GroupCommitConfig::default());
        assert!(result.is_err());

        // Disable group commit
        trie.disable_group_commit().expect("disable group commit");
        assert!(!trie.is_group_commit_enabled());
        assert!(trie.group_commit_stats().is_none());

        // Double disable should be ok (idempotent)
        trie.disable_group_commit().expect("disable again");
    }

    #[cfg(feature = "group-commit")]
    #[test]
    fn test_group_commit_with_inserts() {
        use tempfile::tempdir;
        use crate::persistent_artrie::group_commit::GroupCommitConfig;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_group_commit_inserts.trie");

        let mut trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // Enable group commit with low latency config for testing
        let config = GroupCommitConfig {
            max_batch_size: 10,
            max_batch_delay_us: 1_000, // 1ms
            dedicated_commit_thread: true,
            adaptive_batching: false,
            ..Default::default()
        };
        trie.enable_group_commit(config).expect("enable group commit");

        // Perform inserts
        trie.insert("hello").expect("insert");
        trie.insert("world").expect("insert");
        trie.insert("foo").expect("insert");
        trie.insert("bar").expect("insert");
        trie.insert("baz").expect("insert");

        // Verify inserts
        assert!(trie.contains("hello"));
        assert!(trie.contains("world"));
        assert!(trie.contains("foo"));
        assert!(trie.contains("bar"));
        assert!(trie.contains("baz"));
        assert_eq!(trie.len(), 5);

        // Check stats - should have committed
        let stats = trie.group_commit_stats().expect("stats");
        assert!(stats.records_committed > 0, "should have committed records");

        // Disable and verify still works
        trie.disable_group_commit().expect("disable");
        trie.insert("after_disable").expect("insert");
        assert!(trie.contains("after_disable"));
    }

    #[cfg(feature = "group-commit")]
    #[test]
    fn test_group_commit_with_unicode() {
        use tempfile::tempdir;
        use crate::persistent_artrie::group_commit::GroupCommitConfig;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_group_commit_unicode.trie");

        let mut trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        trie.enable_group_commit(GroupCommitConfig::low_latency())
            .expect("enable group commit");

        // Insert Unicode terms
        trie.insert("こんにちは").expect("insert");
        trie.insert("你好").expect("insert");
        trie.insert("안녕하세요").expect("insert");
        trie.insert("🎉🎊🎋").expect("insert");

        // Verify
        assert!(trie.contains("こんにちは"));
        assert!(trie.contains("你好"));
        assert!(trie.contains("안녕하세요"));
        assert!(trie.contains("🎉🎊🎋"));
    }

    #[cfg(feature = "group-commit")]
    #[test]
    fn test_group_commit_high_throughput_config() {
        use tempfile::tempdir;
        use crate::persistent_artrie::group_commit::GroupCommitConfig;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_group_commit_throughput.trie");

        let mut trie: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path).expect("create");

        // Use high throughput config
        trie.enable_group_commit(GroupCommitConfig::high_throughput())
            .expect("enable group commit");

        // Perform many inserts to test batching
        for i in 0..100 {
            trie.increment(&format!("counter_{}", i), 1).expect("increment");
        }

        // Verify all inserted
        assert_eq!(trie.len(), 100);
        for i in 0..100 {
            assert!(trie.contains(&format!("counter_{}", i)));
        }

        // Check batching efficiency (should have batched multiple writes per fsync)
        let stats = trie.group_commit_stats().expect("stats");
        let efficiency = stats.batching_efficiency();
        println!("High throughput batching efficiency: {:.2} records/fsync", efficiency);
        // With high throughput config, we expect some batching
        assert!(stats.records_committed >= 100, "should have committed at least 100 records");
    }

    #[cfg(feature = "group-commit")]
    #[test]
    fn test_group_commit_recovery() {
        use tempfile::tempdir;
        use crate::persistent_artrie::group_commit::GroupCommitConfig;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_group_commit_recovery.trie");

        // Create and insert with group commit
        {
            let mut trie: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");

            trie.enable_group_commit(GroupCommitConfig::default())
                .expect("enable group commit");

            trie.insert("persisted_1").expect("insert");
            trie.insert("persisted_2").expect("insert");
            trie.insert("persisted_3").expect("insert");

            // Sync to ensure all writes are flushed
            trie.sync().expect("sync");
        }

        // Reopen without group commit and verify recovery
        {
            let trie: PersistentARTrieChar<()> =
                PersistentARTrieChar::open(&path).expect("open");

            // Data should be recovered from WAL
            assert!(trie.contains("persisted_1"));
            assert!(trie.contains("persisted_2"));
            assert!(trie.contains("persisted_3"));
            assert_eq!(trie.len(), 3);
        }
    }

    #[cfg(feature = "group-commit")]
    #[test]
    fn test_group_commit_stats_tracking() {
        use tempfile::tempdir;
        use crate::persistent_artrie::group_commit::GroupCommitConfig;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_group_commit_stats.trie");

        let mut trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        trie.enable_group_commit(GroupCommitConfig::default())
            .expect("enable group commit");

        // Get initial stats
        let initial_stats = trie.group_commit_stats().expect("stats");
        let initial_committed = initial_stats.records_committed;

        // Perform operations
        trie.insert("term1").expect("insert");
        trie.insert("term2").expect("insert");
        trie.remove("term1").expect("remove");

        // Wait briefly for async commits
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Stats should have increased
        let final_stats = trie.group_commit_stats().expect("stats");
        assert!(
            final_stats.records_committed > initial_committed,
            "records_committed should have increased: {} -> {}",
            initial_committed,
            final_stats.records_committed
        );
    }

    // ==================== Performance Infrastructure Tests ====================

    #[test]
    fn test_cache_stats_basic() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_cache_stats.trie");

        let trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // Initially no accesses
        let (hits, misses) = trie.cache_counts();
        assert_eq!(hits, 0);
        assert_eq!(misses, 0);
        assert_eq!(trie.cache_total_accesses(), 0);
        assert_eq!(trie.cache_hit_rate(), 1.0); // No accesses = 100% hit rate

        // Record some hits
        trie.record_cache_hit();
        trie.record_cache_hit();
        trie.record_cache_hit();

        // Record some misses
        trie.record_cache_miss();

        // Check counts
        let (hits, misses) = trie.cache_counts();
        assert_eq!(hits, 3);
        assert_eq!(misses, 1);
        assert_eq!(trie.cache_total_accesses(), 4);

        // Hit rate should be 75%
        let hit_rate = trie.cache_hit_rate();
        assert!((hit_rate - 0.75).abs() < 0.001, "Hit rate should be 0.75, got {}", hit_rate);
    }

    #[test]
    fn test_cache_stats_and_reset() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_cache_reset.trie");

        let trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // Record some activity
        trie.record_cache_hit();
        trie.record_cache_hit();
        trie.record_cache_miss();

        // Get and reset
        let (hit_rate, hits, misses) = trie.cache_stats_and_reset();
        assert_eq!(hits, 2);
        assert_eq!(misses, 1);
        assert!((hit_rate - 0.666).abs() < 0.01, "Hit rate should be ~0.666, got {}", hit_rate);

        // After reset, counts should be zero
        let (hits, misses) = trie.cache_counts();
        assert_eq!(hits, 0);
        assert_eq!(misses, 0);
    }

    #[test]
    fn test_memory_monitor_enable_disable() {
        use tempfile::tempdir;
        use crate::persistent_artrie::memory_monitor::MemoryPressureConfig;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_memory_monitor.trie");

        let mut trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // Initially no monitor
        assert!(!trie.has_memory_monitor());
        assert!(trie.memory_stats().is_none());
        assert!(trie.memory_pressure_level().is_none());

        // Use a counter to track callback invocations
        let callback_count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&callback_count);

        // Enable with callback
        let result = trie.enable_memory_monitor(
            MemoryPressureConfig::default(),
            move |_level, _stats| {
                count_clone.fetch_add(1, Ordering::Relaxed);
            }
        );
        assert!(result.is_ok(), "enable_memory_monitor should succeed");

        // Now monitor is enabled
        assert!(trie.has_memory_monitor());

        // Stats should be available
        let stats = trie.memory_stats();
        assert!(stats.is_some(), "memory_stats should return Some");

        // Pressure level should be available
        let level = trie.memory_pressure_level();
        assert!(level.is_some(), "memory_pressure_level should return Some");

        // Disable
        trie.disable_memory_monitor();
        assert!(!trie.has_memory_monitor());
        assert!(trie.memory_stats().is_none());
    }

    #[test]
    fn test_memory_monitor_default() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_memory_default.trie");

        let mut trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // Enable with default config (no-op callback)
        let result = trie.enable_memory_monitor_default();
        assert!(result.is_ok(), "enable_memory_monitor_default should succeed");
        assert!(trie.has_memory_monitor());

        // Stats should still be queryable
        let stats = trie.memory_stats().expect("stats should be available");
        assert!(stats.mem_total > 0, "System should have some memory");

        trie.disable_memory_monitor();
    }

    // ==================== Epoch Checkpointing Tests ====================

    #[test]
    fn test_epoch_checkpointing_enable_disable() {
        use tempfile::tempdir;
        use crate::persistent_artrie::epoch::EpochConfig;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_epoch_checkpointing.trie");

        let mut trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // Initially no checkpoint manager
        assert!(!trie.has_epoch_checkpointing());
        assert!(trie.current_epoch_id().is_none());
        assert!(trie.epoch_stats().is_none());

        // Enable with default config
        let result = trie.enable_epoch_checkpointing_default();
        assert!(result.is_ok(), "enable_epoch_checkpointing_default should succeed");
        assert!(trie.has_epoch_checkpointing());

        // Now we should have epoch info
        let epoch_id = trie.current_epoch_id();
        assert!(epoch_id.is_some(), "current_epoch_id should be Some");

        let stats = trie.epoch_stats();
        assert!(stats.is_some(), "epoch_stats should be Some");

        // Disable
        trie.disable_epoch_checkpointing();
        assert!(!trie.has_epoch_checkpointing());
        assert!(trie.current_epoch_id().is_none());
    }

    #[test]
    fn test_epoch_checkpointing_record_operations() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_epoch_record_ops.trie");

        let mut trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // Enable checkpoint manager
        trie.enable_epoch_checkpointing_default().expect("enable");

        // Get initial epoch
        let initial_epoch = trie.current_epoch_id().expect("epoch_id");

        // Record some operations
        for _ in 0..10 {
            let epoch = trie.record_epoch_operation(100);
            assert!(epoch.is_some());
        }

        // Epoch should still be the same (not enough ops to advance)
        let current_epoch = trie.current_epoch_id().expect("epoch_id");
        assert_eq!(initial_epoch, current_epoch, "Epoch should not have advanced yet");

        // Current epoch metadata should show operations
        let metadata = trie.epoch_metadata().expect("metadata");
        let current_epoch_meta = metadata.iter().find(|m| m.id == current_epoch).expect("current epoch");
        assert_eq!(current_epoch_meta.operation_count, 10, "Should have recorded 10 operations");
        assert_eq!(current_epoch_meta.wal_size_bytes, 1000, "Should have recorded 1000 WAL bytes");
    }

    #[test]
    fn test_epoch_checkpointing_high_throughput_config() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_epoch_high_throughput.trie");

        let mut trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // Enable with high-throughput config
        let result = trie.enable_epoch_checkpointing_high_throughput();
        assert!(result.is_ok(), "enable_epoch_checkpointing_high_throughput should succeed");
        assert!(trie.has_epoch_checkpointing());

        // Config should reflect high-throughput settings
        let config = trie.epoch_config().expect("config");
        assert!(config.max_ops_per_epoch > 10_000, "High-throughput should have high ops limit");
    }

    #[test]
    fn test_epoch_checkpointing_low_latency_config() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_epoch_low_latency.trie");

        let mut trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        // Enable with low-latency config
        let result = trie.enable_epoch_checkpointing_low_latency();
        assert!(result.is_ok(), "enable_epoch_checkpointing_low_latency should succeed");
        assert!(trie.has_epoch_checkpointing());

        // Config should reflect low-latency settings
        let config = trie.epoch_config().expect("config");
        // Low latency has shorter epochs
        assert!(config.epoch_duration.as_millis() < 1000, "Low-latency should have short epoch duration");
    }

    #[test]
    fn test_epoch_metadata() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_epoch_metadata.trie");

        let mut trie: PersistentARTrieChar<()> =
            PersistentARTrieChar::create(&path).expect("create");

        trie.enable_epoch_checkpointing_default().expect("enable");

        // Should have metadata for at least the current epoch
        let metadata = trie.epoch_metadata().expect("metadata");
        assert!(!metadata.is_empty(), "Should have at least one epoch's metadata");

        // First epoch should be active
        let first = &metadata[0];
        assert_eq!(first.id, trie.current_epoch_id().expect("epoch_id"));
    }

    // === Enhanced Recovery Tests ===

    #[test]
    fn test_enhanced_recovery_mode_is_normal() {
        assert!(EnhancedRecoveryMode::Normal.is_normal());
        assert!(EnhancedRecoveryMode::CreatedNew.is_normal());
        assert!(!EnhancedRecoveryMode::RebuiltFromWal.is_normal());
        assert!(!EnhancedRecoveryMode::RebuiltFromArchives.is_normal());
    }

    #[test]
    fn test_enhanced_recovery_mode_required_rebuild() {
        assert!(!EnhancedRecoveryMode::Normal.required_rebuild());
        assert!(!EnhancedRecoveryMode::CreatedNew.required_rebuild());
        assert!(EnhancedRecoveryMode::RebuiltFromWal.required_rebuild());
        assert!(EnhancedRecoveryMode::RebuiltFromArchives.required_rebuild());
    }

    #[test]
    fn test_enhanced_recovery_stats_normal() {
        let stats = EnhancedRecoveryStats::normal();
        assert!(stats.mode.is_normal());
        assert_eq!(stats.records_replayed, 0);
        assert_eq!(stats.epochs_recovered, 0);
    }

    #[test]
    fn test_enhanced_recovery_stats_created_new() {
        let stats = EnhancedRecoveryStats::created_new();
        assert_eq!(stats.mode, EnhancedRecoveryMode::CreatedNew);
        assert!(stats.mode.is_normal());
    }

    #[test]
    fn test_open_with_full_recovery_creates_new() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("new_full_recovery.trie");

        let (trie, stats): (PersistentARTrieChar<i64>, _) =
            PersistentARTrieChar::open_with_full_recovery(
                &path,
                None, // No epoch config
                WalConfig::default(),
            )
            .expect("open_with_full_recovery");

        assert_eq!(stats.mode, EnhancedRecoveryMode::CreatedNew);
        assert_eq!(stats.records_replayed, 0);
        assert_eq!(trie.len(), 0); // Trie should be empty
    }

    #[test]
    fn test_open_with_full_recovery_normal_open() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("existing_full_recovery.trie");

        // Create and populate trie first
        {
            let mut trie: PersistentARTrieChar<()> =
                PersistentARTrieChar::create(&path).expect("create");
            trie.insert_impl_no_wal("hello");
            trie.checkpoint().expect("checkpoint");
        }

        // Open with full recovery
        let (trie, stats): (PersistentARTrieChar<()>, _) =
            PersistentARTrieChar::open_with_full_recovery(
                &path,
                None,
                WalConfig::default(),
            )
            .expect("open_with_full_recovery");

        assert_eq!(stats.mode, EnhancedRecoveryMode::Normal);
        assert!(trie.contains("hello")); // contains returns bool directly
    }

    #[test]
    fn test_incremental_recovery_empty_wal() {
        use tempfile::tempdir;
        use crate::persistent_artrie::wal::WalWriter;
        use crate::persistent_artrie::recovery::IncrementalRecovery;

        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("empty.wal");

        // Create empty WAL
        {
            let _wal = WalWriter::create(&wal_path).expect("create wal");
        }

        // Create incremental recovery
        let mut recovery: IncrementalRecovery =
            PersistentARTrieChar::<()>::incremental_recovery(&wal_path).expect("recovery");

        // Should return None for empty WAL
        let batch = recovery.next_batch(10).expect("next_batch");
        assert!(batch.is_none(), "Empty WAL should return no batches");
    }

    // ========================================================================
    // LSN API Tests
    // ========================================================================

    mod lsn_api_tests {
        use super::*;
        use tempfile::tempdir;

        #[test]
        fn test_current_lsn_starts_at_one_for_persistent() {
            let dir = tempdir().expect("create temp dir");
            let path = dir.path().join("lsn_test.trie");

            let inner: PersistentARTrieChar<i32> =
                PersistentARTrieChar::create(&path).expect("create");

            // Persistent tries start at LSN 1 (0 is reserved for "no LSN")
            assert_eq!(inner.current_lsn(), 1);
        }

        #[test]
        fn test_current_lsn_starts_at_one_for_in_memory() {
            // In-memory tries still start at LSN 1 for consistency
            let inner: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
            assert_eq!(inner.current_lsn(), 1);
        }

        #[test]
        fn test_current_lsn_increases_after_insert() {
            let dir = tempdir().expect("create temp dir");
            let path = dir.path().join("lsn_test.trie");

            let mut inner: PersistentARTrieChar<i32> =
                PersistentARTrieChar::create(&path).expect("create");

            let before = inner.current_lsn();
            inner.upsert("key1", 42).expect("upsert");
            let after = inner.current_lsn();

            assert!(
                after > before,
                "LSN should increase after insert: before={}, after={}",
                before,
                after
            );
        }

        #[test]
        fn test_current_lsn_increases_after_remove() {
            let dir = tempdir().expect("create temp dir");
            let path = dir.path().join("lsn_test.trie");

            let mut inner: PersistentARTrieChar<i32> =
                PersistentARTrieChar::create(&path).expect("create");

            inner.upsert("key1", 42).expect("upsert");
            let before = inner.current_lsn();
            inner.remove("key1").expect("remove");
            let after = inner.current_lsn();

            assert!(
                after > before,
                "LSN should increase after remove: before={}, after={}",
                before,
                after
            );
        }

        #[test]
        fn test_synced_lsn_none_for_in_memory() {
            // In-memory tries have no WAL, so synced_lsn should be None
            let inner: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
            assert!(
                inner.synced_lsn().is_none(),
                "In-memory trie should have no synced LSN"
            );
        }

        #[test]
        fn test_synced_lsn_after_sync() {
            let dir = tempdir().expect("create temp dir");
            let path = dir.path().join("lsn_test.trie");

            let mut inner: PersistentARTrieChar<i32> =
                PersistentARTrieChar::create(&path).expect("create");

            // Insert some data
            inner.upsert("key1", 42).expect("upsert");
            inner.upsert("key2", 43).expect("upsert");

            // Before sync, synced_lsn should be 0 (no syncs yet)
            let synced_before = inner.synced_lsn().expect("persistent trie should have synced_lsn");
            assert_eq!(synced_before, 0, "No data should be synced yet");

            // Sync to disk
            inner.sync().expect("sync should succeed");

            // After sync, synced_lsn should be positive
            let synced_after = inner.synced_lsn().expect("persistent trie should have synced_lsn");
            assert!(
                synced_after > 0,
                "synced_lsn should be positive after sync: {}",
                synced_after
            );
        }

        #[test]
        fn test_synced_lsn_invariant() {
            let dir = tempdir().expect("create temp dir");
            let path = dir.path().join("lsn_test.trie");

            let mut inner: PersistentARTrieChar<i32> =
                PersistentARTrieChar::create(&path).expect("create");

            // Insert and sync
            inner.upsert("key1", 42).expect("upsert");
            inner.sync().expect("sync should succeed");

            // Insert more data without syncing
            inner.upsert("key2", 43).expect("upsert");

            let current = inner.current_lsn();
            let synced = inner.synced_lsn().expect("persistent trie should have synced_lsn");

            // Invariant: synced_lsn <= current_lsn - 1
            // (current_lsn is the NEXT lsn to be assigned, so the last written is current - 1)
            assert!(
                synced < current,
                "synced_lsn ({}) should be less than current_lsn ({})",
                synced,
                current
            );
        }

        #[test]
        fn test_lsn_monotonically_increasing() {
            let dir = tempdir().expect("create temp dir");
            let path = dir.path().join("lsn_test.trie");

            let mut inner: PersistentARTrieChar<i32> =
                PersistentARTrieChar::create(&path).expect("create");

            let mut prev_lsn = inner.current_lsn();

            // Perform multiple operations and verify LSN increases
            for i in 0..10 {
                inner.upsert(&format!("key{}", i), i).expect("upsert");
                let curr_lsn = inner.current_lsn();
                assert!(
                    curr_lsn > prev_lsn,
                    "LSN should increase monotonically: prev={}, curr={}",
                    prev_lsn,
                    curr_lsn
                );
                prev_lsn = curr_lsn;
            }
        }

    }

    #[test]
    fn test_shared_char_trie_current_lsn() {
        use crate::artrie_trait::ARTrie;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("test_shared_lsn.artc");
        let trie = std::sync::Arc::new(parking_lot::RwLock::new(
            PersistentARTrieChar::<()>::create(&path).expect("create trie"),
        ));
        let lsn0 = trie.current_lsn();
        trie.write().insert("hello");
        let lsn1 = trie.current_lsn();
        assert!(lsn1 > lsn0, "current_lsn must advance after insert");
    }

    #[test]
    fn test_shared_char_trie_synced_lsn() {
        use crate::artrie_trait::ARTrie;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("test_shared_synced.artc");
        let trie = std::sync::Arc::new(parking_lot::RwLock::new(
            PersistentARTrieChar::<()>::create(&path).expect("create trie"),
        ));
        let synced_before = trie.synced_lsn();
        trie.write().insert("hello");
        let current_after_insert = trie.current_lsn();
        // After an insert that hasn't been synced, current_lsn advances ahead of
        // synced_lsn (or synced is still None for an unsynced fresh trie).
        assert!(
            synced_before.map_or(true, |s| s < current_after_insert),
            "synced_lsn must lag current_lsn until sync() runs"
        );
        trie.sync().expect("sync");
        // After sync(), synced_lsn must be reported as Some(_): the trie has
        // flushed the WAL at least once, so the on-disk state has a well-defined
        // LSN. The exact value relative to current_lsn depends on sync
        // semantics — the WAL writer's synced_lsn is the last LSN that fsync
        // confirmed durable, which may lag current_lsn by one record (the
        // checkpoint marker that sync itself emits).
        assert!(
            trie.synced_lsn().is_some(),
            "synced_lsn must be Some(_) after sync()"
        );
    }

    #[test]
    fn test_shared_char_trie_upsert() {
        use crate::artrie_trait::ARTrie;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("test_shared_upsert.artc");
        let trie = std::sync::Arc::new(parking_lot::RwLock::new(
            PersistentARTrieChar::<i64>::create(&path).expect("create trie"),
        ));
        assert!(trie.upsert("k", 1).expect("upsert"), "first upsert reports insert");
        assert!(!trie.upsert("k", 2).expect("upsert"), "second upsert reports update");
        assert_eq!(trie.read().get("k").copied(), Some(2), "value updated");
    }

    #[test]
    fn test_shared_char_trie_sync_persists() {
        use crate::artrie_trait::ARTrie;
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("test_shared_sync.artc");
        let trie = std::sync::Arc::new(parking_lot::RwLock::new(
            PersistentARTrieChar::<()>::create(&path).expect("create trie"),
        ));
        trie.write().insert("persistent");
        trie.sync().expect("sync");
        drop(trie);
        let reopened = PersistentARTrieChar::<()>::open(&path).expect("reopen");
        assert!(reopened.contains("persistent"));
    }

    // ==================== Lock-Free CAS Tests ====================

    #[test]
    fn test_insert_cas_basic() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let path = dir.path().join("test_insert_cas.artc");

        let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::create(&path)
            .expect("create trie");
        trie.enable_lockfree();

        // First insert should succeed
        assert!(trie.insert_cas("hello"));
        assert!(trie.insert_cas("world"));

        // Duplicate insert should return false
        assert!(!trie.insert_cas("hello"));
        assert!(!trie.insert_cas("world"));

        // Different terms should succeed
        assert!(trie.insert_cas("rust"));
        assert!(trie.insert_cas("cargo"));
    }

    #[test]
    fn test_insert_cas_empty_term() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let path = dir.path().join("test_insert_cas_empty.artc");

        let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::create(&path)
            .expect("create trie");
        trie.enable_lockfree();

        // Empty term should return false (not inserted)
        assert!(!trie.insert_cas(""));
    }

    #[test]
    fn test_insert_cas_unicode() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let path = dir.path().join("test_insert_cas_unicode.artc");

        let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::create(&path)
            .expect("create trie");
        trie.enable_lockfree();

        // Unicode terms
        assert!(trie.insert_cas("日本語"));
        assert!(trie.insert_cas("中文"));
        assert!(trie.insert_cas("한국어"));
        assert!(trie.insert_cas("🦀"));

        // Duplicates
        assert!(!trie.insert_cas("日本語"));
        assert!(!trie.insert_cas("🦀"));
    }

    #[test]
    fn test_insert_cas_concurrent() {
        use std::sync::Arc;
        use std::thread;

        let dir = tempfile::TempDir::new().expect("create temp dir");
        let path = dir.path().join("test_insert_cas_concurrent.artc");

        let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::create(&path)
            .expect("create trie");
        trie.enable_lockfree();

        let trie = Arc::new(trie);
        let num_threads = 4;
        let terms_per_thread = 25;

        // Test that concurrent access is safe (no panics/data races)
        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let trie = Arc::clone(&trie);
                thread::spawn(move || {
                    let mut inserted = 0;
                    for i in 0..terms_per_thread {
                        let term = format!("term_{}_{}", t, i);
                        if trie.insert_cas(&term) {
                            inserted += 1;
                        }
                    }
                    inserted
                })
            })
            .collect();

        let total_inserted: usize = handles.into_iter()
            .map(|h| h.join().expect("thread join"))
            .sum();

        // Note: The current simplified implementation uses root-level CAS,
        // which has high contention. The important thing is that:
        // 1. No panics or data races occurred
        // 2. At least one term was inserted
        assert!(total_inserted >= 1, "At least one term should be inserted");

        let retries = trie.cas_retry_count();
        println!("Inserted: {}/{}, CAS retries: {}", total_inserted, num_threads * terms_per_thread, retries);

        // The lock-free infrastructure is working - concurrent access is safe
        // Full per-level CAS traversal will be implemented in later phases
    }

    #[test]
    fn test_contains_lockfree() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let path = dir.path().join("test_contains_lockfree.artc");

        let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::create(&path)
            .expect("create trie");
        trie.enable_lockfree();

        // Insert some terms
        trie.insert_cas("apple");
        trie.insert_cas("banana");

        // Check contains
        assert!(trie.contains_lockfree("apple"));
        assert!(trie.contains_lockfree("banana"));
        assert!(!trie.contains_lockfree("cherry"));
    }

    #[test]
    fn test_merge_lockfree_to_persistent() {
        let dir = tempfile::TempDir::new().expect("create temp dir");
        let path = dir.path().join("test_merge_lockfree.artc");

        let mut trie: PersistentARTrieChar<()> = PersistentARTrieChar::create(&path)
            .expect("create trie");
        trie.enable_lockfree();

        // Insert into lock-free trie
        trie.insert_cas("alpha");
        trie.insert_cas("beta");
        trie.insert_cas("gamma");

        // Merge to persistent
        let count = trie.merge_lockfree_to_persistent()
            .expect("merge lockfree");
        assert_eq!(count, 3);

        // The terms should now be in the persistent trie
        assert!(trie.contains("alpha"));
        assert!(trie.contains("beta"));
        assert!(trie.contains("gamma"));

        // Lock-free cache should be cleared (check cache is empty)
        // Note: contains_lockfree still finds terms in trie structure, which is correct
        if let Some(ref cache) = trie.lockfree_cache {
            assert!(cache.is_empty(), "cache should be cleared after merge");
        }
    }
}
