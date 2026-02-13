//! MVCC-Lite Read Transactions
//!
//! This module provides lightweight Multi-Version Concurrency Control (MVCC) for
//! read transactions. Readers get a consistent snapshot of the trie at the time
//! they begin their transaction, and can read from that snapshot without blocking
//! or being blocked by concurrent writers.
//!
//! # Design
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                        MVCC-Lite Architecture                            │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │                                                                          │
//! │  Writer Thread                    Reader Threads                         │
//! │  ─────────────                    ──────────────                         │
//! │       │                           ┌─────────────┐                        │
//! │       │ insert_cas()              │ begin()     │                        │
//! │       │                           │   ↓         │                        │
//! │       ▼                           │ Capture     │                        │
//! │  ┌─────────────┐                  │ epoch &     │                        │
//! │  │ CAS root    │                  │ root ptr    │                        │
//! │  │ (new ver)   │                  │   ↓         │                        │
//! │  └─────────────┘                  │ get()/      │                        │
//! │       │                           │ contains()  │                        │
//! │       │                           │   ↓         │                        │
//! │       │ New writes                │ (Reads from │                        │
//! │       │ invisible to              │  pinned     │                        │
//! │       │ existing readers          │  version)   │                        │
//! │       │                           │   ↓         │                        │
//! │       │                           │ drop()      │                        │
//! │       │                           │ (releases   │                        │
//! │       ▼                           │  epoch)     │                        │
//! │  Version N+1                      └─────────────┘                        │
//! │  (visible to                                                             │
//! │   new readers)                                                           │
//! │                                                                          │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! let trie = PersistentARTrieChar::create("trie.artc")?;
//! trie.enable_lockfree();
//!
//! // Start a read transaction
//! let tx = ReadTransaction::begin(&trie);
//!
//! // Concurrent write happens here - NOT visible to tx
//! trie.insert_cas("new_term");
//!
//! // Read from pinned version (sees pre-insert state)
//! assert!(!tx.contains("new_term"));
//!
//! // Transaction ends on drop, version becomes eligible for GC
//! drop(tx);
//! ```
//!
//! # Memory Safety
//!
//! Read transactions use epoch-based protection to prevent use-after-free:
//! - Each transaction captures an epoch guard on creation
//! - The guard prevents version GC until the transaction completes
//! - On drop, the guard is released, allowing old versions to be reclaimed

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::concurrency::EpochManager;
use super::nodes::PersistentNode;
use crate::persistent_artrie_char::nodes::PersistentCharNode;

/// Statistics for MVCC read transactions.
#[derive(Debug, Clone, Default)]
pub struct MvccStats {
    /// Total read transactions started
    pub transactions_started: u64,
    /// Total read transactions completed
    pub transactions_completed: u64,
    /// Current active transactions
    pub active_transactions: u64,
    /// Total reads performed
    pub total_reads: u64,
    /// Cache hits during reads
    pub cache_hits: u64,
}

/// Global MVCC statistics tracker.
#[derive(Debug)]
pub struct MvccStatsTracker {
    transactions_started: AtomicU64,
    transactions_completed: AtomicU64,
    active_transactions: AtomicU64,
    total_reads: AtomicU64,
    cache_hits: AtomicU64,
}

impl MvccStatsTracker {
    /// Create a new stats tracker.
    pub fn new() -> Self {
        Self {
            transactions_started: AtomicU64::new(0),
            transactions_completed: AtomicU64::new(0),
            active_transactions: AtomicU64::new(0),
            total_reads: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
        }
    }

    /// Record a transaction start.
    pub fn record_start(&self) {
        self.transactions_started.fetch_add(1, Ordering::Relaxed);
        self.active_transactions.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a transaction completion.
    pub fn record_complete(&self) {
        self.transactions_completed.fetch_add(1, Ordering::Relaxed);
        self.active_transactions.fetch_sub(1, Ordering::Relaxed);
    }

    /// Record a read operation.
    pub fn record_read(&self) {
        self.total_reads.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache hit.
    pub fn record_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Get current statistics.
    pub fn stats(&self) -> MvccStats {
        MvccStats {
            transactions_started: self.transactions_started.load(Ordering::Relaxed),
            transactions_completed: self.transactions_completed.load(Ordering::Relaxed),
            active_transactions: self.active_transactions.load(Ordering::Relaxed),
            total_reads: self.total_reads.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
        }
    }
}

impl Default for MvccStatsTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// A read transaction for MVCC-lite snapshot isolation.
///
/// This struct pins a version of the trie and allows consistent reads from that
/// version. Concurrent writes are not visible to this transaction.
///
/// # Lifetime
///
/// The transaction is active from creation until drop. While active:
/// - The pinned version is protected from garbage collection
/// - All reads see the state at transaction start time
/// - No blocking occurs (all operations are lock-free)
///
/// # Thread Safety
///
/// `ReadTransaction` is `Send` but not `Sync` - it can be moved between threads
/// but should not be shared. Each thread should have its own transaction.
#[derive(Debug)]
pub struct ReadTransaction<T: TrieRoot> {
    /// The pinned root node for this transaction's version
    root: Option<Arc<T>>,
    /// Version ID captured at transaction start
    version_id: u64,
    /// Epoch captured at transaction start (for GC protection)
    epoch: u64,
    /// Reference to the epoch manager (for cleanup on drop)
    epoch_manager: Arc<EpochManager>,
    /// Statistics tracker
    stats: Option<Arc<MvccStatsTracker>>,
}

/// Trait for trie root types that can be used with MVCC.
pub trait TrieRoot: Send + Sync + 'static {
    /// The key type for this trie (u8 or u32).
    type Key: Copy;

    /// Check if this node is a final node (end of a word).
    fn is_final(&self) -> bool;

    /// Find a child by key.
    fn find_child(&self, key: Self::Key) -> Option<Arc<Self>>;

    /// Get the value if this is a final node.
    fn get_value(&self) -> Option<u64>;
}

impl TrieRoot for PersistentNode {
    type Key = u8;

    fn is_final(&self) -> bool {
        PersistentNode::is_final(self)
    }

    fn find_child(&self, key: u8) -> Option<Arc<Self>> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

        if let Some(child_ptr) = PersistentNode::find_child(self, key) {
            if !child_ptr.is_on_disk() {
                if let Some(ptr) = child_ptr.as_ptr::<PersistentNode>() {
                    unsafe {
                        Arc::increment_strong_count(ptr);
                        return Some(Arc::from_raw(ptr));
                    }
                }
            }
        }
        None
    }

    fn get_value(&self) -> Option<u64> {
        PersistentNode::get_value(self)
    }
}

impl TrieRoot for PersistentCharNode {
    type Key = u32;

    fn is_final(&self) -> bool {
        PersistentCharNode::is_final(self)
    }

    fn find_child(&self, key: u32) -> Option<Arc<Self>> {
        if let Some(child_ptr) = PersistentCharNode::find_child(self, key) {
            if !child_ptr.is_on_disk() {
                if let Some(ptr) = child_ptr.as_ptr::<PersistentCharNode>() {
                    unsafe {
                        Arc::increment_strong_count(ptr);
                        return Some(Arc::from_raw(ptr));
                    }
                }
            }
        }
        None
    }

    fn get_value(&self) -> Option<u64> {
        PersistentCharNode::get_value(self)
    }
}

impl<T: TrieRoot> ReadTransaction<T> {
    /// Begin a new read transaction.
    ///
    /// This captures the current version of the trie and pins it for the
    /// duration of the transaction.
    ///
    /// # Arguments
    ///
    /// * `root` - The current root node to pin
    /// * `epoch_manager` - The epoch manager for GC protection
    pub fn begin(root: Arc<T>, epoch_manager: Arc<EpochManager>) -> Self {
        let epoch = epoch_manager.enter_read();
        let version_id = epoch_manager.current_epoch();

        Self {
            root: Some(root),
            version_id,
            epoch,
            epoch_manager,
            stats: None,
        }
    }

    /// Begin a transaction with statistics tracking.
    pub fn begin_with_stats(
        root: Arc<T>,
        epoch_manager: Arc<EpochManager>,
        stats: Arc<MvccStatsTracker>,
    ) -> Self {
        let epoch = epoch_manager.enter_read();
        let version_id = epoch_manager.current_epoch();
        stats.record_start();

        Self {
            root: Some(root),
            version_id,
            epoch,
            epoch_manager,
            stats: Some(stats),
        }
    }

    /// Get the version ID of this transaction.
    #[inline]
    pub fn version_id(&self) -> u64 {
        self.version_id
    }

    /// Get the epoch of this transaction.
    #[inline]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Get a reference to the pinned root node.
    #[inline]
    pub fn root(&self) -> Option<&Arc<T>> {
        self.root.as_ref()
    }
}

impl<T: TrieRoot<Key = u8>> ReadTransaction<T> {
    /// Check if a byte term exists in the pinned version.
    pub fn contains(&self, term: &[u8]) -> bool {
        if let Some(stats) = &self.stats {
            stats.record_read();
        }

        let Some(root) = &self.root else {
            return false;
        };

        let mut current = Arc::clone(root);
        for &key in term {
            match current.find_child(key) {
                Some(child) => current = child,
                None => return false,
            }
        }

        current.is_final()
    }

    /// Get the value for a byte term in the pinned version.
    pub fn get(&self, term: &[u8]) -> Option<u64> {
        if let Some(stats) = &self.stats {
            stats.record_read();
        }

        let root = self.root.as_ref()?;

        let mut current = Arc::clone(root);
        for &key in term {
            match current.find_child(key) {
                Some(child) => current = child,
                None => return None,
            }
        }

        if current.is_final() {
            current.get_value()
        } else {
            None
        }
    }
}

impl<T: TrieRoot<Key = u32>> ReadTransaction<T> {
    /// Check if a string term exists in the pinned version.
    pub fn contains_str(&self, term: &str) -> bool {
        if let Some(stats) = &self.stats {
            stats.record_read();
        }

        let Some(root) = &self.root else {
            return false;
        };

        let mut current = Arc::clone(root);
        for c in term.chars() {
            match current.find_child(c as u32) {
                Some(child) => current = child,
                None => return false,
            }
        }

        current.is_final()
    }

    /// Get the value for a string term in the pinned version.
    pub fn get_str(&self, term: &str) -> Option<u64> {
        if let Some(stats) = &self.stats {
            stats.record_read();
        }

        let root = self.root.as_ref()?;

        let mut current = Arc::clone(root);
        for c in term.chars() {
            match current.find_child(c as u32) {
                Some(child) => current = child,
                None => return None,
            }
        }

        if current.is_final() {
            current.get_value()
        } else {
            None
        }
    }
}

impl<T: TrieRoot> Drop for ReadTransaction<T> {
    fn drop(&mut self) {
        // Release the epoch guard
        self.epoch_manager.exit_read();

        // Release the root reference
        self.root = None;

        // Update statistics
        if let Some(stats) = &self.stats {
            stats.record_complete();
        }
    }
}

// Safety: ReadTransaction can be sent between threads
unsafe impl<T: TrieRoot> Send for ReadTransaction<T> {}

/// A lightweight read guard that doesn't pin a specific root.
///
/// This is useful when you want to protect an epoch without having a root yet,
/// or when you're doing lookups that go through the DashMap cache.
#[derive(Debug)]
pub struct EpochGuard {
    epoch: u64,
    epoch_manager: Arc<EpochManager>,
}

impl EpochGuard {
    /// Create a new epoch guard.
    pub fn new(epoch_manager: Arc<EpochManager>) -> Self {
        let epoch = epoch_manager.enter_read();
        Self { epoch, epoch_manager }
    }

    /// Get the epoch of this guard.
    #[inline]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

impl Drop for EpochGuard {
    fn drop(&mut self) {
        self.epoch_manager.exit_read();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Create a simple test implementation of TrieRoot
    #[derive(Debug)]
    struct TestNode {
        is_final: bool,
        value: Option<u64>,
        children: std::collections::HashMap<u8, Arc<TestNode>>,
    }

    impl TestNode {
        fn new() -> Self {
            Self {
                is_final: false,
                value: None,
                children: std::collections::HashMap::new(),
            }
        }

        fn with_final(mut self) -> Self {
            self.is_final = true;
            self
        }

        fn with_value(mut self, value: u64) -> Self {
            self.value = Some(value);
            self
        }

        fn with_child(mut self, key: u8, child: TestNode) -> Self {
            self.children.insert(key, Arc::new(child));
            self
        }
    }

    impl TrieRoot for TestNode {
        type Key = u8;

        fn is_final(&self) -> bool {
            self.is_final
        }

        fn find_child(&self, key: u8) -> Option<Arc<Self>> {
            self.children.get(&key).cloned()
        }

        fn get_value(&self) -> Option<u64> {
            self.value
        }
    }

    #[test]
    fn test_read_transaction_basic() {
        let epoch_manager = Arc::new(EpochManager::new());

        // Build a simple trie: "ab" -> value 42
        let leaf = TestNode::new().with_final().with_value(42);
        let mid = TestNode::new().with_child(b'b', leaf);
        let root = Arc::new(TestNode::new().with_child(b'a', mid));

        let tx = ReadTransaction::begin(root, epoch_manager);

        assert!(tx.contains(b"ab"));
        assert!(!tx.contains(b"a"));
        assert!(!tx.contains(b"abc"));
        assert!(!tx.contains(b""));

        assert_eq!(tx.get(b"ab"), Some(42));
        assert_eq!(tx.get(b"a"), None);
    }

    #[test]
    fn test_read_transaction_stats() {
        let epoch_manager = Arc::new(EpochManager::new());
        let stats = Arc::new(MvccStatsTracker::new());

        let leaf = TestNode::new().with_final();
        let root = Arc::new(TestNode::new().with_child(b'a', leaf));

        {
            let tx = ReadTransaction::begin_with_stats(
                root.clone(),
                epoch_manager.clone(),
                stats.clone(),
            );

            tx.contains(b"a");
            tx.contains(b"b");

            let current_stats = stats.stats();
            assert_eq!(current_stats.transactions_started, 1);
            assert_eq!(current_stats.active_transactions, 1);
            assert_eq!(current_stats.total_reads, 2);
        }

        // After drop
        let final_stats = stats.stats();
        assert_eq!(final_stats.transactions_completed, 1);
        assert_eq!(final_stats.active_transactions, 0);
    }

    #[test]
    fn test_epoch_guard() {
        let epoch_manager = Arc::new(EpochManager::new());

        assert!(!epoch_manager.has_active_readers());

        {
            let _guard = EpochGuard::new(epoch_manager.clone());
            assert!(epoch_manager.has_active_readers());
        }

        assert!(!epoch_manager.has_active_readers());
    }

    #[test]
    fn test_version_id_and_epoch() {
        let epoch_manager = Arc::new(EpochManager::new());
        let root = Arc::new(TestNode::new());

        // Advance epoch a few times
        epoch_manager.advance();
        epoch_manager.advance();

        let tx = ReadTransaction::begin(root, epoch_manager.clone());

        assert!(tx.version_id() >= 2);
        assert!(tx.epoch() >= 2);
    }
}
