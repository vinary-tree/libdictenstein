//! TraversalContext - Block caching across lookups
//!
//! This module provides block caching during trie traversal to avoid repeated
//! pin/unpin operations. When traversing N nodes, instead of N pin/unpin ops,
//! we cache recently accessed pages and reuse them.
//!
//! ## Problem
//!
//! Each node access does pin/unpin:
//! ```text
//! lookup("hello"):
//!   pin block for 'h' -> read -> unpin
//!   pin block for 'e' -> read -> unpin
//!   pin block for 'l' -> read -> unpin
//!   ...
//! ```
//!
//! ## Solution
//!
//! TraversalContext holds pinned pages during lookup:
//! ```text
//! lookup("hello") with TraversalContext:
//!   ctx.get_page(block_h) -> pin & cache
//!   ctx.get_page(block_e) -> pin & cache (or reuse if same block)
//!   ctx.get_page(block_l) -> reuse from cache!
//!   ...
//!   ctx dropped -> unpin all
//! ```
//!
//! ## Expected Impact
//!
//! - **Latency**: 20-40% reduction for deep lookups
//! - **I/O**: Fewer pin/unpin syscalls
//! - **Memory**: Bounded by max_cached parameter

use crate::persistent_artrie_core::block_storage::BlockStorage;
use crate::persistent_artrie_core::buffer_manager::BufferManager;
use crate::persistent_artrie_core::disk_manager::{MmapDiskManager, BLOCK_SIZE};
use crate::persistent_artrie_core::error::PersistentARTrieError;
use std::collections::{HashMap, VecDeque};
use std::ptr::NonNull;

use parking_lot::RwLock;

use std::sync::Arc;

type Result<T> = std::result::Result<T, PersistentARTrieError>;

struct CachedPage {
    frame_id: crate::persistent_artrie_core::buffer_manager::FrameId,
    ptr: NonNull<[u8; BLOCK_SIZE]>,
}

/// TraversalContext - Caches pinned pages during trie traversal
///
/// This struct holds references to pages that have been accessed during
/// a traversal operation. Pages are kept pinned until the context is dropped,
/// which avoids repeated pin/unpin operations.
///
/// # Lifetime
///
/// The context borrows the BufferManager, so it cannot outlive it.
/// Typical usage is to create a context at the start of a lookup
/// and drop it when the lookup completes.
///
/// # Thread Safety
///
/// TraversalContext is NOT Send/Sync because it holds raw pointers to page data.
/// Each thread should create its own TraversalContext for traversal.
///
/// # Lease contention
///
/// Each cached page is held under a buffer-manager **read lease** for the
/// lifetime of the context (until [`clear`](Self::clear), drop, or FIFO
/// eviction when the cache is full). While a page is cached here, an attempt to
/// acquire an exclusive write lease on the same page (e.g.
/// [`BufferManager::fetch_page_mut`]) fails. Do not keep a `TraversalContext`
/// alive across a mutation of the same pages it has cached.
pub struct TraversalContext<S: BlockStorage = MmapDiskManager> {
    /// Buffer manager reference (Arc for shared ownership)
    buffer_manager: Arc<RwLock<BufferManager<S>>>,
    /// Cached page data: block_id -> frame lease and raw pointer to page data.
    /// The data is valid as long as the frame's read lease remains held.
    cached_pages: HashMap<u32, CachedPage>,
    /// FIFO order of cached pages, used to release the oldest lease when full.
    /// `VecDeque` so the oldest entry is released in O(1) (`pop_front`) rather
    /// than O(n) when the cache is full.
    pinned_blocks: VecDeque<u32>,
    /// Maximum number of pages to cache
    max_cached: usize,
    /// Statistics
    hits: usize,
    misses: usize,
}

impl<S: BlockStorage> TraversalContext<S> {
    /// Create a new traversal context
    ///
    /// # Arguments
    ///
    /// * `buffer_manager` - The buffer manager to use for page access
    /// * `max_cached` - Maximum number of pages to keep cached (default: 64)
    pub fn new(buffer_manager: Arc<RwLock<BufferManager<S>>>, max_cached: usize) -> Self {
        Self {
            buffer_manager,
            cached_pages: HashMap::with_capacity(max_cached.max(1)),
            pinned_blocks: VecDeque::with_capacity(max_cached.max(1)),
            max_cached: max_cached.max(1),
            hits: 0,
            misses: 0,
        }
    }

    /// Create a traversal context with default cache size
    pub fn new_default(buffer_manager: Arc<RwLock<BufferManager<S>>>) -> Self {
        Self::new(buffer_manager, 64)
    }

    /// Get a page by block ID, using cache if available
    ///
    /// Returns a reference to the page data. The reference is valid as long
    /// as the TraversalContext is alive.
    ///
    /// # Safety
    ///
    /// The returned slice is valid because:
    /// 1. We hold a reference to the BufferManager
    /// 2. The page is pinned for the lifetime of this context
    /// 3. We don't release the pin until drop()
    pub fn get_page(&mut self, block_id: u32) -> Result<&[u8; BLOCK_SIZE]> {
        // Check cache first
        if let Some(ptr) = self.cached_pages.get(&block_id) {
            self.hits += 1;
            // SAFETY: The pointer is valid because the page is still pinned
            return Ok(unsafe { ptr.ptr.as_ref() });
        }

        // Cache miss - need to fetch
        self.misses += 1;

        if self.cached_pages.len() >= self.max_cached {
            if let Some(oldest) = self.pinned_blocks.pop_front() {
                self.release_cached_page(oldest);
            }
        }

        let bm = self.buffer_manager.read();
        let (frame_id, non_null) = bm.pin_page_data(block_id)?;
        drop(bm);

        self.cached_pages.insert(
            block_id,
            CachedPage {
                frame_id,
                ptr: non_null,
            },
        );
        self.pinned_blocks.push_back(block_id);

        // SAFETY: The pointer is valid because:
        // 1. We just got it from a valid buffer frame
        // 2. The frame remains read-pinned until clear/drop or FIFO eviction
        Ok(unsafe { non_null.as_ref() })
    }

    /// Get a slice of a page starting at a given offset
    pub fn get_page_slice(&mut self, block_id: u32, offset: usize, len: usize) -> Result<&[u8]> {
        let page = self.get_page(block_id)?;
        if offset + len > BLOCK_SIZE {
            return Err(PersistentARTrieError::corrupted(&format!(
                "Page slice out of bounds: offset={}, len={}, block_size={}",
                offset, len, BLOCK_SIZE
            )));
        }
        Ok(&page[offset..offset + len])
    }

    /// Get cache statistics
    pub fn stats(&self) -> TraversalStats {
        TraversalStats {
            hits: self.hits,
            misses: self.misses,
            cached_pages: self.cached_pages.len(),
            max_cached: self.max_cached,
        }
    }

    /// Get the hit rate (0.0 to 1.0)
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    /// Clear the cache (unpin all pages)
    pub fn clear(&mut self) {
        let blocks = std::mem::take(&mut self.pinned_blocks);
        for block_id in blocks {
            self.release_cached_page(block_id);
        }
        self.cached_pages.clear();
        self.hits = 0;
        self.misses = 0;
    }

    fn release_cached_page(&mut self, block_id: u32) {
        if let Some(page) = self.cached_pages.remove(&block_id) {
            let bm = self.buffer_manager.read();
            bm.unpin_read_frame(page.frame_id);
        }
    }
}

// TraversalContext is NOT Send/Sync because it holds NonNull raw pointers
// which are inherently not thread-safe. The NonNull field already prevents
// automatic Send/Sync implementations.

impl<S: BlockStorage> Drop for TraversalContext<S> {
    fn drop(&mut self) {
        let blocks = std::mem::take(&mut self.pinned_blocks);
        for block_id in blocks {
            self.release_cached_page(block_id);
        }
    }
}

/// Statistics about traversal cache usage
#[derive(Debug, Clone)]
pub struct TraversalStats {
    /// Number of cache hits
    pub hits: usize,
    /// Number of cache misses
    pub misses: usize,
    /// Current number of cached pages
    pub cached_pages: usize,
    /// Maximum cache size
    pub max_cached: usize,
}

impl TraversalStats {
    /// Get the hit rate (0.0 to 1.0)
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// Lightweight traversal context for read-only lookups
///
/// This is a simpler version that doesn't cache across multiple get_page calls,
/// but provides a cleaner API for single-page accesses.
pub struct LightweightTraversalContext<S: BlockStorage = MmapDiskManager> {
    buffer_manager: Arc<RwLock<BufferManager<S>>>,
}

impl<S: BlockStorage> LightweightTraversalContext<S> {
    /// Create a new lightweight context
    pub fn new(buffer_manager: Arc<RwLock<BufferManager<S>>>) -> Self {
        Self { buffer_manager }
    }

    /// Read a page and copy it to a local buffer
    ///
    /// This is simpler than the caching version - it just copies the data
    /// and releases the pin immediately. Use this for occasional accesses
    /// where caching overhead isn't worth it.
    pub fn read_page_copy(&self, block_id: u32) -> Result<Box<[u8; BLOCK_SIZE]>> {
        let bm = self.buffer_manager.read();

        let page = bm.fetch_page(block_id)?;
        let mut copy = Box::new([0u8; BLOCK_SIZE]);
        copy.copy_from_slice(page.data());
        Ok(copy)
    }

    /// Get the buffer manager reference
    pub fn buffer_manager(&self) -> &Arc<RwLock<BufferManager<S>>> {
        &self.buffer_manager
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_artrie::disk_manager::DiskManager;
    use tempfile::TempDir;

    fn create_test_bm() -> (Arc<RwLock<BufferManager>>, TempDir) {
        create_test_bm_with_pool(16)
    }

    fn create_test_bm_with_pool(pool_size: usize) -> (Arc<RwLock<BufferManager>>, TempDir) {
        let temp_dir = TempDir::new().expect("create temp dir");
        let path = temp_dir.path().join("test.db");
        let dm = DiskManager::create(&path).expect("create disk manager");
        let bm = BufferManager::new(dm, pool_size);
        (Arc::new(RwLock::new(bm)), temp_dir)
    }

    #[test]
    fn test_traversal_context_creation() {
        let (bm, _temp) = create_test_bm();
        let ctx = TraversalContext::new(Arc::clone(&bm), 32);
        let stats = ctx.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.cached_pages, 0);
        assert_eq!(stats.max_cached, 32);
    }

    #[test]
    fn test_traversal_context_stats() {
        let stats = TraversalStats {
            hits: 75,
            misses: 25,
            cached_pages: 10,
            max_cached: 64,
        };
        assert!((stats.hit_rate() - 0.75).abs() < 0.001);
    }

    #[test]
    fn traversal_context_holds_page_pin_until_clear() {
        let (bm, _temp) = create_test_bm_with_pool(1);
        let block_id = {
            let bm_guard = bm.read();
            let mut page = bm_guard.new_page().expect("new page");
            page.data_mut()[..5].copy_from_slice(b"pinme");
            page.block_id()
        };

        let mut ctx = TraversalContext::new(Arc::clone(&bm), 1);
        {
            let page = ctx.get_page(block_id).expect("cached page");
            assert_eq!(&page[..5], b"pinme");
        }

        let blocked = {
            let bm_guard = bm.read();
            bm_guard.new_page().err()
        };
        assert!(
            matches!(
                blocked,
                Some(PersistentARTrieError::BufferPoolExhausted { .. })
            ),
            "cached traversal page must keep its frame pinned"
        );

        ctx.clear();

        {
            let bm_guard = bm.read();
            let _page = bm_guard.new_page().expect("pin released after clear");
        }
    }

    #[test]
    fn traversal_context_releases_fifo_pin_before_reusing_cache_slot() {
        let (bm, _temp) = create_test_bm_with_pool(1);
        let first_block = {
            let bm_guard = bm.read();
            let mut page = bm_guard.new_page().expect("first page");
            page.data_mut()[..5].copy_from_slice(b"first");
            page.block_id()
        };
        let second_block = {
            let bm_guard = bm.read();
            let mut page = bm_guard.new_page().expect("second page");
            page.data_mut()[..6].copy_from_slice(b"second");
            page.block_id()
        };

        let mut ctx = TraversalContext::new(Arc::clone(&bm), 1);
        {
            let page = ctx.get_page(first_block).expect("first cached page");
            assert_eq!(&page[..5], b"first");
        }
        {
            let page = ctx.get_page(second_block).expect("second cached page");
            assert_eq!(&page[..6], b"second");
        }

        let stats = ctx.stats();
        assert_eq!(stats.cached_pages, 1);
        assert_eq!(stats.misses, 2);
    }

    #[test]
    fn test_lightweight_context() {
        let (bm, _temp) = create_test_bm();

        // Allocate a page and write some data
        {
            let bm_guard = bm.write();

            let mut page = bm_guard.new_page().expect("new page");
            let data = page.data_mut();
            data[0..5].copy_from_slice(b"hello");
        }

        // Read using lightweight context
        let ctx = LightweightTraversalContext::new(Arc::clone(&bm));
        let page = ctx.read_page_copy(1).expect("read page");
        assert_eq!(&page[0..5], b"hello");
    }
}
