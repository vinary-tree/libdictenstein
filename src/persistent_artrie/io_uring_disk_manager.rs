//! io_uring-based Block Storage Backend
//!
//! This module provides an alternative to [`MmapDiskManager`] using Linux io_uring
//! and `O_DIRECT` for block I/O. This eliminates double-caching (no kernel page cache)
//! and provides predictable latency with explicit I/O scheduling.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────┐
//! │     BufferManager<S>        │
//! │   (page cache, Clock LRU)   │
//! ├─────────────────────────────┤
//! │   IoUringDiskManager        │
//! │  ┌───────────────────────┐  │
//! │  │  io_uring ring        │  │
//! │  │  (async SQE/CQE)      │  │
//! │  ├───────────────────────┤  │
//! │  │  Write-back cache     │  │
//! │  │  (sub-block coalesce)  │  │
//! │  ├───────────────────────┤  │
//! │  │  O_DIRECT file I/O    │  │
//! │  │  (bypass page cache)   │  │
//! │  └───────────────────────┘  │
//! └─────────────────────────────┘
//! ```
//!
//! # Key Advantages over mmap
//!
//! - **No double caching**: `O_DIRECT` bypasses kernel page cache; `BufferManager` is the
//!   only cache layer.
//! - **Predictable latency**: No page faults; all I/O is explicit and measurable.
//! - **Batched submissions**: Multiple I/O operations submitted in one syscall via
//!   `read_blocks_batch` / `write_blocks_batch`.
//! - **Write-back cache**: Sub-block `write_bytes` calls are coalesced per-block,
//!   reducing I/O amplification for header/metadata updates.
//!
//! # Requirements
//!
//! - **Linux kernel >= 5.1** (io_uring support)
//! - **Filesystem supporting O_DIRECT** (ext4, xfs, btrfs, etc.)
//! - **Feature flag**: `io-uring-backend`
//!
//! # Thread Safety
//!
//! The io_uring ring is protected by `parking_lot::Mutex`. Contention is low because
//! I/O latency dominates. For high-concurrency workloads, consider per-thread rings
//! (future optimization).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use io_uring::{opcode, types, IoUring};
use parking_lot::Mutex;

use super::block_storage::{AlignedBlock, BlockStorage};
use super::disk_manager::{FileHeader, BLOCK_SIZE, MAX_BLOCK_COUNT};
use super::error::{PersistentARTrieError, Result};

/// Default io_uring queue depth (number of submission queue entries).
///
/// 256 is a good default: large enough for batched I/O while keeping kernel
/// memory usage reasonable (~32KB for the SQ/CQ rings).
const DEFAULT_RING_ENTRIES: u32 = 256;

/// Cached block for sub-block I/O coalescing.
///
/// When `write_bytes` modifies part of a block, the full block is loaded into
/// this cache, modified in-place, and marked dirty. On `sync()`, all dirty
/// blocks are flushed to disk via io_uring.
#[derive(Debug)]
struct CachedBlock {
    data: Box<AlignedBlock>,
    dirty: bool,
}

/// io_uring + O_DIRECT block storage backend.
///
/// Provides the same [`BlockStorage`] interface as [`MmapDiskManager`](super::disk_manager::MmapDiskManager)
/// but uses io_uring for async I/O and `O_DIRECT` to bypass the kernel page cache.
///
/// # Block Allocation
///
/// Uses the same CAS-based lock-free allocation as `MmapDiskManager`, but
/// extends the file via `ftruncate` + `fallocate` instead of mmap remapping.
/// This is simpler because there is no mmap to remap on file extension.
///
/// # Sub-block I/O
///
/// `O_DIRECT` requires sector-aligned I/O (typically 4096 bytes). Sub-block
/// reads and writes (e.g., header field updates) go through a write-back
/// cache that coalesces modifications per-block, reducing I/O amplification.
///
/// # Batch Operations
///
/// `read_blocks_batch` and `write_blocks_batch` submit multiple SQEs in a
/// single `submit_and_wait` call, amortizing syscall overhead across many
/// blocks. This is the primary throughput advantage over mmap for sequential
/// arena flushes.
///
/// # Future Optimization: AlignedBlock Pool
///
/// If Phase 3 benchmarks show allocator contention under heavy concurrent
/// load, introduce an `AlignedBlock` freelist pool (`Vec<Box<AlignedBlock>>`)
/// to recycle aligned buffers instead of heap-allocating on each misaligned
/// I/O or cache miss. Currently, most hot-path I/O uses pre-aligned
/// `BufferManager` buffers so the intermediate allocation path is rare,
/// but this should be validated with profiling.
pub struct IoUringDiskManager {
    /// The underlying file (opened with O_DIRECT).
    file: File,
    /// Raw file descriptor for io_uring operations.
    fd: i32,
    /// io_uring instance for async I/O submission.
    ring: Mutex<IoUring>,
    /// Current file size in bytes.
    file_size: AtomicU64,
    /// In-memory block count for lock-free CAS allocation.
    block_count: AtomicU32,
    /// Path to the file.
    path: String,
    /// Write-back cache for sub-block I/O coalescing.
    ///
    /// Maps `block_id -> CachedBlock`. Dirty blocks are flushed on `sync()`.
    /// Used by `read_bytes`, `write_bytes`, `read_header`, `write_header`.
    block_cache: Mutex<HashMap<u32, CachedBlock>>,
}

// SAFETY: IoUringDiskManager is Send + Sync because:
// - File is Send + Sync
// - All mutable state is behind atomic ops or Mutex
// - io_uring ring is behind Mutex
// - block_cache is behind Mutex
unsafe impl Send for IoUringDiskManager {}
unsafe impl Sync for IoUringDiskManager {}

impl IoUringDiskManager {
    /// Create a new io_uring disk manager, creating the file if it doesn't exist.
    ///
    /// The file is opened with `O_DIRECT` to bypass the kernel page cache.
    /// An io_uring ring is created with [`DEFAULT_RING_ENTRIES`] (256) SQEs.
    ///
    /// # Arguments
    /// * `path` - Path to the data file
    ///
    /// # Errors
    /// - `IoUringError` if io_uring ring creation fails (requires kernel >= 5.1)
    /// - `IoError` if file creation fails or O_DIRECT is not supported
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();

        // Ensure parent directory exists
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| PersistentARTrieError::IoError {
                    operation: "create parent directory".to_string(),
                    path: parent.display().to_string(),
                    source: e,
                })?;
            }
        }

        // Open or create file with O_DIRECT
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .custom_flags(libc::O_DIRECT)
            .open(&path)
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "create file with O_DIRECT".to_string(),
                path: path_str.clone(),
                source: e,
            })?;

        let fd = file.as_raw_fd();
        let metadata = file.metadata().map_err(|e| PersistentARTrieError::IoError {
            operation: "get metadata".to_string(),
            path: path_str.clone(),
            source: e,
        })?;

        let file_size = metadata.len();

        // If file is empty, initialize with header block
        if file_size == 0 {
            Self::initialize_file(&file, &path_str)?;
        }

        let file_size = file
            .metadata()
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "get metadata after init".to_string(),
                path: path_str.clone(),
                source: e,
            })?
            .len();

        // Create io_uring ring
        let ring = IoUring::new(DEFAULT_RING_ENTRIES).map_err(|e| {
            PersistentARTrieError::IoUringError {
                operation: "create io_uring ring".to_string(),
                source: e,
            }
        })?;

        let block_count = (file_size / BLOCK_SIZE as u64) as u32;

        Ok(Self {
            file,
            fd,
            ring: Mutex::new(ring),
            file_size: AtomicU64::new(file_size),
            block_count: AtomicU32::new(block_count),
            path: path_str,
            block_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Open an existing io_uring disk manager (file must exist).
    ///
    /// Validates the file header on open.
    ///
    /// # Arguments
    /// * `path` - Path to the data file
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_DIRECT)
            .open(&path)
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "open file with O_DIRECT".to_string(),
                path: path_str.clone(),
                source: e,
            })?;

        let fd = file.as_raw_fd();

        let file_size = file
            .metadata()
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "get metadata".to_string(),
                path: path_str.clone(),
                source: e,
            })?
            .len();

        if file_size < BLOCK_SIZE as u64 {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: "File too small to contain header block".to_string(),
            });
        }

        let ring = IoUring::new(DEFAULT_RING_ENTRIES).map_err(|e| {
            PersistentARTrieError::IoUringError {
                operation: "create io_uring ring".to_string(),
                source: e,
            }
        })?;

        let block_count = (file_size / BLOCK_SIZE as u64) as u32;

        let manager = Self {
            file,
            fd,
            ring: Mutex::new(ring),
            file_size: AtomicU64::new(file_size),
            block_count: AtomicU32::new(block_count),
            path: path_str,
            block_cache: Mutex::new(HashMap::new()),
        };

        // Validate header
        let header = manager.read_header_internal()?;
        header.validate()?;

        if !header.verify_checksum() {
            return Err(PersistentARTrieError::ChecksumMismatch {
                block_id: 0,
                expected: header.compute_checksum(),
                found: header.checksum,
            });
        }

        Ok(manager)
    }

    /// Open without header validation (for custom header formats like VocabTrie).
    pub fn open_without_validation<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_DIRECT)
            .open(&path)
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "open file with O_DIRECT".to_string(),
                path: path_str.clone(),
                source: e,
            })?;

        let fd = file.as_raw_fd();

        let file_size = file
            .metadata()
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "get metadata".to_string(),
                path: path_str.clone(),
                source: e,
            })?
            .len();

        if file_size < BLOCK_SIZE as u64 {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: "File too small to contain header block".to_string(),
            });
        }

        let ring = IoUring::new(DEFAULT_RING_ENTRIES).map_err(|e| {
            PersistentARTrieError::IoUringError {
                operation: "create io_uring ring".to_string(),
                source: e,
            }
        })?;

        let block_count = (file_size / BLOCK_SIZE as u64) as u32;

        Ok(Self {
            file,
            fd,
            ring: Mutex::new(ring),
            file_size: AtomicU64::new(file_size),
            block_count: AtomicU32::new(block_count),
            path: path_str,
            block_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Create with custom ring size.
    ///
    /// # Arguments
    /// * `path` - Path to the data file
    /// * `ring_entries` - Number of io_uring SQEs (must be power of 2, min 1, max 32768)
    pub fn create_with_ring_size<P: AsRef<Path>>(path: P, ring_entries: u32) -> Result<Self> {
        let mut manager = Self::create(path)?;
        // Replace the ring with a custom-sized one
        let ring = IoUring::new(ring_entries).map_err(|e| PersistentARTrieError::IoUringError {
            operation: "create io_uring ring with custom size".to_string(),
            source: e,
        })?;
        manager.ring = Mutex::new(ring);
        Ok(manager)
    }

    /// Initialize a new file with a header block.
    ///
    /// Uses `pwrite` with an aligned buffer for O_DIRECT compatibility.
    fn initialize_file(file: &File, path: &str) -> Result<()> {
        // Extend file to one header block
        file.set_len(BLOCK_SIZE as u64)
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "set initial file length".to_string(),
                path: path.to_string(),
                source: e,
            })?;

        // Create header and write via pwrite (aligned buffer for O_DIRECT)
        let mut block = AlignedBlock::new_boxed();
        let mut header = FileHeader::new();
        header.update_checksum();
        block.data[..64].copy_from_slice(&header.to_bytes());

        // pwrite the full block at offset 0 (O_DIRECT: buffer is 4096-aligned,
        // offset is 0, size is BLOCK_SIZE = multiple of 4096)
        file.write_all_at(&block.data, 0)
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "write initial header block".to_string(),
                path: path.to_string(),
                source: e,
            })?;

        file.sync_all()
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "sync after header write".to_string(),
                path: path.to_string(),
                source: e,
            })?;

        Ok(())
    }

    // =========================================================================
    // Validation helpers
    // =========================================================================

    /// Validate a block ID against current block count and file size.
    #[inline]
    fn validate_block_id(&self, block_id: u32) -> Result<()> {
        let current_count = self.block_count.load(Ordering::Acquire);
        if block_id >= current_count {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!("Block ID {} >= block count {}", block_id, current_count),
            });
        }

        let end_offset = (block_id as u64 + 1) * BLOCK_SIZE as u64;
        let current_file_size = self.file_size.load(Ordering::Acquire);
        if end_offset > current_file_size {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Block {} not yet accessible (file_size={}, need={})",
                    block_id, current_file_size, end_offset
                ),
            });
        }

        Ok(())
    }

    // =========================================================================
    // io_uring I/O primitives
    // =========================================================================

    /// Read a full block via io_uring.
    ///
    /// If the destination buffer is 4096-byte aligned (e.g., from `AlignedBlock`),
    /// reads directly into it. Otherwise uses an intermediate aligned buffer.
    fn read_block_uring(&self, block_id: u32, buffer: &mut [u8; BLOCK_SIZE]) -> Result<()> {
        let offset = block_id as u64 * BLOCK_SIZE as u64;
        let ptr = buffer.as_ptr() as usize;
        let is_aligned = ptr % 4096 == 0;

        if is_aligned {
            self.submit_read(buffer.as_mut_ptr(), BLOCK_SIZE, offset)
        } else {
            let mut aligned = AlignedBlock::new_boxed();
            self.submit_read(aligned.data.as_mut_ptr(), BLOCK_SIZE, offset)?;
            buffer.copy_from_slice(&aligned.data);
            Ok(())
        }
    }

    /// Write a full block via io_uring.
    ///
    /// If the source buffer is 4096-byte aligned, writes directly from it.
    /// Otherwise copies to an aligned intermediate buffer first.
    fn write_block_uring(&self, block_id: u32, buffer: &[u8; BLOCK_SIZE]) -> Result<()> {
        let offset = block_id as u64 * BLOCK_SIZE as u64;
        let ptr = buffer.as_ptr() as usize;
        let is_aligned = ptr % 4096 == 0;

        if is_aligned {
            self.submit_write(buffer.as_ptr(), BLOCK_SIZE, offset)
        } else {
            let mut aligned = AlignedBlock::new_boxed();
            aligned.data.copy_from_slice(buffer);
            self.submit_write(aligned.data.as_ptr(), BLOCK_SIZE, offset)
        }
    }

    /// Submit a single read SQE to io_uring and wait for the CQE.
    ///
    /// # Safety contract
    /// - `buf` must point to at least `len` bytes of writable memory
    /// - `buf` must be 4096-byte aligned (O_DIRECT requirement)
    /// - `buf` must remain valid until this function returns
    fn submit_read(&self, buf: *mut u8, len: usize, offset: u64) -> Result<()> {
        let read_e = opcode::Read::new(types::Fd(self.fd), buf, len as u32)
            .offset(offset)
            .build()
            .user_data(0x01);

        let mut ring = self.ring.lock();

        unsafe {
            ring.submission()
                .push(&read_e)
                .map_err(|_| PersistentARTrieError::IoUringError {
                    operation: "push read SQE".to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "io_uring submission queue full",
                    ),
                })?;
        }

        ring.submit_and_wait(1)
            .map_err(|e| PersistentARTrieError::IoUringError {
                operation: "submit_and_wait read".to_string(),
                source: e,
            })?;

        let cqe = ring.completion().next().ok_or_else(|| {
            PersistentARTrieError::IoUringError {
                operation: "read completion".to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "no completion entry after submit_and_wait",
                ),
            }
        })?;

        let result = cqe.result();
        if result < 0 {
            return Err(PersistentARTrieError::IoUringError {
                operation: "read I/O".to_string(),
                source: std::io::Error::from_raw_os_error(-result),
            });
        }
        if (result as usize) < len {
            return Err(PersistentARTrieError::IoUringError {
                operation: "read I/O (short read)".to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("short read: got {} bytes, expected {}", result, len),
                ),
            });
        }

        Ok(())
    }

    /// Submit a single write SQE to io_uring and wait for the CQE.
    ///
    /// # Safety contract
    /// - `buf` must point to at least `len` bytes of readable memory
    /// - `buf` must be 4096-byte aligned (O_DIRECT requirement)
    /// - `buf` must remain valid until this function returns
    fn submit_write(&self, buf: *const u8, len: usize, offset: u64) -> Result<()> {
        let write_e = opcode::Write::new(types::Fd(self.fd), buf, len as u32)
            .offset(offset)
            .build()
            .user_data(0x02);

        let mut ring = self.ring.lock();

        unsafe {
            ring.submission()
                .push(&write_e)
                .map_err(|_| PersistentARTrieError::IoUringError {
                    operation: "push write SQE".to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "io_uring submission queue full",
                    ),
                })?;
        }

        ring.submit_and_wait(1)
            .map_err(|e| PersistentARTrieError::IoUringError {
                operation: "submit_and_wait write".to_string(),
                source: e,
            })?;

        let cqe = ring.completion().next().ok_or_else(|| {
            PersistentARTrieError::IoUringError {
                operation: "write completion".to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "no completion entry after submit_and_wait",
                ),
            }
        })?;

        let result = cqe.result();
        if result < 0 {
            return Err(PersistentARTrieError::IoUringError {
                operation: "write I/O".to_string(),
                source: std::io::Error::from_raw_os_error(-result),
            });
        }
        if (result as usize) < len {
            return Err(PersistentARTrieError::IoUringError {
                operation: "write I/O (short write)".to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    format!("short write: wrote {} bytes, expected {}", result, len),
                ),
            });
        }

        Ok(())
    }

    /// Submit an fsync SQE via io_uring and wait for completion.
    fn submit_fsync(&self) -> Result<()> {
        let fsync_e = opcode::Fsync::new(types::Fd(self.fd))
            .build()
            .user_data(0x03);

        let mut ring = self.ring.lock();

        unsafe {
            ring.submission()
                .push(&fsync_e)
                .map_err(|_| PersistentARTrieError::IoUringError {
                    operation: "push fsync SQE".to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "io_uring submission queue full",
                    ),
                })?;
        }

        ring.submit_and_wait(1)
            .map_err(|e| PersistentARTrieError::IoUringError {
                operation: "submit_and_wait fsync".to_string(),
                source: e,
            })?;

        let cqe = ring.completion().next().ok_or_else(|| {
            PersistentARTrieError::IoUringError {
                operation: "fsync completion".to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "no completion entry after submit_and_wait",
                ),
            }
        })?;

        let result = cqe.result();
        if result < 0 {
            return Err(PersistentARTrieError::IoUringError {
                operation: "fsync".to_string(),
                source: std::io::Error::from_raw_os_error(-result),
            });
        }

        Ok(())
    }

    // =========================================================================
    // Write-back cache for sub-block I/O
    // =========================================================================

    /// Load a block into the cache, reading from disk if not present.
    ///
    /// After this call, the block is guaranteed to be in `block_cache`.
    fn ensure_cached(&self, block_id: u32) -> Result<()> {
        // Quick check under lock
        {
            let cache = self.block_cache.lock();
            if cache.contains_key(&block_id) {
                return Ok(());
            }
        }

        // Cache miss - read from disk (outside cache lock to avoid deadlock with ring lock)
        let mut block = AlignedBlock::new_boxed();
        self.read_block_uring(block_id, &mut block.data)?;

        // Re-acquire cache lock and insert (re-check in case of concurrent load)
        let mut cache = self.block_cache.lock();
        cache.entry(block_id).or_insert(CachedBlock {
            data: block,
            dirty: false,
        });

        Ok(())
    }

    /// Flush all dirty blocks from cache to disk.
    ///
    /// Writes each dirty block via io_uring, then clears dirty flags.
    /// Called by `sync()` before issuing fsync.
    fn flush_dirty_cache(&self) -> Result<()> {
        // Collect dirty blocks and their data (copy out to avoid holding lock during I/O)
        let dirty_entries: Vec<(u32, Box<AlignedBlock>)> = {
            let mut cache = self.block_cache.lock();
            cache
                .iter_mut()
                .filter(|(_, cached)| cached.dirty)
                .map(|(&block_id, cached)| {
                    cached.dirty = false;
                    let mut copy = AlignedBlock::new_boxed();
                    copy.data.copy_from_slice(&cached.data.data);
                    (block_id, copy)
                })
                .collect()
        };

        if dirty_entries.is_empty() {
            return Ok(());
        }

        // Write each dirty block to disk
        for (block_id, block) in &dirty_entries {
            let offset = *block_id as u64 * BLOCK_SIZE as u64;
            self.submit_write(block.data.as_ptr(), BLOCK_SIZE, offset)?;
        }

        Ok(())
    }

    // =========================================================================
    // Internal sub-block I/O (through cache)
    // =========================================================================

    /// Read bytes from a block through the cache.
    fn read_bytes_impl(&self, block_id: u32, offset: usize, buffer: &mut [u8]) -> Result<()> {
        if offset + buffer.len() > BLOCK_SIZE {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Read range [{}, {}) exceeds block size {}",
                    offset,
                    offset + buffer.len(),
                    BLOCK_SIZE
                ),
            });
        }

        self.ensure_cached(block_id)?;

        let cache = self.block_cache.lock();
        let cached = cache
            .get(&block_id)
            .expect("block should be cached after ensure_cached");
        buffer.copy_from_slice(&cached.data.data[offset..offset + buffer.len()]);
        Ok(())
    }

    /// Write bytes to a block through the cache (marks block dirty).
    fn write_bytes_impl(&self, block_id: u32, offset: usize, data: &[u8]) -> Result<()> {
        if offset + data.len() > BLOCK_SIZE {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Write range [{}, {}) exceeds block size {}",
                    offset,
                    offset + data.len(),
                    BLOCK_SIZE
                ),
            });
        }

        self.ensure_cached(block_id)?;

        let mut cache = self.block_cache.lock();
        let cached = cache
            .get_mut(&block_id)
            .expect("block should be cached after ensure_cached");
        cached.data.data[offset..offset + data.len()].copy_from_slice(data);
        cached.dirty = true;
        Ok(())
    }

    /// Read the file header (internal, used during open before BlockStorage is available).
    fn read_header_internal(&self) -> Result<FileHeader> {
        let mut bytes = [0u8; 64];
        self.read_bytes_impl(0, 0, &mut bytes)?;
        Ok(FileHeader::from_bytes(&bytes))
    }

    // =========================================================================
    // Free list helpers
    // =========================================================================

    /// Read the next pointer from a free block.
    fn read_free_block_next(&self, block_id: u32) -> Result<u64> {
        let mut buf = [0u8; 8];
        self.read_bytes_impl(block_id, 0, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    /// Write the next pointer to a free block.
    fn write_free_block_next(&self, block_id: u32, next: u64) -> Result<()> {
        self.write_bytes_impl(block_id, 0, &next.to_le_bytes())
    }

    /// Persist the block count to the on-disk header (best-effort, through cache).
    fn persist_header_block_count(&self, count: u32) {
        let mut cache = self.block_cache.lock();
        if let Some(cached) = cache.get_mut(&0) {
            const BLOCK_COUNT_OFFSET: usize = 24;
            cached.data.data[BLOCK_COUNT_OFFSET..BLOCK_COUNT_OFFSET + 4]
                .copy_from_slice(&count.to_le_bytes());
            cached.dirty = true;
        }
        // If block 0 is not cached, skip - it will be written on next header update
    }

    /// Recompute and update the header checksum in cache before flushing.
    ///
    /// Called by `sync()` to ensure the header block has a valid checksum
    /// after raw field updates (e.g., `persist_header_block_count`).
    fn update_cached_header_checksum(&self) {
        let mut cache = self.block_cache.lock();
        if let Some(cached) = cache.get_mut(&0) {
            if cached.dirty {
                // Read the header fields from the cached raw bytes
                let mut bytes = [0u8; 64];
                bytes.copy_from_slice(&cached.data.data[..64]);
                let header = FileHeader::from_bytes(&bytes);

                // Recompute checksum based on current field values
                let checksum = header.compute_checksum();

                // Write updated checksum back into the cached block
                const CHECKSUM_OFFSET: usize = 48;
                cached.data.data[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 8]
                    .copy_from_slice(&checksum.to_le_bytes());
            }
        }
    }

    /// Get the number of cached blocks.
    pub fn cached_block_count(&self) -> usize {
        self.block_cache.lock().len()
    }

    /// Get the number of dirty cached blocks.
    pub fn dirty_block_count(&self) -> usize {
        self.block_cache
            .lock()
            .values()
            .filter(|c| c.dirty)
            .count()
    }

    /// Clear the block cache (all dirty blocks are lost!).
    ///
    /// Call `sync()` first to flush dirty blocks if needed.
    pub fn clear_cache(&self) {
        self.block_cache.lock().clear();
    }

    /// Read a VocabTrieFileHeader from block 0.
    ///
    /// Convenience method that delegates to the free function in `block_storage`.
    /// Mirrors [`MmapDiskManager::read_vocab_header`](super::disk_manager::MmapDiskManager::read_vocab_header).
    pub fn read_vocab_header(&self) -> Result<crate::persistent_vocab_artrie::types::VocabTrieFileHeader> {
        super::block_storage::read_vocab_header(self)
    }

    /// Get the file path.
    pub fn file_path(&self) -> &str {
        &self.path
    }
}

// =============================================================================
// BlockStorage trait implementation
// =============================================================================

impl BlockStorage for IoUringDiskManager {
    fn read_block(&self, block_id: u32, buffer: &mut [u8; BLOCK_SIZE]) -> Result<()> {
        self.validate_block_id(block_id)?;

        // Check cache first (sub-block writes may have modified this block)
        {
            let cache = self.block_cache.lock();
            if let Some(cached) = cache.get(&block_id) {
                buffer.copy_from_slice(&cached.data.data);
                return Ok(());
            }
        }

        self.read_block_uring(block_id, buffer)
    }

    fn write_block(&self, block_id: u32, buffer: &[u8; BLOCK_SIZE]) -> Result<()> {
        self.validate_block_id(block_id)?;

        // Update cache if block is cached (keeps cache consistent)
        {
            let mut cache = self.block_cache.lock();
            if let Some(cached) = cache.get_mut(&block_id) {
                cached.data.data.copy_from_slice(buffer);
                // Mark NOT dirty since we're writing to disk below
                cached.dirty = false;
            }
        }

        self.write_block_uring(block_id, buffer)
    }

    fn read_bytes(&self, block_id: u32, offset: usize, buffer: &mut [u8]) -> Result<()> {
        self.validate_block_id(block_id)?;
        self.read_bytes_impl(block_id, offset, buffer)
    }

    fn write_bytes(&self, block_id: u32, offset: usize, data: &[u8]) -> Result<()> {
        self.validate_block_id(block_id)?;
        self.write_bytes_impl(block_id, offset, data)
    }

    fn allocate_block(&self) -> Result<u32> {
        // Try free list first
        let header = BlockStorage::read_header(self)?;
        let free_head = header.free_list_head.load(Ordering::Acquire);

        if free_head != 0 {
            let block_id = (free_head >> 40) as u32;
            let next = self.read_free_block_next(block_id)?;
            header.free_list_head.store(next, Ordering::Release);
            BlockStorage::write_header(self, &header)?;
            return Ok(block_id);
        }

        // No free blocks - extend file using CAS loop for lock-free allocation
        loop {
            let current_count = self.block_count.load(Ordering::Acquire);

            if current_count >= MAX_BLOCK_COUNT {
                return Err(PersistentARTrieError::OutOfSpace {
                    current_blocks: current_count,
                    max_blocks: MAX_BLOCK_COUNT,
                });
            }

            let new_block_id = current_count;
            let new_count = current_count + 1;
            let new_file_size = new_count as u64 * BLOCK_SIZE as u64;

            // CAS to claim this block ID - only one thread wins
            match self.block_count.compare_exchange(
                current_count,
                new_count,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    // Winner: extend file via ftruncate (no mmap to remap!)
                    let current_actual_size = self
                        .file
                        .metadata()
                        .map_err(|e| PersistentARTrieError::IoError {
                            operation: "get file metadata before extend".to_string(),
                            path: self.path.clone(),
                            source: e,
                        })?
                        .len();

                    if new_file_size > current_actual_size {
                        self.file
                            .set_len(new_file_size)
                            .map_err(|e| PersistentARTrieError::IoError {
                                operation: "extend file".to_string(),
                                path: self.path.clone(),
                                source: e,
                            })?;

                        // Pre-allocate storage to avoid holes (best-effort).
                        // fallocate prevents sparse regions that could cause ENOSPC later.
                        #[cfg(target_os = "linux")]
                        {
                            let ret = unsafe {
                                libc::fallocate(
                                    self.fd,
                                    0, // default mode: allocate
                                    current_actual_size as i64,
                                    (new_file_size - current_actual_size) as i64,
                                )
                            };
                            if ret != 0 {
                                // fallocate failed - ftruncate already extended the file.
                                // Acceptable: the file may have holes, but I/O will still work.
                                log::debug!(
                                    "fallocate failed (errno={}), continuing with sparse file",
                                    std::io::Error::last_os_error()
                                );
                            }
                        }
                    }

                    // Update file_size atomically (monotonic increase via CAS)
                    loop {
                        let current = self.file_size.load(Ordering::Acquire);
                        if new_file_size <= current {
                            break;
                        }
                        match self.file_size.compare_exchange(
                            current,
                            new_file_size,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        ) {
                            Ok(_) => break,
                            Err(_) => continue,
                        }
                    }

                    // Update on-disk header block count (best-effort through cache)
                    self.persist_header_block_count(new_count);

                    return Ok(new_block_id);
                }
                Err(_) => {
                    // Another thread won - retry with updated count
                    std::hint::spin_loop();
                    continue;
                }
            }
        }
    }

    fn free_block(&self, block_id: u32) -> Result<()> {
        if block_id == 0 {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "Cannot free header block".to_string(),
            });
        }

        let block_count = self.block_count.load(Ordering::Acquire);
        if block_id >= block_count {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!("Block ID {} >= block count {}", block_id, block_count),
            });
        }

        let header = BlockStorage::read_header(self)?;
        let old_head = header.free_list_head.load(Ordering::SeqCst);

        // Write old head pointer into the freed block
        self.write_free_block_next(block_id, old_head)?;

        // Update free list head to point to this block (swizzled format)
        let new_head = (block_id as u64) << 40;

        let mut updated_header = BlockStorage::read_header(self)?;
        updated_header
            .free_list_head
            .store(new_head, Ordering::SeqCst);
        updated_header.checksum = updated_header.compute_checksum();
        BlockStorage::write_header(self, &updated_header)?;

        // Evict freed block from cache
        self.block_cache.lock().remove(&block_id);

        Ok(())
    }

    fn read_header(&self) -> Result<FileHeader> {
        self.read_header_internal()
    }

    fn write_header(&self, header: &FileHeader) -> Result<()> {
        let bytes = header.to_bytes();
        self.write_bytes_impl(0, 0, &bytes)
    }

    fn read_header_bytes(&self, buffer: &mut [u8]) -> Result<()> {
        self.read_bytes_impl(0, 0, buffer)
    }

    fn write_header_bytes(&self, bytes: &[u8]) -> Result<()> {
        self.write_bytes_impl(0, 0, bytes)
    }

    fn root_ptr(&self) -> Result<u64> {
        let header = BlockStorage::read_header(self)?;
        Ok(header.root_ptr.load(Ordering::SeqCst))
    }

    fn set_root_ptr(&self, ptr: u64) -> Result<()> {
        let header = BlockStorage::read_header(self)?;
        header.root_ptr.store(ptr, Ordering::SeqCst);
        let mut updated = header;
        updated.checksum = updated.compute_checksum();
        BlockStorage::write_header(self, &updated)
    }

    fn entry_count(&self) -> Result<u64> {
        let header = BlockStorage::read_header(self)?;
        Ok(header.entry_count.load(Ordering::SeqCst))
    }

    fn set_entry_count(&self, count: u64) -> Result<()> {
        let header = BlockStorage::read_header(self)?;
        header.entry_count.store(count, Ordering::SeqCst);
        let mut updated = header;
        updated.checksum = updated.compute_checksum();
        BlockStorage::write_header(self, &updated)
    }

    fn file_size(&self) -> u64 {
        self.file_size.load(Ordering::SeqCst)
    }

    fn block_count(&self) -> Result<u32> {
        Ok(self.block_count.load(Ordering::Acquire))
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn sync(&self) -> Result<()> {
        // 1. Update header checksum before flushing (raw field updates
        //    like persist_header_block_count don't update the checksum)
        self.update_cached_header_checksum();

        // 2. Flush dirty cache blocks to disk
        self.flush_dirty_cache()?;

        // 3. fsync via io_uring
        self.submit_fsync()
    }

    fn read_blocks_batch(&self, requests: &mut [(u32, &mut [u8; BLOCK_SIZE])]) -> Result<()> {
        if requests.is_empty() {
            return Ok(());
        }

        // Validate all block IDs
        for &(block_id, _) in requests.iter() {
            self.validate_block_id(block_id)?;
        }

        // Determine which blocks need disk I/O vs cache hits
        let mut needs_io: Vec<usize> = Vec::with_capacity(requests.len());
        {
            let cache = self.block_cache.lock();
            for (i, (block_id, buffer)) in requests.iter_mut().enumerate() {
                if let Some(cached) = cache.get(block_id) {
                    buffer.copy_from_slice(&cached.data.data);
                } else {
                    needs_io.push(i);
                }
            }
        }

        if needs_io.is_empty() {
            return Ok(());
        }

        // Allocate aligned buffers for all I/O requests
        let mut aligned_buffers: Vec<Box<AlignedBlock>> =
            (0..needs_io.len()).map(|_| AlignedBlock::new_boxed()).collect();

        // Submit all read SQEs in one batch
        {
            let mut ring = self.ring.lock();

            for (buf_idx, &req_idx) in needs_io.iter().enumerate() {
                let block_id = requests[req_idx].0;
                let offset = block_id as u64 * BLOCK_SIZE as u64;

                let read_e = opcode::Read::new(
                    types::Fd(self.fd),
                    aligned_buffers[buf_idx].data.as_mut_ptr(),
                    BLOCK_SIZE as u32,
                )
                .offset(offset)
                .build()
                .user_data(buf_idx as u64);

                unsafe {
                    ring.submission().push(&read_e).map_err(|_| {
                        PersistentARTrieError::IoUringError {
                            operation: "push batch read SQE".to_string(),
                            source: std::io::Error::new(
                                std::io::ErrorKind::Other,
                                "io_uring submission queue full",
                            ),
                        }
                    })?;
                }
            }

            // Submit all and wait for all completions
            let count = needs_io.len();
            ring.submit_and_wait(count)
                .map_err(|e| PersistentARTrieError::IoUringError {
                    operation: "submit_and_wait batch read".to_string(),
                    source: e,
                })?;

            // Drain all CQEs
            let mut completed = 0;
            for cqe in ring.completion() {
                let result = cqe.result();
                if result < 0 {
                    return Err(PersistentARTrieError::IoUringError {
                        operation: "batch read I/O".to_string(),
                        source: std::io::Error::from_raw_os_error(-result),
                    });
                }
                if (result as usize) < BLOCK_SIZE {
                    return Err(PersistentARTrieError::IoUringError {
                        operation: "batch read I/O (short read)".to_string(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            format!("short read: got {} bytes, expected {}", result, BLOCK_SIZE),
                        ),
                    });
                }
                completed += 1;
            }

            if completed != count {
                return Err(PersistentARTrieError::IoUringError {
                    operation: "batch read completion count".to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("expected {} completions, got {}", count, completed),
                    ),
                });
            }
        }

        // Copy results from aligned buffers to caller's buffers
        for (buf_idx, &req_idx) in needs_io.iter().enumerate() {
            requests[req_idx]
                .1
                .copy_from_slice(&aligned_buffers[buf_idx].data);
        }

        Ok(())
    }

    fn write_blocks_batch(&self, requests: &[(u32, &[u8; BLOCK_SIZE])]) -> Result<()> {
        if requests.is_empty() {
            return Ok(());
        }

        // Validate all block IDs
        for &(block_id, _) in requests {
            self.validate_block_id(block_id)?;
        }

        // Update cache for any cached blocks
        {
            let mut cache = self.block_cache.lock();
            for &(block_id, buffer) in requests {
                if let Some(cached) = cache.get_mut(&block_id) {
                    cached.data.data.copy_from_slice(buffer);
                    cached.dirty = false; // Will be written to disk below
                }
            }
        }

        // Copy all source buffers to aligned buffers
        let aligned_buffers: Vec<Box<AlignedBlock>> = requests
            .iter()
            .map(|(_, buffer)| {
                let mut aligned = AlignedBlock::new_boxed();
                aligned.data.copy_from_slice(*buffer);
                aligned
            })
            .collect();

        // Submit all write SQEs in one batch
        {
            let mut ring = self.ring.lock();

            for (i, &(block_id, _)) in requests.iter().enumerate() {
                let offset = block_id as u64 * BLOCK_SIZE as u64;

                let write_e = opcode::Write::new(
                    types::Fd(self.fd),
                    aligned_buffers[i].data.as_ptr(),
                    BLOCK_SIZE as u32,
                )
                .offset(offset)
                .build()
                .user_data(i as u64);

                unsafe {
                    ring.submission().push(&write_e).map_err(|_| {
                        PersistentARTrieError::IoUringError {
                            operation: "push batch write SQE".to_string(),
                            source: std::io::Error::new(
                                std::io::ErrorKind::Other,
                                "io_uring submission queue full",
                            ),
                        }
                    })?;
                }
            }

            let count = requests.len();
            ring.submit_and_wait(count)
                .map_err(|e| PersistentARTrieError::IoUringError {
                    operation: "submit_and_wait batch write".to_string(),
                    source: e,
                })?;

            let mut completed = 0;
            for cqe in ring.completion() {
                let result = cqe.result();
                if result < 0 {
                    return Err(PersistentARTrieError::IoUringError {
                        operation: "batch write I/O".to_string(),
                        source: std::io::Error::from_raw_os_error(-result),
                    });
                }
                if (result as usize) < BLOCK_SIZE {
                    return Err(PersistentARTrieError::IoUringError {
                        operation: "batch write I/O (short write)".to_string(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::WriteZero,
                            format!(
                                "short write: wrote {} bytes, expected {}",
                                result, BLOCK_SIZE
                            ),
                        ),
                    });
                }
                completed += 1;
            }

            if completed != count {
                return Err(PersistentARTrieError::IoUringError {
                    operation: "batch write completion count".to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("expected {} completions, got {}", count, completed),
                    ),
                });
            }
        }

        Ok(())
    }
}

impl std::fmt::Debug for IoUringDiskManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IoUringDiskManager")
            .field("path", &self.path)
            .field("fd", &self.fd)
            .field("file_size", &self.file_size.load(Ordering::Relaxed))
            .field("block_count", &self.block_count.load(Ordering::Relaxed))
            .field("cached_blocks", &self.block_cache.lock().len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering as AtomicOrdering;
    use tempfile::tempdir;

    #[test]
    fn test_create_and_open() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.part");

        // Create new file
        {
            let dm = IoUringDiskManager::create(&path).expect("create");
            assert_eq!(dm.file_size(), BLOCK_SIZE as u64);

            let header = BlockStorage::read_header(&dm).expect("read header");
            assert_eq!(header.magic, super::super::disk_manager::MAGIC_NUMBER);
            assert_eq!(header.block_count.load(AtomicOrdering::SeqCst), 1);
        }

        // Open existing file
        {
            let dm = IoUringDiskManager::open(&path).expect("open");
            let header = BlockStorage::read_header(&dm).expect("read header");
            assert_eq!(header.magic, super::super::disk_manager::MAGIC_NUMBER);
        }
    }

    #[test]
    fn test_allocate_blocks() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_alloc.part");

        let dm = IoUringDiskManager::create(&path).expect("create");

        let block1 = dm.allocate_block().expect("alloc 1");
        let block2 = dm.allocate_block().expect("alloc 2");
        let block3 = dm.allocate_block().expect("alloc 3");

        assert_eq!(block1, 1);
        assert_eq!(block2, 2);
        assert_eq!(block3, 3);

        assert_eq!(dm.block_count().expect("block count"), 4);
        assert_eq!(dm.file_size(), 4 * BLOCK_SIZE as u64);
    }

    #[test]
    fn test_read_write_block() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_rw.part");

        let dm = IoUringDiskManager::create(&path).expect("create");
        let block_id = dm.allocate_block().expect("alloc");

        // Write test data (heap-allocated to avoid stack overflow with 256KB blocks)
        let mut write_buf = AlignedBlock::new_boxed();
        write_buf.data[0] = 0xDE;
        write_buf.data[1] = 0xAD;
        write_buf.data[2] = 0xBE;
        write_buf.data[3] = 0xEF;
        write_buf.data[BLOCK_SIZE - 1] = 0xFF;

        dm.write_block(block_id, &write_buf.data).expect("write");

        // Read back
        let mut read_buf = AlignedBlock::new_boxed();
        dm.read_block(block_id, &mut read_buf.data).expect("read");

        assert_eq!(read_buf.data[0], 0xDE);
        assert_eq!(read_buf.data[1], 0xAD);
        assert_eq!(read_buf.data[2], 0xBE);
        assert_eq!(read_buf.data[3], 0xEF);
        assert_eq!(read_buf.data[BLOCK_SIZE - 1], 0xFF);
    }

    #[test]
    fn test_read_write_bytes() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_bytes.part");

        let dm = IoUringDiskManager::create(&path).expect("create");
        let block_id = dm.allocate_block().expect("alloc");

        // Write at offset via cache
        let data = b"Hello, io_uring!";
        dm.write_bytes(block_id, 100, data).expect("write bytes");

        // Read back via cache
        let mut read_buf = [0u8; 16];
        dm.read_bytes(block_id, 100, &mut read_buf)
            .expect("read bytes");

        assert_eq!(&read_buf, data);
    }

    #[test]
    fn test_root_ptr() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_root.part");

        let dm = IoUringDiskManager::create(&path).expect("create");

        assert_eq!(dm.root_ptr().expect("root_ptr"), 0);

        dm.set_root_ptr(0x123456789ABCDEF0)
            .expect("set_root_ptr");
        assert_eq!(
            dm.root_ptr().expect("root_ptr after set"),
            0x123456789ABCDEF0
        );

        // Sync and reopen
        dm.sync().expect("sync");
        drop(dm);

        let dm2 = IoUringDiskManager::open(&path).expect("reopen");
        assert_eq!(
            dm2.root_ptr().expect("root_ptr after reopen"),
            0x123456789ABCDEF0
        );
    }

    #[test]
    fn test_entry_count() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_entry.part");

        let dm = IoUringDiskManager::create(&path).expect("create");

        assert_eq!(dm.entry_count().expect("entry_count"), 0);

        dm.set_entry_count(12345).expect("set_entry_count");
        assert_eq!(dm.entry_count().expect("entry_count after set"), 12345);
    }

    #[test]
    fn test_sync_flushes_cache() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_sync.part");

        let dm = IoUringDiskManager::create(&path).expect("create");
        let block_id = dm.allocate_block().expect("alloc");

        // Write via sub-block API (goes through cache)
        dm.write_bytes(block_id, 0, b"cached data!")
            .expect("write bytes");

        // Should have dirty cache entries (block 0 header + data block)
        assert!(
            dm.dirty_block_count() >= 1,
            "expected at least 1 dirty block, got {}",
            dm.dirty_block_count()
        );

        // Sync to flush
        dm.sync().expect("sync");
        assert_eq!(dm.dirty_block_count(), 0);

        // Reopen and verify data persisted
        drop(dm);
        let dm2 = IoUringDiskManager::open(&path).expect("reopen");
        let mut buf = [0u8; 12];
        dm2.read_bytes(block_id, 0, &mut buf).expect("read bytes");
        assert_eq!(&buf, b"cached data!");
    }

    #[test]
    fn test_cannot_free_header_block() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_no_free_header.part");

        let dm = IoUringDiskManager::create(&path).expect("create");
        let result = dm.free_block(0);
        assert!(result.is_err());

        if let Err(PersistentARTrieError::InvalidBlockId { block_id, reason }) = result {
            assert_eq!(block_id, 0);
            assert!(reason.contains("header"));
        } else {
            panic!("Expected InvalidBlockId error");
        }
    }

    #[test]
    fn test_invalid_block_id() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_invalid.part");

        let dm = IoUringDiskManager::create(&path).expect("create");
        let mut buf = AlignedBlock::new_boxed();
        let result = dm.read_block(999, &mut buf.data);
        assert!(result.is_err());
    }

    #[test]
    fn test_batch_read_write() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_batch.part");

        let dm = IoUringDiskManager::create(&path).expect("create");

        // Allocate blocks
        let b1 = dm.allocate_block().expect("alloc 1");
        let b2 = dm.allocate_block().expect("alloc 2");
        let b3 = dm.allocate_block().expect("alloc 3");

        // Write batch (heap-allocated to avoid stack overflow with 256KB blocks)
        let mut buf1 = AlignedBlock::new_boxed();
        let mut buf2 = AlignedBlock::new_boxed();
        let mut buf3 = AlignedBlock::new_boxed();
        buf1.data[0] = 1;
        buf2.data[0] = 2;
        buf3.data[0] = 3;

        dm.write_blocks_batch(&[
            (b1, &buf1.data),
            (b2, &buf2.data),
            (b3, &buf3.data),
        ])
        .expect("batch write");

        // Read batch
        let mut r1 = AlignedBlock::new_boxed();
        let mut r2 = AlignedBlock::new_boxed();
        let mut r3 = AlignedBlock::new_boxed();

        dm.read_blocks_batch(&mut [
            (b1, &mut r1.data),
            (b2, &mut r2.data),
            (b3, &mut r3.data),
        ])
        .expect("batch read");

        assert_eq!(r1.data[0], 1);
        assert_eq!(r2.data[0], 2);
        assert_eq!(r3.data[0], 3);
    }

    #[test]
    fn test_concurrent_allocation() {
        use std::sync::Arc;
        use std::thread;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_concurrent.part");

        let dm = Arc::new(IoUringDiskManager::create(&path).expect("create"));

        const NUM_THREADS: usize = 4;
        const BLOCKS_PER_THREAD: usize = 25;

        let mut handles = Vec::with_capacity(NUM_THREADS);

        for thread_id in 0..NUM_THREADS {
            let dm = Arc::clone(&dm);
            handles.push(thread::spawn(move || {
                let mut ids = Vec::with_capacity(BLOCKS_PER_THREAD);
                for i in 0..BLOCKS_PER_THREAD {
                    let block_id = dm.allocate_block().unwrap_or_else(|e| {
                        panic!(
                            "Thread {} failed to allocate block {}: {:?}",
                            thread_id, i, e
                        )
                    });

                    // Write and verify (same thread, heap-allocated)
                    let mut buf = AlignedBlock::new_boxed();
                    buf.data[0..4].copy_from_slice(&block_id.to_le_bytes());
                    dm.write_block(block_id, &buf.data).unwrap_or_else(|e| {
                        panic!(
                            "Thread {} failed to write block {} (id={}): {:?}",
                            thread_id, i, block_id, e
                        )
                    });

                    let mut read_buf = AlignedBlock::new_boxed();
                    dm.read_block(block_id, &mut read_buf.data).unwrap_or_else(|e| {
                        panic!(
                            "Thread {} failed to read block {} (id={}): {:?}",
                            thread_id, i, block_id, e
                        )
                    });

                    assert_eq!(&read_buf.data[0..4], &block_id.to_le_bytes());
                    ids.push(block_id);
                }
                ids
            }));
        }

        let mut all_ids: Vec<u32> = handles
            .into_iter()
            .flat_map(|h| h.join().expect("thread panicked"))
            .collect();

        all_ids.sort();
        let original_len = all_ids.len();
        all_ids.dedup();

        assert_eq!(
            all_ids.len(),
            original_len,
            "Duplicate block IDs were allocated!"
        );
        assert_eq!(original_len, NUM_THREADS * BLOCKS_PER_THREAD);

        let block_count = dm.block_count().expect("block count");
        assert_eq!(block_count as usize, 1 + NUM_THREADS * BLOCKS_PER_THREAD);
    }

    #[test]
    fn test_open_without_validation() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_no_validate.part");

        IoUringDiskManager::create(&path).expect("create");

        let dm = IoUringDiskManager::open_without_validation(&path).expect("open without validation");
        let header = BlockStorage::read_header(&dm).expect("read header");
        assert_eq!(header.magic, super::super::disk_manager::MAGIC_NUMBER);
    }

    #[test]
    fn test_cache_consistency() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_cache.part");

        let dm = IoUringDiskManager::create(&path).expect("create");
        let block_id = dm.allocate_block().expect("alloc");

        // Write via full-block API (heap-allocated)
        let mut write_buf = AlignedBlock::new_boxed();
        write_buf.data[0..5].copy_from_slice(b"hello");
        dm.write_block(block_id, &write_buf.data)
            .expect("write block");

        // Read sub-block (should go through cache, loading the block first)
        let mut read_buf = [0u8; 5];
        dm.read_bytes(block_id, 0, &mut read_buf)
            .expect("read bytes");
        assert_eq!(&read_buf, b"hello");

        // Modify sub-block via cache
        dm.write_bytes(block_id, 0, b"world").expect("write bytes");

        // Full-block read should see the cached modification
        let mut full_read = AlignedBlock::new_boxed();
        dm.read_block(block_id, &mut full_read.data)
            .expect("read block");
        assert_eq!(&full_read.data[0..5], b"world");
    }

    #[test]
    fn test_free_list() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_free.part");

        let dm = IoUringDiskManager::create(&path).expect("create");

        let b1 = dm.allocate_block().expect("alloc 1");
        let b2 = dm.allocate_block().expect("alloc 2");
        let _b3 = dm.allocate_block().expect("alloc 3");

        assert_eq!(b1, 1);
        assert_eq!(b2, 2);

        // Free block 2
        dm.free_block(b2).expect("free block 2");

        // Next allocation should reuse block 2 from free list
        let b4 = dm.allocate_block().expect("alloc 4");
        assert_eq!(b4, 2);

        // Next allocation extends the file
        let b5 = dm.allocate_block().expect("alloc 5");
        assert_eq!(b5, 4);
    }

    #[test]
    fn test_debug_output() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_debug.part");

        let dm = IoUringDiskManager::create(&path).expect("create");
        let debug = format!("{:?}", dm);
        assert!(debug.contains("IoUringDiskManager"));
        assert!(debug.contains("test_debug.part"));
    }
}
