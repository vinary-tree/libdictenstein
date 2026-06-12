//! `WalError` and its trait impls.
//!
//! Split out of the monolithic `wal.rs` (lines 823-877) as part of the
//! Phase-4 wal decomposition. `AsyncWalError` will join this module in a
//! later incremental split.

use std::io;
use std::path::PathBuf;

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
    /// A regime restamp (`set_overlay_regime`/`set_owned_regime`) was attempted on a
    /// NON-empty WAL (records already appended). An in-place restamp would
    /// mis-classify the pre-existing records (Owned records placed under the Overlay
    /// drop, or vice versa); the non-empty transition requires a WAL rotation.
    InvalidRegimeStamp(String),
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
            WalError::InvalidRegimeStamp(msg) => {
                write!(f, "Invalid WAL regime stamp: {}", msg)
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
            | WalError::ParentNotFound(_)
            | WalError::InvalidRegimeStamp(_) => None,
        }
    }
}

impl From<io::Error> for WalError {
    fn from(err: io::Error) -> Self {
        WalError::Io(err)
    }
}
