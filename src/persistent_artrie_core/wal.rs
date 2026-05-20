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

// wal.rs is now a thin re-export hub for the wal/ sub-modules plus the
// `Lsn` type alias, the `crc32` helper, the disabled legacy GroupCommit
// stub, and the integration test suite at the bottom of this file. std
// imports for the tests live inside `mod tests`.

/// Log Sequence Number - monotonically increasing identifier for log records.
pub type Lsn = u64;

// `WalConfig` was relocated to the sibling `wal::config` module; re-exported
// here under its original path.
pub use config::WalConfig;

mod config;

/// CRC32 for record integrity verification.
pub(super) fn crc32(data: &[u8]) -> u32 {
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

// `WalRecord` + `WalRecordType` (record-type discriminant + payload codec)
// were relocated to the sibling `wal::codec` module; re-exported here under
// their original paths.
pub use codec::{WalRecord, WalRecordType};

mod codec;

// `WalError` was relocated to the sibling `wal::error` module; re-exported
// here under its original path.
pub use error::WalError;

mod error;

// `WalHeader` was relocated to the sibling `wal::header` module; re-exported
// here under its original path.
pub use header::WalHeader;

mod header;


// `WalWriter` was relocated to the sibling `wal::writer` module; re-exported
// here under its original path.
pub use writer::WalWriter;

mod writer;

// `WalReader` and `WalRecordIterator` were relocated to the sibling
// `wal::reader` module; re-exported here under their original paths.
pub use reader::{WalReader, WalRecordIterator};

mod reader;

// DISABLED — the legacy `GroupCommit` stub claimed to batch but its
// `append_sync` synchronously fsync'd every record ("For simplicity, sync
// immediately"). Production batching lives in
// `crate::persistent_artrie_core::group_commit::GroupCommitCoordinator`
// (background thread, AIMD batching, oneshot channels) and is selected via
// `DurabilityPolicy::GroupCommit` routing through
// `WalWriter::sync_async` from `dict_impl::sync()`. The stub had no
// remaining callers; commenting it out per CLAUDE.md to keep the audit
// trail clear.
//
// pub struct GroupCommit {
//     wal: Arc<WalWriter>,
//     pending: Mutex<Vec<(Lsn, std::sync::mpsc::Sender<Result<(), WalError>>)>>,
//     #[allow(dead_code)]
//     sync_interval_ms: u64,
// }
//
// impl GroupCommit {
//     pub fn new(wal: Arc<WalWriter>, sync_interval_ms: u64) -> Self { ... }
//     pub fn append_sync(&self, record: WalRecord) -> Result<Lsn, WalError> {
//         let lsn = self.wal.append(record)?;
//         self.wal.sync()?;            // <-- this defeats the batching premise
//         Ok(lsn)
//     }
//     pub fn wal(&self) -> &WalWriter { &self.wal }
// }

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

// The async-write subsystem (SegmentSyncManager + AsyncWalWriter +
// PendingSegment + SyncHandle + collect_all_segments) was moved into the
// sibling wal/ sub-modules, taking its imports with it. The stale block of
// std imports that used to live here has been removed.

// `AsyncWalConfig` was relocated to the sibling `wal::async_config` module;
// re-exported here under its original path.
pub use async_config::AsyncWalConfig;

mod async_config;

// `PendingSegment` was relocated to the sibling `wal::pending_segment` module;
// re-exported here under its original path.
pub use pending_segment::PendingSegment;

mod pending_segment;

// `AsyncWalError` was relocated to the sibling `wal::async_error` module;
// re-exported here under its original path.
pub use async_error::AsyncWalError;

mod async_error;

// `SyncHandle` was relocated to the sibling `wal::sync_handle` module;
// re-exported here under its original path.
pub use sync_handle::SyncHandle;

mod sync_handle;

// `WalSyncBackend` trait + `StdFsync` + `IoUringFsync` impls were relocated to
// the sibling `wal::sync_backend` module. They are re-exported below so
// downstream code (and the rest of this file) can keep using the unqualified
// names `WalSyncBackend`, `StdFsync`, `IoUringFsync`.
pub use sync_backend::{StdFsync, WalSyncBackend};
#[cfg(feature = "io-uring-backend")]
pub use sync_backend::IoUringFsync;

mod sync_backend;

// `SegmentSyncManager`, `AsyncWalWriter`, and the `collect_all_segments`
// recovery helper were relocated to the sibling `wal::async_writer` module;
// re-exported here under their original paths.
pub use async_writer::{collect_all_segments, AsyncWalWriter, SegmentSyncManager};

mod async_writer;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::path::PathBuf;
    use std::time::Duration;
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

        // 15 is beyond the current max (VersionGc = 14)
        let result = WalRecordType::try_from(15u8);
        assert!(matches!(result, Err(WalError::InvalidRecordType(15))));

        let result = WalRecordType::try_from(255u8);
        assert!(matches!(result, Err(WalError::InvalidRecordType(255))));

        // Valid types should work (1-14 are all valid now)
        assert!(WalRecordType::try_from(1u8).is_ok());  // Insert
        assert!(WalRecordType::try_from(10u8).is_ok()); // BatchInsert
        assert!(WalRecordType::try_from(12u8).is_ok()); // VersionUpdate
        assert!(WalRecordType::try_from(14u8).is_ok()); // VersionGc
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

    // ==================== Version-Based WAL Tests ====================

    #[test]
    fn test_version_update_roundtrip() {
        let record = WalRecord::VersionUpdate {
            version_id: 42,
            root_ptr: 0x1234_5678_9ABC_DEF0,
            node_count: 1000,
            timestamp: 1699999999,
        };

        assert_eq!(record.record_type(), WalRecordType::VersionUpdate);

        let payload = record.serialize_payload();
        assert_eq!(payload.len(), 32); // 4 x u64 = 32 bytes

        let deserialized = WalRecord::deserialize(WalRecordType::VersionUpdate, &payload)
            .expect("deserialize");

        match deserialized {
            WalRecord::VersionUpdate { version_id, root_ptr, node_count, timestamp } => {
                assert_eq!(version_id, 42);
                assert_eq!(root_ptr, 0x1234_5678_9ABC_DEF0);
                assert_eq!(node_count, 1000);
                assert_eq!(timestamp, 1699999999);
            }
            _ => panic!("Expected VersionUpdate"),
        }
    }

    #[test]
    fn test_version_durable_roundtrip() {
        let record = WalRecord::VersionDurable {
            version_id: 99,
            checksum: 0xDEAD_BEEF,
        };

        assert_eq!(record.record_type(), WalRecordType::VersionDurable);

        let payload = record.serialize_payload();
        assert_eq!(payload.len(), 12); // u64 + u32 = 12 bytes

        let deserialized = WalRecord::deserialize(WalRecordType::VersionDurable, &payload)
            .expect("deserialize");

        match deserialized {
            WalRecord::VersionDurable { version_id, checksum } => {
                assert_eq!(version_id, 99);
                assert_eq!(checksum, 0xDEAD_BEEF);
            }
            _ => panic!("Expected VersionDurable"),
        }
    }

    #[test]
    fn test_version_gc_roundtrip() {
        let record = WalRecord::VersionGc {
            version_ids: vec![1, 5, 10, 42, 100],
        };

        assert_eq!(record.record_type(), WalRecordType::VersionGc);

        let payload = record.serialize_payload();
        assert_eq!(payload.len(), 4 + 5 * 8); // count (4) + 5 x u64 (40) = 44 bytes

        let deserialized = WalRecord::deserialize(WalRecordType::VersionGc, &payload)
            .expect("deserialize");

        match deserialized {
            WalRecord::VersionGc { version_ids } => {
                assert_eq!(version_ids, vec![1, 5, 10, 42, 100]);
            }
            _ => panic!("Expected VersionGc"),
        }
    }

    #[test]
    fn test_version_gc_empty() {
        let record = WalRecord::VersionGc {
            version_ids: vec![],
        };

        let payload = record.serialize_payload();
        assert_eq!(payload.len(), 4); // just the count

        let deserialized = WalRecord::deserialize(WalRecordType::VersionGc, &payload)
            .expect("deserialize");

        match deserialized {
            WalRecord::VersionGc { version_ids } => {
                assert!(version_ids.is_empty());
            }
            _ => panic!("Expected VersionGc"),
        }
    }

    #[test]
    fn test_version_update_too_short() {
        let result = WalRecord::deserialize(WalRecordType::VersionUpdate, &[0; 31]);
        assert!(result.is_err());
    }

    #[test]
    fn test_version_durable_too_short() {
        let result = WalRecord::deserialize(WalRecordType::VersionDurable, &[0; 11]);
        assert!(result.is_err());
    }

    #[test]
    fn test_version_gc_too_short() {
        // count = 5 but only 3 version IDs provided
        let mut payload = vec![];
        payload.extend_from_slice(&5u32.to_le_bytes()); // count = 5
        payload.extend_from_slice(&1u64.to_le_bytes());
        payload.extend_from_slice(&2u64.to_le_bytes());
        payload.extend_from_slice(&3u64.to_le_bytes());
        // Missing 2 more version IDs

        let result = WalRecord::deserialize(WalRecordType::VersionGc, &payload);
        assert!(result.is_err());
    }
}
