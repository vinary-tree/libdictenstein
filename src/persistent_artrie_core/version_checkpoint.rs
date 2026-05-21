//! Version-Based Checkpoint Manager
//!
//! This module provides O(1) snapshot checkpointing using immutable version tracking.
//! Instead of serializing the entire trie on each checkpoint, we track versions and
//! only persist new/modified nodes.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                    VersionCheckpointManager                              │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │                                                                          │
//! │   ┌──────────────────┐     ┌──────────────────┐                         │
//! │   │ current_version  │     │ durable_version  │                         │
//! │   │   (AtomicU64)    │     │   (AtomicU64)    │                         │
//! │   └────────┬─────────┘     └────────┬─────────┘                         │
//! │            │                         │                                   │
//! │            ▼                         ▼                                   │
//! │   ┌────────────────────────────────────────────┐                        │
//! │   │           active_versions: DashMap          │                        │
//! │   │   version_id → VersionSnapshot              │                        │
//! │   └────────────────────────────────────────────┘                        │
//! │                         │                                                │
//! │                         ▼                                                │
//! │   ┌────────────────────────────────────────────┐                        │
//! │   │        pending_versions: Mutex<VecDeque>    │                        │
//! │   │   Versions awaiting durability confirmation │                        │
//! │   └────────────────────────────────────────────┘                        │
//! │                                                                          │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```text
//! let mut manager = VersionCheckpointManager::new();
//!
//! // Create a new version snapshot
//! let version_id = manager.create_version(root_ptr, node_count);
//!
//! // Get O(1) snapshot of current version
//! let snapshot = manager.snapshot();
//!
//! // Mark version as durable after WAL sync
//! manager.mark_durable(version_id, checksum);
//!
//! // Recover to last durable version
//! let recovered = manager.recover()?;
//! ```

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use parking_lot::Mutex;

use super::error::{PersistentARTrieError, Result};
use super::wal::{WalRecord, WalWriter};

/// A snapshot of a trie version at a point in time.
#[derive(Debug, Clone)]
pub struct VersionSnapshot {
    /// Unique version identifier (monotonically increasing)
    pub version_id: u64,
    /// Root pointer for this version's trie structure
    pub root_ptr: u64,
    /// Number of nodes in this version
    pub node_count: u64,
    /// Unix timestamp when this version was created
    pub timestamp: u64,
    /// Whether this version has been marked as durable
    pub is_durable: bool,
    /// CRC32 checksum (set when marked durable)
    pub checksum: Option<u32>,
}

impl VersionSnapshot {
    /// Create a new version snapshot.
    pub fn new(version_id: u64, root_ptr: u64, node_count: u64) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        Self {
            version_id,
            root_ptr,
            node_count,
            timestamp,
            is_durable: false,
            checksum: None,
        }
    }

    /// Mark this snapshot as durable with a checksum.
    pub fn mark_durable(&mut self, checksum: u32) {
        self.is_durable = true;
        self.checksum = Some(checksum);
    }
}

/// A pending version awaiting durability confirmation.
#[derive(Debug)]
#[allow(dead_code)] // Fields used for future checkpoint metadata
struct PendingVersion {
    version_id: u64,
    root_ptr: u64,
    node_count: u64,
    timestamp: u64,
}

/// Manager for version-based checkpoints.
///
/// This manager tracks trie versions for O(1) snapshot operations and
/// point-in-time recovery capabilities.
#[derive(Debug)]
pub struct VersionCheckpointManager {
    /// Current version being written to
    current_version: AtomicU64,

    /// Last version that has been marked as durable
    durable_version: AtomicU64,

    /// Active versions that may have readers
    /// Key: version_id, Value: VersionSnapshot
    active_versions: DashMap<u64, VersionSnapshot>,

    /// Versions that have been created but not yet marked durable
    pending_versions: Mutex<VecDeque<PendingVersion>>,

    /// Maximum number of versions to retain
    max_retained_versions: usize,

    /// Statistics: total versions created
    versions_created: AtomicU64,

    /// Statistics: total versions marked durable
    versions_durabled: AtomicU64,

    /// Statistics: total versions garbage collected
    versions_gc: AtomicU64,
}

impl VersionCheckpointManager {
    /// Create a new version checkpoint manager.
    pub fn new() -> Self {
        Self::with_retention(10) // Default: keep 10 versions
    }

    /// Create a new manager with a specific retention count.
    pub fn with_retention(max_retained_versions: usize) -> Self {
        Self {
            current_version: AtomicU64::new(0),
            durable_version: AtomicU64::new(0),
            active_versions: DashMap::new(),
            pending_versions: Mutex::new(VecDeque::new()),
            max_retained_versions,
            versions_created: AtomicU64::new(0),
            versions_durabled: AtomicU64::new(0),
            versions_gc: AtomicU64::new(0),
        }
    }

    /// Get the current version ID.
    #[inline]
    pub fn current_version_id(&self) -> u64 {
        self.current_version.load(Ordering::Acquire)
    }

    /// Get the last durable version ID.
    #[inline]
    pub fn durable_version_id(&self) -> u64 {
        self.durable_version.load(Ordering::Acquire)
    }

    /// Create a new version snapshot.
    ///
    /// This allocates a new version ID and records the root pointer and node count.
    /// The version is not durable until `mark_durable()` is called after WAL sync.
    ///
    /// # Arguments
    ///
    /// * `root_ptr` - The root pointer for this version's trie structure
    /// * `node_count` - Number of nodes in this version
    ///
    /// # Returns
    ///
    /// The new version ID.
    pub fn create_version(&self, root_ptr: u64, node_count: u64) -> u64 {
        let version_id = self.current_version.fetch_add(1, Ordering::AcqRel) + 1;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let snapshot = VersionSnapshot::new(version_id, root_ptr, node_count);
        self.active_versions.insert(version_id, snapshot);

        // Track as pending until marked durable
        {
            let mut pending = self.pending_versions.lock();
            pending.push_back(PendingVersion {
                version_id,
                root_ptr,
                node_count,
                timestamp,
            });
        }

        self.versions_created.fetch_add(1, Ordering::Relaxed);
        version_id
    }

    /// O(1) snapshot - return the current version snapshot.
    ///
    /// This is the key advantage of version-based checkpoints: snapshots are
    /// instantaneous and don't require copying any data.
    pub fn snapshot(&self) -> Option<VersionSnapshot> {
        let version_id = self.current_version.load(Ordering::Acquire);
        if version_id == 0 {
            return None;
        }
        self.active_versions.get(&version_id).map(|v| v.clone())
    }

    /// Get a specific version snapshot by ID.
    pub fn get_version(&self, version_id: u64) -> Option<VersionSnapshot> {
        self.active_versions.get(&version_id).map(|v| v.clone())
    }

    /// Get the last durable version snapshot.
    pub fn durable_snapshot(&self) -> Option<VersionSnapshot> {
        let version_id = self.durable_version.load(Ordering::Acquire);
        if version_id == 0 {
            return None;
        }
        self.active_versions.get(&version_id).map(|v| v.clone())
    }

    /// Mark a version as durable after WAL sync.
    ///
    /// This should be called after:
    /// 1. Writing VersionUpdate record to WAL
    /// 2. Syncing WAL to disk
    ///
    /// # Arguments
    ///
    /// * `version_id` - The version to mark as durable
    /// * `checksum` - CRC32 checksum for validation during recovery
    pub fn mark_durable(&self, version_id: u64, checksum: u32) {
        if let Some(mut snapshot) = self.active_versions.get_mut(&version_id) {
            snapshot.mark_durable(checksum);
        }

        // Update durable version (only if this is the latest)
        let current_durable = self.durable_version.load(Ordering::Acquire);
        if version_id > current_durable {
            self.durable_version.store(version_id, Ordering::Release);
        }

        // Remove from pending
        {
            let mut pending = self.pending_versions.lock();
            pending.retain(|p| p.version_id != version_id);
        }

        self.versions_durabled.fetch_add(1, Ordering::Relaxed);

        // Trigger GC of old versions
        self.gc_old_versions();
    }

    /// Write version records to WAL.
    ///
    /// This writes both VersionUpdate and VersionDurable records.
    pub fn write_version_to_wal(
        &self,
        wal: &mut WalWriter,
        version_id: u64,
        root_ptr: u64,
        node_count: u64,
        checksum: u32,
    ) -> Result<()> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Write VersionUpdate record
        let update_record = WalRecord::VersionUpdate {
            version_id,
            root_ptr,
            node_count,
            timestamp,
        };
        wal.append(update_record).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to write VersionUpdate: {}", e))
        })?;

        // Sync WAL
        wal.sync()
            .map_err(|e| PersistentARTrieError::internal(format!("Failed to sync WAL: {}", e)))?;

        // Write VersionDurable record
        let durable_record = WalRecord::VersionDurable {
            version_id,
            checksum,
        };
        wal.append(durable_record).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to write VersionDurable: {}", e))
        })?;

        wal.sync()
            .map_err(|e| PersistentARTrieError::internal(format!("Failed to sync WAL: {}", e)))?;

        Ok(())
    }

    /// Recover to the last durable version.
    ///
    /// This returns the snapshot of the last version that was successfully
    /// marked as durable, which is safe for recovery.
    pub fn recover(&self) -> Result<Option<VersionSnapshot>> {
        let durable_id = self.durable_version.load(Ordering::Acquire);
        if durable_id == 0 {
            return Ok(None);
        }

        self.active_versions
            .get(&durable_id)
            .map(|v| v.clone())
            .ok_or_else(|| {
                PersistentARTrieError::internal(format!(
                    "Durable version {} not found in active versions",
                    durable_id
                ))
            })
            .map(Some)
    }

    /// Restore from a WAL recovery report.
    ///
    /// This processes VersionUpdate and VersionDurable records from WAL
    /// to restore the version state.
    pub fn restore_from_wal_records(&self, records: &[(u64, WalRecord)]) {
        for (_lsn, record) in records {
            match record {
                WalRecord::VersionUpdate {
                    version_id,
                    root_ptr,
                    node_count,
                    timestamp,
                } => {
                    let snapshot = VersionSnapshot {
                        version_id: *version_id,
                        root_ptr: *root_ptr,
                        node_count: *node_count,
                        timestamp: *timestamp,
                        is_durable: false,
                        checksum: None,
                    };
                    self.active_versions.insert(*version_id, snapshot);

                    // Update current version
                    let current = self.current_version.load(Ordering::Acquire);
                    if *version_id > current {
                        self.current_version.store(*version_id, Ordering::Release);
                    }
                }
                WalRecord::VersionDurable {
                    version_id,
                    checksum,
                } => {
                    if let Some(mut snapshot) = self.active_versions.get_mut(version_id) {
                        snapshot.mark_durable(*checksum);
                    }

                    // Update durable version
                    let current_durable = self.durable_version.load(Ordering::Acquire);
                    if *version_id > current_durable {
                        self.durable_version.store(*version_id, Ordering::Release);
                    }
                }
                WalRecord::VersionGc { version_ids } => {
                    // Remove GC'd versions from tracking
                    for vid in version_ids {
                        self.active_versions.remove(vid);
                    }
                }
                _ => {} // Ignore other record types
            }
        }
    }

    /// Garbage collect old versions.
    ///
    /// This removes versions older than the retention window that have no
    /// active readers.
    fn gc_old_versions(&self) {
        let durable = self.durable_version.load(Ordering::Acquire);
        if durable <= self.max_retained_versions as u64 {
            return; // Not enough versions yet
        }

        // Keep `max_retained_versions` versions: from (durable - retention + 1) to durable
        // E.g., retention=2, durable=5 → cutoff=4, keep versions 4,5
        let cutoff = durable.saturating_sub(self.max_retained_versions as u64 - 1);
        let mut gc_count = 0;

        // Remove versions older than cutoff
        self.active_versions.retain(|&version_id, _| {
            if version_id < cutoff {
                gc_count += 1;
                false
            } else {
                true
            }
        });

        if gc_count > 0 {
            self.versions_gc.fetch_add(gc_count, Ordering::Relaxed);
        }
    }

    /// Get versions that should be garbage collected.
    ///
    /// This returns version IDs that are older than the retention window
    /// and have no active readers.
    pub fn get_gc_candidates(&self) -> Vec<u64> {
        let durable = self.durable_version.load(Ordering::Acquire);
        if durable <= self.max_retained_versions as u64 {
            return Vec::new();
        }

        // Keep `max_retained_versions` versions: from (durable - retention + 1) to durable
        let cutoff = durable.saturating_sub(self.max_retained_versions as u64 - 1);
        self.active_versions
            .iter()
            .filter(|entry| *entry.key() < cutoff && entry.is_durable)
            .map(|entry| *entry.key())
            .collect()
    }

    /// Write a VersionGc record to WAL.
    pub fn write_gc_to_wal(&self, wal: &mut WalWriter, version_ids: Vec<u64>) -> Result<()> {
        if version_ids.is_empty() {
            return Ok(());
        }

        let gc_record = WalRecord::VersionGc { version_ids };
        wal.append(gc_record).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to write VersionGc: {}", e))
        })?;

        Ok(())
    }

    /// Get statistics about version management.
    pub fn stats(&self) -> VersionCheckpointStats {
        VersionCheckpointStats {
            current_version: self.current_version.load(Ordering::Relaxed),
            durable_version: self.durable_version.load(Ordering::Relaxed),
            active_count: self.active_versions.len(),
            pending_count: self.pending_versions.lock().len(),
            versions_created: self.versions_created.load(Ordering::Relaxed),
            versions_durabled: self.versions_durabled.load(Ordering::Relaxed),
            versions_gc: self.versions_gc.load(Ordering::Relaxed),
        }
    }

    /// List all active version IDs.
    pub fn active_version_ids(&self) -> Vec<u64> {
        self.active_versions.iter().map(|e| *e.key()).collect()
    }

    /// Check if a version is durable.
    pub fn is_version_durable(&self, version_id: u64) -> bool {
        self.active_versions
            .get(&version_id)
            .map(|v| v.is_durable)
            .unwrap_or(false)
    }
}

impl Default for VersionCheckpointManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics for version checkpoint management.
#[derive(Debug, Clone)]
pub struct VersionCheckpointStats {
    /// Current version ID
    pub current_version: u64,
    /// Last durable version ID
    pub durable_version: u64,
    /// Number of active versions being tracked
    pub active_count: usize,
    /// Number of versions pending durability
    pub pending_count: usize,
    /// Total versions created
    pub versions_created: u64,
    /// Total versions marked as durable
    pub versions_durabled: u64,
    /// Total versions garbage collected
    pub versions_gc: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_version() {
        let manager = VersionCheckpointManager::new();

        let v1 = manager.create_version(100, 50);
        assert_eq!(v1, 1);
        assert_eq!(manager.current_version_id(), 1);

        let v2 = manager.create_version(200, 75);
        assert_eq!(v2, 2);
        assert_eq!(manager.current_version_id(), 2);
    }

    #[test]
    fn test_snapshot() {
        let manager = VersionCheckpointManager::new();

        // No version yet
        assert!(manager.snapshot().is_none());

        manager.create_version(100, 50);
        let snapshot = manager.snapshot().expect("should have snapshot");
        assert_eq!(snapshot.version_id, 1);
        assert_eq!(snapshot.root_ptr, 100);
        assert_eq!(snapshot.node_count, 50);
        assert!(!snapshot.is_durable);
    }

    #[test]
    fn test_mark_durable() {
        let manager = VersionCheckpointManager::new();

        let v1 = manager.create_version(100, 50);
        assert_eq!(manager.durable_version_id(), 0);

        manager.mark_durable(v1, 0xDEADBEEF);
        assert_eq!(manager.durable_version_id(), 1);

        let snapshot = manager.get_version(v1).expect("should exist");
        assert!(snapshot.is_durable);
        assert_eq!(snapshot.checksum, Some(0xDEADBEEF));
    }

    #[test]
    fn test_durable_snapshot() {
        let manager = VersionCheckpointManager::new();

        // No durable version yet
        assert!(manager.durable_snapshot().is_none());

        let v1 = manager.create_version(100, 50);
        manager.mark_durable(v1, 0x12345678);

        let snapshot = manager.durable_snapshot().expect("should have durable");
        assert_eq!(snapshot.version_id, 1);
        assert!(snapshot.is_durable);
    }

    #[test]
    fn test_multiple_versions() {
        let manager = VersionCheckpointManager::new();

        let v1 = manager.create_version(100, 50);
        let v2 = manager.create_version(200, 75);
        let v3 = manager.create_version(300, 100);

        manager.mark_durable(v1, 0x1111);
        manager.mark_durable(v2, 0x2222);
        // v3 not durable

        assert_eq!(manager.durable_version_id(), 2);
        assert!(manager.is_version_durable(v1));
        assert!(manager.is_version_durable(v2));
        assert!(!manager.is_version_durable(v3));
    }

    #[test]
    fn test_gc_old_versions() {
        let manager = VersionCheckpointManager::with_retention(2);

        // Create and mark durable 5 versions
        for i in 1..=5 {
            let v = manager.create_version(i * 100, i * 50);
            manager.mark_durable(v, i as u32);
        }

        // With retention=2, versions 1, 2, 3 should be GC'd
        // Only versions 4, 5 should remain
        let active = manager.active_version_ids();
        assert!(!active.contains(&1));
        assert!(!active.contains(&2));
        assert!(!active.contains(&3));
        assert!(active.contains(&4));
        assert!(active.contains(&5));
    }

    #[test]
    fn test_stats() {
        let manager = VersionCheckpointManager::new();

        manager.create_version(100, 50);
        manager.create_version(200, 75);
        manager.mark_durable(1, 0x1234);

        let stats = manager.stats();
        assert_eq!(stats.current_version, 2);
        assert_eq!(stats.durable_version, 1);
        assert_eq!(stats.active_count, 2);
        assert_eq!(stats.versions_created, 2);
        assert_eq!(stats.versions_durabled, 1);
    }

    #[test]
    fn test_restore_from_wal_records() {
        let manager = VersionCheckpointManager::new();

        let records: Vec<(u64, WalRecord)> = vec![
            (
                1,
                WalRecord::VersionUpdate {
                    version_id: 1,
                    root_ptr: 100,
                    node_count: 50,
                    timestamp: 1234567890,
                },
            ),
            (
                2,
                WalRecord::VersionDurable {
                    version_id: 1,
                    checksum: 0xABCD,
                },
            ),
            (
                3,
                WalRecord::VersionUpdate {
                    version_id: 2,
                    root_ptr: 200,
                    node_count: 75,
                    timestamp: 1234567891,
                },
            ),
        ];

        manager.restore_from_wal_records(&records);

        assert_eq!(manager.current_version_id(), 2);
        assert_eq!(manager.durable_version_id(), 1);
        assert!(manager.is_version_durable(1));
        assert!(!manager.is_version_durable(2));
    }

    #[test]
    fn test_recover() {
        let manager = VersionCheckpointManager::new();

        let v1 = manager.create_version(100, 50);
        manager.mark_durable(v1, 0x1234);

        let v2 = manager.create_version(200, 75);
        // v2 not durable - simulates crash before sync

        let recovered = manager
            .recover()
            .expect("should recover")
            .expect("should have version");
        assert_eq!(recovered.version_id, 1); // Should recover to v1, not v2
        assert!(recovered.is_durable);
    }
}
