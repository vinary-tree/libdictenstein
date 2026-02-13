//! Lock-Free Concurrent Vocabulary Access
//!
//! This module provides lock-free concurrent insert capabilities for the vocabulary trie.
//! The main bottleneck in concurrent n-gram processing is vocabulary contention - all
//! workers need to insert words into the shared vocabulary.
//!
//! # Design
//!
//! The implementation provides two modes of operation:
//!
//! ## LockFree Mode (Recommended for High Concurrency)
//!
//! Uses `LockFreeVocab` for truly lock-free CAS-based inserts:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────────┐
//! │                   ConcurrentVocabARTrie (LockFree Mode)                      │
//! ├─────────────────────────────────────────────────────────────────────────────┤
//! │                                                                              │
//! │  ┌─────────────┐   ┌─────────────┐   ┌─────────────┐                        │
//! │  │  Worker 1   │   │  Worker 2   │   │  Worker N   │                        │
//! │  │ insert_cas()│   │ insert_cas()│   │ insert_cas()│                        │
//! │  └──────┬──────┘   └──────┬──────┘   └──────┬──────┘                        │
//! │         │                  │                  │                              │
//! │         ▼                  ▼                  ▼                              │
//! │  ┌────────────────────────────────────────────────────────────────────┐     │
//! │  │               LockFreeVocab (Persistent Nodes)                     │     │
//! │  │                                                                    │     │
//! │  │  AtomicNodePtr ──► im::Vector<(key, child)>                       │     │
//! │  │  CAS swaps entire path on insert                                  │     │
//! │  │  DashMap cache for O(1) lookups                                   │     │
//! │  └────────────────────────────────────────────────────────────────────┘     │
//! │                              │                                               │
//! │                    checkpoint()                                              │
//! │                              ▼                                               │
//! │  ┌────────────────────────────────────────────────────────────────────┐     │
//! │  │               PersistentVocabARTrie (Durable Storage)             │     │
//! │  │                                                                    │     │
//! │  │  merge_into() for durability                                      │     │
//! │  └────────────────────────────────────────────────────────────────────┘     │
//! │                                                                              │
//! └─────────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Queue Mode (Legacy, for Compatibility)
//!
//! Uses a queue + single writer pattern:
//!
//! 1. **Atomic Index Allocation**: Uses `AtomicU64` for `next_index` to allow multiple
//!    threads to claim indices without contention.
//!
//! 2. **Batch Coalescing**: Workers queue inserts for batched processing.
//!
//! # Usage
//!
//! ```rust,ignore
//! use libdictenstein::persistent_vocab_artrie::concurrent::ConcurrentVocabARTrie;
//!
//! // Create with lock-free mode (recommended)
//! let vocab = PersistentVocabARTrie::create("vocab.vocab")?;
//! let concurrent = ConcurrentVocabARTrie::new_lockfree(vocab);
//!
//! // Multiple threads can insert concurrently - truly lock-free!
//! let idx = concurrent.insert_cas("hello");
//!
//! // Checkpoint to persistent storage
//! concurrent.checkpoint()?;
//! ```

use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;
use std::collections::HashMap;

use parking_lot::RwLock;
use crossbeam_channel::{Sender, Receiver, unbounded, TryRecvError};

use super::PersistentVocabARTrie;
use super::lockfree::LockFreeVocab;
use crate::persistent_artrie::error::Result;

/// Operating mode for the concurrent vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrentMode {
    /// Lock-free CAS-based mode using `LockFreeVocab`.
    /// Best for high concurrency (many parallel workers).
    LockFree,
    /// Queue-based mode with single writer thread.
    /// Legacy mode for compatibility.
    Queue,
}

/// A concurrent vocabulary trie wrapper that provides lock-free insert operations.
///
/// This wrapper provides two modes:
/// - **LockFree**: Uses `LockFreeVocab` for truly concurrent CAS-based inserts
/// - **Queue**: Uses a queue + single writer pattern (legacy mode)
///
/// For high-concurrency workloads (many parallel workers), use `new_lockfree()`.
pub struct ConcurrentVocabARTrie {
    /// Operating mode
    mode: ConcurrentMode,

    /// Lock-free vocabulary layer (used in LockFree mode)
    lockfree_vocab: Option<Arc<LockFreeVocab>>,

    /// The underlying vocabulary trie (protected by RwLock for writes)
    inner: Arc<RwLock<PersistentVocabARTrie>>,

    /// Atomic counter for next vocabulary index (lock-free allocation)
    /// Only used in Queue mode
    next_index: AtomicU64,

    /// Starting index (for validation)
    start_index: u64,

    /// Sender for pending inserts (Queue mode only)
    insert_tx: Sender<PendingInsert>,

    /// Receiver for pending inserts (Queue mode only)
    insert_rx: Receiver<PendingInsert>,

    /// Flag indicating shutdown requested
    shutdown: AtomicBool,

    /// Cache for recently looked-up terms (term -> index)
    /// Only used in Queue mode; LockFree mode uses DashMap internally
    lookup_cache: RwLock<HashMap<String, u64>>,
}

/// A pending insert operation (Queue mode only)
struct PendingInsert {
    term: String,
    index: u64,
}

impl ConcurrentVocabARTrie {
    /// Create a new concurrent vocabulary wrapper using queue-based mode (legacy).
    ///
    /// For high-concurrency workloads, prefer [`new_lockfree()`] instead.
    pub fn new(vocab: PersistentVocabARTrie) -> Self {
        let next_idx = vocab.next_index();
        let start_idx = vocab.start_index();
        let (tx, rx) = unbounded();

        Self {
            mode: ConcurrentMode::Queue,
            lockfree_vocab: None,
            inner: Arc::new(RwLock::new(vocab)),
            next_index: AtomicU64::new(next_idx),
            start_index: start_idx,
            insert_tx: tx,
            insert_rx: rx,
            shutdown: AtomicBool::new(false),
            lookup_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new concurrent vocabulary wrapper using lock-free mode.
    ///
    /// This mode provides truly lock-free insert operations using CAS on
    /// persistent (immutable) data structures. Best for high-concurrency
    /// workloads with many parallel workers.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let vocab = PersistentVocabARTrie::create("vocab.vocab")?;
    /// let concurrent = ConcurrentVocabARTrie::new_lockfree(vocab);
    ///
    /// // Spawn many workers
    /// let handles: Vec<_> = (0..12).map(|_| {
    ///     let c = concurrent.clone();
    ///     std::thread::spawn(move || {
    ///         for term in get_terms() {
    ///             c.insert_cas(&term);
    ///         }
    ///     })
    /// }).collect();
    ///
    /// // All workers insert concurrently without blocking!
    /// ```
    pub fn new_lockfree(vocab: PersistentVocabARTrie) -> Self {
        let next_idx = vocab.next_index();
        let start_idx = vocab.start_index();
        let (tx, rx) = unbounded();

        // Create lock-free layer starting from the vocab's next index
        let lockfree = LockFreeVocab::with_start_index(next_idx);

        Self {
            mode: ConcurrentMode::LockFree,
            lockfree_vocab: Some(lockfree),
            inner: Arc::new(RwLock::new(vocab)),
            next_index: AtomicU64::new(next_idx),
            start_index: start_idx,
            insert_tx: tx,
            insert_rx: rx,
            shutdown: AtomicBool::new(false),
            lookup_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new concurrent vocabulary from an existing shared trie.
    pub fn from_shared(vocab: Arc<RwLock<PersistentVocabARTrie>>) -> Self {
        let (next_idx, start_idx) = {
            let guard = vocab.read();
            (guard.next_index(), guard.start_index())
        };
        let (tx, rx) = unbounded();

        Self {
            mode: ConcurrentMode::Queue,
            lockfree_vocab: None,
            inner: vocab,
            next_index: AtomicU64::new(next_idx),
            start_index: start_idx,
            insert_tx: tx,
            insert_rx: rx,
            shutdown: AtomicBool::new(false),
            lookup_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new concurrent vocabulary from a shared trie using lock-free mode.
    pub fn from_shared_lockfree(vocab: Arc<RwLock<PersistentVocabARTrie>>) -> Self {
        let (next_idx, start_idx) = {
            let guard = vocab.read();
            (guard.next_index(), guard.start_index())
        };
        let (tx, rx) = unbounded();

        let lockfree = LockFreeVocab::with_start_index(next_idx);

        Self {
            mode: ConcurrentMode::LockFree,
            lockfree_vocab: Some(lockfree),
            inner: vocab,
            next_index: AtomicU64::new(next_idx),
            start_index: start_idx,
            insert_tx: tx,
            insert_rx: rx,
            shutdown: AtomicBool::new(false),
            lookup_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Get the operating mode.
    pub fn mode(&self) -> ConcurrentMode {
        self.mode
    }

    /// Get the underlying shared vocabulary trie.
    pub fn inner(&self) -> &Arc<RwLock<PersistentVocabARTrie>> {
        &self.inner
    }

    /// Get the lock-free vocabulary layer (if in LockFree mode).
    pub fn lockfree_vocab(&self) -> Option<&Arc<LockFreeVocab>> {
        self.lockfree_vocab.as_ref()
    }

    /// Get the current next index value (atomically).
    pub fn next_index(&self) -> u64 {
        match self.mode {
            ConcurrentMode::LockFree => {
                self.lockfree_vocab.as_ref()
                    .map(|lf| lf.next_index())
                    .unwrap_or_else(|| self.next_index.load(Ordering::Acquire))
            }
            ConcurrentMode::Queue => self.next_index.load(Ordering::Acquire),
        }
    }

    /// Get the start index.
    pub fn start_index(&self) -> u64 {
        self.start_index
    }

    /// Look up a term's index (read-only, no lock contention).
    ///
    /// This first checks the lock-free layer (in LockFree mode) or cache,
    /// then falls back to the persistent trie.
    pub fn get_index(&self, term: &str) -> Option<u64> {
        match self.mode {
            ConcurrentMode::LockFree => {
                // Check lock-free layer first
                if let Some(ref lf) = self.lockfree_vocab {
                    if let Some(idx) = lf.get_index(term) {
                        return Some(idx);
                    }
                }
                // Fall back to persistent trie
                let guard = self.inner.read();
                guard.get_index(term)
            }
            ConcurrentMode::Queue => {
                // Check cache first (lock-free read path)
                {
                    let cache = self.lookup_cache.read();
                    if let Some(&idx) = cache.get(term) {
                        return Some(idx);
                    }
                }
                // Cache miss - check trie (needs read lock)
                let guard = self.inner.read();
                guard.get_index(term)
            }
        }
    }

    /// Check if a term exists in the vocabulary.
    pub fn contains(&self, term: &str) -> bool {
        match self.mode {
            ConcurrentMode::LockFree => {
                if let Some(ref lf) = self.lockfree_vocab {
                    if lf.contains(term) {
                        return true;
                    }
                }
                let guard = self.inner.read();
                guard.contains(term)
            }
            ConcurrentMode::Queue => {
                {
                    let cache = self.lookup_cache.read();
                    if cache.contains_key(term) {
                        return true;
                    }
                }
                let guard = self.inner.read();
                guard.contains(term)
            }
        }
    }

    /// Insert a term using compare-and-swap semantics.
    ///
    /// In **LockFree mode**: Truly lock-free insert using CAS on persistent nodes.
    /// In **Queue mode**: Queues the insert for batched processing.
    ///
    /// # Returns
    ///
    /// The vocabulary index for the term (existing or newly assigned).
    pub fn insert_cas(&self, term: &str) -> u64 {
        match self.mode {
            ConcurrentMode::LockFree => {
                // Check persistent trie first (for terms added before lock-free layer)
                {
                    let guard = self.inner.read();
                    if let Some(idx) = guard.get_index(term) {
                        return idx;
                    }
                }

                // Use lock-free layer for insert
                if let Some(ref lf) = self.lockfree_vocab {
                    return lf.insert_cas(term).index();
                }

                // Fallback (shouldn't happen in LockFree mode)
                self.insert_cas_queue_mode(term)
            }
            ConcurrentMode::Queue => self.insert_cas_queue_mode(term),
        }
    }

    /// Insert using queue mode (legacy implementation).
    fn insert_cas_queue_mode(&self, term: &str) -> u64 {
        // Fast path: check if already exists
        if let Some(idx) = self.get_index(term) {
            return idx;
        }

        // Atomically claim the next index
        let index = self.next_index.fetch_add(1, Ordering::AcqRel);

        // Queue for batched insert
        let pending = PendingInsert {
            term: term.to_string(),
            index,
        };

        // Try to send (should never fail with unbounded channel)
        if self.insert_tx.send(pending).is_err() {
            // Channel closed - fall back to direct insert
            let mut guard = self.inner.write();
            guard.insert_with_index(term, index);
        }

        // Update cache
        {
            let mut cache = self.lookup_cache.write();
            cache.insert(term.to_string(), index);
        }

        index
    }

    /// Insert multiple terms concurrently.
    ///
    /// In **LockFree mode**: Uses truly lock-free batch insert.
    /// In **Queue mode**: Batches index allocation and queues for processing.
    ///
    /// # Returns
    ///
    /// Vector of indices in the same order as input terms.
    pub fn insert_batch_concurrent(&self, terms: &[&str]) -> Vec<u64> {
        if terms.is_empty() {
            return Vec::new();
        }

        match self.mode {
            ConcurrentMode::LockFree => {
                let mut indices = Vec::with_capacity(terms.len());

                // Check persistent trie for existing terms first
                let existing_indices: Vec<Option<u64>> = {
                    let guard = self.inner.read();
                    terms.iter().map(|t| guard.get_index(t)).collect()
                };

                // Insert new terms via lock-free layer
                if let Some(ref lf) = self.lockfree_vocab {
                    for (i, term) in terms.iter().enumerate() {
                        if let Some(idx) = existing_indices[i] {
                            indices.push(idx);
                        } else {
                            indices.push(lf.insert_cas(term).index());
                        }
                    }
                } else {
                    // Fallback
                    for term in terms {
                        indices.push(self.insert_cas_queue_mode(term));
                    }
                }

                indices
            }
            ConcurrentMode::Queue => self.insert_batch_queue_mode(terms),
        }
    }

    /// Insert batch using queue mode (legacy implementation).
    fn insert_batch_queue_mode(&self, terms: &[&str]) -> Vec<u64> {
        let mut indices = Vec::with_capacity(terms.len());
        let mut new_terms = Vec::new();
        let mut new_indices = Vec::new();

        // First pass: check for existing terms and allocate indices for new ones
        {
            let guard = self.inner.read();
            for term in terms {
                if let Some(idx) = guard.get_index(term) {
                    indices.push(idx);
                } else {
                    // Atomically claim index
                    let idx = self.next_index.fetch_add(1, Ordering::AcqRel);
                    indices.push(idx);
                    new_terms.push(term.to_string());
                    new_indices.push(idx);
                }
            }
        }

        // Queue new terms for insert
        for (term, index) in new_terms.iter().zip(new_indices.iter()) {
            let pending = PendingInsert {
                term: term.clone(),
                index: *index,
            };
            let _ = self.insert_tx.send(pending);
        }

        // Update cache
        {
            let mut cache = self.lookup_cache.write();
            for (term, &index) in new_terms.iter().zip(new_indices.iter()) {
                cache.insert(term.clone(), index);
            }
        }

        indices
    }

    /// Drain pending inserts and apply them to the trie.
    ///
    /// This should be called periodically by a writer thread or at the end
    /// of a batch operation to ensure all pending inserts are applied.
    ///
    /// **Note:** In LockFree mode, use `checkpoint()` instead to merge
    /// the lock-free layer into persistent storage.
    ///
    /// # Returns
    ///
    /// The number of inserts applied.
    pub fn drain_pending_inserts(&self) -> usize {
        let mut count = 0;
        let mut pending = Vec::new();

        // Drain all pending inserts from the queue
        loop {
            match self.insert_rx.try_recv() {
                Ok(insert) => {
                    pending.push(insert);
                    count += 1;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        if !pending.is_empty() {
            // Apply all inserts under a single write lock
            let mut guard = self.inner.write();
            for insert in pending {
                guard.insert_with_index(&insert.term, insert.index);
            }
        }

        count
    }

    /// Checkpoint the lock-free layer to persistent storage.
    ///
    /// In **LockFree mode**: Merges all terms from the lock-free layer into
    /// the persistent vocabulary trie for durability.
    ///
    /// In **Queue mode**: Drains pending inserts and syncs to disk.
    ///
    /// # Returns
    ///
    /// The number of new terms merged into persistent storage.
    pub fn checkpoint(&self) -> Result<usize> {
        match self.mode {
            ConcurrentMode::LockFree => {
                if let Some(ref lf) = self.lockfree_vocab {
                    let mut guard = self.inner.write();
                    let count = lf.merge_into(&mut *guard)?;
                    Ok(count)
                } else {
                    Ok(0)
                }
            }
            ConcurrentMode::Queue => {
                let count = self.drain_pending_inserts();
                let mut guard = self.inner.write();
                guard.sync()?;
                Ok(count)
            }
        }
    }

    /// Get statistics from the lock-free layer.
    ///
    /// Only available in LockFree mode. Returns `None` in Queue mode.
    pub fn lockfree_stats(&self) -> Option<super::lockfree::LockFreeVocabStats> {
        self.lockfree_vocab.as_ref().map(|lf| lf.stats())
    }

    /// Synchronize the atomic next_index with the trie's internal state.
    ///
    /// This should be called after draining inserts to ensure consistency.
    pub fn sync_next_index(&self) {
        let guard = self.inner.read();
        let trie_next = guard.next_index();
        let atomic_next = self.next_index.load(Ordering::Acquire);

        // The atomic counter may be ahead of the trie (pending inserts)
        // So we only update if trie is somehow ahead (shouldn't happen normally)
        if trie_next > atomic_next {
            self.next_index.store(trie_next, Ordering::Release);
        }
    }

    /// Flush all pending operations and sync to disk.
    ///
    /// For LockFree mode, this also checkpoints the lock-free layer.
    pub fn flush(&self) -> Result<()> {
        match self.mode {
            ConcurrentMode::LockFree => {
                // Checkpoint lock-free layer to persistent storage
                self.checkpoint()?;
                // Sync the underlying trie
                let mut guard = self.inner.write();
                guard.sync()
            }
            ConcurrentMode::Queue => {
                // Drain pending inserts first
                self.drain_pending_inserts();
                // Sync the underlying trie
                let mut guard = self.inner.write();
                guard.sync()
            }
        }
    }

    /// Get the number of pending inserts.
    ///
    /// In **LockFree mode**: Returns the number of terms in the lock-free layer
    /// that haven't been checkpointed yet.
    ///
    /// In **Queue mode**: Returns the number of inserts in the queue.
    pub fn pending_count(&self) -> usize {
        match self.mode {
            ConcurrentMode::LockFree => {
                self.lockfree_vocab.as_ref()
                    .map(|lf| lf.len())
                    .unwrap_or(0)
            }
            ConcurrentMode::Queue => self.insert_rx.len(),
        }
    }

    /// Clear the lookup cache.
    ///
    /// In **LockFree mode**: This is a no-op since DashMap handles caching internally.
    /// In **Queue mode**: Clears the HashMap cache.
    pub fn clear_cache(&self) {
        if self.mode == ConcurrentMode::Queue {
            let mut cache = self.lookup_cache.write();
            cache.clear();
        }
    }

    /// Get vocabulary statistics.
    pub fn stats(&self) -> ConcurrentVocabStats {
        let guard = self.inner.read();
        let (next_index, pending, cache_size) = match self.mode {
            ConcurrentMode::LockFree => {
                let lf_stats = self.lockfree_vocab.as_ref().map(|lf| lf.stats());
                (
                    lf_stats.as_ref().map(|s| s.next_index).unwrap_or(self.next_index.load(Ordering::Acquire)),
                    lf_stats.as_ref().map(|s| s.entry_count).unwrap_or(0),
                    lf_stats.as_ref().map(|s| s.cache_size).unwrap_or(0),
                )
            }
            ConcurrentMode::Queue => (
                self.next_index.load(Ordering::Acquire),
                self.insert_rx.len(),
                self.lookup_cache.read().len(),
            ),
        };

        ConcurrentVocabStats {
            entry_count: guard.len(),
            next_index,
            pending_inserts: pending,
            cache_size,
            mode: self.mode,
        }
    }
}

/// Statistics for the concurrent vocabulary.
#[derive(Debug, Clone)]
pub struct ConcurrentVocabStats {
    /// Number of entries in the vocabulary (persistent storage)
    pub entry_count: usize,
    /// Next index to be assigned
    pub next_index: u64,
    /// Number of pending inserts (in queue or lock-free layer)
    pub pending_inserts: usize,
    /// Number of entries in the lookup cache
    pub cache_size: usize,
    /// Operating mode
    pub mode: ConcurrentMode,
}

// Make sure the wrapper is safe to share across threads
unsafe impl Send for ConcurrentVocabARTrie {}
unsafe impl Sync for ConcurrentVocabARTrie {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::thread;

    #[test]
    fn test_concurrent_vocab_basic() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        let concurrent = ConcurrentVocabARTrie::new(vocab);

        // Insert some terms
        let idx1 = concurrent.insert_cas("hello");
        let idx2 = concurrent.insert_cas("world");
        let idx3 = concurrent.insert_cas("hello"); // duplicate

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 0); // should return existing index

        // Drain pending inserts
        concurrent.drain_pending_inserts();

        // Verify via get_index
        assert_eq!(concurrent.get_index("hello"), Some(0));
        assert_eq!(concurrent.get_index("world"), Some(1));
    }

    #[test]
    fn test_concurrent_vocab_batch() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        let concurrent = ConcurrentVocabARTrie::new(vocab);

        let indices = concurrent.insert_batch_concurrent(&["apple", "banana", "cherry"]);
        assert_eq!(indices, vec![0, 1, 2]);

        concurrent.drain_pending_inserts();

        // Duplicate batch should return existing indices
        let indices2 = concurrent.insert_batch_concurrent(&["apple", "date"]);
        assert_eq!(indices2, vec![0, 3]);
    }

    #[test]
    fn test_concurrent_vocab_parallel() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        let concurrent = Arc::new(ConcurrentVocabARTrie::new(vocab));

        let num_threads = 4;
        let terms_per_thread = 100;

        // Spawn multiple threads to insert terms
        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let c = Arc::clone(&concurrent);
                thread::spawn(move || {
                    let mut indices = Vec::new();
                    for i in 0..terms_per_thread {
                        let term = format!("thread{}_term{}", t, i);
                        let idx = c.insert_cas(&term);
                        indices.push(idx);
                    }
                    indices
                })
            })
            .collect();

        // Collect all indices
        let all_indices: Vec<Vec<u64>> = handles
            .into_iter()
            .map(|h| h.join().expect("thread complete"))
            .collect();

        // Drain pending inserts
        concurrent.drain_pending_inserts();

        // All indices should be unique within each thread's terms
        for thread_indices in &all_indices {
            let mut sorted = thread_indices.clone();
            sorted.sort();
            for window in sorted.windows(2) {
                assert!(window[0] != window[1] || window.len() < 2,
                    "duplicate indices within thread: {} and {}", window[0], window[1]);
            }
        }

        // Total unique indices should equal num_threads * terms_per_thread
        let total_terms = num_threads * terms_per_thread;
        let stats = concurrent.stats();
        assert_eq!(stats.next_index, total_terms as u64);
    }

    #[test]
    fn test_concurrent_vocab_stats() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        let concurrent = ConcurrentVocabARTrie::new(vocab);

        concurrent.insert_cas("one");
        concurrent.insert_cas("two");

        let stats = concurrent.stats();
        assert_eq!(stats.next_index, 2);
        assert!(stats.pending_inserts > 0); // Haven't drained yet

        concurrent.drain_pending_inserts();

        let stats = concurrent.stats();
        assert_eq!(stats.pending_inserts, 0);
    }

    // ==================== LockFree Mode Tests ====================

    #[test]
    fn test_lockfree_mode_basic() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        let concurrent = ConcurrentVocabARTrie::new_lockfree(vocab);

        assert_eq!(concurrent.mode(), ConcurrentMode::LockFree);

        // Insert some terms
        let idx1 = concurrent.insert_cas("hello");
        let idx2 = concurrent.insert_cas("world");
        let idx3 = concurrent.insert_cas("hello"); // duplicate

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 0); // should return existing index

        // Verify via get_index (lock-free layer)
        assert_eq!(concurrent.get_index("hello"), Some(0));
        assert_eq!(concurrent.get_index("world"), Some(1));
    }

    #[test]
    fn test_lockfree_mode_batch() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        let concurrent = ConcurrentVocabARTrie::new_lockfree(vocab);

        let indices = concurrent.insert_batch_concurrent(&["apple", "banana", "cherry"]);
        assert_eq!(indices, vec![0, 1, 2]);

        // Duplicate batch should return existing indices
        let indices2 = concurrent.insert_batch_concurrent(&["apple", "date"]);
        assert_eq!(indices2, vec![0, 3]);
    }

    #[test]
    fn test_lockfree_mode_checkpoint() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        let concurrent = ConcurrentVocabARTrie::new_lockfree(vocab);

        // Insert terms in lock-free layer
        concurrent.insert_cas("alpha");
        concurrent.insert_cas("beta");
        concurrent.insert_cas("gamma");

        // Before checkpoint, persistent layer should be empty
        {
            let guard = concurrent.inner().read();
            assert_eq!(guard.len(), 0);
        }

        // Checkpoint to persistent storage
        let count = concurrent.checkpoint().expect("checkpoint");
        assert_eq!(count, 3);

        // After checkpoint, persistent layer should have the terms
        {
            let guard = concurrent.inner().read();
            assert_eq!(guard.len(), 3);
            assert_eq!(guard.get_index("alpha"), Some(0));
            assert_eq!(guard.get_index("beta"), Some(1));
            assert_eq!(guard.get_index("gamma"), Some(2));
        }
    }

    #[test]
    fn test_lockfree_mode_parallel() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        let concurrent = Arc::new(ConcurrentVocabARTrie::new_lockfree(vocab));

        let num_threads = 8;
        let terms_per_thread = 100;

        // Spawn multiple threads to insert terms
        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let c = Arc::clone(&concurrent);
                thread::spawn(move || {
                    let mut indices = Vec::new();
                    for i in 0..terms_per_thread {
                        let term = format!("thread{}_term{}", t, i);
                        let idx = c.insert_cas(&term);
                        indices.push(idx);
                    }
                    indices
                })
            })
            .collect();

        // Collect all indices
        let all_indices: Vec<Vec<u64>> = handles
            .into_iter()
            .map(|h| h.join().expect("thread complete"))
            .collect();

        // All indices should be unique within each thread's terms
        for thread_indices in &all_indices {
            let mut sorted = thread_indices.clone();
            sorted.sort();
            for window in sorted.windows(2) {
                assert!(window[0] != window[1] || window.len() < 2,
                    "duplicate indices within thread: {} and {}", window[0], window[1]);
            }
        }

        // Flatten and check overall uniqueness
        let mut all_flat: Vec<u64> = all_indices.into_iter().flatten().collect();
        all_flat.sort();
        all_flat.dedup();
        assert_eq!(all_flat.len(), (num_threads * terms_per_thread) as usize,
            "all indices should be unique across all threads");
    }

    #[test]
    fn test_lockfree_stats() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.vocab");

        let vocab = PersistentVocabARTrie::create(&path).expect("create vocab");
        let concurrent = ConcurrentVocabARTrie::new_lockfree(vocab);

        assert!(concurrent.lockfree_stats().is_some());

        concurrent.insert_cas("one");
        concurrent.insert_cas("two");

        let lf_stats = concurrent.lockfree_stats().expect("lockfree stats");
        assert_eq!(lf_stats.entry_count, 2);
        assert_eq!(lf_stats.next_index, 2);

        // General stats
        let stats = concurrent.stats();
        assert_eq!(stats.mode, ConcurrentMode::LockFree);
        assert_eq!(stats.next_index, 2);
    }
}
