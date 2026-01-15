//! Crash recovery for PersistentARTrieChar.
//!
//! This module implements corruption detection and WAL-based recovery for the
//! character-level persistent trie. It handles:
//!
//! - Arena checksum validation (V3+ arenas)
//! - File header checksum verification (V2+ headers)
//! - WAL segment collection from archive mode
//! - Full trie reconstruction from WAL records
//!
//! # Recovery Strategies
//!
//! 1. **Normal**: File checksums valid, WAL replayed after last checkpoint
//! 2. **Partial Recovery**: Some arenas corrupted, rebuild affected portions from WAL
//! 3. **Rebuild from WAL**: File severely corrupted, full reconstruction from archived WAL
//! 4. **Unrecoverable**: WAL also corrupted or missing, data loss
//!
//! # Example
//!
//! ```rust,ignore
//! use libdictenstein::persistent_artrie_char::recovery::{
//!     RecoveryManager, RecoveryPolicy, detect_corruption
//! };
//!
//! // Check for corruption
//! if let Some(corruption) = detect_corruption(&trie_path)? {
//!     println!("Corruption detected: {:?}", corruption);
//!
//!     // Attempt recovery
//!     let report = RecoveryManager::new(&trie_path, &wal_config)
//!         .recover_with_policy(RecoveryPolicy::AutoRecover)?;
//!
//!     println!("Recovered {} records", report.records_replayed);
//! }
//! ```

use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::arena::{ArenaValidation, CharNodeArena, HEADER_SIZE as ARENA_HEADER_SIZE};
use super::dict_impl_char::{CharTrieFileHeader, CHAR_FILE_HEADER_SIZE, CHAR_TRIE_MAGIC};
use crate::persistent_artrie::disk_manager::BLOCK_SIZE;
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::wal::{Lsn, WalConfig, WalReader, WalRecord, WalWriter};

/// Helper to convert io::Error to PersistentARTrieError
fn io_err(operation: &str, path: &Path, e: std::io::Error) -> PersistentARTrieError {
    PersistentARTrieError::IoError {
        operation: operation.to_string(),
        path: path.display().to_string(),
        source: e,
    }
}

/// Recovery mode indicating what type of recovery was performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryMode {
    /// File was valid, normal open (possibly with WAL replay after checkpoint)
    Normal {
        /// Number of WAL records replayed after last checkpoint
        wal_records_replayed: usize,
    },

    /// Some arenas were corrupted but recovered from WAL
    PartialRecovery {
        /// Block IDs of corrupted arenas
        corrupted_arenas: Vec<u32>,
        /// Number of records recovered from WAL
        recovered_records: usize,
    },

    /// File severely corrupted, rebuilt from WAL segments
    RebuildFromWal {
        /// Number of WAL segments processed
        segments_processed: usize,
        /// Total records replayed
        records_replayed: usize,
    },

    /// Recovery failed, data may be lost
    Unrecoverable {
        /// Reason for failure
        reason: String,
    },
}

impl RecoveryMode {
    /// Returns true if recovery was successful
    pub fn is_success(&self) -> bool {
        !matches!(self, RecoveryMode::Unrecoverable { .. })
    }

    /// Returns the number of records replayed during recovery
    pub fn records_replayed(&self) -> usize {
        match self {
            RecoveryMode::Normal { wal_records_replayed } => *wal_records_replayed,
            RecoveryMode::PartialRecovery { recovered_records, .. } => *recovered_records,
            RecoveryMode::RebuildFromWal { records_replayed, .. } => *records_replayed,
            RecoveryMode::Unrecoverable { .. } => 0,
        }
    }
}

/// Recovery statistics and outcome.
#[derive(Debug, Clone)]
pub struct RecoveryReport {
    /// The type of recovery performed
    pub mode: RecoveryMode,
    /// Total time spent in recovery
    pub duration: Duration,
    /// Number of records replayed
    pub records_replayed: usize,
    /// Checkpoint LSN that recovery started from (if any)
    pub checkpoint_lsn: Option<Lsn>,
    /// Number of WAL segments processed
    pub segments_processed: usize,
    /// Number of corrupted records skipped
    pub corrupted_records_skipped: usize,
}

impl RecoveryReport {
    /// Create a new report for normal open
    pub fn normal() -> Self {
        Self {
            mode: RecoveryMode::Normal { wal_records_replayed: 0 },
            duration: Duration::ZERO,
            records_replayed: 0,
            checkpoint_lsn: None,
            segments_processed: 0,
            corrupted_records_skipped: 0,
        }
    }
}

/// Information about detected corruption.
#[derive(Debug, Clone)]
pub struct CorruptionInfo {
    /// Type of corruption detected
    pub corruption_type: CorruptionType,
    /// Block IDs of corrupted arenas (if applicable)
    pub corrupted_arenas: Vec<u32>,
    /// Whether WAL is available for recovery
    pub wal_available: bool,
    /// Number of WAL segments found
    pub wal_segments: usize,
}

/// Types of corruption that can be detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorruptionType {
    /// File header checksum mismatch
    HeaderChecksum { stored: u32, computed: u32 },
    /// File header magic number is invalid
    InvalidMagic,
    /// One or more arenas have checksum mismatches
    ArenaChecksum {
        /// Number of arenas with invalid checksums
        count: usize,
    },
    /// File is truncated
    Truncated { expected: u64, actual: u64 },
    /// File cannot be opened
    FileNotReadable { reason: String },
}

/// Policy for handling corruption during open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecoveryPolicy {
    /// Automatically recover, report what happened
    #[default]
    AutoRecover,
    /// Fail immediately if any corruption detected
    FailOnCorruption,
    /// Recover partial data, report losses
    RecoverPartial,
}

/// Detect corruption in a trie file without loading it.
///
/// This function performs lightweight validation:
/// 1. Checks file header magic and checksum
/// 2. Optionally validates arena checksums (if `validate_arenas` is true)
///
/// # Arguments
///
/// * `path` - Path to the .artrie file
/// * `validate_arenas` - Whether to validate all arena checksums (slower but thorough)
///
/// # Returns
///
/// * `Ok(None)` - No corruption detected
/// * `Ok(Some(info))` - Corruption detected, info contains details
/// * `Err(e)` - Error while checking (distinct from corruption)
pub fn detect_corruption(path: &Path, validate_arenas: bool) -> Result<Option<CorruptionInfo>> {
    // Check if file exists
    if !path.exists() {
        return Ok(None); // No file = no corruption
    }

    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return Ok(Some(CorruptionInfo {
                corruption_type: CorruptionType::FileNotReadable {
                    reason: e.to_string(),
                },
                corrupted_arenas: vec![],
                wal_available: false,
                wal_segments: 0,
            }));
        }
    };

    let metadata = file.metadata().map_err(|e| io_err("metadata", path, e))?;
    let file_size = metadata.len();

    // Check minimum size for header
    if file_size < CHAR_FILE_HEADER_SIZE as u64 {
        return Ok(Some(CorruptionInfo {
            corruption_type: CorruptionType::Truncated {
                expected: CHAR_FILE_HEADER_SIZE as u64,
                actual: file_size,
            },
            corrupted_arenas: vec![],
            wal_available: check_wal_available(path),
            wal_segments: count_wal_segments(path),
        }));
    }

    // Read and validate header
    let mut reader = BufReader::new(file);
    let mut header_buf = [0u8; CHAR_FILE_HEADER_SIZE];
    reader.read_exact(&mut header_buf).map_err(|e| io_err("read header", path, e))?;

    let header = CharTrieFileHeader::from_bytes(&header_buf);

    // Check magic number
    if header.magic != CHAR_TRIE_MAGIC {
        return Ok(Some(CorruptionInfo {
            corruption_type: CorruptionType::InvalidMagic,
            corrupted_arenas: vec![],
            wal_available: check_wal_available(path),
            wal_segments: count_wal_segments(path),
        }));
    }

    // Verify header checksum (V2+)
    if header.has_checksum() && !header.verify_checksum() {
        return Ok(Some(CorruptionInfo {
            corruption_type: CorruptionType::HeaderChecksum {
                stored: header.header_checksum,
                computed: header.compute_checksum(),
            },
            corrupted_arenas: vec![],
            wal_available: check_wal_available(path),
            wal_segments: count_wal_segments(path),
        }));
    }

    // Optionally validate arena checksums
    if validate_arenas {
        let corrupted = validate_all_arenas(&mut reader, file_size);
        if !corrupted.is_empty() {
            return Ok(Some(CorruptionInfo {
                corruption_type: CorruptionType::ArenaChecksum {
                    count: corrupted.len(),
                },
                corrupted_arenas: corrupted,
                wal_available: check_wal_available(path),
                wal_segments: count_wal_segments(path),
            }));
        }
    }

    Ok(None)
}

/// Validate all arenas in a file.
fn validate_all_arenas<R: Read + Seek>(reader: &mut R, file_size: u64) -> Vec<u32> {
    let mut corrupted = Vec::new();
    let mut block_id = 0u32;
    let mut offset = CHAR_FILE_HEADER_SIZE as u64;

    // Round up to block boundary
    offset = (offset + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64 * BLOCK_SIZE as u64;

    while offset + BLOCK_SIZE as u64 <= file_size {
        if reader.seek(SeekFrom::Start(offset)).is_err() {
            break;
        }

        let mut block_buf = vec![0u8; BLOCK_SIZE];
        if reader.read_exact(&mut block_buf).is_err() {
            break; // Truncated file
        }

        // Only validate if it looks like an arena (has arena magic)
        if block_buf.len() >= ARENA_HEADER_SIZE {
            match CharNodeArena::validate_checksums(&block_buf) {
                Ok(ArenaValidation::Valid) => {}
                Ok(ArenaValidation::HeaderChecksumMismatch { .. })
                | Ok(ArenaValidation::DataChecksumMismatch { .. })
                | Ok(ArenaValidation::Truncated { .. }) => {
                    corrupted.push(block_id);
                }
                Ok(ArenaValidation::InvalidMagic) => {
                    // Not an arena, skip
                }
                Err(_) => {
                    corrupted.push(block_id);
                }
            }
        }

        offset += BLOCK_SIZE as u64;
        block_id += 1;
    }

    corrupted
}

/// Check if WAL is available for the given trie path.
fn check_wal_available(trie_path: &Path) -> bool {
    let wal_path = trie_path.with_extension("wal");
    wal_path.exists()
}

/// Count WAL segments available (active + archived).
fn count_wal_segments(trie_path: &Path) -> usize {
    let wal_path = trie_path.with_extension("wal");
    let archive_dir = trie_path.parent().unwrap_or(Path::new(".")).join("wal_archive");

    let mut count = 0;

    // Check active WAL
    if wal_path.exists() {
        count += 1;
    }

    // Check archived segments
    if archive_dir.exists() {
        if let Ok(entries) = fs::read_dir(&archive_dir) {
            count += entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map_or(false, |ext| ext == "segment"))
                .count();
        }
    }

    count
}

/// Recovery manager for corrupted trie files.
pub struct RecoveryManager {
    /// Path to the trie file
    trie_path: PathBuf,
    /// Path to WAL file
    wal_path: PathBuf,
    /// WAL configuration for archive mode
    wal_config: WalConfig,
}

impl RecoveryManager {
    /// Create a new recovery manager.
    pub fn new(trie_path: impl AsRef<Path>, wal_config: WalConfig) -> Self {
        let trie_path = trie_path.as_ref().to_path_buf();
        let wal_path = trie_path.with_extension("wal");

        Self {
            trie_path,
            wal_path,
            wal_config,
        }
    }

    /// Check if recovery is needed.
    pub fn needs_recovery(&self) -> Result<bool> {
        detect_corruption(&self.trie_path, false)
            .map(|opt| opt.is_some())
    }

    /// Perform recovery with specified policy.
    ///
    /// # Arguments
    ///
    /// * `policy` - How to handle corruption
    /// * `apply_fn` - Callback to apply recovered operations
    ///
    /// # Returns
    ///
    /// Recovery report with statistics, or error if recovery failed.
    pub fn recover_with_callback<F>(
        &self,
        policy: RecoveryPolicy,
        mut apply_fn: F,
    ) -> Result<RecoveryReport>
    where
        F: FnMut(RecoveredOperation) -> Result<()>,
    {
        let start = Instant::now();

        // Check for corruption
        let corruption = detect_corruption(&self.trie_path, true)?;

        match (corruption, policy) {
            // No corruption - normal operation
            (None, _) => {
                // Just replay any WAL records after checkpoint
                let replay_count = self.replay_wal_after_checkpoint(&mut apply_fn)?;

                Ok(RecoveryReport {
                    mode: RecoveryMode::Normal {
                        wal_records_replayed: replay_count,
                    },
                    duration: start.elapsed(),
                    records_replayed: replay_count,
                    checkpoint_lsn: self.get_checkpoint_lsn()?,
                    segments_processed: 1,
                    corrupted_records_skipped: 0,
                })
            }

            // Corruption detected but policy says fail
            (Some(info), RecoveryPolicy::FailOnCorruption) => {
                Err(PersistentARTrieError::CorruptedFile {
                    reason: format!("{:?}", info.corruption_type),
                })
            }

            // Corruption detected - attempt recovery
            (Some(info), RecoveryPolicy::AutoRecover | RecoveryPolicy::RecoverPartial) => {
                if !info.wal_available && info.wal_segments == 0 {
                    return Err(PersistentARTrieError::RecoveryError {
                        reason: "No WAL available for recovery".to_string(),
                    });
                }

                self.rebuild_from_wal(&mut apply_fn, start)
            }
        }
    }

    /// Replay WAL records after the last checkpoint.
    fn replay_wal_after_checkpoint<F>(&self, apply_fn: &mut F) -> Result<usize>
    where
        F: FnMut(RecoveredOperation) -> Result<()>,
    {
        if !self.wal_path.exists() {
            return Ok(0);
        }

        let checkpoint_lsn = self.get_checkpoint_lsn()?.unwrap_or(0);

        let reader = match WalReader::new(&self.wal_path) {
            Ok(r) => r,
            Err(_) => return Ok(0),
        };

        let mut replayed = 0;
        for result in reader.iter() {
            match result {
                Ok((lsn, record)) => {
                    // Only replay records after checkpoint
                    if lsn <= checkpoint_lsn {
                        continue;
                    }

                    for op in self.record_to_operations(lsn, record) {
                        apply_fn(op)?;
                        replayed += 1;
                    }
                }
                Err(_) => {
                    // Skip corrupted records
                    continue;
                }
            }
        }

        Ok(replayed)
    }

    /// Rebuild the entire trie from WAL segments.
    fn rebuild_from_wal<F>(&self, apply_fn: &mut F, start: Instant) -> Result<RecoveryReport>
    where
        F: FnMut(RecoveredOperation) -> Result<()>,
    {
        // Collect all WAL segments
        let wal_writer = if self.wal_path.exists() {
            WalWriter::open(&self.wal_path)?
        } else {
            // No WAL - can't recover
            return Err(PersistentARTrieError::RecoveryError {
                reason: "WAL file not found".to_string(),
            });
        };

        let segments = wal_writer.collect_wal_segments(&self.wal_config)?;
        let segment_count = segments.len();

        if segments.is_empty() {
            return Ok(RecoveryReport {
                mode: RecoveryMode::RebuildFromWal {
                    segments_processed: 0,
                    records_replayed: 0,
                },
                duration: start.elapsed(),
                records_replayed: 0,
                checkpoint_lsn: None,
                segments_processed: 0,
                corrupted_records_skipped: 0,
            });
        }

        let mut replayed = 0;
        let mut corrupted_skipped = 0;

        // Process all segments in order
        for segment_path in &segments {
            let reader = match WalReader::new(segment_path) {
                Ok(r) => r,
                Err(_) => continue, // Skip unreadable segments
            };

            for result in reader.iter() {
                match result {
                    Ok((lsn, record)) => {
                        for op in self.record_to_operations(lsn, record) {
                            apply_fn(op)?;
                            replayed += 1;
                        }
                    }
                    Err(_) => {
                        corrupted_skipped += 1;
                    }
                }
            }
        }

        Ok(RecoveryReport {
            mode: RecoveryMode::RebuildFromWal {
                segments_processed: segment_count,
                records_replayed: replayed,
            },
            duration: start.elapsed(),
            records_replayed: replayed,
            checkpoint_lsn: None,
            segments_processed: segment_count,
            corrupted_records_skipped: corrupted_skipped,
        })
    }

    /// Get the checkpoint LSN from the trie file header.
    fn get_checkpoint_lsn(&self) -> Result<Option<Lsn>> {
        if !self.trie_path.exists() {
            return Ok(None);
        }

        let mut file = File::open(&self.trie_path)
            .map_err(|e| io_err("open", &self.trie_path, e))?;
        let mut header_buf = [0u8; CHAR_FILE_HEADER_SIZE];
        file.read_exact(&mut header_buf)
            .map_err(|e| io_err("read header", &self.trie_path, e))?;

        let header = CharTrieFileHeader::from_bytes(&header_buf);
        if header.checkpoint_lsn > 0 {
            Ok(Some(header.checkpoint_lsn))
        } else {
            Ok(None)
        }
    }

    /// Convert a WAL record to recovery operations.
    ///
    /// Returns a vector because BatchInsert records contain multiple operations.
    fn record_to_operations(&self, lsn: Lsn, record: WalRecord) -> Vec<RecoveredOperation> {
        match record {
            WalRecord::Insert { term, value } => {
                vec![RecoveredOperation::Insert { lsn, term, value }]
            }
            WalRecord::Remove { term } => {
                vec![RecoveredOperation::Remove { lsn, term }]
            }
            WalRecord::Increment { term, delta, result } => {
                vec![RecoveredOperation::Increment { lsn, term, delta, result }]
            }
            WalRecord::Upsert { term, value } => {
                vec![RecoveredOperation::Upsert { lsn, term, value }]
            }
            WalRecord::CompareAndSwap {
                term,
                new_value,
                success,
                ..
            } => {
                if success {
                    vec![RecoveredOperation::CompareAndSwap {
                        lsn,
                        term,
                        new_value,
                        success,
                    }]
                } else {
                    vec![] // Failed CAS operations don't need replay
                }
            }
            WalRecord::BatchInsert { entries } => {
                entries
                    .into_iter()
                    .map(|(term, value)| RecoveredOperation::Insert { lsn, term, value })
                    .collect()
            }
            // Skip transaction and checkpoint records
            WalRecord::BeginTx { .. }
            | WalRecord::CommitTx { .. }
            | WalRecord::AbortTx { .. }
            | WalRecord::Checkpoint { .. } => vec![],
        }
    }
}

/// A recovered operation ready to be applied.
#[derive(Debug, Clone)]
pub enum RecoveredOperation {
    /// Insert a term with optional value
    Insert {
        /// Log sequence number
        lsn: Lsn,
        /// Term bytes (UTF-8 encoded)
        term: Vec<u8>,
        /// Optional serialized value
        value: Option<Vec<u8>>,
    },
    /// Remove a term
    Remove {
        /// Log sequence number
        lsn: Lsn,
        /// Term bytes (UTF-8 encoded)
        term: Vec<u8>,
    },
    /// Increment a numeric value
    Increment {
        /// Log sequence number
        lsn: Lsn,
        /// Term bytes
        term: Vec<u8>,
        /// Delta that was added
        delta: i64,
        /// Resulting value
        result: i64,
    },
    /// Upsert a value
    Upsert {
        /// Log sequence number
        lsn: Lsn,
        /// Term bytes
        term: Vec<u8>,
        /// New value
        value: Vec<u8>,
    },
    /// Compare and swap
    CompareAndSwap {
        /// Log sequence number
        lsn: Lsn,
        /// Term bytes
        term: Vec<u8>,
        /// New value
        new_value: Vec<u8>,
        /// Whether successful
        success: bool,
    },
}

impl RecoveredOperation {
    /// Get the term as a string (if valid UTF-8).
    pub fn term_str(&self) -> Option<&str> {
        let bytes = match self {
            Self::Insert { term, .. } => term,
            Self::Remove { term, .. } => term,
            Self::Increment { term, .. } => term,
            Self::Upsert { term, .. } => term,
            Self::CompareAndSwap { term, .. } => term,
        };
        std::str::from_utf8(bytes).ok()
    }

    /// Get the LSN of this operation.
    pub fn lsn(&self) -> Lsn {
        match self {
            Self::Insert { lsn, .. } => *lsn,
            Self::Remove { lsn, .. } => *lsn,
            Self::Increment { lsn, .. } => *lsn,
            Self::Upsert { lsn, .. } => *lsn,
            Self::CompareAndSwap { lsn, .. } => *lsn,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_recovery_mode_is_success() {
        assert!(RecoveryMode::Normal { wal_records_replayed: 0 }.is_success());
        assert!(RecoveryMode::PartialRecovery {
            corrupted_arenas: vec![],
            recovered_records: 0
        }.is_success());
        assert!(RecoveryMode::RebuildFromWal {
            segments_processed: 0,
            records_replayed: 0
        }.is_success());
        assert!(!RecoveryMode::Unrecoverable {
            reason: "test".to_string()
        }.is_success());
    }

    #[test]
    fn test_detect_corruption_missing_file() {
        let dir = tempdir().expect("create tempdir");
        let path = dir.path().join("nonexistent.artrie");

        let result = detect_corruption(&path, false).expect("detect_corruption");
        assert!(result.is_none(), "Missing file should not be corruption");
    }

    #[test]
    fn test_detect_corruption_truncated_file() {
        let dir = tempdir().expect("create tempdir");
        let path = dir.path().join("truncated.artrie");

        // Create a truncated file
        fs::write(&path, &[0u8; 10]).expect("write file");

        let result = detect_corruption(&path, false).expect("detect_corruption");
        assert!(result.is_some());
        match result.unwrap().corruption_type {
            CorruptionType::Truncated { expected, actual } => {
                assert_eq!(expected, CHAR_FILE_HEADER_SIZE as u64);
                assert_eq!(actual, 10);
            }
            _ => panic!("Expected Truncated corruption type"),
        }
    }

    #[test]
    fn test_detect_corruption_invalid_magic() {
        let dir = tempdir().expect("create tempdir");
        let path = dir.path().join("bad_magic.artrie");

        // Create file with wrong magic
        let mut data = [0u8; CHAR_FILE_HEADER_SIZE];
        data[0..4].copy_from_slice(b"XXXX"); // Wrong magic
        fs::write(&path, &data).expect("write file");

        let result = detect_corruption(&path, false).expect("detect_corruption");
        assert!(result.is_some());
        assert!(matches!(
            result.unwrap().corruption_type,
            CorruptionType::InvalidMagic
        ));
    }

    #[test]
    fn test_corruption_info_wal_check() {
        let dir = tempdir().expect("create tempdir");
        let trie_path = dir.path().join("test.artrie");
        let wal_path = dir.path().join("test.wal");

        // Create files
        fs::write(&trie_path, &[0u8; 10]).expect("write trie");
        fs::write(&wal_path, &[0u8; 100]).expect("write wal");

        assert!(check_wal_available(&trie_path));
        assert!(count_wal_segments(&trie_path) >= 1);
    }

    #[test]
    fn test_recovery_manager_no_file() {
        let dir = tempdir().expect("create tempdir");
        let path = dir.path().join("missing.artrie");

        let config = WalConfig::default();
        let manager = RecoveryManager::new(&path, config);

        assert!(!manager.needs_recovery().expect("needs_recovery"));
    }

    #[test]
    fn test_recovered_operation_term_str() {
        let op = RecoveredOperation::Insert {
            lsn: 1,
            term: b"hello".to_vec(),
            value: None,
        };
        assert_eq!(op.term_str(), Some("hello"));
        assert_eq!(op.lsn(), 1);

        // Invalid UTF-8
        let op = RecoveredOperation::Insert {
            lsn: 2,
            term: vec![0xFF, 0xFE],
            value: None,
        };
        assert_eq!(op.term_str(), None);
    }

    #[test]
    fn test_recovery_report_normal() {
        let report = RecoveryReport::normal();
        assert!(report.mode.is_success());
        assert_eq!(report.records_replayed, 0);
    }
}
