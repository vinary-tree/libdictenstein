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
//! The io_uring rings are protected by `parking_lot::Mutex`. A striped `RingPool`
//! distributes I/O across multiple rings to reduce mutex contention under
//! concurrent workloads. The pool size defaults to `min(available_parallelism(), 8)`
//! and is configurable via `create_with_ring_pool_size()`.
//!
//! Pre-registered buffer operations (ReadFixed/WriteFixed) always route to the
//! primary ring (index 0) since buffer registration is per-ring. Standard I/O
//! operations use striped ring selection for load distribution.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

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

/// Maximum number of rings in the pool.
///
/// Capped to avoid excessive kernel memory. Each ring uses ~32KB for SQ/CQ,
/// so 8 rings = ~256KB of kernel memory.
const MAX_RING_POOL_SIZE: usize = 8;

#[derive(Debug, Clone, Copy)]
enum CompletionExpectation {
    FullRead(usize),
    FullWrite(usize),
    NoPayload,
}

fn validate_completion_result(
    operation: &'static str,
    result: i32,
    expectation: CompletionExpectation,
) -> Result<()> {
    if result < 0 {
        return Err(PersistentARTrieError::IoUringError {
            operation: operation.to_string(),
            source: std::io::Error::from_raw_os_error(-result),
        });
    }

    match expectation {
        CompletionExpectation::FullRead(expected) if (result as usize) < expected => {
            Err(PersistentARTrieError::IoUringError {
                operation: format!("{operation} (short read)"),
                source: std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("short read: got {} bytes, expected {}", result, expected),
                ),
            })
        }
        CompletionExpectation::FullWrite(expected) if (result as usize) < expected => {
            Err(PersistentARTrieError::IoUringError {
                operation: format!("{operation} (short write)"),
                source: std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    format!("short write: wrote {} bytes, expected {}", result, expected),
                ),
            })
        }
        _ => Ok(()),
    }
}

fn validate_completion_count(
    operation: &'static str,
    expected: usize,
    completed: usize,
) -> Result<()> {
    if completed == expected {
        Ok(())
    } else {
        Err(PersistentARTrieError::IoUringError {
            operation: operation.to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("expected {} completions, got {}", expected, completed),
            ),
        })
    }
}

/// Get a deterministic index for the current thread.
///
/// Uses `std::thread::current().id()` hashed to a `usize` for ring pool striping.
/// This avoids external dependencies while providing good distribution across rings.
#[inline]
fn thread_index() -> usize {
    let id = std::thread::current().id();
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish() as usize
}

// =============================================================================
// Ring Pool: striped io_uring rings for concurrent I/O
// =============================================================================

/// A pool of io_uring rings for striped I/O submission.
///
/// Each ring is independent with its own SQ/CQ. The file descriptor is safely
/// shared across all rings — the kernel handles concurrent I/O to the same fd.
///
/// # Ring Selection
///
/// - **Standard ops** (Read, Write, Fsync): Use `select()` for load distribution
/// - **Fixed-buffer ops** (ReadFixed, WriteFixed): Use `primary()` since buffer
///   registration is per-ring
///
/// # Thread Safety
///
/// Each ring is individually protected by a `Mutex`. Operations acquire at most
/// one ring lock at a time, so no ring-to-ring ordering is needed.
struct RingPool {
    rings: Vec<Mutex<IoUring>>,
    ring_count: usize,
}

impl RingPool {
    /// Create a new ring pool.
    ///
    /// # Arguments
    /// * `count` - Number of rings (capped at `MAX_RING_POOL_SIZE`)
    /// * `entries_per_ring` - Number of SQEs per ring (must be power of 2)
    fn new(count: usize, entries_per_ring: u32) -> std::result::Result<Self, std::io::Error> {
        let count = count.max(1).min(MAX_RING_POOL_SIZE);
        let mut rings = Vec::with_capacity(count);
        for _ in 0..count {
            rings.push(Mutex::new(IoUring::new(entries_per_ring)?));
        }
        Ok(Self {
            rings,
            ring_count: count,
        })
    }

    /// Select a ring for the calling thread via striping.
    ///
    /// Returns a reference to the ring's mutex. The caller should lock it
    /// for the duration of a single SQE submission + CQE drain cycle.
    #[inline]
    fn select(&self) -> &Mutex<IoUring> {
        let idx = thread_index() % self.ring_count;
        &self.rings[idx]
    }

    /// Primary ring (index 0) for buffer registration and fixed-buffer I/O.
    ///
    /// Buffer registration via `IORING_REGISTER_BUFFERS` is per-ring, so all
    /// ReadFixed/WriteFixed operations must use the ring that holds the
    /// registered buffers. This keeps RLIMIT_MEMLOCK usage at 1× pool_size
    /// instead of N× pool_size.
    #[inline]
    fn primary(&self) -> &Mutex<IoUring> {
        &self.rings[0]
    }

    /// Get the number of rings in the pool.
    #[inline]
    fn ring_count(&self) -> usize {
        self.ring_count
    }
}

// =============================================================================
// AlignedBlock Pool: recycled heap-allocated aligned buffers
// =============================================================================

/// Thread-safe freelist of `Box<AlignedBlock>` for batch I/O operations.
///
/// Avoids heap allocation on every `read_blocks_batch` / `write_blocks_batch`
/// call by recycling aligned buffers. The pool is bounded at `capacity` —
/// excess blocks returned via `release_batch` are dropped.
///
/// Falls back to `AlignedBlock::new_boxed()` when the pool is exhausted,
/// so callers never fail due to pool depletion.
pub(crate) struct AlignedBlockPool {
    pool: Mutex<Vec<Box<AlignedBlock>>>,
    capacity: usize,
}

impl AlignedBlockPool {
    /// Create a new pool pre-populated with `capacity` aligned blocks.
    fn new(capacity: usize) -> Self {
        let pool: Vec<Box<AlignedBlock>> =
            (0..capacity).map(|_| AlignedBlock::new_boxed()).collect();
        Self {
            pool: Mutex::new(pool),
            capacity,
        }
    }

    /// Acquire a batch of aligned blocks from the pool.
    ///
    /// Returns exactly `count` blocks. Blocks are taken from the pool first;
    /// any shortfall is satisfied by fresh heap allocation.
    fn acquire_batch(&self, count: usize) -> Vec<Box<AlignedBlock>> {
        let mut pool = self.pool.lock();
        let from_pool = count.min(pool.len());
        let split_at = pool.len() - from_pool;
        let mut blocks: Vec<Box<AlignedBlock>> = pool.split_off(split_at);
        // Allocate any remaining from heap
        blocks.reserve(count.saturating_sub(from_pool));
        blocks.extend((0..count.saturating_sub(from_pool)).map(|_| AlignedBlock::new_boxed()));
        blocks
    }

    /// Return blocks to the pool for reuse.
    ///
    /// Excess blocks beyond capacity are dropped automatically.
    fn release_batch(&self, blocks: Vec<Box<AlignedBlock>>) {
        let mut pool = self.pool.lock();
        let space = self.capacity.saturating_sub(pool.len());
        pool.extend(blocks.into_iter().take(space));
        // Remaining blocks beyond capacity are dropped automatically
    }
}

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
/// # Per-thread Ring Pool
///
/// A striped `RingPool` distributes I/O across multiple io_uring rings to
/// reduce mutex contention under concurrent workloads. The default ring count
/// is `min(available_parallelism(), 8)`. Pre-registered buffer ops always
/// route to the primary ring (index 0) since registration is per-ring.
///
/// # AlignedBlock Pool
///
/// A thread-safe freelist of `Box<AlignedBlock>` recycles aligned buffers
/// instead of heap-allocating on each batch I/O call. The pool capacity
/// matches `DEFAULT_RING_ENTRIES` (256), so a full batch can be served
/// without heap allocation.
pub struct IoUringDiskManager {
    /// The underlying file (opened with O_DIRECT).
    file: File,
    /// Raw file descriptor for io_uring operations.
    fd: i32,
    /// Striped pool of io_uring rings for concurrent I/O submission.
    ///
    /// Standard ops use `ring_pool.select()` for load distribution.
    /// Fixed-buffer ops use `ring_pool.primary()` (registration is per-ring).
    ring_pool: RingPool,
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
    /// Whether a buffer pool is registered with io_uring for zero-copy I/O.
    ///
    /// When true, `read_block_fixed`/`write_block_fixed` use `ReadFixed`/`WriteFixed`
    /// opcodes that skip kernel buffer copies. Set by `register_buffer_pool()`,
    /// cleared by `unregister_buffer_pool()` or `Drop`.
    buffers_registered: AtomicBool,
    /// Pre-allocated pool of aligned blocks for batch I/O operations.
    ///
    /// Eliminates per-call heap allocation in `read_blocks_batch`,
    /// `write_blocks_batch`, and `flush_dirty_cache`.
    aligned_block_pool: AlignedBlockPool,
}

// SAFETY: IoUringDiskManager is Send + Sync because:
// - File is Send + Sync
// - All mutable state is behind atomic ops or Mutex
// - io_uring ring_pool is behind per-ring Mutexes
// - block_cache is behind Mutex
// - aligned_block_pool is behind Mutex
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
        let metadata = file
            .metadata()
            .map_err(|e| PersistentARTrieError::IoError {
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

        // Create io_uring ring pool (single ring for backward compat)
        let ring_pool = RingPool::new(1, DEFAULT_RING_ENTRIES).map_err(|e| {
            PersistentARTrieError::IoUringError {
                operation: "create io_uring ring pool".to_string(),
                source: e,
            }
        })?;

        let block_count = (file_size / BLOCK_SIZE as u64) as u32;

        Ok(Self {
            file,
            fd,
            ring_pool,
            file_size: AtomicU64::new(file_size),
            block_count: AtomicU32::new(block_count),
            path: path_str,
            block_cache: Mutex::new(HashMap::new()),
            buffers_registered: AtomicBool::new(false),
            aligned_block_pool: AlignedBlockPool::new(DEFAULT_RING_ENTRIES as usize),
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

        let ring_pool = RingPool::new(1, DEFAULT_RING_ENTRIES).map_err(|e| {
            PersistentARTrieError::IoUringError {
                operation: "create io_uring ring pool".to_string(),
                source: e,
            }
        })?;

        let block_count = (file_size / BLOCK_SIZE as u64) as u32;

        let manager = Self {
            file,
            fd,
            ring_pool,
            file_size: AtomicU64::new(file_size),
            block_count: AtomicU32::new(block_count),
            path: path_str,
            block_cache: Mutex::new(HashMap::new()),
            buffers_registered: AtomicBool::new(false),
            aligned_block_pool: AlignedBlockPool::new(DEFAULT_RING_ENTRIES as usize),
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

        let ring_pool = RingPool::new(1, DEFAULT_RING_ENTRIES).map_err(|e| {
            PersistentARTrieError::IoUringError {
                operation: "create io_uring ring pool".to_string(),
                source: e,
            }
        })?;

        let block_count = (file_size / BLOCK_SIZE as u64) as u32;

        Ok(Self {
            file,
            fd,
            ring_pool,
            file_size: AtomicU64::new(file_size),
            block_count: AtomicU32::new(block_count),
            path: path_str,
            block_cache: Mutex::new(HashMap::new()),
            buffers_registered: AtomicBool::new(false),
            aligned_block_pool: AlignedBlockPool::new(DEFAULT_RING_ENTRIES as usize),
        })
    }

    /// Create with custom ring size.
    ///
    /// # Arguments
    /// * `path` - Path to the data file
    /// * `ring_entries` - Number of io_uring SQEs (must be power of 2, min 1, max 32768)
    pub fn create_with_ring_size<P: AsRef<Path>>(path: P, ring_entries: u32) -> Result<Self> {
        let mut manager = Self::create(path)?;
        // Replace the ring pool with a custom-sized one (single ring)
        let ring_pool =
            RingPool::new(1, ring_entries).map_err(|e| PersistentARTrieError::IoUringError {
                operation: "create io_uring ring with custom size".to_string(),
                source: e,
            })?;
        manager.ring_pool = ring_pool;
        Ok(manager)
    }

    /// Create an io_uring disk manager with a configurable ring pool size.
    ///
    /// Multiple io_uring rings are striped across threads to reduce mutex contention
    /// during concurrent I/O. Pre-registered buffer operations always route to the
    /// primary ring (index 0).
    ///
    /// # Arguments
    /// * `path` - Path to the data file
    /// * `ring_count` - Number of io_uring rings in the pool (1 = single ring, default behavior)
    ///
    /// # Examples
    /// ```no_run
    /// use libdictenstein::persistent_artrie::IoUringDiskManager;
    /// let dm = IoUringDiskManager::create_with_ring_pool_size("/tmp/test.part", 4).unwrap();
    /// ```
    pub fn create_with_ring_pool_size<P: AsRef<Path>>(path: P, ring_count: usize) -> Result<Self> {
        let mut manager = Self::create(path)?;
        let ring_pool = RingPool::new(ring_count, DEFAULT_RING_ENTRIES).map_err(|e| {
            PersistentARTrieError::IoUringError {
                operation: format!("create io_uring ring pool ({} rings)", ring_count),
                source: e,
            }
        })?;
        manager.ring_pool = ring_pool;
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
    /// Uses `ring_pool.select()` for striped load distribution.
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

        let mut ring = self.ring_pool.select().lock();

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

        let cqe = ring
            .completion()
            .next()
            .ok_or_else(|| PersistentARTrieError::IoUringError {
                operation: "read completion".to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "no completion entry after submit_and_wait",
                ),
            })?;

        validate_completion_result(
            "read I/O",
            cqe.result(),
            CompletionExpectation::FullRead(len),
        )
    }

    /// Submit a single write SQE to io_uring and wait for the CQE.
    ///
    /// Uses `ring_pool.select()` for striped load distribution.
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

        let mut ring = self.ring_pool.select().lock();

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

        let cqe = ring
            .completion()
            .next()
            .ok_or_else(|| PersistentARTrieError::IoUringError {
                operation: "write completion".to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "no completion entry after submit_and_wait",
                ),
            })?;

        validate_completion_result(
            "write I/O",
            cqe.result(),
            CompletionExpectation::FullWrite(len),
        )
    }

    /// Submit an fsync SQE via io_uring and wait for completion.
    ///
    /// Uses `ring_pool.select()` — any ring can fsync the same fd.
    fn submit_fsync(&self) -> Result<()> {
        let fsync_e = opcode::Fsync::new(types::Fd(self.fd))
            .build()
            .user_data(0x03);

        let mut ring = self.ring_pool.select().lock();

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

        let cqe = ring
            .completion()
            .next()
            .ok_or_else(|| PersistentARTrieError::IoUringError {
                operation: "fsync completion".to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "no completion entry after submit_and_wait",
                ),
            })?;

        validate_completion_result("fsync", cqe.result(), CompletionExpectation::NoPayload)
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

    /// Flush all dirty blocks from cache to disk via batched SQE submission.
    ///
    /// Collects all dirty cache blocks, submits them as a single batch of
    /// io_uring Write SQEs, and waits for all completions. This amortizes
    /// syscall overhead across all dirty blocks (1 mutex acquisition +
    /// 1 `submit_and_wait(N)` instead of N of each).
    ///
    /// If there are more dirty blocks than the ring's SQ capacity
    /// (`DEFAULT_RING_ENTRIES`), submissions are chunked to avoid SQ overflow.
    ///
    /// Called by `sync()` before issuing fsync.
    fn flush_dirty_cache(&self) -> Result<()> {
        // Count dirty entries first to acquire the right batch size from the pool
        let dirty_count = {
            let cache = self.block_cache.lock();
            cache.values().filter(|c| c.dirty).count()
        };

        if dirty_count == 0 {
            return Ok(());
        }

        // Acquire aligned blocks from pool for copying dirty data
        let mut pool_blocks = self.aligned_block_pool.acquire_batch(dirty_count);

        // Collect dirty blocks and their data (copy out to avoid holding lock during I/O)
        let dirty_entries: Vec<(u32, usize)> = {
            let mut cache = self.block_cache.lock();
            cache
                .iter_mut()
                .filter(|(_, cached)| cached.dirty)
                .enumerate()
                .map(|(i, (&block_id, cached))| {
                    cached.dirty = false;
                    pool_blocks[i].data.copy_from_slice(&cached.data.data);
                    (block_id, i)
                })
                .collect()
        };

        // Submit all dirty blocks as batched Write SQEs, chunked to avoid SQ overflow
        let result = (|| -> Result<()> {
            for chunk in dirty_entries.chunks(DEFAULT_RING_ENTRIES as usize) {
                let mut ring = self.ring_pool.select().lock();

                for (i, &(block_id, buf_idx)) in chunk.iter().enumerate() {
                    let offset = block_id as u64 * BLOCK_SIZE as u64;
                    let write_e = opcode::Write::new(
                        types::Fd(self.fd),
                        pool_blocks[buf_idx].data.as_ptr(),
                        BLOCK_SIZE as u32,
                    )
                    .offset(offset)
                    .build()
                    .user_data(i as u64);

                    unsafe {
                        ring.submission().push(&write_e).map_err(|_| {
                            PersistentARTrieError::IoUringError {
                                operation: "push dirty cache flush SQE".to_string(),
                                source: std::io::Error::new(
                                    std::io::ErrorKind::Other,
                                    "io_uring submission queue full",
                                ),
                            }
                        })?;
                    }
                }

                let count = chunk.len();
                ring.submit_and_wait(count)
                    .map_err(|e| PersistentARTrieError::IoUringError {
                        operation: "submit_and_wait dirty cache flush".to_string(),
                        source: e,
                    })?;

                // Drain all CQEs and validate results
                let mut completed = 0;
                for cqe in ring.completion() {
                    validate_completion_result(
                        "dirty cache flush I/O",
                        cqe.result(),
                        CompletionExpectation::FullWrite(BLOCK_SIZE),
                    )?;
                    completed += 1;
                }

                validate_completion_count("dirty cache flush completion count", count, completed)?;
            }

            Ok(())
        })();

        if result.is_err() {
            let mut cache = self.block_cache.lock();
            for (block_id, _) in &dirty_entries {
                if let Some(cached) = cache.get_mut(block_id) {
                    cached.dirty = true;
                }
            }
        }

        // Return pool blocks regardless of success/failure
        self.aligned_block_pool.release_batch(pool_blocks);

        result
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

    /// Get the number of io_uring rings in the pool.
    pub fn ring_count(&self) -> usize {
        self.ring_pool.ring_count()
    }

    /// Get the number of cached blocks.
    pub fn cached_block_count(&self) -> usize {
        self.block_cache.lock().len()
    }

    /// Get the number of dirty cached blocks.
    pub fn dirty_block_count(&self) -> usize {
        self.block_cache.lock().values().filter(|c| c.dirty).count()
    }

    /// Clear the block cache (all dirty blocks are lost!).
    ///
    /// Call `sync()` first to flush dirty blocks if needed.
    pub fn clear_cache(&self) {
        self.block_cache.lock().clear();
    }

    /// Get the file path.
    pub fn file_path(&self) -> &str {
        &self.path
    }

    // =========================================================================
    // Pre-registered buffer I/O primitives (ReadFixed / WriteFixed)
    // =========================================================================

    /// Submit a single ReadFixed SQE to io_uring and wait for the CQE.
    ///
    /// Uses a pre-registered buffer index to avoid kernel-side buffer copies.
    /// The buffer must have been previously registered via `register_buffer_pool()`.
    ///
    /// Always uses `ring_pool.primary()` since buffer registration is per-ring.
    ///
    /// # Safety contract
    /// - `buf` must point to at least `len` bytes of writable memory
    /// - `buf` must be part of the registered buffer pool at `buf_index`
    /// - `buf` must be 4096-byte aligned (O_DIRECT requirement)
    /// - `buf` must remain valid until this function returns
    fn submit_read_fixed(
        &self,
        buf: *mut u8,
        len: usize,
        offset: u64,
        buf_index: u16,
    ) -> Result<()> {
        let read_e = opcode::ReadFixed::new(types::Fd(self.fd), buf, len as u32, buf_index)
            .offset(offset)
            .build()
            .user_data(0x11);

        let mut ring = self.ring_pool.primary().lock();

        unsafe {
            ring.submission()
                .push(&read_e)
                .map_err(|_| PersistentARTrieError::IoUringError {
                    operation: "push ReadFixed SQE".to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "io_uring submission queue full",
                    ),
                })?;
        }

        ring.submit_and_wait(1)
            .map_err(|e| PersistentARTrieError::IoUringError {
                operation: "submit_and_wait ReadFixed".to_string(),
                source: e,
            })?;

        let cqe = ring
            .completion()
            .next()
            .ok_or_else(|| PersistentARTrieError::IoUringError {
                operation: "ReadFixed completion".to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "no completion entry after submit_and_wait",
                ),
            })?;

        validate_completion_result(
            "ReadFixed I/O",
            cqe.result(),
            CompletionExpectation::FullRead(len),
        )
    }

    /// Submit a single WriteFixed SQE to io_uring and wait for the CQE.
    ///
    /// Uses a pre-registered buffer index to avoid kernel-side buffer copies.
    /// The buffer must have been previously registered via `register_buffer_pool()`.
    ///
    /// Always uses `ring_pool.primary()` since buffer registration is per-ring.
    ///
    /// # Safety contract
    /// - `buf` must point to at least `len` bytes of readable memory
    /// - `buf` must be part of the registered buffer pool at `buf_index`
    /// - `buf` must be 4096-byte aligned (O_DIRECT requirement)
    /// - `buf` must remain valid until this function returns
    fn submit_write_fixed(
        &self,
        buf: *const u8,
        len: usize,
        offset: u64,
        buf_index: u16,
    ) -> Result<()> {
        let write_e = opcode::WriteFixed::new(types::Fd(self.fd), buf, len as u32, buf_index)
            .offset(offset)
            .build()
            .user_data(0x12);

        let mut ring = self.ring_pool.primary().lock();

        unsafe {
            ring.submission()
                .push(&write_e)
                .map_err(|_| PersistentARTrieError::IoUringError {
                    operation: "push WriteFixed SQE".to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "io_uring submission queue full",
                    ),
                })?;
        }

        ring.submit_and_wait(1)
            .map_err(|e| PersistentARTrieError::IoUringError {
                operation: "submit_and_wait WriteFixed".to_string(),
                source: e,
            })?;

        let cqe = ring
            .completion()
            .next()
            .ok_or_else(|| PersistentARTrieError::IoUringError {
                operation: "WriteFixed completion".to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "no completion entry after submit_and_wait",
                ),
            })?;

        validate_completion_result(
            "WriteFixed I/O",
            cqe.result(),
            CompletionExpectation::FullWrite(len),
        )
    }
}

// =============================================================================
// Drop implementation: unregister buffers before ring destruction
// =============================================================================

impl Drop for IoUringDiskManager {
    fn drop(&mut self) {
        // Unregister buffers from the primary ring if registered (explicit cleanup
        // for orderly shutdown, even though the kernel auto-cleans on ring destruction)
        if self.buffers_registered.load(Ordering::Acquire) {
            let _ = self.unregister_buffer_pool();
        }
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
        let updated_cached_block = {
            let mut cache = self.block_cache.lock();
            if let Some(cached) = cache.get_mut(&block_id) {
                cached.data.data.copy_from_slice(buffer);
                // Mark NOT dirty since we're writing to disk below
                cached.dirty = false;
                true
            } else {
                false
            }
        };

        let result = self.write_block_uring(block_id, buffer);
        if result.is_err() && updated_cached_block {
            let mut cache = self.block_cache.lock();
            if let Some(cached) = cache.get_mut(&block_id) {
                cached.dirty = true;
            }
        }

        result
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
                        self.file.set_len(new_file_size).map_err(|e| {
                            PersistentARTrieError::IoError {
                                operation: "extend file".to_string(),
                                path: self.path.clone(),
                                source: e,
                            }
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

        // Acquire aligned buffers from pool for I/O requests
        let mut aligned_buffers = self.aligned_block_pool.acquire_batch(needs_io.len());

        // Submit all read SQEs in one batch
        let result = (|| -> Result<()> {
            let mut ring = self.ring_pool.select().lock();

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
                validate_completion_result(
                    "batch read I/O",
                    cqe.result(),
                    CompletionExpectation::FullRead(BLOCK_SIZE),
                )?;
                completed += 1;
            }

            validate_completion_count("batch read completion count", count, completed)?;
            Ok(())
        })();

        if result.is_ok() {
            // Copy results from aligned buffers to caller's buffers
            for (buf_idx, &req_idx) in needs_io.iter().enumerate() {
                requests[req_idx]
                    .1
                    .copy_from_slice(&aligned_buffers[buf_idx].data);
            }
        }

        // Return aligned buffers to pool for reuse
        self.aligned_block_pool.release_batch(aligned_buffers);

        result
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
        let mut cached_block_ids = Vec::new();
        {
            let mut cache = self.block_cache.lock();
            for &(block_id, buffer) in requests {
                if let Some(cached) = cache.get_mut(&block_id) {
                    cached.data.data.copy_from_slice(buffer);
                    cached.dirty = false; // Will be written to disk below
                    cached_block_ids.push(block_id);
                }
            }
        }

        // Acquire aligned buffers from pool and copy source data
        let mut aligned_buffers = self.aligned_block_pool.acquire_batch(requests.len());
        for (i, &(_, buffer)) in requests.iter().enumerate() {
            aligned_buffers[i].data.copy_from_slice(buffer);
        }

        // Submit all write SQEs in one batch
        let result = (|| -> Result<()> {
            let mut ring = self.ring_pool.select().lock();

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
                validate_completion_result(
                    "batch write I/O",
                    cqe.result(),
                    CompletionExpectation::FullWrite(BLOCK_SIZE),
                )?;
                completed += 1;
            }

            validate_completion_count("batch write completion count", count, completed)?;
            Ok(())
        })();

        if result.is_err() {
            let mut cache = self.block_cache.lock();
            for block_id in &cached_block_ids {
                if let Some(cached) = cache.get_mut(block_id) {
                    cached.dirty = true;
                }
            }
        }

        // Return aligned buffers to pool for reuse
        self.aligned_block_pool.release_batch(aligned_buffers);

        result
    }

    // =========================================================================
    // Pre-registered buffer support
    // =========================================================================

    unsafe fn register_buffer_pool(&self, buffers: &[(*mut u8, usize)]) -> Result<()> {
        if buffers.is_empty() {
            return Ok(());
        }

        for (index, &(ptr, len)) in buffers.iter().enumerate() {
            if ptr.is_null() {
                return Err(PersistentARTrieError::internal(format!(
                    "register_buffer_pool rejected null buffer at index {index}"
                )));
            }
            if len != BLOCK_SIZE {
                return Err(PersistentARTrieError::internal(format!(
                    "register_buffer_pool rejected buffer at index {index}: length {len} != \
                     block size {BLOCK_SIZE}"
                )));
            }
            if (ptr as usize) % std::mem::align_of::<AlignedBlock>() != 0 {
                return Err(PersistentARTrieError::internal(format!(
                    "register_buffer_pool rejected unaligned buffer at index {index}"
                )));
            }
        }

        // Convert to libc::iovec for the kernel registration call
        let iovecs: Vec<libc::iovec> = buffers
            .iter()
            .map(|&(ptr, len)| libc::iovec {
                iov_base: ptr as *mut libc::c_void,
                iov_len: len,
            })
            .collect();

        // Register on primary ring only (RLIMIT_MEMLOCK is 1× pool_size, not N×)
        let ring = self.ring_pool.primary().lock();
        ring.submitter().register_buffers(&iovecs).map_err(|e| {
            PersistentARTrieError::IoUringError {
                operation: format!(
                    "register_buffers ({} buffers, {} bytes each)",
                    iovecs.len(),
                    buffers.first().map_or(0, |b| b.1)
                ),
                source: e,
            }
        })?;

        self.buffers_registered.store(true, Ordering::Release);

        Ok(())
    }

    fn unregister_buffer_pool(&self) -> Result<()> {
        if !self.buffers_registered.load(Ordering::Acquire) {
            return Ok(());
        }

        // Unregister from primary ring (the only ring with registered buffers)
        let ring = self.ring_pool.primary().lock();
        ring.submitter()
            .unregister_buffers()
            .map_err(|e| PersistentARTrieError::IoUringError {
                operation: "unregister_buffers".to_string(),
                source: e,
            })?;

        self.buffers_registered.store(false, Ordering::Release);

        Ok(())
    }

    fn read_block_fixed(
        &self,
        block_id: u32,
        buffer: &mut [u8; BLOCK_SIZE],
        buf_index: u16,
    ) -> Result<()> {
        // Graceful degradation: if buffers aren't registered, fall back to
        // regular read_block (which uses opcode::Read instead of ReadFixed)
        if !self.buffers_registered.load(Ordering::Acquire) {
            return self.read_block(block_id, buffer);
        }

        self.validate_block_id(block_id)?;

        // Check cache first (sub-block writes may have modified this block)
        {
            let cache = self.block_cache.lock();
            if let Some(cached) = cache.get(&block_id) {
                buffer.copy_from_slice(&cached.data.data);
                return Ok(());
            }
        }

        let offset = block_id as u64 * BLOCK_SIZE as u64;
        self.submit_read_fixed(buffer.as_mut_ptr(), BLOCK_SIZE, offset, buf_index)
    }

    fn write_block_fixed(
        &self,
        block_id: u32,
        buffer: &[u8; BLOCK_SIZE],
        buf_index: u16,
    ) -> Result<()> {
        // Graceful degradation: if buffers aren't registered, fall back to
        // regular write_block (which uses opcode::Write instead of WriteFixed)
        if !self.buffers_registered.load(Ordering::Acquire) {
            return self.write_block(block_id, buffer);
        }

        self.validate_block_id(block_id)?;

        // Update cache if block is cached (keeps cache consistent)
        let updated_cached_block = {
            let mut cache = self.block_cache.lock();
            if let Some(cached) = cache.get_mut(&block_id) {
                cached.data.data.copy_from_slice(buffer);
                // Mark NOT dirty since we're writing to disk below
                cached.dirty = false;
                true
            } else {
                false
            }
        };

        let offset = block_id as u64 * BLOCK_SIZE as u64;
        let result = self.submit_write_fixed(buffer.as_ptr(), BLOCK_SIZE, offset, buf_index);
        if result.is_err() && updated_cached_block {
            let mut cache = self.block_cache.lock();
            if let Some(cached) = cache.get_mut(&block_id) {
                cached.dirty = true;
            }
        }

        result
    }

    fn supports_fixed_buffers(&self) -> bool {
        self.buffers_registered.load(Ordering::Acquire)
    }

    fn read_blocks_batch_fixed(
        &self,
        requests: &mut [(u32, &mut [u8; BLOCK_SIZE], u16)],
    ) -> Result<()> {
        if requests.is_empty() {
            return Ok(());
        }

        // Graceful degradation: fall back to non-fixed batch read
        if !self.buffers_registered.load(Ordering::Acquire) {
            let mut non_fixed: Vec<(u32, &mut [u8; BLOCK_SIZE])> = requests
                .iter_mut()
                .map(|(block_id, buffer, _)| (*block_id, &mut **buffer))
                .collect();
            return self.read_blocks_batch(&mut non_fixed);
        }

        // Validate all block IDs
        for &(block_id, _, _) in requests.iter() {
            self.validate_block_id(block_id)?;
        }

        // Determine which blocks need disk I/O vs cache hits
        let mut needs_io: Vec<usize> = Vec::with_capacity(requests.len());
        {
            let cache = self.block_cache.lock();
            for (i, (block_id, buffer, _)) in requests.iter_mut().enumerate() {
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

        // Submit all ReadFixed SQEs in chunks (SQ has DEFAULT_RING_ENTRIES capacity)
        // Always use primary ring for fixed-buffer ops (registration is per-ring)
        for chunk in needs_io.chunks(DEFAULT_RING_ENTRIES as usize) {
            let mut ring = self.ring_pool.primary().lock();

            for &req_idx in chunk {
                let (block_id, ref buffer, buf_index) = requests[req_idx];
                let offset = block_id as u64 * BLOCK_SIZE as u64;

                let read_e = opcode::ReadFixed::new(
                    types::Fd(self.fd),
                    buffer.as_ptr() as *mut u8,
                    BLOCK_SIZE as u32,
                    buf_index,
                )
                .offset(offset)
                .build()
                .user_data(req_idx as u64);

                unsafe {
                    ring.submission().push(&read_e).map_err(|_| {
                        PersistentARTrieError::IoUringError {
                            operation: "push batch ReadFixed SQE".to_string(),
                            source: std::io::Error::new(
                                std::io::ErrorKind::Other,
                                "io_uring submission queue full",
                            ),
                        }
                    })?;
                }
            }

            let count = chunk.len();
            ring.submit_and_wait(count)
                .map_err(|e| PersistentARTrieError::IoUringError {
                    operation: "submit_and_wait batch ReadFixed".to_string(),
                    source: e,
                })?;

            let mut completed = 0;
            for cqe in ring.completion() {
                validate_completion_result(
                    "batch ReadFixed I/O",
                    cqe.result(),
                    CompletionExpectation::FullRead(BLOCK_SIZE),
                )?;
                completed += 1;
            }

            validate_completion_count("batch ReadFixed completion count", count, completed)?;
        }

        Ok(())
    }

    fn write_blocks_batch_fixed(&self, requests: &[(u32, &[u8; BLOCK_SIZE], u16)]) -> Result<()> {
        if requests.is_empty() {
            return Ok(());
        }

        // Graceful degradation: fall back to non-fixed batch write
        if !self.buffers_registered.load(Ordering::Acquire) {
            let non_fixed: Vec<(u32, &[u8; BLOCK_SIZE])> = requests
                .iter()
                .map(|&(block_id, buffer, _)| (block_id, buffer))
                .collect();
            return self.write_blocks_batch(&non_fixed);
        }

        // Validate all block IDs
        for &(block_id, _, _) in requests {
            self.validate_block_id(block_id)?;
        }

        // Update cache for any cached blocks
        let mut cached_block_ids = Vec::new();
        {
            let mut cache = self.block_cache.lock();
            for &(block_id, buffer, _) in requests {
                if let Some(cached) = cache.get_mut(&block_id) {
                    cached.data.data.copy_from_slice(buffer);
                    cached.dirty = false; // Will be written to disk below
                    cached_block_ids.push(block_id);
                }
            }
        }

        // Submit all WriteFixed SQEs in chunks (SQ has DEFAULT_RING_ENTRIES capacity)
        // Always use primary ring for fixed-buffer ops (registration is per-ring)
        let result = (|| -> Result<()> {
            for (chunk_idx, chunk) in requests.chunks(DEFAULT_RING_ENTRIES as usize).enumerate() {
                let mut ring = self.ring_pool.primary().lock();
                let base = chunk_idx * DEFAULT_RING_ENTRIES as usize;

                for (i, &(block_id, buffer, buf_index)) in chunk.iter().enumerate() {
                    let offset = block_id as u64 * BLOCK_SIZE as u64;

                    let write_e = opcode::WriteFixed::new(
                        types::Fd(self.fd),
                        buffer.as_ptr(),
                        BLOCK_SIZE as u32,
                        buf_index,
                    )
                    .offset(offset)
                    .build()
                    .user_data((base + i) as u64);

                    unsafe {
                        ring.submission().push(&write_e).map_err(|_| {
                            PersistentARTrieError::IoUringError {
                                operation: "push batch WriteFixed SQE".to_string(),
                                source: std::io::Error::new(
                                    std::io::ErrorKind::Other,
                                    "io_uring submission queue full",
                                ),
                            }
                        })?;
                    }
                }

                let count = chunk.len();
                ring.submit_and_wait(count)
                    .map_err(|e| PersistentARTrieError::IoUringError {
                        operation: "submit_and_wait batch WriteFixed".to_string(),
                        source: e,
                    })?;

                let mut completed = 0;
                for cqe in ring.completion() {
                    validate_completion_result(
                        "batch WriteFixed I/O",
                        cqe.result(),
                        CompletionExpectation::FullWrite(BLOCK_SIZE),
                    )?;
                    completed += 1;
                }

                validate_completion_count("batch WriteFixed completion count", count, completed)?;
            }
            Ok(())
        })();

        if result.is_err() {
            let mut cache = self.block_cache.lock();
            for block_id in &cached_block_ids {
                if let Some(cached) = cache.get_mut(block_id) {
                    cached.dirty = true;
                }
            }
        }

        result
    }
}

impl std::fmt::Debug for IoUringDiskManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IoUringDiskManager")
            .field("path", &self.path)
            .field("fd", &self.fd)
            .field("file_size", &self.file_size.load(Ordering::Relaxed))
            .field("block_count", &self.block_count.load(Ordering::Relaxed))
            .field("ring_count", &self.ring_pool.ring_count())
            .field("cached_blocks", &self.block_cache.lock().len())
            .field(
                "buffers_registered",
                &self.buffers_registered.load(Ordering::Relaxed),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering as AtomicOrdering;
    use tempfile::tempdir;

    #[test]
    fn io_uring_completion_result_contracts_fail_closed() {
        assert!(validate_completion_result(
            "read I/O",
            BLOCK_SIZE as i32,
            CompletionExpectation::FullRead(BLOCK_SIZE)
        )
        .is_ok());

        let short_read = validate_completion_result(
            "read I/O",
            (BLOCK_SIZE - 1) as i32,
            CompletionExpectation::FullRead(BLOCK_SIZE),
        )
        .expect_err("short read must fail closed");
        assert!(matches!(
            short_read,
            PersistentARTrieError::IoUringError { operation, source }
                if operation == "read I/O (short read)"
                    && source.kind() == std::io::ErrorKind::UnexpectedEof
        ));

        let short_write = validate_completion_result(
            "write I/O",
            (BLOCK_SIZE - 1) as i32,
            CompletionExpectation::FullWrite(BLOCK_SIZE),
        )
        .expect_err("short write must fail closed");
        assert!(matches!(
            short_write,
            PersistentARTrieError::IoUringError { operation, source }
                if operation == "write I/O (short write)"
                    && source.kind() == std::io::ErrorKind::WriteZero
        ));

        let negative =
            validate_completion_result("fsync", -libc::EIO, CompletionExpectation::NoPayload)
                .expect_err("negative CQE result must fail closed");
        assert!(matches!(
            negative,
            PersistentARTrieError::IoUringError { operation, source }
                if operation == "fsync"
                    && source.raw_os_error() == Some(libc::EIO)
        ));
    }

    #[test]
    fn io_uring_completion_count_contracts_fail_closed() {
        assert!(validate_completion_count("batch read completion count", 2, 2).is_ok());

        let missing = validate_completion_count("batch read completion count", 2, 1)
            .expect_err("missing CQE must fail closed");
        assert!(matches!(
            missing,
            PersistentARTrieError::IoUringError { operation, source }
                if operation == "batch read completion count"
                    && source.kind() == std::io::ErrorKind::Other
                    && source.to_string().contains("expected 2 completions, got 1")
        ));
    }

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

        dm.set_root_ptr(0x123456789ABCDEF0).expect("set_root_ptr");
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

        dm.write_blocks_batch(&[(b1, &buf1.data), (b2, &buf2.data), (b3, &buf3.data)])
            .expect("batch write");

        // Read batch
        let mut r1 = AlignedBlock::new_boxed();
        let mut r2 = AlignedBlock::new_boxed();
        let mut r3 = AlignedBlock::new_boxed();

        dm.read_blocks_batch(&mut [(b1, &mut r1.data), (b2, &mut r2.data), (b3, &mut r3.data)])
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
                    dm.read_block(block_id, &mut read_buf.data)
                        .unwrap_or_else(|e| {
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

        let dm =
            IoUringDiskManager::open_without_validation(&path).expect("open without validation");
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
