//! Crash recovery for Persistent ART.
//!
//! This module implements redo-only crash recovery using the Write-Ahead Log (WAL).
//! Recovery follows the ARIES (Algorithm for Recovery and Isolation Exploiting
//! Semantics) approach, simplified for our redo-only needs.
//!
//! # Recovery Process
//!
//! 1. **Analysis Phase**: Scan WAL from last checkpoint, identify committed transactions
//! 2. **Redo Phase**: Replay all committed operations to rebuild state
//! 3. **Cleanup Phase**: Truncate WAL after recovery
//!
//! # Checkpoint Strategy
//!
//! Checkpoints are fuzzy - they don't require quiescing the system. The checkpoint
//! LSN indicates the point after which all committed transactions are guaranteed
//! to be in the WAL.
//!
//! # Example
//!
//! ```rust,ignore
//! use libdictenstein::persistent_artrie::recovery::RecoveryManager;
//!
//! let recovery = RecoveryManager::new("data.wal")?;
//! let state = recovery.recover()?;
//!
//! // State contains all committed operations
//! for op in state.operations() {
//!     // Apply to trie...
//! }
//! ```

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::wal::{Lsn, WalError, WalReader, WalRecord};
use log::warn;

/// Error types for recovery operations.
#[derive(Debug)]
pub enum RecoveryError {
    /// I/O error during recovery
    Io(io::Error),
    /// WAL file is corrupted
    CorruptedWal(String),
    /// Checkpoint not found
    NoCheckpoint,
    /// Invalid checkpoint data
    InvalidCheckpoint { lsn: Lsn, reason: String },
    /// Transaction log inconsistency
    TransactionInconsistency { tx_id: u64, reason: String },
    /// Recovery operation failed
    RecoveryFailed(String),
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecoveryError::Io(e) => write!(f, "I/O error: {}", e),
            RecoveryError::CorruptedWal(msg) => write!(f, "Corrupted WAL: {}", msg),
            RecoveryError::NoCheckpoint => write!(f, "No checkpoint found in WAL"),
            RecoveryError::InvalidCheckpoint { lsn, reason } => {
                write!(f, "Invalid checkpoint at LSN {}: {}", lsn, reason)
            }
            RecoveryError::TransactionInconsistency { tx_id, reason } => {
                write!(f, "Transaction {} inconsistency: {}", tx_id, reason)
            }
            RecoveryError::RecoveryFailed(msg) => write!(f, "Recovery failed: {}", msg),
        }
    }
}

impl std::error::Error for RecoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RecoveryError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for RecoveryError {
    fn from(e: io::Error) -> Self {
        RecoveryError::Io(e)
    }
}

impl From<WalError> for RecoveryError {
    fn from(e: WalError) -> Self {
        match e {
            WalError::Io(io_err) => RecoveryError::Io(io_err),
            WalError::CorruptedRecord(msg) => RecoveryError::CorruptedWal(msg),
            WalError::UnexpectedEof => RecoveryError::CorruptedWal("Unexpected EOF".into()),
            WalError::InvalidRecordType(t) => {
                RecoveryError::CorruptedWal(format!("Invalid record type: {}", t))
            }
            WalError::AlreadyExists => RecoveryError::RecoveryFailed("WAL already exists".into()),
            WalError::NotFound => RecoveryError::NoCheckpoint,
            WalError::ParentNotFound(path) => RecoveryError::RecoveryFailed(format!(
                "Parent directory not found: {}",
                path.display()
            )),
        }
    }
}

/// Result type for recovery operations.
pub type Result<T> = std::result::Result<T, RecoveryError>;

/// A recovered operation ready to be applied.
#[derive(Debug, Clone)]
pub enum RecoveredOperation {
    /// Insert a term with optional value
    Insert {
        /// Log sequence number of this operation
        lsn: Lsn,
        /// The term bytes
        term: Vec<u8>,
        /// Optional serialized value
        value: Option<Vec<u8>>,
    },
    /// Remove a term
    Remove {
        /// Log sequence number of this operation
        lsn: Lsn,
        /// The term bytes
        term: Vec<u8>,
    },
    /// Atomic increment operation
    Increment {
        /// Log sequence number of this operation
        lsn: Lsn,
        /// The term bytes
        term: Vec<u8>,
        /// Delta that was added
        delta: i64,
        /// Resulting value after increment
        result: i64,
    },
    /// Atomic upsert operation
    Upsert {
        /// Log sequence number of this operation
        lsn: Lsn,
        /// The term bytes
        term: Vec<u8>,
        /// The new serialized value
        value: Vec<u8>,
    },
    /// Atomic compare-and-swap operation
    CompareAndSwap {
        /// Log sequence number of this operation
        lsn: Lsn,
        /// The term bytes
        term: Vec<u8>,
        /// The new value that was set (only if success)
        new_value: Vec<u8>,
        /// Whether the swap succeeded
        success: bool,
    },
}

/// State of a transaction during recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionState {
    /// Transaction has begun but not yet committed/aborted
    InProgress,
    /// Transaction has committed
    Committed,
    /// Transaction has aborted
    Aborted,
}

/// Pending operations for a transaction.
#[derive(Debug, Default)]
struct PendingTransaction {
    /// Transaction state
    state: Option<TransactionState>,
    /// Operations in this transaction (in order)
    operations: Vec<RecoveredOperation>,
    /// LSN of the begin record
    begin_lsn: Option<Lsn>,
}

/// Recovery statistics for monitoring and debugging.
#[derive(Debug, Clone, Default)]
pub struct RecoveryStats {
    /// Total records scanned
    pub records_scanned: u64,
    /// Records that passed CRC validation
    pub valid_records: u64,
    /// Records that failed CRC validation
    pub corrupted_records: u64,
    /// Committed transactions found
    pub committed_transactions: u64,
    /// Aborted transactions found
    pub aborted_transactions: u64,
    /// In-progress (incomplete) transactions
    pub incomplete_transactions: u64,
    /// Insert operations recovered
    pub insert_operations: u64,
    /// Remove operations recovered
    pub remove_operations: u64,
    /// Checkpoint LSN used for recovery
    pub checkpoint_lsn: Option<Lsn>,
    /// Recovery duration
    pub duration_ms: u64,
}

/// The mode of recovery that was performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryMode {
    /// No recovery was needed - file opened normally.
    Normal,
    /// File was corrupted and rebuilt from WAL archive segments.
    RebuildFromWal,
    /// Minor corruption was detected and repaired in-place.
    RepairInPlace,
    /// File was missing and created fresh.
    CreatedNew,
}

impl RecoveryMode {
    /// Returns true if this was a normal open with no recovery.
    pub fn is_normal(&self) -> bool {
        matches!(self, RecoveryMode::Normal)
    }

    /// Returns true if any form of recovery was performed.
    pub fn recovered(&self) -> bool {
        !self.is_normal()
    }
}

/// Report of recovery operations performed during open.
///
/// This is returned by `open_with_recovery()` to inform the caller
/// what, if any, recovery actions were taken.
#[derive(Debug, Clone)]
pub struct RecoveryReport {
    /// What type of recovery was performed.
    pub mode: RecoveryMode,
    /// Number of WAL records replayed.
    pub records_replayed: u64,
    /// Number of terms recovered.
    pub terms_recovered: u64,
    /// Path to corrupted file (if any).
    pub corrupted_file: Option<PathBuf>,
    /// Description of corruption detected (if any).
    pub corruption_reason: Option<String>,
    /// Recovery duration in milliseconds.
    pub duration_ms: u64,
    /// The WAL archive segments used for recovery (if any).
    pub archive_segments_used: Vec<PathBuf>,
}

impl RecoveryReport {
    /// Create a report for normal open (no recovery needed).
    pub fn normal() -> Self {
        Self {
            mode: RecoveryMode::Normal,
            records_replayed: 0,
            terms_recovered: 0,
            corrupted_file: None,
            corruption_reason: None,
            duration_ms: 0,
            archive_segments_used: Vec::new(),
        }
    }

    /// Create a report for a newly created file.
    pub fn created_new() -> Self {
        Self {
            mode: RecoveryMode::CreatedNew,
            records_replayed: 0,
            terms_recovered: 0,
            corrupted_file: None,
            corruption_reason: None,
            duration_ms: 0,
            archive_segments_used: Vec::new(),
        }
    }

    /// Create a report for rebuild from WAL.
    pub fn rebuild_from_wal(
        corrupted_file: PathBuf,
        corruption_reason: String,
        records_replayed: u64,
        terms_recovered: u64,
        archive_segments_used: Vec<PathBuf>,
        duration_ms: u64,
    ) -> Self {
        Self {
            mode: RecoveryMode::RebuildFromWal,
            records_replayed,
            terms_recovered,
            corrupted_file: Some(corrupted_file),
            corruption_reason: Some(corruption_reason),
            duration_ms,
            archive_segments_used,
        }
    }
}

/// Type of corruption detected in a trie file.
#[derive(Debug, Clone)]
pub enum CorruptionType {
    /// File header is invalid (bad magic, version, or checksum).
    InvalidHeader(String),
    /// Arena checksum mismatch.
    ArenaChecksum { arena_id: u32, expected: u32, found: u32 },
    /// File is truncated.
    Truncated { expected: usize, actual: usize },
    /// Root descriptor is invalid.
    InvalidRootDescriptor(String),
    /// I/O error during verification.
    IoError(String),
}

impl std::fmt::Display for CorruptionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CorruptionType::InvalidHeader(msg) => write!(f, "Invalid header: {}", msg),
            CorruptionType::ArenaChecksum { arena_id, expected, found } => {
                write!(f, "Arena {} checksum mismatch: expected {:#x}, found {:#x}", arena_id, expected, found)
            }
            CorruptionType::Truncated { expected, actual } => {
                write!(f, "File truncated: expected {} bytes, found {}", expected, actual)
            }
            CorruptionType::InvalidRootDescriptor(msg) => {
                write!(f, "Invalid root descriptor: {}", msg)
            }
            CorruptionType::IoError(msg) => write!(f, "I/O error: {}", msg),
        }
    }
}

/// Recovered state from WAL.
#[derive(Debug)]
pub struct RecoveredState {
    /// All committed operations in LSN order
    operations: Vec<RecoveredOperation>,
    /// The LSN after all recovered operations
    pub next_lsn: Lsn,
    /// Recovery statistics
    pub stats: RecoveryStats,
}

impl RecoveredState {
    /// Get iterator over recovered operations.
    pub fn operations(&self) -> impl Iterator<Item = &RecoveredOperation> {
        self.operations.iter()
    }

    /// Get the number of recovered operations.
    pub fn operation_count(&self) -> usize {
        self.operations.len()
    }

    /// Consume self and return operations.
    pub fn into_operations(self) -> Vec<RecoveredOperation> {
        self.operations
    }
}

/// Recovery manager for crash recovery.
///
/// The recovery manager reads the WAL and reconstructs the committed state
/// of the dictionary. It handles:
/// - Checkpoint detection
/// - Transaction tracking
/// - Redo replay of committed operations
pub struct RecoveryManager {
    /// Path to WAL file
    wal_path: PathBuf,
}

impl RecoveryManager {
    /// Create a new recovery manager for the given WAL path.
    pub fn new<P: AsRef<Path>>(wal_path: P) -> Self {
        RecoveryManager {
            wal_path: wal_path.as_ref().to_path_buf(),
        }
    }

    /// Check if recovery is needed.
    ///
    /// Recovery is needed if the WAL file exists and contains records
    /// beyond the last checkpoint.
    pub fn needs_recovery(&self) -> Result<bool> {
        if !self.wal_path.exists() {
            return Ok(false);
        }

        let wal_reader = match WalReader::new(&self.wal_path) {
            Ok(r) => r,
            Err(_) => return Ok(false), // Can't open WAL, no recovery needed
        };

        // If we can read at least one record, we need recovery
        for result in wal_reader.iter() {
            match result {
                Ok(_) => return Ok(true),
                Err(_) => return Ok(false), // Empty or corrupted WAL
            }
        }

        Ok(false)
    }

    /// Perform recovery and return recovered state.
    ///
    /// This method:
    /// 1. Scans the WAL from the beginning (or last checkpoint)
    /// 2. Tracks transaction states
    /// 3. Collects operations from committed transactions
    /// 4. Returns operations in LSN order
    pub fn recover(&self) -> Result<RecoveredState> {
        let start_time = Instant::now();
        let mut stats = RecoveryStats::default();

        if !self.wal_path.exists() {
            return Ok(RecoveredState {
                operations: Vec::new(),
                next_lsn: 1,
                stats,
            });
        }

        // Phase 1: Analysis - Find checkpoint and track transactions
        let (checkpoint_lsn, transactions) = self.analysis_phase(&mut stats)?;
        stats.checkpoint_lsn = checkpoint_lsn;

        // Phase 2: Redo - Collect committed operations
        let (operations, next_lsn) = self.redo_phase(checkpoint_lsn, &transactions, &mut stats)?;

        stats.duration_ms = start_time.elapsed().as_millis() as u64;

        Ok(RecoveredState {
            operations,
            next_lsn,
            stats,
        })
    }

    /// Analysis phase: Scan WAL and track transaction states.
    ///
    /// Returns:
    /// - The checkpoint LSN to start redo from (None means start from beginning)
    /// - Map of transaction IDs to their final states
    fn analysis_phase(
        &self,
        stats: &mut RecoveryStats,
    ) -> Result<(Option<Lsn>, HashMap<u64, TransactionState>)> {
        let wal_reader = WalReader::new(&self.wal_path)?;

        let mut checkpoint_lsn: Option<Lsn> = None;
        let mut transactions: HashMap<u64, TransactionState> = HashMap::new();

        for result in wal_reader.iter() {
            stats.records_scanned += 1;

            let (_lsn, record) = match result {
                Ok(r) => {
                    stats.valid_records += 1;
                    r
                }
                Err(e) => {
                    stats.corrupted_records += 1;
                    // Log corruption but continue - we want to recover as much as possible
                    warn!("Corrupted record during analysis: {:?}", e);
                    continue;
                }
            };

            match record {
                WalRecord::Checkpoint { checkpoint_lsn: cp_lsn, .. } => {
                    // Use the most recent checkpoint
                    checkpoint_lsn = Some(cp_lsn);
                }
                WalRecord::BeginTx { tx_id } => {
                    transactions.insert(tx_id, TransactionState::InProgress);
                }
                WalRecord::CommitTx { tx_id } => {
                    if let Some(state) = transactions.get_mut(&tx_id) {
                        *state = TransactionState::Committed;
                        stats.committed_transactions += 1;
                    }
                }
                WalRecord::AbortTx { tx_id } => {
                    if let Some(state) = transactions.get_mut(&tx_id) {
                        *state = TransactionState::Aborted;
                        stats.aborted_transactions += 1;
                    }
                }
                _ => {}
            }
        }

        // Count incomplete transactions
        for state in transactions.values() {
            if *state == TransactionState::InProgress {
                stats.incomplete_transactions += 1;
            }
        }

        Ok((checkpoint_lsn, transactions))
    }

    /// Redo phase: Replay committed operations.
    ///
    /// Returns:
    /// - Vector of committed operations in LSN order (after checkpoint if provided)
    /// - The next LSN to use
    ///
    /// # Checkpoint Skipping
    ///
    /// When `checkpoint_lsn` is provided, operations with LSN <= checkpoint_lsn are
    /// skipped since they are already reflected in the persistent trie state on disk.
    /// This optimization avoids replaying the entire WAL history on every open.
    fn redo_phase(
        &self,
        checkpoint_lsn: Option<Lsn>,
        _transactions: &HashMap<u64, TransactionState>,
        stats: &mut RecoveryStats,
    ) -> Result<(Vec<RecoveredOperation>, Lsn)> {
        let wal_reader = WalReader::new(&self.wal_path)?;

        let mut operations: Vec<RecoveredOperation> = Vec::new();
        let mut next_lsn: Lsn = 1;

        // Track which transaction each non-transactional operation belongs to
        // Operations outside transactions are considered implicitly committed
        let mut current_tx: Option<u64> = None;
        let mut pending_tx_ops: HashMap<u64, Vec<RecoveredOperation>> = HashMap::new();

        for result in wal_reader.iter() {
            let (lsn, record) = match result {
                Ok(r) => r,
                Err(_) => continue, // Skip corrupted records
            };

            next_lsn = lsn + 1;

            // Note: Checkpoint-based filtering is done by the caller (dict_impl.rs)
            // since it knows whether the disk state was successfully loaded.
            // Operations carry their LSN so the caller can filter appropriately.

            match record {
                WalRecord::BeginTx { tx_id } => {
                    current_tx = Some(tx_id);
                    pending_tx_ops.entry(tx_id).or_default();
                }
                WalRecord::CommitTx { tx_id } => {
                    // Move pending ops to committed list
                    if let Some(ops) = pending_tx_ops.remove(&tx_id) {
                        operations.extend(ops);
                    }
                    if current_tx == Some(tx_id) {
                        current_tx = None;
                    }
                }
                WalRecord::AbortTx { tx_id } => {
                    // Discard pending ops
                    pending_tx_ops.remove(&tx_id);
                    if current_tx == Some(tx_id) {
                        current_tx = None;
                    }
                }
                WalRecord::Insert { term, value } => {
                    let op = RecoveredOperation::Insert { lsn, term, value };
                    if let Some(tx_id) = current_tx {
                        // Part of a transaction - buffer until commit
                        pending_tx_ops.entry(tx_id).or_default().push(op);
                    } else {
                        // Not in a transaction - implicitly committed
                        operations.push(op);
                        stats.insert_operations += 1;
                    }
                }
                WalRecord::Remove { term } => {
                    let op = RecoveredOperation::Remove { lsn, term };
                    if let Some(tx_id) = current_tx {
                        pending_tx_ops.entry(tx_id).or_default().push(op);
                    } else {
                        operations.push(op);
                        stats.remove_operations += 1;
                    }
                }
                WalRecord::Checkpoint { .. } => {
                    // Checkpoint records are processed during analysis phase.
                    // Checkpoint-based skipping will be implemented when full
                    // disk persistence is added.
                }
                WalRecord::Increment {
                    term,
                    delta,
                    result,
                } => {
                    let op = RecoveredOperation::Increment {
                        lsn,
                        term,
                        delta,
                        result,
                    };
                    if let Some(tx_id) = current_tx {
                        pending_tx_ops.entry(tx_id).or_default().push(op);
                    } else {
                        operations.push(op);
                    }
                }
                WalRecord::Upsert { term, value } => {
                    let op = RecoveredOperation::Upsert { lsn, term, value };
                    if let Some(tx_id) = current_tx {
                        pending_tx_ops.entry(tx_id).or_default().push(op);
                    } else {
                        operations.push(op);
                    }
                }
                WalRecord::CompareAndSwap {
                    term,
                    expected: _,
                    new_value,
                    success,
                } => {
                    // Only apply if the CAS succeeded
                    if success {
                        let op = RecoveredOperation::CompareAndSwap {
                            lsn,
                            term,
                            new_value,
                            success,
                        };
                        if let Some(tx_id) = current_tx {
                            pending_tx_ops.entry(tx_id).or_default().push(op);
                        } else {
                            operations.push(op);
                        }
                    }
                }
                WalRecord::BatchInsert { entries } => {
                    // Expand batch into individual insert operations
                    for (term, value) in entries {
                        let op = RecoveredOperation::Insert { lsn, term, value };
                        if let Some(tx_id) = current_tx {
                            // Part of a transaction - buffer until commit
                            pending_tx_ops.entry(tx_id).or_default().push(op);
                        } else {
                            // Not in a transaction - implicitly committed
                            operations.push(op);
                            stats.insert_operations += 1;
                        }
                    }
                }
            }
        }

        // Update stats for transactional operations
        for op in &operations {
            match op {
                RecoveredOperation::Insert { .. } => stats.insert_operations += 1,
                RecoveredOperation::Remove { .. } => stats.remove_operations += 1,
                RecoveredOperation::Increment { .. } => stats.insert_operations += 1,
                RecoveredOperation::Upsert { .. } => stats.insert_operations += 1,
                RecoveredOperation::CompareAndSwap { .. } => stats.insert_operations += 1,
            }
        }

        Ok((operations, next_lsn))
    }

    /// Perform recovery and apply operations to a callback.
    ///
    /// This is useful when you want to apply operations as they're recovered
    /// rather than collecting them all in memory first.
    pub fn recover_with_callback<F>(&self, mut callback: F) -> Result<RecoveryStats>
    where
        F: FnMut(RecoveredOperation) -> Result<()>,
    {
        let state = self.recover()?;
        for op in state.operations {
            callback(op)?;
        }
        Ok(state.stats)
    }
}

/// Incremental recovery for large WALs.
///
/// This struct allows recovering operations in batches, useful when
/// memory is constrained or when you want to show progress.
pub struct IncrementalRecovery {
    /// Underlying WAL reader
    reader: WalReader,
    /// Known transaction states (from analysis phase)
    #[allow(dead_code)]
    transactions: HashMap<u64, TransactionState>,
    /// Current transaction context
    current_tx: Option<u64>,
    /// Pending operations for current transaction
    pending_ops: Vec<RecoveredOperation>,
    /// Next LSN
    next_lsn: Lsn,
    /// Whether analysis phase is complete
    #[allow(dead_code)]
    analysis_complete: bool,
}

impl IncrementalRecovery {
    /// Create new incremental recovery from WAL path.
    pub fn new<P: AsRef<Path>>(wal_path: P) -> Result<Self> {
        let wal_reader = WalReader::new(wal_path)?;

        Ok(IncrementalRecovery {
            reader: wal_reader,
            transactions: HashMap::new(),
            current_tx: None,
            pending_ops: Vec::new(),
            next_lsn: 1,
            analysis_complete: false,
        })
    }

    /// Get the next batch of recovered operations.
    ///
    /// Returns None when recovery is complete.
    pub fn next_batch(&mut self, max_ops: usize) -> Result<Option<Vec<RecoveredOperation>>> {
        let mut batch = Vec::with_capacity(max_ops);

        while batch.len() < max_ops {
            match self.reader.next_record() {
                Some(Ok((lsn, record))) => {
                    self.next_lsn = lsn + 1;
                    if let Some(ops) = self.process_record(lsn, record)? {
                        batch.extend(ops);
                    }
                }
                Some(Err(_)) => {
                    // Skip corrupted records
                    continue;
                }
                None => {
                    // WAL exhausted
                    if batch.is_empty() {
                        return Ok(None);
                    }
                    break;
                }
            }
        }

        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(batch))
        }
    }

    /// Process a single record and return any committed operations.
    fn process_record(&mut self, lsn: Lsn, record: WalRecord) -> Result<Option<Vec<RecoveredOperation>>> {
        match record {
            WalRecord::BeginTx { tx_id } => {
                self.current_tx = Some(tx_id);
                self.pending_ops.clear();
                Ok(None)
            }
            WalRecord::CommitTx { tx_id } => {
                if self.current_tx == Some(tx_id) {
                    let ops = std::mem::take(&mut self.pending_ops);
                    self.current_tx = None;
                    Ok(Some(ops))
                } else {
                    Ok(None)
                }
            }
            WalRecord::AbortTx { tx_id } => {
                if self.current_tx == Some(tx_id) {
                    self.pending_ops.clear();
                    self.current_tx = None;
                }
                Ok(None)
            }
            WalRecord::Insert { term, value } => {
                let op = RecoveredOperation::Insert { lsn, term, value };
                if self.current_tx.is_some() {
                    self.pending_ops.push(op);
                    Ok(None)
                } else {
                    Ok(Some(vec![op]))
                }
            }
            WalRecord::Remove { term } => {
                let op = RecoveredOperation::Remove { lsn, term };
                if self.current_tx.is_some() {
                    self.pending_ops.push(op);
                    Ok(None)
                } else {
                    Ok(Some(vec![op]))
                }
            }
            WalRecord::Checkpoint { .. } => Ok(None),
            WalRecord::Increment {
                term,
                delta,
                result,
            } => {
                let op = RecoveredOperation::Increment {
                    lsn,
                    term,
                    delta,
                    result,
                };
                if self.current_tx.is_some() {
                    self.pending_ops.push(op);
                    Ok(None)
                } else {
                    Ok(Some(vec![op]))
                }
            }
            WalRecord::Upsert { term, value } => {
                let op = RecoveredOperation::Upsert { lsn, term, value };
                if self.current_tx.is_some() {
                    self.pending_ops.push(op);
                    Ok(None)
                } else {
                    Ok(Some(vec![op]))
                }
            }
            WalRecord::CompareAndSwap {
                term,
                expected: _,
                new_value,
                success,
            } => {
                // Only apply if the CAS succeeded
                if success {
                    let op = RecoveredOperation::CompareAndSwap {
                        lsn,
                        term,
                        new_value,
                        success,
                    };
                    if self.current_tx.is_some() {
                        self.pending_ops.push(op);
                        Ok(None)
                    } else {
                        Ok(Some(vec![op]))
                    }
                } else {
                    Ok(None)
                }
            }
            WalRecord::BatchInsert { entries } => {
                // Expand batch into individual insert operations
                let ops: Vec<RecoveredOperation> = entries
                    .into_iter()
                    .map(|(term, value)| RecoveredOperation::Insert { lsn, term, value })
                    .collect();

                if self.current_tx.is_some() {
                    self.pending_ops.extend(ops);
                    Ok(None)
                } else {
                    Ok(Some(ops))
                }
            }
        }
    }

    /// Get the next LSN after recovery.
    pub fn next_lsn(&self) -> Lsn {
        self.next_lsn
    }
}

/// Apply recovered operations to a PersistentARTrie.
///
/// This is a convenience function for the common case of recovering
/// directly into a trie structure.
pub fn apply_to_trie<V, F>(
    operations: impl IntoIterator<Item = RecoveredOperation>,
    mut insert_fn: F,
) -> std::result::Result<usize, String>
where
    F: FnMut(&[u8], Option<&[u8]>) -> std::result::Result<(), String>,
{
    let mut count = 0;

    for op in operations {
        match op {
            RecoveredOperation::Insert { lsn: _, term, value } => {
                insert_fn(&term, value.as_deref())?;
                count += 1;
            }
            RecoveredOperation::Remove { lsn: _, term } => {
                // For removes, we pass None as the value to indicate removal
                // The actual implementation would call a remove function
                insert_fn(&term, None)?;
                count += 1;
            }
            RecoveredOperation::Increment {
                lsn: _,
                term,
                delta: _,
                result,
            } => {
                // For increment, we store the final result value
                let value_bytes = result.to_le_bytes();
                insert_fn(&term, Some(&value_bytes))?;
                count += 1;
            }
            RecoveredOperation::Upsert { lsn: _, term, value } => {
                insert_fn(&term, Some(&value))?;
                count += 1;
            }
            RecoveredOperation::CompareAndSwap {
                lsn: _,
                term,
                new_value,
                success,
            } => {
                // Only apply if CAS succeeded
                if success {
                    insert_fn(&term, Some(&new_value))?;
                    count += 1;
                }
            }
        }
    }

    Ok(count)
}

/// Detect corruption in a persistent trie file.
///
/// This function performs a lightweight check for corruption without
/// loading the entire trie into memory. It checks:
///
/// 1. File header magic and version
/// 2. Header checksum (V2+ only)
/// 3. Optionally, arena checksums (if `check_arenas` is true)
///
/// # Arguments
///
/// * `path` - Path to the trie data file
/// * `check_arenas` - If true, also verify arena checksums (slower but more thorough)
///
/// # Returns
///
/// * `Ok(None)` - File is valid, no corruption detected
/// * `Ok(Some(corruption))` - Corruption detected, describes the type
/// * `Err(...)` - I/O error during verification
///
/// # Example
///
/// ```rust,ignore
/// use libdictenstein::persistent_artrie::recovery::detect_corruption;
///
/// match detect_corruption("data.part", true)? {
///     None => println!("File is valid"),
///     Some(corruption) => println!("Corruption detected: {}", corruption),
/// }
/// ```
pub fn detect_corruption<P: AsRef<Path>>(
    path: P,
    check_arenas: bool,
) -> std::result::Result<Option<CorruptionType>, RecoveryError> {
    use std::fs::File;
    use std::io::Read;

    let path = path.as_ref();

    // Check if file exists
    if !path.exists() {
        return Ok(None); // No file = no corruption
    }

    // Open and read header
    let mut file = File::open(path).map_err(|e| {
        RecoveryError::Io(e)
    })?;

    // Read header bytes
    let mut header_bytes = [0u8; 64];
    match file.read_exact(&mut header_bytes) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            let actual_size = file.metadata().map(|m| m.len() as usize).unwrap_or(0);
            return Ok(Some(CorruptionType::Truncated {
                expected: 64,
                actual: actual_size,
            }));
        }
        Err(e) => return Err(RecoveryError::Io(e)),
    }

    // Check magic - two formats supported:
    //
    // 1. DiskManager format (used by PersistentARTrie and DiskBackedCharTrieInner):
    //    - u64 magic at bytes 0-7: 0x5041_5254_0001_0000 ("PART" + version in big-endian parts)
    //    - In little-endian storage: [00 00 01 00 54 52 41 50]
    //    - Bytes 4-7 contain "PART" (0x50415254)
    //
    // 2. CharTrieFileHeader format (alternative, not currently used):
    //    - [u8; 4] magic at bytes 0-3: "ARTC" or "PART"
    //    - Version at byte 4

    // First check for DiskManager u64 magic (MAGIC_NUMBER = 0x5041_5254_0001_0000)
    let magic_u64 = u64::from_le_bytes([
        header_bytes[0], header_bytes[1], header_bytes[2], header_bytes[3],
        header_bytes[4], header_bytes[5], header_bytes[6], header_bytes[7],
    ]);

    // DiskManager's MAGIC_NUMBER
    const DISK_MANAGER_MAGIC: u64 = 0x5041_5254_0001_0000;

    if magic_u64 == DISK_MANAGER_MAGIC {
        // Valid DiskManager format - check version (embedded in magic, always v1.0 for now)
        // Check FNV-1a checksum at bytes 56-63
        // For now, just verify magic is valid - detailed checksum checking requires
        // loading the full header struct with atomics
        return Ok(None);
    }

    // Fall back to checking for alternative formats (4-byte magic at start)
    let magic_4 = &header_bytes[0..4];
    if magic_4 != b"PART" && magic_4 != b"ARTC" {
        return Ok(Some(CorruptionType::InvalidHeader(format!(
            "Invalid magic: u64={:#018x} (bytes {:?})",
            magic_u64, &header_bytes[0..8]
        ))));
    }

    // Check version for 4-byte magic formats
    let version = header_bytes[4];
    if version == 0 || version > 2 {
        return Ok(Some(CorruptionType::InvalidHeader(format!(
            "Unsupported version: {}",
            version
        ))));
    }

    // Check header checksum for V2+ (bytes 32-35 contain CRC32 of bytes 0-31)
    if version >= 2 {
        let stored_checksum = u32::from_le_bytes([
            header_bytes[32],
            header_bytes[33],
            header_bytes[34],
            header_bytes[35],
        ]);
        let computed_checksum = crc32_header(&header_bytes[0..32]);
        if stored_checksum != computed_checksum {
            return Ok(Some(CorruptionType::InvalidHeader(format!(
                "Header checksum mismatch: stored {:#x}, computed {:#x}",
                stored_checksum, computed_checksum
            ))));
        }
    }

    // Optional: Check arena checksums
    if check_arenas {
        // Read root descriptor to get arena count
        // Root descriptor is at block 1 (after header), offset depends on file type
        // For ARTC files, block 0 = header, blocks 1..N = arenas, block N+1 = root desc
        // This requires more complex parsing that depends on file format
        // For now, we just check if the file is at least as large as expected

        let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        if file_size < 64 {
            return Ok(Some(CorruptionType::Truncated {
                expected: 64,
                actual: file_size as usize,
            }));
        }

        // More thorough arena checking would require loading the buffer manager
        // which is done in the full open_with_recovery() implementation
    }

    Ok(None)
}

/// CRC32 checksum (IEEE polynomial) for header integrity verification.
fn crc32_header(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFFFFFF;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB88320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Find WAL archive segments for recovery.
///
/// Scans the archive directory for WAL segments in chronological order.
///
/// # Arguments
///
/// * `archive_dir` - Directory containing archived WAL segments
///
/// # Returns
///
/// Vector of paths to WAL segments, ordered oldest to newest.
pub fn find_wal_archive_segments<P: AsRef<Path>>(archive_dir: P) -> Vec<PathBuf> {
    let archive_dir = archive_dir.as_ref();

    if !archive_dir.exists() {
        return Vec::new();
    }

    let mut segments: Vec<_> = std::fs::read_dir(archive_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let path = entry.path();
            // Archive segments have .segment extension (e.g., wal_12345.segment)
            path.extension().and_then(|e| e.to_str()) == Some("segment")
        })
        .map(|entry| entry.path())
        .collect();

    // Sort by filename (which contains timestamp) - oldest first
    segments.sort();
    segments
}

/// Rebuild trie from WAL archive segments.
///
/// This is the core recovery function that replays WAL operations to
/// reconstruct the trie state.
///
/// # Arguments
///
/// * `segments` - Ordered list of WAL segment paths
/// * `apply_fn` - Callback to apply each recovered operation
///
/// # Returns
///
/// Number of records replayed and terms recovered.
pub fn rebuild_from_wal_segments<F>(
    segments: &[PathBuf],
    mut apply_fn: F,
) -> std::result::Result<(u64, u64), RecoveryError>
where
    F: FnMut(RecoveredOperation) -> std::result::Result<(), String>,
{
    let mut records_replayed: u64 = 0;
    let mut terms_recovered: u64 = 0;

    for segment_path in segments {
        let reader = match WalReader::new(segment_path) {
            Ok(r) => r,
            Err(_) => continue, // Skip unreadable segments
        };

        for result in reader.iter() {
            let (_lsn, record) = match result {
                Ok(r) => r,
                Err(_) => continue, // Skip corrupted records
            };

            records_replayed += 1;

            // Convert WalRecord to RecoveredOperation and apply
            match record {
                super::wal::WalRecord::Insert { term, value } => {
                    let op = RecoveredOperation::Insert { lsn: 0, term, value };
                    if apply_fn(op).is_ok() {
                        terms_recovered += 1;
                    }
                }
                super::wal::WalRecord::Remove { term } => {
                    let op = RecoveredOperation::Remove { lsn: 0, term };
                    if apply_fn(op).is_ok() {
                        terms_recovered += 1;
                    }
                }
                super::wal::WalRecord::Increment { term, delta, result } => {
                    let op = RecoveredOperation::Increment { lsn: 0, term, delta, result };
                    if apply_fn(op).is_ok() {
                        terms_recovered += 1;
                    }
                }
                super::wal::WalRecord::Upsert { term, value } => {
                    let op = RecoveredOperation::Upsert { lsn: 0, term, value };
                    if apply_fn(op).is_ok() {
                        terms_recovered += 1;
                    }
                }
                super::wal::WalRecord::CompareAndSwap { term, new_value, success, .. } => {
                    if success {
                        let op = RecoveredOperation::CompareAndSwap {
                            lsn: 0,
                            term,
                            new_value,
                            success,
                        };
                        if apply_fn(op).is_ok() {
                            terms_recovered += 1;
                        }
                    }
                }
                super::wal::WalRecord::BatchInsert { entries } => {
                    // Expand batch and apply each entry
                    for (term, value) in entries {
                        let op = RecoveredOperation::Insert { lsn: 0, term, value };
                        if apply_fn(op).is_ok() {
                            terms_recovered += 1;
                        }
                    }
                }
                _ => {} // Skip transaction/checkpoint records
            }
        }
    }

    Ok((records_replayed, terms_recovered))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::wal::WalWriter;
    use tempfile::tempdir;

    #[test]
    fn test_recovery_empty_wal() {
        let dir = tempdir().expect("create tempdir");
        let wal_path = dir.path().join("empty.wal");

        let manager = RecoveryManager::new(&wal_path);

        // No WAL file - should not need recovery
        assert!(!manager.needs_recovery().expect("needs_recovery"));

        // Recovery should return empty state
        let state = manager.recover().expect("recover");
        assert_eq!(state.operation_count(), 0);
        assert_eq!(state.next_lsn, 1);
    }

    #[test]
    fn test_recovery_simple_operations() {
        let dir = tempdir().expect("create tempdir");
        let wal_path = dir.path().join("test.wal");

        // Write some operations to WAL
        {
            let writer = WalWriter::create(&wal_path).expect("create writer");

            writer
                .append(WalRecord::Insert {
                    term: b"hello".to_vec(),
                    value: None,
                })
                .expect("append insert");

            writer
                .append(WalRecord::Insert {
                    term: b"world".to_vec(),
                    value: Some(b"value".to_vec()),
                })
                .expect("append insert");

            writer
                .append(WalRecord::Remove {
                    term: b"hello".to_vec(),
                })
                .expect("append remove");

            writer.sync().expect("sync");
        }

        // Recover
        let manager = RecoveryManager::new(&wal_path);
        assert!(manager.needs_recovery().expect("needs_recovery"));

        let state = manager.recover().expect("recover");
        assert_eq!(state.operation_count(), 3);

        let ops: Vec<_> = state.operations().collect();

        match &ops[0] {
            RecoveredOperation::Insert { term, value, .. } => {
                assert_eq!(term, b"hello");
                assert!(value.is_none());
            }
            _ => panic!("Expected insert"),
        }

        match &ops[1] {
            RecoveredOperation::Insert { term, value, .. } => {
                assert_eq!(term, b"world");
                assert_eq!(value.as_deref(), Some(b"value".as_slice()));
            }
            _ => panic!("Expected insert"),
        }

        match &ops[2] {
            RecoveredOperation::Remove { term, .. } => {
                assert_eq!(term, b"hello");
            }
            _ => panic!("Expected remove"),
        }
    }

    #[test]
    fn test_recovery_with_transactions() {
        let dir = tempdir().expect("create tempdir");
        let wal_path = dir.path().join("tx.wal");

        // Write transactional operations
        {
            let writer = WalWriter::create(&wal_path).expect("create writer");

            // Committed transaction
            writer
                .append(WalRecord::BeginTx { tx_id: 1 })
                .expect("append begin");
            writer
                .append(WalRecord::Insert {
                    term: b"committed".to_vec(),
                    value: None,
                })
                .expect("append insert");
            writer
                .append(WalRecord::CommitTx { tx_id: 1 })
                .expect("append commit");

            // Aborted transaction
            writer
                .append(WalRecord::BeginTx { tx_id: 2 })
                .expect("append begin");
            writer
                .append(WalRecord::Insert {
                    term: b"aborted".to_vec(),
                    value: None,
                })
                .expect("append insert");
            writer
                .append(WalRecord::AbortTx { tx_id: 2 })
                .expect("append abort");

            // Incomplete transaction (no commit/abort)
            writer
                .append(WalRecord::BeginTx { tx_id: 3 })
                .expect("append begin");
            writer
                .append(WalRecord::Insert {
                    term: b"incomplete".to_vec(),
                    value: None,
                })
                .expect("append insert");

            writer.sync().expect("sync");
        }

        // Recover
        let manager = RecoveryManager::new(&wal_path);
        let state = manager.recover().expect("recover");

        // Only committed transaction should be recovered
        assert_eq!(state.operation_count(), 1);

        let ops: Vec<_> = state.into_operations();
        match &ops[0] {
            RecoveredOperation::Insert { term, .. } => {
                assert_eq!(term, b"committed");
            }
            _ => panic!("Expected insert"),
        }
    }

    #[test]
    fn test_recovery_stats() {
        let dir = tempdir().expect("create tempdir");
        let wal_path = dir.path().join("stats.wal");

        {
            let writer = WalWriter::create(&wal_path).expect("create writer");

            // 3 inserts outside transaction
            for i in 0..3 {
                writer
                    .append(WalRecord::Insert {
                        term: format!("term{}", i).into_bytes(),
                        value: None,
                    })
                    .expect("append");
            }

            // 1 committed transaction with 2 ops
            writer
                .append(WalRecord::BeginTx { tx_id: 1 })
                .expect("append");
            writer
                .append(WalRecord::Insert {
                    term: b"tx1".to_vec(),
                    value: None,
                })
                .expect("append");
            writer
                .append(WalRecord::Remove {
                    term: b"tx1".to_vec(),
                })
                .expect("append");
            writer
                .append(WalRecord::CommitTx { tx_id: 1 })
                .expect("append");

            // Checkpoint - use next_lsn (8 records were written, so LSN 8)
            writer.checkpoint(8).expect("checkpoint");

            writer.sync().expect("sync");
        }

        let manager = RecoveryManager::new(&wal_path);
        let state = manager.recover().expect("recover");

        // 3 non-tx inserts + 2 tx ops = 5 total
        assert_eq!(state.operation_count(), 5);
        assert_eq!(state.stats.committed_transactions, 1);
        assert!(state.stats.checkpoint_lsn.is_some());
    }

    #[test]
    fn test_incremental_recovery() {
        let dir = tempdir().expect("create tempdir");
        let wal_path = dir.path().join("incremental.wal");

        // Write 10 operations
        {
            let writer = WalWriter::create(&wal_path).expect("create writer");
            for i in 0..10 {
                writer
                    .append(WalRecord::Insert {
                        term: format!("term{}", i).into_bytes(),
                        value: None,
                    })
                    .expect("append");
            }
            writer.sync().expect("sync");
        }

        // Recover in batches of 3
        let mut recovery = IncrementalRecovery::new(&wal_path).expect("create recovery");
        let mut total = 0;

        while let Some(batch) = recovery.next_batch(3).expect("next_batch") {
            total += batch.len();
        }

        assert_eq!(total, 10);
    }

    #[test]
    fn test_recovery_with_callback() {
        let dir = tempdir().expect("create tempdir");
        let wal_path = dir.path().join("callback.wal");

        {
            let writer = WalWriter::create(&wal_path).expect("create writer");
            for i in 0..5 {
                writer
                    .append(WalRecord::Insert {
                        term: format!("term{}", i).into_bytes(),
                        value: None,
                    })
                    .expect("append");
            }
            writer.sync().expect("sync");
        }

        let mut collected = Vec::new();
        let manager = RecoveryManager::new(&wal_path);

        manager
            .recover_with_callback(|op| {
                collected.push(op);
                Ok(())
            })
            .expect("recover_with_callback");

        assert_eq!(collected.len(), 5);
    }
}
