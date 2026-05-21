//! Shared WAL management trait for persistent trie implementations.
//!
//! This module provides the [`WalManaged`] trait that abstracts WAL operations,
//! eliminating code duplication across `PersistentARTrie`, `PersistentARTrieChar`,
//! and `PersistentVocabARTrie` implementations.
//!
//! # Design
//!
//! Each implementation stores an `Arc<AsyncWalWriter>` and implements the simple
//! `wal_writer()` accessor method. All WAL operations are provided as default
//! trait methods that delegate to the `AsyncWalWriter`.
//!
//! # Benefits
//!
//! - **DRY**: WAL logic is centralized in one place
//! - **Consistency**: All implementations handle WAL identically
//! - **Maintainability**: Change WAL behavior once, all benefit
//! - **Type Safety**: Trait ensures implementations provide WAL access
//!
//! # Example
//!
//! ```text
//! use libdictenstein::persistent_artrie::wal_managed::WalManaged;
//!
//! // Any type implementing WalManaged gets these methods for free
//! impl WalManaged for MyPersistentTrie {
//!     fn wal_writer(&self) -> Option<&Arc<AsyncWalWriter>> {
//!         self.wal_writer.as_ref()
//!     }
//! }
//!
//! // Now you can use:
//! // my_trie.log_insert(term, value)?;
//! // my_trie.log_remove(term)?;
//! // my_trie.wal_sync()?;
//! ```

use std::path::Path;
use std::sync::Arc;

use super::error::{PersistentARTrieError, Result};
use super::wal::{
    AsyncWalConfig, AsyncWalError, AsyncWalWriter, Lsn, SyncHandle, WalConfig, WalRecord,
};

/// Trait for types that support WAL-based persistence.
///
/// Implementations store an `Arc<AsyncWalWriter>` and delegate WAL
/// operations through this trait's default methods.
///
/// # Thread Safety
///
/// `AsyncWalWriter` handles its own synchronization internally via a `Mutex`,
/// so no external locking is needed around WAL operations.
pub trait WalManaged {
    /// Get reference to the WAL writer (if persistence is enabled).
    ///
    /// Returns `None` for in-memory mode where no WAL is configured.
    fn wal_writer(&self) -> Option<&Arc<AsyncWalWriter>>;

    /// Log an insert operation to WAL.
    ///
    /// # Arguments
    ///
    /// * `term` - The term bytes to insert
    /// * `value` - Optional serialized value bytes
    ///
    /// # Returns
    ///
    /// The LSN assigned to this record, or `None` if WAL is disabled.
    fn log_insert(&self, term: &[u8], value: Option<Vec<u8>>) -> Result<Option<Lsn>> {
        if let Some(wal) = self.wal_writer() {
            let record = WalRecord::Insert {
                term: term.to_vec(),
                value,
            };
            let lsn = wal.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "log_insert",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(lsn))
        } else {
            Ok(None)
        }
    }

    /// Log a remove operation to WAL.
    ///
    /// # Arguments
    ///
    /// * `term` - The term bytes to remove
    ///
    /// # Returns
    ///
    /// The LSN assigned to this record, or `None` if WAL is disabled.
    fn log_remove(&self, term: &[u8]) -> Result<Option<Lsn>> {
        if let Some(wal) = self.wal_writer() {
            let record = WalRecord::Remove {
                term: term.to_vec(),
            };
            let lsn = wal.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "log_remove",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(lsn))
        } else {
            Ok(None)
        }
    }

    /// Log a batch of insert operations to WAL as a single record.
    ///
    /// This is more efficient than logging individual inserts because it reduces
    /// WAL header overhead from 17 bytes per insert to 17+4 bytes for the entire batch.
    ///
    /// # Arguments
    ///
    /// * `entries` - Slice of (term_bytes, optional_value_bytes) pairs
    ///
    /// # Returns
    ///
    /// The LSN assigned to this batch record, or `None` if WAL is disabled.
    fn log_batch(&self, entries: &[(Vec<u8>, Option<Vec<u8>>)]) -> Result<Option<Lsn>> {
        if let Some(wal) = self.wal_writer() {
            let lsn = wal.append_batch(entries).map_err(|e| {
                PersistentARTrieError::io_error(
                    "log_batch",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(lsn))
        } else {
            Ok(None)
        }
    }

    /// Log an increment operation to WAL.
    ///
    /// # Arguments
    ///
    /// * `term` - The term bytes
    /// * `delta` - The delta to add
    /// * `result` - The resulting value after increment
    ///
    /// # Returns
    ///
    /// The LSN assigned to this record, or `None` if WAL is disabled.
    fn log_increment(&self, term: &[u8], delta: i64, result: i64) -> Result<Option<Lsn>> {
        if let Some(wal) = self.wal_writer() {
            let record = WalRecord::Increment {
                term: term.to_vec(),
                delta,
                result,
            };
            let lsn = wal.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "log_increment",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(lsn))
        } else {
            Ok(None)
        }
    }

    /// Log an upsert operation to WAL.
    ///
    /// # Arguments
    ///
    /// * `term` - The term bytes
    /// * `value` - The serialized value bytes
    ///
    /// # Returns
    ///
    /// The LSN assigned to this record, or `None` if WAL is disabled.
    fn log_upsert(&self, term: &[u8], value: Vec<u8>) -> Result<Option<Lsn>> {
        if let Some(wal) = self.wal_writer() {
            let record = WalRecord::Upsert {
                term: term.to_vec(),
                value,
            };
            let lsn = wal.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "log_upsert",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(lsn))
        } else {
            Ok(None)
        }
    }

    /// Log a compare-and-swap operation to WAL.
    ///
    /// # Arguments
    ///
    /// * `term` - The term bytes
    /// * `expected` - The expected current value (None means term should not exist)
    /// * `new_value` - The new value to set
    /// * `success` - Whether the swap succeeded
    ///
    /// # Returns
    ///
    /// The LSN assigned to this record, or `None` if WAL is disabled.
    fn log_compare_and_swap(
        &self,
        term: &[u8],
        expected: Option<Vec<u8>>,
        new_value: Vec<u8>,
        success: bool,
    ) -> Result<Option<Lsn>> {
        if let Some(wal) = self.wal_writer() {
            let record = WalRecord::CompareAndSwap {
                term: term.to_vec(),
                expected,
                new_value,
                success,
            };
            let lsn = wal.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "log_compare_and_swap",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(lsn))
        } else {
            Ok(None)
        }
    }

    /// Log a begin transaction record to WAL.
    ///
    /// # Arguments
    ///
    /// * `tx_id` - The transaction ID
    ///
    /// # Returns
    ///
    /// The LSN assigned to this record, or `None` if WAL is disabled.
    fn log_begin_tx(&self, tx_id: u64) -> Result<Option<Lsn>> {
        if let Some(wal) = self.wal_writer() {
            let record = WalRecord::BeginTx { tx_id };
            let lsn = wal.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "log_begin_tx",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(lsn))
        } else {
            Ok(None)
        }
    }

    /// Log a commit transaction record to WAL.
    ///
    /// # Arguments
    ///
    /// * `tx_id` - The transaction ID
    ///
    /// # Returns
    ///
    /// The LSN assigned to this record, or `None` if WAL is disabled.
    fn log_commit_tx(&self, tx_id: u64) -> Result<Option<Lsn>> {
        if let Some(wal) = self.wal_writer() {
            let record = WalRecord::CommitTx { tx_id };
            let lsn = wal.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "log_commit_tx",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(lsn))
        } else {
            Ok(None)
        }
    }

    /// Log an abort transaction record to WAL.
    ///
    /// # Arguments
    ///
    /// * `tx_id` - The transaction ID
    ///
    /// # Returns
    ///
    /// The LSN assigned to this record, or `None` if WAL is disabled.
    fn log_abort_tx(&self, tx_id: u64) -> Result<Option<Lsn>> {
        if let Some(wal) = self.wal_writer() {
            let record = WalRecord::AbortTx { tx_id };
            let lsn = wal.append(record).map_err(|e| {
                PersistentARTrieError::io_error(
                    "log_abort_tx",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(lsn))
        } else {
            Ok(None)
        }
    }

    /// Sync WAL to disk (blocking).
    ///
    /// This performs a simple in-place fsync without segment rotation.
    ///
    /// # Returns
    ///
    /// The highest LSN that is now durable, or `None` if WAL is disabled.
    fn wal_sync(&self) -> Result<Option<Lsn>> {
        if let Some(wal) = self.wal_writer() {
            let lsn = wal.sync().map_err(|e| {
                PersistentARTrieError::io_error(
                    "wal_sync",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(lsn))
        } else {
            Ok(None)
        }
    }

    /// Non-blocking sync - initiates WAL sync and returns a handle.
    ///
    /// This rotates the WAL to a pending segment and syncs it in the background.
    /// Writers can continue appending while sync happens.
    ///
    /// # Returns
    ///
    /// A `SyncHandle` that can be used to wait for sync completion,
    /// or `None` if WAL is disabled.
    fn wal_sync_async(&self) -> Result<Option<SyncHandle>> {
        if let Some(wal) = self.wal_writer() {
            let handle = wal.sync_async().map_err(|e| {
                PersistentARTrieError::io_error(
                    "wal_sync_async",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(handle))
        } else {
            Ok(None)
        }
    }

    /// Get current LSN (next LSN to be assigned).
    ///
    /// # Returns
    ///
    /// The current LSN, or `None` if WAL is disabled.
    fn wal_current_lsn(&self) -> Option<Lsn> {
        self.wal_writer().map(|w| w.current_lsn())
    }

    /// Get last synced LSN.
    ///
    /// # Returns
    ///
    /// The last synced LSN, or `None` if WAL is disabled.
    fn wal_synced_lsn(&self) -> Option<Lsn> {
        self.wal_writer().map(|w| w.synced_lsn())
    }

    /// Log a checkpoint record to WAL.
    ///
    /// # Arguments
    ///
    /// * `checkpoint_lsn` - The LSN up to which data is durable in the main file
    ///
    /// # Returns
    ///
    /// The LSN assigned to this checkpoint record, or `None` if WAL is disabled.
    fn log_checkpoint(&self, checkpoint_lsn: Lsn) -> Result<Option<Lsn>> {
        if let Some(wal) = self.wal_writer() {
            let lsn = wal.checkpoint(checkpoint_lsn).map_err(|e| {
                PersistentARTrieError::io_error(
                    "log_checkpoint",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(Some(lsn))
        } else {
            Ok(None)
        }
    }

    /// Truncate WAL after checkpoint.
    ///
    /// Discards all records after the header. Only call this when all WAL records
    /// have been properly recovered and applied.
    fn wal_truncate(&self) -> Result<()> {
        if let Some(wal) = self.wal_writer() {
            wal.truncate().map_err(|e| {
                PersistentARTrieError::io_error(
                    "wal_truncate",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
        }
        Ok(())
    }

    /// Rotate WAL to archive directory after checkpoint.
    ///
    /// This is an O(1) filesystem rename operation that preserves WAL segments
    /// for recovery purposes. If archive mode is disabled in the config, this
    /// falls back to truncate.
    ///
    /// # Arguments
    ///
    /// * `config` - WAL configuration with archive settings
    ///
    /// # Returns
    ///
    /// The path to the archived segment if archiving occurred, or `None` if
    /// archive mode is disabled or WAL is not configured.
    fn wal_rotate_to_archive(&self, config: &WalConfig) -> Result<Option<std::path::PathBuf>> {
        if let Some(wal) = self.wal_writer() {
            let path = wal.rotate_to_archive(config).map_err(|e| {
                PersistentARTrieError::io_error(
                    "wal_rotate_to_archive",
                    "WAL",
                    std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                )
            })?;
            Ok(path)
        } else {
            Ok(None)
        }
    }
}

/// Helper to create an AsyncWalWriter with standard configuration.
///
/// # Arguments
///
/// * `wal_path` - Path to the WAL file
/// * `data_path` - Path to the main data file (used to derive pending directory)
///
/// # Returns
///
/// A new `AsyncWalWriter` configured with sensible defaults.
pub fn create_async_wal<P: AsRef<Path>>(
    wal_path: P,
    data_path: &Path,
) -> std::result::Result<AsyncWalWriter, AsyncWalError> {
    let async_config = AsyncWalConfig {
        pending_dir: data_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("wal_pending"),
        ..Default::default()
    };
    let archive_config = WalConfig::default();
    AsyncWalWriter::create(wal_path, async_config, archive_config)
}

/// Helper to open or create an AsyncWalWriter with standard configuration.
///
/// Uses TOCTOU-safe pattern to avoid race conditions.
///
/// # Arguments
///
/// * `wal_path` - Path to the WAL file
/// * `data_path` - Path to the main data file (used to derive pending directory)
///
/// # Returns
///
/// An `AsyncWalWriter` for the given path, creating it if it doesn't exist.
pub fn open_or_create_async_wal<P: AsRef<Path>>(
    wal_path: P,
    data_path: &Path,
) -> std::result::Result<AsyncWalWriter, AsyncWalError> {
    let async_config = AsyncWalConfig {
        pending_dir: data_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("wal_pending"),
        ..Default::default()
    };
    let archive_config = WalConfig::default();
    AsyncWalWriter::open_or_create(wal_path, async_config, archive_config)
}

/// Helper to open an existing AsyncWalWriter with standard configuration.
///
/// # Arguments
///
/// * `wal_path` - Path to the WAL file (must exist)
/// * `data_path` - Path to the main data file (used to derive pending directory)
///
/// # Returns
///
/// An `AsyncWalWriter` for the existing WAL file.
pub fn open_async_wal<P: AsRef<Path>>(
    wal_path: P,
    data_path: &Path,
) -> std::result::Result<AsyncWalWriter, AsyncWalError> {
    let async_config = AsyncWalConfig {
        pending_dir: data_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("wal_pending"),
        ..Default::default()
    };
    let archive_config = WalConfig::default();
    AsyncWalWriter::open(wal_path, async_config, archive_config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Test helper that implements WalManaged
    struct TestWalManaged {
        wal_writer: Option<Arc<AsyncWalWriter>>,
    }

    impl WalManaged for TestWalManaged {
        fn wal_writer(&self) -> Option<&Arc<AsyncWalWriter>> {
            self.wal_writer.as_ref()
        }
    }

    #[test]
    fn test_wal_managed_without_wal() {
        let managed = TestWalManaged { wal_writer: None };

        // All operations should return Ok(None) when WAL is disabled
        assert_eq!(managed.log_insert(b"test", None).unwrap(), None);
        assert_eq!(managed.log_remove(b"test").unwrap(), None);
        assert_eq!(managed.log_batch(&[]).unwrap(), None);
        assert_eq!(managed.wal_sync().unwrap(), None);
        assert!(managed.wal_sync_async().unwrap().is_none());
        assert_eq!(managed.wal_current_lsn(), None);
        assert_eq!(managed.wal_synced_lsn(), None);
        assert!(managed.wal_truncate().is_ok());
    }

    #[test]
    fn test_wal_managed_with_wal() {
        let dir = tempdir().unwrap();
        let wal_path = dir.path().join("test.wal");
        let data_path = dir.path().join("test.data");

        let wal = create_async_wal(&wal_path, &data_path).unwrap();
        let managed = TestWalManaged {
            wal_writer: Some(Arc::new(wal)),
        };

        // Insert should return an LSN
        let lsn = managed.log_insert(b"hello", None).unwrap();
        assert!(lsn.is_some());
        assert!(lsn.unwrap() > 0);

        // Remove should return an LSN
        let lsn = managed.log_remove(b"hello").unwrap();
        assert!(lsn.is_some());

        // Batch should return an LSN
        let entries = vec![
            (b"a".to_vec(), None),
            (b"b".to_vec(), Some(b"value".to_vec())),
        ];
        let lsn = managed.log_batch(&entries).unwrap();
        assert!(lsn.is_some());

        // Sync should return an LSN
        let lsn = managed.wal_sync().unwrap();
        assert!(lsn.is_some());

        // Current LSN should be available
        assert!(managed.wal_current_lsn().is_some());

        // Synced LSN should be available after sync
        assert!(managed.wal_synced_lsn().is_some());
    }

    #[test]
    fn test_wal_sync_async() {
        let dir = tempdir().unwrap();
        let wal_path = dir.path().join("test_async.wal");
        let data_path = dir.path().join("test_async.data");

        let wal = create_async_wal(&wal_path, &data_path).unwrap();
        let managed = TestWalManaged {
            wal_writer: Some(Arc::new(wal)),
        };

        // Insert some data to sync
        managed
            .log_insert(b"key1", Some(b"value1".to_vec()))
            .unwrap();
        managed
            .log_insert(b"key2", Some(b"value2".to_vec()))
            .unwrap();

        // Async sync should return a handle
        let handle = managed.wal_sync_async().unwrap();
        assert!(handle.is_some());

        // Wait for sync to complete
        let sync_handle = handle.unwrap();
        sync_handle
            .wait()
            .expect("sync should complete successfully");

        // Synced LSN should be updated
        assert!(managed.wal_synced_lsn().is_some());
    }

    #[test]
    fn test_create_and_open_helpers() {
        let dir = tempdir().unwrap();
        let wal_path = dir.path().join("test.wal");
        let data_path = dir.path().join("test.data");

        // Create should succeed
        let wal = create_async_wal(&wal_path, &data_path).unwrap();
        drop(wal);

        // Open should succeed for existing file
        let wal = open_async_wal(&wal_path, &data_path).unwrap();
        drop(wal);

        // Open or create should work for both cases
        let wal = open_or_create_async_wal(&wal_path, &data_path).unwrap();
        drop(wal);

        let new_path = dir.path().join("new.wal");
        let wal = open_or_create_async_wal(&new_path, &data_path).unwrap();
        drop(wal);
    }
}
