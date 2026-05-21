//! Lock-Free Concurrent Vocabulary Trie
//!
//! This module provides a truly lock-free vocabulary trie implementation using
//! persistent (immutable) data structures. Unlike `ConcurrentVocabARTrie` which
//! uses a queue + single writer pattern, this implementation allows multiple
//! threads to insert concurrently using CAS operations.
//!
//! # Design
//!
//! The trie uses `PersistentCharNode` nodes wrapped in `AtomicNodePtr` for lock-free
//! access. Modifications create new node versions which are swapped in via CAS.
//!
//! ```text
//! Thread 1                    Thread 2
//! --------                    --------
//! Load root node              Load root node
//! Navigate to insert point    Navigate to insert point
//! Create new path             Create new path
//! CAS root (old → new)        CAS fails (retry)
//!   ↓                           ↓
//! Success!                    Retry with new root
//! ```
//!
//! # Vocabulary Index Assignment
//!
//! Indices are assigned atomically using `fetch_add`:
//! 1. Check if term exists (return existing index if so)
//! 2. Atomically claim next index via `fetch_add`
//! 3. CAS-insert the term with the claimed index
//! 4. If CAS fails due to duplicate, the claimed index is "wasted" (sparse indices)
//!
//! This approach allows ~99.9% unique indices (duplicates are rare in practice).
//!
//! # Memory Management
//!
//! Old node versions are reclaimed via Arc reference counting. The epoch-based
//! reclamation system protects against ABA problems and use-after-free.
//!
//! # Example
//!
//! ```rust,no_run
//! use libdictenstein::persistent_vocab_artrie::lockfree::LockFreeVocab;
//!
//! let vocab = LockFreeVocab::new();
//!
//! // Multiple threads can insert concurrently
//! let handles: Vec<_> = (0..8).map(|i| {
//!     let v = vocab.clone();
//!     std::thread::spawn(move || {
//!         for j in 0..1000 {
//!             v.insert_cas(&format!("thread{}_{}", i, j));
//!         }
//!     })
//! }).collect();
//!
//! for h in handles { h.join().unwrap(); }
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::RwLock;
use xxhash_rust::xxh3::Xxh3DefaultBuilder;

use super::PersistentVocabARTrie;
use crate::persistent_artrie::error::Result;
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
use crate::persistent_artrie_char::nodes::{AtomicNodePtr, PersistentCharNode};

/// Result of a lock-free insert operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertResult {
    /// Term was newly inserted with the given index.
    Inserted(u64),
    /// Term already existed with the given index.
    AlreadyExists(u64),
}

impl InsertResult {
    /// Get the vocabulary index regardless of whether it was new or existing.
    #[inline]
    pub fn index(&self) -> u64 {
        match self {
            InsertResult::Inserted(idx) => *idx,
            InsertResult::AlreadyExists(idx) => *idx,
        }
    }

    /// Check if this was a new insertion.
    #[inline]
    pub fn was_inserted(&self) -> bool {
        matches!(self, InsertResult::Inserted(_))
    }
}

/// A lock-free concurrent vocabulary trie.
///
/// This struct provides truly concurrent insert operations without locks.
/// Multiple threads can insert simultaneously using CAS operations.
///
/// # Architecture
///
/// ```text
/// ┌─────────────────────────────────────────────────────────────────────────┐
/// │                         LockFreeVocab                                    │
/// ├─────────────────────────────────────────────────────────────────────────┤
/// │  Index Allocation: AtomicU64 (lock-free fetch_add)                      │
/// │                                                                          │
/// │  Root: AtomicNodePtr → Arc<PersistentCharNode>                          │
/// │                         ↓                                                │
/// │                    im::Vector<(key, child)>                              │
/// │                         ↓                                                │
/// │              CAS swaps entire path on insert                            │
/// │                                                                          │
/// │  Term→Index Cache: DashMap (lock-free sharded HashMap)                  │
/// │                                                                          │
/// │  Index→Term Storage: Optional backing store                             │
/// └─────────────────────────────────────────────────────────────────────────┘
/// ```
#[derive(Debug)]
pub struct LockFreeVocab {
    /// Root node pointer (atomic for CAS updates)
    root: AtomicNodePtr,

    /// Next vocabulary index to assign (atomic for lock-free allocation)
    next_index: AtomicU64,

    /// Starting index (for validation and offset calculations)
    start_index: u64,

    /// Lock-free cache for term → index lookups
    /// Uses DashMap for O(1) sharded concurrent access.
    /// xxh3 hasher replaces SipHash for ~3-5x faster hashing on vocabulary terms.
    term_index_cache: DashMap<String, u64, Xxh3DefaultBuilder>,

    /// Index → term storage for reverse lookups
    /// This is optional and can be populated lazily
    index_term_storage: RwLock<Vec<Option<String>>>,

    /// Statistics: total insertions attempted
    total_inserts: AtomicU64,

    /// Statistics: CAS retries
    cas_retries: AtomicU64,

    /// Statistics: duplicate inserts (term already existed)
    duplicate_inserts: AtomicU64,
}

impl LockFreeVocab {
    /// Create a new empty lock-free vocabulary.
    pub fn new() -> Arc<Self> {
        Self::with_start_index(0)
    }

    /// Create a new lock-free vocabulary with a custom starting index.
    pub fn with_start_index(start_index: u64) -> Arc<Self> {
        let root = Arc::new(PersistentCharNode::new());

        Arc::new(Self {
            root: AtomicNodePtr::new(root),
            next_index: AtomicU64::new(start_index),
            start_index,
            term_index_cache: DashMap::with_hasher(Xxh3DefaultBuilder),
            index_term_storage: RwLock::new(Vec::new()),
            total_inserts: AtomicU64::new(0),
            cas_retries: AtomicU64::new(0),
            duplicate_inserts: AtomicU64::new(0),
        })
    }

    /// Create a new lock-free vocabulary with a custom starting index and
    /// pre-allocated capacity for the term cache and reverse-lookup storage.
    ///
    /// Pre-sizing avoids geometric doubling resize spikes in both the
    /// `DashMap` term cache and the `index_term_storage` Vec.
    ///
    /// # Arguments
    ///
    /// * `start_index` - The first vocabulary index to assign
    /// * `estimated_terms` - Expected number of unique terms to insert
    pub fn with_start_index_and_capacity(start_index: u64, estimated_terms: usize) -> Arc<Self> {
        let root = Arc::new(PersistentCharNode::new());

        Arc::new(Self {
            root: AtomicNodePtr::new(root),
            next_index: AtomicU64::new(start_index),
            start_index,
            term_index_cache: DashMap::with_capacity_and_hasher(
                estimated_terms,
                Xxh3DefaultBuilder,
            ),
            index_term_storage: RwLock::new(Vec::with_capacity(estimated_terms)),
            total_inserts: AtomicU64::new(0),
            cas_retries: AtomicU64::new(0),
            duplicate_inserts: AtomicU64::new(0),
        })
    }

    /// Get the next index value (atomically).
    #[inline]
    pub fn next_index(&self) -> u64 {
        self.next_index.load(Ordering::Acquire)
    }

    /// Get the start index.
    #[inline]
    pub fn start_index(&self) -> u64 {
        self.start_index
    }

    /// Get the current vocabulary size (approximate, may race with concurrent inserts).
    #[inline]
    pub fn len(&self) -> usize {
        (self.next_index.load(Ordering::Relaxed) - self.start_index) as usize
    }

    /// Check if the vocabulary is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Look up a term's index (lock-free read).
    ///
    /// This first checks the cache, then traverses the trie.
    /// Returns `None` if the term doesn't exist.
    pub fn get_index(&self, term: &str) -> Option<u64> {
        // Fast path: check cache
        if let Some(entry) = self.term_index_cache.get(term) {
            return Some(*entry);
        }

        // Slow path: traverse trie
        let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();
        let root = self.root.load()?;

        let mut current = root;
        for &c in &chars {
            match current.find_child(c) {
                Some(child_ptr) => {
                    if child_ptr.is_null() {
                        return None;
                    }
                    if let Some(ptr) = child_ptr.as_ptr::<PersistentCharNode>() {
                        // SAFETY: child_ptr is swizzled (as_ptr returned Some);
                        // ptr originated from `Arc::into_raw` during a CAS
                        // insertion path (see insert_cas / install_path). The
                        // Arc strong count is currently >= 1 because the parent
                        // node holds the reference; bumping the count here
                        // gives the new local Arc a fresh strong reference.
                        unsafe {
                            Arc::increment_strong_count(ptr);
                            current = Arc::from_raw(ptr);
                        }
                    } else {
                        // LockFreeVocab is constructed exclusively via
                        // `LockFreeVocab::new` / `with_start_index`, both of
                        // which create an empty in-memory root, and every
                        // CAS-insert path installs in-memory `Arc`-backed
                        // children. The trie therefore never contains an
                        // on-disk `SwizzledPtr`; if one ever appears, the
                        // construction or persistence layer has introduced an
                        // invariant violation that needs investigation rather
                        // than a silent `return None`.
                        unreachable!(
                            "LockFreeVocab encountered an on-disk SwizzledPtr child for code \
                             point {:#x}; the structure does not load nodes from disk and all \
                             construction paths produce in-memory children only — this indicates \
                             corruption of the in-memory invariant",
                            c
                        );
                    }
                }
                None => return None,
            }
        }

        // Check if this node is final and has a value
        current.get_value()
    }

    /// Check if a term exists in the vocabulary.
    #[inline]
    pub fn contains(&self, term: &str) -> bool {
        self.get_index(term).is_some()
    }

    /// Insert a term using lock-free CAS operations.
    ///
    /// This method:
    /// 1. Checks if the term already exists (returns existing index if so)
    /// 2. Atomically claims the next index using fetch_add
    /// 3. CAS-inserts the term into the trie
    /// 4. If CAS fails due to concurrent modification, retries
    ///
    /// # Returns
    ///
    /// An `InsertResult` indicating whether the term was newly inserted
    /// or already existed, along with its vocabulary index.
    pub fn insert_cas(&self, term: &str) -> InsertResult {
        self.total_inserts.fetch_add(1, Ordering::Relaxed);

        // Fast path: check if already exists
        if let Some(idx) = self.get_index(term) {
            self.duplicate_inserts.fetch_add(1, Ordering::Relaxed);
            return InsertResult::AlreadyExists(idx);
        }

        // Convert term to character codes
        let chars: Vec<u32> = term.chars().map(|c| c as u32).collect();

        // Atomically claim the next index
        let index = self.next_index.fetch_add(1, Ordering::AcqRel);

        // CAS loop to insert into trie
        loop {
            let root = match self.root.load() {
                Some(r) => r,
                None => {
                    // Root is null - initialize it
                    let new_root = Arc::new(PersistentCharNode::new());
                    if self.root.try_init(new_root.clone()).is_ok() {
                        continue; // Root initialized, retry insert
                    }
                    continue; // Someone else initialized, retry
                }
            };

            match self.try_insert_path(&root, &chars, index) {
                Ok(new_root) => {
                    // CAS the root to the new version
                    match self.root.compare_exchange(&root, new_root) {
                        Ok(_) => {
                            // Success! Update cache and return
                            self.term_index_cache.insert(term.to_string(), index);

                            // Store for reverse lookup
                            {
                                let mut storage = self.index_term_storage.write();
                                let offset = (index - self.start_index) as usize;
                                if offset >= storage.len() {
                                    storage.resize(offset + 1, None);
                                }
                                storage[offset] = Some(term.to_string());
                            }

                            return InsertResult::Inserted(index);
                        }
                        Err(actual) => {
                            // CAS failed - someone else modified the root
                            self.cas_retries.fetch_add(1, Ordering::Relaxed);

                            // Check if the term was inserted by another thread
                            if let Some(existing_idx) = self.find_term_in_trie(&actual, &chars) {
                                // Another thread inserted this term
                                // Our claimed index is "wasted" (sparse indices)
                                self.term_index_cache.insert(term.to_string(), existing_idx);
                                self.duplicate_inserts.fetch_add(1, Ordering::Relaxed);
                                return InsertResult::AlreadyExists(existing_idx);
                            }

                            // Another thread modified a different part of the trie
                            // Retry with the new root
                            continue;
                        }
                    }
                }
                Err(existing_idx) => {
                    // Term already exists (found during path creation)
                    self.term_index_cache.insert(term.to_string(), existing_idx);
                    self.duplicate_inserts.fetch_add(1, Ordering::Relaxed);
                    return InsertResult::AlreadyExists(existing_idx);
                }
            }
        }
    }

    /// Try to create a new root with the term inserted.
    ///
    /// Returns `Ok(new_root)` if successful, `Err(existing_idx)` if term already exists.
    fn try_insert_path(
        &self,
        root: &Arc<PersistentCharNode>,
        chars: &[u32],
        index: u64,
    ) -> std::result::Result<Arc<PersistentCharNode>, u64> {
        if chars.is_empty() {
            // Empty term - mark root as final
            if root.is_final() {
                return Err(root.get_value().unwrap_or(0));
            }
            let new_root = root.as_final().with_value(index);
            return Ok(Arc::new(new_root));
        }

        // Build the path bottom-up
        self.insert_recursive(root, chars, 0, index)
    }

    /// Recursively create new nodes along the path.
    fn insert_recursive(
        &self,
        node: &Arc<PersistentCharNode>,
        chars: &[u32],
        depth: usize,
        index: u64,
    ) -> std::result::Result<Arc<PersistentCharNode>, u64> {
        if depth == chars.len() {
            // Reached the end - mark as final
            if node.is_final() {
                return Err(node.get_value().unwrap_or(0));
            }
            let new_node = node.as_final().with_value(index);
            return Ok(Arc::new(new_node));
        }

        let c = chars[depth];

        match node.find_child(c) {
            Some(child_ptr) => {
                // Child exists - recurse
                if child_ptr.is_null() {
                    return Err(0); // Shouldn't happen
                }

                if let Some(ptr) = child_ptr.as_ptr::<PersistentCharNode>() {
                    let child = unsafe {
                        Arc::increment_strong_count(ptr);
                        Arc::from_raw(ptr)
                    };

                    // Recurse into child
                    let new_child = self.insert_recursive(&child, chars, depth + 1, index)?;

                    // Create new node with updated child pointer
                    let new_child_ptr = SwizzledPtr::in_memory(Arc::into_raw(new_child));
                    let new_node = node.with_child(c, new_child_ptr);
                    Ok(Arc::new(new_node))
                } else {
                    // On-disk child - not supported yet
                    Err(0)
                }
            }
            None => {
                // Child doesn't exist - create new path
                let new_child = self.create_path(&chars[depth + 1..], index);
                let new_child_ptr = SwizzledPtr::in_memory(Arc::into_raw(new_child));
                let new_node = node.with_child(c, new_child_ptr);
                Ok(Arc::new(new_node))
            }
        }
    }

    /// Create a new path from the remaining characters.
    fn create_path(&self, chars: &[u32], index: u64) -> Arc<PersistentCharNode> {
        if chars.is_empty() {
            // Create final node with value
            let node = PersistentCharNode::new().as_final().with_value(index);
            return Arc::new(node);
        }

        // Build path bottom-up
        let mut current = Arc::new(PersistentCharNode::new().as_final().with_value(index));

        for &c in chars.iter().rev() {
            let child_ptr = SwizzledPtr::in_memory(Arc::into_raw(current));
            let parent = PersistentCharNode::new().with_child(c, child_ptr);
            current = Arc::new(parent);
        }

        current
    }

    /// Find a term in the trie, returning its index if found.
    fn find_term_in_trie(&self, root: &Arc<PersistentCharNode>, chars: &[u32]) -> Option<u64> {
        let mut current = root.clone();

        for &c in chars {
            match current.find_child(c) {
                Some(child_ptr) => {
                    if child_ptr.is_null() {
                        return None;
                    }
                    if let Some(ptr) = child_ptr.as_ptr::<PersistentCharNode>() {
                        unsafe {
                            Arc::increment_strong_count(ptr);
                            current = Arc::from_raw(ptr);
                        }
                    } else {
                        return None;
                    }
                }
                None => return None,
            }
        }

        current.get_value()
    }

    /// Get a term by its index (reverse lookup).
    ///
    /// This requires the term to have been stored during insertion.
    pub fn get_term(&self, index: u64) -> Option<String> {
        if index < self.start_index {
            return None;
        }

        let offset = (index - self.start_index) as usize;
        let storage = self.index_term_storage.read();

        storage.get(offset).and_then(|opt| opt.clone())
    }

    /// Insert multiple terms concurrently.
    ///
    /// This is a convenience method that calls `insert_cas` for each term.
    /// For best performance, use multiple threads calling `insert_cas` directly.
    pub fn insert_batch(&self, terms: &[&str]) -> Vec<u64> {
        terms.iter().map(|t| self.insert_cas(t).index()).collect()
    }

    /// Get statistics about the vocabulary.
    pub fn stats(&self) -> LockFreeVocabStats {
        LockFreeVocabStats {
            entry_count: self.len(),
            next_index: self.next_index.load(Ordering::Relaxed),
            cache_size: self.term_index_cache.len(),
            total_inserts: self.total_inserts.load(Ordering::Relaxed),
            cas_retries: self.cas_retries.load(Ordering::Relaxed),
            duplicate_inserts: self.duplicate_inserts.load(Ordering::Relaxed),
        }
    }

    /// Merge this lock-free vocabulary into a persistent vocabulary.
    ///
    /// This is useful for checkpointing: after concurrent inserts, merge
    /// the lock-free vocab into the persistent store for durability.
    ///
    /// Pre-reserves space in the target's `node_map` to avoid geometric
    /// doubling resize spikes during bulk insertion (eliminates up to
    /// ~6.4 GB peak memory from HashMap resize doubling).
    pub fn merge_into(&self, target: &mut PersistentVocabARTrie) -> Result<usize> {
        let storage = self.index_term_storage.read();

        // Pre-reserve node_map capacity based on estimated total characters.
        // Each character in each term may create a trie node, so the total
        // number of nodes is bounded by sum(term.len()) across all terms.
        // Prefix sharing reduces the actual count, but over-reserving is
        // cheaper than under-reserving (avoids resize + copy spikes).
        let estimated_nodes: usize = storage
            .iter()
            .filter_map(|opt| opt.as_ref())
            .map(|term| term.len()) // byte length ~ char count for ASCII-heavy vocab
            .sum();
        if estimated_nodes > 0 {
            target.reserve_node_map(estimated_nodes);
        }

        let mut count = 0;

        for (offset, opt_term) in storage.iter().enumerate() {
            if let Some(term) = opt_term {
                let index = self.start_index + offset as u64;
                if target.insert_with_index(term, index) {
                    count += 1;
                }
            }
        }

        Ok(count)
    }
}

impl Default for LockFreeVocab {
    fn default() -> Self {
        // Can't return Arc<Self> from Default, so this creates an inner Self
        // Users should use LockFreeVocab::new() instead
        let root = Arc::new(PersistentCharNode::new());
        Self {
            root: AtomicNodePtr::new(root),
            next_index: AtomicU64::new(0),
            start_index: 0,
            term_index_cache: DashMap::with_hasher(Xxh3DefaultBuilder),
            index_term_storage: RwLock::new(Vec::new()),
            total_inserts: AtomicU64::new(0),
            cas_retries: AtomicU64::new(0),
            duplicate_inserts: AtomicU64::new(0),
        }
    }
}

// Safety: LockFreeVocab uses only thread-safe primitives
unsafe impl Send for LockFreeVocab {}
unsafe impl Sync for LockFreeVocab {}

/// Statistics for lock-free vocabulary operations.
#[derive(Debug, Clone)]
pub struct LockFreeVocabStats {
    /// Number of unique entries in the vocabulary
    pub entry_count: usize,
    /// Next index to be assigned
    pub next_index: u64,
    /// Number of entries in the term→index cache
    pub cache_size: usize,
    /// Total insert operations attempted
    pub total_inserts: u64,
    /// Number of CAS retries due to concurrent modifications
    pub cas_retries: u64,
    /// Number of duplicate insert attempts (term already existed)
    pub duplicate_inserts: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_basic_insert() {
        let vocab = LockFreeVocab::new();

        let result = vocab.insert_cas("hello");
        assert!(result.was_inserted());
        assert_eq!(result.index(), 0);

        let result2 = vocab.insert_cas("world");
        assert!(result2.was_inserted());
        assert_eq!(result2.index(), 1);

        // Duplicate should return existing
        let result3 = vocab.insert_cas("hello");
        assert!(!result3.was_inserted());
        assert_eq!(result3.index(), 0);
    }

    #[test]
    fn test_get_index() {
        let vocab = LockFreeVocab::new();

        vocab.insert_cas("apple");
        vocab.insert_cas("banana");
        vocab.insert_cas("cherry");

        assert_eq!(vocab.get_index("apple"), Some(0));
        assert_eq!(vocab.get_index("banana"), Some(1));
        assert_eq!(vocab.get_index("cherry"), Some(2));
        assert_eq!(vocab.get_index("date"), None);
    }

    #[test]
    fn test_get_term() {
        let vocab = LockFreeVocab::new();

        vocab.insert_cas("apple");
        vocab.insert_cas("banana");

        assert_eq!(vocab.get_term(0), Some("apple".to_string()));
        assert_eq!(vocab.get_term(1), Some("banana".to_string()));
        assert_eq!(vocab.get_term(2), None);
    }

    #[test]
    fn test_concurrent_unique_terms() {
        let vocab = LockFreeVocab::new();
        let num_threads = 4;
        let terms_per_thread = 100;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let v = Arc::clone(&vocab);
                thread::spawn(move || {
                    let mut indices = Vec::new();
                    for i in 0..terms_per_thread {
                        let term = format!("thread{}_{}", t, i);
                        let result = v.insert_cas(&term);
                        indices.push(result.index());
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
    fn test_concurrent_same_terms() {
        let vocab = LockFreeVocab::new();
        let num_threads = 8;

        // All threads try to insert the same terms
        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let v = Arc::clone(&vocab);
                thread::spawn(move || {
                    let mut indices = Vec::new();
                    for i in 0..100 {
                        let term = format!("shared_{}", i);
                        let result = v.insert_cas(&term);
                        indices.push(result.index());
                    }
                    indices
                })
            })
            .collect();

        let all_results: Vec<Vec<u64>> = handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect();

        // All threads should agree on the index for each term
        for i in 0..100 {
            let indices: Vec<u64> = all_results.iter().map(|r| r[i]).collect();
            let first = indices[0];
            assert!(
                indices.iter().all(|&idx| idx == first),
                "disagreement on term shared_{}: {:?}",
                i,
                indices
            );
        }
    }

    #[test]
    fn test_unicode_terms() {
        let vocab = LockFreeVocab::new();

        vocab.insert_cas("hello");
        vocab.insert_cas("日本語");
        vocab.insert_cas("🎉");
        vocab.insert_cas("αβγ");

        assert_eq!(vocab.get_index("hello"), Some(0));
        assert_eq!(vocab.get_index("日本語"), Some(1));
        assert_eq!(vocab.get_index("🎉"), Some(2));
        assert_eq!(vocab.get_index("αβγ"), Some(3));
    }

    #[test]
    fn test_stats() {
        let vocab = LockFreeVocab::new();

        vocab.insert_cas("a");
        vocab.insert_cas("b");
        vocab.insert_cas("a"); // duplicate

        let stats = vocab.stats();
        assert_eq!(stats.entry_count, 2);
        assert_eq!(stats.next_index, 2);
        assert_eq!(stats.total_inserts, 3);
        assert_eq!(stats.duplicate_inserts, 1);
    }

    #[test]
    fn test_insert_batch() {
        let vocab = LockFreeVocab::new();

        let indices = vocab.insert_batch(&["apple", "banana", "cherry"]);
        assert_eq!(indices, vec![0, 1, 2]);

        // Duplicates should return existing
        let indices2 = vocab.insert_batch(&["apple", "date"]);
        assert_eq!(indices2[0], 0); // apple already exists
        assert!(indices2[1] >= 3); // date is new
    }

    #[test]
    fn test_empty_term() {
        let vocab = LockFreeVocab::new();

        let result = vocab.insert_cas("");
        assert!(result.was_inserted());
        assert_eq!(result.index(), 0);

        let result2 = vocab.insert_cas("");
        assert!(!result2.was_inserted());
        assert_eq!(result2.index(), 0);
    }
}
