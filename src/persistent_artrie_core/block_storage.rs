//! Block Storage Trait for Persistent Adaptive Radix Trie
//!
//! This module defines the `BlockStorage` trait, which abstracts over different
//! I/O backends for the persistent ARTrie. The trait provides a unified interface
//! for block-level read/write operations, allowing the ARTrie to operate over:
//!
//! - **mmap** (`MmapDiskManager`): Memory-mapped file I/O (default)
//! - **io_uring** (`IoUringDiskManager`): Linux io_uring + O_DIRECT for
//!   predictable latency and zero double-caching
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────┐
//! │        BufferManager<S>          │
//! │     (Page cache, Clock LRU)      │
//! ├──────────────────────────────────┤
//! │      trait BlockStorage          │
//! ├──────────┬───────────────────────┤
//! │ MmapDisk │  IoUringDiskManager   │
//! │ Manager  │  (O_DIRECT + uring)   │
//! └──────────┴───────────────────────┘
//! ```
//!
//! # Alignment
//!
//! `AlignedBlock` provides 4096-byte alignment required by O_DIRECT.
//! Since `BLOCK_SIZE` (256KB) is already a multiple of 4096, this
//! adds zero overhead while ensuring compatibility with both backends.

use super::disk_manager::{FileHeader, BLOCK_SIZE};
use super::error::Result;

/// A 4096-byte aligned block buffer for O_DIRECT compatibility.
///
/// O_DIRECT requires buffers to be aligned to the filesystem's logical block size
/// (typically 512 or 4096 bytes). Using `#[repr(C, align(4096))]` ensures alignment
/// regardless of the backend, so `BufferManager` can switch between mmap and io_uring
/// without changing its buffer pool.
///
/// Since `BLOCK_SIZE` (256KB = 262144) is already a multiple of 4096, this alignment
/// adds zero padding overhead.
#[repr(C, align(4096))]
pub struct AlignedBlock {
    /// The raw block data.
    pub data: [u8; BLOCK_SIZE],
}

impl AlignedBlock {
    /// Create a new zero-initialized aligned block.
    pub fn new() -> Self {
        Self {
            data: [0u8; BLOCK_SIZE],
        }
    }

    /// Create a new zero-initialized aligned block directly on the heap.
    ///
    /// Unlike `Box::new(AlignedBlock::new())`, this avoids placing the 256KB
    /// struct on the stack first (which can cause stack overflow in debug builds
    /// or when multiple blocks are needed).
    pub fn new_boxed() -> Box<Self> {
        unsafe {
            let layout = std::alloc::Layout::new::<Self>();
            let ptr = std::alloc::alloc_zeroed(layout) as *mut Self;
            assert!(!ptr.is_null(), "failed to allocate AlignedBlock on heap");
            Box::from_raw(ptr)
        }
    }

    /// Create a new aligned block from existing data.
    pub fn from_data(data: [u8; BLOCK_SIZE]) -> Self {
        Self { data }
    }

    /// Get a slice view of the block data.
    #[inline]
    pub fn as_slice(&self) -> &[u8; BLOCK_SIZE] {
        &self.data
    }

    /// Get a mutable slice view of the block data.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [u8; BLOCK_SIZE] {
        &mut self.data
    }
}

impl Default for AlignedBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Deref for AlignedBlock {
    type Target = [u8; BLOCK_SIZE];

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl std::ops::DerefMut for AlignedBlock {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

impl std::fmt::Debug for AlignedBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlignedBlock")
            .field("len", &BLOCK_SIZE)
            .field("align", &std::mem::align_of::<Self>())
            .finish()
    }
}

/// Abstraction over block-level storage I/O.
///
/// This trait defines the interface that `BufferManager` uses for all disk I/O.
/// Implementations must be `Send + Sync` for use in concurrent buffer managers.
///
/// # Block Layout
///
/// - Block 0 contains the file header (first 64 bytes)
/// - Blocks 1..N contain data (256KB each)
/// - Block IDs are 24-bit (max 16M blocks = 4TB)
///
/// # Thread Safety
///
/// Implementations must be safe for concurrent access from multiple threads.
/// The `BufferManager` may call `read_block` from multiple threads simultaneously,
/// and `write_block` / `allocate_block` from one thread while others read.
// `'static`: every storage backend owns its resources (mmap/files/buffers) and
// carries no borrowed lifetime; all impls (MmapDiskManager, IoUringDiskManager,
// TrackingFixedStorage) are concrete owned types. Requiring it lets a
// `PersistentARTrieChar<V, S>` be erased behind a `dyn` trait object (see
// `CharNodeFaulter` in persistent_artrie_char) without a node lifetime param.
pub trait BlockStorage: Send + Sync + 'static {
    /// Read a full block from storage.
    ///
    /// # Arguments
    /// * `block_id` - The block to read (0 = header block)
    /// * `buffer` - Buffer to read into (must be exactly `BLOCK_SIZE` bytes)
    ///
    /// # Errors
    /// Returns an error if the block_id is invalid or I/O fails.
    fn read_block(&self, block_id: u32, buffer: &mut [u8; BLOCK_SIZE]) -> Result<()>;

    /// Write a full block to storage.
    ///
    /// # Arguments
    /// * `block_id` - The block to write (0 = header block)
    /// * `buffer` - Data to write (must be exactly `BLOCK_SIZE` bytes)
    ///
    /// # Errors
    /// Returns an error if the block_id is invalid or I/O fails.
    fn write_block(&self, block_id: u32, buffer: &[u8; BLOCK_SIZE]) -> Result<()>;

    /// Read a sub-block byte range from storage.
    ///
    /// # Arguments
    /// * `block_id` - The block to read from
    /// * `offset` - Offset within the block
    /// * `buffer` - Buffer to read into (determines how many bytes are read)
    ///
    /// # Errors
    /// Returns an error if the range is out of bounds or I/O fails.
    fn read_bytes(&self, block_id: u32, offset: usize, buffer: &mut [u8]) -> Result<()>;

    /// Write a sub-block byte range to storage.
    ///
    /// # Arguments
    /// * `block_id` - The block to write to
    /// * `offset` - Offset within the block
    /// * `data` - Data to write
    ///
    /// # Errors
    /// Returns an error if the range is out of bounds or I/O fails.
    fn write_bytes(&self, block_id: u32, offset: usize, data: &[u8]) -> Result<()>;

    /// Allocate a new block, extending the file if necessary.
    ///
    /// # Returns
    /// The ID of the newly allocated block.
    ///
    /// # Thread Safety
    /// Must be safe to call concurrently. Implementations should use CAS or
    /// similar techniques to ensure unique block IDs.
    fn allocate_block(&self) -> Result<u32>;

    /// Free a block, adding it to the free list for reuse.
    ///
    /// # Arguments
    /// * `block_id` - The block to free (must not be block 0)
    fn free_block(&self, block_id: u32) -> Result<()>;

    /// Read the file header from block 0.
    fn read_header(&self) -> Result<FileHeader>;

    /// Write the file header to block 0.
    fn write_header(&self, header: &FileHeader) -> Result<()>;

    /// Read raw header bytes from block 0 offset 0.
    ///
    /// Convenience method for reading custom header formats
    /// (e.g., VocabTrieFileHeader which is 96 bytes instead of 64).
    fn read_header_bytes(&self, buffer: &mut [u8]) -> Result<()>;

    /// Write raw header bytes to block 0 offset 0.
    ///
    /// Convenience method for writing custom header formats.
    fn write_header_bytes(&self, bytes: &[u8]) -> Result<()>;

    /// Get the root pointer from the file header.
    fn root_ptr(&self) -> Result<u64>;

    /// Set the root pointer in the file header.
    fn set_root_ptr(&self, ptr: u64) -> Result<()>;

    /// Get the entry count from the file header.
    fn entry_count(&self) -> Result<u64>;

    /// Set the entry count in the file header.
    fn set_entry_count(&self, count: u64) -> Result<()>;

    /// Get the current file size in bytes.
    fn file_size(&self) -> u64;

    /// Get the current block count.
    fn block_count(&self) -> Result<u32>;

    /// Get the file path.
    fn path(&self) -> &str;

    /// Flush all changes to durable storage.
    fn sync(&self) -> Result<()>;

    /// Read multiple blocks in a single batch operation.
    ///
    /// The default implementation reads blocks sequentially.
    /// The io_uring backend overrides this with batched SQE submission
    /// for significantly better throughput on sequential arena flushes.
    ///
    /// # Arguments
    /// * `requests` - Slice of (block_id, buffer) pairs to read
    fn read_blocks_batch(&self, requests: &mut [(u32, &mut [u8; BLOCK_SIZE])]) -> Result<()> {
        for (block_id, buffer) in requests.iter_mut() {
            self.read_block(*block_id, buffer)?;
        }
        Ok(())
    }

    /// Write multiple blocks in a single batch operation.
    ///
    /// The default implementation writes blocks sequentially.
    /// The io_uring backend overrides this with batched SQE submission.
    ///
    /// # Arguments
    /// * `requests` - Slice of (block_id, buffer) pairs to write
    fn write_blocks_batch(&self, requests: &[(u32, &[u8; BLOCK_SIZE])]) -> Result<()> {
        for (block_id, buffer) in requests {
            self.write_block(*block_id, buffer)?;
        }
        Ok(())
    }

    // =========================================================================
    // Pre-registered buffer support (zero-copy I/O)
    // =========================================================================

    /// Register a buffer pool for zero-copy I/O.
    ///
    /// Called by `BufferManager` after allocating its buffer pool. Backends that
    /// support pre-registered buffers (e.g., io_uring with `IORING_REGISTER_BUFFERS`)
    /// can pin these buffers in the kernel for zero-copy I/O via
    /// `ReadFixed`/`WriteFixed`, eliminating kernel-side `copy_from_user`/`copy_to_user`.
    ///
    /// # Safety
    /// The caller must ensure the buffer pointers remain valid and unmoved until
    /// `unregister_buffer_pool()` is called or the storage backend is dropped.
    ///
    /// # Arguments
    /// * `buffers` - Slice of (pointer, length) pairs for each buffer to register
    ///
    /// Default: no-op (returns `Ok(())`)
    unsafe fn register_buffer_pool(&self, _buffers: &[(*mut u8, usize)]) -> Result<()> {
        Ok(())
    }

    /// Unregister a previously registered buffer pool.
    ///
    /// Called by `BufferManager` on drop or when the buffer pool is being
    /// deallocated. After this call, `supports_fixed_buffers()` must return false.
    ///
    /// Default: no-op (returns `Ok(())`)
    fn unregister_buffer_pool(&self) -> Result<()> {
        Ok(())
    }

    /// Read a block using a pre-registered buffer index (zero-copy path).
    ///
    /// When buffers are registered, this uses `ReadFixed` instead of `Read`,
    /// eliminating kernel-side buffer copies. The buffer must be part of the
    /// previously registered pool.
    ///
    /// # Arguments
    /// * `block_id` - Block to read
    /// * `buffer` - Destination buffer (must be part of the registered pool)
    /// * `buf_index` - Index into the registered buffer array
    ///
    /// Default: falls back to `read_block()` (ignoring `buf_index`)
    fn read_block_fixed(
        &self,
        block_id: u32,
        buffer: &mut [u8; BLOCK_SIZE],
        _buf_index: u16,
    ) -> Result<()> {
        self.read_block(block_id, buffer)
    }

    /// Write a block using a pre-registered buffer index (zero-copy path).
    ///
    /// When buffers are registered, this uses `WriteFixed` instead of `Write`,
    /// eliminating kernel-side buffer copies. The buffer must be part of the
    /// previously registered pool.
    ///
    /// # Arguments
    /// * `block_id` - Block to write
    /// * `buffer` - Source buffer (must be part of the registered pool)
    /// * `buf_index` - Index into the registered buffer array
    ///
    /// Default: falls back to `write_block()` (ignoring `buf_index`)
    fn write_block_fixed(
        &self,
        block_id: u32,
        buffer: &[u8; BLOCK_SIZE],
        _buf_index: u16,
    ) -> Result<()> {
        self.write_block(block_id, buffer)
    }

    /// Whether this backend supports pre-registered buffers.
    ///
    /// Returns true only after a successful `register_buffer_pool()` call.
    /// `BufferManager` checks this to decide between fixed and non-fixed I/O paths.
    ///
    /// Default: false
    fn supports_fixed_buffers(&self) -> bool {
        false
    }

    /// Batch read using pre-registered buffer indices (zero-copy path).
    ///
    /// Combines the benefits of batch SQE submission with zero-copy I/O.
    /// No intermediate `AlignedBlock` allocation is needed since the caller's
    /// buffers ARE the registered buffers.
    ///
    /// # Arguments
    /// * `requests` - Slice of (block_id, buffer, buf_index) tuples
    ///
    /// Default: falls back to sequential `read_block_fixed()` calls.
    fn read_blocks_batch_fixed(
        &self,
        requests: &mut [(u32, &mut [u8; BLOCK_SIZE], u16)],
    ) -> Result<()> {
        for (block_id, buffer, buf_index) in requests.iter_mut() {
            self.read_block_fixed(*block_id, buffer, *buf_index)?;
        }
        Ok(())
    }

    /// Batch write using pre-registered buffer indices (zero-copy path).
    ///
    /// Combines the benefits of batch SQE submission with zero-copy I/O.
    /// No intermediate `AlignedBlock` allocation is needed since the caller's
    /// buffers ARE the registered buffers.
    ///
    /// # Arguments
    /// * `requests` - Slice of (block_id, buffer, buf_index) tuples
    ///
    /// Default: falls back to sequential `write_block_fixed()` calls.
    fn write_blocks_batch_fixed(&self, requests: &[(u32, &[u8; BLOCK_SIZE], u16)]) -> Result<()> {
        for &(block_id, buffer, buf_index) in requests {
            self.write_block_fixed(block_id, buffer, buf_index)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aligned_block_alignment() {
        let block = AlignedBlock::new();
        let addr = &block as *const AlignedBlock as usize;
        assert_eq!(
            addr % 4096,
            0,
            "AlignedBlock must be 4096-byte aligned, got addr 0x{:x}",
            addr
        );
    }

    #[test]
    fn test_aligned_block_size() {
        assert_eq!(
            std::mem::size_of::<AlignedBlock>(),
            BLOCK_SIZE,
            "AlignedBlock size must equal BLOCK_SIZE"
        );
    }

    #[test]
    fn test_aligned_block_deref() {
        let mut block = AlignedBlock::new();
        block[0] = 0xDE;
        block[1] = 0xAD;
        assert_eq!(block.data[0], 0xDE);
        assert_eq!(block.data[1], 0xAD);

        // Deref
        let slice: &[u8; BLOCK_SIZE] = &*block;
        assert_eq!(slice[0], 0xDE);
    }

    #[test]
    fn test_aligned_block_from_data() {
        let mut data = [0u8; BLOCK_SIZE];
        data[0] = 42;
        let block = AlignedBlock::from_data(data);
        assert_eq!(block.data[0], 42);
    }

    #[test]
    fn test_aligned_block_debug() {
        let block = AlignedBlock::new();
        let debug = format!("{:?}", block);
        assert!(debug.contains("AlignedBlock"));
        assert!(debug.contains("4096"));
    }
}
