//! LRU cache for hot reverse lookups (index → term).
//!
//! This module provides [`VocabReverseCache`], an LRU cache that stores
//! recently accessed (index, term) pairs to avoid repeated parent pointer
//! backtracking for hot lookups.
//!
//! # Design
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                   VocabReverseCache                          │
//! ├─────────────────────────────────────────────────────────────┤
//! │  ┌─────────────────────────────────────────────────────────┐│
//! │  │  LRU Cache: u64 → String                                ││
//! │  │  - Capacity: configurable (default 50K)                 ││
//! │  │  - Eviction: Least Recently Used                        ││
//! │  └─────────────────────────────────────────────────────────┘│
//! │                                                              │
//! │  get(42) → Some("hello") if cached                          │
//! │  put(42, "hello") → updates LRU position                    │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Performance
//!
//! | Operation | Complexity | Notes                          |
//! |-----------|------------|--------------------------------|
//! | get       | O(1)       | Hash lookup + LRU update       |
//! | put       | O(1)       | Hash insert + potential evict  |
//! | contains  | O(1)       | Hash lookup only               |

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};

use lru::LruCache;

use parking_lot::Mutex;

use super::types::DEFAULT_REVERSE_CACHE_SIZE;

/// LRU cache for reverse lookups (vocabulary index → term string).
///
/// This cache provides O(1) access to frequently used (index, term) pairs,
/// avoiding the O(k) cost of parent pointer backtracking for hot lookups.
///
/// # Thread Safety
///
/// The cache uses internal locking (parking_lot::Mutex or std::sync::Mutex)
/// for thread-safe access.
///
/// # Example
///
/// ```rust,ignore
/// use libdictenstein::persistent_vocab_artrie::reverse_cache::VocabReverseCache;
///
/// let cache = VocabReverseCache::new(1000);
///
/// // Cache miss - need to reconstruct from trie
/// assert!(cache.get(42).is_none());
///
/// // Cache the result
/// cache.put(42, "hello".to_string());
///
/// // Cache hit - O(1)
/// assert_eq!(cache.get(42), Some("hello".to_string()));
/// ```
pub struct VocabReverseCache {
    /// The underlying LRU cache
    cache: Mutex<LruCache<u64, String>>,
    /// Cache capacity
    capacity: usize,
    /// Statistics: number of cache hits
    hits: AtomicU64,
    /// Statistics: number of cache misses
    misses: AtomicU64,
}

impl VocabReverseCache {
    /// Create a new cache with the specified capacity.
    ///
    /// # Arguments
    ///
    /// * `capacity` - Maximum number of entries to cache
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).expect("capacity must be > 0");
        Self {
            cache: Mutex::new(LruCache::new(cap)),
            capacity,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Create a new cache with the default capacity (50,000 entries).
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_REVERSE_CACHE_SIZE)
    }

    /// Get the term for a vocabulary index, if cached.
    ///
    /// # Performance
    ///
    /// O(1) - hash lookup + LRU position update
    ///
    /// # Returns
    ///
    /// `Some(term)` if the index is in the cache, `None` otherwise.
    pub fn get(&self, index: u64) -> Option<String> {
        let mut cache = self.cache.lock();

        match cache.get(&index) {
            Some(term) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(term.clone())
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Get the term for a vocabulary index without updating LRU position.
    ///
    /// This is useful for checking if an entry exists without affecting
    /// the cache eviction order.
    pub fn peek(&self, index: u64) -> Option<String> {
        let cache = self.cache.lock();

        cache.peek(&index).cloned()
    }

    /// Cache a (index, term) pair.
    ///
    /// If the cache is at capacity, the least recently used entry is evicted.
    ///
    /// # Performance
    ///
    /// O(1) - hash insert + potential eviction
    pub fn put(&self, index: u64, term: String) {
        let mut cache = self.cache.lock();

        cache.put(index, term);
    }

    /// Check if an index is in the cache without updating LRU position.
    #[inline]
    pub fn contains(&self, index: u64) -> bool {
        let cache = self.cache.lock();

        cache.contains(&index)
    }

    /// Remove an entry from the cache.
    ///
    /// Returns the removed term if it was present.
    pub fn remove(&self, index: u64) -> Option<String> {
        let mut cache = self.cache.lock();

        cache.pop(&index)
    }

    /// Clear all entries from the cache.
    pub fn clear(&self) {
        let mut cache = self.cache.lock();

        cache.clear();
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
    }

    /// Get the current number of cached entries.
    pub fn len(&self) -> usize {
        let cache = self.cache.lock();

        cache.len()
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the cache capacity.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;
        let hit_rate = if total > 0 {
            hits as f64 / total as f64
        } else {
            0.0
        };

        CacheStats {
            hits,
            misses,
            hit_rate,
            size: self.len(),
            capacity: self.capacity,
        }
    }

    /// Reset statistics counters.
    pub fn reset_stats(&self) {
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
    }
}

impl Default for VocabReverseCache {
    fn default() -> Self {
        Self::with_default_capacity()
    }
}

impl std::fmt::Debug for VocabReverseCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stats = self.stats();
        f.debug_struct("VocabReverseCache")
            .field("size", &stats.size)
            .field("capacity", &stats.capacity)
            .field("hit_rate", &format!("{:.2}%", stats.hit_rate * 100.0))
            .finish()
    }
}

/// Statistics about cache performance.
#[derive(Debug, Clone)]
pub struct CacheStats {
    /// Number of cache hits
    pub hits: u64,
    /// Number of cache misses
    pub misses: u64,
    /// Hit rate (0.0 to 1.0)
    pub hit_rate: f64,
    /// Current number of cached entries
    pub size: usize,
    /// Maximum capacity
    pub capacity: usize,
}

impl std::fmt::Display for CacheStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CacheStats {{ hits: {}, misses: {}, hit_rate: {:.2}%, size: {}/{} }}",
            self.hits,
            self.misses,
            self.hit_rate * 100.0,
            self.size,
            self.capacity
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_basic() {
        let cache = VocabReverseCache::new(10);

        // Initial state
        assert!(cache.is_empty());
        assert_eq!(cache.capacity(), 10);

        // Put and get
        cache.put(1, "one".to_string());
        cache.put(2, "two".to_string());
        cache.put(3, "three".to_string());

        assert_eq!(cache.len(), 3);
        assert_eq!(cache.get(1), Some("one".to_string()));
        assert_eq!(cache.get(2), Some("two".to_string()));
        assert_eq!(cache.get(3), Some("three".to_string()));
        assert_eq!(cache.get(4), None);
    }

    #[test]
    fn test_cache_eviction() {
        let cache = VocabReverseCache::new(3);

        // Fill the cache
        cache.put(1, "one".to_string());
        cache.put(2, "two".to_string());
        cache.put(3, "three".to_string());

        assert_eq!(cache.len(), 3);

        // Access 1 and 2 to make them more recently used
        cache.get(1);
        cache.get(2);

        // Add a 4th entry - should evict 3 (least recently used)
        cache.put(4, "four".to_string());

        assert_eq!(cache.len(), 3);
        assert_eq!(cache.get(1), Some("one".to_string()));
        assert_eq!(cache.get(2), Some("two".to_string()));
        assert_eq!(cache.get(3), None); // Evicted
        assert_eq!(cache.get(4), Some("four".to_string()));
    }

    #[test]
    fn test_cache_stats() {
        let cache = VocabReverseCache::new(10);

        cache.put(1, "one".to_string());
        cache.put(2, "two".to_string());

        // 2 hits, 1 miss
        cache.get(1); // hit
        cache.get(2); // hit
        cache.get(3); // miss

        let stats = cache.stats();
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 1);
        assert!((stats.hit_rate - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_cache_peek() {
        let cache = VocabReverseCache::new(3);

        cache.put(1, "one".to_string());
        cache.put(2, "two".to_string());
        cache.put(3, "three".to_string());

        // Peek at 1 - should not affect LRU order
        assert_eq!(cache.peek(1), Some("one".to_string()));

        // Add 4th entry without accessing 1
        cache.put(4, "four".to_string());

        // 1 should be evicted because peek doesn't update LRU
        assert_eq!(cache.get(1), None);
        assert_eq!(cache.get(4), Some("four".to_string()));
    }

    #[test]
    fn test_cache_remove() {
        let cache = VocabReverseCache::new(10);

        cache.put(1, "one".to_string());
        cache.put(2, "two".to_string());

        let removed = cache.remove(1);
        assert_eq!(removed, Some("one".to_string()));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(1), None);
        assert_eq!(cache.get(2), Some("two".to_string()));
    }

    #[test]
    fn test_cache_clear() {
        let cache = VocabReverseCache::new(10);

        cache.put(1, "one".to_string());
        cache.put(2, "two".to_string());
        cache.get(1); // Generate some stats

        cache.clear();

        assert!(cache.is_empty());
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
    }

    #[test]
    fn test_cache_update() {
        let cache = VocabReverseCache::new(10);

        cache.put(1, "one".to_string());
        assert_eq!(cache.get(1), Some("one".to_string()));

        // Update value
        cache.put(1, "ONE".to_string());
        assert_eq!(cache.get(1), Some("ONE".to_string()));
        assert_eq!(cache.len(), 1);
    }
}
