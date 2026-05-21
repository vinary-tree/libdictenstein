//! Version Garbage Collection
//!
//! This module provides garbage collection for old trie versions using a
//! reactive, lock-free scheduling pattern. It tracks active readers to ensure
//! versions are only reclaimed when safe.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                      Version GC Architecture                             │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │                                                                          │
//! │   ┌───────────────────────────────────────────────────────────────────┐ │
//! │   │                    VersionGcRegistry                               │ │
//! │   ├───────────────────────────────────────────────────────────────────┤ │
//! │   │                                                                    │ │
//! │   │  ┌─────────────────┐     ┌─────────────────┐                      │ │
//! │   │  │ active_readers  │     │ gc_candidates   │                      │ │
//! │   │  │ (DashMap)       │     │ (RwLock<Vec>)   │                      │ │
//! │   │  │ version→count   │     │                 │                      │ │
//! │   │  └────────┬────────┘     └────────┬────────┘                      │ │
//! │   │           │                        │                               │ │
//! │   │           ▼                        ▼                               │ │
//! │   │  ┌─────────────────────────────────────────┐                      │ │
//! │   │  │          GC Decision Logic              │                      │ │
//! │   │  │  - Check modifications_since_gc         │                      │ │
//! │   │  │  - Find versions with 0 readers         │                      │ │
//! │   │  │  - Respect retention policy             │                      │ │
//! │   │  └─────────────────────────────────────────┘                      │ │
//! │   │                       │                                            │ │
//! │   │                       ▼                                            │ │
//! │   │  ┌─────────────────────────────────────────┐                      │ │
//! │   │  │         GC Worker (Cron Pattern)        │                      │ │
//! │   │  │  - Runs on configurable schedule        │                      │ │
//! │   │  │  - Lock-free operation                  │                      │ │
//! │   │  │  - Graceful shutdown support            │                      │ │
//! │   │  └─────────────────────────────────────────┘                      │ │
//! │   │                                                                    │ │
//! │   └───────────────────────────────────────────────────────────────────┘ │
//! │                                                                          │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```text
//! let registry = VersionGcRegistry::new(GcConfig::default());
//!
//! // Track a reader for a version
//! registry.add_reader(version_id);
//! // ... reader does work ...
//! registry.remove_reader(version_id);
//!
//! // Add versions eligible for GC
//! registry.add_gc_candidate(old_version_id, root_ptr);
//!
//! // Run GC (usually called by background worker)
//! let collected = registry.run_gc_cycle(&mut wal_writer)?;
//! ```

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use dashmap::DashMap;
use parking_lot::RwLock;

use super::error::{PersistentARTrieError, Result};
use super::wal::{WalRecord, WalWriter};

/// Configuration for version garbage collection.
#[derive(Debug, Clone)]
pub struct GcConfig {
    /// How often to run GC cycles (default: 1 second)
    pub gc_interval: Duration,

    /// Minimum number of versions to retain (default: 5)
    pub min_retained_versions: usize,

    /// Maximum number of versions to retain (default: 100)
    pub max_retained_versions: usize,

    /// Minimum time a version must be unreferenced before GC (default: 5 seconds)
    pub grace_period: Duration,

    /// Maximum number of versions to GC in a single cycle (default: 10)
    pub max_gc_per_cycle: usize,

    /// Whether to run GC in background thread
    pub background_gc: bool,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            gc_interval: Duration::from_secs(1),
            min_retained_versions: 5,
            max_retained_versions: 100,
            grace_period: Duration::from_secs(5),
            max_gc_per_cycle: 10,
            background_gc: true,
        }
    }
}

impl GcConfig {
    /// Create a config for testing (faster cycles, shorter grace period).
    pub fn for_testing() -> Self {
        Self {
            gc_interval: Duration::from_millis(100),
            min_retained_versions: 2,
            max_retained_versions: 10,
            grace_period: Duration::from_millis(100),
            max_gc_per_cycle: 5,
            background_gc: false,
        }
    }

    /// Create a config for high-throughput scenarios.
    pub fn high_throughput() -> Self {
        Self {
            gc_interval: Duration::from_millis(500),
            min_retained_versions: 10,
            max_retained_versions: 1000,
            grace_period: Duration::from_secs(2),
            max_gc_per_cycle: 50,
            background_gc: true,
        }
    }
}

/// A version candidate for garbage collection.
#[derive(Debug, Clone)]
pub struct GcCandidate {
    /// The version ID to potentially collect
    pub version_id: u64,
    /// Root pointer for this version (for validation)
    pub root_ptr: u64,
    /// When this version became eligible for GC
    pub eligible_since: Instant,
    /// Number of nodes in this version
    pub node_count: u64,
}

/// Statistics for version garbage collection.
#[derive(Debug, Clone, Default)]
pub struct GcStats {
    /// Total GC cycles run
    pub cycles_run: u64,
    /// Total versions collected
    pub versions_collected: u64,
    /// Total versions skipped (had active readers)
    pub versions_skipped: u64,
    /// Total bytes reclaimed (estimated)
    pub bytes_reclaimed: u64,
    /// Last GC cycle duration
    pub last_cycle_duration: Duration,
    /// Current number of GC candidates
    pub pending_candidates: usize,
    /// Current number of versions with active readers
    pub versions_with_readers: usize,
}

/// Messages for the GC worker thread.
#[derive(Debug)]
enum GcMessage {
    /// Run a GC cycle
    RunCycle,
    /// Shutdown the worker
    Shutdown,
    /// Add a GC candidate
    AddCandidate(GcCandidate),
}

/// Registry for tracking active readers and GC candidates.
///
/// This is the central coordinator for version garbage collection. It tracks
/// which versions have active readers and which are candidates for collection.
#[derive(Debug)]
pub struct VersionGcRegistry {
    /// Active readers per version: version_id -> reader_count
    active_readers: DashMap<u64, u64>,

    /// Versions eligible for GC
    gc_candidates: RwLock<VecDeque<GcCandidate>>,

    /// Configuration
    config: GcConfig,

    /// Number of modifications since last GC (optimization: skip GC if 0)
    modifications_since_gc: AtomicU64,

    /// Whether the registry is shutting down
    terminating: AtomicBool,

    /// Statistics
    stats: RwLock<GcStats>,

    /// Channel to send messages to background worker
    worker_tx: Option<Sender<GcMessage>>,

    /// Handle to background worker thread
    worker_handle: RwLock<Option<JoinHandle<()>>>,
}

impl VersionGcRegistry {
    /// Create a new GC registry.
    pub fn new(config: GcConfig) -> Arc<Self> {
        let (worker_tx, worker_rx) = if config.background_gc {
            let (tx, rx) = bounded::<GcMessage>(1000);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        let registry = Arc::new(Self {
            active_readers: DashMap::new(),
            gc_candidates: RwLock::new(VecDeque::new()),
            config: config.clone(),
            modifications_since_gc: AtomicU64::new(0),
            terminating: AtomicBool::new(false),
            stats: RwLock::new(GcStats::default()),
            worker_tx,
            worker_handle: RwLock::new(None),
        });

        // Start background worker if configured
        if let Some(rx) = worker_rx {
            let registry_clone = Arc::clone(&registry);
            let interval = config.gc_interval;
            let handle = thread::Builder::new()
                .name("version-gc".to_string())
                .spawn(move || {
                    Self::gc_worker_loop(registry_clone, rx, interval);
                })
                .expect("Failed to spawn GC worker thread");

            *registry.worker_handle.write() = Some(handle);
        }

        registry
    }

    /// Background worker loop.
    fn gc_worker_loop(registry: Arc<Self>, rx: Receiver<GcMessage>, interval: Duration) {
        let mut last_cycle = Instant::now();

        loop {
            // Check for messages with timeout
            match rx.recv_timeout(interval) {
                Ok(GcMessage::Shutdown) => {
                    break;
                }
                Ok(GcMessage::RunCycle) => {
                    registry.run_gc_cycle_internal();
                    last_cycle = Instant::now();
                }
                Ok(GcMessage::AddCandidate(candidate)) => {
                    let mut candidates = registry.gc_candidates.write();
                    candidates.push_back(candidate);
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    // Check if it's time for a periodic GC cycle
                    if last_cycle.elapsed() >= interval {
                        registry.run_gc_cycle_internal();
                        last_cycle = Instant::now();
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    break;
                }
            }

            // Check for shutdown
            if registry.terminating.load(Ordering::Acquire) {
                break;
            }
        }
    }

    /// Add a reader for a version.
    ///
    /// This increments the reference count for the version, preventing it
    /// from being garbage collected.
    pub fn add_reader(&self, version_id: u64) {
        self.active_readers
            .entry(version_id)
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }

    /// Remove a reader for a version.
    ///
    /// This decrements the reference count. When the count reaches zero,
    /// the version becomes eligible for garbage collection.
    pub fn remove_reader(&self, version_id: u64) {
        if let Some(mut entry) = self.active_readers.get_mut(&version_id) {
            *entry = entry.saturating_sub(1);
            if *entry == 0 {
                drop(entry);
                self.active_readers.remove(&version_id);
            }
        }
    }

    /// Get the number of active readers for a version.
    pub fn reader_count(&self, version_id: u64) -> u64 {
        self.active_readers
            .get(&version_id)
            .map(|v| *v)
            .unwrap_or(0)
    }

    /// Check if a version has any active readers.
    pub fn has_readers(&self, version_id: u64) -> bool {
        self.reader_count(version_id) > 0
    }

    /// Add a version as a GC candidate.
    ///
    /// This marks the version as potentially eligible for garbage collection.
    /// The version will only be collected after the grace period expires and
    /// it has no active readers.
    pub fn add_gc_candidate(&self, version_id: u64, root_ptr: u64, node_count: u64) {
        let candidate = GcCandidate {
            version_id,
            root_ptr,
            eligible_since: Instant::now(),
            node_count,
        };

        if let Some(ref tx) = self.worker_tx {
            // Send to background worker
            let _ = tx.try_send(GcMessage::AddCandidate(candidate));
        } else {
            // Add directly
            let mut candidates = self.gc_candidates.write();
            candidates.push_back(candidate);
        }
    }

    /// Record a modification (for skip optimization).
    ///
    /// This increments the modification counter, which the GC uses to skip
    /// cycles when no modifications have occurred.
    pub fn record_modification(&self) {
        self.modifications_since_gc.fetch_add(1, Ordering::Release);
    }

    /// Trigger a GC cycle (asynchronous if background worker is running).
    pub fn trigger_gc(&self) {
        if let Some(ref tx) = self.worker_tx {
            let _ = tx.try_send(GcMessage::RunCycle);
        } else {
            self.run_gc_cycle_internal();
        }
    }

    /// Run a GC cycle and return versions that were collected.
    ///
    /// This should be called with a WAL writer to record VersionGc records.
    pub fn run_gc_cycle(&self, wal: &mut WalWriter) -> Result<Vec<u64>> {
        let collected = self.run_gc_cycle_internal();

        if !collected.is_empty() {
            // Write VersionGc record to WAL
            let record = WalRecord::VersionGc {
                version_ids: collected.clone(),
            };
            wal.append(record).map_err(|e| {
                PersistentARTrieError::internal(format!("Failed to write VersionGc: {}", e))
            })?;
        }

        Ok(collected)
    }

    /// Internal GC cycle implementation.
    fn run_gc_cycle_internal(&self) -> Vec<u64> {
        let start = Instant::now();

        // Early exit: no modifications = nothing to GC
        let mods = self.modifications_since_gc.load(Ordering::Acquire);
        if mods == 0 {
            return Vec::new();
        }

        let mut collected = Vec::new();
        let now = Instant::now();
        let grace_period = self.config.grace_period;
        let max_gc = self.config.max_gc_per_cycle;
        let min_retain = self.config.min_retained_versions;

        {
            let mut candidates = self.gc_candidates.write();

            // Count how many candidates we have
            let total_candidates = candidates.len();
            if total_candidates <= min_retain {
                return Vec::new(); // Not enough to GC
            }

            // Calculate how many we can GC while respecting retention
            let max_gc_for_retention = total_candidates - min_retain;

            // Process candidates from oldest to newest
            let mut remaining = VecDeque::new();
            let mut gc_count = 0;

            while let Some(candidate) = candidates.pop_front() {
                // Check if we've hit the per-cycle limit
                if gc_count >= max_gc {
                    remaining.push_back(candidate);
                    continue;
                }

                // Check if we've hit the retention limit
                if gc_count >= max_gc_for_retention {
                    remaining.push_back(candidate);
                    continue;
                }

                // Check grace period
                if now.duration_since(candidate.eligible_since) < grace_period {
                    remaining.push_back(candidate);
                    continue;
                }

                // Check for active readers
                if self.has_readers(candidate.version_id) {
                    remaining.push_back(candidate);
                    let mut stats = self.stats.write();
                    stats.versions_skipped += 1;
                    continue;
                }

                // This version can be collected
                collected.push(candidate.version_id);
                gc_count += 1;

                // Update stats
                {
                    let mut stats = self.stats.write();
                    stats.versions_collected += 1;
                    // Estimate bytes reclaimed (rough: 200 bytes per node)
                    stats.bytes_reclaimed += candidate.node_count * 200;
                }
            }

            // Put remaining candidates back
            *candidates = remaining;
        }

        // Reset modification counter
        self.modifications_since_gc.store(0, Ordering::Release);

        // Update cycle stats
        {
            let mut stats = self.stats.write();
            stats.cycles_run += 1;
            stats.last_cycle_duration = start.elapsed();
            stats.pending_candidates = self.gc_candidates.read().len();
            stats.versions_with_readers = self.active_readers.len();
        }

        collected
    }

    /// Get current GC statistics.
    pub fn stats(&self) -> GcStats {
        self.stats.read().clone()
    }

    /// Shutdown the GC worker.
    pub fn shutdown(&self) {
        self.terminating.store(true, Ordering::Release);

        if let Some(ref tx) = self.worker_tx {
            let _ = tx.send(GcMessage::Shutdown);
        }

        // Wait for worker to finish
        if let Some(handle) = self.worker_handle.write().take() {
            let _ = handle.join();
        }
    }

    /// Get the number of pending GC candidates.
    pub fn pending_count(&self) -> usize {
        self.gc_candidates.read().len()
    }

    /// Get all pending GC candidate version IDs.
    pub fn pending_versions(&self) -> Vec<u64> {
        self.gc_candidates
            .read()
            .iter()
            .map(|c| c.version_id)
            .collect()
    }

    /// Clear all GC candidates (for testing or reset).
    pub fn clear_candidates(&self) {
        self.gc_candidates.write().clear();
    }
}

impl Drop for VersionGcRegistry {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A guard that tracks reader count for a version.
///
/// This automatically increments the reader count on creation and decrements
/// it on drop, ensuring readers are properly tracked even in the presence
/// of panics.
#[derive(Debug)]
pub struct ReaderGuard {
    version_id: u64,
    registry: Arc<VersionGcRegistry>,
}

impl ReaderGuard {
    /// Create a new reader guard for a version.
    pub fn new(version_id: u64, registry: Arc<VersionGcRegistry>) -> Self {
        registry.add_reader(version_id);
        Self {
            version_id,
            registry,
        }
    }

    /// Get the version ID this guard is protecting.
    #[inline]
    pub fn version_id(&self) -> u64 {
        self.version_id
    }
}

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.registry.remove_reader(self.version_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_remove_reader() {
        let config = GcConfig::for_testing();
        let registry = VersionGcRegistry::new(config);

        assert_eq!(registry.reader_count(1), 0);

        registry.add_reader(1);
        assert_eq!(registry.reader_count(1), 1);

        registry.add_reader(1);
        assert_eq!(registry.reader_count(1), 2);

        registry.remove_reader(1);
        assert_eq!(registry.reader_count(1), 1);

        registry.remove_reader(1);
        assert_eq!(registry.reader_count(1), 0);
        assert!(!registry.has_readers(1));
    }

    #[test]
    fn test_reader_guard() {
        let config = GcConfig::for_testing();
        let registry = VersionGcRegistry::new(config);

        assert_eq!(registry.reader_count(1), 0);

        {
            let _guard = ReaderGuard::new(1, Arc::clone(&registry));
            assert_eq!(registry.reader_count(1), 1);
        }

        assert_eq!(registry.reader_count(1), 0);
    }

    #[test]
    fn test_gc_candidate() {
        let config = GcConfig::for_testing();
        let registry = VersionGcRegistry::new(config);

        registry.add_gc_candidate(1, 100, 50);
        registry.add_gc_candidate(2, 200, 75);

        assert_eq!(registry.pending_count(), 2);
        assert!(registry.pending_versions().contains(&1));
        assert!(registry.pending_versions().contains(&2));
    }

    #[test]
    fn test_gc_cycle_respects_readers() {
        let config = GcConfig {
            grace_period: Duration::from_millis(0), // No grace period for test
            min_retained_versions: 0,
            ..GcConfig::for_testing()
        };
        let registry = VersionGcRegistry::new(config);

        // Add candidate with a reader
        registry.add_gc_candidate(1, 100, 50);
        registry.add_reader(1);
        registry.record_modification();

        // GC should skip version 1 (has reader)
        let collected = registry.run_gc_cycle_internal();
        assert!(collected.is_empty());
        assert_eq!(registry.stats().versions_skipped, 1);

        // Remove reader and try again
        registry.remove_reader(1);
        registry.record_modification();
        let collected = registry.run_gc_cycle_internal();
        assert_eq!(collected, vec![1]);
    }

    #[test]
    fn test_gc_cycle_respects_grace_period() {
        let config = GcConfig {
            grace_period: Duration::from_secs(10), // Long grace period
            min_retained_versions: 0,
            ..GcConfig::for_testing()
        };
        let registry = VersionGcRegistry::new(config);

        registry.add_gc_candidate(1, 100, 50);
        registry.record_modification();

        // GC should skip due to grace period
        let collected = registry.run_gc_cycle_internal();
        assert!(collected.is_empty());
    }

    #[test]
    fn test_gc_cycle_respects_retention() {
        let config = GcConfig {
            grace_period: Duration::from_millis(0),
            min_retained_versions: 3,
            ..GcConfig::for_testing()
        };
        let registry = VersionGcRegistry::new(config);

        // Add 4 candidates
        for i in 1..=4 {
            registry.add_gc_candidate(i, i * 100, i * 50);
        }
        registry.record_modification();

        // Only 1 should be collected (4 - 3 = 1)
        let collected = registry.run_gc_cycle_internal();
        assert_eq!(collected.len(), 1);
        assert_eq!(registry.pending_count(), 3);
    }

    #[test]
    fn test_gc_stats() {
        let config = GcConfig {
            grace_period: Duration::from_millis(0),
            min_retained_versions: 0,
            ..GcConfig::for_testing()
        };
        let registry = VersionGcRegistry::new(config);

        registry.add_gc_candidate(1, 100, 50);
        registry.record_modification();
        registry.run_gc_cycle_internal();

        let stats = registry.stats();
        assert_eq!(stats.cycles_run, 1);
        assert_eq!(stats.versions_collected, 1);
        assert!(stats.bytes_reclaimed > 0);
    }

    #[test]
    fn test_no_gc_without_modifications() {
        let config = GcConfig::for_testing();
        let registry = VersionGcRegistry::new(config);

        registry.add_gc_candidate(1, 100, 50);
        // Don't call record_modification()

        let collected = registry.run_gc_cycle_internal();
        assert!(collected.is_empty());
    }

    #[test]
    fn test_shutdown() {
        let config = GcConfig {
            background_gc: true,
            gc_interval: Duration::from_millis(10),
            ..GcConfig::for_testing()
        };
        let registry = VersionGcRegistry::new(config);

        // Give the worker a moment to start
        thread::sleep(Duration::from_millis(50));

        // Shutdown should not hang
        registry.shutdown();
    }
}
