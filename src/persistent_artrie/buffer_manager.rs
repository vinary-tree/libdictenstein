//! Buffer Manager for Persistent Adaptive Radix Trie
//!
//! This module implements a page cache with LRU eviction using the Clock algorithm.
//! It provides:
//!
//! - **Page Cache**: Fixed-size pool of in-memory pages
//! - **Clock Eviction**: O(1) amortized eviction with reference bit tracking
//! - **Pin/Unpin**: RAII guards that prevent eviction during active use
//! - **Dirty Tracking**: Pages modified in memory are tracked for write-back
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    BufferManager                             │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Page Table: HashMap<BlockId, FrameId>                       │
//! │    Maps disk blocks to buffer pool frames                    │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Frame Metadata: Vec<FrameMetadata>                          │
//! │    [frame 0] [frame 1] [frame 2] ... [frame N-1]            │
//! │    - block_id: Option<u32>                                   │
//! │    - pin_count: AtomicU32                                    │
//! │    - dirty: AtomicBool                                       │
//! │    - reference_bit: AtomicBool                               │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Buffer Pool: Vec<[u8; BLOCK_SIZE]>                          │
//! │    Raw page data storage                                     │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Clock Hand: AtomicUsize                                     │
//! │    Points to next eviction candidate                        │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Clock Algorithm
//!
//! The Clock algorithm is a practical approximation of LRU:
//!
//! 1. Each frame has a "reference bit" set on access
//! 2. Clock hand sweeps through frames looking for eviction candidates
//! 3. If reference bit is set, clear it and move on (second chance)
//! 4. If reference bit is clear and unpinned, evict that frame
//!
//! This gives O(1) amortized eviction time with good cache behavior.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use parking_lot::RwLock;

use super::disk_manager::{DiskManager, BLOCK_SIZE};
use super::error::{PersistentARTrieError, Result};

/// Frame ID type (index into buffer pool)
pub type FrameId = usize;

/// Metadata for a single buffer frame
#[derive(Debug)]
pub struct FrameMetadata {
    /// Block ID stored in this frame (u32::MAX = None/free)
    block_id: AtomicU32,
    /// Number of active pins (frame cannot be evicted while > 0)
    pin_count: AtomicU32,
    /// Whether the page has been modified since last write-back
    dirty: AtomicBool,
    /// Reference bit for Clock algorithm (set on access, cleared by clock hand)
    reference_bit: AtomicBool,
}

impl FrameMetadata {
    /// Sentinel value indicating no block is assigned (frame is free)
    const NONE_BLOCK: u32 = u32::MAX;

    /// Create a new free frame
    fn new() -> Self {
        Self {
            block_id: AtomicU32::new(Self::NONE_BLOCK),
            pin_count: AtomicU32::new(0),
            dirty: AtomicBool::new(false),
            reference_bit: AtomicBool::new(false),
        }
    }

    /// Check if this frame is free (no block assigned)
    fn is_free(&self) -> bool {
        self.block_id.load(Ordering::Acquire) == Self::NONE_BLOCK
    }

    /// Check if this frame is pinned
    fn is_pinned(&self) -> bool {
        self.pin_count.load(Ordering::Acquire) > 0
    }

    /// Increment pin count
    fn pin(&self) {
        self.pin_count.fetch_add(1, Ordering::AcqRel);
        self.reference_bit.store(true, Ordering::Release);
    }

    /// Decrement pin count
    fn unpin(&self) {
        let old = self.pin_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(old > 0, "unpin called on unpinned frame");
    }

    /// Mark the frame as dirty
    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    /// Check if dirty
    fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    /// Clear dirty flag (after write-back)
    fn clear_dirty(&self) {
        self.dirty.store(false, Ordering::Release);
    }

    /// Get the block ID
    fn get_block_id(&self) -> Option<u32> {
        match self.block_id.load(Ordering::Acquire) {
            Self::NONE_BLOCK => None,
            id => Some(id),
        }
    }

    /// Set the block ID
    fn set_block_id(&self, block_id: Option<u32>) {
        let val = block_id.unwrap_or(Self::NONE_BLOCK);
        self.block_id.store(val, Ordering::Release);
    }
}

/// RAII guard for a pinned page (read access)
///
/// The page is automatically unpinned when the guard is dropped.
pub struct PageReadGuard<'a> {
    buffer_manager: &'a BufferManager,
    frame_id: FrameId,
}

impl<'a> PageReadGuard<'a> {
    /// Get a read-only view of the page data
    pub fn data(&self) -> &[u8; BLOCK_SIZE] {
        &self.buffer_manager.buffer_pool[self.frame_id]
    }

    /// Get the block ID of this page
    pub fn block_id(&self) -> u32 {
        self.buffer_manager.frames[self.frame_id]
            .get_block_id()
            .expect("pinned frame must have block_id")
    }
}

impl<'a> Drop for PageReadGuard<'a> {
    fn drop(&mut self) {
        self.buffer_manager.frames[self.frame_id].unpin();
    }
}

/// RAII guard for a pinned page (write access)
///
/// The page is automatically marked dirty and unpinned when the guard is dropped.
pub struct PageWriteGuard<'a> {
    buffer_manager: &'a BufferManager,
    frame_id: FrameId,
}

impl<'a> PageWriteGuard<'a> {
    /// Get a mutable view of the page data
    ///
    /// # Safety
    /// Caller must ensure exclusive access to this frame. The buffer manager
    /// enforces this through the pinning mechanism, but the actual mutation
    /// requires unsafe due to interior mutability.
    pub fn data_mut(&mut self) -> &mut [u8; BLOCK_SIZE] {
        // Safety: We have exclusive access via the write guard and pin
        unsafe {
            let ptr = self.buffer_manager.buffer_pool.as_ptr() as *mut [u8; BLOCK_SIZE];
            &mut *ptr.add(self.frame_id)
        }
    }

    /// Get read-only view of the page data
    pub fn data(&self) -> &[u8; BLOCK_SIZE] {
        &self.buffer_manager.buffer_pool[self.frame_id]
    }

    /// Get the block ID of this page
    pub fn block_id(&self) -> u32 {
        self.buffer_manager.frames[self.frame_id]
            .get_block_id()
            .expect("pinned frame must have block_id")
    }
}

impl<'a> Drop for PageWriteGuard<'a> {
    fn drop(&mut self) {
        self.buffer_manager.frames[self.frame_id].mark_dirty();
        self.buffer_manager.frames[self.frame_id].unpin();
    }
}

/// Buffer manager with Clock eviction algorithm
pub struct BufferManager {
    /// The underlying disk manager
    disk_manager: DiskManager,
    /// Page table: maps block_id -> frame_id
    page_table: RwLock<HashMap<u32, FrameId>>,
    /// Frame metadata
    frames: Vec<FrameMetadata>,
    /// Buffer pool (actual page data)
    buffer_pool: Vec<[u8; BLOCK_SIZE]>,
    /// Clock hand for eviction
    clock_hand: AtomicUsize,
    /// Maximum number of frames in the pool (allocated capacity)
    pool_size: usize,
    /// Currently active pool size (can be <= pool_size for adaptive sizing)
    active_pool_size: AtomicUsize,
}

impl BufferManager {
    /// Create a new buffer manager
    ///
    /// # Arguments
    /// * `disk_manager` - The disk manager for I/O operations
    /// * `pool_size` - Number of frames in the buffer pool
    pub fn new(disk_manager: DiskManager, pool_size: usize) -> Self {
        let frames: Vec<FrameMetadata> = (0..pool_size).map(|_| FrameMetadata::new()).collect();
        let buffer_pool: Vec<[u8; BLOCK_SIZE]> = (0..pool_size)
            .map(|_| [0u8; BLOCK_SIZE])
            .collect();

        Self {
            disk_manager,
            page_table: RwLock::new(HashMap::with_capacity(pool_size)),
            frames,
            buffer_pool,
            clock_hand: AtomicUsize::new(0),
            pool_size,
            active_pool_size: AtomicUsize::new(pool_size),
        }
    }

    /// Create a new buffer manager with adaptive sizing support.
    ///
    /// Pre-allocates `max_pool_size` frames but starts with only `initial_size` active.
    /// Use `grow_pool()` and `shrink_pool()` to adjust the active pool size.
    ///
    /// # Arguments
    /// * `disk_manager` - The disk manager for I/O operations
    /// * `initial_size` - Initial number of active frames
    /// * `max_pool_size` - Maximum number of frames (pre-allocated)
    pub fn new_with_max_capacity(
        disk_manager: DiskManager,
        initial_size: usize,
        max_pool_size: usize,
    ) -> Self {
        let frames: Vec<FrameMetadata> = (0..max_pool_size)
            .map(|_| FrameMetadata::new())
            .collect();
        let buffer_pool: Vec<[u8; BLOCK_SIZE]> = (0..max_pool_size)
            .map(|_| [0u8; BLOCK_SIZE])
            .collect();

        Self {
            disk_manager,
            page_table: RwLock::new(HashMap::with_capacity(max_pool_size)),
            frames,
            buffer_pool,
            clock_hand: AtomicUsize::new(0),
            pool_size: max_pool_size,
            active_pool_size: AtomicUsize::new(initial_size.min(max_pool_size)),
        }
    }

    /// Fetch a page for reading
    ///
    /// If the page is already in the buffer pool, returns a guard immediately.
    /// Otherwise, loads the page from disk (potentially evicting another page).
    pub fn fetch_page(&self, block_id: u32) -> Result<PageReadGuard<'_>> {
        // Check if already in buffer pool
        if let Some(frame_id) = self.lookup_frame(block_id) {
            self.frames[frame_id].pin();
            self.frames[frame_id].reference_bit.store(true, Ordering::Release);
            return Ok(PageReadGuard {
                buffer_manager: self,
                frame_id,
            });
        }

        // Need to load from disk
        let frame_id = self.load_page(block_id)?;
        Ok(PageReadGuard {
            buffer_manager: self,
            frame_id,
        })
    }

    /// Fetch a page for writing
    ///
    /// Similar to `fetch_page`, but the returned guard will mark the page
    /// dirty when dropped.
    pub fn fetch_page_mut(&self, block_id: u32) -> Result<PageWriteGuard<'_>> {
        // Check if already in buffer pool
        if let Some(frame_id) = self.lookup_frame(block_id) {
            self.frames[frame_id].pin();
            self.frames[frame_id].reference_bit.store(true, Ordering::Release);
            return Ok(PageWriteGuard {
                buffer_manager: self,
                frame_id,
            });
        }

        // Need to load from disk
        let frame_id = self.load_page(block_id)?;
        Ok(PageWriteGuard {
            buffer_manager: self,
            frame_id,
        })
    }

    /// Create a new page (allocate a new block)
    ///
    /// Returns a write guard for the newly allocated page.
    pub fn new_page(&self) -> Result<PageWriteGuard<'_>> {
        // Allocate a new block on disk
        let block_id = self.disk_manager.allocate_block()?;

        // Get a frame for it
        let frame_id = self.get_free_frame()?;

        // Initialize the frame
        self.frames[frame_id].set_block_id(Some(block_id));
        self.frames[frame_id].pin();
        self.frames[frame_id].mark_dirty();

        // Clear the buffer
        // Safety: We have exclusive access via the new allocation
        unsafe {
            let ptr = self.buffer_pool.as_ptr() as *mut [u8; BLOCK_SIZE];
            (*ptr.add(frame_id)).fill(0);
        }

        // Update page table
        self.page_table.write().insert(block_id, frame_id);

        Ok(PageWriteGuard {
            buffer_manager: self,
            frame_id,
        })
    }

    /// Delete a page
    ///
    /// The page must not be pinned by anyone else.
    pub fn delete_page(&self, block_id: u32) -> Result<()> {
        // Check if in buffer pool
        if let Some(frame_id) = self.lookup_frame(block_id) {
            let frame = &self.frames[frame_id];

            // Can't delete a pinned page
            if frame.is_pinned() {
                return Err(PersistentARTrieError::InternalError {
                    message: format!("Cannot delete pinned page (block {})", block_id),
                });
            }

            // Clear the frame
            frame.set_block_id(None);
            frame.clear_dirty();
            frame.reference_bit.store(false, Ordering::Release);

            // Remove from page table
            self.page_table.write().remove(&block_id);
        }

        // Free the block on disk
        self.disk_manager.free_block(block_id)
    }

    /// Flush a specific page to disk
    pub fn flush_page(&self, block_id: u32) -> Result<()> {
        if let Some(frame_id) = self.lookup_frame(block_id) {
            let frame = &self.frames[frame_id];

            if frame.is_dirty() {
                self.disk_manager
                    .write_block(block_id, &self.buffer_pool[frame_id])?;
                frame.clear_dirty();
            }
        }
        Ok(())
    }

    /// Flush all dirty pages to disk
    pub fn flush_all(&self) -> Result<()> {
        for (frame_id, frame) in self.frames.iter().enumerate() {
            if frame.is_dirty() {
                if let Some(block_id) = frame.get_block_id() {
                    self.disk_manager
                        .write_block(block_id, &self.buffer_pool[frame_id])?;
                    frame.clear_dirty();
                }
            }
        }
        self.disk_manager.sync()
    }

    /// Look up a frame by block ID
    fn lookup_frame(&self, block_id: u32) -> Option<FrameId> {
        self.page_table.read().get(&block_id).copied()
    }

    /// Load a page from disk into a frame
    fn load_page(&self, block_id: u32) -> Result<FrameId> {
        // Get a free frame (may evict)
        let frame_id = self.get_free_frame()?;

        // Read from disk
        // Safety: We have exclusive access to this frame via get_free_frame
        unsafe {
            let ptr = self.buffer_pool.as_ptr() as *mut [u8; BLOCK_SIZE];
            self.disk_manager.read_block(block_id, &mut *ptr.add(frame_id))?;
        }

        // Set up the frame
        self.frames[frame_id].set_block_id(Some(block_id));
        self.frames[frame_id].pin();
        self.frames[frame_id].clear_dirty();
        self.frames[frame_id].reference_bit.store(true, Ordering::Release);

        // Update page table
        self.page_table.write().insert(block_id, frame_id);

        Ok(frame_id)
    }

    /// Get a free frame using the Clock algorithm
    fn get_free_frame(&self) -> Result<FrameId> {
        let active_size = self.active_pool_size.load(Ordering::Acquire);

        // First pass: look for a free frame within active pool
        for frame_id in 0..active_size {
            if self.frames[frame_id].is_free() {
                return Ok(frame_id);
            }
        }

        // No free frames, need to evict using Clock algorithm
        let mut attempts = 0;
        let max_attempts = active_size * 2; // Two full sweeps

        while attempts < max_attempts {
            let frame_id = self.clock_hand.fetch_add(1, Ordering::Relaxed) % active_size;
            let frame = &self.frames[frame_id];

            // Skip pinned frames
            if frame.is_pinned() {
                attempts += 1;
                continue;
            }

            // Check reference bit
            if frame.reference_bit.swap(false, Ordering::AcqRel) {
                // Reference bit was set, give second chance
                attempts += 1;
                continue;
            }

            // Found a victim: evict it
            if let Some(old_block_id) = frame.get_block_id() {
                // Write back if dirty
                if frame.is_dirty() {
                    self.disk_manager
                        .write_block(old_block_id, &self.buffer_pool[frame_id])?;
                    frame.clear_dirty();
                }

                // Remove from page table
                self.page_table.write().remove(&old_block_id);

                // Clear the frame
                frame.set_block_id(None);
            }

            return Ok(frame_id);
        }

        // All frames are pinned
        Err(PersistentARTrieError::BufferPoolExhausted {
            pinned_pages: self.count_pinned(),
            total_pages: active_size,
        })
    }

    /// Count the number of pinned pages
    fn count_pinned(&self) -> usize {
        self.frames.iter().filter(|f| f.is_pinned()).count()
    }

    /// Get the current active pool size.
    ///
    /// This is the number of frames currently available for use.
    /// May be less than `max_pool_size()` if using adaptive sizing.
    pub fn pool_size(&self) -> usize {
        self.active_pool_size.load(Ordering::Relaxed)
    }

    /// Get the maximum pool size (allocated capacity).
    ///
    /// This is the total number of pre-allocated frames.
    /// The active pool size can grow up to this limit.
    pub fn max_pool_size(&self) -> usize {
        self.pool_size
    }

    /// Grow the buffer pool by activating more pre-allocated frames.
    ///
    /// # Arguments
    /// * `additional_frames` - Number of frames to activate
    ///
    /// # Returns
    /// Ok(new_size) on success, Err if would exceed max capacity.
    ///
    /// # Note
    /// This only works if the BufferManager was created with
    /// `new_with_max_capacity()`. Growing beyond the pre-allocated
    /// capacity is not supported.
    pub fn grow_pool(&self, additional_frames: usize) -> Result<usize> {
        loop {
            let current = self.active_pool_size.load(Ordering::Acquire);
            let new_size = current.saturating_add(additional_frames);

            // Cannot exceed pre-allocated capacity
            if new_size > self.pool_size {
                return Err(PersistentARTrieError::InternalError {
                    message: format!(
                        "Cannot grow pool beyond max capacity {} (current: {}, requested: +{})",
                        self.pool_size, current, additional_frames
                    ),
                });
            }

            // Try to update atomically
            match self.active_pool_size.compare_exchange(
                current,
                new_size,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(new_size),
                Err(_) => continue, // Retry
            }
        }
    }

    /// Shrink the buffer pool by deactivating frames.
    ///
    /// # Arguments
    /// * `frames_to_remove` - Number of frames to deactivate
    ///
    /// # Returns
    /// Ok(new_size) on success, Err if not enough frames or frames in use.
    ///
    /// # Note
    /// Frames that are pinned or contain data must be flushed first.
    /// This method will evict unpinned frames in the shrink range.
    pub fn shrink_pool(&self, frames_to_remove: usize) -> Result<usize> {
        loop {
            let current = self.active_pool_size.load(Ordering::Acquire);

            // Minimum pool size of 1
            let new_size = current.saturating_sub(frames_to_remove).max(1);

            if new_size == current {
                return Ok(current); // Nothing to shrink
            }

            // Check that frames in the shrink range are not pinned
            for frame_id in new_size..current {
                let frame = &self.frames[frame_id];
                if frame.is_pinned() {
                    return Err(PersistentARTrieError::InternalError {
                        message: format!(
                            "Cannot shrink pool: frame {} is pinned",
                            frame_id
                        ),
                    });
                }

                // Flush dirty frames before shrinking
                if frame.is_dirty() {
                    if let Some(block_id) = frame.get_block_id() {
                        self.disk_manager.write_block(block_id, &self.buffer_pool[frame_id])?;
                        frame.clear_dirty();
                    }
                }

                // Evict the frame
                if let Some(block_id) = frame.get_block_id() {
                    self.page_table.write().remove(&block_id);
                    frame.set_block_id(None);
                }
            }

            // Try to update atomically
            match self.active_pool_size.compare_exchange(
                current,
                new_size,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // Reset clock hand if it's beyond new size
                    let clock = self.clock_hand.load(Ordering::Relaxed);
                    if clock >= new_size {
                        self.clock_hand.store(0, Ordering::Relaxed);
                    }
                    return Ok(new_size);
                }
                Err(_) => continue, // Retry
            }
        }
    }

    /// Get statistics about the buffer pool
    pub fn stats(&self) -> BufferPoolStats {
        let active_size = self.active_pool_size.load(Ordering::Relaxed);
        let mut free = 0;
        let mut pinned = 0;
        let mut dirty = 0;

        // Only count frames within active pool
        for frame_id in 0..active_size {
            let frame = &self.frames[frame_id];
            if frame.is_free() {
                free += 1;
            } else {
                if frame.is_pinned() {
                    pinned += 1;
                }
                if frame.is_dirty() {
                    dirty += 1;
                }
            }
        }

        BufferPoolStats {
            total_frames: active_size,
            max_frames: self.pool_size,
            free_frames: free,
            pinned_frames: pinned,
            dirty_frames: dirty,
            used_frames: active_size - free,
        }
    }

    /// Get a reference to the underlying disk manager
    pub fn disk_manager(&self) -> &DiskManager {
        &self.disk_manager
    }
}

/// Statistics about the buffer pool
#[derive(Debug, Clone, Copy)]
pub struct BufferPoolStats {
    /// Total number of active frames in the pool
    pub total_frames: usize,
    /// Maximum number of frames (allocated capacity)
    pub max_frames: usize,
    /// Number of free (unallocated) frames
    pub free_frames: usize,
    /// Number of pinned frames (cannot be evicted)
    pub pinned_frames: usize,
    /// Number of dirty frames (need write-back)
    pub dirty_frames: usize,
    /// Number of frames with data (total - free)
    pub used_frames: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_buffer_manager(pool_size: usize) -> BufferManager {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.part");
        let disk_manager = DiskManager::create(&path).expect("Failed to create disk manager");

        // Keep the temp dir alive by leaking it (for tests only)
        std::mem::forget(dir);

        BufferManager::new(disk_manager, pool_size)
    }

    #[test]
    fn test_new_page() {
        let bm = create_buffer_manager(10);

        let mut guard = bm.new_page().expect("new_page");
        let block_id = guard.block_id();

        // Write some data
        guard.data_mut()[0] = 0xDE;
        guard.data_mut()[1] = 0xAD;
        drop(guard);

        // Read it back
        let guard = bm.fetch_page(block_id).expect("fetch_page");
        assert_eq!(guard.data()[0], 0xDE);
        assert_eq!(guard.data()[1], 0xAD);
    }

    #[test]
    fn test_fetch_page() {
        let bm = create_buffer_manager(10);

        // Create a page and write data
        let mut guard = bm.new_page().expect("new_page");
        let block_id = guard.block_id();
        guard.data_mut()[100] = 42;
        drop(guard);

        // Flush to disk
        bm.flush_page(block_id).expect("flush");

        // Fetch again and verify
        let guard = bm.fetch_page(block_id).expect("fetch_page");
        assert_eq!(guard.data()[100], 42);
    }

    #[test]
    fn test_multiple_pages() {
        let bm = create_buffer_manager(10);
        let mut block_ids = Vec::new();

        // Create several pages
        for i in 0..5 {
            let mut guard = bm.new_page().expect("new_page");
            guard.data_mut()[0] = i as u8;
            block_ids.push(guard.block_id());
        }

        // Verify all pages
        for (i, &block_id) in block_ids.iter().enumerate() {
            let guard = bm.fetch_page(block_id).expect("fetch_page");
            assert_eq!(guard.data()[0], i as u8);
        }
    }

    #[test]
    fn test_eviction() {
        // Create a small buffer pool
        let bm = create_buffer_manager(3);
        let mut block_ids = Vec::new();

        // Create more pages than the pool can hold
        for i in 0..10 {
            let mut guard = bm.new_page().expect("new_page");
            guard.data_mut()[0] = i as u8;
            block_ids.push(guard.block_id());
        }

        // All pages should still be accessible (via eviction and reload)
        for (i, &block_id) in block_ids.iter().enumerate() {
            let guard = bm.fetch_page(block_id).expect("fetch_page");
            assert_eq!(guard.data()[0], i as u8, "Page {} corrupted", i);
        }
    }

    #[test]
    fn test_stats() {
        let bm = create_buffer_manager(10);

        let initial_stats = bm.stats();
        assert_eq!(initial_stats.total_frames, 10);
        assert_eq!(initial_stats.free_frames, 10);
        assert_eq!(initial_stats.used_frames, 0);

        // Create some pages
        let guard1 = bm.new_page().expect("new_page");
        let _guard2 = bm.new_page().expect("new_page");

        let stats = bm.stats();
        assert_eq!(stats.used_frames, 2);
        assert_eq!(stats.free_frames, 8);
        assert!(stats.pinned_frames >= 2); // Both guards are still held
        assert!(stats.dirty_frames >= 2);

        drop(guard1);

        let stats = bm.stats();
        assert!(stats.pinned_frames >= 1);
    }

    #[test]
    fn test_flush_all() {
        let bm = create_buffer_manager(10);

        // Create and modify some pages
        for i in 0..5 {
            let mut guard = bm.new_page().expect("new_page");
            guard.data_mut()[0] = i as u8;
        }

        // Flush all
        bm.flush_all().expect("flush_all");

        // Check that dirty count is 0
        let stats = bm.stats();
        assert_eq!(stats.dirty_frames, 0);
    }

    #[test]
    fn test_delete_page() {
        let bm = create_buffer_manager(10);

        // Create a page
        let guard = bm.new_page().expect("new_page");
        let block_id = guard.block_id();
        drop(guard);

        // Delete it
        bm.delete_page(block_id).expect("delete_page");

        // Stats should show one less used frame
        let stats = bm.stats();
        assert_eq!(stats.used_frames, 0);
    }

    #[test]
    fn test_pinned_page_not_evicted() {
        // Create a very small buffer pool
        let bm = create_buffer_manager(2);

        // Pin one page
        let pinned_guard = bm.new_page().expect("new_page");
        let pinned_block = pinned_guard.block_id();

        // Fill the pool with another page
        let mut other_guard = bm.new_page().expect("new_page");
        other_guard.data_mut()[0] = 99;
        drop(other_guard);

        // Try to create more pages (should evict the unpinned one)
        let _new_guard = bm.new_page().expect("new_page - should evict unpinned");

        // The pinned page should still be valid
        assert_eq!(pinned_guard.block_id(), pinned_block);
    }
}
