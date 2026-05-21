//! WAL record codec — `WalRecordType` discriminant + `WalRecord` enum +
//! `serialize_payload` / `deserialize` byte-level codec.
//!
//! Split out of the monolithic `wal.rs` (lines ~86-821) as part of the
//! Phase-4 wal decomposition. This is the largest single piece of the
//! original file (~700 LOC) and lives in its own sub-module so the
//! type-tag → byte-payload mapping can be navigated independently of
//! the writer / reader machinery.

use super::{Lsn, WalError};

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

    // === Version-Based WAL Records (Phase 6) ===
    /// Version update - records a new version of the trie structure.
    ///
    /// This replaces N mutation records with a single version record,
    /// enabling point-in-time recovery via version restoration.
    VersionUpdate = 12,

    /// Version durable marker - indicates a version has been fully persisted.
    ///
    /// Used to mark which versions are safe for recovery.
    VersionDurable = 13,

    /// Version garbage collection - records versions that have been reclaimed.
    ///
    /// Used during recovery to skip GC'd versions.
    VersionGc = 14,
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
            12 => Ok(WalRecordType::VersionUpdate),
            13 => Ok(WalRecordType::VersionDurable),
            14 => Ok(WalRecordType::VersionGc),
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
    BatchInsert {
        /// The entries in this batch (term, optional value)
        entries: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    },
    /// Batch increment - multiple increment operations in a single WAL record.
    ///
    /// Used by document transactions to batch INCREMENT operations atomically.
    /// Unlike BatchInsert which uses SET semantics, BatchIncrement accumulates
    /// deltas with existing values.
    BatchIncrement {
        /// The increment entries (term, delta)
        entries: Vec<(Vec<u8>, i64)>,
    },

    // === Version-Based WAL Records (Phase 6) ===
    /// Version update - records a new version of the trie structure.
    VersionUpdate {
        /// Unique version identifier (monotonically increasing)
        version_id: u64,
        /// Root pointer to the versioned trie structure
        root_ptr: u64,
        /// Number of nodes in this version
        node_count: u64,
        /// Timestamp when this version was created
        timestamp: u64,
    },

    /// Version durable marker - indicates a version has been fully persisted.
    VersionDurable {
        /// Version that is now durable
        version_id: u64,
        /// Checksum of the persisted version data
        checksum: u32,
    },

    /// Version garbage collection - records versions that have been reclaimed.
    VersionGc {
        /// List of version IDs that have been garbage collected
        version_ids: Vec<u64>,
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
            WalRecord::VersionUpdate { .. } => WalRecordType::VersionUpdate,
            WalRecord::VersionDurable { .. } => WalRecordType::VersionDurable,
            WalRecord::VersionGc { .. } => WalRecordType::VersionGc,
        }
    }

    /// Serialize the record payload to bytes.
    pub fn serialize_payload(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        match self {
            WalRecord::Insert { term, value } => {
                buf.extend_from_slice(&(term.len() as u32).to_le_bytes());
                buf.extend_from_slice(term);
                if let Some(v) = value {
                    buf.push(1);
                    buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
                    buf.extend_from_slice(v);
                } else {
                    buf.push(0);
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
                buf.extend_from_slice(&(term.len() as u32).to_le_bytes());
                buf.extend_from_slice(term);
                buf.extend_from_slice(&delta.to_le_bytes());
                buf.extend_from_slice(&result.to_le_bytes());
            }
            WalRecord::Upsert { term, value } => {
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
                buf.extend_from_slice(&(term.len() as u32).to_le_bytes());
                buf.extend_from_slice(term);
                if let Some(exp) = expected {
                    buf.push(1);
                    buf.extend_from_slice(&(exp.len() as u32).to_le_bytes());
                    buf.extend_from_slice(exp);
                } else {
                    buf.push(0);
                }
                buf.extend_from_slice(&(new_value.len() as u32).to_le_bytes());
                buf.extend_from_slice(new_value);
                buf.push(if *success { 1 } else { 0 });
            }
            WalRecord::BatchInsert { entries } => {
                buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
                for (term, value) in entries {
                    buf.extend_from_slice(&(term.len() as u32).to_le_bytes());
                    buf.extend_from_slice(term);
                    if let Some(v) = value {
                        buf.push(1);
                        buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
                        buf.extend_from_slice(v);
                    } else {
                        buf.push(0);
                    }
                }
            }
            WalRecord::BatchIncrement { entries } => {
                buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
                for (term, delta) in entries {
                    buf.extend_from_slice(&(term.len() as u32).to_le_bytes());
                    buf.extend_from_slice(term);
                    buf.extend_from_slice(&delta.to_le_bytes());
                }
            }
            WalRecord::VersionUpdate {
                version_id,
                root_ptr,
                node_count,
                timestamp,
            } => {
                buf.extend_from_slice(&version_id.to_le_bytes());
                buf.extend_from_slice(&root_ptr.to_le_bytes());
                buf.extend_from_slice(&node_count.to_le_bytes());
                buf.extend_from_slice(&timestamp.to_le_bytes());
            }
            WalRecord::VersionDurable {
                version_id,
                checksum,
            } => {
                buf.extend_from_slice(&version_id.to_le_bytes());
                buf.extend_from_slice(&checksum.to_le_bytes());
            }
            WalRecord::VersionGc { version_ids } => {
                buf.extend_from_slice(&(version_ids.len() as u32).to_le_bytes());
                for vid in version_ids {
                    buf.extend_from_slice(&vid.to_le_bytes());
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
                        return Err(WalError::CorruptedRecord(
                            "Insert value length truncated".into(),
                        ));
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
                    return Err(WalError::CorruptedRecord(
                        "Checkpoint payload too short".into(),
                    ));
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
                    return Err(WalError::CorruptedRecord(
                        "BeginTx payload too short".into(),
                    ));
                }
                let tx_id = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                Ok(WalRecord::BeginTx { tx_id })
            }
            WalRecordType::CommitTx => {
                if payload.len() < 8 {
                    return Err(WalError::CorruptedRecord(
                        "CommitTx payload too short".into(),
                    ));
                }
                let tx_id = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                Ok(WalRecord::CommitTx { tx_id })
            }
            WalRecordType::AbortTx => {
                if payload.len() < 8 {
                    return Err(WalError::CorruptedRecord(
                        "AbortTx payload too short".into(),
                    ));
                }
                let tx_id = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                Ok(WalRecord::AbortTx { tx_id })
            }
            WalRecordType::Increment => {
                if payload.len() < 4 {
                    return Err(WalError::CorruptedRecord(
                        "Increment payload too short".into(),
                    ));
                }
                let term_len = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                if payload.len() < 4 + term_len + 16 {
                    return Err(WalError::CorruptedRecord(
                        "Increment payload truncated".into(),
                    ));
                }
                let term = payload[4..4 + term_len].to_vec();
                let delta_offset = 4 + term_len;
                let delta =
                    i64::from_le_bytes(payload[delta_offset..delta_offset + 8].try_into().unwrap());
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
                let value =
                    payload[value_len_offset + 4..value_len_offset + 4 + value_len].to_vec();
                Ok(WalRecord::Upsert { term, value })
            }
            WalRecordType::CompareAndSwap => {
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
                        return Err(WalError::CorruptedRecord(
                            "CAS expected length truncated".into(),
                        ));
                    }
                    let exp_len =
                        u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap())
                            as usize;
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
                    return Err(WalError::CorruptedRecord(
                        "CAS new_value length truncated".into(),
                    ));
                }
                let new_value_len =
                    u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
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
                if payload.len() < 4 {
                    return Err(WalError::CorruptedRecord(
                        "BatchInsert payload too short".into(),
                    ));
                }
                let count = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                let mut offset = 4;
                let mut entries = Vec::with_capacity(count);

                for i in 0..count {
                    if payload.len() < offset + 4 {
                        return Err(WalError::CorruptedRecord(format!(
                            "BatchInsert entry {} term_len truncated",
                            i
                        )));
                    }
                    let term_len =
                        u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap())
                            as usize;
                    offset += 4;

                    if payload.len() < offset + term_len + 1 {
                        return Err(WalError::CorruptedRecord(format!(
                            "BatchInsert entry {} term truncated",
                            i
                        )));
                    }
                    let term = payload[offset..offset + term_len].to_vec();
                    offset += term_len;

                    let has_value = payload[offset] != 0;
                    offset += 1;

                    let value = if has_value {
                        if payload.len() < offset + 4 {
                            return Err(WalError::CorruptedRecord(format!(
                                "BatchInsert entry {} value_len truncated",
                                i
                            )));
                        }
                        let value_len =
                            u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap())
                                as usize;
                        offset += 4;

                        if payload.len() < offset + value_len {
                            return Err(WalError::CorruptedRecord(format!(
                                "BatchInsert entry {} value truncated",
                                i
                            )));
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
                if payload.len() < 4 {
                    return Err(WalError::CorruptedRecord(
                        "BatchIncrement payload too short".into(),
                    ));
                }
                let count = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                let mut offset = 4;
                let mut entries = Vec::with_capacity(count);

                for i in 0..count {
                    if payload.len() < offset + 4 {
                        return Err(WalError::CorruptedRecord(format!(
                            "BatchIncrement entry {} term_len truncated",
                            i
                        )));
                    }
                    let term_len =
                        u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap())
                            as usize;
                    offset += 4;

                    if payload.len() < offset + term_len + 8 {
                        return Err(WalError::CorruptedRecord(format!(
                            "BatchIncrement entry {} term or delta truncated",
                            i
                        )));
                    }
                    let term = payload[offset..offset + term_len].to_vec();
                    offset += term_len;

                    let delta = i64::from_le_bytes(payload[offset..offset + 8].try_into().unwrap());
                    offset += 8;

                    entries.push((term, delta));
                }

                Ok(WalRecord::BatchIncrement { entries })
            }
            WalRecordType::VersionUpdate => {
                if payload.len() < 32 {
                    return Err(WalError::CorruptedRecord(
                        "VersionUpdate payload too short".into(),
                    ));
                }
                let version_id = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                let root_ptr = u64::from_le_bytes(payload[8..16].try_into().unwrap());
                let node_count = u64::from_le_bytes(payload[16..24].try_into().unwrap());
                let timestamp = u64::from_le_bytes(payload[24..32].try_into().unwrap());
                Ok(WalRecord::VersionUpdate {
                    version_id,
                    root_ptr,
                    node_count,
                    timestamp,
                })
            }
            WalRecordType::VersionDurable => {
                if payload.len() < 12 {
                    return Err(WalError::CorruptedRecord(
                        "VersionDurable payload too short".into(),
                    ));
                }
                let version_id = u64::from_le_bytes(payload[0..8].try_into().unwrap());
                let checksum = u32::from_le_bytes(payload[8..12].try_into().unwrap());
                Ok(WalRecord::VersionDurable {
                    version_id,
                    checksum,
                })
            }
            WalRecordType::VersionGc => {
                if payload.len() < 4 {
                    return Err(WalError::CorruptedRecord(
                        "VersionGc payload too short".into(),
                    ));
                }
                let count = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                if payload.len() < 4 + count * 8 {
                    return Err(WalError::CorruptedRecord(
                        "VersionGc version_ids truncated".into(),
                    ));
                }
                let mut version_ids = Vec::with_capacity(count);
                for i in 0..count {
                    let offset = 4 + i * 8;
                    let vid = u64::from_le_bytes(payload[offset..offset + 8].try_into().unwrap());
                    version_ids.push(vid);
                }
                Ok(WalRecord::VersionGc { version_ids })
            }
        }
    }
}
