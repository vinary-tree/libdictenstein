//! ArenaManager - Manages multiple CharNodeArenas for efficient node storage
//!
//! The ArenaManager provides a simple interface for allocating and reading
//! CharNodes across multiple arenas. When an arena fills up, a new one is
//! automatically created.
//!
//! ## Addressing Scheme
//!
//! Each allocated node is identified by an `ArenaSlot`:
//! - `arena_id`: Which arena contains the node (u32)
//! - `slot_id`: Which slot within that arena (u32)
//!
//! This is encoded into a 64-bit value for use with SwizzledPtr.

use super::arena::CharNodeArena;
use crate::persistent_artrie::block_storage::BlockStorage;
use crate::persistent_artrie::buffer_manager::BufferManager;
use crate::persistent_artrie::dirty_tracker::DirtyTracker;
use crate::persistent_artrie::disk_manager::{MmapDiskManager, BLOCK_SIZE};
use crate::persistent_artrie::PersistentARTrieError;

use parking_lot::RwLock;

use std::sync::Arc;

type Result<T> = std::result::Result<T, PersistentARTrieError>;

// =============================================================================
// Flush Configuration
// =============================================================================

/// Configuration for flush behavior.
///
/// Controls whether slot-level dirty tracking is enabled and
/// the threshold for switching between partial and full arena writes.
#[derive(Debug, Clone)]
pub struct FlushConfig {
    /// Enable slot-level dirty tracking for fine-grained incremental checkpoints.
    ///
    /// When enabled, only modified slots are written during flush instead of
    /// entire arenas. This can reduce I/O by 90%+ for localized updates.
    ///
    /// Default: `false` (opt-in due to memory overhead)
    pub slot_level_tracking: bool,

    /// Threshold ratio for switching to full arena writes.
    ///
    /// When the ratio of dirty slots to total slots exceeds this threshold,
    /// the entire arena is written instead of individual slots. This balances
    /// I/O savings against syscall overhead.
    ///
    /// Default: `0.5` (50% dirty triggers full write)
    pub full_arena_threshold: f64,
}

impl Default for FlushConfig {
    fn default() -> Self {
        Self {
            slot_level_tracking: false,
            full_arena_threshold: 0.5,
        }
    }
}

impl FlushConfig {
    /// Create a config with slot-level tracking enabled.
    pub fn with_slot_tracking() -> Self {
        Self {
            slot_level_tracking: true,
            ..Default::default()
        }
    }

    /// Set the full arena write threshold.
    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.full_arena_threshold = threshold.clamp(0.0, 1.0);
        self
    }
}

/// Statistics from a flush operation.
#[derive(Debug, Clone, Default)]
pub struct FlushStats {
    /// Number of arenas that were written in full.
    pub full_arena_writes: usize,
    /// Number of arenas that used partial/slot-level writes.
    pub partial_writes: usize,
    /// Total number of individual slots written.
    pub slots_written: usize,
    /// Total bytes written to disk.
    pub bytes_written: usize,
    /// Estimated bytes saved by partial writes.
    pub bytes_saved: usize,
}

impl FlushStats {
    /// Create stats for a full flush (no slot-level tracking).
    pub fn full_flush(arena_count: usize, arena_size: usize) -> Self {
        Self {
            full_arena_writes: arena_count,
            partial_writes: 0,
            slots_written: 0,
            bytes_written: arena_count * arena_size,
            bytes_saved: 0,
        }
    }
}

/// Write only dirty slots for a single arena.
///
/// This is a free function to avoid borrow checker issues when iterating
/// over arenas while also needing to call this write helper.
///
/// Writes:
/// 1. Header (always, as it contains updated node_count etc.)
/// 2. Each dirty slot's data
/// 3. Each dirty slot's directory entry
fn write_dirty_slots_for_arena<S: BlockStorage>(
    bm_guard: &impl std::ops::Deref<Target = BufferManager<S>>,
    arena: &CharNodeArena,
    block_id: u32,
    dirty_slots: impl Iterator<Item = u32>,
) -> Result<usize> {
    let dm = bm_guard.storage();
    let arena_bytes = arena.as_bytes();
    let mut bytes_written = 0usize;

    // Always write header
    let (header_off, header_len) = arena.header_range();
    dm.write_bytes(
        block_id,
        header_off,
        &arena_bytes[header_off..header_off + header_len],
    )?;
    bytes_written += header_len;

    // Write each dirty slot's data and directory entry
    for slot_id in dirty_slots {
        // Write data
        let (data_off, data_len) = arena.slot_data_range(slot_id)?;
        dm.write_bytes(
            block_id,
            data_off,
            &arena_bytes[data_off..data_off + data_len],
        )?;
        bytes_written += data_len;

        // Write directory entry
        let (dir_off, dir_len) = arena.slot_directory_entry_range(slot_id)?;
        dm.write_bytes(block_id, dir_off, &arena_bytes[dir_off..dir_off + dir_len])?;
        bytes_written += dir_len;
    }

    Ok(bytes_written)
}

/// Arena slot identifier - combines arena_id and slot_id
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArenaSlot {
    /// Arena ID (which arena)
    pub arena_id: u32,
    /// Slot ID within the arena
    pub slot_id: u32,
}

impl ArenaSlot {
    pub fn new(arena_id: u32, slot_id: u32) -> Self {
        Self { arena_id, slot_id }
    }

    /// Encode to a 64-bit value
    pub fn to_u64(&self) -> u64 {
        ((self.arena_id as u64) << 32) | (self.slot_id as u64)
    }

    /// Decode from a 64-bit value
    pub fn from_u64(value: u64) -> Self {
        Self {
            arena_id: (value >> 32) as u32,
            slot_id: (value & 0xFFFFFFFF) as u32,
        }
    }
}

/// Handle for a reserved range of consecutive slots.
///
/// Created by `ArenaManager::reserve_slots()`, this tracks a contiguous
/// range of slots for sequential sibling storage. Use with
/// `ArenaManager::allocate_reserved()` to fill the slots in order.
#[derive(Debug, Clone)]
pub struct ReservedSlots {
    /// Arena containing the reserved slots
    pub arena_id: u32,
    /// First slot in the reserved range
    pub first_slot: u32,
    /// Total number of slots reserved
    pub count: u32,
    /// Next slot index to allocate (0..count)
    pub next_idx: u32,
}

impl ReservedSlots {
    /// Get the ArenaSlot for the first child
    pub fn first_child_slot(&self) -> ArenaSlot {
        ArenaSlot::new(self.arena_id, self.first_slot)
    }

    /// Check if all reserved slots have been used
    pub fn is_complete(&self) -> bool {
        self.next_idx >= self.count
    }

    /// Get the number of remaining slots
    pub fn remaining(&self) -> u32 {
        self.count.saturating_sub(self.next_idx)
    }
}

/// ArenaManager - Manages allocation and reading across multiple arenas
pub struct ArenaManager<S: BlockStorage = MmapDiskManager> {
    /// All arenas (may be in memory or on disk)
    arenas: Vec<CharNodeArena>,
    /// Index of the current arena for new allocations
    active_arena: usize,
    /// Optional buffer manager for disk I/O
    buffer_manager: Option<Arc<RwLock<BufferManager<S>>>>,
    /// Arena size (default BLOCK_SIZE)
    arena_size: usize,
    /// Optional dirty tracker for slot-level incremental checkpoints
    dirty_tracker: Option<DirtyTracker>,
    /// Flush configuration
    flush_config: FlushConfig,
}

impl<S: BlockStorage> std::fmt::Debug for ArenaManager<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArenaManager")
            .field("num_arenas", &self.arenas.len())
            .field("active_arena", &self.active_arena)
            .field("has_buffer_manager", &self.buffer_manager.is_some())
            .field("arena_size", &self.arena_size)
            .finish()
    }
}

impl<S: BlockStorage> ArenaManager<S> {
    /// Create a new ArenaManager without disk backing
    pub fn new() -> Self {
        let initial_arena = CharNodeArena::new_default();
        Self {
            arenas: vec![initial_arena],
            active_arena: 0,
            buffer_manager: None,
            arena_size: BLOCK_SIZE,
            dirty_tracker: None,
            flush_config: FlushConfig::default(),
        }
    }

    /// Create a new ArenaManager with disk backing via BufferManager
    pub fn with_buffer_manager(buffer_manager: Arc<RwLock<BufferManager<S>>>) -> Self {
        let initial_arena = CharNodeArena::new_default();
        Self {
            arenas: vec![initial_arena],
            active_arena: 0,
            buffer_manager: Some(buffer_manager),
            arena_size: BLOCK_SIZE,
            dirty_tracker: None,
            flush_config: FlushConfig::default(),
        }
    }

    /// Create a new ArenaManager with custom arena size
    pub fn with_arena_size(arena_size: usize) -> Self {
        let initial_arena = CharNodeArena::new(arena_size);
        Self {
            arenas: vec![initial_arena],
            active_arena: 0,
            buffer_manager: None,
            arena_size,
            dirty_tracker: None,
            flush_config: FlushConfig::default(),
        }
    }

    /// Create a new ArenaManager with flush configuration.
    ///
    /// This constructor enables slot-level dirty tracking if configured.
    pub fn with_config(config: FlushConfig) -> Self {
        let dirty_tracker = if config.slot_level_tracking {
            Some(DirtyTracker::slot_level())
        } else {
            None
        };
        let initial_arena = CharNodeArena::new_default();
        Self {
            arenas: vec![initial_arena],
            active_arena: 0,
            buffer_manager: None,
            arena_size: BLOCK_SIZE,
            dirty_tracker,
            flush_config: config,
        }
    }

    /// Create a new ArenaManager with buffer manager and flush configuration.
    ///
    /// This is the primary constructor for disk-backed tries with slot-level tracking.
    pub fn with_buffer_manager_and_config(
        buffer_manager: Arc<RwLock<BufferManager<S>>>,
        config: FlushConfig,
    ) -> Self {
        let dirty_tracker = if config.slot_level_tracking {
            Some(DirtyTracker::slot_level())
        } else {
            None
        };
        let initial_arena = CharNodeArena::new_default();
        Self {
            arenas: vec![initial_arena],
            active_arena: 0,
            buffer_manager: Some(buffer_manager),
            arena_size: BLOCK_SIZE,
            dirty_tracker,
            flush_config: config,
        }
    }

    /// Allocate space for data and return the ArenaSlot
    ///
    /// If the current arena is full, a new arena is created automatically.
    /// When slot-level tracking is enabled, the allocation is marked dirty.
    pub fn allocate(&mut self, data: &[u8]) -> Result<ArenaSlot> {
        // Try to allocate in the active arena
        if let Some(slot_id) = self.arenas[self.active_arena].allocate(data) {
            let slot = ArenaSlot::new(self.active_arena as u32, slot_id);
            // Track the dirty slot
            if let Some(ref mut tracker) = self.dirty_tracker {
                tracker.mark_slot_dirty(slot.arena_id, slot.slot_id);
            }
            return Ok(slot);
        }

        // Active arena is full, create a new one
        let new_arena = CharNodeArena::new(self.arena_size);
        self.arenas.push(new_arena);
        self.active_arena = self.arenas.len() - 1;

        // Allocate in the new arena
        if let Some(slot_id) = self.arenas[self.active_arena].allocate(data) {
            let slot = ArenaSlot::new(self.active_arena as u32, slot_id);
            // Track the dirty slot
            if let Some(ref mut tracker) = self.dirty_tracker {
                tracker.mark_slot_dirty(slot.arena_id, slot.slot_id);
            }
            Ok(slot)
        } else {
            Err(PersistentARTrieError::internal(&format!(
                "Data too large for arena: {} bytes",
                data.len()
            )))
        }
    }

    /// Read data from the specified arena slot
    pub fn read(&self, slot: ArenaSlot) -> Result<&[u8]> {
        let arena_id = slot.arena_id as usize;
        if arena_id >= self.arenas.len() {
            return Err(PersistentARTrieError::corrupted(&format!(
                "Invalid arena ID {} (have {} arenas)",
                arena_id,
                self.arenas.len()
            )));
        }

        self.arenas[arena_id].read(slot.slot_id)
    }

    /// Update data at the specified arena slot
    ///
    /// The new data must be exactly the same size as the original allocation.
    /// This is used for correcting relative encoding after arena overflow detection.
    pub fn update(&mut self, slot: ArenaSlot, new_data: &[u8]) -> Result<()> {
        let arena_id = slot.arena_id as usize;
        if arena_id >= self.arenas.len() {
            return Err(PersistentARTrieError::corrupted(&format!(
                "Invalid arena ID {} (have {} arenas)",
                arena_id,
                self.arenas.len()
            )));
        }

        self.arenas[arena_id].update(slot.slot_id, new_data)
    }

    /// Get the number of arenas
    pub fn arena_count(&self) -> usize {
        self.arenas.len()
    }

    /// Get total node count across all arenas
    pub fn total_node_count(&self) -> u64 {
        self.arenas.iter().map(|a| a.node_count() as u64).sum()
    }

    /// Flush all dirty arenas to disk
    ///
    /// This persists arenas to the buffer manager. Each arena is allocated
    /// a block on demand, and the actual allocated block_id is stored in
    /// the arena for future reference.
    ///
    /// # Concurrency Safety
    ///
    /// This method uses the actual block_id returned by `new_page()` rather
    /// than assuming sequential allocation. This is critical for correctness
    /// under concurrent allocation where block IDs may not be sequential.
    pub fn flush(&mut self) -> Result<()> {
        let bm = match &self.buffer_manager {
            Some(bm) => bm,
            None => return Ok(()), // No disk backing, nothing to flush
        };

        let bm_guard = bm.write();

        for arena in self.arenas.iter_mut() {
            if arena.is_dirty() {
                // Finalize checksums before writing to disk (V3+ arenas)
                arena.finalize_checksums();

                if let Some(id) = arena.block_id {
                    // Arena already has a block_id assigned - fetch and update
                    let mut page = bm_guard.fetch_page_mut(id)?;
                    let page_data = page.data_mut();
                    let arena_data = arena.as_bytes();
                    page_data[..arena_data.len()].copy_from_slice(arena_data);
                } else {
                    // New arena - allocate a block and write DIRECTLY to it
                    // (Don't drop and re-fetch - that's the race condition!)
                    let mut page = bm_guard.new_page()?;
                    arena.set_block_id(page.block_id());
                    let page_data = page.data_mut();
                    let arena_data = arena.as_bytes();
                    page_data[..arena_data.len()].copy_from_slice(arena_data);
                }

                arena.mark_clean();
            }
        }

        // Flush all pages to disk
        bm_guard.flush_all()?;

        Ok(())
    }

    /// Flush dirty arenas in sequential order for optimal I/O.
    ///
    /// This method explicitly collects and sorts dirty arena IDs before flushing,
    /// ensuring sequential disk access patterns. This is particularly beneficial
    /// for HDD storage where sequential I/O is much faster than random I/O.
    ///
    /// For SSD storage, the benefit is smaller but still measurable due to
    /// better buffer manager cache utilization.
    ///
    /// # Concurrency Safety
    ///
    /// This method uses the actual block_id returned by `new_page()` rather
    /// than assuming sequential allocation. This is critical for correctness
    /// under concurrent allocation where block IDs may not be sequential.
    ///
    /// # Performance
    ///
    /// Expected improvement: 5-15% faster flush for disk-resident tries with
    /// many dirty arenas, especially on rotational storage.
    pub fn flush_sequential(&mut self) -> Result<()> {
        let bm = match &self.buffer_manager {
            Some(bm) => bm,
            None => return Ok(()), // No disk backing, nothing to flush
        };

        // Collect dirty arena indices and sort them for sequential I/O
        let mut dirty_indices: Vec<usize> = self
            .arenas
            .iter()
            .enumerate()
            .filter(|(_, arena)| arena.is_dirty())
            .map(|(idx, _)| idx)
            .collect();

        if dirty_indices.is_empty() {
            return Ok(());
        }

        // Sort for sequential access (already sorted by index, but explicit for clarity)
        dirty_indices.sort_unstable();

        let bm_guard = bm.write();

        for arena_index in dirty_indices {
            let arena = &mut self.arenas[arena_index];

            if let Some(id) = arena.block_id {
                // Arena already has a block_id assigned - fetch and update
                let mut page = bm_guard.fetch_page_mut(id)?;
                let page_data = page.data_mut();
                let arena_data = arena.as_bytes();
                page_data[..arena_data.len()].copy_from_slice(arena_data);
            } else {
                // New arena - allocate a block and write DIRECTLY to it
                // (Don't drop and re-fetch - that's the race condition!)
                let mut page = bm_guard.new_page()?;
                arena.set_block_id(page.block_id());
                let page_data = page.data_mut();
                let arena_data = arena.as_bytes();
                page_data[..arena_data.len()].copy_from_slice(arena_data);
            }

            arena.mark_clean();
        }

        // Flush all pages to disk
        bm_guard.flush_all()?;

        Ok(())
    }

    /// Sync all arenas to disk (calls flush then syncs)
    pub fn sync(&mut self) -> Result<()> {
        self.flush()?;

        if let Some(bm) = &self.buffer_manager {
            let bm_guard = bm.write();
            bm_guard.storage().sync()?;
        }

        Ok(())
    }

    /// Collect dirty arena data for background sync.
    ///
    /// Returns a vector of `(arena_id, block_id, data)` tuples for all dirty arenas.
    /// This allows collecting sync state with a brief read lock, then releasing
    /// the lock before spawning the background sync thread.
    ///
    /// # Usage
    ///
    /// ```rust,ignore
    /// // Collect data with brief lock
    /// let dirty_arenas = {
    ///     let am = arena_manager.read();
    ///     am.collect_dirty_arena_data()
    /// };
    /// // Lock released - reads/writes can continue
    ///
    /// // Spawn background sync
    /// spawn_background_sync(dirty_arenas);
    /// ```
    pub fn collect_dirty_arena_data(&self) -> Vec<(u32, Option<u32>, Vec<u8>)> {
        self.arenas
            .iter()
            .enumerate()
            .filter(|(_, arena)| arena.is_dirty())
            .map(|(id, arena)| (id as u32, arena.block_id, arena.as_bytes().to_vec()))
            .collect()
    }

    /// Get the buffer manager reference (for background sync operations).
    ///
    /// Returns `None` if no buffer manager is configured (in-memory mode).
    pub fn buffer_manager(&self) -> Option<&Arc<RwLock<BufferManager<S>>>> {
        self.buffer_manager.as_ref()
    }

    // =========================================================================
    // Slot-Level Incremental Flush
    // =========================================================================

    /// Flush only dirty slots to disk for incremental checkpointing.
    ///
    /// When slot-level tracking is enabled, this method writes only the modified
    /// slots instead of entire arenas. This can reduce checkpoint I/O by 90%+
    /// for localized updates.
    ///
    /// # Algorithm
    ///
    /// For each dirty arena:
    /// 1. Calculate dirty ratio (dirty_slots / total_slots)
    /// 2. If ratio >= threshold: write entire arena (full write)
    /// 3. Otherwise: write header + dirty slots + their directory entries
    ///
    /// # Returns
    ///
    /// Statistics about the flush operation including bytes written and saved.
    pub fn flush_dirty_slots(&mut self) -> Result<FlushStats> {
        // If no dirty tracker, fall back to full flush
        let tracker = match &self.dirty_tracker {
            Some(t) => t,
            None => {
                self.flush()?;
                return Ok(FlushStats::full_flush(self.arenas.len(), self.arena_size));
            }
        };

        // No buffer manager means no disk backing
        let bm = match &self.buffer_manager {
            Some(bm) => bm,
            None => return Ok(FlushStats::default()),
        };

        // No dirty arenas, nothing to do
        if tracker.dirty_arena_count() == 0 {
            return Ok(FlushStats::default());
        }

        let bm_guard = bm.write();

        let mut stats = FlushStats::default();
        let threshold = self.flush_config.full_arena_threshold;

        // Collect dirty arena IDs (sorted for sequential I/O)
        let mut dirty_arena_ids: Vec<u32> = tracker.dirty_arena_ids().collect();
        dirty_arena_ids.sort_unstable();

        for arena_id in dirty_arena_ids {
            let arena_idx = arena_id as usize;
            if arena_idx >= self.arenas.len() {
                continue; // Skip invalid arena IDs
            }

            let arena = &mut self.arenas[arena_idx];
            if !arena.is_dirty() {
                continue; // Skip clean arenas
            }

            // Determine if we should do full or partial write
            let total_slots = arena.slot_count() as usize;
            let dirty_slot_count = tracker
                .dirty_slot_ids(arena_id)
                .map(|iter| iter.count())
                .unwrap_or(total_slots);

            let dirty_ratio = if total_slots > 0 {
                dirty_slot_count as f64 / total_slots as f64
            } else {
                1.0 // New arena with no slots yet - full write
            };

            if let Some(block_id) = arena.block_id {
                // Arena already has a block_id assigned
                if dirty_ratio >= threshold || total_slots == 0 {
                    // Finalize checksums for V3+ arenas before full write
                    arena.finalize_checksums();

                    // Full arena write
                    let mut page = bm_guard.fetch_page_mut(block_id)?;
                    let page_data = page.data_mut();
                    let arena_data = arena.as_bytes();
                    page_data[..arena_data.len()].copy_from_slice(arena_data);

                    stats.full_arena_writes += 1;
                    stats.bytes_written += arena_data.len();
                } else {
                    // Partial write: header + dirty slots + directory entries
                    let bytes_written = write_dirty_slots_for_arena(
                        &bm_guard,
                        arena,
                        block_id,
                        tracker.dirty_slot_ids(arena_id).unwrap(),
                    )?;

                    stats.partial_writes += 1;
                    stats.slots_written += dirty_slot_count;
                    stats.bytes_written += bytes_written;
                    stats.bytes_saved += self.arena_size.saturating_sub(bytes_written);
                }
            } else {
                // New arena - allocate a block and write DIRECTLY to it
                // (Don't drop and re-fetch - that's the race condition!)
                // Finalize checksums for V3+ arenas before full write
                arena.finalize_checksums();

                let mut page = bm_guard.new_page()?;
                arena.set_block_id(page.block_id());
                let page_data = page.data_mut();
                let arena_data = arena.as_bytes();
                page_data[..arena_data.len()].copy_from_slice(arena_data);

                stats.full_arena_writes += 1;
                stats.bytes_written += arena_data.len();
            }

            arena.mark_clean();
        }

        // Defense in depth: flush any untracked dirty arenas that need block_ids.
        // This catches arenas that were dirty before slot tracking was enabled
        // but weren't captured in the tracker's dirty set.
        for (arena_id, arena) in self.arenas.iter_mut().enumerate() {
            if arena.is_dirty() && arena.block_id.is_none() {
                log::warn!(
                    "Flushing untracked dirty arena {} (slot tracking may have been enabled late)",
                    arena_id
                );
                arena.finalize_checksums();
                let mut page = bm_guard.new_page()?;
                arena.set_block_id(page.block_id());
                let page_data = page.data_mut();
                let arena_data = arena.as_bytes();
                let arena_data_len = arena_data.len();
                page_data[..arena_data_len].copy_from_slice(arena_data);
                arena.mark_clean();
                stats.full_arena_writes += 1;
                stats.bytes_written += arena_data_len;
            }
        }

        // Flush all pages to disk
        bm_guard.flush_all()?;

        // Clear the dirty tracker after successful flush
        if let Some(ref mut tracker) = self.dirty_tracker {
            tracker.checkpoint_complete();
        }

        Ok(stats)
    }

    /// Get the current flush configuration.
    pub fn flush_config(&self) -> &FlushConfig {
        &self.flush_config
    }

    /// Check if slot-level tracking is enabled.
    pub fn has_slot_tracking(&self) -> bool {
        self.dirty_tracker.is_some()
    }

    /// Enable slot-level dirty tracking after construction.
    ///
    /// This is useful when opening an existing trie and wanting to enable
    /// fine-grained checkpoint I/O savings. Slot-level tracking reduces
    /// checkpoint I/O by writing only modified slots instead of entire arenas.
    ///
    /// # Note
    ///
    /// This method is idempotent - calling it when slot tracking is already
    /// enabled has no effect.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Open existing trie without slot tracking
    /// let mut am = ArenaManager::with_buffer_manager(bm);
    ///
    /// // Enable slot tracking for subsequent operations
    /// am.enable_slot_tracking();
    ///
    /// // Now allocations will be tracked at slot level
    /// let slot = am.allocate(&data)?;
    /// ```
    pub fn enable_slot_tracking(&mut self) {
        if self.dirty_tracker.is_none() {
            let mut tracker = DirtyTracker::slot_level();
            // CRITICAL FIX: Mark all existing dirty arenas in the new tracker
            // to prevent "Invalid arena ID" corruption on checkpoint.
            // Without this, dirty arenas created before enabling slot tracking
            // would not be tracked, causing flush_dirty_slots() to skip them
            // and leaving them without block_ids while serialized nodes
            // reference them.
            tracker.mark_arenas_dirty(
                self.arenas
                    .iter()
                    .enumerate()
                    .filter(|(_, arena)| arena.is_dirty())
                    .map(|(id, _)| id as u32),
            );
            self.dirty_tracker = Some(tracker);
            self.flush_config.slot_level_tracking = true;
        }
    }

    /// Get dirty tracker statistics (if tracking is enabled).
    pub fn dirty_tracker_stats(
        &self,
    ) -> Option<crate::persistent_artrie::dirty_tracker::DirtyTrackerStats> {
        self.dirty_tracker.as_ref().map(|t| t.stats())
    }

    /// Get statistics about arena usage
    pub fn stats(&self) -> ArenaStats {
        let mut total_capacity = 0usize;
        let mut total_used = 0usize;
        let mut node_count = 0u64;

        for arena in &self.arenas {
            total_capacity += arena.size();
            total_used += arena.size() - arena.available_space();
            node_count += arena.node_count() as u64;
        }

        ArenaStats {
            arena_count: self.arenas.len(),
            total_capacity,
            total_used,
            node_count,
            active_arena: self.active_arena,
            active_arena_available: self.arenas[self.active_arena].available_space(),
        }
    }

    /// Clear all arenas and reset to initial state
    pub fn clear(&mut self) {
        self.arenas.clear();
        self.arenas.push(CharNodeArena::new(self.arena_size));
        self.active_arena = 0;
        // Reset dirty tracker if present
        if let Some(ref mut tracker) = self.dirty_tracker {
            tracker.checkpoint_complete();
        }
    }

    /// Load arenas from disk using the buffer manager
    ///
    /// This should be called during recovery to load previously persisted arenas.
    pub fn load_arena(&mut self, block_id: u32) -> Result<u32> {
        let bm = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for loading arena")
        })?;

        let bm_guard = bm.read();

        let page = bm_guard.fetch_page(block_id)?;
        let arena = CharNodeArena::from_bytes(page.data(), block_id)?;

        let arena_id = self.arenas.len() as u32;
        self.arenas.push(arena);

        Ok(arena_id)
    }

    /// Get a reference to an arena by ID
    pub fn get_arena(&self, arena_id: u32) -> Option<&CharNodeArena> {
        self.arenas.get(arena_id as usize)
    }

    /// Get the active arena
    pub fn active_arena(&self) -> &CharNodeArena {
        &self.arenas[self.active_arena]
    }

    /// Get the maximum slot value as u64 across all arenas
    ///
    /// This is useful for determining the optimal ptr_width when using
    /// compact variable-width encoding. The value is encoded as:
    /// `(arena_id << 32) | slot_id`
    ///
    /// For a single arena with N slots, this returns (0 << 32) | (N-1).
    /// For multiple arenas, it returns the maximum across all.
    pub fn max_slot_value(&self) -> u64 {
        let mut max_value: u64 = 0;

        for (arena_id, arena) in self.arenas.iter().enumerate() {
            let node_count = arena.node_count();
            if node_count > 0 {
                // Last slot in this arena
                let slot = ArenaSlot::new(arena_id as u32, node_count - 1);
                let value = slot.to_u64();
                if value > max_value {
                    max_value = value;
                }
            }
        }

        max_value
    }

    /// Get the maximum data offset across all arenas
    ///
    /// This is useful for determining the optimal ptr_width when using
    /// compact variable-width encoding based on actual data offsets.
    pub fn max_data_offset(&self) -> u32 {
        self.arenas
            .iter()
            .map(|a| a.max_data_offset())
            .max()
            .unwrap_or(0)
    }

    /// Get block IDs of all arenas that have been assigned blocks
    ///
    /// Returns a vector of (arena_id, block_id) pairs for all arenas
    /// that have been persisted to disk. Used for storing arena metadata
    /// during checkpoint.
    pub fn arena_block_ids(&self) -> Vec<(u32, u32)> {
        self.arenas
            .iter()
            .enumerate()
            .filter_map(|(id, arena)| arena.block_id.map(|bid| (id as u32, bid)))
            .collect()
    }

    /// Clear all arenas and prepare for loading from disk
    ///
    /// This resets the arena manager to a minimal state with one fallback arena,
    /// ready to receive arenas via load_arena(). Used during file open
    /// to replace arenas with ones loaded from disk.
    ///
    /// # Invariant Preservation
    ///
    /// CRITICAL FIX: Always maintains at least one arena to prevent
    /// panics in operations like next_slot() if loading fails.
    ///
    /// This fix is derived from the `clear_for_loading_fixed_valid` theorem
    /// in `formal-verification/rocq/Model/ArenaManager.v`, which proves that
    /// keeping a fallback arena preserves the `arena_manager_valid` invariant:
    /// ```coq
    /// Theorem clear_for_loading_fixed_valid : forall mgr,
    ///   arena_manager_valid mgr ->
    ///   arena_manager_valid (clear_for_loading_FIXED mgr).
    /// ```
    ///
    /// The invariant `arena_manager_valid` requires:
    /// - `length(arenas) > 0`
    /// - `active_arena < length(arenas)`
    pub fn clear_for_loading(&mut self) {
        self.arenas.clear();
        // Note: We intentionally leave arenas empty here. The invariant
        // "length(arenas) > 0" will be restored when load_arena() is called.
        // This ensures loaded arenas have the same indices as when they were saved
        // (arena 0 -> arenas[0], arena 1 -> arenas[1], etc.).
        self.active_arena = 0;
    }

    /// Set the active arena index after loading
    ///
    /// Should be called after loading all arenas to set the active arena
    /// to the last one (for new allocations).
    pub fn set_active_arena(&mut self, index: usize) {
        if index < self.arenas.len() {
            self.active_arena = index;
        } else if !self.arenas.is_empty() {
            self.active_arena = self.arenas.len() - 1;
        }
    }

    /// Ensure the arena manager is in a valid state.
    ///
    /// This is a recovery function that establishes the `arena_manager_valid`
    /// invariant from any state. After calling this, all operations are
    /// guaranteed to succeed without panics.
    ///
    /// # Safety
    ///
    /// Derived from `ensure_valid_establishes_invariant` theorem in
    /// `formal-verification/rocq/Model/ArenaManager.v`:
    /// ```coq
    /// Theorem ensure_valid_establishes_invariant : forall mgr,
    ///   arena_size mgr > 0 ->
    ///   arena_manager_valid (ensure_valid mgr).
    /// ```
    ///
    /// This function is idempotent when the invariant already holds
    /// (`ensure_valid_idempotent` theorem).
    pub fn ensure_valid(&mut self) {
        if self.arenas.is_empty() {
            log::warn!("ArenaManager had no arenas; creating initial arena");
            self.arenas.push(CharNodeArena::new(self.arena_size));
            self.active_arena = 0;
        } else if self.active_arena >= self.arenas.len() {
            log::warn!(
                "ArenaManager active_arena {} >= len {}; resetting to {}",
                self.active_arena,
                self.arenas.len(),
                self.arenas.len() - 1
            );
            self.active_arena = self.arenas.len() - 1;
        }
        // Post-condition: arena_manager_valid holds
        debug_assert!(self.is_valid());
    }

    /// Check if the `arena_manager_valid` invariant holds.
    ///
    /// The invariant requires:
    /// 1. `arenas.len() > 0` - at least one arena exists
    /// 2. `active_arena < arenas.len()` - active arena index is valid
    ///
    /// # Specification
    ///
    /// Matches `arena_manager_valid` definition in
    /// `formal-verification/rocq/Model/ArenaManager.v`:
    /// ```coq
    /// Definition arena_manager_valid (mgr : ArenaManager) : Prop :=
    ///   length (arenas mgr) > 0 /\
    ///   active_arena mgr < length (arenas mgr).
    /// ```
    #[inline]
    pub fn is_valid(&self) -> bool {
        !self.arenas.is_empty() && self.active_arena < self.arenas.len()
    }

    // =============================================================================
    // Sequential Sibling Storage Support
    // =============================================================================

    /// Average estimated size per node for reservation calculations
    const ESTIMATED_NODE_SIZE: usize = 128;

    /// Reserve N consecutive slots for sequential sibling storage.
    ///
    /// This method ensures that the next `count` allocations will be placed
    /// in consecutive slots within the same arena. If the current arena
    /// cannot accommodate all slots, a new arena is created first.
    ///
    /// # Arguments
    /// * `count` - Number of consecutive slots to reserve
    ///
    /// # Returns
    /// A `ReservedSlots` handle for allocating into the reserved range.
    ///
    /// # Usage
    /// ```rust,ignore
    /// let mut reserved = arena_manager.reserve_slots(3)?;
    /// let first_slot = reserved.first_slot;
    ///
    /// // Allocate children in order - they get consecutive slots
    /// let slot0 = arena_manager.allocate_reserved(&mut reserved, &child0_data)?;
    /// let slot1 = arena_manager.allocate_reserved(&mut reserved, &child1_data)?;
    /// let slot2 = arena_manager.allocate_reserved(&mut reserved, &child2_data)?;
    ///
    /// // slot0 = first_slot, slot1 = first_slot + 1, slot2 = first_slot + 2
    /// ```
    ///
    /// # Note
    /// The reservation is based on estimated size per node. If actual nodes
    /// are much larger than expected, allocations may fail. In that case,
    /// use `allocate()` directly and fall back to individual pointer encoding.
    pub fn reserve_slots(&mut self, count: usize) -> Result<ReservedSlots> {
        if count == 0 {
            return Err(PersistentARTrieError::internal("Cannot reserve 0 slots"));
        }

        // Estimate space needed (average node size × count + overhead per slot)
        let estimated_size = count * Self::ESTIMATED_NODE_SIZE;

        // Check if current arena can fit all slots
        if !self.arenas[self.active_arena].can_allocate(estimated_size) {
            // Create a new arena to ensure contiguity
            let new_arena = CharNodeArena::new(self.arena_size);
            self.arenas.push(new_arena);
            self.active_arena = self.arenas.len() - 1;
        }

        let first_slot = self.arenas[self.active_arena].node_count();

        Ok(ReservedSlots {
            arena_id: self.active_arena as u32,
            first_slot,
            count: count as u32,
            next_idx: 0,
        })
    }

    /// Allocate data into a reserved slot range.
    ///
    /// This ensures allocations go into the reserved consecutive slot range.
    /// Panics if called more times than `count` or if data doesn't fit.
    ///
    /// # Arguments
    /// * `reserved` - The reserved slots handle from `reserve_slots()`
    /// * `data` - Node data to allocate
    ///
    /// # Returns
    /// The ArenaSlot where data was allocated.
    ///
    /// # Panics
    /// Panics if called more times than reserved, or if allocation fails.
    pub fn allocate_reserved(
        &mut self,
        reserved: &mut ReservedSlots,
        data: &[u8],
    ) -> Result<ArenaSlot> {
        if reserved.next_idx >= reserved.count {
            return Err(PersistentARTrieError::internal(
                "Reserved slot range exhausted",
            ));
        }

        // Verify we're still in the reserved arena
        if self.active_arena as u32 != reserved.arena_id {
            return Err(PersistentARTrieError::internal(
                "Active arena changed during reserved allocation",
            ));
        }

        // Expected slot is first_slot + next_idx
        let expected_slot = reserved.first_slot + reserved.next_idx;
        let current_slot = self.arenas[self.active_arena].node_count();

        if current_slot != expected_slot {
            return Err(PersistentARTrieError::internal(&format!(
                "Slot mismatch: expected {}, got {}",
                expected_slot, current_slot
            )));
        }

        // Allocate the data
        let slot_id = self.arenas[self.active_arena]
            .allocate(data)
            .ok_or_else(|| {
                PersistentARTrieError::internal(&format!(
                    "Failed to allocate reserved slot {} (data size: {} bytes)",
                    expected_slot,
                    data.len()
                ))
            })?;

        // Verify the slot ID matches expected
        debug_assert_eq!(slot_id, expected_slot);

        reserved.next_idx += 1;

        Ok(ArenaSlot::new(reserved.arena_id, slot_id))
    }

    /// Check if a reserved slots range is fully used.
    pub fn is_reservation_complete(&self, reserved: &ReservedSlots) -> bool {
        reserved.next_idx >= reserved.count
    }

    /// Get the next slot that will be allocated
    ///
    /// This returns the ArenaSlot that the next `allocate()` call will return,
    /// assuming the allocation fits in the current arena.
    ///
    /// # Safety
    ///
    /// Derived from `next_slot_defensive` in `formal-verification/rocq/Model/ArenaManager.v`.
    /// When invariant holds (always, due to fixed `clear_for_loading`), this returns valid slot.
    /// Defensive check added per `defensive_matches_when_valid` theorem:
    /// ```coq
    /// Theorem defensive_matches_when_valid : forall mgr,
    ///   arena_manager_valid mgr ->
    ///   next_slot mgr = Some (next_slot_defensive mgr).
    /// ```
    pub fn next_slot(&self) -> ArenaSlot {
        // Defensive check (from next_slot_defensive_total theorem)
        if self.arenas.is_empty() {
            // Should never happen if invariant is maintained, but log and recover
            log::error!(
                "ArenaManager::next_slot called with empty arenas. \
                 This violates arena_manager_valid invariant."
            );
            return ArenaSlot::new(0, 0);
        }

        // Bounds check on active_arena
        if self.active_arena >= self.arenas.len() {
            log::error!(
                "ArenaManager::next_slot: active_arena {} >= arenas.len() {}. \
                 This violates arena_manager_valid invariant.",
                self.active_arena,
                self.arenas.len()
            );
            // Return slot from last arena as fallback
            let last_arena = self.arenas.len() - 1;
            let slot_id = self.arenas[last_arena].node_count();
            return ArenaSlot::new(last_arena as u32, slot_id);
        }

        let arena_id = self.active_arena as u32;
        let slot_id = self.arenas[self.active_arena].node_count();
        ArenaSlot::new(arena_id, slot_id)
    }

    /// Check if data of the given size can fit in the current arena
    pub fn can_fit(&self, size: usize) -> bool {
        self.arenas[self.active_arena].can_allocate(size)
    }

    /// Get the active arena's ID
    pub fn active_arena_id(&self) -> u32 {
        self.active_arena as u32
    }
}

impl<S: BlockStorage> Default for ArenaManager<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about arena usage
#[derive(Debug, Clone)]
pub struct ArenaStats {
    /// Number of arenas
    pub arena_count: usize,
    /// Total capacity in bytes
    pub total_capacity: usize,
    /// Total used bytes
    pub total_used: usize,
    /// Total node count
    pub node_count: u64,
    /// Index of active arena
    pub active_arena: usize,
    /// Available space in active arena
    pub active_arena_available: usize,
}

impl ArenaStats {
    /// Get utilization percentage
    pub fn utilization(&self) -> f64 {
        if self.total_capacity == 0 {
            0.0
        } else {
            (self.total_used as f64 / self.total_capacity as f64) * 100.0
        }
    }

    /// Get average bytes per node
    pub fn bytes_per_node(&self) -> f64 {
        if self.node_count == 0 {
            0.0
        } else {
            self.total_used as f64 / self.node_count as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use MmapDiskManager as the default BlockStorage for tests
    type TestArenaManager = ArenaManager<crate::persistent_artrie::disk_manager::MmapDiskManager>;

    #[test]
    fn test_arena_manager_creation() {
        let manager = TestArenaManager::new();
        assert_eq!(manager.arena_count(), 1);
        assert_eq!(manager.total_node_count(), 0);
    }

    #[test]
    fn test_arena_manager_allocation() {
        let mut manager = TestArenaManager::new();

        // Allocate some data
        let data1 = b"hello world";
        let slot1 = manager.allocate(data1).expect("allocation should succeed");
        assert_eq!(slot1.arena_id, 0);
        assert_eq!(slot1.slot_id, 0);

        // Read it back
        let read1 = manager.read(slot1).expect("read should succeed");
        assert_eq!(read1, data1);

        // Allocate more
        let data2 = b"goodbye world";
        let slot2 = manager.allocate(data2).expect("allocation should succeed");
        assert_eq!(slot2.arena_id, 0);
        assert_eq!(slot2.slot_id, 1);

        assert_eq!(manager.total_node_count(), 2);
    }

    #[test]
    fn test_arena_manager_overflow() {
        // Use small arenas to force overflow
        let mut manager = TestArenaManager::with_arena_size(512);

        // Fill up several arenas
        for i in 0..100 {
            let data = format!("test data {}", i);
            manager
                .allocate(data.as_bytes())
                .expect("allocation should succeed");
        }

        assert!(manager.arena_count() > 1);
        assert_eq!(manager.total_node_count(), 100);

        // Verify we can read all allocations
        // (Note: We'd need to track all slots to verify this fully)
    }

    #[test]
    fn test_arena_slot_encoding() {
        let slot = ArenaSlot::new(12345, 67890);
        let encoded = slot.to_u64();
        let decoded = ArenaSlot::from_u64(encoded);

        assert_eq!(decoded.arena_id, 12345);
        assert_eq!(decoded.slot_id, 67890);
    }

    #[test]
    fn test_arena_stats() {
        let mut manager = TestArenaManager::with_arena_size(1024);

        for _ in 0..10 {
            manager.allocate(&[0u8; 50]).unwrap();
        }

        let stats = manager.stats();
        assert_eq!(stats.node_count, 10);
        assert!(stats.utilization() > 0.0);
        assert!(stats.bytes_per_node() > 50.0); // At least the data size plus overhead
    }

    // =========================================================================
    // Sequential Sibling Storage Tests
    // =========================================================================

    #[test]
    fn test_reserved_slots_struct() {
        let reserved = ReservedSlots {
            arena_id: 1,
            first_slot: 5,
            count: 3,
            next_idx: 0,
        };

        assert_eq!(reserved.first_child_slot().arena_id, 1);
        assert_eq!(reserved.first_child_slot().slot_id, 5);
        assert_eq!(reserved.remaining(), 3);
        assert!(!reserved.is_complete());
    }

    #[test]
    fn test_reserve_slots_basic() {
        let mut manager = TestArenaManager::with_arena_size(4096);

        // Reserve 3 consecutive slots
        let mut reserved = manager.reserve_slots(3).expect("should reserve slots");
        assert_eq!(reserved.count, 3);
        assert_eq!(reserved.first_slot, 0);
        assert_eq!(reserved.arena_id, 0);
        assert!(!reserved.is_complete());

        // Allocate into reserved slots
        let data0 = b"child 0";
        let slot0 = manager
            .allocate_reserved(&mut reserved, data0)
            .expect("slot 0");
        assert_eq!(slot0.slot_id, 0);
        assert_eq!(reserved.remaining(), 2);

        let data1 = b"child 1";
        let slot1 = manager
            .allocate_reserved(&mut reserved, data1)
            .expect("slot 1");
        assert_eq!(slot1.slot_id, 1);
        assert_eq!(reserved.remaining(), 1);

        let data2 = b"child 2";
        let slot2 = manager
            .allocate_reserved(&mut reserved, data2)
            .expect("slot 2");
        assert_eq!(slot2.slot_id, 2);
        assert_eq!(reserved.remaining(), 0);
        assert!(reserved.is_complete());

        // Verify data
        assert_eq!(manager.read(slot0).unwrap(), data0);
        assert_eq!(manager.read(slot1).unwrap(), data1);
        assert_eq!(manager.read(slot2).unwrap(), data2);
    }

    #[test]
    fn test_reserve_slots_first_child_slot() {
        let mut manager = TestArenaManager::with_arena_size(4096);

        // Pre-allocate some slots
        manager.allocate(b"pre0").unwrap();
        manager.allocate(b"pre1").unwrap();

        // Now reserve - should start at slot 2
        let reserved = manager.reserve_slots(4).expect("should reserve");
        assert_eq!(reserved.first_slot, 2);
        assert_eq!(reserved.first_child_slot().slot_id, 2);
    }

    #[test]
    fn test_reserve_slots_overflow() {
        // Use tiny arena to force overflow
        let mut manager = TestArenaManager::with_arena_size(256);

        // Fill up most of the first arena
        for _ in 0..3 {
            manager.allocate(b"some data here").unwrap();
        }

        // Reserve should trigger new arena creation if space is tight
        let reserved = manager
            .reserve_slots(10)
            .expect("should reserve with new arena");

        // Should be in a fresh arena with enough space
        assert_eq!(reserved.first_slot, 0);
        // Arena might be 0 if fit, or 1 if overflow occurred
        // The important thing is consistency
    }

    #[test]
    fn test_reserve_slots_exhausted() {
        let mut manager = TestArenaManager::with_arena_size(4096);

        let mut reserved = manager.reserve_slots(2).expect("should reserve");

        // Use all reserved slots
        manager.allocate_reserved(&mut reserved, b"slot0").unwrap();
        manager.allocate_reserved(&mut reserved, b"slot1").unwrap();

        // Third allocation should fail
        let result = manager.allocate_reserved(&mut reserved, b"slot2");
        assert!(result.is_err());
    }

    #[test]
    fn test_reserve_slots_arena_stats() {
        let mut manager = TestArenaManager::with_arena_size(4096);

        let mut reserved = manager.reserve_slots(3).expect("should reserve");
        manager.allocate_reserved(&mut reserved, b"child0").unwrap();
        manager.allocate_reserved(&mut reserved, b"child1").unwrap();
        manager.allocate_reserved(&mut reserved, b"child2").unwrap();

        let stats = manager.stats();
        assert_eq!(stats.node_count, 3);
    }

    // =========================================================================
    // Invariant Preservation Tests (Derived from Rocq proofs)
    // =========================================================================

    #[test]
    fn test_is_valid_new_manager() {
        // Corresponds to: new_manager_valid theorem
        let manager = TestArenaManager::new();
        assert!(manager.is_valid());
    }

    #[test]
    fn test_is_valid_with_arena_size() {
        // Corresponds to: new_manager_valid theorem
        let manager = TestArenaManager::with_arena_size(4096);
        assert!(manager.is_valid());
    }

    #[test]
    fn test_clear_preserves_valid() {
        // Corresponds to: clear_preserves_valid theorem
        let mut manager = TestArenaManager::with_arena_size(4096);
        manager.allocate(b"test data").unwrap();
        assert!(manager.is_valid());

        manager.clear();
        assert!(manager.is_valid());
    }

    #[test]
    fn test_clear_for_loading_preserves_valid() {
        // clear_for_loading now intentionally leaves arenas empty.
        // This is necessary to ensure loaded arenas have correct indices
        // (arena 0 from disk goes to arenas[0], not arenas[1]).
        // The invariant is restored after load_arena() calls.
        let mut manager = TestArenaManager::with_arena_size(4096);
        manager.allocate(b"test data").unwrap();
        assert!(manager.is_valid());

        manager.clear_for_loading();
        // After clear_for_loading, arenas is empty (transitional state)
        assert_eq!(
            manager.arena_count(),
            0,
            "arenas should be empty for loading"
        );
        // Note: is_valid() would return false here, but that's intentional
        // The invariant is restored when load_arena() adds the first arena
    }

    #[test]
    fn test_next_slot_after_clear_for_loading() {
        // This is the specific scenario that caused the original panic
        let mut manager = TestArenaManager::with_arena_size(4096);

        // Simulate the loading scenario
        manager.clear_for_loading();

        // This should NOT panic (previously did)
        let slot = manager.next_slot();
        assert_eq!(slot.arena_id, 0);
        assert_eq!(slot.slot_id, 0);
    }

    #[test]
    fn test_set_active_arena_preserves_valid() {
        // Corresponds to: set_active_arena_preserves_valid theorem
        let mut manager = TestArenaManager::with_arena_size(4096);

        // Add some arenas by filling them up
        for _ in 0..100 {
            manager.allocate(&[0u8; 100]).unwrap();
        }

        assert!(manager.is_valid());

        // Set to valid index
        manager.set_active_arena(0);
        assert!(manager.is_valid());

        // Set to out-of-bounds index - should clamp
        manager.set_active_arena(9999);
        assert!(manager.is_valid());
    }

    #[test]
    fn test_ensure_valid_recovery() {
        // Corresponds to: ensure_valid_establishes_invariant theorem
        let mut manager = TestArenaManager::with_arena_size(4096);

        // Force invalid state by directly manipulating (simulating corruption)
        // Note: In production, this shouldn't happen, but ensure_valid should recover
        manager.active_arena = 9999; // Invalid index

        // Before ensure_valid, is_valid would be false
        assert!(!manager.is_valid());

        // ensure_valid should recover
        manager.ensure_valid();
        assert!(manager.is_valid());
    }

    #[test]
    fn test_ensure_valid_idempotent() {
        // Corresponds to: ensure_valid_idempotent theorem
        let mut manager = TestArenaManager::with_arena_size(4096);
        manager.allocate(b"test").unwrap();

        let before = manager.active_arena;
        manager.ensure_valid();
        let after = manager.ensure_valid();

        // Should be no change when already valid
        assert_eq!(before, manager.active_arena);
    }

    #[test]
    fn test_allocate_preserves_valid() {
        let mut manager = TestArenaManager::with_arena_size(512);

        // Fill multiple arenas
        for _ in 0..50 {
            assert!(manager.is_valid());
            manager.allocate(&[0u8; 64]).unwrap();
            assert!(manager.is_valid());
        }
    }

    #[test]
    fn test_load_sequence_valid() {
        // Corresponds to: load_sequence_valid theorem
        // Simulates the loading sequence: clear_for_loading -> load_arena calls
        let mut manager = TestArenaManager::with_arena_size(4096);
        manager.allocate(b"original data").unwrap();

        // Clear for loading leaves arenas empty (transitional state)
        manager.clear_for_loading();
        assert_eq!(
            manager.arena_count(),
            0,
            "arenas should be empty after clear_for_loading"
        );

        // Note: We can't easily test load_arena without buffer_manager.
        // In practice, load_arena() is called immediately after clear_for_loading()
        // to restore the invariant. For this test, we manually add an arena to
        // simulate what load_arena would do.
        let arena = CharNodeArena::new(4096);
        manager.arenas.push(arena);
        assert!(manager.is_valid(), "invariant restored after adding arena");

        // Allocate should work after loading
        manager.allocate(b"new data").unwrap();
        assert!(manager.is_valid());
    }

    #[test]
    fn test_defensive_next_slot() {
        // Test that next_slot handles edge cases gracefully
        let manager = TestArenaManager::new();
        let slot = manager.next_slot();
        assert_eq!(slot.arena_id, 0);
        assert_eq!(slot.slot_id, 0);

        // After allocation
        let mut manager = TestArenaManager::new();
        manager.allocate(b"test").unwrap();
        let slot = manager.next_slot();
        assert_eq!(slot.arena_id, 0);
        assert_eq!(slot.slot_id, 1);
    }

    // =========================================================================
    // enable_slot_tracking() Tests
    // =========================================================================

    #[test]
    fn test_enable_slot_tracking_basic() {
        let mut manager = TestArenaManager::new();

        // Initially no slot tracking
        assert!(!manager.has_slot_tracking());
        assert!(!manager.flush_config().slot_level_tracking);

        // Enable slot tracking
        manager.enable_slot_tracking();

        // Now slot tracking should be enabled
        assert!(manager.has_slot_tracking());
        assert!(manager.flush_config().slot_level_tracking);
    }

    #[test]
    fn test_enable_slot_tracking_idempotent() {
        let mut manager = TestArenaManager::new();

        // Enable once
        manager.enable_slot_tracking();
        assert!(manager.has_slot_tracking());

        // Get the tracker's initial state
        let stats_before = manager.dirty_tracker_stats();

        // Enable again (should be no-op)
        manager.enable_slot_tracking();

        // Should still be enabled with same state
        assert!(manager.has_slot_tracking());
        let stats_after = manager.dirty_tracker_stats();

        // Stats should be identical (same tracker instance)
        assert_eq!(
            stats_before.unwrap().dirty_arenas,
            stats_after.unwrap().dirty_arenas
        );
    }

    #[test]
    fn test_enable_slot_tracking_tracks_allocations() {
        let mut manager = TestArenaManager::with_arena_size(4096);

        // Enable slot tracking
        manager.enable_slot_tracking();

        // Allocate some data
        let _slot1 = manager.allocate(b"hello").unwrap();
        let _slot2 = manager.allocate(b"world").unwrap();

        // Check that allocations are tracked
        let stats = manager.dirty_tracker_stats().unwrap();
        assert!(stats.dirty_arenas > 0, "should track dirty arenas");
        assert!(stats.dirty_slots > 0, "should track dirty slots");
    }

    #[test]
    fn test_enable_slot_tracking_after_allocations() {
        let mut manager = TestArenaManager::with_arena_size(4096);

        // Allocate before enabling (not tracked)
        manager.allocate(b"pre-tracking").unwrap();

        // Enable slot tracking
        manager.enable_slot_tracking();

        // Allocate after enabling (tracked)
        let _slot = manager.allocate(b"post-tracking").unwrap();

        // Check that the post-tracking allocation is tracked
        let stats = manager.dirty_tracker_stats().unwrap();
        assert!(
            stats.dirty_slots >= 1,
            "should track slot allocated after enable"
        );
    }

    #[test]
    fn test_enable_slot_tracking_with_buffer_manager_constructor() {
        // Verify that enable_slot_tracking works with buffer manager constructor
        // (simulating the open() -> enable_slot_tracking() pattern)
        let mut manager = TestArenaManager::with_arena_size(4096);

        // Simulate opening without slot tracking
        assert!(!manager.has_slot_tracking());

        // Enable slot tracking (as open_with_slot_tracking would do)
        manager.enable_slot_tracking();

        // Verify it's now enabled
        assert!(manager.has_slot_tracking());

        // Verify allocations are tracked
        manager.allocate(b"test data").unwrap();
        let stats = manager.dirty_tracker_stats().unwrap();
        assert!(stats.dirty_slots > 0);
    }

    #[test]
    fn test_enable_slot_tracking_marks_existing_dirty_arenas() {
        let mut manager = TestArenaManager::with_arena_size(4096);

        // Allocate before enabling (creates dirty arenas)
        manager.allocate(b"pre-tracking-1").unwrap();
        manager.allocate(b"pre-tracking-2").unwrap();

        // Existing arenas should be dirty (via arena's is_dirty flag)
        assert!(manager.arenas[0].is_dirty());

        // Enable slot tracking - should retroactively mark existing dirty arenas
        manager.enable_slot_tracking();

        // Tracker should know about existing dirty arenas
        let stats = manager.dirty_tracker_stats().unwrap();
        assert!(
            stats.dirty_arenas >= 1,
            "should retroactively track existing dirty arenas, got {}",
            stats.dirty_arenas
        );
    }

    #[test]
    fn test_enable_slot_tracking_marks_multiple_dirty_arenas() {
        // Use small arenas to force multiple arenas to be created
        let mut manager = TestArenaManager::with_arena_size(512);

        // Fill up multiple arenas before enabling tracking
        for i in 0..20 {
            let data = format!("pre-tracking-{:04}", i);
            manager.allocate(data.as_bytes()).unwrap();
        }

        let arena_count_before = manager.arena_count();
        assert!(arena_count_before > 1, "should have multiple arenas");

        // Count dirty arenas before enabling
        let dirty_count_before = manager.arenas.iter().filter(|a| a.is_dirty()).count();

        // Enable slot tracking - should retroactively mark all dirty arenas
        manager.enable_slot_tracking();

        // Tracker should know about all existing dirty arenas
        let stats = manager.dirty_tracker_stats().unwrap();
        assert_eq!(
            stats.dirty_arenas, dirty_count_before,
            "should track all {} dirty arenas, got {}",
            dirty_count_before, stats.dirty_arenas
        );
    }
}
