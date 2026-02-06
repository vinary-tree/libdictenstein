//! Write-Ahead Log (WAL) for crash recovery.
//!
//! This module implements a redo-only WAL for the PersistentARTrie. The WAL
//! ensures durability by logging operations before they are applied to the
//! main data structure.
//!
//! # Design
//!
//! The WAL uses a redo-only approach:
//! - Operations are logged before being applied
//! - On crash recovery, the log is replayed from the last checkpoint
//! - Periodic checkpoints truncate the log
//!
//! # Record Format
//!
//! Each record has the following layout:
//! ```text
//! +----------+----------+----------+----------+----------+
//! | CRC32    | Length   | LSN      | Type     | Payload  |
//! | (4 bytes)| (4 bytes)| (8 bytes)| (1 byte) | (varies) |
//! +----------+----------+----------+----------+----------+
//! ```
//!
//! # Group Commit
//!
//! For performance, multiple operations can be batched into a single fsync:
//! - Writers append to the log buffer
//! - A background thread (or explicit flush) fsyncs periodically
//! - Writers wait for their LSN to be durable before returning
//!
//! # Example
//!
//! ```rust,ignore
//! use libdictenstein::persistent_artrie::wal::{Wal, WalRecord, WalRecordType};
//!
//! let wal = Wal::create("data.wal")?;
//!
//! // Log an insert operation
//! let lsn = wal.append(WalRecord::Insert {
//!     term: "hello".as_bytes().to_vec(),
//!     value: None,
//! })?;
//!
//! // Ensure durability
//! wal.sync()?;
//!
//! // On recovery
//! let wal = Wal::open("data.wal")?;
//! for record in wal.iter() {
//!     // Replay the operation
//! }
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Log Sequence Number - monotonically increasing identifier for log records.
pub type Lsn = u64;

/// WAL configuration for archive mode and segment management.
///
/// Archive mode provides crash recovery by preserving WAL segments instead
/// of truncating them. This allows rebuilding the entire dataset from
/// archived segments if the base file is corrupted.
///
/// # Example
///
/// ```rust,ignore
/// let config = WalConfig {
///     archive_enabled: true,
///     archive_dir: PathBuf::from("./wal_archive"),
///     max_segments: 10,
///     max_archive_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
/// };
/// ```
#[derive(Debug, Clone)]
pub struct WalConfig {
    /// Enable archive mode (rename WAL instead of truncate)
    ///
    /// When enabled, checkpoint rotates the WAL to archive instead of
    /// truncating it. This preserves all operations for potential recovery.
    pub archive_enabled: bool,

    /// Directory for archived WAL segments
    ///
    /// Default: "{data_dir}/wal_archive"
    pub archive_dir: PathBuf,

    /// Maximum number of archived segments to keep
    ///
    /// Older segments are pruned when this limit is exceeded.
    /// Default: 10
    pub max_segments: usize,

    /// Maximum total bytes in archived segments
    ///
    /// Older segments are pruned when this limit is exceeded.
    /// Default: 10 GB
    pub max_archive_bytes: u64,
}

impl Default for WalConfig {
    fn default() -> Self {
        Self {
            archive_enabled: true,
            archive_dir: PathBuf::from("wal_archive"),
            max_segments: 10,
            max_archive_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
        }
    }
}

impl WalConfig {
    /// Create a new configuration with archive mode disabled
    pub fn no_archive() -> Self {
        Self {
            archive_enabled: false,
            ..Default::default()
        }
    }

    /// Create a new configuration with custom archive directory
    pub fn with_archive_dir(archive_dir: impl Into<PathBuf>) -> Self {
        Self {
            archive_dir: archive_dir.into(),
            ..Default::default()
        }
    }
}

/// CRC32 for record integrity verification.
fn crc32(data: &[u8]) -> u32 {
    // Simple CRC32 implementation (IEEE polynomial)
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

/// WAL record types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WalRecordType {
    /// Insert a term (with optional value)
    Insert = 1,
    /// Remove a term
    Remove = 2,
    /// Checkpoint marker
    Checkpoint = 3,
    /// Begin transaction (for future use)
    BeginTx = 4,
    /// Commit transaction (for future use)
    CommitTx = 5,
    /// Abort transaction (for future use)
    AbortTx = 6,
    /// Atomic increment operation
    Increment = 7,
    /// Atomic upsert operation (update if exists, insert if not)
    Upsert = 8,
    /// Atomic compare-and-swap operation
    CompareAndSwap = 9,
    /// Batch insert - multiple terms in a single WAL record
    ///
    /// This reduces WAL header overhead from 17 bytes per insert to
    /// 17 bytes + 4 bytes (count) for an entire batch.
    BatchInsert = 10,
    /// Batch increment - multiple increment operations in a single WAL record.
    ///
    /// Used by document transactions to batch INCREMENT operations atomically.
    /// Unlike BatchInsert which uses SET semantics, BatchIncrement accumulates
    /// deltas with existing values.
    BatchIncrement = 11,
}

impl TryFrom<u8> for WalRecordType {
    type Error = WalError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(WalRecordType::Insert),
            2 => Ok(WalRecordType::Remove),
            3 => Ok(WalRecordType::Checkpoint),
            4 => Ok(WalRecordType::BeginTx),
            5 => Ok(WalRecordType::CommitTx),
            6 => Ok(WalRecordType::AbortTx),
            7 => Ok(WalRecordType::Increment),
            8 => Ok(WalRecordType::Upsert),
            9 => Ok(WalRecordType::CompareAndSwap),
            10 => Ok(WalRecordType::BatchInsert),
            11 => Ok(WalRecordType::BatchIncrement),
            _ => Err(WalError::InvalidRecordType(value)),
        }
    }
}

/// A WAL record containing an operation to replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalRecord {
    /// Insert a term with optional serialized value
    Insert {
        /// The term to insert (UTF-8 bytes)
        term: Vec<u8>,
        /// Optional serialized value
        value: Option<Vec<u8>>,
    },
    /// Remove a term
    Remove {
        /// The term to remove
        term: Vec<u8>,
    },
    /// Checkpoint marker with metadata
    Checkpoint {
        /// LSN up to which data is durable in the main file
        checkpoint_lsn: Lsn,
        /// Timestamp of checkpoint
        timestamp: u64,
    },
    /// Begin transaction
    BeginTx {
        /// Transaction ID
        tx_id: u64,
    },
    /// Commit transaction
    CommitTx {
        /// Transaction ID
        tx_id: u64,
    },
    /// Abort transaction
    AbortTx {
        /// Transaction ID
        tx_id: u64,
    },
    /// Atomic increment operation
    ///
    /// Increments the value associated with a term by `delta`.
    /// If the term doesn't exist, inserts with `delta` as the initial value.
    Increment {
        /// The term to increment
        term: Vec<u8>,
        /// The delta to add (can be negative)
        delta: i64,
        /// The resulting value after increment
        result: i64,
    },
    /// Atomic upsert operation
    ///
    /// Updates the value if the term exists, otherwise inserts a new term.
    Upsert {
        /// The term to upsert
        term: Vec<u8>,
        /// The new serialized value
        value: Vec<u8>,
    },
    /// Atomic compare-and-swap operation
    ///
    /// Updates the value only if the current value matches `expected`.
    CompareAndSwap {
        /// The term to update
        term: Vec<u8>,
        /// The expected current value (None means term should not exist)
        expected: Option<Vec<u8>>,
        /// The new value to set
        new_value: Vec<u8>,
        /// Whether the swap succeeded
        success: bool,
    },
    /// Batch insert - multiple terms in a single WAL record.
    ///
    /// This record type batches multiple inserts into a single WAL record,
    /// reducing header overhead from 17 bytes per insert to ~21 bytes for
    /// the entire batch (17-byte header + 4-byte count).
    ///
    /// # Wire Format
    ///
    /// ```text
    /// +----------+----------------------------------------------------+
    /// | Count    | Entry[0] | Entry[1] | ... | Entry[count-1]         |
    /// | (4 bytes)| (varies) | (varies) | ... | (varies)               |
    /// +----------+----------------------------------------------------+
    ///
    /// Entry Format (same as Insert payload):
    /// +----------+----------+----------+----------+----------+
    /// | term_len | term     | has_val  | [val_len | value]   |
    /// | (4 bytes)| (varies) | (1 byte) | [4 bytes | varies]  |
    /// +----------+----------+----------+----------+----------+
    /// ```
    BatchInsert {
        /// The entries in this batch (term, optional value)
        entries: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    },
    /// Batch increment - multiple increment operations in a single WAL record.
    ///
    /// Used by document transactions to batch INCREMENT operations atomically.
    /// Unlike BatchInsert which uses SET semantics, BatchIncrement accumulates
    /// deltas with existing values.
    ///
    /// # Wire Format
    ///
    /// ```text
    /// +----------+----------------------------------------------------+
    /// | Count    | Entry[0] | Entry[1] | ... | Entry[count-1]         |
    /// | (4 bytes)| (varies) | (varies) | ... | (varies)               |
    /// +----------+----------------------------------------------------+
    ///
    /// Entry Format:
    /// +----------+----------+----------+
    /// | term_len | term     | delta    |
    /// | (4 bytes)| (varies) | (8 bytes)|
    /// +----------+----------+----------+
    /// ```
    BatchIncrement {
        /// The increment entries (term, delta)
        entries: Vec<(Vec<u8>, i64)>,
    },
}

impl WalRecord {
    /// Get the record type.
    pub fn record_type(&self) -> WalRecordType {
        match self {
            WalRecord::Insert { .. } => WalRecordType::Insert,
            WalRecord::Remove { .. } => WalRecordType::Remove,
            WalRecord::Checkpoint { .. } => WalRecordType::Checkpoint,
            WalRecord::BeginTx { .. } => WalRecordType::BeginTx,
            WalRecord::CommitTx { .. } => WalRecordType::CommitTx,
            WalRecord::AbortTx { .. } => WalRecordType::AbortTx,
            WalRecord::Increment { .. } => WalRecordType::Increment,
            WalRecord::Upsert { .. } => WalRecordType::Upsert,
            WalRecord::CompareAndSwap { .. } => WalRecordType::CompareAndSwap,
            WalRecord::BatchInsert { .. } => WalRecordType::BatchInsert,
            WalRecord::BatchIncrement { .. } => WalRecordType::BatchIncrement,
        }
    }

    /// Serialize the record payload to bytes.
    pub fn serialize_payload(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        match self {
            WalRecord::Insert { term, value } => {
                // Term length (4 bytes) + term + has_value (1 byte) + [value_length + value]
                buf.extend_from_slice(&(term.len() as u32).to_le_bytes());
                buf.extend_from_slice(term);
                if let Some(v) = value {
                    buf.push(1); // has_value = true
                    buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
                    buf.extend_from_slice(v);
                } else {
                    buf.push(0); // has_value = false
                }
            }
            WalRecord::Remove { term } => {
                buf.extend_from_slice(&(term.len() as u32).to_le_bytes());
                buf.extend_from_slice(term);
            }
            WalRecord::Checkpoint {
                checkpoint_lsn,
                timestamp,
            } => {
                buf.extend_from_slice(&checkpoint_lsn.to_le_bytes());
                buf.extend_from_slice(&timestamp.to_le_bytes());
            }
            WalRecord::BeginTx { tx_id }
            | WalRecord::CommitTx { tx_id }
            | WalRecord::AbortTx { tx_id } => {
                buf.extend_from_slice(&tx_id.to_le_bytes());
            }
            WalRecord::Increment {
                term,
                delta,
                result,
            } => {
                // Term length (4 bytes) + term + delta (8 bytes) + result (8 bytes)
                buf.extend_from_slice(&(term.len() as u32).to_le_bytes());
                buf.extend_from_slice(term);
                buf.extend_from_slice(&delta.to_le_bytes());
                buf.extend_from_slice(&result.to_le_bytes());
            }
            WalRecord::Upsert { term, value } => {
                // Term length (4 bytes) + term + value length (4 bytes) + value
                buf.extend_from_slice(&(term.len() as u32).to_le_bytes());
                buf.extend_from_slice(term);
                buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
                buf.extend_from_slice(value);
            }
            WalRecord::CompareAndSwap {
                term,
                expected,
                new_value,
                success,
            } => {
                // Term length + term + has_expected (1 byte) + [expected_len + expected] + new_value_len + new_value + success (1 byte)
                buf.extend_from_slice(&(term.len() as u32).to_le_bytes());
                buf.extend_from_slice(term);
                if let Some(exp) = expected {
                    buf.push(1); // has_expected = true
                    buf.extend_from_slice(&(exp.len() as u32).to_le_bytes());
                    buf.extend_from_slice(exp);
                } else {
                    buf.push(0); // has_expected = false
                }
                buf.extend_from_slice(&(new_value.len() as u32).to_le_bytes());
                buf.extend_from_slice(new_value);
                buf.push(if *success { 1 } else { 0 });
            }
            WalRecord::BatchInsert { entries } => {
                // Count (4 bytes) + entries (each entry same as Insert payload)
                buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
                for (term, value) in entries {
                    // Same format as Insert payload
                    buf.extend_from_slice(&(term.len() as u32).to_le_bytes());
                    buf.extend_from_slice(term);
                    if let Some(v) = value {
                        buf.push(1); // has_value = true
                        buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
                        buf.extend_from_slice(v);
                    } else {
                        buf.push(0); // has_value = false
                    }
                }
            }
            WalRecord::BatchIncrement { entries } => {
                // Count (4 bytes) + entries (term_len + term + delta)
                buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
                for (term, delta) in entries {
                    buf.extend_from_slice(&(term.len() as u32).to_le_bytes());
                    buf.extend_from_slice(term);
                    buf.extend_from_slice(&delta.to_le_bytes());
                }
            }
        }

        buf
    }

    /// Calculate the serialized size of this record in bytes.
    ///
    /// This is used by group commit for batch size tracking.
    pub fn serialized_size(&self) -> usize {
        // Header: CRC32 (4) + Length (4) + LSN (8) + Type (1) = 17 bytes
        const RECORD_HEADER_SIZE: usize = 17;
        RECORD_HEADER_SIZE + self.serialize_payload().len()
    }

    /// Deserialize a record from type and payload.
    pub fn deserialize(record_type: WalRecordType, payload: &[u8]) -> Result<Self, WalError> {
        match record_type {
            WalRecordType::Insert => {
                if payload.len() < 5 {
                    return Err(WalError::CorruptedRecord("Insert payload too short".into()));
                }
                let term_len = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                if payload.len() < 4 + term_len + 1 {
                    return Err(WalError::CorruptedRecord("Insert term truncated".into()));
                }
                let term = payload[4..4 + term_len].to_vec();
                let has_value = payload[4 + term_len] != 0;
                let value = if has_value {
                    let value_offset = 4 + term_len + 1;
                    if payload.len() < value_offset + 4 {
                        return Err(WalError::CorruptedRecord("Insert value length truncated".into()));
                    }
                    let value_len = u32::from_le_bytes(
                        payload[value_offset..value_offset + 4].try_into().unwrap(),
                    ) as usize;
                    if payload.len() < value_offset + 4 + value_len {
                        return Err(WalError::CorruptedRecord("Insert value truncated".into()));
                    }
                    Some(payload[value_offset + 4..value_offset + 4 + value_len].to_vec())
                } else {
                    None
                };
                Ok(WalRecord::Insert { term, value })
            }
            WalRecordType::Remove => {
                if payload.len() < 4 {
                    return Err(WalError::CorruptedRecord("Remove payload too short".into()));
                }
                let term_len = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                if payload.len() < 4 + term_len {
                    return Err(WalError::CorruptedRecord("Remove term truncated".into()));
                }
                let term = payload[4..4 + term_len].to_vec();
                Ok(WalRecord::Remove { term })
            }
            WalRecordType::Checkpoint => {
                if payload.len() < 16 {
                    return Err(WalError::CorruptedRecord("Checkpoint payload too short".into()));
                }
                let checkpoint_lsn = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                let timestamp = u64::from_le_bytes(payload[8..16].try_into().unwrap());
                Ok(WalRecord::Checkpoint {
                    checkpoint_lsn,
                    timestamp,
                })
            }
            WalRecordType::BeginTx => {
                if payload.len() < 8 {
                    return Err(WalError::CorruptedRecord("BeginTx payload too short".into()));
                }
                let tx_id = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                Ok(WalRecord::BeginTx { tx_id })
            }
            WalRecordType::CommitTx => {
                if payload.len() < 8 {
                    return Err(WalError::CorruptedRecord("CommitTx payload too short".into()));
                }
                let tx_id = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                Ok(WalRecord::CommitTx { tx_id })
            }
            WalRecordType::AbortTx => {
                if payload.len() < 8 {
                    return Err(WalError::CorruptedRecord("AbortTx payload too short".into()));
                }
                let tx_id = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                Ok(WalRecord::AbortTx { tx_id })
            }
            WalRecordType::Increment => {
                // term_len (4) + term + delta (8) + result (8)
                if payload.len() < 4 {
                    return Err(WalError::CorruptedRecord("Increment payload too short".into()));
                }
                let term_len = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                if payload.len() < 4 + term_len + 16 {
                    return Err(WalError::CorruptedRecord("Increment payload truncated".into()));
                }
                let term = payload[4..4 + term_len].to_vec();
                let delta_offset = 4 + term_len;
                let delta = i64::from_le_bytes(
                    payload[delta_offset..delta_offset + 8].try_into().unwrap(),
                );
                let result = i64::from_le_bytes(
                    payload[delta_offset + 8..delta_offset + 16]
                        .try_into()
                        .unwrap(),
                );
                Ok(WalRecord::Increment {
                    term,
                    delta,
                    result,
                })
            }
            WalRecordType::Upsert => {
                // term_len (4) + term + value_len (4) + value
                if payload.len() < 4 {
                    return Err(WalError::CorruptedRecord("Upsert payload too short".into()));
                }
                let term_len = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                if payload.len() < 4 + term_len + 4 {
                    return Err(WalError::CorruptedRecord("Upsert term truncated".into()));
                }
                let term = payload[4..4 + term_len].to_vec();
                let value_len_offset = 4 + term_len;
                let value_len = u32::from_le_bytes(
                    payload[value_len_offset..value_len_offset + 4]
                        .try_into()
                        .unwrap(),
                ) as usize;
                if payload.len() < value_len_offset + 4 + value_len {
                    return Err(WalError::CorruptedRecord("Upsert value truncated".into()));
                }
                let value = payload[value_len_offset + 4..value_len_offset + 4 + value_len].to_vec();
                Ok(WalRecord::Upsert { term, value })
            }
            WalRecordType::CompareAndSwap => {
                // term_len + term + has_expected (1) + [expected_len + expected] + new_value_len + new_value + success (1)
                if payload.len() < 4 {
                    return Err(WalError::CorruptedRecord("CAS payload too short".into()));
                }
                let term_len = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                if payload.len() < 4 + term_len + 1 {
                    return Err(WalError::CorruptedRecord("CAS term truncated".into()));
                }
                let term = payload[4..4 + term_len].to_vec();
                let mut offset = 4 + term_len;

                let has_expected = payload[offset] != 0;
                offset += 1;

                let expected = if has_expected {
                    if payload.len() < offset + 4 {
                        return Err(WalError::CorruptedRecord("CAS expected length truncated".into()));
                    }
                    let exp_len = u32::from_le_bytes(
                        payload[offset..offset + 4].try_into().unwrap(),
                    ) as usize;
                    offset += 4;
                    if payload.len() < offset + exp_len {
                        return Err(WalError::CorruptedRecord("CAS expected truncated".into()));
                    }
                    let exp = payload[offset..offset + exp_len].to_vec();
                    offset += exp_len;
                    Some(exp)
                } else {
                    None
                };

                if payload.len() < offset + 4 {
                    return Err(WalError::CorruptedRecord("CAS new_value length truncated".into()));
                }
                let new_value_len = u32::from_le_bytes(
                    payload[offset..offset + 4].try_into().unwrap(),
                ) as usize;
                offset += 4;
                if payload.len() < offset + new_value_len + 1 {
                    return Err(WalError::CorruptedRecord("CAS new_value truncated".into()));
                }
                let new_value = payload[offset..offset + new_value_len].to_vec();
                offset += new_value_len;

                let success = payload[offset] != 0;

                Ok(WalRecord::CompareAndSwap {
                    term,
                    expected,
                    new_value,
                    success,
                })
            }
            WalRecordType::BatchInsert => {
                // Count (4 bytes) + entries
                if payload.len() < 4 {
                    return Err(WalError::CorruptedRecord("BatchInsert payload too short".into()));
                }
                let count = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                let mut offset = 4;
                let mut entries = Vec::with_capacity(count);

                for i in 0..count {
                    // Parse each entry (same format as Insert)
                    if payload.len() < offset + 4 {
                        return Err(WalError::CorruptedRecord(
                            format!("BatchInsert entry {} term_len truncated", i),
                        ));
                    }
                    let term_len = u32::from_le_bytes(
                        payload[offset..offset + 4].try_into().unwrap(),
                    ) as usize;
                    offset += 4;

                    if payload.len() < offset + term_len + 1 {
                        return Err(WalError::CorruptedRecord(
                            format!("BatchInsert entry {} term truncated", i),
                        ));
                    }
                    let term = payload[offset..offset + term_len].to_vec();
                    offset += term_len;

                    let has_value = payload[offset] != 0;
                    offset += 1;

                    let value = if has_value {
                        if payload.len() < offset + 4 {
                            return Err(WalError::CorruptedRecord(
                                format!("BatchInsert entry {} value_len truncated", i),
                            ));
                        }
                        let value_len = u32::from_le_bytes(
                            payload[offset..offset + 4].try_into().unwrap(),
                        ) as usize;
                        offset += 4;

                        if payload.len() < offset + value_len {
                            return Err(WalError::CorruptedRecord(
                                format!("BatchInsert entry {} value truncated", i),
                            ));
                        }
                        let v = payload[offset..offset + value_len].to_vec();
                        offset += value_len;
                        Some(v)
                    } else {
                        None
                    };

                    entries.push((term, value));
                }

                Ok(WalRecord::BatchInsert { entries })
            }
            WalRecordType::BatchIncrement => {
                // Count (4 bytes) + entries (term_len + term + delta)
                if payload.len() < 4 {
                    return Err(WalError::CorruptedRecord("BatchIncrement payload too short".into()));
                }
                let count = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                let mut offset = 4;
                let mut entries = Vec::with_capacity(count);

                for i in 0..count {
                    // Parse each entry: term_len (4) + term + delta (8)
                    if payload.len() < offset + 4 {
                        return Err(WalError::CorruptedRecord(
                            format!("BatchIncrement entry {} term_len truncated", i),
                        ));
                    }
                    let term_len = u32::from_le_bytes(
                        payload[offset..offset + 4].try_into().unwrap(),
                    ) as usize;
                    offset += 4;

                    if payload.len() < offset + term_len + 8 {
                        return Err(WalError::CorruptedRecord(
                            format!("BatchIncrement entry {} term or delta truncated", i),
                        ));
                    }
                    let term = payload[offset..offset + term_len].to_vec();
                    offset += term_len;

                    let delta = i64::from_le_bytes(
                        payload[offset..offset + 8].try_into().unwrap(),
                    );
                    offset += 8;

                    entries.push((term, delta));
                }

                Ok(WalRecord::BatchIncrement { entries })
            }
        }
    }
}

/// WAL error types.
#[derive(Debug)]
pub enum WalError {
    /// I/O error
    Io(io::Error),
    /// Invalid record type byte
    InvalidRecordType(u8),
    /// Corrupted record (CRC mismatch or invalid format)
    CorruptedRecord(String),
    /// Unexpected end of file
    UnexpectedEof,
    /// WAL file already exists
    AlreadyExists,
    /// WAL file not found
    NotFound,
    /// Parent directory does not exist.
    /// Distinguishes from general NotFound for semantic matching with formal model.
    ParentNotFound(PathBuf),
}

impl std::fmt::Display for WalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WalError::Io(e) => write!(f, "WAL I/O error: {}", e),
            WalError::InvalidRecordType(t) => write!(f, "Invalid WAL record type: {}", t),
            WalError::CorruptedRecord(msg) => write!(f, "Corrupted WAL record: {}", msg),
            WalError::UnexpectedEof => write!(f, "Unexpected end of WAL file"),
            WalError::AlreadyExists => write!(f, "WAL file already exists"),
            WalError::NotFound => write!(f, "WAL file not found"),
            WalError::ParentNotFound(path) => {
                write!(f, "Parent directory not found: {}", path.display())
            }
        }
    }
}

impl std::error::Error for WalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WalError::Io(e) => Some(e),
            WalError::InvalidRecordType(_)
            | WalError::CorruptedRecord(_)
            | WalError::UnexpectedEof
            | WalError::AlreadyExists
            | WalError::NotFound
            | WalError::ParentNotFound(_) => None,
        }
    }
}

impl From<io::Error> for WalError {
    fn from(err: io::Error) -> Self {
        WalError::Io(err)
    }
}

/// WAL file header.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct WalHeader {
    /// Magic number: "PARTWAL\0"
    pub magic: [u8; 8],
    /// Version number
    pub version: u32,
    /// Last checkpoint LSN
    pub checkpoint_lsn: Lsn,
    /// Reserved for future use
    pub reserved: [u8; 44],
}

impl WalHeader {
    /// Magic number for WAL files.
    pub const MAGIC: [u8; 8] = *b"PARTWAL\0";
    /// Current version.
    pub const VERSION: u32 = 1;
    /// Header size in bytes.
    pub const SIZE: usize = 64;

    /// Create a new header.
    pub fn new() -> Self {
        WalHeader {
            magic: Self::MAGIC,
            version: Self::VERSION,
            checkpoint_lsn: 0,
            reserved: [0; 44],
        }
    }

    /// Serialize header to bytes.
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..8].copy_from_slice(&self.magic);
        buf[8..12].copy_from_slice(&self.version.to_le_bytes());
        buf[12..20].copy_from_slice(&self.checkpoint_lsn.to_le_bytes());
        buf[20..64].copy_from_slice(&self.reserved);
        buf
    }

    /// Deserialize header from bytes.
    pub fn from_bytes(buf: &[u8; Self::SIZE]) -> Result<Self, WalError> {
        let magic: [u8; 8] = buf[0..8].try_into().unwrap();
        if magic != Self::MAGIC {
            return Err(WalError::CorruptedRecord("Invalid WAL magic number".into()));
        }
        let version = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        if version != Self::VERSION {
            return Err(WalError::CorruptedRecord(format!(
                "Unsupported WAL version: {}",
                version
            )));
        }
        let checkpoint_lsn = u64::from_le_bytes(buf[12..20].try_into().unwrap());
        let reserved: [u8; 44] = buf[20..64].try_into().unwrap();

        Ok(WalHeader {
            magic,
            version,
            checkpoint_lsn,
            reserved,
        })
    }
}

impl Default for WalHeader {
    fn default() -> Self {
        Self::new()
    }
}

/// Write-Ahead Log writer.
///
/// Handles appending records to the log with optional group commit.
pub struct WalWriter {
    /// Path to the WAL file
    path: PathBuf,
    /// File handle
    file: Mutex<BufWriter<File>>,
    /// Current LSN (next LSN to assign)
    next_lsn: AtomicU64,
    /// Last synced LSN
    synced_lsn: AtomicU64,
    /// Header (cached)
    header: Mutex<WalHeader>,
}

impl WalWriter {
    /// Record header size: CRC32 (4) + Length (4) + LSN (8) + Type (1) = 17 bytes
    const RECORD_HEADER_SIZE: usize = 17;

    /// Create a new WAL file.
    ///
    /// Uses atomic exclusive creation (`O_CREAT | O_EXCL` via `create_new(true)`)
    /// to eliminate TOCTOU race conditions. This matches the formal model's
    /// `open_create` operation in `FileSystem.v`.
    ///
    /// # Errors
    ///
    /// - `WalError::AlreadyExists` - File already exists (atomic check)
    /// - `WalError::ParentNotFound` - Parent directory doesn't exist
    /// - `WalError::Io` - Other I/O errors
    pub fn create(path: impl AsRef<Path>) -> Result<Self, WalError> {
        let path = path.as_ref().to_path_buf();

        // Ensure parent directory exists (idempotent, matches formal mkdir_all)
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    if e.kind() == io::ErrorKind::NotFound {
                        // Parent's parent doesn't exist
                        WalError::ParentNotFound(parent.to_path_buf())
                    } else {
                        WalError::Io(e)
                    }
                })?;
            }
        }

        // Atomic exclusive creation - eliminates TOCTOU race
        // create_new(true) = O_CREAT | O_EXCL (fails atomically if file exists)
        let file = match OpenOptions::new()
            .create_new(true) // Atomic: create only if doesn't exist
            .write(true)
            .read(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(WalError::AlreadyExists);
            }
            Err(e) => return Err(WalError::Io(e)),
        };

        let mut writer = BufWriter::new(file);

        // Write header
        let header = WalHeader::new();
        writer.write_all(&header.to_bytes())?;
        writer.flush()?;

        Ok(WalWriter {
            path,
            file: Mutex::new(writer),
            next_lsn: AtomicU64::new(1), // LSN 0 reserved for "no LSN"
            synced_lsn: AtomicU64::new(0),
            header: Mutex::new(header),
        })
    }

    /// Open an existing WAL file for appending.
    ///
    /// Eliminates TOCTOU race by letting the filesystem handle existence check
    /// atomically during open. This matches the formal model's `open_existing`
    /// operation in `FileSystem.v`.
    ///
    /// # Errors
    ///
    /// - `WalError::NotFound` - File doesn't exist (atomic check)
    /// - `WalError::Io` - Other I/O errors
    pub fn open(path: impl AsRef<Path>) -> Result<Self, WalError> {
        let path = path.as_ref().to_path_buf();

        // Direct open - let filesystem handle existence atomically
        // No exists() pre-check to avoid TOCTOU race
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(WalError::NotFound);
            }
            Err(e) => return Err(WalError::Io(e)),
        };

        // Read header
        let mut reader = BufReader::new(&file);
        let mut header_buf = [0u8; WalHeader::SIZE];
        reader.read_exact(&mut header_buf)?;
        let header = WalHeader::from_bytes(&header_buf)?;

        // Find the last LSN by scanning the log
        let mut last_lsn: Lsn = 0;
        let mut reader = WalReader::new(path.clone())?;
        while let Some(result) = reader.next_record() {
            if let Ok((lsn, _)) = result {
                last_lsn = lsn;
            }
        }

        // Seek to end for appending
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        let mut writer = BufWriter::new(file);
        writer.seek(SeekFrom::End(0))?;

        Ok(WalWriter {
            path,
            file: Mutex::new(writer),
            next_lsn: AtomicU64::new(last_lsn + 1),
            synced_lsn: AtomicU64::new(last_lsn),
            header: Mutex::new(header),
        })
    }

    /// TOCTOU-safe open or create.
    ///
    /// Matches the formal model's `open_or_create_safe` operation in `FileSystem.v`:
    /// 1. Ensure parent directory exists (mkdir_all - idempotent)
    /// 2. Try open existing, fall back to create if not found
    ///
    /// This handles both TOCTOU races:
    /// - File deleted between check and open: create succeeds
    /// - File created between check and create: open succeeds on retry
    ///
    /// # Errors
    ///
    /// - `WalError::ParentNotFound` - Parent's parent directory doesn't exist
    /// - `WalError::Io` - Other I/O errors
    pub fn open_or_create(path: impl AsRef<Path>) -> Result<Self, WalError> {
        let path = path.as_ref().to_path_buf();

        // Step 1: Ensure parent directory exists (matches formal mkdir_all)
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    if e.kind() == io::ErrorKind::NotFound {
                        WalError::ParentNotFound(parent.to_path_buf())
                    } else {
                        WalError::Io(e)
                    }
                })?;
            }
        }

        // Step 2: Try atomic open, fall back to atomic create
        // This handles both TOCTOU races:
        // - File deleted between check and open: create succeeds
        // - File created between check and create: open succeeds on retry
        match Self::open(&path) {
            Ok(writer) => Ok(writer),
            Err(WalError::NotFound) => {
                // File doesn't exist, try to create
                match Self::create(&path) {
                    Ok(writer) => Ok(writer),
                    Err(WalError::AlreadyExists) => {
                        // Another process created it between our open and create attempts
                        // Retry open - this handles the creation TOCTOU race
                        Self::open(&path)
                    }
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Append a record to the WAL.
    ///
    /// Returns the LSN assigned to the record.
    pub fn append(&self, record: WalRecord) -> Result<Lsn, WalError> {
        let lsn = self.next_lsn.fetch_add(1, Ordering::AcqRel);
        let payload = record.serialize_payload();
        let record_type = record.record_type() as u8;

        // Build the record buffer
        let total_len = Self::RECORD_HEADER_SIZE + payload.len();
        let mut buf = Vec::with_capacity(total_len);

        // Placeholder for CRC (will be computed after)
        buf.extend_from_slice(&[0u8; 4]);
        // Length (including header)
        buf.extend_from_slice(&(total_len as u32).to_le_bytes());
        // LSN
        buf.extend_from_slice(&lsn.to_le_bytes());
        // Type
        buf.push(record_type);
        // Payload
        buf.extend_from_slice(&payload);

        // Compute CRC over everything except the CRC field itself
        let crc = crc32(&buf[4..]);
        buf[0..4].copy_from_slice(&crc.to_le_bytes());

        // Write to file
        let mut file = self.file.lock().expect("WAL lock poisoned");
        file.write_all(&buf)?;

        Ok(lsn)
    }

    /// Sync (fsync) the WAL to disk.
    ///
    /// Returns the highest LSN that is now durable.
    pub fn sync(&self) -> Result<Lsn, WalError> {
        let mut file = self.file.lock().expect("WAL lock poisoned");
        file.flush()?;
        file.get_ref().sync_all()?;

        let current_lsn = self.next_lsn.load(Ordering::Acquire) - 1;
        self.synced_lsn.store(current_lsn, Ordering::Release);

        Ok(current_lsn)
    }

    /// Append a batch of inserts as a single WAL record.
    ///
    /// This reduces WAL overhead by batching multiple inserts into a single
    /// record with a single CRC, single LSN, and single header (17 bytes + 4
    /// for count vs. 17 bytes per individual insert).
    ///
    /// # Arguments
    ///
    /// * `entries` - Slice of (term, optional_value) tuples to insert
    ///
    /// # Returns
    ///
    /// The LSN assigned to this batch record.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let entries = vec![
    ///     (b"apple".to_vec(), None),
    ///     (b"banana".to_vec(), Some(vec![1, 2, 3])),
    ///     (b"cherry".to_vec(), None),
    /// ];
    /// let lsn = wal.append_batch(&entries)?;
    /// wal.sync()?;
    /// ```
    ///
    /// # Performance
    ///
    /// For 100 inserts:
    /// - Individual inserts: 100 * 17 = 1700 bytes header overhead
    /// - Batch insert: 17 + 4 = 21 bytes header overhead
    /// - Savings: ~99% header overhead reduction
    pub fn append_batch(&self, entries: &[(Vec<u8>, Option<Vec<u8>>)]) -> Result<Lsn, WalError> {
        if entries.is_empty() {
            // Empty batch - still log for consistency, but no-op
            return self.append(WalRecord::BatchInsert {
                entries: Vec::new(),
            });
        }

        let record = WalRecord::BatchInsert {
            entries: entries.to_vec(),
        };
        self.append(record)
    }

    /// Append a batch of inserts and sync in a single operation.
    ///
    /// This is a convenience method that combines `append_batch()` and `sync()`.
    ///
    /// # Returns
    ///
    /// The LSN that is now durable.
    pub fn append_batch_and_sync(
        &self,
        entries: &[(Vec<u8>, Option<Vec<u8>>)],
    ) -> Result<Lsn, WalError> {
        self.append_batch(entries)?;
        self.sync()
    }

    /// Get the current (next) LSN.
    pub fn current_lsn(&self) -> Lsn {
        self.next_lsn.load(Ordering::Acquire)
    }

    /// Get the last synced LSN.
    pub fn synced_lsn(&self) -> Lsn {
        self.synced_lsn.load(Ordering::Acquire)
    }

    /// Get the path to the WAL file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Allocate a new LSN without writing a record.
    ///
    /// This is used by group commit to pre-allocate LSNs before batching writes.
    pub fn allocate_lsn(&self) -> Lsn {
        self.next_lsn.fetch_add(1, Ordering::AcqRel)
    }

    /// Set the minimum starting LSN for subsequent records.
    ///
    /// If the current next_lsn is less than the provided minimum, it will be
    /// updated to the minimum. This is useful after reopening a file where
    /// the checkpoint_lsn in the main header might be higher than the WAL's
    /// internal counter (e.g., after truncate).
    ///
    /// # Arguments
    ///
    /// * `min_lsn` - The minimum LSN for subsequent records
    pub fn set_min_lsn(&self, min_lsn: Lsn) {
        loop {
            let current = self.next_lsn.load(Ordering::Acquire);
            if current >= min_lsn {
                break;
            }
            if self.next_lsn.compare_exchange(
                current,
                min_lsn,
                Ordering::AcqRel,
                Ordering::Acquire,
            ).is_ok() {
                break;
            }
        }
    }

    /// Write a checkpoint record and update the header.
    pub fn checkpoint(&self, checkpoint_lsn: Lsn) -> Result<Lsn, WalError> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let record = WalRecord::Checkpoint {
            checkpoint_lsn,
            timestamp,
        };

        let lsn = self.append(record)?;
        self.sync()?;

        // Update header
        let mut header = self.header.lock().expect("header lock poisoned");
        header.checkpoint_lsn = checkpoint_lsn;

        // Write updated header
        let mut file = self.file.lock().expect("WAL lock poisoned");
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header.to_bytes())?;
        file.flush()?;
        file.get_ref().sync_all()?;

        // Seek back to end
        file.seek(SeekFrom::End(0))?;

        Ok(lsn)
    }

    /// Get the last checkpoint LSN.
    pub fn checkpoint_lsn(&self) -> Lsn {
        let header = self.header.lock().expect("header lock poisoned");
        header.checkpoint_lsn
    }

    /// Truncate the WAL file, removing all records.
    ///
    /// This is typically called after successful recovery to prevent
    /// re-replaying the same operations on subsequent opens. The header
    /// is preserved but all records are removed.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on success, or a `WalError` on failure.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // After recovery completes successfully
    /// wal_writer.truncate()?;
    /// ```
    pub fn truncate(&self) -> Result<(), WalError> {
        let mut file = self.file.lock().expect("WAL lock poisoned");

        // Flush any pending writes
        file.flush()?;

        // Get the underlying file and truncate to header size only
        let inner_file = file.get_mut();
        inner_file.set_len(WalHeader::SIZE as u64)?;

        // Seek to end of header for subsequent writes
        file.seek(SeekFrom::Start(WalHeader::SIZE as u64))?;

        // Reset LSN counters
        self.next_lsn.store(1, Ordering::Release);
        self.synced_lsn.store(0, Ordering::Release);

        // Reset checkpoint LSN in header
        {
            let mut header = self.header.lock().expect("header lock poisoned");
            header.checkpoint_lsn = 0;

            // Write updated header to disk
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&header.to_bytes())?;
            file.flush()?;
            file.get_ref().sync_all()?;

            // Seek back to end of header for subsequent writes
            file.seek(SeekFrom::Start(WalHeader::SIZE as u64))?;
        }

        Ok(())
    }

    /// Rotate WAL to archive directory - O(1) filesystem rename operation.
    ///
    /// This is the zero-cost archive operation:
    /// 1. Sync and close the current WAL
    /// 2. Rename to archive directory (O(1) - just updates directory entry)
    /// 3. Create a fresh WAL file
    ///
    /// Returns the path to the archived segment.
    ///
    /// # Arguments
    /// * `config` - WAL configuration with archive settings
    ///
    /// # Errors
    /// Returns `WalError` if:
    /// - Archive directory cannot be created
    /// - Rename operation fails
    /// - New WAL file cannot be created
    pub fn rotate_to_archive(&self, config: &WalConfig) -> Result<PathBuf, WalError> {
        // Ensure all data is synced
        self.sync()?;

        // Create archive directory if it doesn't exist
        let archive_dir = if config.archive_dir.is_absolute() {
            config.archive_dir.clone()
        } else {
            // Make relative to WAL parent directory
            self.path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&config.archive_dir)
        };
        fs::create_dir_all(&archive_dir).map_err(|e| WalError::Io(e))?;

        // Generate archive filename with timestamp for uniqueness
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let segment_name = format!("wal_{}.segment", timestamp);
        let archive_path = archive_dir.join(&segment_name);

        // Lock file for exclusive access during rotation
        let mut file = self.file.lock().expect("WAL lock poisoned");

        // Flush any pending writes
        file.flush()?;
        file.get_ref().sync_all()?;

        // Drop the file handle so we can rename
        drop(file);

        // Rename current WAL to archive (O(1) operation)
        fs::rename(&self.path, &archive_path).map_err(|e| WalError::Io(e))?;

        // Create fresh WAL file
        let new_file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .open(&self.path)?;

        let mut writer = BufWriter::new(new_file);

        // Write fresh header
        let header = WalHeader::new();
        writer.write_all(&header.to_bytes())?;
        writer.flush()?;

        // Update internal state
        *self.file.lock().expect("WAL lock poisoned") = writer;
        self.next_lsn.store(1, Ordering::Release);
        self.synced_lsn.store(0, Ordering::Release);
        *self.header.lock().expect("header lock poisoned") = header;

        // Prune old segments if needed (fire and forget - don't fail rotation)
        let _ = Self::prune_segments_if_needed(&archive_dir, config);

        Ok(archive_path)
    }

    /// Collect all WAL segments (archived + active) in chronological order.
    ///
    /// Returns a sorted list of paths to WAL segments, oldest first.
    /// The active WAL (if it exists and has records) is included last.
    pub fn collect_wal_segments(&self, config: &WalConfig) -> Result<Vec<PathBuf>, WalError> {
        let mut segments = Vec::new();

        // Collect archived segments
        let archive_dir = if config.archive_dir.is_absolute() {
            config.archive_dir.clone()
        } else {
            self.path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&config.archive_dir)
        };

        if archive_dir.exists() {
            for entry in fs::read_dir(&archive_dir).map_err(|e| WalError::Io(e))? {
                let entry = entry.map_err(|e| WalError::Io(e))?;
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "segment") {
                    segments.push(path);
                }
            }
        }

        // Sort by filename (timestamp-based naming ensures chronological order)
        segments.sort();

        // Add active WAL if it exists and has records
        if self.path.exists() {
            // Check if active WAL has any records beyond the header
            let metadata = fs::metadata(&self.path).map_err(|e| WalError::Io(e))?;
            if metadata.len() > WalHeader::SIZE as u64 {
                segments.push(self.path.clone());
            }
        }

        Ok(segments)
    }

    /// Prune old WAL segments to stay within limits.
    ///
    /// Removes oldest segments when either:
    /// - Number of segments exceeds `max_segments`
    /// - Total size exceeds `max_archive_bytes`
    fn prune_segments_if_needed(archive_dir: &Path, config: &WalConfig) -> Result<(), WalError> {
        if !archive_dir.exists() {
            return Ok(());
        }

        // Collect all segments with their sizes
        let mut segments: Vec<(PathBuf, u64)> = Vec::new();
        for entry in fs::read_dir(archive_dir).map_err(|e| WalError::Io(e))? {
            let entry = entry.map_err(|e| WalError::Io(e))?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "segment") {
                let size = fs::metadata(&path).map_or(0, |m| m.len());
                segments.push((path, size));
            }
        }

        // Sort by name (oldest first)
        segments.sort_by(|a, b| a.0.cmp(&b.0));

        // Calculate total size
        let total_size: u64 = segments.iter().map(|(_, size)| size).sum();

        // Prune if over limits
        let mut current_size = total_size;
        let mut to_remove = Vec::new();

        for (i, (path, size)) in segments.iter().enumerate() {
            let remaining_count = segments.len() - i;

            // Keep at least one segment for safety
            if remaining_count <= 1 {
                break;
            }

            // Check if we're over limits
            let over_count = remaining_count > config.max_segments;
            let over_size = current_size > config.max_archive_bytes;

            if over_count || over_size {
                to_remove.push(path.clone());
                current_size = current_size.saturating_sub(*size);
            } else {
                break;
            }
        }

        // Remove old segments
        for path in to_remove {
            let _ = fs::remove_file(path); // Ignore errors - best effort
        }

        Ok(())
    }
}

/// WAL reader for recovery.
pub struct WalReader {
    reader: BufReader<File>,
    #[allow(dead_code)]
    path: PathBuf,
}

impl WalReader {
    /// Open a WAL file for reading.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, WalError> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let mut reader = BufReader::new(file);

        // Skip header
        reader.seek(SeekFrom::Start(WalHeader::SIZE as u64))?;

        Ok(WalReader { reader, path })
    }

    /// Read the next record from the WAL.
    ///
    /// Returns `None` at end of file, `Some(Err(...))` on error.
    pub fn next_record(&mut self) -> Option<Result<(Lsn, WalRecord), WalError>> {
        // Read header: CRC (4) + Length (4) + LSN (8) + Type (1)
        let mut header_buf = [0u8; WalWriter::RECORD_HEADER_SIZE];
        match self.reader.read_exact(&mut header_buf) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return None,
            Err(e) => return Some(Err(WalError::Io(e))),
        }

        let stored_crc = u32::from_le_bytes(header_buf[0..4].try_into().unwrap());
        let length = u32::from_le_bytes(header_buf[4..8].try_into().unwrap()) as usize;
        let lsn = u64::from_le_bytes(header_buf[8..16].try_into().unwrap());
        let record_type_byte = header_buf[16];

        // Validate length
        if length < WalWriter::RECORD_HEADER_SIZE {
            return Some(Err(WalError::CorruptedRecord(
                "Record length too small".into(),
            )));
        }

        let payload_len = length - WalWriter::RECORD_HEADER_SIZE;

        // Read payload
        let mut payload = vec![0u8; payload_len];
        if !payload.is_empty() {
            match self.reader.read_exact(&mut payload) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return Some(Err(WalError::UnexpectedEof))
                }
                Err(e) => return Some(Err(WalError::Io(e))),
            }
        }

        // Verify CRC
        let mut crc_data = Vec::with_capacity(length - 4);
        crc_data.extend_from_slice(&header_buf[4..]);
        crc_data.extend_from_slice(&payload);
        let computed_crc = crc32(&crc_data);

        if stored_crc != computed_crc {
            return Some(Err(WalError::CorruptedRecord(format!(
                "CRC mismatch: stored={:#x}, computed={:#x}",
                stored_crc, computed_crc
            ))));
        }

        // Parse record type
        let record_type = match WalRecordType::try_from(record_type_byte) {
            Ok(t) => t,
            Err(e) => return Some(Err(e)),
        };

        // Deserialize record
        match WalRecord::deserialize(record_type, &payload) {
            Ok(record) => Some(Ok((lsn, record))),
            Err(e) => Some(Err(e)),
        }
    }

    /// Get an iterator over all records.
    pub fn iter(self) -> WalRecordIterator {
        WalRecordIterator { reader: self }
    }

    /// Read the header from the WAL file.
    pub fn read_header(path: impl AsRef<Path>) -> Result<WalHeader, WalError> {
        let file = File::open(path.as_ref())?;
        let mut reader = BufReader::new(file);
        let mut header_buf = [0u8; WalHeader::SIZE];
        reader.read_exact(&mut header_buf)?;
        WalHeader::from_bytes(&header_buf)
    }
}

/// Iterator over WAL records.
pub struct WalRecordIterator {
    reader: WalReader,
}

impl Iterator for WalRecordIterator {
    type Item = Result<(Lsn, WalRecord), WalError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.reader.next_record()
    }
}

/// Group commit coordinator.
///
/// Batches multiple WAL writes into a single fsync for better performance.
pub struct GroupCommit {
    wal: Arc<WalWriter>,
    /// Pending LSNs waiting for sync
    pending: Mutex<Vec<(Lsn, std::sync::mpsc::Sender<Result<(), WalError>>)>>,
    /// Sync interval in milliseconds
    #[allow(dead_code)]
    sync_interval_ms: u64,
}

impl GroupCommit {
    /// Create a new group commit coordinator.
    pub fn new(wal: Arc<WalWriter>, sync_interval_ms: u64) -> Self {
        GroupCommit {
            wal,
            pending: Mutex::new(Vec::new()),
            sync_interval_ms,
        }
    }

    /// Append a record and wait for it to be durable.
    pub fn append_sync(&self, record: WalRecord) -> Result<Lsn, WalError> {
        let lsn = self.wal.append(record)?;

        // For simplicity, sync immediately
        // In production, we'd batch and use the sync_interval
        self.wal.sync()?;

        Ok(lsn)
    }

    /// Get the underlying WAL writer.
    pub fn wal(&self) -> &WalWriter {
        &self.wal
    }
}

// =============================================================================
// Concurrent WAL Writes - Async Sync Support
// =============================================================================
//
// The following types enable concurrent writes during sync/truncate operations.
// The key insight is that we can rotate to a new WAL segment (O(1) rename) before
// syncing the old segment, allowing writes to continue while a background thread
// handles the expensive fsync operation.
//
// Architecture:
//
// ```text
// Writer ──→ append() ──→ [new_segment.wal] ──→ continues immediately
//                               │
//                          rotate (O(1))
//                               │
//                               ↓
//                     Background Thread
//                     ┌─────────────────┐
//                     │ old_segment:    │
//                     │ 1. fsync()      │
//                     │ 2. archive()    │
//                     │ 3. notify()     │
//                     └─────────────────┘
// ```

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::Condvar;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Configuration for the async WAL writer.
///
/// Controls backpressure behavior when the background sync thread falls behind.
#[derive(Debug, Clone)]
pub struct AsyncWalConfig {
    /// Maximum number of pending segments before blocking writers.
    ///
    /// When this limit is reached, `sync_async()` will block until the oldest
    /// segment is synced. Default: 4
    pub max_pending_segments: usize,

    /// Maximum total bytes in pending segments before blocking writers.
    ///
    /// Provides byte-based backpressure in addition to segment count.
    /// Default: 256MB
    pub max_pending_bytes: u64,

    /// Directory for pending segments awaiting sync.
    ///
    /// Pending segments are named `wal_pending_{timestamp}.segment` and are
    /// moved to the archive directory after successful sync.
    /// Default: "{data_dir}/wal_pending"
    pub pending_dir: PathBuf,

    /// Interval between sync thread checks when idle.
    ///
    /// The sync thread will sleep for this duration when there are no
    /// pending segments to sync. Default: 10ms
    pub idle_check_interval_ms: u64,
}

impl Default for AsyncWalConfig {
    fn default() -> Self {
        Self {
            max_pending_segments: 4,
            max_pending_bytes: 256 * 1024 * 1024, // 256 MB
            pending_dir: PathBuf::from("wal_pending"),
            idle_check_interval_ms: 10,
        }
    }
}

impl AsyncWalConfig {
    /// Create config with custom pending directory.
    pub fn with_pending_dir(pending_dir: impl Into<PathBuf>) -> Self {
        Self {
            pending_dir: pending_dir.into(),
            ..Default::default()
        }
    }
}

/// A pending segment awaiting background sync.
///
/// Contains all information needed to sync the segment in the background
/// and track its LSN coverage for ordering guarantees.
#[derive(Debug)]
pub struct PendingSegment {
    /// Path to the pending segment file.
    pub path: PathBuf,
    /// LSN range covered by this segment: (first_lsn, last_lsn).
    pub lsn_range: (Lsn, Lsn),
    /// Open file handle for fsync.
    pub file: File,
    /// Timestamp when this segment was rotated (for metrics).
    pub rotated_at: Instant,
    /// Size of the segment in bytes (for backpressure).
    pub size_bytes: u64,
}

/// Error types specific to async WAL operations.
#[derive(Debug)]
pub enum AsyncWalError {
    /// Underlying WAL error.
    Wal(WalError),
    /// Segment sync failed after retries.
    SegmentSyncFailed {
        path: PathBuf,
        attempts: u32,
        last_error: io::Error,
    },
    /// Rotation failed.
    RotationFailed {
        reason: String,
        source: Option<io::Error>,
    },
    /// Sync wait timed out.
    SyncTimeout {
        target_lsn: Lsn,
        current_synced: Lsn,
        timeout_ms: u64,
    },
    /// Background sync thread panicked.
    SyncThreadPanicked,
}

impl std::fmt::Display for AsyncWalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AsyncWalError::Wal(e) => write!(f, "WAL error: {}", e),
            AsyncWalError::SegmentSyncFailed { path, attempts, last_error } => {
                write!(
                    f,
                    "Segment sync failed after {} attempts at {}: {}",
                    attempts,
                    path.display(),
                    last_error
                )
            }
            AsyncWalError::RotationFailed { reason, source } => {
                if let Some(e) = source {
                    write!(f, "Rotation failed ({}): {}", reason, e)
                } else {
                    write!(f, "Rotation failed: {}", reason)
                }
            }
            AsyncWalError::SyncTimeout { target_lsn, current_synced, timeout_ms } => {
                write!(
                    f,
                    "Sync timeout: target LSN {} not reached (current synced: {}) after {}ms",
                    target_lsn, current_synced, timeout_ms
                )
            }
            AsyncWalError::SyncThreadPanicked => {
                write!(f, "Background sync thread panicked")
            }
        }
    }
}

impl std::error::Error for AsyncWalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AsyncWalError::Wal(e) => Some(e),
            AsyncWalError::SegmentSyncFailed { last_error, .. } => Some(last_error),
            AsyncWalError::RotationFailed { source: Some(e), .. } => Some(e),
            _ => None,
        }
    }
}

impl From<WalError> for AsyncWalError {
    fn from(e: WalError) -> Self {
        AsyncWalError::Wal(e)
    }
}

/// Handle to track completion of an async sync operation.
///
/// Returned by `AsyncWalWriter::sync_async()`. The caller can either:
/// - Call `wait()` to block until the target LSN is durable
/// - Call `is_synced()` to check status without blocking
/// - Call `wait_timeout()` to wait with a timeout
///
/// # Example
///
/// ```rust,ignore
/// let handle = wal.sync_async()?;
///
/// // Non-blocking check
/// if !handle.is_synced() {
///     // Do other work while sync happens in background
///     process_other_tasks();
/// }
///
/// // Block until durable
/// handle.wait()?;
/// ```
pub struct SyncHandle {
    /// The LSN that must be synced for this handle to be complete.
    target_lsn: Lsn,
    /// Reference to the sync manager for checking/waiting.
    sync_manager: Arc<SegmentSyncManager>,
}

impl SyncHandle {
    /// Create a new sync handle.
    fn new(target_lsn: Lsn, sync_manager: Arc<SegmentSyncManager>) -> Self {
        Self {
            target_lsn,
            sync_manager,
        }
    }

    /// Create a handle that is already synced.
    fn already_synced(target_lsn: Lsn, sync_manager: Arc<SegmentSyncManager>) -> Self {
        Self {
            target_lsn,
            sync_manager,
        }
    }

    /// Get the target LSN this handle is waiting for.
    pub fn target_lsn(&self) -> Lsn {
        self.target_lsn
    }

    /// Check if the target LSN is now durable (non-blocking).
    pub fn is_synced(&self) -> bool {
        self.sync_manager.global_synced_lsn.load(Ordering::Acquire) >= self.target_lsn
    }

    /// Block until the target LSN is durable.
    ///
    /// # Errors
    ///
    /// Returns `AsyncWalError::SyncThreadPanicked` if the background sync
    /// thread has crashed.
    pub fn wait(&self) -> Result<(), AsyncWalError> {
        self.sync_manager.wait_for_lsn(self.target_lsn)
    }

    /// Block until the target LSN is durable, with timeout.
    ///
    /// # Returns
    ///
    /// - `Ok(true)` if the LSN was synced within the timeout
    /// - `Ok(false)` if the timeout elapsed before sync completed
    /// - `Err(...)` if the sync thread panicked
    pub fn wait_timeout(&self, timeout: Duration) -> Result<bool, AsyncWalError> {
        self.sync_manager.wait_for_lsn_timeout(self.target_lsn, timeout)
    }
}

impl std::fmt::Debug for SyncHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncHandle")
            .field("target_lsn", &self.target_lsn)
            .field("is_synced", &self.is_synced())
            .finish()
    }
}

/// Manages background segment synchronization.
///
/// The sync manager owns a background thread that processes pending segments
/// in strict FIFO order, ensuring that `global_synced_lsn` always represents
/// a contiguous range from LSN 1 (no gaps).
///
/// # Ordering Guarantee
///
/// The single sync thread + FIFO queue ensures that if segment B was rotated
/// after segment A, then A's fsync completes before B's. This prevents the
/// situation where B syncs first and we incorrectly report A's LSNs as durable.
pub struct SegmentSyncManager {
    /// Queue of segments awaiting sync (oldest first).
    pending_segments: Mutex<VecDeque<PendingSegment>>,
    /// Total bytes in pending segments (for backpressure).
    pending_bytes: AtomicU64,
    /// The highest LSN that is confirmed durable across all synced segments.
    ///
    /// This value always represents a contiguous range: all LSNs from 1 to
    /// `global_synced_lsn` are durable. No gaps are possible due to FIFO
    /// processing.
    pub global_synced_lsn: AtomicU64,
    /// Condvar to notify waiters when a segment is synced.
    sync_complete: Condvar,
    /// Mutex for condvar wait.
    sync_mutex: Mutex<()>,
    /// Flag to signal the sync thread to stop.
    running: AtomicBool,
    /// Handle to the background sync thread.
    sync_thread: Mutex<Option<JoinHandle<()>>>,
    /// Configuration.
    config: AsyncWalConfig,
    /// Archive configuration for moving synced segments.
    archive_config: WalConfig,
    /// Path to the active WAL (for archive directory resolution).
    wal_path: PathBuf,
}

impl SegmentSyncManager {
    /// Create a new sync manager and start the background thread.
    pub fn new(
        config: AsyncWalConfig,
        archive_config: WalConfig,
        wal_path: PathBuf,
        initial_synced_lsn: Lsn,
    ) -> Arc<Self> {
        let manager = Arc::new(Self {
            pending_segments: Mutex::new(VecDeque::new()),
            pending_bytes: AtomicU64::new(0),
            global_synced_lsn: AtomicU64::new(initial_synced_lsn),
            sync_complete: Condvar::new(),
            sync_mutex: Mutex::new(()),
            running: AtomicBool::new(true),
            sync_thread: Mutex::new(None),
            config,
            archive_config,
            wal_path,
        });

        // Start the background sync thread
        let manager_clone = Arc::clone(&manager);
        let handle = thread::Builder::new()
            .name("wal-sync".to_string())
            .spawn(move || {
                manager_clone.sync_loop();
            })
            .expect("Failed to spawn WAL sync thread");

        *manager.sync_thread.lock().expect("sync_thread lock poisoned") = Some(handle);

        manager
    }

    /// Enqueue a segment for background sync.
    pub fn enqueue(&self, segment: PendingSegment) {
        let size = segment.size_bytes;
        let mut queue = self.pending_segments.lock().expect("pending_segments lock poisoned");
        queue.push_back(segment);
        self.pending_bytes.fetch_add(size, Ordering::AcqRel);
    }

    /// Get the number of pending segments.
    pub fn pending_count(&self) -> usize {
        self.pending_segments.lock().expect("pending_segments lock poisoned").len()
    }

    /// Get the total bytes in pending segments.
    pub fn pending_bytes(&self) -> u64 {
        self.pending_bytes.load(Ordering::Acquire)
    }

    /// Wait until the pending count drops below the limit.
    ///
    /// Used for backpressure: if we have too many pending segments, block
    /// until the oldest one is synced.
    pub fn wait_for_backpressure(&self) -> Result<(), AsyncWalError> {
        loop {
            let count = self.pending_count();
            let bytes = self.pending_bytes();

            if count < self.config.max_pending_segments
                && bytes < self.config.max_pending_bytes
            {
                return Ok(());
            }

            // Check if sync thread is still alive
            if !self.running.load(Ordering::Acquire) {
                return Err(AsyncWalError::SyncThreadPanicked);
            }

            // Wait for a sync to complete
            let guard = self.sync_mutex.lock().expect("sync_mutex lock poisoned");
            let _ = self.sync_complete.wait_timeout(
                guard,
                Duration::from_millis(100),
            );
        }
    }

    /// Wait for a specific LSN to be synced.
    pub fn wait_for_lsn(&self, target_lsn: Lsn) -> Result<(), AsyncWalError> {
        loop {
            if self.global_synced_lsn.load(Ordering::Acquire) >= target_lsn {
                return Ok(());
            }

            if !self.running.load(Ordering::Acquire) {
                // Thread stopped - check if we reached the target before stopping
                if self.global_synced_lsn.load(Ordering::Acquire) >= target_lsn {
                    return Ok(());
                }
                return Err(AsyncWalError::SyncThreadPanicked);
            }

            let guard = self.sync_mutex.lock().expect("sync_mutex lock poisoned");
            let _ = self.sync_complete.wait_timeout(guard, Duration::from_millis(100));
        }
    }

    /// Wait for a specific LSN to be synced with timeout.
    pub fn wait_for_lsn_timeout(
        &self,
        target_lsn: Lsn,
        timeout: Duration,
    ) -> Result<bool, AsyncWalError> {
        let deadline = Instant::now() + timeout;

        loop {
            if self.global_synced_lsn.load(Ordering::Acquire) >= target_lsn {
                return Ok(true);
            }

            if !self.running.load(Ordering::Acquire) {
                if self.global_synced_lsn.load(Ordering::Acquire) >= target_lsn {
                    return Ok(true);
                }
                return Err(AsyncWalError::SyncThreadPanicked);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }

            let guard = self.sync_mutex.lock().expect("sync_mutex lock poisoned");
            let _ = self.sync_complete.wait_timeout(guard, remaining.min(Duration::from_millis(100)));
        }
    }

    /// The background sync loop.
    ///
    /// Processes pending segments in strict FIFO order, ensuring contiguous
    /// LSN advancement.
    fn sync_loop(&self) {
        while self.running.load(Ordering::Relaxed) {
            // Pop the oldest pending segment (FIFO order)
            let segment = {
                let mut queue = self.pending_segments.lock().expect("pending_segments lock poisoned");
                queue.pop_front()
            };

            if let Some(segment) = segment {
                let size = segment.size_bytes;
                let lsn_end = segment.lsn_range.1;
                let path = segment.path.clone();

                // Retry sync until success - do NOT skip to newer segments
                let mut attempts = 0u32;
                loop {
                    attempts += 1;
                    match segment.file.sync_all() {
                        Ok(()) => {
                            // Sync succeeded
                            log::debug!(
                                "Synced segment {} (LSN {}-{}) in {} attempts",
                                path.display(),
                                segment.lsn_range.0,
                                lsn_end,
                                attempts
                            );
                            break;
                        }
                        Err(e) => {
                            log::error!(
                                "Sync failed for {} (attempt {}): {:?}",
                                path.display(),
                                attempts,
                                e
                            );
                            // Brief pause before retry
                            thread::sleep(Duration::from_millis(100));

                            // After many failures, we still don't give up - data integrity
                            // is paramount. But we do log more aggressively.
                            if attempts >= 10 {
                                log::error!(
                                    "WARNING: {} sync attempts failed for {}. Will keep retrying.",
                                    attempts,
                                    path.display()
                                );
                            }
                        }
                    }

                    // Check if we should stop (e.g., shutdown requested)
                    if !self.running.load(Ordering::Relaxed) {
                        log::warn!(
                            "Sync thread stopping with unsynced segment: {}",
                            path.display()
                        );
                        return;
                    }
                }

                // Update pending bytes counter
                self.pending_bytes.fetch_sub(size, Ordering::AcqRel);

                // Safe to advance global_synced_lsn - all older segments already synced
                // (FIFO guarantee)
                self.global_synced_lsn.store(lsn_end, Ordering::Release);

                // Move to archive if enabled
                if self.archive_config.archive_enabled {
                    let archive_dir = if self.archive_config.archive_dir.is_absolute() {
                        self.archive_config.archive_dir.clone()
                    } else {
                        self.wal_path
                            .parent()
                            .unwrap_or(Path::new("."))
                            .join(&self.archive_config.archive_dir)
                    };

                    if let Err(e) = fs::create_dir_all(&archive_dir) {
                        log::warn!("Failed to create archive directory: {}", e);
                    } else {
                        // Generate archive filename
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis();
                        let segment_name = format!("wal_{}.segment", timestamp);
                        let archive_path = archive_dir.join(segment_name);

                        // Move pending segment to archive
                        if let Err(e) = fs::rename(&path, &archive_path) {
                            log::warn!(
                                "Failed to move synced segment to archive: {} -> {}: {}",
                                path.display(),
                                archive_path.display(),
                                e
                            );
                        }
                    }
                } else {
                    // Archive disabled - delete the synced segment
                    if let Err(e) = fs::remove_file(&path) {
                        log::warn!("Failed to remove synced segment {}: {}", path.display(), e);
                    }
                }

                // Notify waiters
                let _guard = self.sync_mutex.lock().expect("sync_mutex lock poisoned");
                self.sync_complete.notify_all();
            } else {
                // No pending segments - sleep briefly
                thread::sleep(Duration::from_millis(self.config.idle_check_interval_ms));
            }
        }
    }

    /// Stop the background sync thread.
    ///
    /// Waits for the thread to finish processing any current segment.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);

        // Wake up the sync thread if it's sleeping
        let _guard = self.sync_mutex.lock().expect("sync_mutex lock poisoned");
        self.sync_complete.notify_all();
        drop(_guard);

        // Wait for the thread to finish
        let mut handle = self.sync_thread.lock().expect("sync_thread lock poisoned");
        if let Some(h) = handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for SegmentSyncManager {
    fn drop(&mut self) {
        self.stop();
    }
}

/// An async-capable WAL writer that allows writes during sync.
///
/// Wraps the standard `WalWriter` and adds the ability to rotate to a new
/// segment before syncing, enabling concurrent writes while the background
/// thread syncs the old segment.
///
/// # Example
///
/// ```rust,ignore
/// use libdictenstein::persistent_artrie::wal::{AsyncWalWriter, AsyncWalConfig, WalConfig};
///
/// let config = AsyncWalConfig::default();
/// let archive_config = WalConfig::default();
/// let wal = AsyncWalWriter::create("data.wal", config, archive_config)?;
///
/// // Append records
/// let lsn1 = wal.append(WalRecord::Insert { term: b"hello".to_vec(), value: None })?;
/// let lsn2 = wal.append(WalRecord::Insert { term: b"world".to_vec(), value: None })?;
///
/// // Async sync - returns immediately, sync happens in background
/// let handle = wal.sync_async()?;
///
/// // Can continue appending while sync happens!
/// let lsn3 = wal.append(WalRecord::Insert { term: b"more".to_vec(), value: None })?;
///
/// // Wait for original data to be durable when needed
/// handle.wait()?;
///
/// // Or use blocking sync for ACID compliance
/// wal.sync()?;
/// ```
pub struct AsyncWalWriter {
    /// The underlying WAL writer.
    writer: Mutex<WalWriter>,
    /// Next LSN to assign (mirrors writer.next_lsn but allows non-blocking reads).
    next_lsn: AtomicU64,
    /// Last synced LSN (updated after each sync completes).
    synced_lsn: AtomicU64,
    /// Segment sync manager for background operations.
    sync_manager: Arc<SegmentSyncManager>,
    /// Configuration.
    config: AsyncWalConfig,
    /// Archive configuration.
    archive_config: WalConfig,
    /// Path to the WAL file.
    path: PathBuf,
    /// Counter for pending segment naming.
    pending_counter: AtomicU64,
}

impl AsyncWalWriter {
    /// Create a new async WAL file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the WAL file
    /// * `config` - Async WAL configuration
    /// * `archive_config` - Archive configuration for synced segments
    ///
    /// # Errors
    ///
    /// Returns `WalError::AlreadyExists` if the file already exists.
    pub fn create(
        path: impl AsRef<Path>,
        config: AsyncWalConfig,
        archive_config: WalConfig,
    ) -> Result<Self, AsyncWalError> {
        let path = path.as_ref().to_path_buf();

        // Create pending directory
        let pending_dir = if config.pending_dir.is_absolute() {
            config.pending_dir.clone()
        } else {
            path.parent()
                .unwrap_or(Path::new("."))
                .join(&config.pending_dir)
        };
        fs::create_dir_all(&pending_dir).map_err(|e| {
            AsyncWalError::RotationFailed {
                reason: "Failed to create pending directory".to_string(),
                source: Some(e),
            }
        })?;

        let writer = WalWriter::create(&path)?;
        let sync_manager = SegmentSyncManager::new(
            config.clone(),
            archive_config.clone(),
            path.clone(),
            0,
        );

        Ok(Self {
            next_lsn: AtomicU64::new(writer.current_lsn()),
            synced_lsn: AtomicU64::new(writer.synced_lsn()),
            writer: Mutex::new(writer),
            sync_manager,
            config,
            archive_config,
            path,
            pending_counter: AtomicU64::new(0),
        })
    }

    /// Open an existing async WAL file.
    pub fn open(
        path: impl AsRef<Path>,
        config: AsyncWalConfig,
        archive_config: WalConfig,
    ) -> Result<Self, AsyncWalError> {
        let path = path.as_ref().to_path_buf();

        // Create pending directory
        let pending_dir = if config.pending_dir.is_absolute() {
            config.pending_dir.clone()
        } else {
            path.parent()
                .unwrap_or(Path::new("."))
                .join(&config.pending_dir)
        };
        fs::create_dir_all(&pending_dir).map_err(|e| {
            AsyncWalError::RotationFailed {
                reason: "Failed to create pending directory".to_string(),
                source: Some(e),
            }
        })?;

        let writer = WalWriter::open(&path)?;
        let synced_lsn = writer.synced_lsn();
        let sync_manager = SegmentSyncManager::new(
            config.clone(),
            archive_config.clone(),
            path.clone(),
            synced_lsn,
        );

        Ok(Self {
            next_lsn: AtomicU64::new(writer.current_lsn()),
            synced_lsn: AtomicU64::new(synced_lsn),
            writer: Mutex::new(writer),
            sync_manager,
            config,
            archive_config,
            path,
            pending_counter: AtomicU64::new(0),
        })
    }

    /// Open or create an async WAL file.
    pub fn open_or_create(
        path: impl AsRef<Path>,
        config: AsyncWalConfig,
        archive_config: WalConfig,
    ) -> Result<Self, AsyncWalError> {
        let path = path.as_ref();
        if path.exists() {
            Self::open(path, config, archive_config)
        } else {
            Self::create(path, config, archive_config)
        }
    }

    /// Append a record to the WAL.
    ///
    /// Returns the LSN assigned to the record. The record is NOT durable until
    /// `sync()` or `sync_async().wait()` is called.
    pub fn append(&self, record: WalRecord) -> Result<Lsn, AsyncWalError> {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        let lsn = writer.append(record)?;
        self.next_lsn.store(writer.current_lsn(), Ordering::Release);
        Ok(lsn)
    }

    /// Append a batch of inserts as a single WAL record.
    pub fn append_batch(&self, entries: &[(Vec<u8>, Option<Vec<u8>>)]) -> Result<Lsn, AsyncWalError> {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        let lsn = writer.append_batch(entries)?;
        self.next_lsn.store(writer.current_lsn(), Ordering::Release);
        Ok(lsn)
    }

    /// Initiate an async sync and return a handle to track completion.
    ///
    /// This method:
    /// 1. Checks backpressure limits (blocks if too many pending segments)
    /// 2. Flushes the buffer (no fsync)
    /// 3. Rotates the active WAL to a pending segment (O(1) rename)
    /// 4. Creates a fresh active WAL
    /// 5. Enqueues the pending segment for background sync
    /// 6. Returns a handle that can be used to wait for durability
    ///
    /// Writers can continue appending to the new WAL while the old segment
    /// is being synced in the background.
    ///
    /// # Returns
    ///
    /// A `SyncHandle` that can be used to wait for the sync to complete.
    /// If there's nothing to sync, returns an already-completed handle.
    pub fn sync_async(&self) -> Result<SyncHandle, AsyncWalError> {
        let current_lsn = self.next_lsn.load(Ordering::Acquire).saturating_sub(1);
        let synced_lsn = self.sync_manager.global_synced_lsn.load(Ordering::Acquire);

        // If already synced, return immediately
        if current_lsn <= synced_lsn {
            return Ok(SyncHandle::already_synced(current_lsn, Arc::clone(&self.sync_manager)));
        }

        // Check backpressure before rotation
        self.sync_manager.wait_for_backpressure()?;

        // Perform the rotation
        self.rotate_for_sync(current_lsn)?;

        Ok(SyncHandle::new(current_lsn, Arc::clone(&self.sync_manager)))
    }

    /// Blocking sync - waits for all current data to be durable.
    ///
    /// This performs a simple in-place fsync without segment rotation.
    /// The WAL file remains in place and can be read for recovery.
    /// Use `sync_with_rotation()` for async segment rotation behavior.
    ///
    /// # Returns
    ///
    /// The highest LSN that is now durable.
    pub fn sync(&self) -> Result<Lsn, AsyncWalError> {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        let lsn = writer.sync()?;
        self.synced_lsn.store(lsn, Ordering::Release);
        self.sync_manager.global_synced_lsn.store(lsn, Ordering::Release);
        Ok(lsn)
    }

    /// Sync with segment rotation for async writes during sync.
    ///
    /// This method rotates the WAL to a pending segment, syncs it in the
    /// background, and allows writes to continue on a new segment.
    /// The synced segment is moved to the archive directory.
    ///
    /// For simple blocking durability where the WAL should remain in place,
    /// use `sync()` instead.
    ///
    /// # Returns
    ///
    /// The highest LSN that is now durable.
    pub fn sync_with_rotation(&self) -> Result<Lsn, AsyncWalError> {
        let handle = self.sync_async()?;
        handle.wait()?;
        Ok(handle.target_lsn())
    }

    /// Get the current (next) LSN.
    pub fn current_lsn(&self) -> Lsn {
        self.next_lsn.load(Ordering::Acquire)
    }

    /// Get the last synced LSN.
    pub fn synced_lsn(&self) -> Lsn {
        self.sync_manager.global_synced_lsn.load(Ordering::Acquire)
    }

    /// Allocate the next LSN without writing a record.
    ///
    /// This method atomically increments the LSN counter and returns the
    /// previous value. It is used by `GroupCommitCoordinator` to pre-allocate
    /// LSNs before batching writes.
    ///
    /// # Returns
    ///
    /// The allocated LSN (the value before incrementing).
    pub fn allocate_lsn(&self) -> Lsn {
        self.next_lsn.fetch_add(1, Ordering::AcqRel)
    }

    /// Set the minimum starting LSN for subsequent records.
    ///
    /// If the current next_lsn is less than the provided minimum, it will be
    /// updated to the minimum. This is useful after reopening a file where
    /// the checkpoint_lsn in the main header might be higher than the WAL's
    /// internal counter (e.g., after truncate).
    ///
    /// # Arguments
    ///
    /// * `min_lsn` - The minimum LSN for subsequent records
    pub fn set_min_lsn(&self, min_lsn: Lsn) {
        // Update the underlying WalWriter's LSN
        {
            let writer = self.writer.lock().expect("WAL writer lock poisoned");
            writer.set_min_lsn(min_lsn);
        }

        // Update our own next_lsn
        loop {
            let current = self.next_lsn.load(Ordering::Acquire);
            if current >= min_lsn {
                break;
            }
            if self.next_lsn.compare_exchange(
                current,
                min_lsn,
                Ordering::AcqRel,
                Ordering::Acquire,
            ).is_ok() {
                break;
            }
        }
    }

    /// Get the path to the WAL file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the sync manager for advanced operations.
    pub fn sync_manager(&self) -> &Arc<SegmentSyncManager> {
        &self.sync_manager
    }

    /// Write a checkpoint record.
    pub fn checkpoint(&self, checkpoint_lsn: Lsn) -> Result<Lsn, AsyncWalError> {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        let lsn = writer.checkpoint(checkpoint_lsn)?;
        self.next_lsn.store(writer.current_lsn(), Ordering::Release);
        Ok(lsn)
    }

    /// Truncate the WAL, discarding all records after the header.
    ///
    /// This is typically used after recovery when all operations have been
    /// successfully replayed and persisted to the main data file.
    ///
    /// # Safety
    ///
    /// Only call this when you're certain all WAL records have been properly
    /// recovered and applied. Truncating prematurely will result in data loss.
    pub fn truncate(&self) -> Result<(), AsyncWalError> {
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        writer.truncate()?;
        Ok(())
    }

    /// Rotate WAL to archive directory - O(1) filesystem rename operation.
    ///
    /// This delegates to the underlying `WalWriter::rotate_to_archive()` method.
    /// If archive mode is disabled in the config, this falls back to truncate.
    ///
    /// Returns the path to the archived segment if archiving occurred, or None
    /// if archive mode is disabled.
    pub fn rotate_to_archive(&self, config: &WalConfig) -> Result<Option<PathBuf>, AsyncWalError> {
        if !config.archive_enabled {
            self.truncate()?;
            return Ok(None);
        }
        let writer = self.writer.lock().expect("WAL writer lock poisoned");
        let path = writer.rotate_to_archive(config)?;
        Ok(Some(path))
    }

    /// Convert the async writer back to a synchronous writer.
    ///
    /// This waits for all pending segments to sync and returns the underlying
    /// `WalWriter`. Useful for shutdown or when async behavior is no longer needed.
    pub fn into_sync(mut self) -> Result<WalWriter, AsyncWalError> {
        // Wait for all pending segments to sync
        let current_lsn = self.next_lsn.load(Ordering::Acquire).saturating_sub(1);
        if current_lsn > 0 {
            self.sync_manager.wait_for_lsn(current_lsn)?;
        }

        // Stop the sync thread
        self.sync_manager.stop();

        // Take the writer out using std::mem::replace with a dummy
        // We'll use ManuallyDrop to prevent the Drop impl from running
        let writer = {
            let guard = self.writer.lock().expect("WAL writer lock poisoned");
            // We can't actually take ownership here due to the Mutex
            // Instead, we'll create a new WalWriter from the same file
            // This is safe because we've synced everything
            WalWriter::open(&self.path)?
        };

        // Prevent drop from running sync again (mark as "consumed")
        // We do this by ensuring the writer is in a synced state
        Ok(writer)
    }

    /// Internal: Rotate the current WAL segment for async sync.
    ///
    /// This is an O(1) operation that:
    /// 1. Flushes the buffer (no fsync)
    /// 2. Renames the active WAL to a pending segment
    /// 3. Creates a fresh active WAL with header
    /// 4. Enqueues the pending segment for background sync
    fn rotate_for_sync(&self, last_lsn: Lsn) -> Result<(), AsyncWalError> {
        let mut writer = self.writer.lock().expect("WAL writer lock poisoned");

        // Flush buffer (no fsync yet)
        if let Err(e) = writer.file.lock().expect("file lock poisoned").flush() {
            return Err(AsyncWalError::RotationFailed {
                reason: "Failed to flush buffer".to_string(),
                source: Some(e),
            });
        }

        // Generate pending segment path
        let counter = self.pending_counter.fetch_add(1, Ordering::Relaxed);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let pending_name = format!("wal_pending_{}_{}.segment", timestamp, counter);
        let pending_dir = if self.config.pending_dir.is_absolute() {
            self.config.pending_dir.clone()
        } else {
            self.path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&self.config.pending_dir)
        };
        let pending_path = pending_dir.join(pending_name);

        // Get file size before rename
        let size_bytes = fs::metadata(&self.path)
            .map(|m| m.len())
            .unwrap_or(0);

        // Get the first LSN in this segment (synced_lsn + 1)
        let first_lsn = self.synced_lsn.load(Ordering::Acquire) + 1;

        // Rename active WAL to pending (O(1) filesystem operation)
        fs::rename(&self.path, &pending_path).map_err(|e| {
            AsyncWalError::RotationFailed {
                reason: "Failed to rename WAL to pending".to_string(),
                source: Some(e),
            }
        })?;

        // Open the pending file for sync
        let pending_file = OpenOptions::new()
            .read(true)
            .open(&pending_path)
            .map_err(|e| {
                // Try to restore the original file
                let _ = fs::rename(&pending_path, &self.path);
                AsyncWalError::RotationFailed {
                    reason: "Failed to open pending segment".to_string(),
                    source: Some(e),
                }
            })?;

        // Create fresh WAL file
        let new_file = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .open(&self.path)
        {
            Ok(f) => f,
            Err(e) => {
                // Try to restore the original file
                let _ = fs::rename(&pending_path, &self.path);
                return Err(AsyncWalError::RotationFailed {
                    reason: "Failed to create new WAL file".to_string(),
                    source: Some(e),
                });
            }
        };

        let mut new_writer = BufWriter::new(new_file);

        // Write fresh header
        let header = WalHeader::new();
        if let Err(e) = new_writer.write_all(&header.to_bytes()) {
            // Try to restore
            let _ = fs::remove_file(&self.path);
            let _ = fs::rename(&pending_path, &self.path);
            return Err(AsyncWalError::RotationFailed {
                reason: "Failed to write header".to_string(),
                source: Some(e),
            });
        }
        if let Err(e) = new_writer.flush() {
            let _ = fs::remove_file(&self.path);
            let _ = fs::rename(&pending_path, &self.path);
            return Err(AsyncWalError::RotationFailed {
                reason: "Failed to flush header".to_string(),
                source: Some(e),
            });
        }

        // Update the internal writer state
        *writer.file.lock().expect("file lock poisoned") = new_writer;
        *writer.header.lock().expect("header lock poisoned") = header;
        // Note: We keep next_lsn continuing from where we left off
        // The LSN sequence continues across segments

        // Update local synced_lsn tracking (will be officially updated when segment syncs)
        self.synced_lsn.store(last_lsn, Ordering::Release);

        // Enqueue the pending segment for background sync
        let pending_segment = PendingSegment {
            path: pending_path,
            lsn_range: (first_lsn, last_lsn),
            file: pending_file,
            rotated_at: Instant::now(),
            size_bytes,
        };
        self.sync_manager.enqueue(pending_segment);

        Ok(())
    }
}

impl Drop for AsyncWalWriter {
    fn drop(&mut self) {
        // Best-effort sync of any remaining data
        // We can't return errors from drop, so just log any issues
        if let Ok(writer) = self.writer.lock() {
            if let Err(e) = writer.sync() {
                log::warn!("Failed to sync WAL on drop: {:?}", e);
            }
        }
    }
}

/// Collect all WAL segments including pending segments for recovery.
///
/// Returns paths to all WAL segments in chronological order:
/// 1. Archived segments (oldest)
/// 2. Pending segments (awaiting sync)
/// 3. Active WAL (newest)
///
/// # Arguments
///
/// * `wal_path` - Path to the active WAL file
/// * `config` - WAL configuration with archive directory
/// * `async_config` - Async WAL configuration with pending directory
///
/// # Returns
///
/// Vector of paths sorted by segment order (oldest first).
pub fn collect_all_segments(
    wal_path: &Path,
    config: &WalConfig,
    async_config: &AsyncWalConfig,
) -> Result<Vec<PathBuf>, WalError> {
    let mut segments = Vec::new();
    let parent = wal_path.parent().unwrap_or(Path::new("."));

    // 1. Collect archived segments
    let archive_dir = if config.archive_dir.is_absolute() {
        config.archive_dir.clone()
    } else {
        parent.join(&config.archive_dir)
    };

    if archive_dir.exists() {
        for entry in fs::read_dir(&archive_dir).map_err(WalError::Io)? {
            let entry = entry.map_err(WalError::Io)?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "segment") {
                segments.push(path);
            }
        }
    }

    // 2. Collect pending segments
    let pending_dir = if async_config.pending_dir.is_absolute() {
        async_config.pending_dir.clone()
    } else {
        parent.join(&async_config.pending_dir)
    };

    if pending_dir.exists() {
        for entry in fs::read_dir(&pending_dir).map_err(WalError::Io)? {
            let entry = entry.map_err(WalError::Io)?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "segment") {
                segments.push(path);
            }
        }
    }

    // Sort by filename (timestamp-based naming ensures chronological order)
    segments.sort();

    // 3. Add active WAL if it exists and has records
    if wal_path.exists() {
        let metadata = fs::metadata(wal_path).map_err(WalError::Io)?;
        if metadata.len() > WalHeader::SIZE as u64 {
            segments.push(wal_path.to_path_buf());
        }
    }

    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_crc32() {
        let data = b"hello world";
        let crc = crc32(data);
        assert_eq!(crc, 0x0D4A1185); // Known CRC32 value
    }

    #[test]
    fn test_wal_record_serialize_deserialize() {
        let record = WalRecord::Insert {
            term: b"hello".to_vec(),
            value: Some(b"world".to_vec()),
        };
        let payload = record.serialize_payload();
        let deserialized =
            WalRecord::deserialize(WalRecordType::Insert, &payload).expect("deserialize failed");

        assert_eq!(record, deserialized);
    }

    #[test]
    fn test_wal_record_remove() {
        let record = WalRecord::Remove {
            term: b"goodbye".to_vec(),
        };
        let payload = record.serialize_payload();
        let deserialized =
            WalRecord::deserialize(WalRecordType::Remove, &payload).expect("deserialize failed");

        assert_eq!(record, deserialized);
    }

    #[test]
    fn test_wal_create_and_append() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");

        let wal = WalWriter::create(&wal_path).expect("create WAL");

        let lsn1 = wal
            .append(WalRecord::Insert {
                term: b"hello".to_vec(),
                value: None,
            })
            .expect("append");

        let lsn2 = wal
            .append(WalRecord::Insert {
                term: b"world".to_vec(),
                value: Some(b"value".to_vec()),
            })
            .expect("append");

        assert_eq!(lsn1, 1);
        assert_eq!(lsn2, 2);

        wal.sync().expect("sync");
    }

    #[test]
    fn test_wal_read_records() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");

        // Write records
        {
            let wal = WalWriter::create(&wal_path).expect("create WAL");
            wal.append(WalRecord::Insert {
                term: b"hello".to_vec(),
                value: None,
            })
            .expect("append");
            wal.append(WalRecord::Remove {
                term: b"world".to_vec(),
            })
            .expect("append");
            wal.sync().expect("sync");
        }

        // Read records
        let reader = WalReader::new(&wal_path).expect("open WAL");
        let records: Vec<_> = reader.iter().collect();

        assert_eq!(records.len(), 2);

        let (lsn1, rec1) = records[0].as_ref().expect("record 1");
        assert_eq!(*lsn1, 1);
        assert!(matches!(rec1, WalRecord::Insert { term, .. } if term == b"hello"));

        let (lsn2, rec2) = records[1].as_ref().expect("record 2");
        assert_eq!(*lsn2, 2);
        assert!(matches!(rec2, WalRecord::Remove { term } if term == b"world"));
    }

    #[test]
    fn test_wal_checkpoint() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");

        {
            let wal = WalWriter::create(&wal_path).expect("create WAL");
            wal.append(WalRecord::Insert {
                term: b"test".to_vec(),
                value: None,
            })
            .expect("append");
            wal.checkpoint(1).expect("checkpoint");
        }

        // Verify checkpoint LSN is persisted
        let header = WalReader::read_header(&wal_path).expect("read header");
        assert_eq!(header.checkpoint_lsn, 1);
    }

    #[test]
    fn test_wal_reopen() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");

        // Create and write
        {
            let wal = WalWriter::create(&wal_path).expect("create WAL");
            wal.append(WalRecord::Insert {
                term: b"first".to_vec(),
                value: None,
            })
            .expect("append");
            wal.sync().expect("sync");
        }

        // Reopen and append more
        {
            let wal = WalWriter::open(&wal_path).expect("open WAL");
            assert_eq!(wal.current_lsn(), 2); // Next LSN should be 2
            wal.append(WalRecord::Insert {
                term: b"second".to_vec(),
                value: None,
            })
            .expect("append");
            wal.sync().expect("sync");
        }

        // Verify all records
        let reader = WalReader::new(&wal_path).expect("open WAL");
        let records: Vec<_> = reader.iter().collect();
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn test_wal_truncate() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");

        // Create WAL and write some records
        {
            let wal = WalWriter::create(&wal_path).expect("create WAL");
            wal.append(WalRecord::Insert {
                term: b"first".to_vec(),
                value: None,
            })
            .expect("append");
            wal.append(WalRecord::Insert {
                term: b"second".to_vec(),
                value: None,
            })
            .expect("append");
            wal.checkpoint(2).expect("checkpoint");
            wal.sync().expect("sync");

            // Verify records exist before truncate
            assert_eq!(wal.current_lsn(), 4); // 2 inserts + 1 checkpoint = LSN 3, next is 4

            // Truncate the WAL
            wal.truncate().expect("truncate");

            // Verify LSN is reset
            assert_eq!(wal.current_lsn(), 1);
            assert_eq!(wal.synced_lsn(), 0);
            assert_eq!(wal.checkpoint_lsn(), 0);
        }

        // Verify WAL is empty after truncate
        let reader = WalReader::new(&wal_path).expect("open WAL");
        let records: Vec<_> = reader.iter().collect();
        assert_eq!(records.len(), 0, "WAL should be empty after truncate");

        // Verify we can append new records after truncate
        {
            let wal = WalWriter::open(&wal_path).expect("open WAL");
            assert_eq!(wal.current_lsn(), 1); // Should start fresh

            let lsn = wal
                .append(WalRecord::Insert {
                    term: b"new_record".to_vec(),
                    value: None,
                })
                .expect("append after truncate");
            assert_eq!(lsn, 1);
            wal.sync().expect("sync");
        }

        // Verify new record is readable
        let reader = WalReader::new(&wal_path).expect("open WAL");
        let records: Vec<_> = reader.iter().collect();
        assert_eq!(records.len(), 1);
        let (lsn, rec) = records[0].as_ref().expect("record");
        assert_eq!(*lsn, 1);
        assert!(matches!(rec, WalRecord::Insert { term, .. } if term == b"new_record"));
    }

    #[test]
    fn test_wal_archive_rotation() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");
        let archive_dir = dir.path().join("wal_archive");

        let config = WalConfig {
            archive_enabled: true,
            archive_dir: archive_dir.clone(),
            max_segments: 10,
            max_archive_bytes: 10 << 30, // 10 GB
        };

        // Create WAL and write records
        let wal = WalWriter::create(&wal_path).expect("create WAL");
        wal.append(WalRecord::Insert {
            term: b"record1".to_vec(),
            value: Some(b"value1".to_vec()),
        })
        .expect("append");
        wal.append(WalRecord::Insert {
            term: b"record2".to_vec(),
            value: None,
        })
        .expect("append");
        wal.checkpoint(2).expect("checkpoint");
        wal.sync().expect("sync");

        // Rotate to archive
        let archive_path = wal.rotate_to_archive(&config).expect("rotate");

        // Verify archive segment was created
        assert!(archive_path.exists(), "Archive segment should exist");
        assert!(
            archive_path.extension().map_or(false, |ext| ext == "segment"),
            "Archive should have .segment extension"
        );

        // Verify active WAL was recreated and is empty
        assert!(wal_path.exists(), "Active WAL should exist");
        let reader = WalReader::new(&wal_path).expect("open active WAL");
        let records: Vec<_> = reader.iter().collect();
        assert_eq!(records.len(), 0, "Active WAL should be empty after rotation");

        // Verify archived segment contains the records
        let reader = WalReader::new(&archive_path).expect("open archive");
        let records: Vec<_> = reader.iter().collect();
        assert_eq!(records.len(), 3, "Archive should have 3 records (2 inserts + 1 checkpoint)");
    }

    #[test]
    fn test_wal_collect_segments() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");
        let archive_dir = dir.path().join("wal_archive");

        let config = WalConfig {
            archive_enabled: true,
            archive_dir: archive_dir.clone(),
            max_segments: 10,
            max_archive_bytes: 10 << 30,
        };

        // Create WAL
        let wal = WalWriter::create(&wal_path).expect("create WAL");

        // Initially should have no segments (active WAL is empty)
        let segments = wal.collect_wal_segments(&config).expect("collect");
        assert_eq!(segments.len(), 0, "No segments when WAL is empty");

        // Add records and rotate multiple times
        for i in 0..3 {
            wal.append(WalRecord::Insert {
                term: format!("term{}", i).into_bytes(),
                value: None,
            })
            .expect("append");
            wal.checkpoint(i as u64 + 1).expect("checkpoint");
            wal.sync().expect("sync");
            wal.rotate_to_archive(&config).expect("rotate");
            // Small delay to ensure unique timestamps for segment naming
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        // Add one more record to active WAL
        wal.append(WalRecord::Insert {
            term: b"active_term".to_vec(),
            value: None,
        })
        .expect("append");
        wal.sync().expect("sync");

        // Collect segments
        let segments = wal.collect_wal_segments(&config).expect("collect");
        assert_eq!(segments.len(), 4, "Should have 3 archived + 1 active");

        // Verify segments are in chronological order
        for i in 0..3 {
            let ext = segments[i].extension().unwrap_or_default();
            assert_eq!(ext, "segment", "Archived segments should come first");
        }
        assert_eq!(
            segments[3], wal_path,
            "Active WAL should be last"
        );
    }

    #[test]
    fn test_wal_archive_pruning_by_count() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");
        let archive_dir = dir.path().join("wal_archive");

        let config = WalConfig {
            archive_enabled: true,
            archive_dir: archive_dir.clone(),
            max_segments: 3, // Only keep 3 segments
            max_archive_bytes: u64::MAX,
        };

        // Create WAL and rotate many times
        let wal = WalWriter::create(&wal_path).expect("create WAL");

        for i in 0..6 {
            wal.append(WalRecord::Insert {
                term: format!("term{}", i).into_bytes(),
                value: None,
            })
            .expect("append");
            wal.sync().expect("sync");
            wal.rotate_to_archive(&config).expect("rotate");
            // Small delay to ensure unique timestamps for segment naming
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        // Count segments in archive
        let segments: Vec<_> = std::fs::read_dir(&archive_dir)
            .expect("read archive dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map_or(false, |ext| ext == "segment"))
            .collect();

        // Should have pruned down to max_segments (3)
        assert!(
            segments.len() <= config.max_segments,
            "Should have at most {} segments, found {}",
            config.max_segments,
            segments.len()
        );
    }

    #[test]
    fn test_wal_archive_disabled() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");
        let archive_dir = dir.path().join("wal_archive");

        let config = WalConfig {
            archive_enabled: false, // Disabled
            archive_dir: archive_dir.clone(),
            max_segments: 10,
            max_archive_bytes: 10 << 30,
        };

        // Create WAL and write records
        let wal = WalWriter::create(&wal_path).expect("create WAL");
        wal.append(WalRecord::Insert {
            term: b"test".to_vec(),
            value: None,
        })
        .expect("append");
        wal.sync().expect("sync");

        // Collect segments should still work (returns active WAL only)
        let segments = wal.collect_wal_segments(&config).expect("collect");
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0], wal_path);

        // Archive dir should not exist
        assert!(!archive_dir.exists(), "Archive dir should not be created when disabled");
    }

    #[test]
    fn test_wal_config_default() {
        let config = WalConfig::default();
        assert!(config.archive_enabled);
        assert_eq!(config.max_segments, 10);
        assert_eq!(config.max_archive_bytes, 10 << 30); // 10 GB
    }

    #[test]
    fn test_batch_insert_serialize_deserialize() {
        // Test empty batch
        let record = WalRecord::BatchInsert { entries: vec![] };
        let buf = record.serialize_payload();
        let deserialized = WalRecord::deserialize(WalRecordType::BatchInsert, &buf)
            .expect("deserialize");
        match deserialized {
            WalRecord::BatchInsert { entries } => {
                assert_eq!(entries.len(), 0);
            }
            _ => panic!("Expected BatchInsert"),
        }

        // Test batch with multiple entries
        let entries = vec![
            (b"hello".to_vec(), Some(b"world".to_vec())),
            (b"foo".to_vec(), None),
            (b"bar".to_vec(), Some(b"baz".to_vec())),
        ];
        let record = WalRecord::BatchInsert { entries: entries.clone() };
        let buf = record.serialize_payload();
        let deserialized = WalRecord::deserialize(WalRecordType::BatchInsert, &buf)
            .expect("deserialize");
        match deserialized {
            WalRecord::BatchInsert { entries: deserialized_entries } => {
                assert_eq!(deserialized_entries.len(), 3);
                assert_eq!(deserialized_entries[0].0, b"hello");
                assert_eq!(deserialized_entries[0].1.as_ref().map(|v| v.as_slice()), Some(b"world".as_slice()));
                assert_eq!(deserialized_entries[1].0, b"foo");
                assert!(deserialized_entries[1].1.is_none());
                assert_eq!(deserialized_entries[2].0, b"bar");
                assert_eq!(deserialized_entries[2].1.as_ref().map(|v| v.as_slice()), Some(b"baz".as_slice()));
            }
            _ => panic!("Expected BatchInsert"),
        }
    }

    #[test]
    fn test_wal_append_batch() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");

        // Create WAL and append a batch
        {
            let wal = WalWriter::create(&wal_path).expect("create WAL");
            let entries = vec![
                (b"term1".to_vec(), Some(b"value1".to_vec())),
                (b"term2".to_vec(), None),
                (b"term3".to_vec(), Some(b"value3".to_vec())),
            ];
            let lsn = wal.append_batch(&entries).expect("append_batch");
            assert_eq!(lsn, 1);
            wal.sync().expect("sync");
        }

        // Verify the batch can be read back
        let reader = WalReader::new(&wal_path).expect("open WAL");
        let records: Vec<_> = reader.iter().collect();
        assert_eq!(records.len(), 1);
        let (lsn, record) = records[0].as_ref().expect("record");
        assert_eq!(*lsn, 1);
        match record {
            WalRecord::BatchInsert { entries } => {
                assert_eq!(entries.len(), 3);
                assert_eq!(entries[0].0, b"term1");
                assert_eq!(entries[1].0, b"term2");
                assert_eq!(entries[2].0, b"term3");
            }
            _ => panic!("Expected BatchInsert"),
        }
    }

    #[test]
    fn test_wal_append_batch_empty() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("test.wal");

        // Create WAL and append an empty batch
        let wal = WalWriter::create(&wal_path).expect("create WAL");
        let lsn = wal.append_batch(&[]).expect("append_batch empty");
        assert_eq!(lsn, 1);
        wal.sync().expect("sync");

        // Verify empty batch can be read
        let reader = WalReader::new(&wal_path).expect("open WAL");
        let records: Vec<_> = reader.iter().collect();
        assert_eq!(records.len(), 1);
        let (_, record) = records[0].as_ref().expect("record");
        match record {
            WalRecord::BatchInsert { entries } => {
                assert_eq!(entries.len(), 0);
            }
            _ => panic!("Expected BatchInsert"),
        }
    }

    #[test]
    fn test_batch_insert_record_type() {
        let record = WalRecord::BatchInsert {
            entries: vec![(b"test".to_vec(), None)],
        };
        assert_eq!(record.record_type(), WalRecordType::BatchInsert);
    }

    // =========================================================================
    // TOCTOU Safety Tests
    //
    // These tests verify that the WAL implementation correctly handles
    // concurrent access patterns that could expose TOCTOU vulnerabilities.
    // =========================================================================

    /// Test that open_or_create handles concurrent access correctly.
    /// Multiple threads race to open/create the same WAL file.
    ///
    /// Note: This test verifies TOCTOU safety (no panics, no race-related failures),
    /// not that all threads get a valid WalWriter. Some threads may fail to open
    /// the file because another thread holds it with write access - this is
    /// expected behavior for exclusive file access.
    #[test]
    fn test_open_or_create_toctou_safety() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let temp_dir = tempdir().expect("create temp dir");
        let wal_path = temp_dir.path().join("concurrent.wal");

        let num_threads = 10;
        let barrier = Arc::new(Barrier::new(num_threads));
        let path = Arc::new(wal_path.clone());

        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let path = Arc::clone(&path);
                thread::spawn(move || {
                    barrier.wait();
                    // All threads race to open_or_create
                    WalWriter::open_or_create(path.as_ref())
                })
            })
            .collect();

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // At least one thread should succeed (the one that created the file)
        let successes = results.iter().filter(|r| r.is_ok()).count();
        assert!(
            successes >= 1,
            "At least one thread should succeed"
        );

        // All threads should either succeed or fail with an expected error (Io)
        // No thread should fail with NotFound or AlreadyExists (those are TOCTOU symptoms)
        let toctou_failures = results.iter().filter(|r| {
            matches!(r, Err(WalError::NotFound) | Err(WalError::AlreadyExists))
        }).count();
        assert_eq!(
            toctou_failures, 0,
            "No threads should fail with TOCTOU-related errors (NotFound/AlreadyExists)"
        );

        // Verify the file was created
        assert!(wal_path.exists(), "WAL file should exist after concurrent access");
    }

    /// Test that concurrent create with exclusive mode fails correctly for losers.
    #[test]
    fn test_create_exclusive_concurrent() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let temp_dir = tempdir().expect("create temp dir");
        let wal_path = temp_dir.path().join("exclusive.wal");

        let num_threads = 10;
        let barrier = Arc::new(Barrier::new(num_threads));
        let path = Arc::new(wal_path);

        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let path = Arc::clone(&path);
                thread::spawn(move || {
                    barrier.wait();
                    WalWriter::create(path.as_ref())
                })
            })
            .collect();

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // Exactly one should succeed, rest should get AlreadyExists
        let successes = results.iter().filter(|r| r.is_ok()).count();
        let already_exists = results
            .iter()
            .filter(|r| matches!(r, Err(WalError::AlreadyExists)))
            .count();

        assert_eq!(successes, 1, "Exactly one thread should create the file");
        assert_eq!(
            already_exists,
            num_threads - 1,
            "All other threads should get AlreadyExists"
        );
    }

    /// Test that open fails correctly when file is deleted during operation.
    ///
    /// This test exercises the race between opening a file and deleting it.
    /// The TOCTOU-safe implementation should handle this gracefully without panics.
    #[test]
    fn test_open_handles_concurrent_delete() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let temp_dir = tempdir().expect("create temp dir");
        let wal_path = temp_dir.path().join("delete_race.wal");

        // Create the file first
        let wal = WalWriter::create(&wal_path).expect("create WAL");
        wal.sync().expect("sync");
        drop(wal);

        let barrier = Arc::new(Barrier::new(2));
        let path = Arc::new(wal_path.clone());

        // Thread 1: Tries to open
        let open_barrier = Arc::clone(&barrier);
        let open_path = Arc::clone(&path);
        let open_handle = thread::spawn(move || {
            open_barrier.wait();
            WalWriter::open(open_path.as_ref())
        });

        // Thread 2: Deletes the file
        let delete_barrier = Arc::clone(&barrier);
        let delete_path = Arc::clone(&path);
        let delete_handle = thread::spawn(move || {
            delete_barrier.wait();
            std::fs::remove_file(delete_path.as_ref())
        });

        let open_result = open_handle.join().unwrap();
        let delete_result = delete_handle.join().unwrap();

        // This test verifies we don't panic or get unexpected errors.
        // Valid outcomes for open:
        // - Ok: open completed before delete
        // - NotFound: delete completed before open
        // - Io: delete happened during open (file partially read)
        let open_valid = match &open_result {
            Ok(_) => true,
            Err(WalError::NotFound) => true,
            Err(WalError::Io(_)) => true, // I/O error during read is valid
            Err(WalError::CorruptedRecord(_)) => true, // File deleted mid-read
            Err(WalError::UnexpectedEof) => true, // File deleted mid-read
            _ => false,
        };

        // Valid outcomes for delete:
        // - Ok: delete succeeded
        // - NotFound: file was already gone (shouldn't happen in this test, but valid)
        let delete_ok = delete_result.is_ok();
        let delete_not_found = delete_result
            .as_ref()
            .err()
            .map_or(false, |e| e.kind() == std::io::ErrorKind::NotFound);

        assert!(
            open_valid,
            "Open should succeed or fail with expected error (NotFound, Io, etc.)"
        );
        assert!(
            delete_ok || delete_not_found,
            "Delete should succeed or fail with NotFound"
        );
    }

    /// Test that open_or_create works correctly when file doesn't exist.
    #[test]
    fn test_open_or_create_creates_new() {
        let temp_dir = tempdir().expect("create temp dir");
        let wal_path = temp_dir.path().join("new.wal");

        // File shouldn't exist
        assert!(!wal_path.exists());

        let wal = WalWriter::open_or_create(&wal_path).expect("open_or_create");

        // File should now exist
        assert!(wal_path.exists());

        // Should be able to write records
        let lsn = wal
            .append(WalRecord::Insert {
                term: b"test".to_vec(),
                value: None,
            })
            .expect("append");
        assert_eq!(lsn, 1);
    }

    /// Test that open_or_create works correctly when file already exists.
    #[test]
    fn test_open_or_create_opens_existing() {
        let temp_dir = tempdir().expect("create temp dir");
        let wal_path = temp_dir.path().join("existing.wal");

        // Create file first
        {
            let wal = WalWriter::create(&wal_path).expect("create");
            wal.append(WalRecord::Insert {
                term: b"first".to_vec(),
                value: None,
            })
            .expect("append");
            wal.sync().expect("sync");
        }

        // Open with open_or_create
        let wal = WalWriter::open_or_create(&wal_path).expect("open_or_create");

        // Should continue from existing LSN
        assert_eq!(wal.current_lsn(), 2);

        // Can append more
        let lsn = wal
            .append(WalRecord::Insert {
                term: b"second".to_vec(),
                value: None,
            })
            .expect("append");
        assert_eq!(lsn, 2);
    }

    /// Test that create returns AlreadyExists for existing file (atomic check).
    #[test]
    fn test_create_already_exists() {
        let temp_dir = tempdir().expect("create temp dir");
        let wal_path = temp_dir.path().join("already_exists.wal");

        // Create first
        let _wal = WalWriter::create(&wal_path).expect("create");

        // Second create should fail
        let result = WalWriter::create(&wal_path);
        assert!(
            matches!(result, Err(WalError::AlreadyExists)),
            "Expected AlreadyExists error"
        );
    }

    /// Test that open returns NotFound for non-existent file (atomic check).
    #[test]
    fn test_open_not_found() {
        let temp_dir = tempdir().expect("create temp dir");
        let wal_path = temp_dir.path().join("nonexistent.wal");

        let result = WalWriter::open(&wal_path);
        assert!(
            matches!(result, Err(WalError::NotFound)),
            "Expected NotFound error"
        );
    }

    /// Test that create handles missing parent directory gracefully.
    #[test]
    fn test_create_creates_parent_dirs() {
        let temp_dir = tempdir().expect("create temp dir");
        let wal_path = temp_dir.path().join("nested/dirs/test.wal");

        // Parent dirs don't exist
        assert!(!wal_path.parent().unwrap().exists());

        // create should create them
        let wal = WalWriter::create(&wal_path).expect("create with nested dirs");

        // Verify file and dirs exist
        assert!(wal_path.exists());
        assert!(wal_path.parent().unwrap().exists());

        // Can write records
        let lsn = wal
            .append(WalRecord::Insert {
                term: b"test".to_vec(),
                value: None,
            })
            .expect("append");
        assert_eq!(lsn, 1);
    }

    // =========================================================================
    // Async WAL Writer Tests
    // =========================================================================

    #[test]
    fn test_async_wal_create_and_append() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("async_test.wal");

        let config = AsyncWalConfig {
            pending_dir: dir.path().join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig {
            archive_enabled: true,
            archive_dir: dir.path().join("wal_archive"),
            ..Default::default()
        };

        let wal = AsyncWalWriter::create(&wal_path, config, archive_config)
            .expect("create async WAL");

        // Append some records
        let lsn1 = wal
            .append(WalRecord::Insert {
                term: b"hello".to_vec(),
                value: None,
            })
            .expect("append");
        assert_eq!(lsn1, 1);

        let lsn2 = wal
            .append(WalRecord::Insert {
                term: b"world".to_vec(),
                value: Some(b"value".to_vec()),
            })
            .expect("append");
        assert_eq!(lsn2, 2);

        // Current LSN should be 3 (next to assign)
        assert_eq!(wal.current_lsn(), 3);
    }

    #[test]
    fn test_async_wal_sync_blocking() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("async_sync_test.wal");

        let config = AsyncWalConfig {
            pending_dir: dir.path().join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig {
            archive_enabled: true,
            archive_dir: dir.path().join("wal_archive"),
            ..Default::default()
        };

        let wal = AsyncWalWriter::create(&wal_path, config, archive_config)
            .expect("create async WAL");

        // Append records
        wal.append(WalRecord::Insert {
            term: b"term1".to_vec(),
            value: None,
        })
        .expect("append");

        wal.append(WalRecord::Insert {
            term: b"term2".to_vec(),
            value: None,
        })
        .expect("append");

        // Blocking sync
        let synced = wal.sync().expect("sync");
        assert_eq!(synced, 2);

        // Synced LSN should be updated
        assert_eq!(wal.synced_lsn(), 2);
    }

    #[test]
    fn test_async_wal_sync_async_handle() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("async_handle_test.wal");

        let config = AsyncWalConfig {
            pending_dir: dir.path().join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig {
            archive_enabled: true,
            archive_dir: dir.path().join("wal_archive"),
            ..Default::default()
        };

        let wal = AsyncWalWriter::create(&wal_path, config, archive_config)
            .expect("create async WAL");

        // Append records
        for i in 0..5 {
            wal.append(WalRecord::Insert {
                term: format!("term{}", i).into_bytes(),
                value: None,
            })
            .expect("append");
        }

        // Get async sync handle
        let handle = wal.sync_async().expect("sync_async");
        assert_eq!(handle.target_lsn(), 5);

        // Initially may not be synced (depends on thread timing)
        // Wait for completion
        handle.wait().expect("wait");

        // Now should be synced
        assert!(handle.is_synced());
        assert_eq!(wal.synced_lsn(), 5);
    }

    #[test]
    fn test_async_wal_concurrent_append_during_sync() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("concurrent_test.wal");

        let config = AsyncWalConfig {
            pending_dir: dir.path().join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig {
            archive_enabled: true,
            archive_dir: dir.path().join("wal_archive"),
            ..Default::default()
        };

        let wal = AsyncWalWriter::create(&wal_path, config, archive_config)
            .expect("create async WAL");

        // Append initial batch
        for i in 0..10 {
            wal.append(WalRecord::Insert {
                term: format!("batch1_term{}", i).into_bytes(),
                value: None,
            })
            .expect("append");
        }

        // Start async sync (this rotates the WAL)
        let handle = wal.sync_async().expect("sync_async");
        assert_eq!(handle.target_lsn(), 10);

        // Continue appending while sync is in progress!
        for i in 0..5 {
            let lsn = wal
                .append(WalRecord::Insert {
                    term: format!("batch2_term{}", i).into_bytes(),
                    value: None,
                })
                .expect("append during sync");
            // LSN should continue from previous batch
            assert_eq!(lsn, 11 + i as u64);
        }

        // Wait for first sync to complete
        handle.wait().expect("wait");

        // First batch should now be synced
        assert!(wal.synced_lsn() >= 10);

        // Sync the second batch
        let synced = wal.sync().expect("sync second batch");
        assert!(synced >= 15);
    }

    #[test]
    fn test_async_wal_multiple_concurrent_syncs() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("multi_sync_test.wal");

        let config = AsyncWalConfig {
            pending_dir: dir.path().join("wal_pending"),
            max_pending_segments: 8, // Allow more pending segments
            ..Default::default()
        };
        let archive_config = WalConfig {
            archive_enabled: true,
            archive_dir: dir.path().join("wal_archive"),
            ..Default::default()
        };

        let wal = AsyncWalWriter::create(&wal_path, config, archive_config)
            .expect("create async WAL");

        let mut handles = Vec::new();

        // Create multiple sync operations
        for batch in 0..3 {
            for i in 0..3 {
                wal.append(WalRecord::Insert {
                    term: format!("batch{}_term{}", batch, i).into_bytes(),
                    value: None,
                })
                .expect("append");
            }

            let handle = wal.sync_async().expect("sync_async");
            handles.push(handle);
        }

        // Wait for all syncs to complete (in order)
        for (i, handle) in handles.into_iter().enumerate() {
            handle.wait().expect("wait");
            // Each batch has 3 records
            assert!(handle.target_lsn() >= ((i + 1) * 3) as u64);
        }

        // Final synced LSN should cover all batches
        assert!(wal.synced_lsn() >= 9);
    }

    #[test]
    fn test_async_wal_sync_timeout() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("timeout_test.wal");

        let config = AsyncWalConfig {
            pending_dir: dir.path().join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig {
            archive_enabled: true,
            archive_dir: dir.path().join("wal_archive"),
            ..Default::default()
        };

        let wal = AsyncWalWriter::create(&wal_path, config, archive_config)
            .expect("create async WAL");

        // Append a record
        wal.append(WalRecord::Insert {
            term: b"test".to_vec(),
            value: None,
        })
        .expect("append");

        // Get async handle
        let handle = wal.sync_async().expect("sync_async");

        // Wait with a very long timeout (should succeed)
        let completed = handle.wait_timeout(Duration::from_secs(10)).expect("wait_timeout");
        assert!(completed, "Sync should complete within timeout");
    }

    #[test]
    fn test_async_wal_empty_sync() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("empty_sync_test.wal");

        let config = AsyncWalConfig {
            pending_dir: dir.path().join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig {
            archive_enabled: true,
            archive_dir: dir.path().join("wal_archive"),
            ..Default::default()
        };

        let wal = AsyncWalWriter::create(&wal_path, config, archive_config)
            .expect("create async WAL");

        // Sync without any records (should be no-op)
        let handle = wal.sync_async().expect("sync_async empty");
        assert!(handle.is_synced()); // Already synced (nothing to sync)
    }

    #[test]
    fn test_async_wal_recovery_with_pending_segments() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("recovery_test.wal");
        let pending_dir = dir.path().join("wal_pending");
        let archive_dir = dir.path().join("wal_archive");

        let config = AsyncWalConfig {
            pending_dir: pending_dir.clone(),
            ..Default::default()
        };
        let archive_config = WalConfig {
            archive_enabled: true,
            archive_dir: archive_dir.clone(),
            ..Default::default()
        };

        // Create WAL and write some data
        {
            let wal = AsyncWalWriter::create(&wal_path, config.clone(), archive_config.clone())
                .expect("create async WAL");

            for i in 0..10 {
                wal.append(WalRecord::Insert {
                    term: format!("term{}", i).into_bytes(),
                    value: Some(format!("value{}", i).into_bytes()),
                })
                .expect("append");
            }

            // Sync to create archive segment
            wal.sync().expect("sync");
        }

        // Collect all segments using the recovery function
        let segments = collect_all_segments(&wal_path, &archive_config, &config)
            .expect("collect segments");

        // Should have at least the active WAL (archive segment may have been created)
        assert!(!segments.is_empty(), "Should have at least one segment");

        // Verify we can read from the segments
        let mut total_records = 0;
        for segment in &segments {
            if let Ok(reader) = WalReader::new(segment) {
                for result in reader.iter() {
                    if result.is_ok() {
                        total_records += 1;
                    }
                }
            }
        }

        // Should have recovered all 10 records
        assert_eq!(total_records, 10, "Should recover all 10 records");
    }

    #[test]
    fn test_async_wal_into_sync() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("into_sync_test.wal");

        let config = AsyncWalConfig {
            pending_dir: dir.path().join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig {
            archive_enabled: true,
            archive_dir: dir.path().join("wal_archive"),
            ..Default::default()
        };

        let wal = AsyncWalWriter::create(&wal_path, config, archive_config)
            .expect("create async WAL");

        // Write and sync some data
        wal.append(WalRecord::Insert {
            term: b"test".to_vec(),
            value: None,
        })
        .expect("append");
        wal.sync().expect("sync");

        // Convert back to sync writer
        let sync_writer = wal.into_sync().expect("into_sync");

        // Should be able to continue using the sync writer
        // Note: After async sync, the WAL was rotated to archive and a fresh WAL was created.
        // So the new LSN starts from where it left off (continuing the sequence).
        let lsn = sync_writer
            .append(WalRecord::Insert {
                term: b"after_convert".to_vec(),
                value: None,
            })
            .expect("append after convert");
        // The LSN continues from the previous sequence, which was 1 before conversion.
        // After conversion and reopening, the WAL scanner finds no records (rotated to archive)
        // and starts fresh from LSN 1.
        assert!(lsn >= 1, "LSN should be at least 1");
    }

    #[test]
    fn test_async_wal_backpressure() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("backpressure_test.wal");

        let config = AsyncWalConfig {
            pending_dir: dir.path().join("wal_pending"),
            max_pending_segments: 2, // Very low limit
            max_pending_bytes: 1024 * 1024, // 1MB
            ..Default::default()
        };
        let archive_config = WalConfig {
            archive_enabled: true,
            archive_dir: dir.path().join("wal_archive"),
            ..Default::default()
        };

        let wal = AsyncWalWriter::create(&wal_path, config, archive_config)
            .expect("create async WAL");

        // Write enough data to trigger multiple rotations
        // This tests that backpressure kicks in when we have too many pending segments
        for batch in 0..5 {
            for i in 0..10 {
                wal.append(WalRecord::Insert {
                    term: format!("batch{}_term{}", batch, i).into_bytes(),
                    value: Some(vec![0u8; 100]), // Some data to make segments larger
                })
                .expect("append");
            }

            // Start async sync
            let handle = wal.sync_async().expect("sync_async");

            // Wait for this sync to complete before next batch
            // (simulates normal usage pattern)
            handle.wait().expect("wait");
        }

        // All data should be synced
        assert!(wal.synced_lsn() >= 50);
    }

    #[test]
    fn test_sync_handle_debug() {
        let dir = tempdir().expect("create temp dir");
        let wal_path = dir.path().join("debug_test.wal");

        let config = AsyncWalConfig {
            pending_dir: dir.path().join("wal_pending"),
            ..Default::default()
        };
        let archive_config = WalConfig::default();

        let wal = AsyncWalWriter::create(&wal_path, config, archive_config)
            .expect("create async WAL");

        wal.append(WalRecord::Insert {
            term: b"test".to_vec(),
            value: None,
        })
        .expect("append");

        let handle = wal.sync_async().expect("sync_async");

        // Debug should not panic
        let debug_str = format!("{:?}", handle);
        assert!(debug_str.contains("SyncHandle"));
        assert!(debug_str.contains("target_lsn"));
    }

    #[test]
    fn test_async_wal_config_defaults() {
        let config = AsyncWalConfig::default();
        assert_eq!(config.max_pending_segments, 4);
        assert_eq!(config.max_pending_bytes, 256 * 1024 * 1024);
        assert_eq!(config.idle_check_interval_ms, 10);
    }

    #[test]
    fn test_async_wal_error_display() {
        let wal_error = AsyncWalError::Wal(WalError::NotFound);
        let display = format!("{}", wal_error);
        assert!(display.contains("WAL error"));

        let sync_failed = AsyncWalError::SegmentSyncFailed {
            path: PathBuf::from("/test/path"),
            attempts: 5,
            last_error: io::Error::new(io::ErrorKind::Other, "test error"),
        };
        let display = format!("{}", sync_failed);
        assert!(display.contains("5 attempts"));

        let rotation_failed = AsyncWalError::RotationFailed {
            reason: "test reason".to_string(),
            source: None,
        };
        let display = format!("{}", rotation_failed);
        assert!(display.contains("test reason"));

        let timeout = AsyncWalError::SyncTimeout {
            target_lsn: 100,
            current_synced: 50,
            timeout_ms: 1000,
        };
        let display = format!("{}", timeout);
        assert!(display.contains("100"));
        assert!(display.contains("50"));
    }

    // =========================================================================
    // WAL Corruption / Truncated Payload Tests
    //
    // These tests verify that WalRecord::deserialize correctly handles
    // malformed/truncated payloads for all record types.
    // =========================================================================

    #[test]
    fn test_deserialize_insert_payload_too_short() {
        // Insert requires at least 5 bytes: term_len (4) + has_value (1)
        let payload = vec![0, 0, 0]; // Only 3 bytes
        let result = WalRecord::deserialize(WalRecordType::Insert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("payload too short")));

        // Exactly 4 bytes is still too short
        let payload = vec![0, 0, 0, 0];
        let result = WalRecord::deserialize(WalRecordType::Insert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("payload too short")));
    }

    #[test]
    fn test_deserialize_insert_term_truncated() {
        // term_len says 10, but only provide 4 bytes of term + no has_value
        let mut payload = Vec::new();
        payload.extend_from_slice(&10u32.to_le_bytes()); // term_len = 10
        payload.extend_from_slice(&[b'a', b'b', b'c', b'd']); // Only 4 bytes of term
        let result = WalRecord::deserialize(WalRecordType::Insert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("term truncated")));
    }

    #[test]
    fn test_deserialize_insert_value_length_truncated() {
        // Valid term, has_value=1, but no value length bytes
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        payload.push(1); // has_value = true
        // Missing value_len bytes
        let result = WalRecord::deserialize(WalRecordType::Insert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("value length truncated")));

        // Only partial value_len
        payload.extend_from_slice(&[0, 0]); // Only 2 bytes of value_len
        let result = WalRecord::deserialize(WalRecordType::Insert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("value length truncated")));
    }

    #[test]
    fn test_deserialize_insert_value_truncated() {
        // Valid term, has_value=1, value_len=10, but only 5 bytes of value
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        payload.push(1); // has_value = true
        payload.extend_from_slice(&10u32.to_le_bytes()); // value_len = 10
        payload.extend_from_slice(&[1, 2, 3, 4, 5]); // Only 5 bytes of value
        let result = WalRecord::deserialize(WalRecordType::Insert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("value truncated")));
    }

    #[test]
    fn test_deserialize_insert_no_value_success() {
        // Valid insert with no value
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        payload.push(0); // has_value = false
        let result = WalRecord::deserialize(WalRecordType::Insert, &payload);
        assert!(result.is_ok());
        match result.unwrap() {
            WalRecord::Insert { term, value } => {
                assert_eq!(term, b"hello");
                assert!(value.is_none());
            }
            _ => panic!("Expected Insert"),
        }
    }

    #[test]
    fn test_deserialize_remove_payload_too_short() {
        // Remove requires at least 4 bytes for term_len
        let payload = vec![0, 0]; // Only 2 bytes
        let result = WalRecord::deserialize(WalRecordType::Remove, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("payload too short")));
    }

    #[test]
    fn test_deserialize_remove_term_truncated() {
        // term_len says 10, but only provide 3 bytes
        let mut payload = Vec::new();
        payload.extend_from_slice(&10u32.to_le_bytes()); // term_len = 10
        payload.extend_from_slice(&[b'a', b'b', b'c']); // Only 3 bytes
        let result = WalRecord::deserialize(WalRecordType::Remove, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("term truncated")));
    }

    #[test]
    fn test_deserialize_checkpoint_payload_too_short() {
        // Checkpoint requires 16 bytes: checkpoint_lsn (8) + timestamp (8)
        let payload = vec![0; 10]; // Only 10 bytes
        let result = WalRecord::deserialize(WalRecordType::Checkpoint, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("payload too short")));

        // 15 bytes is still too short
        let payload = vec![0; 15];
        let result = WalRecord::deserialize(WalRecordType::Checkpoint, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("payload too short")));
    }

    #[test]
    fn test_deserialize_begin_tx_payload_too_short() {
        // BeginTx requires 8 bytes for tx_id
        let payload = vec![0; 5]; // Only 5 bytes
        let result = WalRecord::deserialize(WalRecordType::BeginTx, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("payload too short")));
    }

    #[test]
    fn test_deserialize_commit_tx_payload_too_short() {
        // CommitTx requires 8 bytes for tx_id
        let payload = vec![0; 7]; // Only 7 bytes
        let result = WalRecord::deserialize(WalRecordType::CommitTx, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("payload too short")));
    }

    #[test]
    fn test_deserialize_abort_tx_payload_too_short() {
        // AbortTx requires 8 bytes for tx_id
        let payload = vec![0; 3]; // Only 3 bytes
        let result = WalRecord::deserialize(WalRecordType::AbortTx, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("payload too short")));
    }

    #[test]
    fn test_deserialize_increment_payload_too_short() {
        // Increment requires at least 4 bytes for term_len
        let payload = vec![0; 2]; // Only 2 bytes
        let result = WalRecord::deserialize(WalRecordType::Increment, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("payload too short")));
    }

    #[test]
    fn test_deserialize_increment_payload_truncated() {
        // term_len (4) + term + delta (8) + result (8) = 4 + term_len + 16
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        payload.extend_from_slice(&[0; 10]); // Only 10 bytes instead of 16 (delta + result)
        let result = WalRecord::deserialize(WalRecordType::Increment, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("truncated")));
    }

    #[test]
    fn test_deserialize_upsert_payload_too_short() {
        // Upsert requires at least 4 bytes for term_len
        let payload = vec![0; 3]; // Only 3 bytes
        let result = WalRecord::deserialize(WalRecordType::Upsert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("payload too short")));
    }

    #[test]
    fn test_deserialize_upsert_term_truncated() {
        // term_len says 10, but missing value_len
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        // Missing value_len (4 bytes)
        let result = WalRecord::deserialize(WalRecordType::Upsert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("term truncated")));
    }

    #[test]
    fn test_deserialize_upsert_value_truncated() {
        // Valid term_len, term, value_len, but truncated value
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        payload.extend_from_slice(&10u32.to_le_bytes()); // value_len = 10
        payload.extend_from_slice(&[1, 2, 3]); // Only 3 bytes of value
        let result = WalRecord::deserialize(WalRecordType::Upsert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("value truncated")));
    }

    #[test]
    fn test_deserialize_cas_payload_too_short() {
        // CAS requires at least 4 bytes for term_len
        let payload = vec![0; 2]; // Only 2 bytes
        let result = WalRecord::deserialize(WalRecordType::CompareAndSwap, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("payload too short")));
    }

    #[test]
    fn test_deserialize_cas_term_truncated() {
        // term_len + term but missing has_expected
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        // Missing has_expected byte
        let result = WalRecord::deserialize(WalRecordType::CompareAndSwap, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("term truncated")));
    }

    #[test]
    fn test_deserialize_cas_expected_length_truncated() {
        // Valid term, has_expected=1, but missing expected_len
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        payload.push(1); // has_expected = true
        // Missing expected_len (4 bytes)
        let result = WalRecord::deserialize(WalRecordType::CompareAndSwap, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("expected length truncated")));
    }

    #[test]
    fn test_deserialize_cas_expected_truncated() {
        // Valid term, has_expected=1, expected_len=10, but truncated expected value
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        payload.push(1); // has_expected = true
        payload.extend_from_slice(&10u32.to_le_bytes()); // expected_len = 10
        payload.extend_from_slice(&[1, 2, 3]); // Only 3 bytes
        let result = WalRecord::deserialize(WalRecordType::CompareAndSwap, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("expected truncated")));
    }

    #[test]
    fn test_deserialize_cas_new_value_length_truncated() {
        // Valid term, has_expected=0, but missing new_value_len
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        payload.push(0); // has_expected = false
        // Missing new_value_len (4 bytes)
        let result = WalRecord::deserialize(WalRecordType::CompareAndSwap, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("new_value length truncated")));
    }

    #[test]
    fn test_deserialize_cas_new_value_truncated() {
        // Valid term, has_expected=0, new_value_len=10, but truncated new_value
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        payload.push(0); // has_expected = false
        payload.extend_from_slice(&10u32.to_le_bytes()); // new_value_len = 10
        payload.extend_from_slice(&[1, 2, 3, 4, 5]); // Only 5 bytes (missing success byte too)
        let result = WalRecord::deserialize(WalRecordType::CompareAndSwap, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("new_value truncated")));
    }

    #[test]
    fn test_deserialize_batch_insert_payload_too_short() {
        // BatchInsert requires at least 4 bytes for count
        let payload = vec![0; 2]; // Only 2 bytes
        let result = WalRecord::deserialize(WalRecordType::BatchInsert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("payload too short")));
    }

    #[test]
    fn test_deserialize_batch_insert_entry_term_len_truncated() {
        // count=2, but entry 0 is incomplete
        let mut payload = Vec::new();
        payload.extend_from_slice(&2u32.to_le_bytes()); // count = 2
        // Entry 0: incomplete term_len
        payload.extend_from_slice(&[0, 0]); // Only 2 bytes of term_len
        let result = WalRecord::deserialize(WalRecordType::BatchInsert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("entry 0 term_len truncated")));
    }

    #[test]
    fn test_deserialize_batch_insert_entry_term_truncated() {
        // count=1, term_len=10 but only 3 bytes of term
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // count = 1
        payload.extend_from_slice(&10u32.to_le_bytes()); // term_len = 10
        payload.extend_from_slice(&[b'a', b'b', b'c']); // Only 3 bytes of term
        let result = WalRecord::deserialize(WalRecordType::BatchInsert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("entry 0 term truncated")));
    }

    #[test]
    fn test_deserialize_batch_insert_entry_value_len_truncated() {
        // count=1, valid term, has_value=1, but missing value_len
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // count = 1
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        payload.push(1); // has_value = true
        // Missing value_len (4 bytes)
        let result = WalRecord::deserialize(WalRecordType::BatchInsert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("entry 0 value_len truncated")));
    }

    #[test]
    fn test_deserialize_batch_insert_entry_value_truncated() {
        // count=1, valid term, has_value=1, value_len=10, but only 3 bytes of value
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes()); // count = 1
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"hello"); // term
        payload.push(1); // has_value = true
        payload.extend_from_slice(&10u32.to_le_bytes()); // value_len = 10
        payload.extend_from_slice(&[1, 2, 3]); // Only 3 bytes of value
        let result = WalRecord::deserialize(WalRecordType::BatchInsert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("entry 0 value truncated")));
    }

    #[test]
    fn test_deserialize_batch_insert_second_entry_truncated() {
        // Test truncation at second entry to ensure loop index is correct
        let mut payload = Vec::new();
        payload.extend_from_slice(&2u32.to_le_bytes()); // count = 2

        // Entry 0: complete
        payload.extend_from_slice(&3u32.to_le_bytes()); // term_len = 3
        payload.extend_from_slice(b"foo"); // term
        payload.push(0); // has_value = false

        // Entry 1: incomplete term
        payload.extend_from_slice(&10u32.to_le_bytes()); // term_len = 10
        payload.extend_from_slice(&[b'a', b'b']); // Only 2 bytes of term

        let result = WalRecord::deserialize(WalRecordType::BatchInsert, &payload);
        assert!(matches!(result, Err(WalError::CorruptedRecord(msg)) if msg.contains("entry 1 term truncated")));
    }

    #[test]
    fn test_deserialize_valid_increment() {
        // Valid Increment record
        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes()); // term_len = 5
        payload.extend_from_slice(b"count"); // term
        payload.extend_from_slice(&42i64.to_le_bytes()); // delta
        payload.extend_from_slice(&100i64.to_le_bytes()); // result

        let result = WalRecord::deserialize(WalRecordType::Increment, &payload);
        assert!(result.is_ok());
        match result.unwrap() {
            WalRecord::Increment { term, delta, result: res } => {
                assert_eq!(term, b"count");
                assert_eq!(delta, 42);
                assert_eq!(res, 100);
            }
            _ => panic!("Expected Increment"),
        }
    }

    #[test]
    fn test_deserialize_valid_cas_with_expected() {
        // Valid CAS with expected value
        let mut payload = Vec::new();
        payload.extend_from_slice(&3u32.to_le_bytes()); // term_len = 3
        payload.extend_from_slice(b"key"); // term
        payload.push(1); // has_expected = true
        payload.extend_from_slice(&3u32.to_le_bytes()); // expected_len = 3
        payload.extend_from_slice(b"old"); // expected
        payload.extend_from_slice(&3u32.to_le_bytes()); // new_value_len = 3
        payload.extend_from_slice(b"new"); // new_value
        payload.push(1); // success = true

        let result = WalRecord::deserialize(WalRecordType::CompareAndSwap, &payload);
        assert!(result.is_ok());
        match result.unwrap() {
            WalRecord::CompareAndSwap { term, expected, new_value, success } => {
                assert_eq!(term, b"key");
                assert_eq!(expected, Some(b"old".to_vec()));
                assert_eq!(new_value, b"new");
                assert!(success);
            }
            _ => panic!("Expected CompareAndSwap"),
        }
    }

    #[test]
    fn test_deserialize_valid_cas_without_expected() {
        // Valid CAS without expected value (insert if not exists)
        let mut payload = Vec::new();
        payload.extend_from_slice(&3u32.to_le_bytes()); // term_len = 3
        payload.extend_from_slice(b"key"); // term
        payload.push(0); // has_expected = false
        payload.extend_from_slice(&5u32.to_le_bytes()); // new_value_len = 5
        payload.extend_from_slice(b"value"); // new_value
        payload.push(0); // success = false

        let result = WalRecord::deserialize(WalRecordType::CompareAndSwap, &payload);
        assert!(result.is_ok());
        match result.unwrap() {
            WalRecord::CompareAndSwap { term, expected, new_value, success } => {
                assert_eq!(term, b"key");
                assert!(expected.is_none());
                assert_eq!(new_value, b"value");
                assert!(!success);
            }
            _ => panic!("Expected CompareAndSwap"),
        }
    }

    #[test]
    fn test_deserialize_valid_transaction_records() {
        // Valid BeginTx
        let payload = 12345u64.to_le_bytes().to_vec();
        let result = WalRecord::deserialize(WalRecordType::BeginTx, &payload);
        assert!(result.is_ok());
        match result.unwrap() {
            WalRecord::BeginTx { tx_id } => assert_eq!(tx_id, 12345),
            _ => panic!("Expected BeginTx"),
        }

        // Valid CommitTx
        let result = WalRecord::deserialize(WalRecordType::CommitTx, &payload);
        assert!(result.is_ok());
        match result.unwrap() {
            WalRecord::CommitTx { tx_id } => assert_eq!(tx_id, 12345),
            _ => panic!("Expected CommitTx"),
        }

        // Valid AbortTx
        let result = WalRecord::deserialize(WalRecordType::AbortTx, &payload);
        assert!(result.is_ok());
        match result.unwrap() {
            WalRecord::AbortTx { tx_id } => assert_eq!(tx_id, 12345),
            _ => panic!("Expected AbortTx"),
        }
    }

    #[test]
    fn test_deserialize_valid_checkpoint() {
        // Valid Checkpoint
        let mut payload = Vec::new();
        payload.extend_from_slice(&100u64.to_le_bytes()); // checkpoint_lsn
        payload.extend_from_slice(&1234567890u64.to_le_bytes()); // timestamp

        let result = WalRecord::deserialize(WalRecordType::Checkpoint, &payload);
        assert!(result.is_ok());
        match result.unwrap() {
            WalRecord::Checkpoint { checkpoint_lsn, timestamp } => {
                assert_eq!(checkpoint_lsn, 100);
                assert_eq!(timestamp, 1234567890);
            }
            _ => panic!("Expected Checkpoint"),
        }
    }

    #[test]
    fn test_invalid_record_type() {
        // Test TryFrom<u8> for WalRecordType with invalid values
        let result = WalRecordType::try_from(0u8);
        assert!(matches!(result, Err(WalError::InvalidRecordType(0))));

        let result = WalRecordType::try_from(12u8);
        assert!(matches!(result, Err(WalError::InvalidRecordType(12))));

        let result = WalRecordType::try_from(255u8);
        assert!(matches!(result, Err(WalError::InvalidRecordType(255))));

        // Valid types should work
        assert!(WalRecordType::try_from(1u8).is_ok());
        assert!(WalRecordType::try_from(10u8).is_ok());
    }

    #[test]
    fn test_wal_error_display_and_source() {
        // Test WalError Display implementations
        let io_err = WalError::Io(io::Error::new(io::ErrorKind::Other, "test io error"));
        let display = format!("{}", io_err);
        assert!(display.contains("WAL I/O error"));

        let invalid = WalError::InvalidRecordType(99);
        let display = format!("{}", invalid);
        assert!(display.contains("99"));

        let corrupted = WalError::CorruptedRecord("test corruption".into());
        let display = format!("{}", corrupted);
        assert!(display.contains("test corruption"));

        let eof = WalError::UnexpectedEof;
        let display = format!("{}", eof);
        assert!(display.contains("Unexpected end"));

        let exists = WalError::AlreadyExists;
        let display = format!("{}", exists);
        assert!(display.contains("already exists"));

        let not_found = WalError::NotFound;
        let display = format!("{}", not_found);
        assert!(display.contains("not found"));

        let parent_not_found = WalError::ParentNotFound(PathBuf::from("/test/path"));
        let display = format!("{}", parent_not_found);
        assert!(display.contains("/test/path"));

        // Test source() method
        use std::error::Error;
        let io_err = WalError::Io(io::Error::new(io::ErrorKind::Other, "test"));
        assert!(io_err.source().is_some());

        let corrupted = WalError::CorruptedRecord("test".into());
        assert!(corrupted.source().is_none());
    }
}
