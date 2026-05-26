# PersistentARTrie Persistence Enhancements: Experimental Plan

## Executive Summary

This document outlines a series of experiments to enhance the persistence layer of `PersistentARTrie` and `PersistentARTrieChar`. The goal is to improve throughput, reduce I/O overhead, and maintain crash recovery guarantees while adapting to available system resources.

**Current Architecture:** Write-back with redo-only WAL, fixed buffer pool, Clock eviction, manual sync/checkpoint.

**Target Improvements:**
- 2-5x throughput improvement for write-heavy workloads
- Adaptive memory utilization (use available RAM efficiently)
- Reduced WAL sync overhead via group commit
- Faster crash recovery via per-node logging
- Automatic durability guarantees without manual `sync()` calls

---

## Table of Contents

1. [Background and Motivation](#1-background-and-motivation)
2. [Experiment 1: Group Commit for WAL](#2-experiment-1-group-commit-for-wal)
3. [Experiment 2: Epoch-Based Automatic Checkpointing](#3-experiment-2-epoch-based-automatic-checkpointing)
4. [Experiment 3: Memory-Pressure-Aware Eviction](#4-experiment-3-memory-pressure-aware-eviction)
5. [Experiment 4: Adaptive Buffer Pool Sizing](#5-experiment-4-adaptive-buffer-pool-sizing)
6. [Experiment 5: Per-Node Logging](#6-experiment-5-per-node-logging)
7. [Benchmarking Methodology](#7-benchmarking-methodology)
8. [Implementation Order and Dependencies](#8-implementation-order-and-dependencies)
9. [References](#9-references)

---

## 1. Background and Motivation

### 1.1 Current Architecture Analysis

The current `PersistentARTrie` persistence layer has these characteristics:

| Component | Current Implementation | Limitation |
|-----------|----------------------|------------|
| Buffer Pool | Fixed size (default 16 frames × 256KB) | Cannot adapt to available memory |
| Eviction | Clock algorithm (reactive) | No proactive flushing under memory pressure |
| WAL Writes | Buffered `BufWriter`, no auto-sync | Small durability window without explicit `sync()` |
| WAL Sync | Per-operation or manual | High fsync overhead for write-heavy workloads |
| Checkpoints | Manual only | WAL grows unbounded without user intervention |
| Recovery | Full WAL replay from checkpoint | Slow for large WAL files |

### 1.2 Performance Bottlenecks

Based on typical persistent data structure workloads:

1. **fsync overhead**: Each `sync()` call triggers expensive disk flush (~10ms HDD, ~0.1ms NVMe)
2. **Fixed memory**: Cannot utilize available RAM beyond configured pool size
3. **Recovery time**: Linear in WAL size; no incremental recovery
4. **Manual management**: Users must call `sync()` and `checkpoint()` explicitly

### 1.3 State-of-the-Art Techniques

Recent research (2024-2025) provides guidance:

- **BD+Tree (SIGMOD 2024)** [1]: Epoch-based relaxed persistence achieves 2.4x throughput, 90% fewer NVM writes
- **Per-page logging** [2]: Near-instant recovery by storing redo info per-node
- **Bw-Tree delta records** [3]: Lock-free updates via append-only delta chains
- **ARIES** [4]: Industry-standard WAL and recovery algorithm
- **Linux PSI** [5]: Kernel-level memory pressure detection

### 1.4 ARIES Recovery Model

Our implementation follows the ARIES (Algorithms for Recovery and Isolation Exploiting Semantics) model [4], the industry standard for database recovery:

**Key Principles:**
1. **Write-Ahead Logging (WAL)**: Log records must be written to stable storage before the corresponding data page changes
2. **Redo-at-Restart**: On recovery, redo all logged operations to restore exact pre-crash state
3. **Undo-for-Rollback**: Use undo records to roll back uncommitted transactions

**Log Sequence Numbers (LSN):**
```
Each log record has a unique, monotonically increasing LSN.
LSN ordering: LSN_a < LSN_b implies record_a occurred before record_b
```

**Recovery Phases:**
```
1. Analysis: Scan log from last checkpoint, identify dirty pages and active transactions
2. Redo: Replay all logged operations from checkpoint forward
3. Undo: Roll back incomplete transactions (not needed for our redo-only design)
```

Our current implementation uses a simplified redo-only variant suitable for dictionary operations.

---

## 2. Experiment 1: Group Commit for WAL

### 2.1 Hypothesis

Batching WAL syncs will reduce fsync overhead by amortizing the cost across multiple operations, improving write throughput by 2-5x for high-concurrency workloads.

### 2.2 Background: How Production Systems Implement Group Commit

#### 2.2.1 PostgreSQL Approach [6][7]

PostgreSQL uses a **time-based delay strategy**:

```
Parameters:
- commit_delay: microseconds to wait before WAL flush (default: 0)
- commit_siblings: minimum concurrent transactions to trigger delay (default: 5)

Mechanism:
1. Transaction reaches commit point
2. If commit_siblings active transactions exist:
   - Sleep for commit_delay microseconds
   - Other transactions accumulate in queue
3. Leader performs single fsync() for all queued transactions
4. All waiters are notified of completion
```

**Natural Group Commit**: Even with `commit_delay=0`, PostgreSQL achieves implicit grouping because transactions that arrive during an ongoing fsync are batched with the next fsync.

#### 2.2.2 MySQL/InnoDB Approach [8]

MySQL uses a **leader-follower pattern** with three phases:

```
Phase 1 - Flush Stage:
  - Leader collects all pending transactions
  - Writes binary log entries for entire group

Phase 2 - Sync Stage:
  - Single fsync() for all collected entries
  - binlog_group_commit_sync_delay: max wait time (microseconds)
  - binlog_group_commit_sync_no_delay_count: flush immediately if N transactions waiting

Phase 3 - Commit Stage:
  - Update transaction status for all group members
  - Release locks in order
```

#### 2.2.3 RocksDB Approach [9]

RocksDB uses **explicit leader-based group commit**:

```rust
// Pseudocode for RocksDB's approach
fn write_batch_group(batches: Vec<WriteBatch>) {
    // First writer becomes leader
    let leader = batches[0].thread_id;

    // Leader concatenates all batches
    let combined = batches.iter()
        .flat_map(|b| b.entries())
        .collect();

    // Single WAL write + fsync
    wal.append(combined);
    wal.sync();

    // Notify all followers
    for batch in batches {
        batch.notify_complete();
    }
}
```

**Pipelined Write Optimization**: When enabled, WAL writes and memtable updates are decoupled into separate queues, allowing greater parallelism.

### 2.3 Design

Our design combines the best aspects of these approaches:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        Group Commit Architecture                         │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  Writer Threads                    Commit Coordinator                    │
│  ┌──────────┐                     ┌────────────────────────────────┐    │
│  │ Thread 1 │──┐                  │  Pending Queue                 │    │
│  └──────────┘  │                  │  ┌────┬────┬────┬────┬────┐   │    │
│  ┌──────────┐  │   submit()       │  │ R1 │ R2 │ R3 │ R4 │ R5 │   │    │
│  │ Thread 2 │──┼─────────────────►│  └────┴────┴────┴────┴────┘   │    │
│  └──────────┘  │                  │                                │    │
│  ┌──────────┐  │                  │  Batch Triggers:               │    │
│  │ Thread N │──┘                  │  • size >= MAX_BATCH_SIZE      │    │
│  └──────────┘                     │  • time >= MAX_BATCH_DELAY     │    │
│       ▲                           │  • explicit flush()            │    │
│       │                           └───────────────┬────────────────┘    │
│       │                                           │                      │
│       │   notify()                                ▼                      │
│       │                           ┌────────────────────────────────┐    │
│       └───────────────────────────│  Commit Process                │    │
│                                   │  1. Acquire WAL lock           │    │
│                                   │  2. Append all records         │    │
│                                   │  3. Single fsync()             │    │
│                                   │  4. Update synced_lsn          │    │
│                                   │  5. Notify all waiters         │    │
│                                   └────────────────────────────────┘    │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

### 2.4 Configuration Parameters

```rust
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
```

### 2.5 Complete Implementation

#### 2.5.1 Core Types

```rust
// src/persistent_artrie/group_commit.rs

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};

use crate::persistent_artrie::wal::{Lsn, WalRecord, WalWriter};
use crate::Result;

/// A pending write waiting for group commit.
#[derive(Debug)]
struct PendingWrite {
    /// The WAL record to write.
    record: WalRecord,

    /// The assigned LSN for this record.
    lsn: Lsn,

    /// Channel to notify the waiter when commit is complete.
    response_tx: oneshot::Sender<Result<Lsn>>,

    /// Timestamp when this write was submitted.
    submitted_at: Instant,

    /// Size of the serialized record in bytes.
    serialized_size: usize,
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

/// Adaptive batching controller using AIMD (Additive Increase, Multiplicative Decrease).
#[derive(Debug)]
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
            min_delay_us: 100,    // 0.1ms minimum
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
/// ```rust,ignore
/// let wal = Arc::new(RwLock::new(WalWriter::open(path)?));
/// let config = GroupCommitConfig::default();
/// let coordinator = GroupCommitCoordinator::new(wal, config)?;
///
/// // From multiple threads:
/// let lsn = coordinator.append_with_sync(record)?;
/// ```
pub struct GroupCommitCoordinator {
    /// The underlying WAL writer.
    wal: Arc<RwLock<WalWriter>>,

    /// Configuration.
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
    adaptive: Option<AdaptiveController>,

    /// Statistics.
    stats: Arc<RwLock<GroupCommitStats>>,
}

impl GroupCommitCoordinator {
    /// Create a new group commit coordinator.
    pub fn new(wal: Arc<RwLock<WalWriter>>, config: GroupCommitConfig) -> Result<Self> {
        let (submit_tx, submit_rx) = bounded(config.max_batch_size * 4);
        let shutdown = Arc::new(AtomicBool::new(false));
        let synced_lsn = Arc::new(AtomicU64::new(0));
        let stats = Arc::new(RwLock::new(GroupCommitStats::default()));

        let adaptive = if config.adaptive_batching {
            Some(AdaptiveController::new(&config))
        } else {
            None
        };

        let commit_thread = if config.dedicated_commit_thread {
            let wal_clone = Arc::clone(&wal);
            let shutdown_clone = Arc::clone(&shutdown);
            let synced_lsn_clone = Arc::clone(&synced_lsn);
            let stats_clone = Arc::clone(&stats);
            let config_clone = config.clone();
            let adaptive_clone = adaptive.as_ref().map(|a| {
                // Share the adaptive controller state
                AdaptiveController {
                    current_delay_us: AtomicU64::new(a.current_delay_us.load(Ordering::Relaxed)),
                    current_batch_size: AtomicUsize::new(a.current_batch_size.load(Ordering::Relaxed)),
                    latency_samples: Mutex::new(VecDeque::new()),
                    target_latency_us: a.target_latency_us,
                    min_delay_us: a.min_delay_us,
                    max_delay_us: a.max_delay_us,
                }
            });

            Some(thread::Builder::new()
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
                .expect("failed to spawn commit thread"))
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
        let lsn = {
            let wal = self.wal.read().expect("WAL lock poisoned");
            wal.allocate_lsn()
        };

        // Create response channel
        let (response_tx, response_rx) = oneshot::channel();

        // Submit to queue
        let pending = PendingWrite {
            record,
            lsn,
            response_tx,
            submitted_at,
            serialized_size,
        };

        self.submit_tx.send(pending)
            .map_err(|_| crate::Error::ChannelClosed)?;

        // Wait for response
        let result = response_rx.recv()
            .map_err(|_| crate::Error::ChannelClosed)??;

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
        let lsn = {
            let wal = self.wal.read().expect("WAL lock poisoned");
            wal.allocate_lsn()
        };

        // Create a response channel but don't wait on it
        let (response_tx, _response_rx) = oneshot::channel();

        let pending = PendingWrite {
            record,
            lsn,
            response_tx,
            submitted_at: Instant::now(),
            serialized_size: 0, // Not tracked for async
        };

        self.submit_tx.send(pending)
            .map_err(|_| crate::Error::ChannelClosed)?;

        Ok(lsn)
    }

    /// Wait until the given LSN has been durably synced.
    pub fn wait_for_lsn(&self, target_lsn: Lsn) -> Result<()> {
        loop {
            let synced = self.synced_lsn.load(Ordering::Acquire);
            if synced >= target_lsn {
                return Ok(());
            }

            // Spin with backoff
            std::hint::spin_loop();
            thread::yield_now();
        }
    }

    /// Force an immediate sync of all pending records.
    pub fn flush(&self) -> Result<()> {
        // Send a flush signal (empty record with special flag)
        // For simplicity, we'll just wait for current queue to drain
        let current_lsn = {
            let wal = self.wal.read().expect("WAL lock poisoned");
            wal.current_lsn()
        };
        self.wait_for_lsn(current_lsn)
    }

    /// Get current statistics.
    pub fn stats(&self) -> GroupCommitStats {
        self.stats.read().expect("stats lock poisoned").clone()
    }

    /// The background commit loop.
    fn commit_loop(
        submit_rx: Receiver<PendingWrite>,
        wal: Arc<RwLock<WalWriter>>,
        shutdown: Arc<AtomicBool>,
        synced_lsn: Arc<AtomicU64>,
        stats: Arc<RwLock<GroupCommitStats>>,
        config: GroupCommitConfig,
        adaptive: Option<AdaptiveController>,
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
                (adaptive.get_current_delay_us(), adaptive.get_current_batch_size())
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
            let should_flush = !batch.is_empty() && (
                batch.len() >= max_size ||
                batch_start.elapsed() >= max_delay ||
                accumulated_size >= 1024 * 1024 // 1MB size threshold
            );

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
        wal: &Arc<RwLock<WalWriter>>,
        synced_lsn: &Arc<AtomicU64>,
        stats: &Arc<RwLock<GroupCommitStats>>,
    ) {
        if batch.is_empty() {
            return;
        }

        let batch_size = batch.len();
        let mut max_lsn: Lsn = 0;
        let mut total_bytes: usize = 0;

        // Write all records to WAL (under write lock)
        let write_result = {
            let mut wal_guard = wal.write().expect("WAL lock poisoned");

            for pending in batch.iter() {
                match wal_guard.append(&pending.record) {
                    Ok(_) => {
                        max_lsn = max_lsn.max(pending.lsn);
                        total_bytes += pending.serialized_size;
                    }
                    Err(e) => {
                        // On write error, notify all waiters of failure
                        for pending in batch.drain(..) {
                            let _ = pending.response_tx.send(Err(e.clone()));
                        }
                        return;
                    }
                }
            }

            // Single fsync for entire batch
            wal_guard.sync()
        };

        // Update synced LSN and notify waiters
        match write_result {
            Ok(()) => {
                synced_lsn.store(max_lsn, Ordering::Release);

                // Update statistics
                {
                    let mut stats_guard = stats.write().expect("stats lock poisoned");
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
                    let _ = pending.response_tx.send(Err(e.clone()));
                }
            }
        }
    }
}

impl Drop for GroupCommitCoordinator {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);

        // Close the submit channel to unblock the commit thread
        drop(self.submit_tx.clone());

        if let Some(handle) = self.commit_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Oneshot channel implementation for response notification.
mod oneshot {
    use std::sync::{Arc, Condvar, Mutex};

    pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
        let shared = Arc::new((Mutex::new(None), Condvar::new()));
        (
            Sender { shared: Arc::clone(&shared) },
            Receiver { shared },
        )
    }

    pub struct Sender<T> {
        shared: Arc<(Mutex<Option<T>>, Condvar)>,
    }

    impl<T> Sender<T> {
        pub fn send(self, value: T) -> Result<(), T> {
            let (lock, cvar) = &*self.shared;
            let mut guard = lock.lock().unwrap();
            *guard = Some(value);
            cvar.notify_one();
            Ok(())
        }
    }

    pub struct Receiver<T> {
        shared: Arc<(Mutex<Option<T>>, Condvar)>,
    }

    impl<T> Receiver<T> {
        pub fn recv(self) -> Result<T, ()> {
            let (lock, cvar) = &*self.shared;
            let mut guard = lock.lock().unwrap();
            while guard.is_none() {
                guard = cvar.wait(guard).unwrap();
            }
            guard.take().ok_or(())
        }
    }
}
```

#### 2.5.2 Integration with PersistentARTrie

```rust
// Modifications to src/persistent_artrie/dict_impl.rs

impl<V: DictionaryValue> PersistentARTrie<V> {
    /// Insert a term with group commit.
    ///
    /// The operation is logged to WAL and batched with other concurrent
    /// operations for efficient fsync.
    pub fn insert_grouped(&self, term: &str) -> Result<bool> {
        let record = WalRecord::Insert {
            term: term.to_string(),
            value: None,
        };

        // Submit to group commit (blocks until durable)
        let inner = self.inner.read().expect("inner lock poisoned");
        if let Some(ref gc) = inner.group_commit {
            gc.append_with_sync(record)?;
        } else {
            // Fallback to direct WAL write
            if let Some(ref wal) = inner.wal_writer {
                let mut wal_guard = wal.lock().expect("WAL lock poisoned");
                wal_guard.append(&record)?;
                wal_guard.sync()?;
            }
        }

        // Perform the actual insert
        self.insert_internal(term)
    }

    /// Insert without waiting for durability.
    ///
    /// Returns immediately after queueing the operation. The LSN can be
    /// used to later wait for durability via `wait_for_lsn()`.
    pub fn insert_async(&self, term: &str) -> Result<Lsn> {
        let record = WalRecord::Insert {
            term: term.to_string(),
            value: None,
        };

        let inner = self.inner.read().expect("inner lock poisoned");
        let lsn = if let Some(ref gc) = inner.group_commit {
            gc.append_async(record)?
        } else {
            return Err(crate::Error::GroupCommitNotEnabled);
        };

        // Perform the actual insert (in-memory)
        self.insert_internal(term)?;

        Ok(lsn)
    }
}
```

### 2.6 Expected Outcomes

| Metric | Baseline | Expected | Measurement Method |
|--------|----------|----------|-------------------|
| Write throughput (ops/sec) | ~10K | ~50K-100K | Criterion benchmark |
| fsync calls/sec | ~10K | ~100-1K | `perf stat -e syscalls:sys_enter_fsync` |
| P50 write latency | ~0.1ms | ~1-2ms | Histogram in stats |
| P99 write latency | ~0.5ms | ~10-15ms | Histogram in stats |
| Durability window | 0 (with sync) | ≤max_batch_delay_us | Configuration |
| Batching efficiency | 1.0 | 10-100 | avg_batch_size metric |

### 2.7 Verification Strategy

#### 2.7.1 Correctness Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Test that all records are durable after append_with_sync returns.
    #[test]
    fn test_durability_guarantee() {
        let dir = tempdir().unwrap();
        let wal = Arc::new(RwLock::new(
            WalWriter::create(dir.path().join("test.wal")).unwrap()
        ));
        let gc = GroupCommitCoordinator::new(wal.clone(), GroupCommitConfig::default()).unwrap();

        // Write some records
        let mut lsns = Vec::new();
        for i in 0..100 {
            let record = WalRecord::Insert {
                term: format!("key_{}", i),
                value: None,
            };
            lsns.push(gc.append_with_sync(record).unwrap());
        }

        // Verify all records are readable from WAL
        drop(gc);
        let wal_reader = WalReader::open(dir.path().join("test.wal")).unwrap();
        let records: Vec<_> = wal_reader.iter().collect();
        assert_eq!(records.len(), 100);
    }

    /// Test crash recovery with group commit.
    #[test]
    fn test_crash_recovery() {
        let dir = tempdir().unwrap();

        // Phase 1: Write records with group commit
        {
            let wal = Arc::new(RwLock::new(
                WalWriter::create(dir.path().join("test.wal")).unwrap()
            ));
            let gc = GroupCommitCoordinator::new(wal, GroupCommitConfig::default()).unwrap();

            for i in 0..1000 {
                let record = WalRecord::Insert {
                    term: format!("key_{}", i),
                    value: None,
                };
                gc.append_with_sync(record).unwrap();
            }

            // Simulate crash by not calling shutdown cleanly
            std::mem::forget(gc);
        }

        // Phase 2: Recover and verify
        let wal_reader = WalReader::open(dir.path().join("test.wal")).unwrap();
        let records: Vec<_> = wal_reader.iter().collect();

        // All synced records should be present
        // (Some may be lost from the last incomplete batch)
        assert!(records.len() >= 900); // Allow for some loss in final batch
    }

    /// Test LSN ordering is preserved.
    #[test]
    fn test_lsn_ordering() {
        let dir = tempdir().unwrap();
        let wal = Arc::new(RwLock::new(
            WalWriter::create(dir.path().join("test.wal")).unwrap()
        ));
        let gc = GroupCommitCoordinator::new(wal, GroupCommitConfig::default()).unwrap();

        let mut prev_lsn = 0;
        for i in 0..100 {
            let record = WalRecord::Insert {
                term: format!("key_{}", i),
                value: None,
            };
            let lsn = gc.append_with_sync(record).unwrap();
            assert!(lsn > prev_lsn, "LSN must be monotonically increasing");
            prev_lsn = lsn;
        }
    }

    /// Test concurrent writers.
    #[test]
    fn test_concurrent_writers() {
        use std::thread;

        let dir = tempdir().unwrap();
        let wal = Arc::new(RwLock::new(
            WalWriter::create(dir.path().join("test.wal")).unwrap()
        ));
        let gc = Arc::new(
            GroupCommitCoordinator::new(wal, GroupCommitConfig::default()).unwrap()
        );

        let num_threads = 8;
        let ops_per_thread = 1000;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let gc = Arc::clone(&gc);
                thread::spawn(move || {
                    for i in 0..ops_per_thread {
                        let record = WalRecord::Insert {
                            term: format!("thread_{}_key_{}", t, i),
                            value: None,
                        };
                        gc.append_with_sync(record).unwrap();
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        let stats = gc.stats();
        assert_eq!(stats.records_committed, (num_threads * ops_per_thread) as u64);
        assert!(stats.avg_batch_size > 1.0, "Should have batching");
    }
}
```

#### 2.7.2 Performance Benchmarks

```rust
// benches/group_commit_bench.rs

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId, Throughput};
use libdictenstein::persistent_artrie::{GroupCommitConfig, GroupCommitCoordinator};
use std::sync::Arc;
use tempfile::tempdir;

fn benchmark_group_commit(c: &mut Criterion) {
    let mut group = c.benchmark_group("group_commit");

    // Test different batch sizes
    for batch_size in [1, 10, 100, 1000] {
        group.throughput(Throughput::Elements(10000));
        group.bench_with_input(
            BenchmarkId::new("batch_size", batch_size),
            &batch_size,
            |b, &batch_size| {
                let dir = tempdir().unwrap();
                let config = GroupCommitConfig {
                    max_batch_size: batch_size,
                    max_batch_delay_us: 10_000,
                    ..Default::default()
                };
                let wal = Arc::new(RwLock::new(
                    WalWriter::create(dir.path().join("bench.wal")).unwrap()
                ));
                let gc = GroupCommitCoordinator::new(wal, config).unwrap();

                b.iter(|| {
                    for i in 0..10000 {
                        let record = WalRecord::Insert {
                            term: format!("key_{}", i),
                            value: None,
                        };
                        black_box(gc.append_with_sync(record).unwrap());
                    }
                });
            },
        );
    }

    group.finish();
}

fn benchmark_concurrent_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_throughput");

    for num_threads in [1, 2, 4, 8, 16] {
        group.throughput(Throughput::Elements(10000));
        group.bench_with_input(
            BenchmarkId::new("threads", num_threads),
            &num_threads,
            |b, &num_threads| {
                let dir = tempdir().unwrap();
                let config = GroupCommitConfig::default();
                let wal = Arc::new(RwLock::new(
                    WalWriter::create(dir.path().join("bench.wal")).unwrap()
                ));
                let gc = Arc::new(GroupCommitCoordinator::new(wal, config).unwrap());

                b.iter(|| {
                    let ops_per_thread = 10000 / num_threads;
                    let handles: Vec<_> = (0..num_threads)
                        .map(|t| {
                            let gc = Arc::clone(&gc);
                            std::thread::spawn(move || {
                                for i in 0..ops_per_thread {
                                    let record = WalRecord::Insert {
                                        term: format!("t{}_k{}", t, i),
                                        value: None,
                                    };
                                    black_box(gc.append_with_sync(record).unwrap());
                                }
                            })
                        })
                        .collect();

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, benchmark_group_commit, benchmark_concurrent_throughput);
criterion_main!(benches);
```

---

## 3. Experiment 2: Epoch-Based Automatic Checkpointing

> Verification note (2026-05-25): this section is the original experiment
> plan. The checked production claim is narrower: epoch management tracks
> WAL/metadata, public mutations update that tracking after successful WAL
> appends, and `force_epoch_checkpoint()` publishes the trie checkpoint before
> durable epoch metadata. Threshold-driven epoch advancement is not claimed to
> be an automatic full-trie checkpoint without the explicit checkpoint path.

### 3.1 Hypothesis

Automatic periodic checkpointing will bound WAL size, provide predictable durability guarantees, and enable faster recovery without manual intervention.

### 3.2 Background: BD+Tree Epoch-Based Persistence

The BD+Tree [1] introduced epoch-based persistence for B+ trees on Non-Volatile Memory (NVM), achieving:
- **2.4x throughput improvement** over state-of-the-art persistent B+ trees
- **Up to 99% reduction in NVM writes** through cache reuse
- **Bounded recovery window** of 2 epochs

**Key Insight**: By dividing time into epochs and guaranteeing recovery only to epoch N-2, the system can batch writes within each epoch and exploit cache reuse.

```
Timeline: ═══════════════════════════════════════════════════════════►
          │ Epoch 0 │ Epoch 1 │ Epoch 2 │ Epoch 3 │ Epoch 4 │
          │         │         │         │    ▲    │         │
          │         │         │         │ CRASH   │         │
          │         │         │         │         │         │
Recovery: ◄─────────────────────────────┘
          Recover to end of Epoch 2 (guaranteed durable)
          Replay Epoch 3 WAL (best effort)
```

### 3.3 Design

#### 3.3.1 Epoch Lifecycle

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         Epoch State Machine                              │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│   ┌───────────┐    advance()    ┌───────────┐    checkpoint()           │
│   │  ACTIVE   │─────────────────►│  SEALING  │─────────────────┐        │
│   │           │                  │           │                  │        │
│   │ Epoch N   │                  │ Epoch N   │                  │        │
│   │           │                  │ No new    │                  │        │
│   │ Accepts   │                  │ writes    │                  ▼        │
│   │ new ops   │                  │ allowed   │          ┌───────────┐   │
│   └───────────┘                  └───────────┘          │  DURABLE  │   │
│         ▲                                               │           │   │
│         │                                               │ Epoch N   │   │
│         │         New epoch starts                      │ Fully     │   │
│         └─────────────────────────────────────────────  │ persisted │   │
│                                                         └───────────┘   │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

#### 3.3.2 WAL Segmentation

Each epoch has its own WAL segment:

```
Directory Structure:
data/
├── artrie.dat           # Main trie data file
├── wal/
│   ├── epoch_0000000042.wal
│   ├── epoch_0000000043.wal
│   ├── epoch_0000000044.wal  (current)
│   └── checkpoint.meta       # Last checkpoint info
└── checkpoint/
    └── checkpoint_0000000042.snap
```

### 3.4 Configuration Parameters

```rust
/// Configuration for epoch-based checkpoint tracking.
///
/// Epochs divide WAL writes into discrete intervals. Durable epoch metadata is
/// published by explicit checkpoint paths after the trie data checkpoint has
/// been persisted and verified.
///
/// # Recovery Semantics
///
/// The epoch metadata layer does not replace the trie checkpoint/WAL recovery
/// boundary. Recovery may trust a durable epoch only after metadata publication
/// follows a successful trie checkpoint.
///
/// # Tuning Guidelines
///
/// - **epoch_duration**: Controls the maximum data loss window
///   - 100ms: Low latency, frequent checkpoints, higher overhead
///   - 1s: Balanced, good for most workloads
///   - 10s: High throughput, larger recovery window
///
/// - **max_wal_size_bytes**: Prevents unbounded WAL growth
///   - Set based on available disk space and recovery time requirements
///   - Larger values = faster writes, slower recovery
#[derive(Debug, Clone)]
pub struct EpochConfig {
    /// Duration of each epoch.
    ///
    /// This is the target time between checkpoints. The actual epoch
    /// duration may be shorter if other triggers fire first.
    ///
    /// Default: 100ms
    /// Range: 10ms - 60s
    pub epoch_duration: Duration,

    /// Maximum operations per epoch before forcing early checkpoint.
    ///
    /// When this many operations have been logged in the current epoch,
    /// an early checkpoint is triggered regardless of time elapsed.
    ///
    /// Default: 10_000
    /// Range: 100 - 10_000_000
    pub max_ops_per_epoch: usize,

    /// Maximum WAL size in bytes before forcing checkpoint.
    ///
    /// This bounds the total WAL size across all epochs. When exceeded,
    /// the oldest epoch is checkpointed and its WAL segment deleted.
    ///
    /// Default: 64MB
    /// Range: 1MB - 10GB
    pub max_wal_size_bytes: usize,

    /// Number of epoch WAL segments to retain.
    ///
    /// After checkpoint, this many old WAL segments are kept for
    /// debugging/auditing. Set to 0 to delete immediately.
    ///
    /// Default: 2
    /// Range: 0 - 100
    pub retention_epochs: usize,

    /// Use a background thread for checkpointing.
    ///
    /// When true, checkpoints happen in the background without blocking
    /// foreground operations. When false, checkpoint is synchronous.
    ///
    /// Default: true
    pub background_checkpoint: bool,

    /// Enable incremental checkpointing.
    ///
    /// When true, only dirty pages since the last checkpoint are written.
    /// When false, the entire trie is persisted (simpler but slower).
    ///
    /// Default: true
    pub incremental_checkpoint: bool,

    /// Checkpoint compression level (0 = none, 1-9 = zstd levels).
    ///
    /// Higher levels give better compression but use more CPU.
    ///
    /// Default: 0 (no compression)
    /// Range: 0 - 9
    pub checkpoint_compression_level: u8,
}

impl Default for EpochConfig {
    fn default() -> Self {
        Self {
            epoch_duration: Duration::from_millis(100),
            max_ops_per_epoch: 10_000,
            max_wal_size_bytes: 64 * 1024 * 1024,
            retention_epochs: 2,
            background_checkpoint: true,
            incremental_checkpoint: true,
            checkpoint_compression_level: 0,
        }
    }
}
```

### 3.5 Complete Implementation

#### 3.5.1 Epoch Manager

```rust
// src/persistent_artrie/epoch.rs

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::persistent_artrie::wal::{Lsn, WalWriter, WalReader, WalRecord};
use crate::Result;

/// Unique identifier for an epoch.
pub type EpochId = u64;

/// State of an epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochState {
    /// Epoch is active and accepting new operations.
    Active,
    /// Epoch is being sealed (no new operations, pending writes completing).
    Sealing,
    /// Epoch has been checkpointed and is durable.
    Durable,
    /// Epoch has been archived (WAL deleted, only checkpoint remains).
    Archived,
}

/// Metadata for a single epoch.
#[derive(Debug, Clone)]
pub struct EpochMetadata {
    /// Unique epoch identifier.
    pub id: EpochId,

    /// Current state of this epoch.
    pub state: EpochState,

    /// When this epoch started.
    pub started_at: SystemTime,

    /// When this epoch was sealed (None if still active).
    pub sealed_at: Option<SystemTime>,

    /// When this epoch was checkpointed (None if not yet).
    pub checkpointed_at: Option<SystemTime>,

    /// Number of operations in this epoch.
    pub operation_count: usize,

    /// WAL size for this epoch in bytes.
    pub wal_size_bytes: usize,

    /// First LSN in this epoch.
    pub first_lsn: Lsn,

    /// Last LSN in this epoch (updated as operations are added).
    pub last_lsn: Lsn,
}

/// Checkpoint metadata stored on disk.
#[derive(Debug, Clone)]
struct CheckpointMeta {
    /// Epoch that was checkpointed.
    epoch_id: EpochId,

    /// LSN up to which data is durable.
    checkpoint_lsn: Lsn,

    /// Timestamp of checkpoint.
    timestamp: SystemTime,

    /// Checksum of the checkpoint data.
    checksum: u64,
}

impl CheckpointMeta {
    fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(40);
        buf.extend_from_slice(&self.epoch_id.to_le_bytes());
        buf.extend_from_slice(&self.checkpoint_lsn.to_le_bytes());
        let timestamp_secs = self.timestamp
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        buf.extend_from_slice(&timestamp_secs.to_le_bytes());
        buf.extend_from_slice(&self.checksum.to_le_bytes());
        buf
    }

    fn deserialize(data: &[u8]) -> Option<Self> {
        if data.len() < 32 {
            return None;
        }

        let epoch_id = u64::from_le_bytes(data[0..8].try_into().ok()?);
        let checkpoint_lsn = u64::from_le_bytes(data[8..16].try_into().ok()?);
        let timestamp_secs = u64::from_le_bytes(data[16..24].try_into().ok()?);
        let checksum = u64::from_le_bytes(data[24..32].try_into().ok()?);

        Some(Self {
            epoch_id,
            checkpoint_lsn,
            timestamp: UNIX_EPOCH + Duration::from_secs(timestamp_secs),
            checksum,
        })
    }
}

/// The epoch manager coordinates epoch lifecycle and checkpointing.
pub struct EpochManager {
    /// Base directory for WAL and checkpoint files.
    base_dir: PathBuf,

    /// Configuration.
    config: EpochConfig,

    /// Current epoch ID (atomically incrementing).
    current_epoch: AtomicU64,

    /// Operations in current epoch.
    current_ops: AtomicUsize,

    /// WAL bytes in current epoch.
    current_wal_bytes: AtomicUsize,

    /// When current epoch started.
    epoch_start: RwLock<Instant>,

    /// Metadata for recent epochs.
    epochs: RwLock<VecDeque<EpochMetadata>>,

    /// Current WAL writer.
    wal_writer: RwLock<Option<WalWriter>>,

    /// Background checkpoint thread.
    checkpoint_thread: Mutex<Option<JoinHandle<()>>>,

    /// Signal for background thread.
    checkpoint_signal: Arc<(Mutex<bool>, Condvar)>,

    /// Shutdown flag.
    shutdown: Arc<AtomicBool>,

    /// Last checkpoint metadata.
    last_checkpoint: RwLock<Option<CheckpointMeta>>,
}

impl EpochManager {
    /// Create a new epoch manager.
    pub fn new(base_dir: impl AsRef<Path>, config: EpochConfig) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();

        // Create directories
        fs::create_dir_all(base_dir.join("wal"))?;
        fs::create_dir_all(base_dir.join("checkpoint"))?;

        // Load last checkpoint if exists
        let last_checkpoint = Self::load_checkpoint_meta(&base_dir)?;
        let starting_epoch = last_checkpoint
            .as_ref()
            .map(|c| c.epoch_id + 1)
            .unwrap_or(0);

        let manager = Self {
            base_dir,
            config,
            current_epoch: AtomicU64::new(starting_epoch),
            current_ops: AtomicUsize::new(0),
            current_wal_bytes: AtomicUsize::new(0),
            epoch_start: RwLock::new(Instant::now()),
            epochs: RwLock::new(VecDeque::new()),
            wal_writer: RwLock::new(None),
            checkpoint_thread: Mutex::new(None),
            checkpoint_signal: Arc::new((Mutex::new(false), Condvar::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
            last_checkpoint: RwLock::new(last_checkpoint),
        };

        // Open WAL for current epoch
        manager.open_epoch_wal(starting_epoch)?;

        // Start background checkpoint thread if configured
        if manager.config.background_checkpoint {
            manager.start_checkpoint_thread();
        }

        Ok(manager)
    }

    /// Get the current epoch ID.
    pub fn current_epoch_id(&self) -> EpochId {
        self.current_epoch.load(Ordering::Acquire)
    }

    /// Record an operation in the current epoch.
    ///
    /// Returns the epoch ID the operation was recorded in.
    pub fn record_operation(&self, wal_bytes: usize) -> EpochId {
        let epoch = self.current_epoch.load(Ordering::Acquire);
        self.current_ops.fetch_add(1, Ordering::Relaxed);
        self.current_wal_bytes.fetch_add(wal_bytes, Ordering::Relaxed);

        // Check if we should trigger epoch advance
        self.maybe_advance_epoch();

        epoch
    }

    /// Get the WAL writer for appending records.
    pub fn wal_writer(&self) -> impl std::ops::Deref<Target = Option<WalWriter>> + '_ {
        self.wal_writer.read().expect("wal lock poisoned")
    }

    /// Advance to a new epoch.
    pub fn advance_epoch(&self) -> Result<EpochId> {
        let old_epoch = self.current_epoch.load(Ordering::Acquire);
        let new_epoch = old_epoch + 1;

        // Seal the current epoch
        {
            let mut epochs = self.epochs.write().expect("epochs lock poisoned");
            if let Some(current) = epochs.back_mut() {
                if current.id == old_epoch {
                    current.state = EpochState::Sealing;
                    current.sealed_at = Some(SystemTime::now());
                    current.operation_count = self.current_ops.load(Ordering::Relaxed);
                    current.wal_size_bytes = self.current_wal_bytes.load(Ordering::Relaxed);
                }
            }
        }

        // Sync the current WAL
        {
            let mut wal = self.wal_writer.write().expect("wal lock poisoned");
            if let Some(ref mut w) = *wal {
                w.sync()?;
            }
        }

        // Open WAL for new epoch
        self.open_epoch_wal(new_epoch)?;

        // Update counters
        self.current_epoch.store(new_epoch, Ordering::Release);
        self.current_ops.store(0, Ordering::Relaxed);
        self.current_wal_bytes.store(0, Ordering::Relaxed);
        *self.epoch_start.write().expect("epoch_start lock") = Instant::now();

        // Add new epoch metadata
        {
            let mut epochs = self.epochs.write().expect("epochs lock");
            epochs.push_back(EpochMetadata {
                id: new_epoch,
                state: EpochState::Active,
                started_at: SystemTime::now(),
                sealed_at: None,
                checkpointed_at: None,
                operation_count: 0,
                wal_size_bytes: 0,
                first_lsn: 0, // Will be set on first operation
                last_lsn: 0,
            });

            // Trim old epochs beyond retention
            while epochs.len() > self.config.retention_epochs + 2 {
                epochs.pop_front();
            }
        }

        // Signal background thread to potentially checkpoint
        self.signal_checkpoint();

        Ok(new_epoch)
    }

    /// Perform a checkpoint of the specified epoch.
    pub fn checkpoint_epoch(&self, epoch_id: EpochId) -> Result<()> {
        // Implementation of checkpoint logic
        // This would serialize the trie state and write checkpoint file

        let checkpoint_path = self.checkpoint_path(epoch_id);

        // For now, just create the checkpoint metadata
        let meta = CheckpointMeta {
            epoch_id,
            checkpoint_lsn: 0, // Would be actual LSN
            timestamp: SystemTime::now(),
            checksum: 0, // Would be actual checksum
        };

        // Write checkpoint metadata
        self.write_checkpoint_meta(&meta)?;

        // Mark epoch as durable
        {
            let mut epochs = self.epochs.write().expect("epochs lock");
            for epoch in epochs.iter_mut() {
                if epoch.id == epoch_id {
                    epoch.state = EpochState::Durable;
                    epoch.checkpointed_at = Some(SystemTime::now());
                    break;
                }
            }
        }

        // Delete old WAL segments beyond retention
        self.cleanup_old_wals()?;

        // Update last checkpoint
        *self.last_checkpoint.write().expect("checkpoint lock") = Some(meta);

        Ok(())
    }

    /// Force a synchronous checkpoint of the current epoch.
    pub fn force_checkpoint(&self) -> Result<EpochId> {
        let epoch = self.advance_epoch()?;
        self.checkpoint_epoch(epoch.saturating_sub(1))?;
        Ok(epoch)
    }

    /// Get the last durable epoch (safe recovery point).
    pub fn last_durable_epoch(&self) -> Option<EpochId> {
        self.last_checkpoint.read().expect("lock")
            .as_ref()
            .map(|c| c.epoch_id)
    }

    // --- Private methods ---

    fn maybe_advance_epoch(&self) {
        let ops = self.current_ops.load(Ordering::Relaxed);
        let bytes = self.current_wal_bytes.load(Ordering::Relaxed);
        let elapsed = self.epoch_start.read().expect("lock").elapsed();

        let should_advance =
            ops >= self.config.max_ops_per_epoch ||
            bytes >= self.config.max_wal_size_bytes ||
            elapsed >= self.config.epoch_duration;

        if should_advance {
            // Use try_lock to avoid blocking hot path
            if let Ok(_guard) = self.checkpoint_thread.try_lock() {
                let _ = self.advance_epoch();
            }
        }
    }

    fn open_epoch_wal(&self, epoch: EpochId) -> Result<()> {
        let wal_path = self.wal_path(epoch);
        let writer = WalWriter::create(wal_path)?;

        let mut wal = self.wal_writer.write().expect("wal lock");
        *wal = Some(writer);

        Ok(())
    }

    fn wal_path(&self, epoch: EpochId) -> PathBuf {
        self.base_dir.join("wal").join(format!("epoch_{:016}.wal", epoch))
    }

    fn checkpoint_path(&self, epoch: EpochId) -> PathBuf {
        self.base_dir.join("checkpoint").join(format!("checkpoint_{:016}.snap", epoch))
    }

    fn checkpoint_meta_path(&self) -> PathBuf {
        self.base_dir.join("wal").join("checkpoint.meta")
    }

    fn load_checkpoint_meta(base_dir: &Path) -> Result<Option<CheckpointMeta>> {
        let path = base_dir.join("wal").join("checkpoint.meta");
        if !path.exists() {
            return Ok(None);
        }

        let mut file = File::open(path)?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        Ok(CheckpointMeta::deserialize(&data))
    }

    fn write_checkpoint_meta(&self, meta: &CheckpointMeta) -> Result<()> {
        let path = self.checkpoint_meta_path();
        let temp_path = path.with_extension("meta.tmp");

        // Write to temp file first
        {
            let mut file = File::create(&temp_path)?;
            file.write_all(&meta.serialize())?;
            file.sync_all()?;
        }

        // Atomic rename
        fs::rename(temp_path, path)?;

        Ok(())
    }

    fn cleanup_old_wals(&self) -> Result<()> {
        let last_durable = self.last_durable_epoch().unwrap_or(0);
        let cutoff = last_durable.saturating_sub(self.config.retention_epochs as u64);

        let wal_dir = self.base_dir.join("wal");
        for entry in fs::read_dir(&wal_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if name_str.starts_with("epoch_") && name_str.ends_with(".wal") {
                if let Some(epoch_str) = name_str
                    .strip_prefix("epoch_")
                    .and_then(|s| s.strip_suffix(".wal"))
                {
                    if let Ok(epoch) = epoch_str.parse::<EpochId>() {
                        if epoch < cutoff {
                            fs::remove_file(entry.path())?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn start_checkpoint_thread(&self) {
        let signal = Arc::clone(&self.checkpoint_signal);
        let shutdown = Arc::clone(&self.shutdown);
        let config = self.config.clone();

        let handle = thread::Builder::new()
            .name("artrie-epoch-checkpoint".to_string())
            .spawn(move || {
                Self::checkpoint_loop(signal, shutdown, config);
            })
            .expect("failed to spawn checkpoint thread");

        *self.checkpoint_thread.lock().expect("lock") = Some(handle);
    }

    fn checkpoint_loop(
        signal: Arc<(Mutex<bool>, Condvar)>,
        shutdown: Arc<AtomicBool>,
        config: EpochConfig,
    ) {
        let (lock, cvar) = &*signal;

        loop {
            // Wait for signal or timeout
            let mut triggered = lock.lock().expect("lock");
            let result = cvar.wait_timeout(triggered, config.epoch_duration)
                .expect("wait failed");
            triggered = result.0;
            *triggered = false;

            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            // Checkpoint logic would go here
            // In a real implementation, this would:
            // 1. Identify epochs that need checkpointing
            // 2. Call checkpoint_epoch() for each
        }
    }

    fn signal_checkpoint(&self) {
        let (lock, cvar) = &*self.checkpoint_signal;
        let mut triggered = lock.lock().expect("lock");
        *triggered = true;
        cvar.notify_one();
    }
}

impl Drop for EpochManager {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.signal_checkpoint();

        if let Some(handle) = self.checkpoint_thread.lock().expect("lock").take() {
            let _ = handle.join();
        }
    }
}
```

#### 3.5.2 Recovery Process

```rust
// src/persistent_artrie/recovery.rs (additions)

/// Recover from the last checkpoint using epoch-aware recovery.
///
/// This implements a simplified ARIES-style recovery [4]:
/// 1. Load the last checkpoint
/// 2. Replay WAL records from checkpoint LSN forward
/// 3. Apply only committed operations
pub fn recover_from_epochs(
    base_dir: &Path,
    trie: &mut PersistentARTrieInner,
) -> Result<RecoveryInfo> {
    // Load checkpoint metadata
    let checkpoint_meta = EpochManager::load_checkpoint_meta(base_dir)?;

    let (start_epoch, start_lsn) = match checkpoint_meta {
        Some(meta) => {
            // Load checkpoint data
            load_checkpoint(base_dir, meta.epoch_id, trie)?;
            (meta.epoch_id, meta.checkpoint_lsn)
        }
        None => {
            // No checkpoint, start from beginning
            (0, 0)
        }
    };

    // Find WAL files to replay
    let wal_dir = base_dir.join("wal");
    let mut wal_files: Vec<(EpochId, PathBuf)> = Vec::new();

    for entry in fs::read_dir(&wal_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.starts_with("epoch_") && name_str.ends_with(".wal") {
            if let Some(epoch_str) = name_str
                .strip_prefix("epoch_")
                .and_then(|s| s.strip_suffix(".wal"))
            {
                if let Ok(epoch) = epoch_str.parse::<EpochId>() {
                    if epoch >= start_epoch {
                        wal_files.push((epoch, entry.path()));
                    }
                }
            }
        }
    }

    // Sort by epoch
    wal_files.sort_by_key(|(epoch, _)| *epoch);

    // Replay WAL files
    let mut replayed_ops = 0;
    let mut last_lsn = start_lsn;

    for (epoch, wal_path) in wal_files {
        let reader = WalReader::open(&wal_path)?;

        for record_result in reader.iter() {
            let (lsn, record) = record_result?;

            // Skip records before checkpoint LSN
            if epoch == start_epoch && lsn <= start_lsn {
                continue;
            }

            // Apply record to trie
            apply_wal_record(trie, &record)?;
            replayed_ops += 1;
            last_lsn = lsn;
        }
    }

    Ok(RecoveryInfo {
        checkpoint_epoch: checkpoint_meta.map(|m| m.epoch_id),
        replayed_operations: replayed_ops,
        final_lsn: last_lsn,
        recovery_time: Duration::default(), // Would measure actual time
    })
}

/// Information about recovery process.
#[derive(Debug, Clone)]
pub struct RecoveryInfo {
    pub checkpoint_epoch: Option<EpochId>,
    pub replayed_operations: usize,
    pub final_lsn: Lsn,
    pub recovery_time: Duration,
}
```

### 3.6 Expected Outcomes

| Metric | Baseline | Expected | Measurement |
|--------|----------|----------|-------------|
| WAL size (unbounded workload) | Unbounded | ≤64MB (configurable) | File size |
| Recovery time (1M ops) | ~10s (full replay) | ~1s (from checkpoint) | Benchmark |
| Durability guarantee | Manual | ≤epoch_duration | Configuration |
| Checkpoint overhead | N/A | <5% throughput | Benchmark |
| Data loss window | 0 (with sync) | ≤2 epochs | By design |

### 3.7 Verification

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_epoch_advancement() {
        let dir = tempdir().unwrap();
        let config = EpochConfig {
            epoch_duration: Duration::from_millis(10),
            max_ops_per_epoch: 100,
            ..Default::default()
        };

        let manager = EpochManager::new(dir.path(), config).unwrap();

        // Record operations until epoch advances
        for _ in 0..150 {
            manager.record_operation(100);
        }

        // Epoch should have advanced
        assert!(manager.current_epoch_id() >= 1);
    }

    #[test]
    fn test_checkpoint_and_recovery() {
        let dir = tempdir().unwrap();
        let config = EpochConfig::default();

        // Write some data
        {
            let manager = EpochManager::new(dir.path(), config.clone()).unwrap();
            for _ in 0..1000 {
                manager.record_operation(100);
            }
            manager.force_checkpoint().unwrap();
        }

        // Recover
        let manager = EpochManager::new(dir.path(), config).unwrap();
        assert!(manager.last_durable_epoch().is_some());
    }
}
```

---

## 4. Experiment 3: Memory-Pressure-Aware Eviction

### 4.1 Hypothesis

Proactively flushing dirty pages when system memory pressure is detected will prevent OOM conditions and improve overall system stability without sacrificing performance under normal conditions.

### 4.2 Background: Linux Pressure Stall Information (PSI)

Linux 4.20+ provides Pressure Stall Information (PSI) [5], a kernel-level mechanism for detecting resource pressure:

```
/proc/pressure/memory contains:
some avg10=0.00 avg60=0.00 avg300=0.00 total=0
full avg10=0.00 avg60=0.00 avg300=0.00 total=0

Where:
- some: Percentage of time at least one task was stalled on memory
- full: Percentage of time ALL tasks were stalled on memory
- avg10/60/300: Moving averages over 10s/60s/300s windows
- total: Cumulative stall time in microseconds
```

**PSI Triggers**: Applications can register triggers to be notified when pressure exceeds thresholds:

```c
// Example: Trigger when memory stall exceeds 150ms in any 1s window
write(fd, "some 150000 1000000", 19);  // fd = open("/proc/pressure/memory")
poll(fd, POLLIN);  // Blocks until threshold exceeded
```

### 4.3 Design

```
┌─────────────────────────────────────────────────────────────────────────┐
│                     Memory Pressure Architecture                         │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                    Pressure Sources                               │    │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────────┐  │    │
│  │  │/proc/meminfo│  │PSI Triggers │  │ cgroup memory.pressure  │  │    │
│  │  │MemAvailable │  │(Linux 4.20+)│  │ (containerized)         │  │    │
│  │  └──────┬──────┘  └──────┬──────┘  └───────────┬─────────────┘  │    │
│  │         │                │                     │                 │    │
│  │         └────────────────┼─────────────────────┘                 │    │
│  │                          ▼                                       │    │
│  │              ┌───────────────────────┐                           │    │
│  │              │   Pressure Monitor    │                           │    │
│  │              │   (unified interface) │                           │    │
│  │              └───────────┬───────────┘                           │    │
│  └──────────────────────────┼───────────────────────────────────────┘    │
│                             │                                            │
│                             ▼                                            │
│  ┌──────────────────────────────────────────────────────────────────┐   │
│  │                    Pressure Level Classification                  │   │
│  │                                                                   │   │
│  │    ┌────────────┐    ┌────────────┐    ┌────────────────────┐    │   │
│  │    │   Normal   │    │    Low     │    │     Critical       │    │   │
│  │    │  > 30%     │    │  10-30%    │    │      < 10%         │    │   │
│  │    │ available  │    │ available  │    │    available       │    │   │
│  │    │            │    │            │    │                    │    │   │
│  │    │ No action  │    │ Evict 25%  │    │ Emergency flush    │    │   │
│  │    │            │    │ of dirty   │    │ all dirty pages    │    │   │
│  │    │            │    │ pages      │    │ + shrink pool      │    │   │
│  │    └────────────┘    └────────────┘    └────────────────────┘    │   │
│  └──────────────────────────────────────────────────────────────────┘   │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

### 4.4 Configuration Parameters

```rust
/// Configuration for memory pressure detection and response.
///
/// This module monitors system memory pressure using multiple sources:
/// - `/proc/meminfo` for basic memory statistics
/// - Linux PSI (Pressure Stall Information) for pressure events
/// - cgroup memory.pressure for containerized environments
///
/// # Response Levels
///
/// The system responds to memory pressure in three levels:
///
/// 1. **Normal** (>30% available): No action, full caching
/// 2. **Low** (10-30% available): Proactive eviction of dirty pages
/// 3. **Critical** (<10% available): Emergency flush, shrink buffer pool
///
/// # Platform Support
///
/// - **Linux 4.20+**: Full PSI support with efficient triggers
/// - **Linux <4.20**: Falls back to polling /proc/meminfo
/// - **macOS/Windows**: Uses platform-specific memory APIs
#[derive(Debug, Clone)]
pub struct MemoryPressureConfig {
    /// Polling interval for memory statistics.
    ///
    /// How often to check /proc/meminfo when PSI triggers are unavailable.
    /// Lower values give faster response but higher CPU overhead.
    ///
    /// Default: 1 second
    /// Range: 100ms - 60s
    pub poll_interval: Duration,

    /// Threshold for "low memory" state (fraction of total memory).
    ///
    /// When available memory drops below this fraction, proactive
    /// eviction begins.
    ///
    /// Default: 0.30 (30%)
    /// Range: 0.05 - 0.50
    pub low_memory_threshold: f64,

    /// Threshold for "critical memory" state (fraction of total memory).
    ///
    /// When available memory drops below this fraction, emergency
    /// measures are taken (flush all dirty pages, shrink pool).
    ///
    /// Default: 0.10 (10%)
    /// Range: 0.01 - 0.25
    pub critical_memory_threshold: f64,

    /// Fraction of dirty pages to evict when in low memory state.
    ///
    /// Default: 0.25 (evict 25% of dirty pages)
    /// Range: 0.10 - 1.0
    pub low_memory_evict_fraction: f64,

    /// Use PSI (Pressure Stall Information) if available.
    ///
    /// PSI provides more accurate and efficient pressure detection
    /// on Linux 4.20+. If unavailable, falls back to polling.
    ///
    /// Default: true
    pub use_psi: bool,

    /// PSI threshold for "some" pressure (microseconds per second).
    ///
    /// Trigger when any task is stalled for this duration within 1 second.
    ///
    /// Default: 50_000 (50ms per second)
    /// Range: 10_000 - 500_000
    pub psi_some_threshold_us: u64,

    /// PSI threshold for "full" pressure (microseconds per second).
    ///
    /// Trigger when all tasks are stalled for this duration within 1 second.
    ///
    /// Default: 10_000 (10ms per second)
    /// Range: 1_000 - 100_000
    pub psi_full_threshold_us: u64,

    /// PSI monitoring window (microseconds).
    ///
    /// The time window over which pressure is measured.
    ///
    /// Default: 1_000_000 (1 second)
    /// Range: 500_000 - 10_000_000
    pub psi_window_us: u64,

    /// Enable memory pressure monitoring.
    ///
    /// Set to false to disable all memory pressure handling.
    ///
    /// Default: true
    pub enabled: bool,

    /// Debounce duration for pressure level changes.
    ///
    /// Prevents rapid oscillation between pressure levels.
    ///
    /// Default: 500ms
    pub debounce_duration: Duration,
}

impl Default for MemoryPressureConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            low_memory_threshold: 0.30,
            critical_memory_threshold: 0.10,
            low_memory_evict_fraction: 0.25,
            use_psi: true,
            psi_some_threshold_us: 50_000,
            psi_full_threshold_us: 10_000,
            psi_window_us: 1_000_000,
            enabled: true,
            debounce_duration: Duration::from_millis(500),
        }
    }
}
```

### 4.5 Complete Implementation

```rust
// src/persistent_artrie/memory_monitor.rs

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Memory pressure levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum MemoryPressureLevel {
    /// Normal operation, >30% memory available.
    Normal = 0,

    /// Low memory, 10-30% available. Proactive eviction recommended.
    Low = 1,

    /// Critical memory, <10% available. Emergency measures required.
    Critical = 2,
}

impl From<u8> for MemoryPressureLevel {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Normal,
            1 => Self::Low,
            _ => Self::Critical,
        }
    }
}

/// Memory statistics from /proc/meminfo.
#[derive(Debug, Clone, Default)]
pub struct MemoryStats {
    /// Total system memory in bytes.
    pub mem_total: u64,

    /// Available memory in bytes (MemAvailable from kernel).
    pub mem_available: u64,

    /// Free memory in bytes (MemFree).
    pub mem_free: u64,

    /// Cached memory in bytes.
    pub cached: u64,

    /// Buffer memory in bytes.
    pub buffers: u64,

    /// Swap total in bytes.
    pub swap_total: u64,

    /// Swap used in bytes.
    pub swap_used: u64,
}

impl MemoryStats {
    /// Available memory as a fraction of total.
    pub fn available_fraction(&self) -> f64 {
        if self.mem_total == 0 {
            return 1.0;
        }
        self.mem_available as f64 / self.mem_total as f64
    }

    /// Whether swap is being used (early warning sign).
    pub fn is_swapping(&self) -> bool {
        self.swap_used > 0
    }
}

/// PSI (Pressure Stall Information) metrics.
#[derive(Debug, Clone, Default)]
pub struct PsiMetrics {
    /// Percentage of time some tasks were stalled (10s average).
    pub some_avg10: f64,

    /// Percentage of time some tasks were stalled (60s average).
    pub some_avg60: f64,

    /// Percentage of time all tasks were stalled (10s average).
    pub full_avg10: f64,

    /// Percentage of time all tasks were stalled (60s average).
    pub full_avg60: f64,

    /// Total stall time in microseconds.
    pub total_us: u64,
}

/// Callback type for pressure level changes.
pub type PressureCallback = Box<dyn Fn(MemoryPressureLevel, &MemoryStats) + Send + Sync>;

/// Memory pressure monitor.
///
/// Monitors system memory pressure and invokes callbacks when pressure
/// levels change. Uses Linux PSI when available for efficient detection.
///
/// # Example
///
/// ```rust,ignore
/// let config = MemoryPressureConfig::default();
/// let monitor = MemoryPressureMonitor::start(config, |level, stats| {
///     match level {
///         MemoryPressureLevel::Normal => {},
///         MemoryPressureLevel::Low => evict_some_pages(),
///         MemoryPressureLevel::Critical => emergency_flush(),
///     }
/// })?;
/// ```
pub struct MemoryPressureMonitor {
    /// Configuration.
    config: MemoryPressureConfig,

    /// Current pressure level (atomic for lock-free reads).
    current_level: Arc<AtomicU8>,

    /// Shutdown flag.
    shutdown: Arc<AtomicBool>,

    /// Monitor thread handle.
    monitor_thread: Option<JoinHandle<()>>,

    /// PSI trigger file descriptor (if using PSI).
    psi_fd: Option<i32>,
}

impl MemoryPressureMonitor {
    /// Start the memory pressure monitor.
    ///
    /// The callback will be invoked whenever the pressure level changes.
    pub fn start(
        config: MemoryPressureConfig,
        callback: impl Fn(MemoryPressureLevel, &MemoryStats) + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let current_level = Arc::new(AtomicU8::new(MemoryPressureLevel::Normal as u8));
        let shutdown = Arc::new(AtomicBool::new(false));

        // Try to set up PSI trigger
        let psi_fd = if config.use_psi {
            Self::setup_psi_trigger(&config).ok()
        } else {
            None
        };

        let monitor_thread = if config.enabled {
            let level_clone = Arc::clone(&current_level);
            let shutdown_clone = Arc::clone(&shutdown);
            let config_clone = config.clone();
            let callback = Arc::new(callback);

            Some(thread::Builder::new()
                .name("artrie-memory-monitor".to_string())
                .spawn(move || {
                    if psi_fd.is_some() {
                        Self::psi_monitor_loop(
                            psi_fd.unwrap(),
                            level_clone,
                            shutdown_clone,
                            config_clone,
                            callback,
                        );
                    } else {
                        Self::polling_monitor_loop(
                            level_clone,
                            shutdown_clone,
                            config_clone,
                            callback,
                        );
                    }
                })?)
        } else {
            None
        };

        Ok(Self {
            config,
            current_level,
            shutdown,
            monitor_thread,
            psi_fd,
        })
    }

    /// Get the current pressure level (lock-free).
    pub fn current_level(&self) -> MemoryPressureLevel {
        MemoryPressureLevel::from(self.current_level.load(Ordering::Relaxed))
    }

    /// Get current memory statistics.
    pub fn current_stats(&self) -> std::io::Result<MemoryStats> {
        Self::read_meminfo()
    }

    /// Check if PSI is being used.
    pub fn using_psi(&self) -> bool {
        self.psi_fd.is_some()
    }

    // --- Private methods ---

    fn setup_psi_trigger(config: &MemoryPressureConfig) -> std::io::Result<i32> {
        let psi_path = Path::new("/proc/pressure/memory");
        if !psi_path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "PSI not available (requires Linux 4.20+)",
            ));
        }

        // Open PSI file for trigger registration
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(psi_path)?;

        // Register trigger: "some <threshold_us> <window_us>"
        let trigger = format!(
            "some {} {}",
            config.psi_some_threshold_us,
            config.psi_window_us,
        );

        // Write trigger configuration
        let mut file = file;
        file.write_all(trigger.as_bytes())?;

        Ok(file.as_raw_fd())
    }

    fn psi_monitor_loop(
        psi_fd: i32,
        current_level: Arc<AtomicU8>,
        shutdown: Arc<AtomicBool>,
        config: MemoryPressureConfig,
        callback: Arc<dyn Fn(MemoryPressureLevel, &MemoryStats) + Send + Sync>,
    ) {
        use std::os::unix::io::FromRawFd;

        let mut last_level = MemoryPressureLevel::Normal;
        let mut last_change = Instant::now();

        // Use poll() to wait for PSI events
        let mut pollfd = libc::pollfd {
            fd: psi_fd,
            events: libc::POLLIN | libc::POLLPRI,
            revents: 0,
        };

        while !shutdown.load(Ordering::Relaxed) {
            // Poll with timeout
            let timeout_ms = config.poll_interval.as_millis() as i32;
            let ret = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };

            if ret < 0 {
                // Error, fall back to polling
                thread::sleep(config.poll_interval);
                continue;
            }

            // Check current memory status
            let stats = match Self::read_meminfo() {
                Ok(s) => s,
                Err(_) => continue,
            };

            let new_level = Self::classify_pressure(&stats, &config);

            // Debounce: only change level if stable
            if new_level != last_level {
                if last_change.elapsed() >= config.debounce_duration {
                    current_level.store(new_level as u8, Ordering::Relaxed);
                    callback(new_level, &stats);
                    last_level = new_level;
                    last_change = Instant::now();
                }
            } else {
                last_change = Instant::now();
            }
        }
    }

    fn polling_monitor_loop(
        current_level: Arc<AtomicU8>,
        shutdown: Arc<AtomicBool>,
        config: MemoryPressureConfig,
        callback: Arc<dyn Fn(MemoryPressureLevel, &MemoryStats) + Send + Sync>,
    ) {
        let mut last_level = MemoryPressureLevel::Normal;
        let mut last_change = Instant::now();

        while !shutdown.load(Ordering::Relaxed) {
            thread::sleep(config.poll_interval);

            let stats = match Self::read_meminfo() {
                Ok(s) => s,
                Err(_) => continue,
            };

            let new_level = Self::classify_pressure(&stats, &config);

            // Debounce
            if new_level != last_level {
                if last_change.elapsed() >= config.debounce_duration {
                    current_level.store(new_level as u8, Ordering::Relaxed);
                    callback(new_level, &stats);
                    last_level = new_level;
                    last_change = Instant::now();
                }
            } else {
                last_change = Instant::now();
            }
        }
    }

    fn classify_pressure(stats: &MemoryStats, config: &MemoryPressureConfig) -> MemoryPressureLevel {
        let available = stats.available_fraction();

        if available < config.critical_memory_threshold {
            MemoryPressureLevel::Critical
        } else if available < config.low_memory_threshold {
            MemoryPressureLevel::Low
        } else {
            MemoryPressureLevel::Normal
        }
    }

    fn read_meminfo() -> std::io::Result<MemoryStats> {
        let file = File::open("/proc/meminfo")?;
        let reader = BufReader::new(file);

        let mut stats = MemoryStats::default();

        for line in reader.lines() {
            let line = line?;
            let parts: Vec<&str> = line.split_whitespace().collect();

            if parts.len() < 2 {
                continue;
            }

            let value_kb: u64 = parts[1].parse().unwrap_or(0);
            let value_bytes = value_kb * 1024;

            match parts[0] {
                "MemTotal:" => stats.mem_total = value_bytes,
                "MemAvailable:" => stats.mem_available = value_bytes,
                "MemFree:" => stats.mem_free = value_bytes,
                "Cached:" => stats.cached = value_bytes,
                "Buffers:" => stats.buffers = value_bytes,
                "SwapTotal:" => stats.swap_total = value_bytes,
                "SwapFree:" => {
                    stats.swap_used = stats.swap_total.saturating_sub(value_bytes);
                }
                _ => {}
            }
        }

        // If MemAvailable not present (old kernels), estimate it
        if stats.mem_available == 0 {
            stats.mem_available = stats.mem_free + stats.cached + stats.buffers;
        }

        Ok(stats)
    }

    /// Read PSI metrics from /proc/pressure/memory.
    pub fn read_psi() -> std::io::Result<PsiMetrics> {
        let content = fs::read_to_string("/proc/pressure/memory")?;
        let mut metrics = PsiMetrics::default();

        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            let is_some = parts[0] == "some";
            let is_full = parts[0] == "full";

            for part in &parts[1..] {
                if let Some((key, value)) = part.split_once('=') {
                    let v: f64 = value.parse().unwrap_or(0.0);
                    match (is_some, is_full, key) {
                        (true, _, "avg10") => metrics.some_avg10 = v,
                        (true, _, "avg60") => metrics.some_avg60 = v,
                        (_, true, "avg10") => metrics.full_avg10 = v,
                        (_, true, "avg60") => metrics.full_avg60 = v,
                        (true, _, "total") => metrics.total_us = v as u64,
                        _ => {}
                    }
                }
            }
        }

        Ok(metrics)
    }
}

impl Drop for MemoryPressureMonitor {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);

        if let Some(handle) = self.monitor_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Integration with BufferManager for pressure-aware eviction.
impl BufferManager {
    /// Handle a memory pressure event.
    ///
    /// Called by the memory pressure monitor when pressure level changes.
    pub fn on_memory_pressure(&self, level: MemoryPressureLevel, stats: &MemoryStats) {
        match level {
            MemoryPressureLevel::Normal => {
                // No action needed
            }
            MemoryPressureLevel::Low => {
                // Evict some dirty pages proactively
                let dirty_count = self.dirty_page_count();
                let to_evict = (dirty_count as f64 * 0.25) as usize;
                self.evict_pages(to_evict, /* prefer_clean */ false);
            }
            MemoryPressureLevel::Critical => {
                // Emergency: flush all dirty pages
                let _ = self.flush_all();

                // Also shrink the pool if possible
                self.shrink_pool_emergency();
            }
        }
    }
}
```

### 4.6 Expected Outcomes

| Metric | Baseline | Expected | Measurement |
|--------|----------|----------|-------------|
| OOM kills | Possible under pressure | None | System logs |
| Memory utilization | Fixed pool | Adaptive to pressure | `/proc/meminfo` |
| Latency spike under pressure | Very high (OOM) | Moderate (controlled flush) | Histogram |
| System responsiveness | Poor under pressure | Stable | Observation |
| Monitoring overhead | N/A | <0.1% CPU | `perf` |

### 4.7 Verification

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_meminfo_parsing() {
        let stats = MemoryPressureMonitor::read_meminfo().unwrap();
        assert!(stats.mem_total > 0);
        assert!(stats.mem_available > 0);
        assert!(stats.available_fraction() > 0.0);
        assert!(stats.available_fraction() <= 1.0);
    }

    #[test]
    fn test_pressure_classification() {
        let config = MemoryPressureConfig::default();

        // Normal: 50% available
        let stats = MemoryStats {
            mem_total: 100,
            mem_available: 50,
            ..Default::default()
        };
        assert_eq!(
            MemoryPressureMonitor::classify_pressure(&stats, &config),
            MemoryPressureLevel::Normal
        );

        // Low: 20% available
        let stats = MemoryStats {
            mem_total: 100,
            mem_available: 20,
            ..Default::default()
        };
        assert_eq!(
            MemoryPressureMonitor::classify_pressure(&stats, &config),
            MemoryPressureLevel::Low
        );

        // Critical: 5% available
        let stats = MemoryStats {
            mem_total: 100,
            mem_available: 5,
            ..Default::default()
        };
        assert_eq!(
            MemoryPressureMonitor::classify_pressure(&stats, &config),
            MemoryPressureLevel::Critical
        );
    }

    #[test]
    #[ignore] // Requires Linux with PSI support
    fn test_psi_reading() {
        if Path::new("/proc/pressure/memory").exists() {
            let psi = MemoryPressureMonitor::read_psi().unwrap();
            // PSI values should be non-negative
            assert!(psi.some_avg10 >= 0.0);
            assert!(psi.full_avg10 >= 0.0);
        }
    }
}
```

---

## 5. Experiment 4: Adaptive Buffer Pool Sizing

### 5.1 Hypothesis

Dynamically adjusting buffer pool size based on available system memory and workload characteristics will improve cache hit rates and reduce I/O without risking OOM.

### 5.2 Design: PID Controller for Pool Sizing

We use a PID (Proportional-Integral-Derivative) controller to adaptively size the buffer pool:

```
                    ┌─────────────────────────────────────────┐
                    │        Adaptive Pool Controller         │
                    ├─────────────────────────────────────────┤
                    │                                         │
                    │  Inputs:                                │
┌──────────────┐    │  ├─ Available memory (target: 25%)     │
│   Memory     │───►│  ├─ Cache hit rate (target: 95%)       │
│   Stats      │    │  ├─ Recent I/O operations              │
└──────────────┘    │  └─ Memory pressure level              │
                    │                                         │
┌──────────────┐    │  Error = target_hit_rate - actual      │
│  Hit Rate    │───►│                                        │
│  Counters    │    │  PID Control:                          │
└──────────────┘    │  ├─ P: Kp * error                      │
                    │  ├─ I: Ki * integral(error)            │
                    │  └─ D: Kd * derivative(error)          │
┌──────────────┐    │                                         │
│   Pressure   │───►│  Output: Δpool_size                    │
│   Level      │    │                                         │
└──────────────┘    └──────────────────┬──────────────────────┘
                                       │
                                       ▼
                    ┌─────────────────────────────────────────┐
                    │            Pool Resizer                 │
                    │                                         │
                    │  if Δpool_size > 0: grow_pool(Δ)       │
                    │  if Δpool_size < 0: shrink_pool(-Δ)    │
                    │                                         │
                    │  Constraints:                           │
                    │  ├─ min_pool_size ≤ size ≤ max_pool   │
                    │  ├─ max_growth_step per adjustment     │
                    │  └─ Respect memory pressure level      │
                    └─────────────────────────────────────────┘
```

### 5.3 Configuration Parameters

```rust
/// Configuration for adaptive buffer pool sizing.
///
/// The adaptive pool controller uses a PID-like algorithm to adjust
/// the buffer pool size based on cache hit rate and available memory.
///
/// # Algorithm
///
/// Every `adjustment_interval`, the controller:
/// 1. Measures current cache hit rate and available memory
/// 2. Computes error from target values
/// 3. Adjusts pool size proportionally (bounded by step limits)
///
/// # Stability
///
/// The controller includes safeguards against oscillation:
/// - Maximum step sizes limit rapid changes
/// - Hysteresis zone around target prevents thrashing
/// - Memory pressure overrides normal growth
#[derive(Debug, Clone)]
pub struct AdaptivePoolConfig {
    /// Minimum pool size in frames.
    ///
    /// The pool will never shrink below this size.
    ///
    /// Default: 16 (4MB with 256KB frames)
    /// Range: 4 - 1024
    pub min_pool_size: usize,

    /// Maximum pool size in frames.
    ///
    /// The pool will never grow beyond this size, even if memory
    /// is available. Set based on workload requirements.
    ///
    /// Default: 1024 (256MB with 256KB frames)
    /// Range: 16 - 65536
    pub max_pool_size: usize,

    /// Target fraction of available system memory to use.
    ///
    /// The controller tries to keep pool size at this fraction
    /// of available memory. Lower values leave more headroom
    /// for other applications.
    ///
    /// Default: 0.25 (25%)
    /// Range: 0.05 - 0.50
    pub target_memory_fraction: f64,

    /// Target cache hit rate.
    ///
    /// The controller will grow the pool if hit rate falls below
    /// this target (and memory is available).
    ///
    /// Default: 0.95 (95%)
    /// Range: 0.50 - 0.99
    pub target_hit_rate: f64,

    /// Hit rate threshold below which growth is considered.
    ///
    /// Pool only grows if hit rate is below this AND below target.
    /// This creates a hysteresis zone to prevent oscillation.
    ///
    /// Default: 0.90 (90%)
    pub min_hit_rate_for_growth: f64,

    /// Interval between pool size adjustments.
    ///
    /// Default: 10 seconds
    /// Range: 1s - 60s
    pub adjustment_interval: Duration,

    /// Maximum frames to add per adjustment.
    ///
    /// Limits how fast the pool can grow.
    ///
    /// Default: 16
    /// Range: 1 - 256
    pub max_growth_step: usize,

    /// Maximum frames to remove per adjustment.
    ///
    /// Limits how fast the pool can shrink (slower than growth
    /// to avoid thrashing).
    ///
    /// Default: 8
    /// Range: 1 - 128
    pub max_shrink_step: usize,

    /// Proportional gain for PID controller.
    ///
    /// Higher values give faster response but may cause oscillation.
    ///
    /// Default: 0.5
    pub kp: f64,

    /// Integral gain for PID controller.
    ///
    /// Helps eliminate steady-state error. Set to 0 to disable.
    ///
    /// Default: 0.1
    pub ki: f64,

    /// Derivative gain for PID controller.
    ///
    /// Dampens oscillation. Set to 0 to disable.
    ///
    /// Default: 0.05
    pub kd: f64,

    /// Enable adaptive sizing.
    ///
    /// Set to false to use a fixed pool size.
    ///
    /// Default: true
    pub enabled: bool,
}

impl Default for AdaptivePoolConfig {
    fn default() -> Self {
        Self {
            min_pool_size: 16,
            max_pool_size: 1024,
            target_memory_fraction: 0.25,
            target_hit_rate: 0.95,
            min_hit_rate_for_growth: 0.90,
            adjustment_interval: Duration::from_secs(10),
            max_growth_step: 16,
            max_shrink_step: 8,
            kp: 0.5,
            ki: 0.1,
            kd: 0.05,
            enabled: true,
        }
    }
}
```

### 5.4 Complete Implementation

```rust
// src/persistent_artrie/adaptive_pool.rs

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::buffer_manager::BufferManager;
use super::memory_monitor::{MemoryPressureLevel, MemoryPressureMonitor, MemoryStats};

/// Cache hit/miss counters.
#[derive(Debug, Default)]
pub struct CacheStats {
    hits: AtomicU64,
    misses: AtomicU64,
    last_reset: RwLock<Instant>,
}

impl CacheStats {
    pub fn new() -> Self {
        Self {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            last_reset: RwLock::new(Instant::now()),
        }
    }

    pub fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Get hit rate and reset counters.
    pub fn get_and_reset(&self) -> (f64, u64, u64) {
        let hits = self.hits.swap(0, Ordering::Relaxed);
        let misses = self.misses.swap(0, Ordering::Relaxed);
        *self.last_reset.write().expect("lock") = Instant::now();

        let total = hits + misses;
        let hit_rate = if total == 0 {
            1.0 // No accesses means no misses
        } else {
            hits as f64 / total as f64
        };

        (hit_rate, hits, misses)
    }

    /// Get current hit rate without resetting.
    pub fn hit_rate(&self) -> f64 {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;

        if total == 0 {
            1.0
        } else {
            hits as f64 / total as f64
        }
    }
}

/// PID controller state.
#[derive(Debug)]
struct PidController {
    /// Configuration.
    kp: f64,
    ki: f64,
    kd: f64,

    /// Integral accumulator.
    integral: f64,

    /// Previous error for derivative calculation.
    prev_error: f64,

    /// Anti-windup: clamp integral term.
    integral_min: f64,
    integral_max: f64,
}

impl PidController {
    fn new(kp: f64, ki: f64, kd: f64) -> Self {
        Self {
            kp,
            ki,
            kd,
            integral: 0.0,
            prev_error: 0.0,
            integral_min: -100.0,
            integral_max: 100.0,
        }
    }

    fn compute(&mut self, error: f64, dt: f64) -> f64 {
        // Proportional term
        let p = self.kp * error;

        // Integral term with anti-windup
        self.integral += error * dt;
        self.integral = self.integral.clamp(self.integral_min, self.integral_max);
        let i = self.ki * self.integral;

        // Derivative term
        let d = if dt > 0.0 {
            self.kd * (error - self.prev_error) / dt
        } else {
            0.0
        };
        self.prev_error = error;

        p + i + d
    }

    fn reset(&mut self) {
        self.integral = 0.0;
        self.prev_error = 0.0;
    }
}

/// Adaptive buffer pool controller.
pub struct AdaptivePoolController {
    /// Configuration.
    config: AdaptivePoolConfig,

    /// Buffer manager reference.
    buffer_manager: Arc<BufferManager>,

    /// Cache statistics.
    cache_stats: Arc<CacheStats>,

    /// Memory pressure monitor.
    memory_monitor: Arc<MemoryPressureMonitor>,

    /// Current pool size.
    current_size: AtomicUsize,

    /// PID controller for hit rate targeting.
    pid: RwLock<PidController>,

    /// Controller thread.
    controller_thread: Option<JoinHandle<()>>,

    /// Shutdown flag.
    shutdown: Arc<AtomicBool>,

    /// Last adjustment time.
    last_adjustment: RwLock<Instant>,
}

impl AdaptivePoolController {
    /// Create a new adaptive pool controller.
    pub fn new(
        config: AdaptivePoolConfig,
        buffer_manager: Arc<BufferManager>,
        cache_stats: Arc<CacheStats>,
        memory_monitor: Arc<MemoryPressureMonitor>,
    ) -> Self {
        let initial_size = buffer_manager.pool_size();

        let controller = Self {
            config: config.clone(),
            buffer_manager,
            cache_stats,
            memory_monitor,
            current_size: AtomicUsize::new(initial_size),
            pid: RwLock::new(PidController::new(config.kp, config.ki, config.kd)),
            controller_thread: None,
            shutdown: Arc::new(AtomicBool::new(false)),
            last_adjustment: RwLock::new(Instant::now()),
        };

        controller
    }

    /// Start the controller background thread.
    pub fn start(&mut self) {
        if !self.config.enabled {
            return;
        }

        let config = self.config.clone();
        let buffer_manager = Arc::clone(&self.buffer_manager);
        let cache_stats = Arc::clone(&self.cache_stats);
        let memory_monitor = Arc::clone(&self.memory_monitor);
        let current_size = self.current_size.load(Ordering::Relaxed);
        let shutdown = Arc::clone(&self.shutdown);

        self.controller_thread = Some(thread::Builder::new()
            .name("artrie-adaptive-pool".to_string())
            .spawn(move || {
                Self::control_loop(
                    config,
                    buffer_manager,
                    cache_stats,
                    memory_monitor,
                    current_size,
                    shutdown,
                );
            })
            .expect("failed to spawn controller thread"));
    }

    /// Get current pool size.
    pub fn pool_size(&self) -> usize {
        self.current_size.load(Ordering::Relaxed)
    }

    /// Force a pool size adjustment.
    pub fn adjust_now(&self) {
        let (hit_rate, _, _) = self.cache_stats.get_and_reset();
        let memory_stats = self.memory_monitor.current_stats()
            .unwrap_or_default();
        let pressure = self.memory_monitor.current_level();

        self.do_adjustment(hit_rate, &memory_stats, pressure);
    }

    fn control_loop(
        config: AdaptivePoolConfig,
        buffer_manager: Arc<BufferManager>,
        cache_stats: Arc<CacheStats>,
        memory_monitor: Arc<MemoryPressureMonitor>,
        mut current_size: usize,
        shutdown: Arc<AtomicBool>,
    ) {
        let mut pid = PidController::new(config.kp, config.ki, config.kd);
        let mut last_time = Instant::now();

        while !shutdown.load(Ordering::Relaxed) {
            thread::sleep(config.adjustment_interval);

            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            // Collect metrics
            let (hit_rate, hits, misses) = cache_stats.get_and_reset();
            let memory_stats = memory_monitor.current_stats().unwrap_or_default();
            let pressure = memory_monitor.current_level();

            // Calculate time delta
            let now = Instant::now();
            let dt = now.duration_since(last_time).as_secs_f64();
            last_time = now;

            // Skip if no activity
            if hits + misses == 0 {
                continue;
            }

            // Calculate target size based on available memory
            let available_memory = memory_stats.mem_available as f64;
            let frame_size = 256 * 1024; // 256KB
            let memory_target_size = ((available_memory * config.target_memory_fraction) / frame_size as f64) as usize;

            // Calculate adjustment based on hit rate
            let hit_rate_error = config.target_hit_rate - hit_rate;
            let pid_output = pid.compute(hit_rate_error, dt);

            // Convert PID output to size change
            let size_delta = (pid_output * current_size as f64) as isize;

            // Apply constraints
            let new_size = match pressure {
                MemoryPressureLevel::Critical => {
                    // Emergency: shrink to minimum
                    config.min_pool_size
                }
                MemoryPressureLevel::Low => {
                    // Under pressure: don't grow, may shrink
                    let shrink = (size_delta.min(0).abs() as usize).min(config.max_shrink_step);
                    current_size.saturating_sub(shrink).max(config.min_pool_size)
                }
                MemoryPressureLevel::Normal => {
                    // Normal: apply PID control
                    if size_delta > 0 && hit_rate < config.min_hit_rate_for_growth {
                        // Grow (bounded)
                        let grow = (size_delta as usize).min(config.max_growth_step);
                        (current_size + grow).min(config.max_pool_size).min(memory_target_size)
                    } else if size_delta < 0 {
                        // Shrink (bounded)
                        let shrink = (size_delta.abs() as usize).min(config.max_shrink_step);
                        current_size.saturating_sub(shrink).max(config.min_pool_size)
                    } else {
                        current_size
                    }
                }
            };

            // Apply size change
            if new_size != current_size {
                if new_size > current_size {
                    buffer_manager.grow_pool(new_size - current_size);
                } else {
                    buffer_manager.shrink_pool(current_size - new_size);
                }
                current_size = new_size;
            }
        }
    }

    fn do_adjustment(&self, hit_rate: f64, memory_stats: &MemoryStats, pressure: MemoryPressureLevel) {
        // Implementation similar to control_loop but for single adjustment
        // ...
    }
}

impl Drop for AdaptivePoolController {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);

        if let Some(handle) = self.controller_thread.take() {
            let _ = handle.join();
        }
    }
}
```

### 5.5 Expected Outcomes

| Metric | Baseline (fixed) | Expected (adaptive) | Measurement |
|--------|-----------------|---------------------|-------------|
| Cache hit rate | ~80% | ~95% | Counter ratio |
| Memory utilization | Fixed | Up to 25% of available | Pool size |
| Read I/O | High | Reduced 2-3x | `perf stat` |
| Adaptation latency | N/A | <30s to steady state | Time series |
| Memory pressure handling | Poor | Graceful degradation | Observation |

---

## 6. Experiment 5: Per-Node Logging

### 6.1 Hypothesis

Storing redo information per-node (instead of a global WAL) will enable near-instant recovery by replaying only the logs of nodes that were dirty at crash time.

### 6.2 Background: Bw-Tree Delta Records

The Bw-Tree [3] pioneered the use of delta records for lock-free tree updates:

```
Traditional Update:
┌──────────────────┐
│ Node (Page)      │
│ ┌──────────────┐ │     1. Lock node
│ │ Key1 -> V1   │ │     2. Modify in-place
│ │ Key2 -> V2   │ │     3. Unlock
│ │ Key3 -> V3   │ │
│ └──────────────┘ │
└──────────────────┘

Bw-Tree Delta Update:
┌──────────────────┐
│ Delta Record     │──┐
│ +Key4 -> V4      │  │
└──────────────────┘  │
                      ▼
┌──────────────────┐
│ Delta Record     │──┐
│ -Key2            │  │  No locks needed!
└──────────────────┘  │  CAS on pointer
                      ▼
┌──────────────────┐
│ Base Node        │
│ Key1 -> V1       │
│ Key2 -> V2       │
│ Key3 -> V3       │
└──────────────────┘

Reading: Traverse delta chain and merge with base
Consolidation: Periodically merge deltas into new base
```

### 6.3 Our Approach: Hybrid Per-Node Logging

We combine the Bw-Tree approach with virtualized per-page logging [2]:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    Per-Node Log Architecture                             │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ART Node with Inline Log:                                               │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │ Header (16 bytes)                                                  │  │
│  │ ┌─────────────┬─────────────┬───────────────┬──────────────────┐  │  │
│  │ │ node_type   │ prefix_len  │ log_offset    │ log_length       │  │  │
│  │ │ (1 byte)    │ (1 byte)    │ (2 bytes)     │ (2 bytes)        │  │  │
│  │ └─────────────┴─────────────┴───────────────┴──────────────────┘  │  │
│  │                                                                    │  │
│  │ Base Data (variable, ~200 bytes for Node16)                        │  │
│  │ ┌─────────────────────────────────────────────────────────────┐   │  │
│  │ │ keys[], children[], prefix[], value (if leaf)               │   │  │
│  │ └─────────────────────────────────────────────────────────────┘   │  │
│  │                                                                    │  │
│  │ Inline Log (up to 64 bytes)                                        │  │
│  │ ┌─────────────────────────────────────────────────────────────┐   │  │
│  │ │ [+key1, +key2, -key3, ...]                                  │   │  │
│  │ └─────────────────────────────────────────────────────────────┘   │  │
│  │                                                                    │  │
│  │ Overflow Pointer (if log > 64 bytes)                               │  │
│  │ ┌─────────────────────────────────────────────────────────────┐   │  │
│  │ │ overflow_page_id (8 bytes)                                  │   │  │
│  │ └───────────────────────────────────────────────────────────────┘  │  │
│  └───────────────────────────────────────────────────────────────────┘  │
│                                                                          │
│  Overflow Log Page (4KB):                                                │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │ Page Header                                                        │  │
│  │ ┌─────────────┬─────────────┬───────────────────────────────────┐ │  │
│  │ │ magic       │ node_id     │ log_entries[]                     │ │  │
│  │ └─────────────┴─────────────┴───────────────────────────────────┘ │  │
│  └───────────────────────────────────────────────────────────────────┘  │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

### 6.4 Configuration Parameters

```rust
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
/// | Space overhead | Separate file | Inline + overflow |
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
    /// Default: number of CPU cores
    /// Range: 1 - 256
    pub recovery_threads: usize,

    /// Track dirty nodes for recovery.
    ///
    /// Maintains a persistent set of nodes with non-empty logs.
    /// Enables O(dirty) recovery instead of O(all).
    ///
    /// Default: true
    pub track_dirty_nodes: bool,
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
            recovery_threads: num_cpus::get(),
            track_dirty_nodes: true,
        }
    }
}
```

### 6.5 Implementation Sketch

```rust
// src/persistent_artrie/per_node_log.rs

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

/// Log entry types for per-node logging.
#[derive(Debug, Clone)]
pub enum NodeLogEntry {
    /// Insert a child edge.
    InsertChild {
        key: u8,
        child_id: NodeId,
    },

    /// Remove a child edge.
    RemoveChild {
        key: u8,
    },

    /// Update the node's value (for leaf nodes).
    SetValue {
        value: Vec<u8>,
    },

    /// Clear the node's value.
    ClearValue,

    /// Update prefix.
    SetPrefix {
        prefix: Vec<u8>,
    },
}

impl NodeLogEntry {
    /// Serialize the log entry.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            Self::InsertChild { key, child_id } => {
                buf.push(0x01);
                buf.push(*key);
                buf.extend_from_slice(&child_id.to_le_bytes());
            }
            Self::RemoveChild { key } => {
                buf.push(0x02);
                buf.push(*key);
            }
            Self::SetValue { value } => {
                buf.push(0x03);
                buf.extend_from_slice(&(value.len() as u16).to_le_bytes());
                buf.extend_from_slice(value);
            }
            Self::ClearValue => {
                buf.push(0x04);
            }
            Self::SetPrefix { prefix } => {
                buf.push(0x05);
                buf.push(prefix.len() as u8);
                buf.extend_from_slice(prefix);
            }
        }
        buf
    }

    /// Deserialize a log entry.
    pub fn deserialize(data: &[u8]) -> Option<(Self, usize)> {
        if data.is_empty() {
            return None;
        }

        match data[0] {
            0x01 => {
                // InsertChild
                if data.len() < 10 {
                    return None;
                }
                let key = data[1];
                let child_id = u64::from_le_bytes(data[2..10].try_into().ok()?);
                Some((Self::InsertChild { key, child_id }, 10))
            }
            0x02 => {
                // RemoveChild
                if data.len() < 2 {
                    return None;
                }
                let key = data[1];
                Some((Self::RemoveChild { key }, 2))
            }
            0x03 => {
                // SetValue
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
            0x04 => {
                Some((Self::ClearValue, 1))
            }
            0x05 => {
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
}

/// Per-node log manager.
pub struct PerNodeLogManager {
    /// Configuration.
    config: PerNodeLogConfig,

    /// Set of dirty node IDs (nodes with non-empty logs).
    dirty_nodes: RwLock<HashSet<NodeId>>,

    /// Overflow pages for large logs.
    overflow_allocator: Arc<dyn OverflowAllocator>,
}

impl PerNodeLogManager {
    /// Append a log entry to a node.
    pub fn append_log(&self, node: &mut dyn NodeWithLog, entry: NodeLogEntry) -> Result<()> {
        let serialized = entry.serialize();

        // Try inline first
        if node.inline_log_space() >= serialized.len() {
            node.append_inline_log(&serialized);
        } else {
            // Overflow to separate page
            self.append_overflow(node, &serialized)?;
        }

        // Track dirty node
        {
            let mut dirty = self.dirty_nodes.write().expect("lock");
            dirty.insert(node.id());
        }

        // Check if compaction needed
        if self.should_compact(node) {
            self.schedule_compaction(node.id());
        }

        Ok(())
    }

    /// Replay a node's log to reconstruct its state.
    pub fn replay_log(&self, node: &mut dyn NodeWithLog) -> Result<()> {
        // Read inline log
        let mut log_data = node.inline_log().to_vec();

        // Read overflow if present
        if let Some(overflow_id) = node.overflow_page_id() {
            let overflow = self.overflow_allocator.read(overflow_id)?;
            log_data.extend_from_slice(&overflow);
        }

        // Parse and apply entries
        let mut offset = 0;
        while offset < log_data.len() {
            if let Some((entry, len)) = NodeLogEntry::deserialize(&log_data[offset..]) {
                self.apply_entry(node, &entry)?;
                offset += len;
            } else {
                break;
            }
        }

        // Clear log after replay
        node.clear_log();

        Ok(())
    }

    /// Compact a node by merging its log into the base.
    pub fn compact_node(&self, node: &mut dyn NodeWithLog) -> Result<()> {
        // Replay log to update base
        self.replay_log(node)?;

        // Free overflow page if any
        if let Some(overflow_id) = node.overflow_page_id() {
            self.overflow_allocator.free(overflow_id)?;
            node.set_overflow_page_id(None);
        }

        // Remove from dirty set
        {
            let mut dirty = self.dirty_nodes.write().expect("lock");
            dirty.remove(&node.id());
        }

        Ok(())
    }

    /// Parallel recovery of all dirty nodes.
    pub fn recover_parallel(&self, nodes: &mut [Box<dyn NodeWithLog>]) -> Result<RecoveryStats> {
        use rayon::prelude::*;

        let dirty: HashSet<NodeId> = self.dirty_nodes.read().expect("lock").clone();

        let results: Vec<Result<()>> = nodes
            .par_iter_mut()
            .filter(|n| dirty.contains(&n.id()))
            .map(|node| self.replay_log(node.as_mut()))
            .collect();

        let mut recovered = 0;
        let mut errors = 0;

        for result in results {
            match result {
                Ok(()) => recovered += 1,
                Err(_) => errors += 1,
            }
        }

        Ok(RecoveryStats { recovered, errors })
    }

    fn should_compact(&self, node: &dyn NodeWithLog) -> bool {
        let log_size = node.inline_log().len() + node.overflow_size();
        let base_size = node.base_size();

        log_size >= self.config.max_log_size ||
        (base_size > 0 && log_size as f64 / base_size as f64 >= self.config.compaction_threshold)
    }

    fn apply_entry(&self, node: &mut dyn NodeWithLog, entry: &NodeLogEntry) -> Result<()> {
        match entry {
            NodeLogEntry::InsertChild { key, child_id } => {
                node.insert_child(*key, *child_id);
            }
            NodeLogEntry::RemoveChild { key } => {
                node.remove_child(*key);
            }
            NodeLogEntry::SetValue { value } => {
                node.set_value(value.clone());
            }
            NodeLogEntry::ClearValue => {
                node.clear_value();
            }
            NodeLogEntry::SetPrefix { prefix } => {
                node.set_prefix(prefix.clone());
            }
        }
        Ok(())
    }

    fn append_overflow(&self, node: &mut dyn NodeWithLog, data: &[u8]) -> Result<()> {
        let overflow_id = node.overflow_page_id().unwrap_or_else(|| {
            let id = self.overflow_allocator.allocate().expect("allocate overflow");
            node.set_overflow_page_id(Some(id));
            id
        });

        self.overflow_allocator.append(overflow_id, data)?;
        Ok(())
    }

    fn schedule_compaction(&self, node_id: NodeId) {
        // Would queue for background compaction
        // For now, this is a no-op placeholder
    }
}

/// Trait for nodes that support per-node logging.
pub trait NodeWithLog {
    fn id(&self) -> NodeId;
    fn inline_log(&self) -> &[u8];
    fn inline_log_space(&self) -> usize;
    fn append_inline_log(&mut self, data: &[u8]);
    fn clear_log(&mut self);
    fn overflow_page_id(&self) -> Option<PageId>;
    fn set_overflow_page_id(&mut self, id: Option<PageId>);
    fn overflow_size(&self) -> usize;
    fn base_size(&self) -> usize;

    fn insert_child(&mut self, key: u8, child_id: NodeId);
    fn remove_child(&mut self, key: u8);
    fn set_value(&mut self, value: Vec<u8>);
    fn clear_value(&mut self);
    fn set_prefix(&mut self, prefix: Vec<u8>);
}

/// Recovery statistics.
#[derive(Debug, Clone, Default)]
pub struct RecoveryStats {
    pub recovered: usize,
    pub errors: usize,
}
```

### 6.6 Expected Outcomes

| Metric | Baseline (Global WAL) | Expected (Per-Node) | Measurement |
|--------|----------------------|---------------------|-------------|
| Recovery time (1M ops, 10K dirty) | ~10s (full replay) | <1s (10K nodes only) | Benchmark |
| Recovery parallelism | 1 thread | N threads | CPU utilization |
| Write amplification | ~2x (data + WAL) | ~1.5x (inline log) | I/O bytes |
| Space overhead | Separate WAL file | ~10% inline | File size |
| Complexity | Moderate | High | Code metrics |

---

## 7. Benchmarking Methodology

### 7.1 Hardware Configuration

Reference: `/home/dylon/.claude/hardware-specifications.md`

**CPU Affinity**: Pin benchmark processes to specific cores
```bash
taskset -c 0-7 ./benchmark  # Use cores 0-7
```

**CPU Frequency**: Set to maximum for consistent results
```bash
sudo cpupower frequency-set -g performance
```

**Disable Turbo Boost** (for consistency):
```bash
echo 1 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo
```

### 7.2 Benchmark Workloads

Following YCSB (Yahoo! Cloud Serving Benchmark) patterns:

| Workload | Read % | Update % | Insert % | Distribution | Use Case |
|----------|--------|----------|----------|--------------|----------|
| YCSB-A | 50% | 50% | 0% | Zipfian | Session store |
| YCSB-B | 95% | 5% | 0% | Zipfian | Photo tagging |
| YCSB-C | 100% | 0% | 0% | Zipfian | Cache |
| YCSB-D | 95% | 0% | 5% | Latest | User status |
| YCSB-E | 0% | 0% | 5% + 95% scan | Zipfian | Threaded discussions |
| YCSB-F | 50% | 50% RMW | 0% | Zipfian | User database |

### 7.3 Metrics Collection

```rust
/// Comprehensive benchmark metrics.
#[derive(Debug, Clone, Default)]
pub struct BenchmarkMetrics {
    // === Throughput ===
    /// Operations per second.
    pub ops_per_second: f64,

    /// Reads per second.
    pub reads_per_second: f64,

    /// Writes per second.
    pub writes_per_second: f64,

    // === Latency (microseconds) ===
    pub latency_min: u64,
    pub latency_p50: u64,
    pub latency_p90: u64,
    pub latency_p99: u64,
    pub latency_p999: u64,
    pub latency_max: u64,
    pub latency_mean: f64,
    pub latency_stddev: f64,

    // === I/O ===
    /// Disk reads (count).
    pub disk_reads: u64,

    /// Disk writes (count).
    pub disk_writes: u64,

    /// fsync calls.
    pub fsync_calls: u64,

    /// Bytes read from disk.
    pub bytes_read: u64,

    /// Bytes written to disk.
    pub bytes_written: u64,

    // === Memory ===
    /// Peak RSS in bytes.
    pub peak_memory_bytes: usize,

    /// Buffer pool size.
    pub buffer_pool_size: usize,

    /// Cache hit rate.
    pub cache_hit_rate: f64,

    // === Recovery ===
    /// Recovery time in milliseconds.
    pub recovery_time_ms: u64,

    /// WAL size in bytes.
    pub wal_size_bytes: usize,

    /// Operations replayed during recovery.
    pub recovery_ops_replayed: u64,

    // === Group Commit ===
    /// Average batch size.
    pub avg_batch_size: f64,

    /// fsync efficiency (ops per fsync).
    pub fsync_efficiency: f64,
}
```

### 7.4 Profiling Commands

```bash
# CPU profiling
perf record -g --call-graph dwarf ./benchmark
perf report --hierarchy

# I/O and syscall counting
perf stat -e syscalls:sys_enter_fsync,syscalls:sys_enter_write,syscalls:sys_enter_read ./benchmark

# Memory profiling
valgrind --tool=massif ./benchmark
ms_print massif.out.*

# Flame graph generation
perf record -F 99 -g ./benchmark
perf script | stackcollapse-perf.pl | flamegraph.pl > flamegraph.svg
```

### 7.5 Statistical Rigor

- Run each benchmark **10+ times**
- Report: **mean, stddev, min, max, median**
- Use **warm-up period**: discard first 10% of samples
- **Control for filesystem caching**:
  ```bash
  echo 3 | sudo tee /proc/sys/vm/drop_caches
  sync
  ```
- Use **CPU isolation**:
  ```bash
  # In /etc/default/grub:
  GRUB_CMDLINE_LINUX="isolcpus=8-15"
  ```
- **Disable ASLR** for reproducibility:
  ```bash
  echo 0 | sudo tee /proc/sys/kernel/randomize_va_space
  ```

---

## 8. Implementation Order and Dependencies

### 8.1 Dependency Graph

```
                    ┌─────────────────┐
                    │ 1. Group Commit │
                    │                 │
                    │ Foundation for  │
                    │ all durability  │
                    └────────┬────────┘
                             │
              ┌──────────────┼──────────────┐
              ▼              ▼              │
┌─────────────────┐  ┌─────────────────┐   │
│ 2. Epoch-Based  │  │ 3. Memory       │   │
│    Checkpoints  │  │    Pressure     │   │
│                 │  │    Monitor      │   │
│ Uses group      │  │                 │   │
│ commit for WAL  │  │ Independent     │   │
└────────┬────────┘  └────────┬────────┘   │
         │                    │            │
         │           ┌────────┴────────┐   │
         │           ▼                 │   │
         │   ┌─────────────────┐       │   │
         │   │ 4. Adaptive     │◄──────┘   │
         │   │    Pool Sizing  │           │
         │   │                 │           │
         │   │ Requires memory │           │
         │   │ pressure info   │           │
         │   └────────┬────────┘           │
         │            │                    │
         └────────────┼────────────────────┘
                      ▼
              ┌─────────────────┐
              │ 5. Per-Node     │
              │    Logging      │
              │                 │
              │ Can replace     │
              │ global WAL      │
              │ (most complex)  │
              └─────────────────┘
```

### 8.2 Recommended Implementation Order

| Phase | Experiment | Dependencies | Rationale |
|-------|------------|--------------|-----------|
| 1 | Group Commit | None | Foundational, highest ROI |
| 2 | Epoch-Based Checkpoints | Group Commit | Bounds WAL, auto-durability |
| 3 | Memory Pressure Monitor | None | Independent, improves stability |
| 4 | Adaptive Pool Sizing | Memory Pressure | Requires pressure signals |
| 5 | Per-Node Logging | All above | Most complex, can be deferred |

### 8.3 Milestones

**Milestone 1: Basic Durability Improvements (Experiments 1-2)**
- [ ] Group commit implemented and tested
- [ ] Epoch-based checkpointing working
- [ ] Benchmark shows 2x+ write throughput improvement
- [ ] WAL size bounded under continuous load

**Milestone 2: Memory Adaptivity (Experiments 3-4)**
- [ ] Memory pressure detection working on Linux
- [ ] PSI triggers functional (Linux 4.20+)
- [ ] Adaptive pool sizing implemented
- [ ] System stable under memory pressure (cgroup tests)
- [ ] Cache hit rate improved to 95%+

**Milestone 3: Advanced Recovery (Experiment 5)**
- [ ] Per-node logging design finalized
- [ ] Implementation complete with inline + overflow
- [ ] Parallel recovery implemented
- [ ] Recovery time <1s for 1M operations with 10K dirty nodes
- [ ] Comprehensive crash testing passed

---

## 9. References

### Academic Papers

1. **[1] BD+Tree (SIGMOD 2024)**: Du, M., & Scott, M. L. "Buffered Persistence in B+ Trees"
   - Paper: https://www.cs.rochester.edu/u/scott/papers/2025_SIGMOD_BD+Tree.pdf
   - ACM DL: https://dl.acm.org/doi/10.1145/3698801
   - Key contribution: Epoch-based relaxed persistence, 2.4x throughput improvement

2. **[2] Per-Page Logging (VLDB 2024)**: "Breathing New Life into An Old Tree: Resolving Logging Dilemma of Persistent B+-tree on Modern NVM"
   - Paper: https://www.vldb.org/pvldb/vol17/p134-huang.pdf
   - Key contribution: Virtualized data pages with per-page logs, near-instant recovery

3. **[3] Bw-Tree (ICDE 2013)**: Levandoski, J., Lomet, D., & Sengupta, S. "The Bw-Tree: A B-tree for New Hardware Platforms"
   - Paper: https://15721.courses.cs.cmu.edu/spring2016/papers/bwtree-icde2013.pdf
   - Microsoft Research: https://www.microsoft.com/en-us/research/publication/bw-tree-latch-free-b-tree-log-structured-flash-storage/
   - Key contribution: Lock-free B-tree with delta records

4. **[4] ARIES (TODS 1992)**: Mohan, C., et al. "ARIES: A Transaction Recovery Method Supporting Fine-Granularity Locking and Partial Rollbacks"
   - Paper: https://web.stanford.edu/class/cs345d-01/rl/aries.pdf
   - Overview: https://blog.acolyer.org/2016/01/08/aries/
   - Key contribution: Industry-standard WAL and recovery algorithm

5. **[5] Linux PSI**: Facebook. "Pressure Stall Information"
   - Kernel docs: https://docs.kernel.org/accounting/psi.html
   - Getting started: https://facebookmicrosites.github.io/psi/docs/overview
   - LWN article: https://lwn.net/Articles/759781/

6. **[6] PostgreSQL WAL**: PostgreSQL Documentation
   - WAL Configuration: https://www.postgresql.org/docs/current/wal-configuration.html
   - Runtime Config: https://www.postgresql.org/docs/current/runtime-config-wal.html
   - Group Commit Wiki: https://wiki.postgresql.org/wiki/Group_commit

7. **[7] PostgreSQL Asynchronous Commit**: PostgreSQL Documentation
   - https://www.postgresql.org/docs/current/wal-async-commit.html

8. **[8] MySQL Group Commit**: MySQL Documentation and Worklogs
   - Worklog: https://dev.mysql.com/worklog/task/?id=5223
   - Analysis: https://summerxwu.me/posts/mysql_group_commit/

9. **[9] RocksDB**: Facebook RocksDB Wiki
   - Tuning Guide: https://github.com/facebook/rocksdb/wiki/RocksDB-Tuning-Guide
   - Transactions: https://github.com/facebook/rocksdb/wiki/WritePrepared-Transactions

10. **BzTree (VLDB 2018)**: Arulraj, J., et al. "BzTree: A High-Performance Latch-Free Range Index for Non-Volatile Memory"
    - Paper: https://dl.acm.org/doi/10.1145/3164135.3164147
    - Key contribution: PMwCAS-based latch-free B-tree

11. **B-Tree Logging Survey (TODS 2012)**: Graefe, G. "A Survey of B-Tree Logging and Recovery Techniques"
    - Paper: https://dl.acm.org/doi/abs/10.1145/2109196.2109197

12. **LSM-Tree Survey (2024)**: "A Survey of LSM-Tree based Indexes, Data Systems and KV-stores"
    - Paper: https://arxiv.org/html/2402.10460v2

### Implementation References

- **SQLite WAL**: https://www.sqlite.org/wal.html
- **SQLite Durability**: https://www.agwa.name/blog/post/sqlite_durability
- **LevelDB Design**: https://github.com/google/leveldb/blob/main/doc/impl.md
- **Martin Fowler - WAL Pattern**: https://martinfowler.com/articles/patterns-of-distributed-systems/write-ahead-log.html
- **Raft Consensus**: https://raft.github.io/

### Rust Crates

| Crate | Purpose | Version |
|-------|---------|---------|
| `crossbeam-channel` | Lock-free MPMC channels for group commit | ^0.5 |
| `parking_lot` | Fast synchronization primitives | ^0.12 |
| `criterion` | Benchmarking framework | ^0.5 |
| `rayon` | Parallel iterators for recovery | ^1.8 |
| `sysinfo` | Cross-platform system information | ^0.30 |
| `num_cpus` | Detect number of CPU cores | ^1.16 |
| `libc` | FFI bindings for PSI/poll | ^0.2 |
| `tempfile` | Temporary files for testing | ^3.9 |

### Cargo.toml Additions

```toml
[dependencies]
crossbeam-channel = "0.5"
parking_lot = "0.12"
rayon = "1.8"
sysinfo = "0.30"
num_cpus = "1.16"
libc = "0.2"

[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
tempfile = "3.9"
rand = "0.8"

[[bench]]
name = "group_commit"
harness = false

[[bench]]
name = "recovery"
harness = false

[[bench]]
name = "adaptive_pool"
harness = false
```

---

*Document created: 2026-01-10*
*Last updated: 2026-01-10*
*Status: Planning phase - experiments not yet implemented*
*Author: Claude (Anthropic)*
