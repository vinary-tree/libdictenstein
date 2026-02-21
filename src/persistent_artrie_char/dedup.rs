//! NodeDeduplicator - Hash-based deduplication for space efficiency (char version)
//!
//! This module provides hash-based lookup for identical node data, allowing
//! reuse of existing allocations instead of duplicating data.
//!
//! ## Problem
//!
//! Many trie structures have redundant subtrees:
//! ```text
//! N-gram data has common suffixes:
//! "the_cat" and "the_dog" share "the_" prefix nodes
//! Without dedup: 2 copies of identical prefix nodes
//! With dedup: 1 copy, 2 references
//! ```
//!
//! ## Solution
//!
//! Hash node data before allocation, check cache for existing copy:
//! ```text
//! allocate(data):
//!   hash = xxhash3(data)
//!   if cache[hash] exists && verify_data_matches:
//!     return cache[hash]  // Reuse existing
//!   else:
//!     slot = arena.allocate(data)
//!     cache[hash] = slot
//!     return slot
//! ```
//!
//! ## Hash Function Choice
//!
//! Uses xxHash3 (via `xxhash-rust` crate) for hashing node data:
//! - **2-3x faster than FNV-1a** for short inputs (<16 bytes)
//! - **5-10x faster than FNV-1a** for medium-long inputs
//! - **Excellent hash quality** with good avalanche properties
//! - **SIMD-accelerated** using AVX2/SSE when available
//! - **Safe implementation** with no buffer overflow risks
//!
//! ## Expected Impact
//!
//! - **Space**: 10-30% reduction for redundant subtrees
//! - **Best for**: Dictionaries with common prefixes/suffixes

use std::collections::HashMap;

use super::arena_manager::{ArenaManager, ArenaSlot};
use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::disk_manager::MmapDiskManager;
use crate::persistent_artrie::PersistentARTrieError;

type Result<T> = std::result::Result<T, PersistentARTrieError>;

/// Hash function for node data using xxHash3
///
/// xxHash3 provides excellent performance across all input sizes:
/// - Short inputs (<16B): ~15-25 cycles (2-3x faster than FNV-1a)
/// - Medium inputs (16-64B): ~20-35 cycles (5-10x faster than FNV-1a)
/// - Long inputs (>256B): ~0.3 cycles/byte (15-20x faster than FNV-1a)
///
/// It also has excellent avalanche properties for fewer hash collisions.
#[inline]
fn compute_hash(data: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64(data)
}

/// NodeDeduplicator - Hash-based deduplication for node data
///
/// This struct maintains a cache mapping data hashes to arena slots.
/// Before allocating new data, it checks if identical data already exists.
///
/// Uses xxHash3 for fast, high-quality hashing of node data.
#[derive(Debug)]
pub struct NodeDeduplicator {
    /// Cache mapping hash -> arena slot
    cache: HashMap<u64, ArenaSlot>,
    /// Statistics
    hits: u64,
    misses: u64,
    collisions: u64,
}

impl NodeDeduplicator {
    /// Create a new deduplicator
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            hits: 0,
            misses: 0,
            collisions: 0,
        }
    }

    /// Create a deduplicator with estimated capacity
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            cache: HashMap::with_capacity(capacity),
            hits: 0,
            misses: 0,
            collisions: 0,
        }
    }

    /// Compute hash for data using xxHash3
    #[inline]
    fn hash(&self, data: &[u8]) -> u64 {
        compute_hash(data)
    }

    /// Check if data exists in cache
    ///
    /// Returns Some(slot) if found, None if not cached.
    /// Note: This doesn't verify data matches (potential false positive).
    pub fn lookup(&self, data: &[u8]) -> Option<ArenaSlot> {
        let hash = self.hash(data);
        self.cache.get(&hash).copied()
    }

    /// Insert a new entry into the cache
    ///
    /// Call this after allocating new data.
    pub fn insert(&mut self, data: &[u8], slot: ArenaSlot) {
        let hash = self.hash(data);
        self.cache.insert(hash, slot);
    }

    /// Record a cache hit
    pub fn record_hit(&mut self) {
        self.hits += 1;
    }

    /// Record a cache miss
    pub fn record_miss(&mut self) {
        self.misses += 1;
    }

    /// Record a hash collision (different data, same hash)
    pub fn record_collision(&mut self) {
        self.collisions += 1;
    }

    /// Get statistics
    pub fn stats(&self) -> DeduplicatorStats {
        DeduplicatorStats {
            cache_size: self.cache.len(),
            hits: self.hits,
            misses: self.misses,
            collisions: self.collisions,
        }
    }

    /// Clear the cache
    pub fn clear(&mut self) {
        self.cache.clear();
        self.hits = 0;
        self.misses = 0;
        self.collisions = 0;
    }

    /// Get cache capacity
    pub fn capacity(&self) -> usize {
        self.cache.capacity()
    }

    /// Get number of cached entries
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

impl Default for NodeDeduplicator {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about deduplication effectiveness
#[derive(Debug, Clone)]
pub struct DeduplicatorStats {
    /// Number of entries in cache
    pub cache_size: usize,
    /// Number of cache hits (data reused)
    pub hits: u64,
    /// Number of cache misses (new allocation)
    pub misses: u64,
    /// Number of hash collisions detected
    pub collisions: u64,
}

impl DeduplicatorStats {
    /// Calculate hit rate (0.0 to 1.0)
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    /// Calculate space savings percentage
    ///
    /// Assumes each hit saves `avg_node_size` bytes.
    pub fn space_savings(&self, avg_node_size: usize) -> usize {
        self.hits as usize * avg_node_size
    }
}

/// DeduplicatingArenaManager - ArenaManager wrapper with deduplication
///
/// This wraps an ArenaManager to provide transparent deduplication.
/// When allocating, it first checks the dedup cache.
#[derive(Debug)]
pub struct DeduplicatingArenaManager<S: BlockStorage = MmapDiskManager> {
    /// The underlying arena manager
    arena_manager: ArenaManager<S>,
    /// Deduplication cache
    dedup: NodeDeduplicator,
    /// Whether to verify data on cache hit (slower but safer)
    verify_on_hit: bool,
}

impl<S: BlockStorage> DeduplicatingArenaManager<S> {
    /// Create a new deduplicating arena manager
    pub fn new(arena_manager: ArenaManager<S>) -> Self {
        Self {
            arena_manager,
            dedup: NodeDeduplicator::new(),
            verify_on_hit: true,
        }
    }

    /// Create with dedup capacity hint
    pub fn with_capacity(arena_manager: ArenaManager<S>, dedup_capacity: usize) -> Self {
        Self {
            arena_manager,
            dedup: NodeDeduplicator::with_capacity(dedup_capacity),
            verify_on_hit: true,
        }
    }

    /// Set whether to verify data on cache hit
    ///
    /// If true (default), reads back data to verify it matches before reusing.
    /// If false, trusts the hash and returns cached slot without verification.
    pub fn set_verify_on_hit(&mut self, verify: bool) {
        self.verify_on_hit = verify;
    }

    /// Allocate with deduplication
    ///
    /// Returns existing slot if identical data found, otherwise allocates new.
    pub fn allocate_dedup(&mut self, data: &[u8]) -> Result<ArenaSlot> {
        // Check cache first
        if let Some(slot) = self.dedup.lookup(data) {
            if self.verify_on_hit {
                // Verify data matches
                let existing = self.arena_manager.read(slot)?;
                if existing == data {
                    self.dedup.record_hit();
                    return Ok(slot);
                } else {
                    // Hash collision - different data, same hash
                    self.dedup.record_collision();
                }
            } else {
                // Trust the hash
                self.dedup.record_hit();
                return Ok(slot);
            }
        }

        // Cache miss - allocate new
        self.dedup.record_miss();
        let slot = self.arena_manager.allocate(data)?;
        self.dedup.insert(data, slot);
        Ok(slot)
    }

    /// Allocate without deduplication (bypass cache)
    pub fn allocate_direct(&mut self, data: &[u8]) -> Result<ArenaSlot> {
        self.arena_manager.allocate(data)
    }

    /// Read data from a slot
    pub fn read(&self, slot: ArenaSlot) -> Result<&[u8]> {
        self.arena_manager.read(slot)
    }

    /// Get the underlying arena manager
    pub fn arena_manager(&self) -> &ArenaManager<S> {
        &self.arena_manager
    }

    /// Get mutable access to the underlying arena manager
    pub fn arena_manager_mut(&mut self) -> &mut ArenaManager<S> {
        &mut self.arena_manager
    }

    /// Get deduplication statistics
    pub fn dedup_stats(&self) -> DeduplicatorStats {
        self.dedup.stats()
    }

    /// Clear dedup cache (e.g., after checkpoint)
    pub fn clear_dedup_cache(&mut self) {
        self.dedup.clear();
    }
}

/// Batch deduplicator for collecting hashes across operations
///
/// Use this to build up a dedup cache during a bulk insert,
/// then merge into the main deduplicator.
#[derive(Debug)]
pub struct BatchDeduplicator {
    /// Local cache
    local: NodeDeduplicator,
    /// Batch size threshold for merge hint
    batch_threshold: usize,
}

impl BatchDeduplicator {
    /// Create a new batch deduplicator
    pub fn new(batch_threshold: usize) -> Self {
        Self {
            local: NodeDeduplicator::new(),
            batch_threshold,
        }
    }

    /// Check if data exists in local cache
    pub fn lookup(&self, data: &[u8]) -> Option<ArenaSlot> {
        self.local.lookup(data)
    }

    /// Insert entry into local cache
    pub fn insert(&mut self, data: &[u8], slot: ArenaSlot) {
        self.local.insert(data, slot);
    }

    /// Check if batch should be merged
    pub fn should_merge(&self) -> bool {
        self.local.len() >= self.batch_threshold
    }

    /// Take the local deduplicator for merging
    pub fn take(&mut self) -> NodeDeduplicator {
        std::mem::take(&mut self.local)
    }

    /// Get current cache size
    pub fn len(&self) -> usize {
        self.local.len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.local.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deduplicator_creation() {
        let dedup = NodeDeduplicator::new();
        assert_eq!(dedup.len(), 0);
        assert!(dedup.is_empty());
    }

    #[test]
    fn test_hash_consistency() {
        let data = b"hello world";
        let hash1 = compute_hash(data);
        let hash2 = compute_hash(data);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_different_data() {
        let data1 = b"hello";
        let data2 = b"world";
        let hash1 = compute_hash(data1);
        let hash2 = compute_hash(data2);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_deduplicator_lookup_insert() {
        let mut dedup = NodeDeduplicator::new();

        let data = b"test data";
        let slot = ArenaSlot::new(1, 42);

        // Should not find before insert
        assert!(dedup.lookup(data).is_none());

        // Insert and find
        dedup.insert(data, slot);
        assert_eq!(dedup.lookup(data), Some(slot));
    }

    #[test]
    fn test_deduplicator_stats() {
        let mut dedup = NodeDeduplicator::new();

        dedup.record_hit();
        dedup.record_hit();
        dedup.record_miss();

        let stats = dedup.stats();
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 1);
        assert!((stats.hit_rate() - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_deduplicator_clear() {
        let mut dedup = NodeDeduplicator::new();

        let data = b"test";
        dedup.insert(data, ArenaSlot::new(0, 0));
        dedup.record_hit();

        assert!(!dedup.is_empty());

        dedup.clear();

        assert!(dedup.is_empty());
        assert!(dedup.lookup(data).is_none());
        assert_eq!(dedup.stats().hits, 0);
    }

    #[test]
    fn test_batch_deduplicator() {
        let mut batch = BatchDeduplicator::new(10);

        for i in 0..15 {
            let data = format!("data{}", i);
            batch.insert(data.as_bytes(), ArenaSlot::new(0, i));
        }

        assert!(batch.should_merge());
        assert_eq!(batch.len(), 15);

        let taken = batch.take();
        assert_eq!(taken.len(), 15);
        assert!(batch.is_empty());
    }

    #[test]
    fn test_space_savings_calculation() {
        let stats = DeduplicatorStats {
            cache_size: 100,
            hits: 50,
            misses: 100,
            collisions: 2,
        };

        // 50 hits * 200 bytes avg = 10KB saved
        assert_eq!(stats.space_savings(200), 10000);
    }
}
