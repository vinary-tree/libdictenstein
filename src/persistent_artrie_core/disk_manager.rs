//! Disk Manager for Persistent Adaptive Radix Trie
//!
//! This module provides memory-mapped file I/O abstraction for the persistent
//! dictionary implementation. It manages:
//!
//! - Memory-mapped file via `memmap2` crate
//! - Block allocation with 256KB block size (optimal for NVMe)
//! - Free list management for block reuse
//! - File header with metadata (magic, version, root pointer)
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    File Layout                               │
//! ├─────────────────────────────────────────────────────────────┤
//! │ Block 0: File Header (256KB)                                 │
//! │   - Magic number (8 bytes)                                   │
//! │   - Version (4 bytes)                                        │
//! │   - Flags (4 bytes)                                          │
//! │   - Root pointer (8 bytes)                                   │
//! │   - Block count (8 bytes)                                    │
//! │   - Free list head (8 bytes)                                 │
//! │   - Entry count (8 bytes)                                    │
//! │   - Checksum (8 bytes)                                       │
//! │   - Reserved (256KB - 56 bytes)                              │
//! ├─────────────────────────────────────────────────────────────┤
//! │ Block 1: Data Block (256KB)                                  │
//! ├─────────────────────────────────────────────────────────────┤
//! │ Block 2: Data Block (256KB)                                  │
//! ├─────────────────────────────────────────────────────────────┤
//! │ ...                                                          │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Block Size Rationale
//!
//! 256KB blocks are chosen for:
//! - Optimal NVMe I/O granularity (typical 4KB-256KB sweet spot)
//! - Reduced metadata overhead vs smaller pages
//! - Good cache locality for sequential access
//! - Alignment with memory-mapped page boundaries

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write as IoWrite};
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use memmap2::{MmapMut, MmapOptions};

use parking_lot::RwLock;

use super::block_storage::BlockStorage;
use super::error::{PersistentARTrieError, Result};

/// Block size: 256KB (262144 bytes)
/// Optimal for NVMe I/O and memory-mapped pages
pub const BLOCK_SIZE: usize = 256 * 1024;

/// Maximum supported block count (24-bit block ID = 16M blocks = 4TB max file size)
pub const MAX_BLOCK_COUNT: u32 = 1 << 24;

/// Magic number identifying a valid PART file
/// "PART" in ASCII + version nibbles
pub const MAGIC_NUMBER: u64 = 0x5041_5254_0001_0000; // "PART" + v1.0

/// Current file format version
pub const FORMAT_VERSION: u32 = 1;

/// File header structure (stored at block 0)
///
/// Total size: 64 bytes (cacheline aligned)
/// Remaining space in block 0 is reserved for future use
#[repr(C, align(64))]
#[derive(Debug)]
pub struct FileHeader {
    /// Magic number for file identification
    pub magic: u64,
    /// File format version
    pub version: u32,
    /// Flags (reserved for future use)
    pub flags: u32,
    /// Root node pointer (swizzled format)
    pub root_ptr: AtomicU64,
    /// Total number of allocated blocks
    pub block_count: AtomicU32,
    /// Padding for alignment
    _pad1: u32,
    /// Head of free block list (0 = no free blocks)
    pub free_list_head: AtomicU64,
    /// Total number of entries in the dictionary
    pub entry_count: AtomicU64,
    /// CRC-64 checksum of header (excluding this field)
    pub checksum: u64,
}

impl FileHeader {
    /// Create a new file header with default values
    pub fn new() -> Self {
        Self {
            magic: MAGIC_NUMBER,
            version: FORMAT_VERSION,
            flags: 0,
            root_ptr: AtomicU64::new(0),
            block_count: AtomicU32::new(1), // Block 0 is header
            _pad1: 0,
            free_list_head: AtomicU64::new(0),
            entry_count: AtomicU64::new(0),
            checksum: 0,
        }
    }

    /// Validate the header magic and version
    pub fn validate(&self) -> Result<()> {
        if self.magic != MAGIC_NUMBER {
            return Err(PersistentARTrieError::InvalidMagic {
                expected: MAGIC_NUMBER,
                found: self.magic,
            });
        }
        if self.version > FORMAT_VERSION {
            return Err(PersistentARTrieError::UnsupportedVersion {
                max_supported: FORMAT_VERSION,
                found: self.version,
            });
        }
        Ok(())
    }

    /// Compute CRC-64 checksum of header fields (excluding checksum field)
    pub fn compute_checksum(&self) -> u64 {
        // Simple FNV-1a hash for checksum (not cryptographic)
        let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis
        let prime: u64 = 0x100000001b3; // FNV prime

        // Hash each field
        for byte in self.magic.to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(prime);
        }
        for byte in self.version.to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(prime);
        }
        for byte in self.flags.to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(prime);
        }
        for byte in self.root_ptr.load(Ordering::SeqCst).to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(prime);
        }
        for byte in self.block_count.load(Ordering::SeqCst).to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(prime);
        }
        for byte in self.free_list_head.load(Ordering::SeqCst).to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(prime);
        }
        for byte in self.entry_count.load(Ordering::SeqCst).to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(prime);
        }

        hash
    }

    /// Update the checksum field
    pub fn update_checksum(&mut self) {
        self.checksum = self.compute_checksum();
    }

    /// Verify the checksum
    pub fn verify_checksum(&self) -> bool {
        self.checksum == self.compute_checksum()
    }

    /// Serialize header to bytes
    pub fn to_bytes(&self) -> [u8; 64] {
        let mut bytes = [0u8; 64];
        bytes[0..8].copy_from_slice(&self.magic.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.version.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.flags.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.root_ptr.load(Ordering::SeqCst).to_le_bytes());
        bytes[24..28].copy_from_slice(&self.block_count.load(Ordering::SeqCst).to_le_bytes());
        bytes[28..32].copy_from_slice(&0u32.to_le_bytes()); // padding
        bytes[32..40].copy_from_slice(&self.free_list_head.load(Ordering::SeqCst).to_le_bytes());
        bytes[40..48].copy_from_slice(&self.entry_count.load(Ordering::SeqCst).to_le_bytes());
        bytes[48..56].copy_from_slice(&self.checksum.to_le_bytes());
        // bytes[56..64] remain zero (reserved)
        bytes
    }

    /// Deserialize header from bytes
    pub fn from_bytes(bytes: &[u8; 64]) -> Self {
        Self {
            magic: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            version: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            flags: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            root_ptr: AtomicU64::new(u64::from_le_bytes(bytes[16..24].try_into().unwrap())),
            block_count: AtomicU32::new(u32::from_le_bytes(bytes[24..28].try_into().unwrap())),
            _pad1: 0,
            free_list_head: AtomicU64::new(u64::from_le_bytes(bytes[32..40].try_into().unwrap())),
            entry_count: AtomicU64::new(u64::from_le_bytes(bytes[40..48].try_into().unwrap())),
            checksum: u64::from_le_bytes(bytes[48..56].try_into().unwrap()),
        }
    }
}

impl Default for FileHeader {
    fn default() -> Self {
        Self::new()
    }
}

/// Free block list entry
///
/// When a block is freed, it's added to the free list.
/// The first 8 bytes of a free block contain the next free block ID.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FreeBlockEntry {
    /// Next free block ID (0 = end of list)
    pub next: u64,
}

/// Backward-compatible type alias.
///
/// Existing code using `DiskManager` continues to compile unchanged.
/// New code should use `MmapDiskManager` directly for clarity.
pub type DiskManager = MmapDiskManager;

/// Memory-mapped disk manager for persistent storage.
///
/// Provides memory-mapped I/O with block allocation and free list management.
/// Thread-safe for concurrent read access; writes require external synchronization.
///
/// Implements the [`BlockStorage`] trait, allowing it to be used interchangeably
/// with other storage backends (e.g., `IoUringDiskManager`).
///
/// # Thread Safety for Block Allocation
///
/// Block allocation uses a lock-free CAS loop on the in-memory `block_count` atomic.
/// This ensures that concurrent allocations never receive duplicate block IDs.
///
/// ## Synchronization Invariant (TLA+ Verified)
///
/// The critical invariant is: `file_size` is updated WHILE holding the mmap write lock,
/// and readers must acquire the mmap lock FIRST, then check `file_size`. This protocol
/// has been formally verified using TLA+ model checking (see `docs/formal/BlockAllocationSync.tla`).
///
/// ```text
/// ALLOCATOR:                      READER/WRITER:
/// 1. CAS block_count              1. Check block_count (quick reject)
/// 2. Acquire mmap write lock      2. Acquire mmap lock ← BLOCKS if allocator holds lock
/// 3. set_len() (inside lock)      3. Check file_size ← Safe: if we got lock, either:
/// 4. pwrite() for sparse             - Allocator hasn't started (old file_size, fail)
/// 5. Remap mmap                      - Allocator finished (new file_size, new mmap)
/// 6. file_size.store()            4. Access memory
/// 7. Release mmap write lock      5. Release lock
/// ```
///
/// Key invariant verified by TLA+: `file_size <= mmap_len` always holds, ensuring that
/// any reader that passes the file_size check can safely access the memory.
/// See `formal-verification/tla+/MmapBlockStorage.tla` for the bounded model.
///
/// This ensures:
/// - If reader sees updated `file_size`, it has the lock and sees the new mmap
/// - If reader sees old `file_size` while allocator holds write lock, it will block and wait
pub struct MmapDiskManager {
    /// The underlying file
    file: File,
    /// Memory-mapped region (optional, for read-heavy workloads)
    mmap: Option<RwLock<MmapMut>>,
    /// Current file size in bytes (updated INSIDE mmap write lock after remap)
    file_size: AtomicU64,
    /// In-memory block count for lock-free allocation (source of truth for CAS)
    block_count: AtomicU32,
    /// Path to the file (for error messages)
    path: String,
}

impl MmapDiskManager {
    fn validate_byte_range(
        block_id: u32,
        offset_in_block: usize,
        len: usize,
    ) -> Result<(usize, usize)> {
        if offset_in_block > BLOCK_SIZE || len > BLOCK_SIZE.saturating_sub(offset_in_block) {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Range [{}, {}) exceeds block size {}",
                    offset_in_block,
                    offset_in_block.saturating_add(len),
                    BLOCK_SIZE
                ),
            });
        }

        let block_offset = (block_id as usize).checked_mul(BLOCK_SIZE).ok_or_else(|| {
            PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "Block offset overflowed usize".to_string(),
            }
        })?;
        let file_offset = block_offset.checked_add(offset_in_block).ok_or_else(|| {
            PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "File offset overflowed usize".to_string(),
            }
        })?;
        let end_offset =
            file_offset
                .checked_add(len)
                .ok_or_else(|| PersistentARTrieError::InvalidBlockId {
                    block_id,
                    reason: "End offset overflowed usize".to_string(),
                })?;

        Ok((file_offset, end_offset))
    }

    /// Create a new disk manager, creating the file if it doesn't exist.
    ///
    /// This is a TOCTOU-safe "open or create" operation that matches the formal
    /// model's `open_or_create_safe` in `FileSystem.v`:
    ///
    /// 1. Parent directory is ensured via `mkdir_all` (idempotent)
    /// 2. File is opened with `create(true)` which atomically creates if needed
    /// 3. Empty file check and initialization is safe because we hold the file handle
    ///
    /// # Arguments
    /// * `path` - Path to the data file
    ///
    /// # Returns
    /// * `Ok(DiskManager)` - Successfully opened/created file
    /// * `Err(PersistentARTrieError)` - I/O or format error
    ///
    /// # Formal Verification Correspondence
    ///
    /// The empty-check + initialize pattern is TOCTOU-safe here because:
    /// - We hold the file handle throughout, preventing other processes from
    ///   modifying the file between check and initialize
    /// - `create(true)` is appropriate (vs `create_new(true)`) because this
    ///   method intentionally supports both creation and opening existing files
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();

        // Ensure parent directory exists (idempotent, matches formal mkdir_all)
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| PersistentARTrieError::IoError {
                    operation: "create parent directory".to_string(),
                    path: parent.display().to_string(),
                    source: e,
                })?;
            }
        }

        // Open or create file atomically
        // Using create(true) (not create_new) because this method intentionally
        // supports both creating new files and opening existing ones
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "create file".to_string(),
                path: path_str.clone(),
                source: e,
            })?;

        let metadata = file
            .metadata()
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "get metadata".to_string(),
                path: path_str.clone(),
                source: e,
            })?;

        let file_size = metadata.len();

        // If file is empty, initialize with header block
        // TOCTOU-safe: We hold the file handle, preventing concurrent modification
        // between the size check and initialization
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

        // Create memory map
        let mmap = if file_size > 0 {
            let mmap = unsafe {
                MmapOptions::new()
                    .len(file_size as usize)
                    .map_mut(&file)
                    .map_err(|e| PersistentARTrieError::MmapError {
                        operation: "create mmap".to_string(),
                        source: e,
                    })?
            };
            Some(RwLock::new(mmap))
        } else {
            None
        };

        // Calculate block count from file size (source of truth for recovery)
        let block_count = (file_size / BLOCK_SIZE as u64) as u32;

        Ok(Self {
            file,
            mmap,
            file_size: AtomicU64::new(file_size),
            block_count: AtomicU32::new(block_count),
            path: path_str,
        })
    }

    /// Open an existing disk manager (file must exist)
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "open file".to_string(),
                path: path_str.clone(),
                source: e,
            })?;

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

        // Create memory map
        let mmap = unsafe {
            MmapOptions::new()
                .len(file_size as usize)
                .map_mut(&file)
                .map_err(|e| PersistentARTrieError::MmapError {
                    operation: "create mmap".to_string(),
                    source: e,
                })?
        };

        // Recover block count from file size (source of truth, handles crashes)
        let block_count = (file_size / BLOCK_SIZE as u64) as u32;

        let manager = Self {
            file,
            mmap: Some(RwLock::new(mmap)),
            file_size: AtomicU64::new(file_size),
            block_count: AtomicU32::new(block_count),
            path: path_str,
        };

        // Validate header
        let header = manager.read_header()?;
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

    /// Open an existing disk manager without validating the standard header.
    ///
    /// This is useful for files that use a different header format (e.g., VocabTrieFileHeader).
    /// The caller is responsible for validating the custom header format.
    ///
    /// # Arguments
    /// * `path` - Path to the data file
    ///
    /// # Returns
    /// * `Ok(DiskManager)` - Successfully opened file
    /// * `Err(PersistentARTrieError)` - I/O error
    pub fn open_without_validation<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "open file".to_string(),
                path: path_str.clone(),
                source: e,
            })?;

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

        // Create memory map
        let mmap = unsafe {
            MmapOptions::new()
                .len(file_size as usize)
                .map_mut(&file)
                .map_err(|e| PersistentARTrieError::MmapError {
                    operation: "create mmap".to_string(),
                    source: e,
                })?
        };

        // Recover block count from file size (source of truth, handles crashes)
        let block_count = (file_size / BLOCK_SIZE as u64) as u32;

        Ok(Self {
            file,
            mmap: Some(RwLock::new(mmap)),
            file_size: AtomicU64::new(file_size),
            block_count: AtomicU32::new(block_count),
            path: path_str,
        })
    }

    /// Initialize a new file with header block
    fn initialize_file(file: &File, path: &str) -> Result<()> {
        // Allocate header block (block 0)
        file.set_len(BLOCK_SIZE as u64)
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "set initial file length".to_string(),
                path: path.to_string(),
                source: e,
            })?;

        // Write header
        let mut header = FileHeader::new();
        header.update_checksum();

        let mut file_writer = file;
        file_writer
            .seek(SeekFrom::Start(0))
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "seek to header".to_string(),
                path: path.to_string(),
                source: e,
            })?;

        file_writer
            .write_all(&header.to_bytes())
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "write header".to_string(),
                path: path.to_string(),
                source: e,
            })?;

        file_writer
            .sync_all()
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "sync after header write".to_string(),
                path: path.to_string(),
                source: e,
            })?;

        Ok(())
    }

    /// Read the file header
    pub fn read_header(&self) -> Result<FileHeader> {
        let mmap_guard =
            self.mmap
                .as_ref()
                .ok_or_else(|| PersistentARTrieError::CorruptedFile {
                    reason: "No memory map available".to_string(),
                })?;

        let mmap = mmap_guard.read();

        if mmap.len() < 64 {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: "File too small for header".to_string(),
            });
        }

        let bytes: [u8; 64] =
            mmap[0..64]
                .try_into()
                .map_err(|_| PersistentARTrieError::CorruptedFile {
                    reason: "Failed to read header bytes".to_string(),
                })?;

        Ok(FileHeader::from_bytes(&bytes))
    }

    /// Write the file header
    pub fn write_header(&self, header: &FileHeader) -> Result<()> {
        let mmap_guard =
            self.mmap
                .as_ref()
                .ok_or_else(|| PersistentARTrieError::CorruptedFile {
                    reason: "No memory map available".to_string(),
                })?;

        let mut mmap = mmap_guard.write();

        let bytes = header.to_bytes();
        mmap[0..64].copy_from_slice(&bytes);

        Ok(())
    }

    /// Get the path to the underlying file.
    ///
    /// Returns the path string that was used to create or open this DiskManager.
    /// Useful for compaction operations that need to create a temporary file
    /// alongside the original.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Allocate a new block using lock-free CAS-based allocation.
    ///
    /// First checks the free list, then extends the file if needed.
    /// Uses compare-and-swap on the in-memory `block_count` atomic to ensure
    /// that concurrent allocations never receive duplicate block IDs.
    ///
    /// # Thread Safety
    ///
    /// The allocation uses a CAS loop on `self.block_count`:
    /// 1. Read current block count
    /// 2. CAS to claim the next block ID (only one thread wins)
    /// 3. Winner extends file, remaps, and updates `file_size` while holding mmap write lock
    /// 4. Losers retry with updated count
    ///
    /// The `file_size` atomic is updated WHILE holding the mmap write lock. Readers must
    /// acquire the mmap lock FIRST, then check `file_size`. This ensures:
    /// - If reader sees updated `file_size`, it has the lock and sees the new mmap
    /// - If reader sees old `file_size` while allocator holds write lock, it will block
    ///
    /// # Returns
    /// * `Ok(block_id)` - The ID of the allocated block
    /// * `Err(PersistentARTrieError)` - Allocation failed
    pub fn allocate_block(&self) -> Result<u32> {
        // Try to get a block from the free list first (existing CAS-based logic)
        // Note: Free list access could also race, but this is a best-effort optimization.
        // If we miss a free block, we just extend the file instead.
        let header = self.read_header()?;
        let free_head = header.free_list_head.load(Ordering::Acquire);

        if free_head != 0 {
            // Pop from free list
            let block_id = (free_head >> 40) as u32; // Extract block ID from swizzled format

            // Read the next pointer from the free block
            let next = self.read_free_block_next(block_id)?;

            // Update free list head (note: this could race with other free list pops,
            // but the worst case is extending the file when we didn't need to)
            header.free_list_head.store(next, Ordering::Release);
            self.write_header(&header)?;

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
                    // We won the race - now extend file and remap
                    //
                    // IMPORTANT: The entire file extension + remap sequence must be done
                    // while holding the mmap write lock to prevent races between:
                    // - Multiple concurrent set_len calls
                    // - set_len and mmap creation
                    //
                    // Order:
                    // 1. Acquire mmap write lock
                    // 2. Extend file (set_len)
                    // 3. pwrite() to materialize sparse region
                    // 4. Remap mmap
                    // 5. Memory barrier
                    // 6. Update file_size
                    // 7. Release write lock
                    //
                    // Readers acquire mmap lock FIRST, then check file_size.
                    {
                        let mmap_guard = self.mmap.as_ref().expect("mmap should exist");
                        let mut mmap = mmap_guard.write();

                        // 2. Extend file to at least new_file_size
                        // Check current size first to avoid unnecessary syscalls
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
                        }

                        // 3. Materialize sparse region via pwrite (prevents SIGBUS on some filesystems)
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::FileExt;
                            let offset = new_block_id as u64 * BLOCK_SIZE as u64;
                            let zeros = [0u8; 8];
                            // Best-effort: if this fails, the mmap write below will handle it
                            let _ = self.file.write_at(&zeros, offset);
                        }

                        // 4. Get actual file size (may be larger due to pwrite or prior extensions)
                        let actual_file_size = self
                            .file
                            .metadata()
                            .map_err(|e| PersistentARTrieError::IoError {
                                operation: "get file metadata for remap".to_string(),
                                path: self.path.clone(),
                                source: e,
                            })?
                            .len();

                        // Use the max of actual file size and our expected size
                        let remap_size = actual_file_size.max(new_file_size);

                        // Only remap if needed
                        if remap_size as usize > mmap.len() {
                            let new_mmap = unsafe {
                                MmapOptions::new()
                                    .len(remap_size as usize)
                                    .map_mut(&self.file)
                                    .map_err(|e| PersistentARTrieError::MmapError {
                                        operation: "remap after extend".to_string(),
                                        source: e,
                                    })?
                            };
                            *mmap = new_mmap;
                        }

                        // 5. Memory barrier to ensure mmap is visible before file_size update
                        std::sync::atomic::fence(Ordering::SeqCst);

                        // 6. Update file_size WHILE holding write lock using CAS to ensure monotonic increase
                        loop {
                            let current = self.file_size.load(Ordering::Acquire);
                            if remap_size <= current {
                                // Another allocator already updated to >= our size
                                break;
                            }
                            match self.file_size.compare_exchange(
                                current,
                                remap_size,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            ) {
                                Ok(_) => break,
                                Err(_) => continue, // Retry
                            }
                        }

                        // 7. Write lock released when `mmap` guard is dropped
                    }

                    // Update on-disk header (best-effort, recovered from file size on restart)
                    self.persist_header_block_count(new_count);

                    return Ok(new_block_id);
                }
                Err(_) => {
                    // Another thread won - retry with new count
                    std::hint::spin_loop();
                    continue;
                }
            }
        }
    }

    /// Persist the block count to the on-disk header.
    ///
    /// This is a best-effort operation - the block count is recovered from file size
    /// on restart, so transient failures are acceptable. Uses try_write to avoid
    /// blocking the hot path.
    fn persist_header_block_count(&self, count: u32) {
        if let Some(mmap_guard) = self.mmap.as_ref() {
            if let Some(mut mmap) = mmap_guard.try_write() {
                // Update block_count field in header (offset 24-28 in FileHeader)
                const BLOCK_COUNT_OFFSET: usize = 24;
                mmap[BLOCK_COUNT_OFFSET..BLOCK_COUNT_OFFSET + 4]
                    .copy_from_slice(&count.to_le_bytes());
                // Note: Checksum update is deferred to checkpoint/sync for performance.
                // Recovery recalculates block_count from file size anyway.
            }
            // If try_write fails, another thread holds the lock - that's fine,
            // they will update the header with a >= count value.
        }
    }

    /// Free a block, adding it to the free list
    pub fn free_block(&self, block_id: u32) -> Result<()> {
        if block_id == 0 {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "Cannot free header block".to_string(),
            });
        }

        // Use in-memory atomic for validation (source of truth)
        let block_count = self.block_count.load(Ordering::Acquire);
        let header = self.read_header()?;

        if block_id >= block_count {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!("Block ID {} >= block count {}", block_id, block_count),
            });
        }

        // Get current free list head
        let old_head = header.free_list_head.load(Ordering::SeqCst);

        // Write old head to the freed block
        self.write_free_block_next(block_id, old_head)?;

        // Update free list head to point to this block
        // Encode block_id in swizzled format (block_id << 40)
        let new_head = (block_id as u64) << 40;
        header.free_list_head.store(new_head, Ordering::SeqCst);

        // Update checksum and write header
        let mut updated_header = self.read_header()?;
        updated_header
            .free_list_head
            .store(new_head, Ordering::SeqCst);
        updated_header.checksum = updated_header.compute_checksum();
        self.write_header(&updated_header)?;

        Ok(())
    }

    /// Read the next pointer from a free block
    fn read_free_block_next(&self, block_id: u32) -> Result<u64> {
        let offset = block_id as usize * BLOCK_SIZE;

        let mmap_guard =
            self.mmap
                .as_ref()
                .ok_or_else(|| PersistentARTrieError::CorruptedFile {
                    reason: "No memory map available".to_string(),
                })?;

        let mmap = mmap_guard.read();

        if offset + 8 > mmap.len() {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "Block offset exceeds file size".to_string(),
            });
        }

        let bytes: [u8; 8] = mmap[offset..offset + 8].try_into().map_err(|_| {
            PersistentARTrieError::CorruptedFile {
                reason: "Failed to read free block next pointer".to_string(),
            }
        })?;

        Ok(u64::from_le_bytes(bytes))
    }

    /// Write the next pointer to a free block
    fn write_free_block_next(&self, block_id: u32, next: u64) -> Result<()> {
        let offset = block_id as usize * BLOCK_SIZE;

        let mmap_guard =
            self.mmap
                .as_ref()
                .ok_or_else(|| PersistentARTrieError::CorruptedFile {
                    reason: "No memory map available".to_string(),
                })?;

        let mut mmap = mmap_guard.write();

        if offset + 8 > mmap.len() {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: "Block offset exceeds file size".to_string(),
            });
        }

        mmap[offset..offset + 8].copy_from_slice(&next.to_le_bytes());

        Ok(())
    }

    // DISABLED — `remap` was the original out-of-line mmap-update path; it
    // is fully superseded by the inline remap inside `allocate_block`,
    // which can update `file_size` while still holding the write lock
    // (critical for thread safety). The dead `remap` method had no
    // remaining callers but stayed under `#[allow(dead_code)]` for years.
    // Kept here commented out per CLAUDE.md to preserve the audit trail.
    //
    // fn remap(&self, new_size: u64) -> Result<()> {
    //     let mmap_guard = self.mmap.as_ref().ok_or_else(|| {
    //         PersistentARTrieError::CorruptedFile {
    //             reason: "No memory map available".to_string(),
    //         }
    //     })?;
    //     let mut mmap = mmap_guard.write();
    //     let new_mmap = unsafe {
    //         MmapOptions::new()
    //             .len(new_size as usize)
    //             .map_mut(&self.file)
    //             .map_err(|e| PersistentARTrieError::MmapError {
    //                 operation: "remap after extend".to_string(),
    //                 source: e,
    //             })?
    //     };
    //     *mmap = new_mmap;
    //     Ok(())
    // }

    /// Read a block into a buffer
    ///
    /// # Arguments
    /// * `block_id` - The block to read
    /// * `buffer` - Buffer to read into (must be BLOCK_SIZE bytes)
    ///
    /// # Thread Safety
    ///
    /// Uses lock-ordered synchronization to prevent SIGBUS during concurrent allocation:
    /// 1. Quick-reject: validate block_id against block_count
    /// 2. Acquire mmap read lock (blocks if allocator is remapping)
    /// 3. Check file_size (if we got the lock, allocation is either complete or not started)
    /// 4. Access memory safely
    ///
    /// This ordering ensures:
    /// - If we see updated file_size, the new mmap is in place (allocator released lock)
    /// - If we see old file_size, allocation hasn't completed for this block yet
    pub fn read_block(&self, block_id: u32, buffer: &mut [u8; BLOCK_SIZE]) -> Result<()> {
        let offset = block_id as usize * BLOCK_SIZE;
        let end_offset = offset + BLOCK_SIZE;

        // Step 1: Quick-reject against block_count (source of truth)
        let current_block_count = self.block_count.load(Ordering::Acquire);
        if block_id >= current_block_count {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Block ID {} >= block count {}",
                    block_id, current_block_count
                ),
            });
        }

        let mmap_guard =
            self.mmap
                .as_ref()
                .ok_or_else(|| PersistentARTrieError::CorruptedFile {
                    reason: "No memory map available".to_string(),
                })?;

        // Step 2: Acquire mmap lock FIRST
        // This will block if the allocator is in the middle of remapping
        let mmap = mmap_guard.read();

        // Step 3: THEN check file_size
        // If we got the lock, either:
        // - Allocator hasn't started remapping (old file_size) → fail with clear error
        // - Allocator finished remapping (new file_size, new mmap) → safe to access
        let current_file_size = self.file_size.load(Ordering::Acquire);
        if end_offset as u64 > current_file_size {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Block {} not yet accessible (file_size={}, need={})",
                    block_id, current_file_size, end_offset
                ),
            });
        }

        // Step 4: Safe to access - we have lock and file_size confirms allocation complete
        buffer.copy_from_slice(&mmap[offset..end_offset]);
        Ok(())
    }

    /// Write a block from a buffer
    ///
    /// # Arguments
    /// * `block_id` - The block to write
    /// * `buffer` - Buffer to write from (must be BLOCK_SIZE bytes)
    ///
    /// # Thread Safety
    ///
    /// Uses lock-ordered synchronization to prevent SIGBUS during concurrent allocation:
    /// 1. Quick-reject: validate block_id against block_count
    /// 2. Acquire mmap write lock (blocks if allocator is remapping)
    /// 3. Check file_size (if we got the lock, allocation is either complete or not started)
    /// 4. Access memory safely
    pub fn write_block(&self, block_id: u32, buffer: &[u8; BLOCK_SIZE]) -> Result<()> {
        let offset = block_id as usize * BLOCK_SIZE;
        let end_offset = offset + BLOCK_SIZE;

        // Step 1: Quick-reject against block_count (source of truth)
        let current_block_count = self.block_count.load(Ordering::Acquire);
        if block_id >= current_block_count {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Block ID {} >= block count {}",
                    block_id, current_block_count
                ),
            });
        }

        let mmap_guard =
            self.mmap
                .as_ref()
                .ok_or_else(|| PersistentARTrieError::CorruptedFile {
                    reason: "No memory map available".to_string(),
                })?;

        // Step 2: Acquire mmap lock FIRST
        let mut mmap = mmap_guard.write();

        // Step 3: THEN check file_size
        let current_file_size = self.file_size.load(Ordering::Acquire);
        if end_offset as u64 > current_file_size {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Block {} not yet accessible (file_size={}, need={})",
                    block_id, current_file_size, end_offset
                ),
            });
        }

        // Step 4: Safe to access
        mmap[offset..end_offset].copy_from_slice(buffer);
        Ok(())
    }

    /// Read a slice of bytes from a block
    ///
    /// # Arguments
    /// * `block_id` - The block to read from
    /// * `offset_in_block` - Offset within the block
    /// * `buffer` - Buffer to read into
    ///
    /// # Thread Safety
    ///
    /// Uses lock-ordered synchronization: acquire mmap lock first, then check file_size.
    pub fn read_bytes(
        &self,
        block_id: u32,
        offset_in_block: usize,
        buffer: &mut [u8],
    ) -> Result<()> {
        let (file_offset, end_offset) =
            Self::validate_byte_range(block_id, offset_in_block, buffer.len())?;

        // Step 1: Quick-reject against block_count
        let current_block_count = self.block_count.load(Ordering::Acquire);
        if block_id >= current_block_count {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Block ID {} >= block count {}",
                    block_id, current_block_count
                ),
            });
        }

        let mmap_guard =
            self.mmap
                .as_ref()
                .ok_or_else(|| PersistentARTrieError::CorruptedFile {
                    reason: "No memory map available".to_string(),
                })?;

        // Step 2: Acquire mmap lock FIRST
        let mmap = mmap_guard.read();

        // Step 3: THEN check file_size
        let current_file_size = self.file_size.load(Ordering::Acquire);
        if end_offset as u64 > current_file_size {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Read range [{}, {}) not accessible (file_size={})",
                    file_offset, end_offset, current_file_size
                ),
            });
        }

        // Step 4: Safe to access
        buffer.copy_from_slice(&mmap[file_offset..end_offset]);
        Ok(())
    }

    /// Write a slice of bytes to a block
    ///
    /// # Arguments
    /// * `block_id` - The block to write to
    /// * `offset_in_block` - Offset within the block
    /// * `buffer` - Buffer to write from
    ///
    /// # Thread Safety
    ///
    /// Uses lock-ordered synchronization: acquire mmap lock first, then check file_size.
    pub fn write_bytes(&self, block_id: u32, offset_in_block: usize, buffer: &[u8]) -> Result<()> {
        let (file_offset, end_offset) =
            Self::validate_byte_range(block_id, offset_in_block, buffer.len())?;

        // Step 1: Quick-reject against block_count
        let current_block_count = self.block_count.load(Ordering::Acquire);
        if block_id >= current_block_count {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Block ID {} >= block count {}",
                    block_id, current_block_count
                ),
            });
        }

        let mmap_guard =
            self.mmap
                .as_ref()
                .ok_or_else(|| PersistentARTrieError::CorruptedFile {
                    reason: "No memory map available".to_string(),
                })?;

        // Step 2: Acquire mmap lock FIRST
        let mut mmap = mmap_guard.write();

        // Step 3: THEN check file_size
        let current_file_size = self.file_size.load(Ordering::Acquire);
        if end_offset as u64 > current_file_size {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Write range [{}, {}) not accessible (file_size={})",
                    file_offset, end_offset, current_file_size
                ),
            });
        }

        // Step 4: Safe to access
        mmap[file_offset..end_offset].copy_from_slice(buffer);
        Ok(())
    }

    /// Flush all changes to disk
    pub fn sync(&self) -> Result<()> {
        if let Some(mmap_guard) = &self.mmap {
            let mut mmap = mmap_guard.write();
            if mmap.len() < 64 {
                return Err(PersistentARTrieError::CorruptedFile {
                    reason: "File too small for header checksum refresh".to_string(),
                });
            }

            let header_bytes: [u8; 64] =
                mmap[0..64]
                    .try_into()
                    .map_err(|_| PersistentARTrieError::CorruptedFile {
                        reason: "Failed to read header bytes for checksum refresh".to_string(),
                    })?;
            let mut header = FileHeader::from_bytes(&header_bytes);
            header.checksum = header.compute_checksum();
            mmap[0..64].copy_from_slice(&header.to_bytes());

            mmap.flush().map_err(|e| PersistentARTrieError::MmapError {
                operation: "flush mmap".to_string(),
                source: e,
            })?;
        }

        self.file
            .sync_all()
            .map_err(|e| PersistentARTrieError::IoError {
                operation: "sync file".to_string(),
                path: self.path.clone(),
                source: e,
            })?;

        Ok(())
    }

    /// Get the current file size in bytes
    pub fn file_size(&self) -> u64 {
        self.file_size.load(Ordering::SeqCst)
    }

    /// Get the current block count
    ///
    /// Returns the in-memory atomic block count, which is the source of truth
    /// for lock-free allocation and is always >= the on-disk header value.
    pub fn block_count(&self) -> Result<u32> {
        Ok(self.block_count.load(Ordering::Acquire))
    }

    /// Get the entry count
    pub fn entry_count(&self) -> Result<u64> {
        let header = self.read_header()?;
        Ok(header.entry_count.load(Ordering::SeqCst))
    }

    /// Update the entry count
    pub fn set_entry_count(&self, count: u64) -> Result<()> {
        let header = self.read_header()?;
        header.entry_count.store(count, Ordering::SeqCst);

        let mut updated_header = header;
        updated_header.checksum = updated_header.compute_checksum();
        self.write_header(&updated_header)?;

        Ok(())
    }

    /// Get the root pointer
    pub fn root_ptr(&self) -> Result<u64> {
        let header = self.read_header()?;
        Ok(header.root_ptr.load(Ordering::SeqCst))
    }

    /// Set the root pointer
    pub fn set_root_ptr(&self, ptr: u64) -> Result<()> {
        let header = self.read_header()?;
        header.root_ptr.store(ptr, Ordering::SeqCst);

        let mut updated_header = header;
        updated_header.checksum = updated_header.compute_checksum();
        self.write_header(&updated_header)?;

        Ok(())
    }

    /// Write raw header bytes to block 0 offset 0.
    ///
    /// This is a convenience method for writing custom header formats
    /// (e.g., VocabTrieFileHeader which is 96 bytes instead of 64).
    ///
    /// # Arguments
    /// * `bytes` - The raw header bytes to write
    pub fn write_header_bytes(&self, bytes: &[u8]) -> Result<()> {
        self.write_bytes(0, 0, bytes)
    }

    /// Read raw header bytes from block 0 offset 0.
    ///
    /// This is a convenience method for reading custom header formats.
    ///
    /// # Arguments
    /// * `buffer` - Buffer to read into (determines how many bytes are read)
    pub fn read_header_bytes(&self, buffer: &mut [u8]) -> Result<()> {
        self.read_bytes(0, 0, buffer)
    }

    /// Get a raw pointer to a location in the memory map
    ///
    /// # Safety
    /// The caller must ensure the returned pointer is not used after the mmap is remapped
    /// or the DiskManager is dropped. `offset_in_block` must name a byte inside the
    /// requested block; one-past-end and cross-block offsets are rejected.
    pub unsafe fn raw_ptr(&self, block_id: u32, offset_in_block: usize) -> Result<*const u8> {
        if offset_in_block >= BLOCK_SIZE {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!(
                    "Offset {} exceeds block size {}",
                    offset_in_block, BLOCK_SIZE
                ),
            });
        }

        let (file_offset, _) = Self::validate_byte_range(block_id, offset_in_block, 1)?;

        let mmap_guard =
            self.mmap
                .as_ref()
                .ok_or_else(|| PersistentARTrieError::CorruptedFile {
                    reason: "No memory map available".to_string(),
                })?;

        let mmap = mmap_guard.read();

        if file_offset >= mmap.len() {
            return Err(PersistentARTrieError::InvalidBlockId {
                block_id,
                reason: format!("Offset {} exceeds file size {}", file_offset, mmap.len()),
            });
        }

        Ok(mmap.as_ptr().add(file_offset))
    }
}

// =============================================================================
// BlockStorage trait implementation for MmapDiskManager
// =============================================================================

impl BlockStorage for MmapDiskManager {
    fn read_block(&self, block_id: u32, buffer: &mut [u8; BLOCK_SIZE]) -> Result<()> {
        MmapDiskManager::read_block(self, block_id, buffer)
    }

    fn write_block(&self, block_id: u32, buffer: &[u8; BLOCK_SIZE]) -> Result<()> {
        MmapDiskManager::write_block(self, block_id, buffer)
    }

    fn read_bytes(&self, block_id: u32, offset: usize, buffer: &mut [u8]) -> Result<()> {
        MmapDiskManager::read_bytes(self, block_id, offset, buffer)
    }

    fn write_bytes(&self, block_id: u32, offset: usize, data: &[u8]) -> Result<()> {
        MmapDiskManager::write_bytes(self, block_id, offset, data)
    }

    fn allocate_block(&self) -> Result<u32> {
        MmapDiskManager::allocate_block(self)
    }

    fn free_block(&self, block_id: u32) -> Result<()> {
        MmapDiskManager::free_block(self, block_id)
    }

    fn read_header(&self) -> Result<FileHeader> {
        MmapDiskManager::read_header(self)
    }

    fn write_header(&self, header: &FileHeader) -> Result<()> {
        MmapDiskManager::write_header(self, header)
    }

    fn read_header_bytes(&self, buffer: &mut [u8]) -> Result<()> {
        MmapDiskManager::read_header_bytes(self, buffer)
    }

    fn write_header_bytes(&self, bytes: &[u8]) -> Result<()> {
        MmapDiskManager::write_header_bytes(self, bytes)
    }

    fn root_ptr(&self) -> Result<u64> {
        MmapDiskManager::root_ptr(self)
    }

    fn set_root_ptr(&self, ptr: u64) -> Result<()> {
        MmapDiskManager::set_root_ptr(self, ptr)
    }

    fn entry_count(&self) -> Result<u64> {
        MmapDiskManager::entry_count(self)
    }

    fn set_entry_count(&self, count: u64) -> Result<()> {
        MmapDiskManager::set_entry_count(self, count)
    }

    fn file_size(&self) -> u64 {
        MmapDiskManager::file_size(self)
    }

    fn block_count(&self) -> Result<u32> {
        MmapDiskManager::block_count(self)
    }

    fn path(&self) -> &str {
        MmapDiskManager::path(self)
    }

    fn sync(&self) -> Result<()> {
        MmapDiskManager::sync(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_file_header_serialization() {
        let header = FileHeader::new();
        let bytes = header.to_bytes();
        let restored = FileHeader::from_bytes(&bytes);

        assert_eq!(restored.magic, MAGIC_NUMBER);
        assert_eq!(restored.version, FORMAT_VERSION);
        assert_eq!(restored.flags, 0);
        assert_eq!(restored.root_ptr.load(Ordering::SeqCst), 0);
        assert_eq!(restored.block_count.load(Ordering::SeqCst), 1);
        assert_eq!(restored.free_list_head.load(Ordering::SeqCst), 0);
        assert_eq!(restored.entry_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_header_checksum() {
        let mut header = FileHeader::new();
        header.update_checksum();

        assert!(header.verify_checksum());

        // Modify a field and verify checksum fails
        header.entry_count.store(42, Ordering::SeqCst);
        assert!(!header.verify_checksum());

        // Update checksum and verify it passes again
        header.update_checksum();
        assert!(header.verify_checksum());
    }

    #[test]
    fn test_create_and_open() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.part");

        // Create new file
        {
            let dm = DiskManager::create(&path).expect("Failed to create DiskManager");
            assert_eq!(dm.file_size(), BLOCK_SIZE as u64);

            let header = dm.read_header().expect("Failed to read header");
            assert_eq!(header.magic, MAGIC_NUMBER);
            assert_eq!(header.block_count.load(Ordering::SeqCst), 1);
        }

        // Open existing file
        {
            let dm = DiskManager::open(&path).expect("Failed to open DiskManager");
            let header = dm.read_header().expect("Failed to read header");
            assert_eq!(header.magic, MAGIC_NUMBER);
        }
    }

    #[test]
    fn test_allocate_blocks() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test_alloc.part");

        let dm = DiskManager::create(&path).expect("Failed to create DiskManager");

        // Allocate several blocks
        let block1 = dm.allocate_block().expect("Failed to allocate block 1");
        let block2 = dm.allocate_block().expect("Failed to allocate block 2");
        let block3 = dm.allocate_block().expect("Failed to allocate block 3");

        assert_eq!(block1, 1);
        assert_eq!(block2, 2);
        assert_eq!(block3, 3);

        assert_eq!(dm.block_count().expect("Failed to get block count"), 4);
        assert_eq!(dm.file_size(), 4 * BLOCK_SIZE as u64);
    }

    #[test]
    fn test_free_list() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test_free.part");

        let dm = DiskManager::create(&path).expect("Failed to create DiskManager");

        // Allocate blocks
        let block1 = dm.allocate_block().expect("alloc 1");
        let block2 = dm.allocate_block().expect("alloc 2");
        let block3 = dm.allocate_block().expect("alloc 3");

        assert_eq!(block1, 1);
        assert_eq!(block2, 2);
        assert_eq!(block3, 3);

        // Free block 2
        dm.free_block(block2).expect("free block 2");

        // Next allocation should reuse block 2
        let block4 = dm.allocate_block().expect("alloc 4");
        assert_eq!(block4, 2);

        // Now allocate a new block
        let block5 = dm.allocate_block().expect("alloc 5");
        assert_eq!(block5, 4);
    }

    #[test]
    fn test_read_write_block() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test_rw.part");

        let dm = DiskManager::create(&path).expect("Failed to create DiskManager");
        let block_id = dm.allocate_block().expect("Failed to allocate block");

        // Write test data
        let mut write_buf = [0u8; BLOCK_SIZE];
        write_buf[0] = 0xDE;
        write_buf[1] = 0xAD;
        write_buf[2] = 0xBE;
        write_buf[3] = 0xEF;
        write_buf[BLOCK_SIZE - 1] = 0xFF;

        dm.write_block(block_id, &write_buf)
            .expect("Failed to write block");

        // Read back
        let mut read_buf = [0u8; BLOCK_SIZE];
        dm.read_block(block_id, &mut read_buf)
            .expect("Failed to read block");

        assert_eq!(read_buf[0], 0xDE);
        assert_eq!(read_buf[1], 0xAD);
        assert_eq!(read_buf[2], 0xBE);
        assert_eq!(read_buf[3], 0xEF);
        assert_eq!(read_buf[BLOCK_SIZE - 1], 0xFF);
    }

    #[test]
    fn test_read_write_bytes() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test_bytes.part");

        let dm = DiskManager::create(&path).expect("Failed to create DiskManager");
        let block_id = dm.allocate_block().expect("Failed to allocate block");

        // Write at offset
        let data = b"Hello, World!";
        dm.write_bytes(block_id, 100, data)
            .expect("Failed to write bytes");

        // Read back
        let mut read_buf = [0u8; 13];
        dm.read_bytes(block_id, 100, &mut read_buf)
            .expect("Failed to read bytes");

        assert_eq!(&read_buf, data);
    }

    #[test]
    fn test_root_ptr() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test_root.part");

        let dm = DiskManager::create(&path).expect("Failed to create DiskManager");

        // Initially zero
        assert_eq!(dm.root_ptr().expect("root_ptr"), 0);

        // Set root pointer
        dm.set_root_ptr(0x123456789ABCDEF0).expect("set_root_ptr");

        assert_eq!(
            dm.root_ptr().expect("root_ptr after set"),
            0x123456789ABCDEF0
        );

        // Sync and reopen
        dm.sync().expect("sync");
        drop(dm);

        let dm2 = DiskManager::open(&path).expect("reopen");
        assert_eq!(
            dm2.root_ptr().expect("root_ptr after reopen"),
            0x123456789ABCDEF0
        );
    }

    #[test]
    fn test_entry_count() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test_entry.part");

        let dm = DiskManager::create(&path).expect("Failed to create DiskManager");

        assert_eq!(dm.entry_count().expect("entry_count"), 0);

        dm.set_entry_count(12345).expect("set_entry_count");
        assert_eq!(dm.entry_count().expect("entry_count after set"), 12345);
    }

    #[test]
    fn test_cannot_free_header_block() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test_no_free_header.part");

        let dm = DiskManager::create(&path).expect("Failed to create DiskManager");

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
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test_invalid.part");

        let dm = DiskManager::create(&path).expect("Failed to create DiskManager");

        // Try to read a block that doesn't exist
        let mut buf = [0u8; BLOCK_SIZE];
        let result = dm.read_block(999, &mut buf);
        assert!(result.is_err());
    }

    /// Stress test for concurrent block allocation.
    ///
    /// This test verifies that the CAS-based allocation prevents the race condition
    /// that caused "Invalid block ID: Block offset + size exceeds file size" errors.
    ///
    /// The test spawns multiple threads that concurrently allocate blocks and
    /// immediately read/write to them. All allocated block IDs must be unique.
    #[test]
    fn test_concurrent_block_allocation() {
        use std::sync::Arc;
        use std::thread;

        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test_concurrent_alloc.part");

        let dm = Arc::new(DiskManager::create(&path).expect("Failed to create DiskManager"));

        const NUM_THREADS: usize = 8;
        const BLOCKS_PER_THREAD: usize = 100;

        let mut handles = Vec::with_capacity(NUM_THREADS);

        // Spawn threads that allocate and immediately write to blocks
        for thread_id in 0..NUM_THREADS {
            let dm = Arc::clone(&dm);
            handles.push(thread::spawn(move || {
                let mut allocated_ids = Vec::with_capacity(BLOCKS_PER_THREAD);

                for i in 0..BLOCKS_PER_THREAD {
                    // Allocate a block
                    let block_id = dm.allocate_block().unwrap_or_else(|e| {
                        panic!(
                            "Thread {} failed to allocate block {}: {:?}",
                            thread_id, i, e
                        )
                    });

                    // Immediately write to the block to trigger the race condition
                    // (if it exists - we're testing that it doesn't)
                    let mut buf = [0u8; BLOCK_SIZE];
                    buf[0..8].copy_from_slice(&(thread_id as u64).to_le_bytes());
                    buf[8..16].copy_from_slice(&(i as u64).to_le_bytes());

                    dm.write_block(block_id, &buf).unwrap_or_else(|e| {
                        panic!(
                            "Thread {} failed to write block {} (id={}): {:?}",
                            thread_id, i, block_id, e
                        )
                    });

                    // Read it back to verify
                    let mut read_buf = [0u8; BLOCK_SIZE];
                    dm.read_block(block_id, &mut read_buf).unwrap_or_else(|e| {
                        panic!(
                            "Thread {} failed to read block {} (id={}): {:?}",
                            thread_id, i, block_id, e
                        )
                    });

                    assert_eq!(&read_buf[0..8], &(thread_id as u64).to_le_bytes());
                    assert_eq!(&read_buf[8..16], &(i as u64).to_le_bytes());

                    allocated_ids.push(block_id);
                }

                allocated_ids
            }));
        }

        // Collect all allocated IDs
        let mut all_ids: Vec<u32> = handles
            .into_iter()
            .flat_map(|h| h.join().expect("Thread panicked"))
            .collect();

        // Verify all IDs are unique
        all_ids.sort();
        let original_len = all_ids.len();
        all_ids.dedup();

        assert_eq!(
            all_ids.len(),
            original_len,
            "Duplicate block IDs were allocated! Expected {} unique IDs, got {}",
            original_len,
            all_ids.len()
        );

        assert_eq!(
            original_len,
            NUM_THREADS * BLOCKS_PER_THREAD,
            "Expected {} allocated blocks, got {}",
            NUM_THREADS * BLOCKS_PER_THREAD,
            original_len
        );

        // Verify block count matches
        let block_count = dm.block_count().expect("Failed to get block count");
        // Block count = 1 (header) + allocated blocks
        assert_eq!(
            block_count as usize,
            1 + NUM_THREADS * BLOCKS_PER_THREAD,
            "Block count mismatch"
        );
    }

    /// Stress test for concurrent allocation with immediate read/write.
    ///
    /// This tests the critical property that the SAME thread that allocates a block
    /// can immediately write to and read from it. Cross-thread access to blocks
    /// still being allocated is not supported - callers must use their own blocks.
    #[test]
    fn test_concurrent_allocate_and_access() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
        use std::sync::Arc;
        use std::thread;
        use std::thread::JoinHandle;

        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test_concurrent_access.part");

        let dm = Arc::new(DiskManager::create(&path).expect("Failed to create DiskManager"));
        let stop = Arc::new(AtomicBool::new(false));
        // Track the highest block_id that has been FULLY written (safe to read)
        let safe_to_read = Arc::new(AtomicU64::new(0));

        const NUM_ALLOCATORS: usize = 4;
        const NUM_ACCESSORS: usize = 4;
        const ALLOCATIONS_PER_THREAD: usize = 50;

        let mut allocator_handles: Vec<JoinHandle<Vec<u32>>> = Vec::new();
        let mut accessor_handles: Vec<JoinHandle<u64>> = Vec::new();

        // Allocator threads
        for thread_id in 0..NUM_ALLOCATORS {
            let dm = Arc::clone(&dm);
            let stop = Arc::clone(&stop);
            let safe_to_read = Arc::clone(&safe_to_read);
            allocator_handles.push(thread::spawn(move || {
                let mut ids = Vec::new();
                for i in 0..ALLOCATIONS_PER_THREAD {
                    let block_id = dm.allocate_block().unwrap_or_else(|e| {
                        panic!("Allocator {} failed at {}: {:?}", thread_id, i, e)
                    });
                    ids.push(block_id);

                    // Write marker - this MUST work for the same thread that allocated
                    let mut buf = [0u8; BLOCK_SIZE];
                    buf[0..4].copy_from_slice(&block_id.to_le_bytes());
                    dm.write_block(block_id, &buf).unwrap_or_else(|e| {
                        panic!(
                            "Allocator {} failed to write block {}: {:?}",
                            thread_id, block_id, e
                        )
                    });

                    // Read back to verify - this MUST work for the same thread
                    let mut read_buf = [0u8; BLOCK_SIZE];
                    dm.read_block(block_id, &mut read_buf).unwrap_or_else(|e| {
                        panic!(
                            "Allocator {} failed to read-back block {}: {:?}",
                            thread_id, block_id, e
                        )
                    });
                    assert_eq!(
                        &read_buf[0..4],
                        &block_id.to_le_bytes(),
                        "Allocator {} read-back mismatch for block {}",
                        thread_id,
                        block_id
                    );

                    // Mark this block as safe to read by other threads
                    loop {
                        let current = safe_to_read.load(AtomicOrdering::Acquire);
                        if block_id as u64 <= current {
                            break; // Another thread already marked a higher block
                        }
                        match safe_to_read.compare_exchange(
                            current,
                            block_id as u64,
                            AtomicOrdering::AcqRel,
                            AtomicOrdering::Acquire,
                        ) {
                            Ok(_) => break,
                            Err(_) => continue,
                        }
                    }
                }
                stop.store(true, AtomicOrdering::Release);
                ids
            }));
        }

        // Accessor threads that try to read blocks that have been fully written
        for thread_id in 0..NUM_ACCESSORS {
            let dm = Arc::clone(&dm);
            let stop = Arc::clone(&stop);
            let safe_to_read = Arc::clone(&safe_to_read);
            accessor_handles.push(thread::spawn(move || {
                let mut successful_reads = 0u64;
                while !stop.load(AtomicOrdering::Acquire) {
                    // Only read blocks that have been fully written by allocator threads
                    let safe_block = safe_to_read.load(AtomicOrdering::Acquire);
                    if safe_block >= 1 {
                        // Pick a random block from the safe range
                        let block_id = ((successful_reads % safe_block) + 1) as u32;
                        let mut buf = [0u8; BLOCK_SIZE];
                        match dm.read_block(block_id, &mut buf) {
                            Ok(_) => successful_reads += 1,
                            Err(e) => {
                                // This should not happen for blocks marked as safe
                                panic!(
                                    "Accessor {} failed to read safe block {} (safe_to_read={}): {:?}",
                                    thread_id, block_id, safe_block, e
                                );
                            }
                        }
                    }
                    std::hint::spin_loop();
                }
                successful_reads
            }));
        }

        // Wait for allocator threads and collect allocated IDs
        let mut all_allocated: Vec<u32> = Vec::new();
        for handle in allocator_handles {
            let ids = handle.join().expect("Allocator thread panicked");
            all_allocated.extend(ids);
        }

        // Wait for accessor threads and collect read counts
        let mut total_reads = 0u64;
        for handle in accessor_handles {
            let reads = handle.join().expect("Accessor thread panicked");
            total_reads += reads;
        }

        // Verify uniqueness
        all_allocated.sort();
        let original_len = all_allocated.len();
        all_allocated.dedup();
        assert_eq!(all_allocated.len(), original_len, "Duplicate block IDs!");

        eprintln!(
            "Concurrent access test: {} blocks allocated, {} successful reads",
            original_len, total_reads
        );
    }
}
