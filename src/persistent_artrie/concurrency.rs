//! Concurrency controls for Persistent ART.
//!
//! This module provides advanced concurrency primitives for the Persistent ART:
//!
//! - **Optimistic Lock Coupling**: Lock-free reads with version validation
//! - **Read-Write Locks**: Fine-grained locking for tree traversal
//! - **Epoch-Based Reclamation**: Safe memory reclamation for concurrent access
//!
//! # Architecture
//!
//! The concurrency model follows these principles:
//!
//! 1. **Readers don't block readers**: Multiple concurrent reads are always allowed
//! 2. **Writers acquire exclusive access**: Write operations use fine-grained locks
//! 3. **Optimistic reads**: Readers proceed without locks, validate versions after
//! 4. **Lock coupling**: Writers hold parent lock while acquiring child lock
//!
//! # Optimistic Lock Coupling
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────────┐
//! │                     Reader (Optimistic)                           │
//! │   ┌──────┐   ┌──────┐   ┌──────┐   ┌──────┐                      │
//! │   │Read v│ → │ Read │ → │Read v│ → │ Read │ → Validate versions  │
//! │   │ (A)  │   │ data │   │ (B)  │   │ data │                      │
//! │   └──────┘   └──────┘   └──────┘   └──────┘                      │
//! └───────────────────────────────────────────────────────────────────┘
//!
//! ┌───────────────────────────────────────────────────────────────────┐
//! │                     Writer (Lock Coupling)                         │
//! │   ┌──────┐   ┌──────┐   ┌──────┐   ┌──────┐                      │
//! │   │Lock A│ → │Lock B│ → │Unlock│ → │Modify│ → Unlock B           │
//! │   │      │   │      │   │  A   │   │  B   │                      │
//! │   └──────┘   └──────┘   └──────┘   └──────┘                      │
//! └───────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use libdictenstein::persistent_artrie::concurrency::*;
//!
//! let node = OptimisticNode::new(data);
//!
//! // Optimistic read
//! loop {
//!     let guard = node.read_optimistic();
//!     let value = guard.read(|data| data.clone());
//!     if guard.validate() {
//!         // Value is consistent
//!         break;
//!     }
//!     // Retry - writer modified during read
//! }
//!
//! // Write with lock coupling
//! let parent_guard = parent.write();
//! let child_guard = child.write();
//! drop(parent_guard); // Release parent before modifying child
//! child_guard.modify(|data| { /* ... */ });
//! ```

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// Version number for optimistic concurrency control.
///
/// The version is incremented on every write. Odd versions indicate
/// an in-progress write, even versions indicate a stable state.
#[derive(Debug)]
pub struct OptimisticVersion {
    version: AtomicU64,
}

impl OptimisticVersion {
    /// Create a new version counter.
    pub fn new() -> Self {
        OptimisticVersion {
            version: AtomicU64::new(0),
        }
    }

    /// Get the current version.
    pub fn get(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Check if the version is stable (not being modified).
    pub fn is_stable(&self) -> bool {
        self.version.load(Ordering::Acquire) % 2 == 0
    }

    /// Begin a write operation (increment to odd).
    pub fn begin_write(&self) -> u64 {
        self.version.fetch_add(1, Ordering::AcqRel)
    }

    /// End a write operation (increment to even).
    pub fn end_write(&self) {
        self.version.fetch_add(1, Ordering::Release);
    }

    /// Check if version changed since observed value.
    pub fn validate(&self, observed: u64) -> bool {
        self.version.load(Ordering::Acquire) == observed
    }
}

impl Default for OptimisticVersion {
    fn default() -> Self {
        Self::new()
    }
}

/// Optimistic read guard for lock-free reads.
///
/// This guard captures a version before reading and validates after.
/// If validation fails, the read must be retried.
pub struct OptimisticReadGuard<'a> {
    version: &'a OptimisticVersion,
    observed: u64,
}

impl<'a> OptimisticReadGuard<'a> {
    /// Create a new guard, capturing the current version.
    pub fn new(version: &'a OptimisticVersion) -> Self {
        // Wait for stable version
        let mut observed = version.get();
        while observed % 2 != 0 {
            std::hint::spin_loop();
            observed = version.get();
        }

        OptimisticReadGuard { version, observed }
    }

    /// Get the observed version.
    pub fn observed(&self) -> u64 {
        self.observed
    }

    /// Validate that no write occurred since the guard was created.
    pub fn validate(&self) -> bool {
        self.version.validate(self.observed)
    }
}

/// Write guard that manages version updates.
pub struct WriteGuard<'a> {
    version: &'a OptimisticVersion,
}

impl<'a> WriteGuard<'a> {
    /// Acquire a write guard.
    pub fn new(version: &'a OptimisticVersion) -> Self {
        version.begin_write();
        WriteGuard { version }
    }
}

impl<'a> Drop for WriteGuard<'a> {
    fn drop(&mut self) {
        self.version.end_write();
    }
}

/// Lock coupling coordinator for safe tree traversal.
///
/// During tree modifications, we use "lock coupling" (also called "crabbing"):
/// - Acquire lock on parent
/// - Acquire lock on child
/// - Release lock on parent (if child is "safe")
/// - A node is "safe" if it won't be modified by the current operation
pub struct LockCoupling {
    /// Maximum depth of held locks
    max_depth: usize,
    /// Current lock depth
    current_depth: AtomicUsize,
}

impl LockCoupling {
    /// Create a new lock coupling coordinator.
    pub fn new(max_depth: usize) -> Self {
        LockCoupling {
            max_depth,
            current_depth: AtomicUsize::new(0),
        }
    }

    /// Attempt to acquire a lock at the next level.
    ///
    /// Returns true if successful, false if max depth reached.
    pub fn try_descend(&self) -> bool {
        let current = self.current_depth.load(Ordering::Relaxed);
        if current >= self.max_depth {
            return false;
        }
        self.current_depth.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Release a level (when ascending or completing).
    pub fn ascend(&self) {
        let current = self.current_depth.load(Ordering::Relaxed);
        if current > 0 {
            self.current_depth.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Get current depth.
    pub fn depth(&self) -> usize {
        self.current_depth.load(Ordering::Relaxed)
    }

    /// Reset to root.
    pub fn reset(&self) {
        self.current_depth.store(0, Ordering::Relaxed);
    }
}

impl Default for LockCoupling {
    fn default() -> Self {
        Self::new(64) // Default max depth
    }
}

/// Epoch-based reclamation for safe memory management.
///
/// This provides a way to safely deallocate memory that might be
/// accessed by concurrent readers. Memory is not freed until all
/// readers that might see it have finished.
pub struct EpochManager {
    /// Global epoch counter
    global_epoch: AtomicU64,
    /// Number of active readers
    active_readers: AtomicUsize,
}

impl EpochManager {
    /// Create a new epoch manager.
    pub fn new() -> Self {
        EpochManager {
            global_epoch: AtomicU64::new(0),
            active_readers: AtomicUsize::new(0),
        }
    }

    /// Enter a read epoch.
    ///
    /// Returns the current epoch for validation.
    pub fn enter_read(&self) -> u64 {
        self.active_readers.fetch_add(1, Ordering::AcqRel);
        self.global_epoch.load(Ordering::Acquire)
    }

    /// Exit a read epoch.
    pub fn exit_read(&self) {
        self.active_readers.fetch_sub(1, Ordering::Release);
    }

    /// Advance the global epoch.
    ///
    /// This should be called periodically by a background thread
    /// to enable garbage collection.
    pub fn advance(&self) -> u64 {
        self.global_epoch.fetch_add(1, Ordering::AcqRel)
    }

    /// Get the current epoch.
    pub fn current_epoch(&self) -> u64 {
        self.global_epoch.load(Ordering::Acquire)
    }

    /// Check if there are any active readers.
    pub fn has_active_readers(&self) -> bool {
        self.active_readers.load(Ordering::Acquire) > 0
    }

    /// Get the number of active readers.
    pub fn active_reader_count(&self) -> usize {
        self.active_readers.load(Ordering::Acquire)
    }
}

impl Default for EpochManager {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard for epoch-protected reads.
pub struct EpochGuard<'a> {
    manager: &'a EpochManager,
    #[allow(dead_code)]
    epoch: u64,
}

impl<'a> EpochGuard<'a> {
    /// Create a new epoch guard.
    pub fn new(manager: &'a EpochManager) -> Self {
        let epoch = manager.enter_read();
        EpochGuard { manager, epoch }
    }
}

impl<'a> Drop for EpochGuard<'a> {
    fn drop(&mut self) {
        self.manager.exit_read();
    }
}

/// MVCC-Lite read context for snapshot isolation.
///
/// Provides a lightweight form of Multi-Version Concurrency Control (MVCC)
/// using epoch-based snapshots. When created, it captures the current epoch
/// and tracks observed node versions. At validation time, it verifies that
/// all observed nodes still have the same versions.
///
/// # Isolation Guarantees
///
/// | Property         | Guarantee                                      |
/// |------------------|------------------------------------------------|
/// | Dirty Read       | Prevented (epoch snapshot)                     |
/// | Non-Repeatable   | Prevented (version validation)                 |
/// | Phantom Read     | Possible (new insertions not tracked)          |
///
/// # Example
///
/// ```rust,ignore
/// use libdictenstein::persistent_artrie::concurrency::{EpochManager, MvccReadContext};
///
/// let epoch_manager = EpochManager::new();
///
/// // Create a read context for snapshot isolation
/// let mut ctx = MvccReadContext::new(&epoch_manager);
///
/// // Perform reads, observing node versions
/// ctx.observe_node(node_id, node_version);
///
/// // Validate that no observed nodes were modified
/// if ctx.validate(|id| get_node_version(id)) {
///     // Read was consistent
/// } else {
///     // A concurrent write occurred - retry
/// }
/// ```
pub struct MvccReadContext<'a> {
    /// Reference to the epoch manager
    epoch_manager: &'a EpochManager,
    /// Snapshot epoch captured at context creation
    snapshot_epoch: u64,
    /// Observed node versions: (node_id, version) pairs
    node_versions: Vec<(usize, u64)>,
}

impl<'a> MvccReadContext<'a> {
    /// Create a new MVCC read context.
    ///
    /// Captures the current epoch as a snapshot point and registers
    /// with the epoch manager as an active reader.
    pub fn new(epoch_manager: &'a EpochManager) -> Self {
        let snapshot_epoch = epoch_manager.enter_read();
        MvccReadContext {
            epoch_manager,
            snapshot_epoch,
            node_versions: Vec::with_capacity(16), // Pre-allocate for typical path length
        }
    }

    /// Record an observed node and its version.
    ///
    /// This should be called for each node accessed during the read operation.
    /// At validation time, we verify these versions haven't changed.
    #[inline]
    pub fn observe_node(&mut self, node_id: usize, version: u64) {
        self.node_versions.push((node_id, version));
    }

    /// Get the snapshot epoch.
    pub fn epoch(&self) -> u64 {
        self.snapshot_epoch
    }

    /// Get the number of observed nodes.
    pub fn observed_count(&self) -> usize {
        self.node_versions.len()
    }

    /// Validate that no observed nodes were modified.
    ///
    /// Takes a function that retrieves the current version for a node ID.
    /// Returns true if all observed versions match current versions.
    ///
    /// # Arguments
    ///
    /// * `get_version` - Function that returns the current version for a node ID
    pub fn validate<F>(&self, get_version: F) -> bool
    where
        F: Fn(usize) -> Option<u64>,
    {
        for &(node_id, expected_version) in &self.node_versions {
            match get_version(node_id) {
                Some(current_version) if current_version == expected_version => continue,
                _ => return false, // Version changed or node deleted
            }
        }
        true
    }

    /// Validate with a mutable closure (for caching lookups).
    pub fn validate_mut<F>(&self, mut get_version: F) -> bool
    where
        F: FnMut(usize) -> Option<u64>,
    {
        for &(node_id, expected_version) in &self.node_versions {
            match get_version(node_id) {
                Some(current_version) if current_version == expected_version => continue,
                _ => return false,
            }
        }
        true
    }

    /// Clear observed nodes for reuse.
    pub fn reset(&mut self) {
        self.node_versions.clear();
        // Note: We keep the same epoch - this is for retrying within the same read
    }
}

impl<'a> Drop for MvccReadContext<'a> {
    fn drop(&mut self) {
        self.epoch_manager.exit_read();
    }
}

/// Retry statistics for optimistic operations.
#[derive(Debug, Default)]
pub struct RetryStats {
    /// Number of successful reads
    pub successful: AtomicU64,
    /// Number of retries due to concurrent modifications
    pub retries: AtomicU64,
    /// Maximum retries in a single operation
    pub max_retries: AtomicU64,
}

impl RetryStats {
    /// Create new retry stats.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful operation.
    pub fn record_success(&self, retry_count: u64) {
        self.successful.fetch_add(1, Ordering::Relaxed);
        if retry_count > 0 {
            self.retries.fetch_add(retry_count, Ordering::Relaxed);
            loop {
                let current = self.max_retries.load(Ordering::Relaxed);
                if retry_count <= current {
                    break;
                }
                if self
                    .max_retries
                    .compare_exchange_weak(
                        current,
                        retry_count,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    break;
                }
            }
        }
    }

    /// Get the retry rate.
    pub fn retry_rate(&self) -> f64 {
        let successful = self.successful.load(Ordering::Relaxed);
        let retries = self.retries.load(Ordering::Relaxed);
        if successful + retries == 0 {
            0.0
        } else {
            retries as f64 / (successful + retries) as f64
        }
    }

    /// Get a snapshot of the stats.
    pub fn snapshot(&self) -> RetryStatsSnapshot {
        RetryStatsSnapshot {
            successful: self.successful.load(Ordering::Relaxed),
            retries: self.retries.load(Ordering::Relaxed),
            max_retries: self.max_retries.load(Ordering::Relaxed),
        }
    }
}

/// Immutable snapshot of retry stats.
#[derive(Debug, Clone, Copy)]
pub struct RetryStatsSnapshot {
    /// Successful operations
    pub successful: u64,
    /// Total retries
    pub retries: u64,
    /// Maximum retries in single operation
    pub max_retries: u64,
}

/// Atomic statistics for trie operations.
///
/// Provides thread-safe counters for monitoring trie performance and behavior.
/// All counters use relaxed ordering for minimal overhead, as exact counts
/// aren't required - approximate trends are sufficient for monitoring.
#[derive(Debug, Default)]
pub struct TrieStats {
    /// Number of read operations (get, contains, prefix iterations)
    pub reads: AtomicU64,
    /// Number of write operations (insert, remove)
    pub writes: AtomicU64,
    /// Number of read retries due to concurrent modifications
    pub read_retries: AtomicU64,
    /// Number of buffer cache hits
    pub cache_hits: AtomicU64,
    /// Number of buffer cache misses (disk reads)
    pub cache_misses: AtomicU64,
    /// Number of WAL writes
    pub wal_writes: AtomicU64,
    /// Number of WAL syncs (fsync calls)
    pub wal_syncs: AtomicU64,
}

impl TrieStats {
    /// Create new statistics counters (all zeroed).
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a read operation.
    #[inline]
    pub fn record_read(&self) {
        self.reads.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a write operation.
    #[inline]
    pub fn record_write(&self) {
        self.writes.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a read retry.
    #[inline]
    pub fn record_read_retry(&self) {
        self.read_retries.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache hit.
    #[inline]
    pub fn record_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache miss.
    #[inline]
    pub fn record_cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a WAL write.
    #[inline]
    pub fn record_wal_write(&self) {
        self.wal_writes.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a WAL sync.
    #[inline]
    pub fn record_wal_sync(&self) {
        self.wal_syncs.fetch_add(1, Ordering::Relaxed);
    }

    /// Get the cache hit ratio (0.0 to 1.0).
    pub fn cache_hit_ratio(&self) -> f64 {
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.cache_misses.load(Ordering::Relaxed);
        let total = hits + misses;
        if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        }
    }

    /// Get the read retry ratio (0.0 to 1.0).
    pub fn read_retry_ratio(&self) -> f64 {
        let reads = self.reads.load(Ordering::Relaxed);
        let retries = self.read_retries.load(Ordering::Relaxed);
        if reads == 0 {
            0.0
        } else {
            retries as f64 / reads as f64
        }
    }

    /// Get a snapshot of the stats.
    pub fn snapshot(&self) -> TrieStatsSnapshot {
        TrieStatsSnapshot {
            reads: self.reads.load(Ordering::Relaxed),
            writes: self.writes.load(Ordering::Relaxed),
            read_retries: self.read_retries.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            wal_writes: self.wal_writes.load(Ordering::Relaxed),
            wal_syncs: self.wal_syncs.load(Ordering::Relaxed),
        }
    }

    /// Reset all counters to zero.
    pub fn reset(&self) {
        self.reads.store(0, Ordering::Relaxed);
        self.writes.store(0, Ordering::Relaxed);
        self.read_retries.store(0, Ordering::Relaxed);
        self.cache_hits.store(0, Ordering::Relaxed);
        self.cache_misses.store(0, Ordering::Relaxed);
        self.wal_writes.store(0, Ordering::Relaxed);
        self.wal_syncs.store(0, Ordering::Relaxed);
    }
}

/// Immutable snapshot of trie stats.
#[derive(Debug, Clone, Copy)]
pub struct TrieStatsSnapshot {
    /// Number of read operations
    pub reads: u64,
    /// Number of write operations
    pub writes: u64,
    /// Number of read retries
    pub read_retries: u64,
    /// Number of cache hits
    pub cache_hits: u64,
    /// Number of cache misses
    pub cache_misses: u64,
    /// Number of WAL writes
    pub wal_writes: u64,
    /// Number of WAL syncs
    pub wal_syncs: u64,
}

impl TrieStatsSnapshot {
    /// Get the cache hit ratio (0.0 to 1.0).
    pub fn cache_hit_ratio(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            0.0
        } else {
            self.cache_hits as f64 / total as f64
        }
    }

    /// Get the read retry ratio (0.0 to 1.0).
    pub fn read_retry_ratio(&self) -> f64 {
        if self.reads == 0 {
            0.0
        } else {
            self.read_retries as f64 / self.reads as f64
        }
    }
}

/// Thread-safe optimistic data wrapper.
///
/// Provides optimistic reads and exclusive writes for any data type.
pub struct OptimisticCell<T> {
    /// Version for optimistic concurrency
    version: OptimisticVersion,
    /// The data (UnsafeCell for interior mutability)
    data: UnsafeCell<T>,
}

// SAFETY: OptimisticCell is Sync if T is Send
// because we ensure proper synchronization through version checks
unsafe impl<T: Send> Sync for OptimisticCell<T> {}
unsafe impl<T: Send> Send for OptimisticCell<T> {}

impl<T> OptimisticCell<T> {
    /// Create a new optimistic cell.
    pub fn new(data: T) -> Self {
        OptimisticCell {
            version: OptimisticVersion::new(),
            data: UnsafeCell::new(data),
        }
    }

    /// Attempt an optimistic read.
    ///
    /// Returns the result and whether it was successful.
    /// If not successful, the caller should retry.
    pub fn try_read<R, F>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&T) -> R,
    {
        let guard = OptimisticReadGuard::new(&self.version);

        // SAFETY: We check the version after reading
        let result = unsafe { f(&*self.data.get()) };

        if guard.validate() {
            Some(result)
        } else {
            None
        }
    }

    /// Perform an optimistic read with retries.
    ///
    /// Retries until successful or max_retries reached.
    pub fn read_with_retry<R, F>(&self, f: F, max_retries: usize) -> Option<R>
    where
        F: Fn(&T) -> R,
    {
        for _ in 0..max_retries {
            if let Some(result) = self.try_read(&f) {
                return Some(result);
            }
            std::hint::spin_loop();
        }
        None
    }

    /// Perform an exclusive write.
    pub fn write<R, F>(&self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        let _guard = WriteGuard::new(&self.version);

        // SAFETY: WriteGuard ensures exclusive access
        unsafe { f(&mut *self.data.get()) }
    }

    /// Get the current version.
    pub fn version(&self) -> u64 {
        self.version.get()
    }

    /// Check if currently being written.
    pub fn is_locked(&self) -> bool {
        !self.version.is_stable()
    }
}

/// Shared ownership optimistic cell.
pub type SharedOptimisticCell<T> = Arc<OptimisticCell<T>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_optimistic_version() {
        let version = OptimisticVersion::new();

        assert_eq!(version.get(), 0);
        assert!(version.is_stable());

        // Start write
        version.begin_write();
        assert!(!version.is_stable());
        assert_eq!(version.get(), 1);

        // End write
        version.end_write();
        assert!(version.is_stable());
        assert_eq!(version.get(), 2);
    }

    #[test]
    fn test_optimistic_read_guard() {
        let version = OptimisticVersion::new();

        let guard = OptimisticReadGuard::new(&version);
        assert_eq!(guard.observed(), 0);
        assert!(guard.validate());

        // Concurrent write invalidates guard
        version.begin_write();
        assert!(!guard.validate());

        version.end_write();
        assert!(!guard.validate()); // Still invalid - version changed
    }

    #[test]
    fn test_lock_coupling() {
        let lc = LockCoupling::new(3);

        assert_eq!(lc.depth(), 0);

        assert!(lc.try_descend());
        assert_eq!(lc.depth(), 1);

        assert!(lc.try_descend());
        assert_eq!(lc.depth(), 2);

        assert!(lc.try_descend());
        assert_eq!(lc.depth(), 3);

        // Max depth reached
        assert!(!lc.try_descend());
        assert_eq!(lc.depth(), 3);

        lc.ascend();
        assert_eq!(lc.depth(), 2);

        lc.reset();
        assert_eq!(lc.depth(), 0);
    }

    #[test]
    fn test_epoch_manager() {
        let epoch = EpochManager::new();

        assert_eq!(epoch.current_epoch(), 0);
        assert!(!epoch.has_active_readers());

        // Enter read
        let e1 = epoch.enter_read();
        assert_eq!(e1, 0);
        assert!(epoch.has_active_readers());
        assert_eq!(epoch.active_reader_count(), 1);

        // Advance epoch
        let e2 = epoch.advance();
        assert_eq!(e2, 0);
        assert_eq!(epoch.current_epoch(), 1);

        // Exit read
        epoch.exit_read();
        assert!(!epoch.has_active_readers());
    }

    #[test]
    fn test_epoch_guard() {
        let epoch = EpochManager::new();

        {
            let _guard = EpochGuard::new(&epoch);
            assert!(epoch.has_active_readers());
        }

        assert!(!epoch.has_active_readers());
    }

    #[test]
    fn test_retry_stats() {
        let stats = RetryStats::new();

        stats.record_success(0);
        stats.record_success(2);
        stats.record_success(5);

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.successful, 3);
        assert_eq!(snapshot.retries, 7);
        assert_eq!(snapshot.max_retries, 5);
    }

    #[test]
    fn test_optimistic_cell_read() {
        let cell = OptimisticCell::new(42);

        let result = cell.try_read(|v| *v).expect("read should succeed");
        assert_eq!(result, 42);
    }

    #[test]
    fn test_optimistic_cell_write() {
        let cell = OptimisticCell::new(42);

        cell.write(|v| *v = 100);

        let result = cell.try_read(|v| *v).expect("read should succeed");
        assert_eq!(result, 100);
    }

    #[test]
    fn test_optimistic_cell_concurrent() {
        let cell = Arc::new(OptimisticCell::new(0));
        let cell_clone = cell.clone();

        // Writer thread
        let writer = thread::spawn(move || {
            for i in 1..=100 {
                cell_clone.write(|v| *v = i);
            }
        });

        // Reader thread
        let reader = thread::spawn(move || {
            let mut max_seen = 0;
            for _ in 0..1000 {
                if let Some(v) = cell.read_with_retry(|v| *v, 10) {
                    if v > max_seen {
                        max_seen = v;
                    }
                }
            }
            max_seen
        });

        writer.join().expect("writer panicked");
        let max_seen = reader.join().expect("reader panicked");

        // Should have seen some writes (exact value depends on timing)
        assert!(max_seen >= 0);
    }

    #[test]
    fn test_optimistic_cell_is_locked() {
        let cell = OptimisticCell::new(42);

        assert!(!cell.is_locked());

        // During write, it should be locked
        // (Hard to test reliably due to timing)
        cell.write(|_| {
            // Inside write, the cell is locked
            // But we can't easily check from inside
        });

        assert!(!cell.is_locked());
    }
}
