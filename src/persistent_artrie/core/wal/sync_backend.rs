//! Pluggable `fsync` backends for WAL segments.
//!
//! Originally inline in `persistent_artrie/core/wal.rs` lines 2033-2130;
//! split into its own file as part of the Phase-4 wal decomposition so the
//! io_uring-specific implementation is independently navigable.

/// Abstraction over the fsync mechanism for WAL segments.
///
/// This trait allows swapping the fsync implementation between:
/// - Standard `file.sync_all()` (the default, works everywhere)
/// - io_uring `IORING_OP_FSYNC` (Linux-only, avoids blocking the sync thread)
///
/// The `SegmentSyncManager` uses this trait to perform durable writes.
pub trait WalSyncBackend: Send + Sync {
    /// Sync a file's data and metadata to durable storage.
    ///
    /// This must provide the same durability guarantee as `file.sync_all()`.
    fn sync_file(&self, file: &std::fs::File) -> std::io::Result<()>;
}

/// Standard fsync backend using `file.sync_all()`.
///
/// This is the default backend and works on all platforms.
pub struct StdFsync;

impl WalSyncBackend for StdFsync {
    #[inline]
    fn sync_file(&self, file: &std::fs::File) -> std::io::Result<()> {
        file.sync_all()
    }
}

/// io_uring-based fsync backend for WAL segments.
///
/// Submits `IORING_OP_FSYNC` via a dedicated io_uring ring, avoiding the
/// blocking `sync_all()` syscall. This can reduce sync thread latency
/// and improve throughput when multiple segments are queued.
///
/// # Thread Safety
///
/// The io_uring ring is protected by a `parking_lot::Mutex`. Since WAL fsync
/// is typically serialized through the single sync thread, contention is minimal.
#[cfg(feature = "io-uring-backend")]
pub struct IoUringFsync {
    ring: parking_lot::Mutex<io_uring::IoUring>,
}

#[cfg(feature = "io-uring-backend")]
impl IoUringFsync {
    /// Create a new io_uring fsync backend.
    ///
    /// # Arguments
    /// * `ring_entries` - Number of SQE entries in the ring (default: 8 is sufficient for WAL sync)
    pub fn new(ring_entries: u32) -> std::io::Result<Self> {
        let ring = io_uring::IoUring::new(ring_entries)?;
        Ok(Self {
            ring: parking_lot::Mutex::new(ring),
        })
    }

    /// Create with default ring size (8 entries).
    pub fn default_ring() -> std::io::Result<Self> {
        Self::new(8)
    }
}

#[cfg(feature = "io-uring-backend")]
impl WalSyncBackend for IoUringFsync {
    fn sync_file(&self, file: &std::fs::File) -> std::io::Result<()> {
        use std::os::unix::io::AsRawFd;

        let fd = io_uring::types::Fd(file.as_raw_fd());
        let fsync_op = io_uring::opcode::Fsync::new(fd).build();

        let mut ring = self.ring.lock();

        // Submit fsync SQE
        unsafe {
            ring.submission().push(&fsync_op).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::Other, "io_uring submission queue full")
            })?;
        }

        // Submit and wait for completion
        ring.submit_and_wait(1)?;

        // Check the CQE result
        let cqe = ring.completion().next().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                "io_uring: no completion entry after fsync",
            )
        })?;

        let result = cqe.result();
        if result < 0 {
            return Err(std::io::Error::from_raw_os_error(-result));
        }

        Ok(())
    }
}
