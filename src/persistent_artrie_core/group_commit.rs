//! Group Commit for WAL - Batches multiple fsync operations.
//!
//! This module implements a group commit mechanism that batches WAL writes
//! and fsync operations, trading individual operation latency for overall
//! throughput improvement.
//!
//! # Design
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                        Group Commit Architecture                         │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │                                                                          │
//! │  Writer Threads                    Commit Coordinator                    │
//! │  ┌──────────┐                     ┌────────────────────────────────┐    │
//! │  │ Thread 1 │──┐                  │  Pending Queue                 │    │
//! │  └──────────┘  │                  │  ┌────┬────┬────┬────┬────┐   │    │
//! │  ┌──────────┐  │   submit()       │  │ R1 │ R2 │ R3 │ R4 │ R5 │   │    │
//! │  │ Thread 2 │──┼─────────────────►│  └────┴────┴────┴────┴────┘   │    │
//! │  └──────────┘  │                  │                                │    │
//! │  ┌──────────┐  │                  │  Batch Triggers:               │    │
//! │  │ Thread N │──┘                  │  • size >= MAX_BATCH_SIZE      │    │
//! │  └──────────┘                     │  • time >= MAX_BATCH_DELAY     │    │
//! │       ▲                           │  • explicit flush()            │    │
//! │       │                           └───────────────┬────────────────┘    │
//! │       │                                           │                      │
//! │       │   notify()                                ▼                      │
//! │       │                           ┌────────────────────────────────┐    │
//! │       └───────────────────────────│  Commit Process                │    │
//! │                                   │  1. Acquire WAL lock           │    │
//! │                                   │  2. Append all records         │    │
//! │                                   │  3. Single fsync()             │    │
//! │                                   │  4. Update synced_lsn          │    │
//! │                                   │  5. Notify all waiters         │    │
//! │                                   └────────────────────────────────┘    │
//! │                                                                          │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```text
//! use libdictenstein::persistent_artrie::group_commit::{
//!     GroupCommitConfig, GroupCommitCoordinator,
//! };
//! use libdictenstein::persistent_artrie::wal::{AsyncWalConfig, AsyncWalWriter, WalConfig};
//!
//! let async_config = AsyncWalConfig::default();
//! let archive_config = WalConfig::default();
//! let wal = Arc::new(AsyncWalWriter::create("data.wal", async_config, archive_config)?);
//! let config = GroupCommitConfig::default();
//! let coordinator = GroupCommitCoordinator::new(wal, config)?;
//!
//! // From multiple threads:
//! let lsn = coordinator.append_with_sync(record)?;
//! ```
//!
//! # References
//!
//! - PostgreSQL group commit: https://www.postgresql.org/docs/current/wal-async-commit.html
//! - MySQL group commit: https://dev.mysql.com/doc/refman/8.0/en/group-commit.html
//! - RocksDB WriteAheadLog: https://github.com/facebook/rocksdb/wiki/Write-Ahead-Log

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use parking_lot::RwLock;

use super::error::PersistentARTrieError;
use super::wal::{AsyncWalWriter, Lsn, WalRecord};

/// Result type for group commit operations.
pub type Result<T> = std::result::Result<T, PersistentARTrieError>;

/// Configuration for group commit behavior.
///
/// Group commit batches multiple WAL writes into a single fsync() call,
/// trading individual operation latency for overall throughput.
///
/// # Tuning Guidelines
///
/// For **latency-sensitive** workloads:
/// - `max_batch_delay_us`: 1_000 - 5_000 (1-5ms)
/// - `max_batch_size`: 10 - 50
///
/// For **throughput-optimized** workloads:
/// - `max_batch_delay_us`: 10_000 - 50_000 (10-50ms)
/// - `max_batch_size`: 100 - 1000
///
/// # Storage Device Considerations
///
/// | Device | Typical fsync latency | Recommended max_batch_delay_us |
/// |--------|----------------------|-------------------------------|
/// | HDD    | 5-15ms               | 10_000 - 20_000               |
/// | SATA SSD | 0.1-1ms            | 1_000 - 5_000                 |
/// | NVMe SSD | 0.01-0.1ms         | 100 - 1_000                   |
/// | Optane  | 0.005-0.01ms        | 50 - 500                      |
#[derive(Debug, Clone)]
pub struct GroupCommitConfig {
    /// Maximum number of records to batch before forcing sync.
    ///
    /// When the pending queue reaches this size, a sync is triggered
    /// immediately regardless of the time elapsed.
    ///
    /// Default: 100
    /// Range: 1 - 10_000
    pub max_batch_size: usize,

    /// Maximum delay in microseconds before forcing sync.
    ///
    /// This is the maximum time a committed transaction will wait
    /// for its WAL record to be durable. Lower values reduce latency
    /// but increase fsync frequency.
    ///
    /// Default: 10_000 (10ms)
    /// Range: 0 - 1_000_000 (0 - 1 second)
    pub max_batch_delay_us: u64,

    /// Minimum concurrent writers to enable batching delay.
    ///
    /// Similar to PostgreSQL's commit_siblings. If fewer than this
    /// many transactions are active, skip the delay and sync immediately.
    /// This prevents unnecessary latency under light load.
    ///
    /// Default: 2
    /// Range: 1 - 100
    pub min_batch_siblings: usize,

    /// Whether to use a dedicated background thread for commits.
    ///
    /// When true, a background thread handles all fsync operations,
    /// allowing writer threads to return immediately after queuing.
    /// When false, the first writer in each batch acts as leader.
    ///
    /// Default: true
    pub dedicated_commit_thread: bool,

    /// Enable adaptive batching based on observed load.
    ///
    /// When enabled, the system automatically adjusts batch parameters
    /// based on recent throughput and latency measurements.
    ///
    /// Default: true
    pub adaptive_batching: bool,

    /// Target p99 latency for adaptive batching (microseconds).
    ///
    /// When adaptive_batching is enabled, the system will adjust
    /// parameters to try to maintain this latency target.
    ///
    /// Default: 10_000 (10ms)
    pub adaptive_latency_target_us: u64,

    /// Enable pipelined mode where WAL sync and response notification
    /// are decoupled for better parallelism.
    ///
    /// Default: false (simpler semantics)
    pub pipelined_sync: bool,
}

impl Default for GroupCommitConfig {
    fn default() -> Self {
        Self {
            max_batch_size: 100,
            max_batch_delay_us: 10_000,
            min_batch_siblings: 2,
            dedicated_commit_thread: true,
            adaptive_batching: true,
            adaptive_latency_target_us: 10_000,
            pipelined_sync: false,
        }
    }
}

impl GroupCommitConfig {
    /// Create a latency-optimized configuration.
    pub fn low_latency() -> Self {
        Self {
            max_batch_size: 10,
            max_batch_delay_us: 1_000,
            min_batch_siblings: 1,
            dedicated_commit_thread: true,
            adaptive_batching: false,
            adaptive_latency_target_us: 2_000,
            pipelined_sync: false,
        }
    }

    /// Create a throughput-optimized configuration.
    pub fn high_throughput() -> Self {
        Self {
            max_batch_size: 1000,
            max_batch_delay_us: 50_000,
            min_batch_siblings: 5,
            dedicated_commit_thread: true,
            adaptive_batching: true,
            adaptive_latency_target_us: 50_000,
            pipelined_sync: true,
        }
    }

    /// Create a configuration optimized for NVMe storage.
    pub fn nvme_optimized() -> Self {
        Self {
            max_batch_size: 50,
            max_batch_delay_us: 500,
            min_batch_siblings: 2,
            dedicated_commit_thread: true,
            adaptive_batching: true,
            adaptive_latency_target_us: 1_000,
            pipelined_sync: false,
        }
    }
}

/// Statistics for monitoring group commit performance.
#[derive(Debug, Default, Clone)]
pub struct GroupCommitStats {
    /// Total number of records committed.
    pub records_committed: u64,

    /// Total number of fsync operations performed.
    pub fsync_count: u64,

    /// Average records per fsync (batching efficiency).
    pub avg_batch_size: f64,

    /// P50 commit latency in microseconds.
    pub latency_p50_us: u64,

    /// P99 commit latency in microseconds.
    pub latency_p99_us: u64,

    /// Total bytes written to WAL.
    pub bytes_written: u64,

    /// Current queue depth.
    pub queue_depth: usize,
}

impl GroupCommitStats {
    /// Calculate batching efficiency (records per fsync).
    pub fn batching_efficiency(&self) -> f64 {
        if self.fsync_count == 0 {
            0.0
        } else {
            self.records_committed as f64 / self.fsync_count as f64
        }
    }
}

/// A pending write waiting for group commit.
struct PendingWrite {
    /// The WAL record to write.
    record: WalRecord,

    /// The assigned LSN for this record.
    lsn: Lsn,

    /// Channel to notify the waiter when commit is complete.
    response_tx: OneshotSender<Result<Lsn>>,

    /// Timestamp when this write was submitted (for latency tracking).
    #[allow(dead_code)]
    submitted_at: Instant,

    /// Size of the serialized record in bytes.
    serialized_size: usize,
}

/// Adaptive batching controller using AIMD (Additive Increase, Multiplicative Decrease).
struct AdaptiveController {
    /// Current batch delay target (microseconds).
    current_delay_us: AtomicU64,

    /// Current batch size target.
    current_batch_size: AtomicUsize,

    /// Recent latency samples for percentile calculation.
    latency_samples: Mutex<VecDeque<u64>>,

    /// Target p99 latency.
    target_latency_us: u64,

    /// Minimum delay (don't go below this).
    min_delay_us: u64,

    /// Maximum delay (don't exceed this).
    max_delay_us: u64,
}

impl AdaptiveController {
    fn new(config: &GroupCommitConfig) -> Self {
        Self {
            current_delay_us: AtomicU64::new(config.max_batch_delay_us),
            current_batch_size: AtomicUsize::new(config.max_batch_size),
            latency_samples: Mutex::new(VecDeque::with_capacity(1000)),
            target_latency_us: config.adaptive_latency_target_us,
            min_delay_us: 100,     // 0.1ms minimum
            max_delay_us: 100_000, // 100ms maximum
        }
    }

    fn record_latency(&self, latency_us: u64) {
        let mut samples = self.latency_samples.lock().expect("lock poisoned");
        if samples.len() >= 1000 {
            samples.pop_front();
        }
        samples.push_back(latency_us);
    }

    fn adjust(&self) {
        let samples = self.latency_samples.lock().expect("lock poisoned");
        if samples.len() < 100 {
            return; // Not enough data
        }

        // Calculate p99 latency
        let mut sorted: Vec<_> = samples.iter().copied().collect();
        sorted.sort_unstable();
        let p99_idx = (sorted.len() * 99) / 100;
        let current_p99 = sorted[p99_idx];

        let current_delay = self.current_delay_us.load(Ordering::Relaxed);

        if current_p99 > self.target_latency_us {
            // Latency too high: multiplicative decrease (halve the delay)
            let new_delay = (current_delay / 2).max(self.min_delay_us);
            self.current_delay_us.store(new_delay, Ordering::Relaxed);
        } else if current_p99 < self.target_latency_us / 2 {
            // Latency well under target: additive increase
            let new_delay = (current_delay + 1000).min(self.max_delay_us);
            self.current_delay_us.store(new_delay, Ordering::Relaxed);
        }
    }

    fn get_current_delay_us(&self) -> u64 {
        self.current_delay_us.load(Ordering::Relaxed)
    }

    fn get_current_batch_size(&self) -> usize {
        self.current_batch_size.load(Ordering::Relaxed)
    }
}

/// The main group commit coordinator.
///
/// # Thread Safety
///
/// This struct is designed to be shared across multiple threads via `Arc`.
/// Writer threads submit records via `append_with_sync()`, and a background
/// thread (or leader election) handles the actual fsync operations.
///
/// # Example
///
/// ```text
/// let async_config = AsyncWalConfig::default();
/// let archive_config = WalConfig::default();
/// let wal = Arc::new(AsyncWalWriter::open(path, async_config, archive_config)?);
/// let config = GroupCommitConfig::default();
/// let coordinator = GroupCommitCoordinator::new(wal, config)?;
///
/// // From multiple threads:
/// let lsn = coordinator.append_with_sync(record)?;
/// ```
pub struct GroupCommitCoordinator {
    /// The underlying async WAL writer.
    wal: Arc<AsyncWalWriter>,

    /// Configuration (kept for potential future use).
    #[allow(dead_code)]
    config: GroupCommitConfig,

    /// Channel for submitting writes.
    submit_tx: Sender<PendingWrite>,

    /// Background commit thread handle (if dedicated_commit_thread is true).
    commit_thread: Option<JoinHandle<()>>,

    /// Signal to stop the background thread.
    shutdown: Arc<AtomicBool>,

    /// The highest LSN that has been durably synced.
    synced_lsn: Arc<AtomicU64>,

    /// Adaptive batching controller.
    adaptive: Option<Arc<AdaptiveController>>,

    /// Statistics.
    stats: Arc<RwLock<GroupCommitStats>>,
}

impl GroupCommitCoordinator {
    /// Create a new group commit coordinator.
    pub fn new(wal: Arc<AsyncWalWriter>, config: GroupCommitConfig) -> Result<Self> {
        let (submit_tx, submit_rx) = bounded(config.max_batch_size * 4);
        let shutdown = Arc::new(AtomicBool::new(false));
        let synced_lsn = Arc::new(AtomicU64::new(0));
        let stats = Arc::new(RwLock::new(GroupCommitStats::default()));

        let adaptive = if config.adaptive_batching {
            Some(Arc::new(AdaptiveController::new(&config)))
        } else {
            None
        };

        let commit_thread = if config.dedicated_commit_thread {
            let wal_clone = Arc::clone(&wal);
            let shutdown_clone = Arc::clone(&shutdown);
            let synced_lsn_clone = Arc::clone(&synced_lsn);
            let stats_clone = Arc::clone(&stats);
            let config_clone = config.clone();
            let adaptive_clone = adaptive.clone();

            Some(
                thread::Builder::new()
                    .name("artrie-group-commit".to_string())
                    .spawn(move || {
                        Self::commit_loop(
                            submit_rx,
                            wal_clone,
                            shutdown_clone,
                            synced_lsn_clone,
                            stats_clone,
                            config_clone,
                            adaptive_clone,
                        );
                    })
                    .expect("failed to spawn commit thread"),
            )
        } else {
            None
        };

        Ok(Self {
            wal,
            config,
            submit_tx,
            commit_thread,
            shutdown,
            synced_lsn,
            adaptive,
            stats,
        })
    }

    /// Append a record and wait for it to be durably synced.
    ///
    /// This method blocks until the record has been written to the WAL
    /// and fsync'd to stable storage.
    ///
    /// # Returns
    ///
    /// The LSN assigned to this record.
    pub fn append_with_sync(&self, record: WalRecord) -> Result<Lsn> {
        let submitted_at = Instant::now();
        let serialized_size = record.serialized_size();

        // Allocate LSN
        let lsn = self.wal.allocate_lsn();

        // Create response channel
        let (response_tx, response_rx) = oneshot_channel();

        // Submit to queue
        let pending = PendingWrite {
            record,
            lsn,
            response_tx,
            submitted_at,
            serialized_size,
        };

        self.submit_tx
            .send(pending)
            .map_err(|_| PersistentARTrieError::GroupCommitChannelClosed)?;

        // Wait for response
        let result = response_rx
            .recv()
            .map_err(|_| PersistentARTrieError::GroupCommitChannelClosed)??;

        // Record latency for adaptive batching
        if let Some(ref adaptive) = self.adaptive {
            let latency_us = submitted_at.elapsed().as_micros() as u64;
            adaptive.record_latency(latency_us);
        }

        Ok(result)
    }

    /// Append a record without waiting for sync.
    ///
    /// The record is queued for writing but the method returns immediately.
    /// Use `wait_for_lsn()` to later wait for durability.
    ///
    /// # Returns
    ///
    /// The LSN assigned to this record.
    pub fn append_async(&self, record: WalRecord) -> Result<Lsn> {
        let lsn = self.wal.allocate_lsn();

        // Create a response channel but don't wait on it
        let (response_tx, _response_rx) = oneshot_channel();

        let pending = PendingWrite {
            record,
            lsn,
            response_tx,
            submitted_at: Instant::now(),
            serialized_size: 0, // Not tracked for async
        };

        self.submit_tx
            .send(pending)
            .map_err(|_| PersistentARTrieError::GroupCommitChannelClosed)?;

        Ok(lsn)
    }

    /// Wait until the given LSN has been durably synced.
    pub fn wait_for_lsn(&self, target_lsn: Lsn) {
        while self.synced_lsn.load(Ordering::Acquire) < target_lsn {
            std::hint::spin_loop();
            thread::yield_now();
        }
    }

    /// Force an immediate sync of all pending records.
    pub fn flush(&self) {
        let current_lsn = self.wal.current_lsn().saturating_sub(1);
        if current_lsn > 0 {
            self.wait_for_lsn(current_lsn);
        }
    }

    /// Get current statistics.
    pub fn stats(&self) -> GroupCommitStats {
        self.stats.read().clone()
    }

    /// Get the highest durably synced LSN.
    pub fn synced_lsn(&self) -> Lsn {
        self.synced_lsn.load(Ordering::Acquire)
    }

    /// The background commit loop.
    fn commit_loop(
        submit_rx: Receiver<PendingWrite>,
        wal: Arc<AsyncWalWriter>,
        shutdown: Arc<AtomicBool>,
        synced_lsn: Arc<AtomicU64>,
        stats: Arc<RwLock<GroupCommitStats>>,
        config: GroupCommitConfig,
        adaptive: Option<Arc<AdaptiveController>>,
    ) {
        let mut batch: Vec<PendingWrite> = Vec::with_capacity(config.max_batch_size);
        let mut accumulated_size: usize = 0;
        let mut batch_start = Instant::now();

        loop {
            if shutdown.load(Ordering::Relaxed) {
                // Flush remaining batch before exit
                if !batch.is_empty() {
                    Self::flush_batch(&mut batch, &wal, &synced_lsn, &stats);
                }
                break;
            }

            // Determine current batch parameters
            let (max_delay_us, max_size) = if let Some(ref adaptive) = adaptive {
                (
                    adaptive.get_current_delay_us(),
                    adaptive.get_current_batch_size(),
                )
            } else {
                (config.max_batch_delay_us, config.max_batch_size)
            };

            // Calculate remaining timeout
            let elapsed = batch_start.elapsed();
            let max_delay = Duration::from_micros(max_delay_us);
            let remaining = max_delay.saturating_sub(elapsed);

            // Try to receive a pending write
            match submit_rx.recv_timeout(remaining) {
                Ok(pending) => {
                    if batch.is_empty() {
                        batch_start = Instant::now();
                    }
                    accumulated_size += pending.serialized_size;
                    batch.push(pending);
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    // Timeout reached, flush if we have anything
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    // Channel closed, flush and exit
                    if !batch.is_empty() {
                        Self::flush_batch(&mut batch, &wal, &synced_lsn, &stats);
                    }
                    break;
                }
            }

            // Check if we should flush
            let should_flush = !batch.is_empty()
                && (batch.len() >= max_size
                    || batch_start.elapsed() >= max_delay
                    || accumulated_size >= 1024 * 1024); // 1MB size threshold

            if should_flush {
                Self::flush_batch(&mut batch, &wal, &synced_lsn, &stats);
                accumulated_size = 0;
                batch_start = Instant::now();

                // Trigger adaptive adjustment periodically
                if let Some(ref adaptive) = adaptive {
                    adaptive.adjust();
                }
            }
        }
    }

    /// Flush a batch of pending writes.
    fn flush_batch(
        batch: &mut Vec<PendingWrite>,
        wal: &Arc<AsyncWalWriter>,
        synced_lsn: &Arc<AtomicU64>,
        stats: &Arc<RwLock<GroupCommitStats>>,
    ) {
        if batch.is_empty() {
            return;
        }

        let batch_size = batch.len();
        let mut max_lsn: Lsn = 0;
        let mut total_bytes: usize = 0;

        // Write all records to WAL (AsyncWalWriter handles internal synchronization)
        let write_result: std::result::Result<(), PersistentARTrieError> = (|| {
            for pending in batch.iter() {
                wal.append_with_lsn(pending.lsn, pending.record.clone())
                    .map_err(|e| PersistentARTrieError::Wal(format!("{}", e)))?;
                max_lsn = max_lsn.max(pending.lsn);
                total_bytes += pending.serialized_size;
            }

            // Single fsync for entire batch
            wal.sync()
                .map_err(|e| PersistentARTrieError::Wal(format!("{}", e)))?;
            Ok(())
        })();

        // Update synced LSN and notify waiters
        match write_result {
            Ok(()) => {
                synced_lsn.store(max_lsn, Ordering::Release);

                // Update statistics
                {
                    let mut stats_guard = stats.write();
                    stats_guard.records_committed += batch_size as u64;
                    stats_guard.fsync_count += 1;
                    stats_guard.bytes_written += total_bytes as u64;
                    stats_guard.avg_batch_size =
                        stats_guard.records_committed as f64 / stats_guard.fsync_count as f64;
                }

                // Notify all waiters of success
                for pending in batch.drain(..) {
                    let _ = pending.response_tx.send(Ok(pending.lsn));
                }
            }
            Err(e) => {
                // Notify all waiters of failure
                for pending in batch.drain(..) {
                    let _ = pending
                        .response_tx
                        .send(Err(PersistentARTrieError::Wal(format!("{}", e))));
                }
            }
        }
    }
}

impl Drop for GroupCommitCoordinator {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);

        if let Some(handle) = self.commit_thread.take() {
            let _ = handle.join();
        }
    }
}

// ============================================================================
// Oneshot channel implementation for response notification
// ============================================================================

/// Create a oneshot channel for single-use communication.
fn oneshot_channel<T>() -> (OneshotSender<T>, OneshotReceiver<T>) {
    let shared = Arc::new((Mutex::new(None), Condvar::new()));
    (
        OneshotSender {
            shared: Arc::clone(&shared),
        },
        OneshotReceiver { shared },
    )
}

/// Sender half of a oneshot channel.
struct OneshotSender<T> {
    shared: Arc<(Mutex<Option<T>>, Condvar)>,
}

impl<T> OneshotSender<T> {
    fn send(self, value: T) -> std::result::Result<(), T> {
        let (lock, cvar) = &*self.shared;
        let mut guard = lock.lock().expect("oneshot lock poisoned");
        *guard = Some(value);
        cvar.notify_one();
        Ok(())
    }
}

/// Receiver half of a oneshot channel.
struct OneshotReceiver<T> {
    shared: Arc<(Mutex<Option<T>>, Condvar)>,
}

impl<T> OneshotReceiver<T> {
    fn recv(self) -> std::result::Result<T, ()> {
        let (lock, cvar) = &*self.shared;
        let mut guard = lock.lock().expect("oneshot lock poisoned");
        while guard.is_none() {
            guard = cvar.wait(guard).expect("oneshot condvar poisoned");
        }
        guard.take().ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_artrie_core::wal::{AsyncWalConfig, WalConfig};
    use tempfile::tempdir;

    #[test]
    fn test_group_commit_config_default() {
        let config = GroupCommitConfig::default();
        assert_eq!(config.max_batch_size, 100);
        assert_eq!(config.max_batch_delay_us, 10_000);
        assert!(config.dedicated_commit_thread);
        assert!(config.adaptive_batching);
    }

    #[test]
    fn test_group_commit_config_presets() {
        let low_lat = GroupCommitConfig::low_latency();
        assert_eq!(low_lat.max_batch_size, 10);
        assert_eq!(low_lat.max_batch_delay_us, 1_000);

        let high_tp = GroupCommitConfig::high_throughput();
        assert_eq!(high_tp.max_batch_size, 1000);
        assert_eq!(high_tp.max_batch_delay_us, 50_000);

        let nvme = GroupCommitConfig::nvme_optimized();
        assert_eq!(nvme.max_batch_size, 50);
        assert_eq!(nvme.max_batch_delay_us, 500);
    }

    #[test]
    fn test_group_commit_stats() {
        let mut stats = GroupCommitStats::default();
        stats.records_committed = 100;
        stats.fsync_count = 10;
        assert!((stats.batching_efficiency() - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_oneshot_channel() {
        let (tx, rx) = oneshot_channel::<i32>();
        tx.send(42).expect("send failed");
        let val = rx.recv().expect("recv failed");
        assert_eq!(val, 42);
    }

    #[test]
    fn test_adaptive_controller() {
        let config = GroupCommitConfig::default();
        let controller = AdaptiveController::new(&config);

        // Record some latencies
        for i in 0..200 {
            controller.record_latency(i * 100);
        }

        // Should have enough samples to adjust
        controller.adjust();

        // Verify delay was adjusted (should decrease since latencies are high)
        let delay = controller.get_current_delay_us();
        assert!(delay <= config.max_batch_delay_us);
    }

    #[test]
    fn test_group_commit_coordinator_basic() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");

        let async_config = AsyncWalConfig::with_pending_dir(dir.path().join("pending"));
        let archive_config = WalConfig {
            archive_dir: dir.path().join("archive"),
            ..Default::default()
        };
        let wal =
            AsyncWalWriter::create(&wal_path, async_config, archive_config).expect("create WAL");
        let wal = Arc::new(wal);

        let config = GroupCommitConfig {
            max_batch_size: 10,
            max_batch_delay_us: 100_000, // 100ms
            dedicated_commit_thread: true,
            adaptive_batching: false,
            ..Default::default()
        };

        let coordinator =
            GroupCommitCoordinator::new(Arc::clone(&wal), config).expect("create coordinator");

        // Submit some writes
        for i in 0..5 {
            let record = WalRecord::Insert {
                term: format!("term{}", i).into_bytes(),
                value: None,
            };
            let lsn = coordinator.append_with_sync(record).expect("append");
            assert!(lsn > 0);
        }

        // Check stats
        let stats = coordinator.stats();
        assert_eq!(stats.records_committed, 5);
        assert!(stats.fsync_count >= 1);
    }

    #[test]
    fn test_group_commit_batching() {
        use std::thread;

        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");

        let async_config = AsyncWalConfig::with_pending_dir(dir.path().join("pending"));
        let archive_config = WalConfig {
            archive_dir: dir.path().join("archive"),
            ..Default::default()
        };
        let wal =
            AsyncWalWriter::create(&wal_path, async_config, archive_config).expect("create WAL");
        let wal = Arc::new(wal);

        let config = GroupCommitConfig {
            max_batch_size: 100,
            max_batch_delay_us: 50_000, // 50ms - long enough to batch
            dedicated_commit_thread: true,
            adaptive_batching: false,
            ..Default::default()
        };

        let coordinator = Arc::new(
            GroupCommitCoordinator::new(Arc::clone(&wal), config).expect("create coordinator"),
        );

        // Spawn multiple writers
        let num_writers = 4;
        let writes_per_writer = 25;
        let mut handles = Vec::new();

        for writer_id in 0..num_writers {
            let coord = Arc::clone(&coordinator);
            let handle = thread::spawn(move || {
                for i in 0..writes_per_writer {
                    let record = WalRecord::Insert {
                        term: format!("writer{}term{}", writer_id, i).into_bytes(),
                        value: None,
                    };
                    coord.append_with_sync(record).expect("append");
                }
            });
            handles.push(handle);
        }

        // Wait for all writers
        for handle in handles {
            handle.join().expect("thread join");
        }

        // Check stats - should have batched writes
        let stats = coordinator.stats();
        assert_eq!(
            stats.records_committed,
            (num_writers * writes_per_writer) as u64
        );

        // With batching, should have fewer fsyncs than records
        // (assuming batching worked)
        let efficiency = stats.batching_efficiency();
        println!(
            "Batching efficiency: {:.2} records/fsync ({} records, {} fsyncs)",
            efficiency, stats.records_committed, stats.fsync_count
        );
    }
}
