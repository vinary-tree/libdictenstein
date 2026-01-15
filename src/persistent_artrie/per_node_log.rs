//! Per-Node Logging for Near-Instant Recovery.
//!
//! This module implements per-node redo logging as an alternative to global WAL.
//! Each node stores its own redo log, enabling O(dirty nodes) recovery instead
//! of O(total operations).
//!
//! # Architecture
//!
//! ```text
//! Traditional Global WAL:
//! ┌─────────────────────────────────────────────────┐
//! │ WAL File                                         │
//! │ [Op1][Op2][Op3]...[OpN]                          │
//! │ Recovery: Replay ALL operations O(N)             │
//! └─────────────────────────────────────────────────┘
//!
//! Per-Node Logging:
//! ┌───────────────────┐  ┌───────────────────┐
//! │ Node A            │  │ Node B            │
//! │ ┌───────────────┐ │  │ ┌───────────────┐ │
//! │ │ Inline Log    │ │  │ │ Inline Log    │ │
//! │ │ [+key1][-key2]│ │  │ │ (empty)       │ │
//! │ └───────────────┘ │  │ └───────────────┘ │
//! └───────────────────┘  └───────────────────┘
//! Recovery: Only replay logs of dirty nodes O(D) where D << N
//! ```
//!
//! # Key Features
//!
//! - **Inline Log**: Up to 64 bytes stored with each node
//! - **Overflow Pages**: Larger logs spill to separate pages
//! - **Dirty Tracking**: Bitmap tracks which nodes have non-empty logs
//! - **Parallel Recovery**: Each node can be recovered independently
//! - **Background Compaction**: Merge logs into base asynchronously
//!
//! # References
//!
//! - Bw-Tree (Levandoski et al., ICDE 2013) - Delta record approach
//! - LeanStore (Leis et al., CIDR 2018) - Pointer swizzling
//! - Umbra (Neumann & Freitag, VLDB 2020) - Variable-size pages

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

/// Type alias for node identifiers.
pub type NodeId = u64;

/// Type alias for page identifiers.
pub type PageId = u64;

/// Configuration for per-node logging.
///
/// Per-node logging stores redo information with each node instead of
/// in a global WAL. This enables:
/// - Near-instant recovery (only dirty nodes need replay)
/// - Parallel recovery (each node can be recovered independently)
/// - Better locality (log is adjacent to data)
///
/// # Trade-offs
///
/// | Aspect | Global WAL | Per-Node Log |
/// |--------|-----------|--------------|
/// | Recovery time | O(total ops) | O(dirty nodes) |
/// | Write amplification | ~2x | ~1.5x (inline) |
/// | Complexity | Low | High |
/// | Space overhead | Separate file | ~10% inline |
/// | Parallelism | Sequential | Per-node parallel |
#[derive(Debug, Clone)]
pub struct PerNodeLogConfig {
    /// Maximum inline log size in bytes.
    ///
    /// Log entries up to this size are stored inline with the node.
    /// Larger logs overflow to a separate page.
    ///
    /// Default: 64 bytes
    /// Range: 16 - 256
    pub max_inline_log_size: usize,

    /// Maximum total log size per node before compaction.
    ///
    /// When a node's log (inline + overflow) exceeds this size,
    /// compaction is triggered to merge the log into the base.
    ///
    /// Default: 4096 bytes
    /// Range: 256 - 65536
    pub max_log_size: usize,

    /// Compaction threshold (log_size / base_size ratio).
    ///
    /// When the log grows to this fraction of the base node size,
    /// compaction is triggered.
    ///
    /// Default: 1.0 (log can be as large as base)
    /// Range: 0.25 - 4.0
    pub compaction_threshold: f64,

    /// Number of log entries before compaction.
    ///
    /// Alternative trigger: compact after this many log entries.
    ///
    /// Default: 100
    /// Range: 10 - 10000
    pub max_log_entries: usize,

    /// Enable background compaction.
    ///
    /// When true, compaction runs in a background thread.
    /// When false, compaction is synchronous (blocking).
    ///
    /// Default: true
    pub background_compaction: bool,

    /// Compaction batch size.
    ///
    /// Number of nodes to compact per background batch.
    ///
    /// Default: 16
    /// Range: 1 - 256
    pub compaction_batch_size: usize,

    /// Enable parallel recovery.
    ///
    /// When true, recovery replays each node's log in parallel.
    ///
    /// Default: true
    pub parallel_recovery: bool,

    /// Number of threads for parallel recovery.
    ///
    /// Default: number of CPU cores (capped at 16)
    /// Range: 1 - 256
    pub recovery_threads: usize,

    /// Track dirty nodes for recovery.
    ///
    /// Maintains a persistent set of nodes with non-empty logs.
    /// Enables O(dirty) recovery instead of O(all).
    ///
    /// Default: true
    pub track_dirty_nodes: bool,

    /// Whether per-node logging is enabled.
    ///
    /// Default: false (use global WAL by default)
    pub enabled: bool,
}

impl Default for PerNodeLogConfig {
    fn default() -> Self {
        Self {
            max_inline_log_size: 64,
            max_log_size: 4096,
            compaction_threshold: 1.0,
            max_log_entries: 100,
            background_compaction: true,
            compaction_batch_size: 16,
            parallel_recovery: true,
            recovery_threads: std::thread::available_parallelism()
                .map(|p| p.get().min(16))
                .unwrap_or(4),
            track_dirty_nodes: true,
            enabled: false,
        }
    }
}

/// Log entry types for per-node logging.
///
/// Each entry represents an atomic operation on a node that can be
/// replayed during recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeLogEntry {
    /// Insert a child edge.
    InsertChild {
        /// Key byte for the child edge
        key: u8,
        /// Node ID of the child
        child_id: NodeId,
    },

    /// Remove a child edge.
    RemoveChild {
        /// Key byte for the child edge to remove
        key: u8,
    },

    /// Update the node's value (for leaf nodes).
    SetValue {
        /// Serialized value bytes
        value: Vec<u8>,
    },

    /// Clear the node's value.
    ClearValue,

    /// Update prefix (path compression).
    SetPrefix {
        /// New prefix bytes
        prefix: Vec<u8>,
    },
}

/// Log entry type discriminators for serialization.
mod log_entry_type {
    pub const INSERT_CHILD: u8 = 0x01;
    pub const REMOVE_CHILD: u8 = 0x02;
    pub const SET_VALUE: u8 = 0x03;
    pub const CLEAR_VALUE: u8 = 0x04;
    pub const SET_PREFIX: u8 = 0x05;
}

impl NodeLogEntry {
    /// Serialize the log entry to bytes.
    ///
    /// Format varies by type:
    /// - InsertChild: [0x01][key:1][child_id:8] = 10 bytes
    /// - RemoveChild: [0x02][key:1] = 2 bytes
    /// - SetValue: [0x03][len:2][value:len] = 3 + len bytes
    /// - ClearValue: [0x04] = 1 byte
    /// - SetPrefix: [0x05][len:1][prefix:len] = 2 + len bytes
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(16);
        match self {
            Self::InsertChild { key, child_id } => {
                buf.push(log_entry_type::INSERT_CHILD);
                buf.push(*key);
                buf.extend_from_slice(&child_id.to_le_bytes());
            }
            Self::RemoveChild { key } => {
                buf.push(log_entry_type::REMOVE_CHILD);
                buf.push(*key);
            }
            Self::SetValue { value } => {
                buf.push(log_entry_type::SET_VALUE);
                buf.extend_from_slice(&(value.len() as u16).to_le_bytes());
                buf.extend_from_slice(value);
            }
            Self::ClearValue => {
                buf.push(log_entry_type::CLEAR_VALUE);
            }
            Self::SetPrefix { prefix } => {
                buf.push(log_entry_type::SET_PREFIX);
                buf.push(prefix.len() as u8);
                buf.extend_from_slice(prefix);
            }
        }
        buf
    }

    /// Deserialize a log entry from bytes.
    ///
    /// Returns the entry and number of bytes consumed, or None if invalid.
    pub fn deserialize(data: &[u8]) -> Option<(Self, usize)> {
        if data.is_empty() {
            return None;
        }

        match data[0] {
            log_entry_type::INSERT_CHILD => {
                // [type:1][key:1][child_id:8] = 10 bytes
                if data.len() < 10 {
                    return None;
                }
                let key = data[1];
                let child_id = u64::from_le_bytes(data[2..10].try_into().ok()?);
                Some((Self::InsertChild { key, child_id }, 10))
            }
            log_entry_type::REMOVE_CHILD => {
                // [type:1][key:1] = 2 bytes
                if data.len() < 2 {
                    return None;
                }
                let key = data[1];
                Some((Self::RemoveChild { key }, 2))
            }
            log_entry_type::SET_VALUE => {
                // [type:1][len:2][value:len] = 3 + len bytes
                if data.len() < 3 {
                    return None;
                }
                let len = u16::from_le_bytes(data[1..3].try_into().ok()?) as usize;
                if data.len() < 3 + len {
                    return None;
                }
                let value = data[3..3 + len].to_vec();
                Some((Self::SetValue { value }, 3 + len))
            }
            log_entry_type::CLEAR_VALUE => {
                Some((Self::ClearValue, 1))
            }
            log_entry_type::SET_PREFIX => {
                // [type:1][len:1][prefix:len] = 2 + len bytes
                if data.len() < 2 {
                    return None;
                }
                let len = data[1] as usize;
                if data.len() < 2 + len {
                    return None;
                }
                let prefix = data[2..2 + len].to_vec();
                Some((Self::SetPrefix { prefix }, 2 + len))
            }
            _ => None,
        }
    }

    /// Get the serialized size of this entry without allocating.
    pub fn serialized_size(&self) -> usize {
        match self {
            Self::InsertChild { .. } => 10,
            Self::RemoveChild { .. } => 2,
            Self::SetValue { value } => 3 + value.len(),
            Self::ClearValue => 1,
            Self::SetPrefix { prefix } => 2 + prefix.len(),
        }
    }
}

/// Inline log buffer for storing small logs within a node.
///
/// This struct manages a fixed-size buffer that can be embedded
/// directly in a node's serialized representation.
#[derive(Debug, Clone)]
pub struct InlineLog {
    /// The log buffer (fixed size, may be partially filled)
    data: Vec<u8>,
    /// Maximum capacity
    capacity: usize,
    /// Current write position
    len: usize,
    /// Number of entries in the log
    entry_count: usize,
}

impl InlineLog {
    /// Create a new inline log with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0u8; capacity],
            capacity,
            len: 0,
            entry_count: 0,
        }
    }

    /// Create from existing data.
    pub fn from_data(data: Vec<u8>, entry_count: usize) -> Self {
        let len = data.len();
        let capacity = data.capacity().max(len);
        Self {
            data,
            capacity,
            len,
            entry_count,
        }
    }

    /// Available space in bytes.
    pub fn available_space(&self) -> usize {
        self.capacity.saturating_sub(self.len)
    }

    /// Current used space in bytes.
    pub fn used_space(&self) -> usize {
        self.len
    }

    /// Number of log entries.
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Check if log is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Try to append an entry to the inline log.
    ///
    /// Returns true if successful, false if not enough space.
    pub fn try_append(&mut self, entry: &NodeLogEntry) -> bool {
        let serialized = entry.serialize();
        if serialized.len() > self.available_space() {
            return false;
        }

        let start = self.len;
        let end = start + serialized.len();

        // Extend data if needed
        if end > self.data.len() {
            self.data.resize(end.max(self.capacity), 0);
        }

        self.data[start..end].copy_from_slice(&serialized);
        self.len = end;
        self.entry_count += 1;
        true
    }

    /// Get the log data as a slice.
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..self.len]
    }

    /// Clear the log.
    pub fn clear(&mut self) {
        self.len = 0;
        self.entry_count = 0;
    }

    /// Iterate over entries in the log.
    pub fn iter(&self) -> InlineLogIter<'_> {
        InlineLogIter {
            data: self.as_slice(),
            offset: 0,
        }
    }
}

/// Iterator over entries in an inline log.
pub struct InlineLogIter<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Iterator for InlineLogIter<'a> {
    type Item = NodeLogEntry;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.data.len() {
            return None;
        }

        let (entry, consumed) = NodeLogEntry::deserialize(&self.data[self.offset..])?;
        self.offset += consumed;
        Some(entry)
    }
}

/// Statistics for per-node logging operations.
#[derive(Debug, Clone, Default)]
pub struct PerNodeLogStats {
    /// Total log entries written.
    pub entries_written: u64,
    /// Total log entries read (during recovery).
    pub entries_read: u64,
    /// Total bytes written to inline logs.
    pub inline_bytes_written: u64,
    /// Total bytes written to overflow pages.
    pub overflow_bytes_written: u64,
    /// Number of nodes with non-empty logs (dirty nodes).
    pub dirty_node_count: usize,
    /// Number of compactions performed.
    pub compactions: u64,
    /// Number of overflow allocations.
    pub overflow_allocations: u64,
    /// Number of overflow page frees.
    pub overflow_frees: u64,
    /// Recovery time in microseconds.
    pub recovery_time_us: u64,
    /// Nodes recovered in parallel.
    pub parallel_recoveries: u64,
}

/// Atomic counters for per-node logging statistics.
#[derive(Debug)]
pub struct PerNodeLogStatsAtomic {
    entries_written: AtomicU64,
    entries_read: AtomicU64,
    inline_bytes_written: AtomicU64,
    overflow_bytes_written: AtomicU64,
    dirty_node_count: AtomicUsize,
    compactions: AtomicU64,
    overflow_allocations: AtomicU64,
    overflow_frees: AtomicU64,
}

impl PerNodeLogStatsAtomic {
    /// Create new atomic stats.
    pub fn new() -> Self {
        Self {
            entries_written: AtomicU64::new(0),
            entries_read: AtomicU64::new(0),
            inline_bytes_written: AtomicU64::new(0),
            overflow_bytes_written: AtomicU64::new(0),
            dirty_node_count: AtomicUsize::new(0),
            compactions: AtomicU64::new(0),
            overflow_allocations: AtomicU64::new(0),
            overflow_frees: AtomicU64::new(0),
        }
    }

    /// Record an entry write.
    pub fn record_entry_written(&self, bytes: usize, is_overflow: bool) {
        self.entries_written.fetch_add(1, Ordering::Relaxed);
        if is_overflow {
            self.overflow_bytes_written.fetch_add(bytes as u64, Ordering::Relaxed);
        } else {
            self.inline_bytes_written.fetch_add(bytes as u64, Ordering::Relaxed);
        }
    }

    /// Record an entry read.
    pub fn record_entry_read(&self) {
        self.entries_read.fetch_add(1, Ordering::Relaxed);
    }

    /// Update dirty node count.
    pub fn set_dirty_count(&self, count: usize) {
        self.dirty_node_count.store(count, Ordering::Relaxed);
    }

    /// Increment dirty node count.
    pub fn increment_dirty_count(&self) {
        self.dirty_node_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement dirty node count.
    pub fn decrement_dirty_count(&self) {
        self.dirty_node_count.fetch_sub(1, Ordering::Relaxed);
    }

    /// Record a compaction.
    pub fn record_compaction(&self) {
        self.compactions.fetch_add(1, Ordering::Relaxed);
    }

    /// Record overflow allocation.
    pub fn record_overflow_alloc(&self) {
        self.overflow_allocations.fetch_add(1, Ordering::Relaxed);
    }

    /// Record overflow free.
    pub fn record_overflow_free(&self) {
        self.overflow_frees.fetch_add(1, Ordering::Relaxed);
    }

    /// Get a snapshot of the statistics.
    pub fn snapshot(&self) -> PerNodeLogStats {
        PerNodeLogStats {
            entries_written: self.entries_written.load(Ordering::Relaxed),
            entries_read: self.entries_read.load(Ordering::Relaxed),
            inline_bytes_written: self.inline_bytes_written.load(Ordering::Relaxed),
            overflow_bytes_written: self.overflow_bytes_written.load(Ordering::Relaxed),
            dirty_node_count: self.dirty_node_count.load(Ordering::Relaxed),
            compactions: self.compactions.load(Ordering::Relaxed),
            overflow_allocations: self.overflow_allocations.load(Ordering::Relaxed),
            overflow_frees: self.overflow_frees.load(Ordering::Relaxed),
            recovery_time_us: 0,
            parallel_recoveries: 0,
        }
    }
}

impl Default for PerNodeLogStatsAtomic {
    fn default() -> Self {
        Self::new()
    }
}

/// Dirty node tracker for recovery optimization.
///
/// Maintains a set of node IDs that have non-empty logs,
/// enabling O(dirty) recovery instead of O(all).
#[derive(Debug)]
pub struct DirtyNodeTracker {
    /// Set of dirty node IDs
    dirty_nodes: RwLock<HashSet<NodeId>>,
    /// Statistics
    stats: Arc<PerNodeLogStatsAtomic>,
}

impl DirtyNodeTracker {
    /// Create a new dirty node tracker.
    pub fn new(stats: Arc<PerNodeLogStatsAtomic>) -> Self {
        Self {
            dirty_nodes: RwLock::new(HashSet::new()),
            stats,
        }
    }

    /// Mark a node as dirty.
    pub fn mark_dirty(&self, node_id: NodeId) {
        let mut dirty = self.dirty_nodes.write().expect("lock poisoned");
        if dirty.insert(node_id) {
            self.stats.increment_dirty_count();
        }
    }

    /// Mark a node as clean (after compaction).
    pub fn mark_clean(&self, node_id: NodeId) {
        let mut dirty = self.dirty_nodes.write().expect("lock poisoned");
        if dirty.remove(&node_id) {
            self.stats.decrement_dirty_count();
        }
    }

    /// Check if a node is dirty.
    pub fn is_dirty(&self, node_id: NodeId) -> bool {
        let dirty = self.dirty_nodes.read().expect("lock poisoned");
        dirty.contains(&node_id)
    }

    /// Get all dirty node IDs.
    pub fn get_dirty_nodes(&self) -> Vec<NodeId> {
        let dirty = self.dirty_nodes.read().expect("lock poisoned");
        dirty.iter().copied().collect()
    }

    /// Get the number of dirty nodes.
    pub fn dirty_count(&self) -> usize {
        let dirty = self.dirty_nodes.read().expect("lock poisoned");
        dirty.len()
    }

    /// Clear all dirty markers (after full checkpoint).
    pub fn clear(&self) {
        let mut dirty = self.dirty_nodes.write().expect("lock poisoned");
        dirty.clear();
        self.stats.set_dirty_count(0);
    }
}

/// Recovery result for a single node.
#[derive(Debug, Clone)]
pub struct NodeRecoveryResult {
    /// Node ID that was recovered
    pub node_id: NodeId,
    /// Number of log entries replayed
    pub entries_replayed: usize,
    /// Time taken in microseconds
    pub time_us: u64,
    /// Whether recovery was successful
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
}

/// Aggregate recovery statistics.
#[derive(Debug, Clone, Default)]
pub struct RecoveryResult {
    /// Total nodes recovered
    pub nodes_recovered: usize,
    /// Total entries replayed
    pub entries_replayed: usize,
    /// Total time in microseconds
    pub total_time_us: u64,
    /// Number of failures
    pub failures: usize,
    /// Per-node results (if detailed tracking enabled)
    pub node_results: Vec<NodeRecoveryResult>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = PerNodeLogConfig::default();
        assert_eq!(config.max_inline_log_size, 64);
        assert_eq!(config.max_log_size, 4096);
        assert!((config.compaction_threshold - 1.0).abs() < f64::EPSILON);
        assert_eq!(config.max_log_entries, 100);
        assert!(config.background_compaction);
        assert!(config.parallel_recovery);
        assert!(config.track_dirty_nodes);
        assert!(!config.enabled);
    }

    #[test]
    fn test_insert_child_serialization() {
        let entry = NodeLogEntry::InsertChild {
            key: 0x42,
            child_id: 12345678901234567890,
        };

        let serialized = entry.serialize();
        assert_eq!(serialized.len(), 10);
        assert_eq!(serialized[0], log_entry_type::INSERT_CHILD);
        assert_eq!(serialized[1], 0x42);

        let (deserialized, consumed) = NodeLogEntry::deserialize(&serialized).unwrap();
        assert_eq!(consumed, 10);
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn test_remove_child_serialization() {
        let entry = NodeLogEntry::RemoveChild { key: 0xFF };

        let serialized = entry.serialize();
        assert_eq!(serialized.len(), 2);

        let (deserialized, consumed) = NodeLogEntry::deserialize(&serialized).unwrap();
        assert_eq!(consumed, 2);
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn test_set_value_serialization() {
        let entry = NodeLogEntry::SetValue {
            value: vec![1, 2, 3, 4, 5],
        };

        let serialized = entry.serialize();
        assert_eq!(serialized.len(), 8); // 1 + 2 + 5

        let (deserialized, consumed) = NodeLogEntry::deserialize(&serialized).unwrap();
        assert_eq!(consumed, 8);
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn test_clear_value_serialization() {
        let entry = NodeLogEntry::ClearValue;

        let serialized = entry.serialize();
        assert_eq!(serialized.len(), 1);

        let (deserialized, consumed) = NodeLogEntry::deserialize(&serialized).unwrap();
        assert_eq!(consumed, 1);
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn test_set_prefix_serialization() {
        let entry = NodeLogEntry::SetPrefix {
            prefix: b"hello".to_vec(),
        };

        let serialized = entry.serialize();
        assert_eq!(serialized.len(), 7); // 1 + 1 + 5

        let (deserialized, consumed) = NodeLogEntry::deserialize(&serialized).unwrap();
        assert_eq!(consumed, 7);
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn test_inline_log_basic_operations() {
        let mut log = InlineLog::new(64);

        assert!(log.is_empty());
        assert_eq!(log.available_space(), 64);

        let entry1 = NodeLogEntry::InsertChild { key: 0x01, child_id: 100 };
        assert!(log.try_append(&entry1));
        assert_eq!(log.entry_count(), 1);
        assert_eq!(log.used_space(), 10);

        let entry2 = NodeLogEntry::RemoveChild { key: 0x02 };
        assert!(log.try_append(&entry2));
        assert_eq!(log.entry_count(), 2);
        assert_eq!(log.used_space(), 12);
    }

    #[test]
    fn test_inline_log_overflow() {
        let mut log = InlineLog::new(16);

        // First entry fits (10 bytes)
        let entry1 = NodeLogEntry::InsertChild { key: 0x01, child_id: 100 };
        assert!(log.try_append(&entry1));

        // Second entry doesn't fit (10 bytes but only 6 available)
        let entry2 = NodeLogEntry::InsertChild { key: 0x02, child_id: 200 };
        assert!(!log.try_append(&entry2));

        // Small entry fits (2 bytes)
        let entry3 = NodeLogEntry::RemoveChild { key: 0x03 };
        assert!(log.try_append(&entry3));
    }

    #[test]
    fn test_inline_log_iteration() {
        let mut log = InlineLog::new(64);

        log.try_append(&NodeLogEntry::InsertChild { key: 0x01, child_id: 100 });
        log.try_append(&NodeLogEntry::RemoveChild { key: 0x02 });
        log.try_append(&NodeLogEntry::ClearValue);

        let entries: Vec<_> = log.iter().collect();
        assert_eq!(entries.len(), 3);

        assert_eq!(entries[0], NodeLogEntry::InsertChild { key: 0x01, child_id: 100 });
        assert_eq!(entries[1], NodeLogEntry::RemoveChild { key: 0x02 });
        assert_eq!(entries[2], NodeLogEntry::ClearValue);
    }

    #[test]
    fn test_inline_log_clear() {
        let mut log = InlineLog::new(64);

        log.try_append(&NodeLogEntry::InsertChild { key: 0x01, child_id: 100 });
        assert!(!log.is_empty());

        log.clear();
        assert!(log.is_empty());
        assert_eq!(log.entry_count(), 0);
        assert_eq!(log.used_space(), 0);
    }

    #[test]
    fn test_dirty_node_tracker() {
        let stats = Arc::new(PerNodeLogStatsAtomic::new());
        let tracker = DirtyNodeTracker::new(Arc::clone(&stats));

        assert_eq!(tracker.dirty_count(), 0);

        tracker.mark_dirty(1);
        tracker.mark_dirty(2);
        tracker.mark_dirty(3);
        assert_eq!(tracker.dirty_count(), 3);
        assert_eq!(stats.snapshot().dirty_node_count, 3);

        assert!(tracker.is_dirty(1));
        assert!(!tracker.is_dirty(4));

        // Marking same node dirty again shouldn't change count
        tracker.mark_dirty(1);
        assert_eq!(tracker.dirty_count(), 3);

        tracker.mark_clean(2);
        assert_eq!(tracker.dirty_count(), 2);
        assert!(!tracker.is_dirty(2));

        let dirty = tracker.get_dirty_nodes();
        assert_eq!(dirty.len(), 2);
        assert!(dirty.contains(&1));
        assert!(dirty.contains(&3));

        tracker.clear();
        assert_eq!(tracker.dirty_count(), 0);
    }

    #[test]
    fn test_stats_atomic() {
        let stats = PerNodeLogStatsAtomic::new();

        stats.record_entry_written(10, false);
        stats.record_entry_written(100, true);
        stats.record_entry_read();
        stats.record_entry_read();
        stats.record_compaction();
        stats.record_overflow_alloc();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.entries_written, 2);
        assert_eq!(snapshot.entries_read, 2);
        assert_eq!(snapshot.inline_bytes_written, 10);
        assert_eq!(snapshot.overflow_bytes_written, 100);
        assert_eq!(snapshot.compactions, 1);
        assert_eq!(snapshot.overflow_allocations, 1);
    }

    #[test]
    fn test_serialized_size() {
        let entry1 = NodeLogEntry::InsertChild { key: 0, child_id: 0 };
        assert_eq!(entry1.serialized_size(), entry1.serialize().len());

        let entry2 = NodeLogEntry::RemoveChild { key: 0 };
        assert_eq!(entry2.serialized_size(), entry2.serialize().len());

        let entry3 = NodeLogEntry::SetValue { value: vec![1, 2, 3] };
        assert_eq!(entry3.serialized_size(), entry3.serialize().len());

        let entry4 = NodeLogEntry::ClearValue;
        assert_eq!(entry4.serialized_size(), entry4.serialize().len());

        let entry5 = NodeLogEntry::SetPrefix { prefix: vec![1, 2] };
        assert_eq!(entry5.serialized_size(), entry5.serialize().len());
    }

    #[test]
    fn test_deserialize_truncated_data() {
        // Empty data
        assert!(NodeLogEntry::deserialize(&[]).is_none());

        // Truncated InsertChild
        assert!(NodeLogEntry::deserialize(&[log_entry_type::INSERT_CHILD]).is_none());
        assert!(NodeLogEntry::deserialize(&[log_entry_type::INSERT_CHILD, 0x42]).is_none());

        // Truncated SetValue
        assert!(NodeLogEntry::deserialize(&[log_entry_type::SET_VALUE, 0x00]).is_none());
        assert!(NodeLogEntry::deserialize(&[log_entry_type::SET_VALUE, 0x05, 0x00]).is_none());

        // Unknown type
        assert!(NodeLogEntry::deserialize(&[0xFF]).is_none());
    }

    #[test]
    fn test_inline_log_from_data() {
        let data = vec![
            log_entry_type::REMOVE_CHILD, 0x42,
            log_entry_type::CLEAR_VALUE,
        ];

        let log = InlineLog::from_data(data.clone(), 2);
        assert_eq!(log.used_space(), 3);
        assert_eq!(log.entry_count(), 2);

        let entries: Vec<_> = log.iter().collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], NodeLogEntry::RemoveChild { key: 0x42 });
        assert_eq!(entries[1], NodeLogEntry::ClearValue);
    }
}
