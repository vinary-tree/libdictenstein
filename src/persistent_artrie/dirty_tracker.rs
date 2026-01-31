//! DirtyTracker - Incremental update tracking for efficient checkpoints
//!
//! This module tracks which arenas and slots have been modified since the last
//! checkpoint, enabling incremental persistence that only writes dirty data.
//!
//! ## Problem
//!
//! Full checkpoint writes all data, even unmodified:
//! ```text
//! Before: modify 1 node out of 1M
//! Full checkpoint: write all 1M nodes (~100+ arenas)
//! I/O: 100+ × 256KB = 25+ MB
//! ```
//!
//! ## Solution
//!
//! Track dirty arenas and only write modified ones:
//! ```text
//! After: modify 1 node in arena 42
//! Incremental checkpoint: write only arena 42
//! I/O: 1 × 256KB = 256 KB
//! ```
//!
//! ## Expected Impact
//!
//! - **I/O**: 90%+ reduction for small updates
//! - **Latency**: Much faster checkpoints
//! - **Memory**: Small tracking overhead

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

/// DirtyTracker - Tracks modified arenas for incremental checkpointing
///
/// This struct maintains a set of dirty arena IDs and optionally tracks
/// individual dirty slots within each arena for fine-grained updates.
#[derive(Debug)]
pub struct DirtyTracker {
    /// Set of arena IDs that have been modified
    dirty_arenas: HashSet<u32>,
    /// Optional per-arena slot tracking (arena_id -> set of slot_ids)
    dirty_slots: HashMap<u32, HashSet<u32>>,
    /// Current epoch number (incremented on checkpoint)
    epoch: AtomicU64,
    /// Whether to track individual slots (vs just arenas)
    track_slots: bool,
    /// Statistics
    total_marks: u64,
    checkpoint_count: u64,
}

impl DirtyTracker {
    /// Create a new DirtyTracker
    ///
    /// # Arguments
    ///
    /// * `track_slots` - If true, track individual slot modifications within arenas.
    ///   This enables more granular partial writes but uses more memory.
    pub fn new(track_slots: bool) -> Self {
        Self {
            dirty_arenas: HashSet::new(),
            dirty_slots: HashMap::new(),
            epoch: AtomicU64::new(0),
            track_slots,
            total_marks: 0,
            checkpoint_count: 0,
        }
    }

    /// Create a tracker that only tracks arenas (not individual slots)
    pub fn arena_level() -> Self {
        Self::new(false)
    }

    /// Create a tracker that tracks both arenas and slots
    pub fn slot_level() -> Self {
        Self::new(true)
    }

    /// Mark an arena as dirty
    ///
    /// Call this whenever a node is allocated or modified in an arena.
    pub fn mark_arena_dirty(&mut self, arena_id: u32) {
        self.dirty_arenas.insert(arena_id);
        self.total_marks += 1;
    }

    /// Mark a specific slot within an arena as dirty
    ///
    /// Also marks the arena as dirty.
    pub fn mark_slot_dirty(&mut self, arena_id: u32, slot_id: u32) {
        self.dirty_arenas.insert(arena_id);

        if self.track_slots {
            self.dirty_slots
                .entry(arena_id)
                .or_insert_with(HashSet::new)
                .insert(slot_id);
        }

        self.total_marks += 1;
    }

    /// Check if an arena is dirty
    pub fn is_arena_dirty(&self, arena_id: u32) -> bool {
        self.dirty_arenas.contains(&arena_id)
    }

    /// Check if a specific slot is dirty
    ///
    /// Returns true if the arena is dirty (in non-slot-tracking mode)
    /// or if the specific slot is dirty (in slot-tracking mode).
    pub fn is_slot_dirty(&self, arena_id: u32, slot_id: u32) -> bool {
        if !self.dirty_arenas.contains(&arena_id) {
            return false;
        }

        if !self.track_slots {
            // If not tracking slots, any slot in a dirty arena is considered dirty
            return true;
        }

        self.dirty_slots
            .get(&arena_id)
            .is_some_and(|slots| slots.contains(&slot_id))
    }

    /// Get all dirty arena IDs
    pub fn dirty_arena_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.dirty_arenas.iter().copied()
    }

    /// Get dirty slot IDs for a specific arena
    ///
    /// Returns None if not tracking slots or arena is not dirty.
    pub fn dirty_slot_ids(&self, arena_id: u32) -> Option<impl Iterator<Item = u32> + '_> {
        if !self.track_slots {
            return None;
        }

        self.dirty_slots.get(&arena_id).map(|slots| slots.iter().copied())
    }

    /// Get the number of dirty arenas
    pub fn dirty_arena_count(&self) -> usize {
        self.dirty_arenas.len()
    }

    /// Get the total number of dirty slots across all arenas
    ///
    /// Returns 0 if not tracking slots.
    pub fn dirty_slot_count(&self) -> usize {
        if !self.track_slots {
            return 0;
        }

        self.dirty_slots.values().map(|s| s.len()).sum()
    }

    /// Clear tracking for a checkpoint completion
    ///
    /// Call this after successfully persisting all dirty data.
    /// Increments the epoch counter.
    pub fn checkpoint_complete(&mut self) {
        self.dirty_arenas.clear();
        self.dirty_slots.clear();
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.checkpoint_count += 1;
    }

    /// Clear only dirty tracking for specific arenas
    ///
    /// Useful for partial checkpoint scenarios.
    pub fn clear_arenas(&mut self, arena_ids: &[u32]) {
        for &arena_id in arena_ids {
            self.dirty_arenas.remove(&arena_id);
            self.dirty_slots.remove(&arena_id);
        }
    }

    /// Get the current epoch number
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Get statistics
    pub fn stats(&self) -> DirtyTrackerStats {
        DirtyTrackerStats {
            dirty_arenas: self.dirty_arenas.len(),
            dirty_slots: self.dirty_slot_count(),
            total_marks: self.total_marks,
            checkpoint_count: self.checkpoint_count,
            epoch: self.epoch(),
            track_slots: self.track_slots,
        }
    }

    /// Merge another tracker into this one
    ///
    /// Useful for combining tracking from multiple threads.
    pub fn merge(&mut self, other: &DirtyTracker) {
        for &arena_id in &other.dirty_arenas {
            self.dirty_arenas.insert(arena_id);
        }

        if self.track_slots && other.track_slots {
            for (&arena_id, slots) in &other.dirty_slots {
                self.dirty_slots
                    .entry(arena_id)
                    .or_insert_with(HashSet::new)
                    .extend(slots.iter().copied());
            }
        }

        self.total_marks += other.total_marks;
    }
}

impl Default for DirtyTracker {
    fn default() -> Self {
        Self::arena_level()
    }
}

/// Statistics about dirty tracking
#[derive(Debug, Clone)]
pub struct DirtyTrackerStats {
    /// Number of dirty arenas
    pub dirty_arenas: usize,
    /// Number of dirty slots (0 if not tracking slots)
    pub dirty_slots: usize,
    /// Total mark operations since creation
    pub total_marks: u64,
    /// Number of checkpoint completions
    pub checkpoint_count: u64,
    /// Current epoch number
    pub epoch: u64,
    /// Whether slot-level tracking is enabled
    pub track_slots: bool,
}

impl DirtyTrackerStats {
    /// Estimate the I/O savings from incremental checkpoint
    ///
    /// # Arguments
    ///
    /// * `total_arenas` - Total number of arenas in the system
    /// * `arena_size` - Size of each arena in bytes
    ///
    /// # Returns
    ///
    /// A tuple of (full_checkpoint_bytes, incremental_bytes, savings_percent)
    pub fn estimate_io_savings(&self, total_arenas: usize, arena_size: usize) -> (usize, usize, f64) {
        let full_bytes = total_arenas * arena_size;
        let incremental_bytes = self.dirty_arenas * arena_size;

        let savings = if full_bytes > 0 {
            (1.0 - (incremental_bytes as f64 / full_bytes as f64)) * 100.0
        } else {
            0.0
        };

        (full_bytes, incremental_bytes, savings)
    }
}

/// Batch dirty tracker for collecting marks across threads
///
/// Use this to batch dirty marks in a thread-local tracker, then
/// merge them into the main tracker periodically.
#[derive(Debug)]
pub struct BatchDirtyTracker {
    /// Local tracking
    local: DirtyTracker,
    /// Batch size before auto-merge hint
    batch_threshold: usize,
}

impl BatchDirtyTracker {
    /// Create a new batch tracker
    pub fn new(track_slots: bool, batch_threshold: usize) -> Self {
        Self {
            local: DirtyTracker::new(track_slots),
            batch_threshold,
        }
    }

    /// Mark an arena as dirty
    pub fn mark_arena_dirty(&mut self, arena_id: u32) {
        self.local.mark_arena_dirty(arena_id);
    }

    /// Mark a slot as dirty
    pub fn mark_slot_dirty(&mut self, arena_id: u32, slot_id: u32) {
        self.local.mark_slot_dirty(arena_id, slot_id);
    }

    /// Check if batch should be merged (exceeded threshold)
    pub fn should_merge(&self) -> bool {
        self.local.dirty_arena_count() >= self.batch_threshold
    }

    /// Take the local tracker for merging (replaces with empty)
    pub fn take(&mut self) -> DirtyTracker {
        let track_slots = self.local.track_slots;
        std::mem::replace(&mut self.local, DirtyTracker::new(track_slots))
    }

    /// Get current dirty count
    pub fn dirty_count(&self) -> usize {
        self.local.dirty_arena_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dirty_tracker_creation() {
        let tracker = DirtyTracker::arena_level();
        assert_eq!(tracker.dirty_arena_count(), 0);
        assert_eq!(tracker.epoch(), 0);
    }

    #[test]
    fn test_mark_arena_dirty() {
        let mut tracker = DirtyTracker::arena_level();

        tracker.mark_arena_dirty(1);
        tracker.mark_arena_dirty(2);
        tracker.mark_arena_dirty(1); // Duplicate

        assert_eq!(tracker.dirty_arena_count(), 2);
        assert!(tracker.is_arena_dirty(1));
        assert!(tracker.is_arena_dirty(2));
        assert!(!tracker.is_arena_dirty(3));
    }

    #[test]
    fn test_mark_slot_dirty() {
        let mut tracker = DirtyTracker::slot_level();

        tracker.mark_slot_dirty(1, 100);
        tracker.mark_slot_dirty(1, 200);
        tracker.mark_slot_dirty(2, 50);

        assert_eq!(tracker.dirty_arena_count(), 2);
        assert_eq!(tracker.dirty_slot_count(), 3);

        assert!(tracker.is_slot_dirty(1, 100));
        assert!(tracker.is_slot_dirty(1, 200));
        assert!(tracker.is_slot_dirty(2, 50));
        assert!(!tracker.is_slot_dirty(1, 300));
    }

    #[test]
    fn test_checkpoint_complete() {
        let mut tracker = DirtyTracker::arena_level();

        tracker.mark_arena_dirty(1);
        tracker.mark_arena_dirty(2);
        assert_eq!(tracker.dirty_arena_count(), 2);
        assert_eq!(tracker.epoch(), 0);

        tracker.checkpoint_complete();

        assert_eq!(tracker.dirty_arena_count(), 0);
        assert_eq!(tracker.epoch(), 1);
    }

    #[test]
    fn test_io_savings_estimate() {
        let mut tracker = DirtyTracker::arena_level();

        // Mark 5 out of 100 arenas dirty
        for i in 0..5 {
            tracker.mark_arena_dirty(i);
        }

        let stats = tracker.stats();
        let (full, incremental, savings) = stats.estimate_io_savings(100, 256 * 1024);

        // 100 arenas * 256KB = 25.6 MB full
        assert_eq!(full, 100 * 256 * 1024);
        // 5 arenas * 256KB = 1.28 MB incremental
        assert_eq!(incremental, 5 * 256 * 1024);
        // 95% savings
        assert!((savings - 95.0).abs() < 0.01);
    }

    #[test]
    fn test_dirty_tracker_merge() {
        let mut tracker1 = DirtyTracker::slot_level();
        let mut tracker2 = DirtyTracker::slot_level();

        tracker1.mark_slot_dirty(1, 100);
        tracker1.mark_slot_dirty(2, 200);

        tracker2.mark_slot_dirty(2, 300); // Overlapping arena
        tracker2.mark_slot_dirty(3, 400);

        tracker1.merge(&tracker2);

        assert_eq!(tracker1.dirty_arena_count(), 3); // 1, 2, 3
        assert_eq!(tracker1.dirty_slot_count(), 4); // 100, 200, 300, 400
    }

    #[test]
    fn test_batch_dirty_tracker() {
        let mut batch = BatchDirtyTracker::new(false, 10);

        for i in 0..15 {
            batch.mark_arena_dirty(i);
        }

        assert!(batch.should_merge());
        assert_eq!(batch.dirty_count(), 15);

        let taken = batch.take();
        assert_eq!(taken.dirty_arena_count(), 15);
        assert_eq!(batch.dirty_count(), 0); // Reset after take
    }

    #[test]
    fn test_clear_specific_arenas() {
        let mut tracker = DirtyTracker::arena_level();

        tracker.mark_arena_dirty(1);
        tracker.mark_arena_dirty(2);
        tracker.mark_arena_dirty(3);

        tracker.clear_arenas(&[1, 3]);

        assert!(!tracker.is_arena_dirty(1));
        assert!(tracker.is_arena_dirty(2));
        assert!(!tracker.is_arena_dirty(3));
    }
}
