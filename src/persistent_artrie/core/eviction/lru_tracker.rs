//! LRU (Least Recently Used) tracking for eviction selection.
//!
//! This module provides access tracking to enable smarter eviction decisions.
//! Nodes are evicted in LRU order (coldest first), keeping hot data in memory.

use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use dashmap::DashMap;

/// Lightweight access timestamp for LRU tracking.
///
/// Stored inline in nodes or in a separate registry. Uses atomic operations
/// for lock-free updates during traversal.
///
/// # Implementation Note
///
/// The `last_access` timestamp is relative to the registry's epoch start,
/// stored in microseconds for space efficiency. This limits the maximum
/// trackable duration to ~584,000 years, which should be sufficient.
#[derive(Debug)]
pub struct AccessTracker {
    /// Timestamp of last access (epoch-relative microseconds).
    last_access: AtomicU64,
    /// Access count (for tie-breaking when timestamps are equal).
    access_count: AtomicU64,
}

impl AccessTracker {
    /// Create a new tracker with current time.
    pub fn new() -> Self {
        Self {
            last_access: AtomicU64::new(0),
            access_count: AtomicU64::new(0),
        }
    }

    /// Create a tracker with a specific initial timestamp.
    pub fn with_timestamp(timestamp_us: u64) -> Self {
        Self {
            last_access: AtomicU64::new(timestamp_us),
            access_count: AtomicU64::new(1),
        }
    }

    /// Record an access (called during traversal).
    ///
    /// This is lock-free and safe to call from multiple threads.
    #[inline]
    pub fn touch(&self, now_us: u64) {
        self.last_access.store(now_us, Ordering::Relaxed);
        self.access_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get last access timestamp (microseconds since epoch).
    #[inline]
    pub fn last_access(&self) -> u64 {
        self.last_access.load(Ordering::Relaxed)
    }

    /// Get total access count.
    #[inline]
    pub fn access_count(&self) -> u64 {
        self.access_count.load(Ordering::Relaxed)
    }

    /// Check if this tracker is older than another.
    ///
    /// Uses access count as a tie-breaker when timestamps are equal.
    pub fn is_older_than(&self, other: &AccessTracker) -> bool {
        let self_time = self.last_access();
        let other_time = other.last_access();

        if self_time != other_time {
            self_time < other_time
        } else {
            // Tie-breaker: fewer accesses = colder
            self.access_count() < other.access_count()
        }
    }

    /// Get a "coldness" score for sorting (higher = colder = evict first).
    ///
    /// This combines recency and frequency into a single comparable value.
    pub fn coldness_score(&self, now_us: u64) -> u64 {
        let age = now_us.saturating_sub(self.last_access());
        let count = self.access_count().max(1);

        // Simple LRU: age is the primary factor
        // Divide by access count to give frequently-accessed nodes a boost
        age / count
    }
}

impl Default for AccessTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for AccessTracker {
    fn clone(&self) -> Self {
        Self {
            last_access: AtomicU64::new(self.last_access.load(Ordering::Relaxed)),
            access_count: AtomicU64::new(self.access_count.load(Ordering::Relaxed)),
        }
    }
}

/// Hash a node path to a u64 for registry lookup.
///
/// Uses FNV-1a for speed; collisions are acceptable since this is
/// just for LRU approximation, not correctness.
fn hash_path(path: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for &byte in path {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Alternative path hasher using the standard library.
fn hash_path_std<T: Hash>(path: &[T]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

/// Registry for node access tracking.
///
/// Maps node path hashes to access trackers. This is an alternative to
/// inline tracking that doesn't require modifying the node structure.
///
/// # Memory Overhead
///
/// Each tracked node uses approximately 32 bytes:
/// - 8 bytes for the path hash key
/// - 16 bytes for the AccessTracker
/// - 8 bytes for DashMap overhead
///
/// For a trie with 1M nodes, this is ~32MB of tracking overhead.
pub struct LruRegistry {
    /// Maps node path hash to access tracker.
    trackers: DashMap<u64, AccessTracker>,
    /// Start time for relative timestamps.
    epoch_start: Instant,
    /// Maximum entries to track (older entries are pruned).
    max_entries: usize,
}

impl LruRegistry {
    /// Create a new LRU registry with default capacity.
    pub fn new() -> Self {
        Self::with_capacity(1_000_000)
    }

    /// Create a new LRU registry with specified capacity.
    ///
    /// The registry will track up to `capacity` nodes. When full,
    /// older entries may be pruned during eviction selection.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            trackers: DashMap::with_capacity(capacity.min(65536)),
            epoch_start: Instant::now(),
            max_entries: capacity,
        }
    }

    /// Record an access for a node path.
    ///
    /// This is the primary interface for tracking node accesses during
    /// trie traversal.
    #[inline]
    pub fn touch(&self, path: &[u8]) {
        let hash = hash_path(path);
        let now = self.now_us();

        // Try to update existing entry first (common case)
        if let Some(tracker) = self.trackers.get(&hash) {
            tracker.touch(now);
            return;
        }

        // Insert new entry
        // Note: There's a tiny race window here, but it's harmless for LRU purposes
        if self.trackers.len() < self.max_entries {
            self.trackers
                .insert(hash, AccessTracker::with_timestamp(now));
        }
    }

    /// Record an access for a node with a pre-computed hash.
    #[inline]
    pub fn touch_hash(&self, hash: u64) {
        let now = self.now_us();

        if let Some(tracker) = self.trackers.get(&hash) {
            tracker.touch(now);
            return;
        }

        if self.trackers.len() < self.max_entries {
            self.trackers
                .insert(hash, AccessTracker::with_timestamp(now));
        }
    }

    /// Get the last access time for a node path.
    ///
    /// Returns `None` if the path hasn't been tracked.
    pub fn last_access(&self, path: &[u8]) -> Option<u64> {
        let hash = hash_path(path);
        self.trackers.get(&hash).map(|t| t.last_access())
    }

    /// Get the coldness score for a node path.
    ///
    /// Higher scores indicate colder (less recently used) nodes
    /// that should be evicted first.
    pub fn coldness_score(&self, path: &[u8]) -> u64 {
        let hash = hash_path(path);
        let now = self.now_us();

        self.trackers
            .get(&hash)
            .map(|t| t.coldness_score(now))
            .unwrap_or(u64::MAX) // Untracked nodes are coldest
    }

    /// Get the coldness score for a pre-computed hash.
    pub fn coldness_score_hash(&self, hash: u64) -> u64 {
        let now = self.now_us();

        self.trackers
            .get(&hash)
            .map(|t| t.coldness_score(now))
            .unwrap_or(u64::MAX)
    }

    /// Remove tracking for a node path (called after eviction).
    pub fn remove(&self, path: &[u8]) {
        let hash = hash_path(path);
        self.trackers.remove(&hash);
    }

    /// Remove tracking for a pre-computed hash.
    pub fn remove_hash(&self, hash: u64) {
        self.trackers.remove(&hash);
    }

    /// Get the number of tracked nodes.
    pub fn len(&self) -> usize {
        self.trackers.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.trackers.is_empty()
    }

    /// Clear all tracking data.
    pub fn clear(&self) {
        self.trackers.clear();
    }

    /// Get the N coldest path hashes for eviction.
    ///
    /// Returns up to `n` hashes sorted by coldness (coldest first).
    pub fn coldest_n(&self, n: usize) -> Vec<u64> {
        let now = self.now_us();
        let mut entries: Vec<_> = self
            .trackers
            .iter()
            .map(|entry| (*entry.key(), entry.value().coldness_score(now)))
            .collect();

        // Partial sort for efficiency when n << len
        let len = entries.len();
        if n < len / 4 {
            entries.select_nth_unstable_by_key(n.min(len.saturating_sub(1)), |(_h, score)| {
                std::cmp::Reverse(*score)
            });
            entries.truncate(n);
        } else {
            entries.sort_unstable_by_key(|(_h, score)| std::cmp::Reverse(*score));
            entries.truncate(n);
        }

        entries.into_iter().map(|(h, _)| h).collect()
    }

    /// Prune the registry to target size by removing coldest entries.
    ///
    /// Returns the number of entries removed.
    pub fn prune_to(&self, target_size: usize) -> usize {
        if self.trackers.len() <= target_size {
            return 0;
        }

        let to_remove = self.trackers.len() - target_size;
        let coldest = self.coldest_n(to_remove);

        for hash in &coldest {
            self.trackers.remove(hash);
        }

        coldest.len()
    }

    /// Get current timestamp in microseconds since epoch start.
    #[inline]
    fn now_us(&self) -> u64 {
        self.epoch_start.elapsed().as_micros() as u64
    }

    /// Compute the hash for a path (exposed for external use).
    pub fn path_hash(path: &[u8]) -> u64 {
        hash_path(path)
    }
}

impl Default for LruRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute the hash for a char path (for PersistentARTrieChar).
pub fn hash_char_path(path: &[char]) -> u64 {
    hash_path_std(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_access_tracker_basic() {
        let tracker = AccessTracker::new();
        assert_eq!(tracker.last_access(), 0);
        assert_eq!(tracker.access_count(), 0);

        tracker.touch(1000);
        assert_eq!(tracker.last_access(), 1000);
        assert_eq!(tracker.access_count(), 1);

        tracker.touch(2000);
        assert_eq!(tracker.last_access(), 2000);
        assert_eq!(tracker.access_count(), 2);
    }

    #[test]
    fn test_access_tracker_comparison() {
        let old = AccessTracker::with_timestamp(1000);
        let new = AccessTracker::with_timestamp(2000);

        assert!(old.is_older_than(&new));
        assert!(!new.is_older_than(&old));
    }

    #[test]
    fn test_access_tracker_coldness() {
        let tracker = AccessTracker::with_timestamp(1000);
        let now = 2000;

        // Age is 1000us, count is 1 -> score is 1000
        let score1 = tracker.coldness_score(now);

        // Access twice more
        tracker.touch(1500);
        tracker.touch(1500);

        // Now count is 3, age is 500 -> score is ~166
        let score2 = tracker.coldness_score(now);

        // More accesses = lower coldness score (hotter)
        assert!(score2 < score1);
    }

    #[test]
    fn test_lru_registry_basic() {
        let registry = LruRegistry::new();
        assert!(registry.is_empty());

        registry.touch(b"hello");
        assert_eq!(registry.len(), 1);

        registry.touch(b"world");
        assert_eq!(registry.len(), 2);

        registry.remove(b"hello");
        assert_eq!(registry.len(), 1);

        registry.clear();
        assert!(registry.is_empty());
    }

    #[test]
    fn test_lru_registry_last_access() {
        let registry = LruRegistry::new();

        assert!(registry.last_access(b"missing").is_none());

        registry.touch(b"test");
        let access1 = registry.last_access(b"test");
        assert!(access1.is_some());

        thread::sleep(Duration::from_micros(100));

        registry.touch(b"test");
        let access2 = registry.last_access(b"test");
        assert!(access2.unwrap() > access1.unwrap());
    }

    #[test]
    fn test_lru_registry_coldest() {
        let registry = LruRegistry::new();

        // Touch paths with delays to create different coldness
        registry.touch(b"cold1");
        thread::sleep(Duration::from_micros(100));
        registry.touch(b"cold2");
        thread::sleep(Duration::from_micros(100));
        registry.touch(b"hot");

        // Touch "hot" multiple times
        for _ in 0..10 {
            registry.touch(b"hot");
        }

        let coldest = registry.coldest_n(2);
        assert_eq!(coldest.len(), 2);

        // The coldest hashes should be cold1 and cold2, not hot
        let hot_hash = hash_path(b"hot");
        assert!(!coldest.contains(&hot_hash));
    }

    #[test]
    fn test_lru_registry_prune() {
        let registry = LruRegistry::with_capacity(100);

        for i in 0..50 {
            registry.touch(&[i as u8]);
        }
        assert_eq!(registry.len(), 50);

        let removed = registry.prune_to(30);
        assert_eq!(removed, 20);
        assert_eq!(registry.len(), 30);
    }

    #[test]
    fn test_hash_path_deterministic() {
        let path = b"test/path/to/node";
        let hash1 = hash_path(path);
        let hash2 = hash_path(path);
        assert_eq!(hash1, hash2);

        // Different paths should (usually) have different hashes
        let hash3 = hash_path(b"different/path");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;

        let registry = Arc::new(LruRegistry::new());
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let reg = Arc::clone(&registry);
                thread::spawn(move || {
                    for j in 0..100 {
                        let path = format!("thread{}path{}", i, j);
                        reg.touch(path.as_bytes());
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("thread panicked");
        }

        // Should have tracked up to 400 unique paths (4 threads × 100 paths)
        assert!(registry.len() <= 400);
        assert!(registry.len() >= 200); // At least some should be tracked
    }
}
