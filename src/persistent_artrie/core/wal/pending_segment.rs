//! `PendingSegment` data carrier for the async WAL writer.
//!
//! Split out of the monolithic `wal.rs` (lines ~183-199) as a small first
//! cut of the async-writer cluster's decomposition. Holds the path, LSN
//! range, file handle, rotation timestamp, and size of a WAL segment that
//! has been rotated away from the active writer but still awaits its
//! background fsync.

use std::fs::File;
use std::path::PathBuf;
use std::time::Instant;

use super::Lsn;

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
