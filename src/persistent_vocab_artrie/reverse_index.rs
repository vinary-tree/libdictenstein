//! Memory-mapped reverse index for O(1) vocabulary index → NodeRef lookups.
//!
//! This module provides [`VocabReverseIndex`], a persistent index that maps
//! vocabulary indices (u64) to node references (NodeRef), enabling fast
//! reverse lookups without traversing the trie.
//!
//! # Design
//!
//! The reverse index is stored as a memory-mapped file containing a dense
//! array of NodeRef values:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    ReverseIndex File                         │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Header (32 bytes)                                          │
//! │  ┌─────────────────────────────────────────────────────────┐│
//! │  │ magic: [u8; 4]      // "VRIX"                           ││
//! │  │ version: u8         // 1                                ││
//! │  │ _reserved: [u8; 3]                                      ││
//! │  │ start_index: u64    // First valid index                ││
//! │  │ entry_count: u64    // Number of valid entries          ││
//! │  │ capacity: u64       // Total capacity                   ││
//! │  └─────────────────────────────────────────────────────────┘│
//! ├─────────────────────────────────────────────────────────────┤
//! │  Entries (8 bytes each)                                     │
//! │  ┌─────────────────────────────────────────────────────────┐│
//! │  │ [0]: NodeRef for index start_index                      ││
//! │  │ [1]: NodeRef for index start_index + 1                  ││
//! │  │ ...                                                     ││
//! │  │ [N-1]: NodeRef for index start_index + N - 1            ││
//! │  └─────────────────────────────────────────────────────────┘│
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```text
//! use libdictenstein::persistent_vocab_artrie::reverse_index::VocabReverseIndex;
//!
//! // Create a new index
//! let mut index = VocabReverseIndex::create("vocab.idx", 0, 10000)?;
//!
//! // Add entries
//! index.set(42, NodeRef::new(0, 123))?;
//!
//! // Look up entries
//! if let Some(node_ref) = index.get(42) {
//!     println!("Index 42 maps to arena {} slot {}", node_ref.arena_id, node_ref.slot_index);
//! }
//! ```

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use memmap2::{MmapMut, MmapOptions};

use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie_char::types::NodeRef;

/// Magic bytes for reverse index file: "VRIX"
pub const REVERSE_INDEX_MAGIC: [u8; 4] = *b"VRIX";

/// Reverse index header size in bytes
pub const REVERSE_INDEX_HEADER_SIZE: usize = 32;

/// Size of each entry (NodeRef = 8 bytes)
pub const ENTRY_SIZE: usize = 8;

/// Default initial capacity
pub const DEFAULT_INITIAL_CAPACITY: u64 = 1024;

/// Growth factor when capacity is exceeded
pub const GROWTH_FACTOR: f64 = 1.5;

/// Reverse index header (32 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ReverseIndexHeader {
    /// Magic bytes "VRIX"
    pub magic: [u8; 4],
    /// Format version
    pub version: u8,
    /// Reserved bytes
    pub _reserved: [u8; 3],
    /// Starting index
    pub start_index: u64,
    /// Number of valid entries
    pub entry_count: u64,
    /// Total capacity (number of slots)
    pub capacity: u64,
}

impl ReverseIndexHeader {
    /// Create a new header
    pub fn new(start_index: u64, capacity: u64) -> Self {
        Self {
            magic: REVERSE_INDEX_MAGIC,
            version: 1,
            _reserved: [0; 3],
            start_index,
            entry_count: 0,
            capacity,
        }
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> [u8; REVERSE_INDEX_HEADER_SIZE] {
        let mut bytes = [0u8; REVERSE_INDEX_HEADER_SIZE];
        bytes[0..4].copy_from_slice(&self.magic);
        bytes[4] = self.version;
        bytes[5..8].copy_from_slice(&self._reserved);
        bytes[8..16].copy_from_slice(&self.start_index.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.entry_count.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.capacity.to_le_bytes());
        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8; REVERSE_INDEX_HEADER_SIZE]) -> Self {
        Self {
            magic: [bytes[0], bytes[1], bytes[2], bytes[3]],
            version: bytes[4],
            _reserved: [bytes[5], bytes[6], bytes[7]],
            start_index: u64::from_le_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
            entry_count: u64::from_le_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22],
                bytes[23],
            ]),
            capacity: u64::from_le_bytes([
                bytes[24], bytes[25], bytes[26], bytes[27], bytes[28], bytes[29], bytes[30],
                bytes[31],
            ]),
        }
    }

    /// Validate the header
    pub fn validate(&self) -> Result<()> {
        if self.magic != REVERSE_INDEX_MAGIC {
            let expected = u64::from_le_bytes([
                REVERSE_INDEX_MAGIC[0],
                REVERSE_INDEX_MAGIC[1],
                REVERSE_INDEX_MAGIC[2],
                REVERSE_INDEX_MAGIC[3],
                0,
                0,
                0,
                0,
            ]);
            let found = u64::from_le_bytes([
                self.magic[0],
                self.magic[1],
                self.magic[2],
                self.magic[3],
                0,
                0,
                0,
                0,
            ]);
            return Err(PersistentARTrieError::InvalidMagic { expected, found });
        }
        if self.version != 1 {
            return Err(PersistentARTrieError::UnsupportedVersion {
                max_supported: 1,
                found: self.version as u32,
            });
        }
        Ok(())
    }
}

/// Memory-mapped reverse index for vocabulary index → NodeRef lookups.
///
/// This provides O(1) lookup of the node containing a given vocabulary index,
/// enabling efficient reverse lookups (index → term) via parent pointer backtracking.
pub struct VocabReverseIndex {
    /// Path to the index file
    path: PathBuf,
    /// Memory-mapped file
    mmap: MmapMut,
    /// Starting vocabulary index
    start_index: u64,
    /// Current number of entries
    entry_count: u64,
    /// Total capacity
    capacity: u64,
}

impl VocabReverseIndex {
    /// Create a new reverse index file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the index file
    /// * `start_index` - Starting vocabulary index
    /// * `initial_capacity` - Initial number of entries to allocate
    pub fn create<P: AsRef<Path>>(
        path: P,
        start_index: u64,
        initial_capacity: u64,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let capacity = initial_capacity.max(DEFAULT_INITIAL_CAPACITY);

        // Create and size the file
        let file_size = REVERSE_INDEX_HEADER_SIZE + (capacity as usize * ENTRY_SIZE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| {
                PersistentARTrieError::io_error("create reverse index", path.to_string_lossy(), e)
            })?;

        file.set_len(file_size as u64).map_err(|e| {
            PersistentARTrieError::io_error("set reverse index size", path.to_string_lossy(), e)
        })?;

        // Memory map the file
        let mut mmap = unsafe {
            MmapOptions::new().map_mut(&file).map_err(|e| {
                PersistentARTrieError::io_error("mmap reverse index", path.to_string_lossy(), e)
            })?
        };

        // Write header
        let header = ReverseIndexHeader::new(start_index, capacity);
        mmap[..REVERSE_INDEX_HEADER_SIZE].copy_from_slice(&header.to_bytes());

        // Initialize entries to NULL
        let null_bytes = NodeRef::NULL.to_bytes();
        for i in 0..capacity as usize {
            let offset = REVERSE_INDEX_HEADER_SIZE + i * ENTRY_SIZE;
            mmap[offset..offset + ENTRY_SIZE].copy_from_slice(&null_bytes);
        }

        // Sync to disk
        mmap.flush().map_err(|e| {
            PersistentARTrieError::io_error("flush reverse index", path.to_string_lossy(), e)
        })?;

        Ok(Self {
            path,
            mmap,
            start_index,
            entry_count: 0,
            capacity,
        })
    }

    /// Open an existing reverse index file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the index file
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| {
                PersistentARTrieError::io_error("open reverse index", path.to_string_lossy(), e)
            })?;

        // Memory map the file
        let mmap = unsafe {
            MmapOptions::new().map_mut(&file).map_err(|e| {
                PersistentARTrieError::io_error("mmap reverse index", path.to_string_lossy(), e)
            })?
        };

        // Read and validate header
        if mmap.len() < REVERSE_INDEX_HEADER_SIZE {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: format!("Reverse index file too small: {} bytes", mmap.len()),
            });
        }

        let mut header_bytes = [0u8; REVERSE_INDEX_HEADER_SIZE];
        header_bytes.copy_from_slice(&mmap[..REVERSE_INDEX_HEADER_SIZE]);
        let header = ReverseIndexHeader::from_bytes(&header_bytes);
        header.validate()?;

        Ok(Self {
            path,
            mmap,
            start_index: header.start_index,
            entry_count: header.entry_count,
            capacity: header.capacity,
        })
    }

    /// Get the NodeRef for a vocabulary index.
    ///
    /// Returns `None` if:
    /// - The index is below start_index
    /// - The index is beyond the current entry count
    /// - The entry is NULL (not yet assigned)
    ///
    /// # Performance
    ///
    /// O(1) - direct memory access
    #[inline]
    pub fn get(&self, index: u64) -> Option<NodeRef> {
        if index < self.start_index {
            return None;
        }

        let slot = index - self.start_index;
        if slot >= self.capacity {
            return None;
        }

        let offset = REVERSE_INDEX_HEADER_SIZE + (slot as usize * ENTRY_SIZE);
        if offset + ENTRY_SIZE > self.mmap.len() {
            return None;
        }

        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&self.mmap[offset..offset + ENTRY_SIZE]);
        let node_ref = NodeRef::from_bytes(&bytes);

        if node_ref.is_null() {
            None
        } else {
            Some(node_ref)
        }
    }

    /// Set the NodeRef for a vocabulary index.
    ///
    /// # Arguments
    ///
    /// * `index` - The vocabulary index
    /// * `node_ref` - The NodeRef pointing to the trie node
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, `Err` if the index is out of range or growth fails
    pub fn set(&mut self, index: u64, node_ref: NodeRef) -> Result<()> {
        if index < self.start_index {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: format!("Index {} is below start_index {}", index, self.start_index),
            });
        }

        let slot = index - self.start_index;

        // Grow if necessary
        if slot >= self.capacity {
            self.grow(slot + 1)?;
        }

        // Write entry
        let offset = REVERSE_INDEX_HEADER_SIZE + (slot as usize * ENTRY_SIZE);
        self.mmap[offset..offset + ENTRY_SIZE].copy_from_slice(&node_ref.to_bytes());

        // Update entry count if this extends it
        let new_count = slot + 1;
        if new_count > self.entry_count {
            self.entry_count = new_count;
            self.update_header()?;
        }

        Ok(())
    }

    /// Check if an index is present (has a non-NULL NodeRef).
    #[inline]
    pub fn contains(&self, index: u64) -> bool {
        self.get(index).is_some()
    }

    /// Get the starting vocabulary index.
    #[inline]
    pub fn start_index(&self) -> u64 {
        self.start_index
    }

    /// Get the number of entries.
    #[inline]
    pub fn len(&self) -> u64 {
        self.entry_count
    }

    /// Check if the index is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    /// Get the current capacity.
    #[inline]
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Flush changes to disk.
    pub fn flush(&self) -> Result<()> {
        self.mmap.flush().map_err(|e| {
            PersistentARTrieError::io_error("flush reverse index", self.path.to_string_lossy(), e)
        })
    }

    /// Grow the index to accommodate at least `min_capacity` entries.
    fn grow(&mut self, min_capacity: u64) -> Result<()> {
        // Calculate new capacity with growth factor
        let mut new_capacity = self.capacity;
        while new_capacity < min_capacity {
            new_capacity =
                ((new_capacity as f64 * GROWTH_FACTOR).ceil() as u64).max(new_capacity + 1);
        }

        // Unmap current file
        self.mmap.flush().map_err(|e| {
            PersistentARTrieError::io_error("flush before grow", self.path.to_string_lossy(), e)
        })?;

        // Open file and resize
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(|e| {
                PersistentARTrieError::io_error("open for grow", self.path.to_string_lossy(), e)
            })?;

        let new_file_size = REVERSE_INDEX_HEADER_SIZE + (new_capacity as usize * ENTRY_SIZE);
        file.set_len(new_file_size as u64).map_err(|e| {
            PersistentARTrieError::io_error("grow reverse index", self.path.to_string_lossy(), e)
        })?;

        // Re-map
        let mut new_mmap = unsafe {
            MmapOptions::new().map_mut(&file).map_err(|e| {
                PersistentARTrieError::io_error("remap after grow", self.path.to_string_lossy(), e)
            })?
        };

        // Initialize new entries to NULL
        let null_bytes = NodeRef::NULL.to_bytes();
        for i in self.capacity as usize..new_capacity as usize {
            let offset = REVERSE_INDEX_HEADER_SIZE + i * ENTRY_SIZE;
            new_mmap[offset..offset + ENTRY_SIZE].copy_from_slice(&null_bytes);
        }

        // Update capacity in header
        self.capacity = new_capacity;
        let header = ReverseIndexHeader {
            magic: REVERSE_INDEX_MAGIC,
            version: 1,
            _reserved: [0; 3],
            start_index: self.start_index,
            entry_count: self.entry_count,
            capacity: self.capacity,
        };
        new_mmap[..REVERSE_INDEX_HEADER_SIZE].copy_from_slice(&header.to_bytes());

        // Swap mmap
        self.mmap = new_mmap;

        // Sync
        self.mmap.flush().map_err(|e| {
            PersistentARTrieError::io_error("flush after grow", self.path.to_string_lossy(), e)
        })?;

        Ok(())
    }

    /// Update the header in the mmap
    fn update_header(&mut self) -> Result<()> {
        let header = ReverseIndexHeader {
            magic: REVERSE_INDEX_MAGIC,
            version: 1,
            _reserved: [0; 3],
            start_index: self.start_index,
            entry_count: self.entry_count,
            capacity: self.capacity,
        };
        self.mmap[..REVERSE_INDEX_HEADER_SIZE].copy_from_slice(&header.to_bytes());
        Ok(())
    }

    /// Iterate over all valid (index, NodeRef) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (u64, NodeRef)> + '_ {
        (0..self.entry_count).filter_map(move |slot| {
            let index = self.start_index + slot;
            self.get(index).map(|node_ref| (index, node_ref))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_reverse_index_create_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.idx");

        // Create
        {
            let mut index = VocabReverseIndex::create(&path, 100, 1000).unwrap();
            assert_eq!(index.start_index(), 100);
            assert_eq!(index.len(), 0);
            assert!(index.capacity() >= 1000);

            // Add entries
            index.set(100, NodeRef::new(0, 10)).unwrap();
            index.set(101, NodeRef::new(0, 20)).unwrap();
            index.set(105, NodeRef::new(1, 5)).unwrap();

            assert_eq!(index.len(), 6); // 100..105 inclusive
            index.flush().unwrap();
        }

        // Open
        {
            let index = VocabReverseIndex::open(&path).unwrap();
            assert_eq!(index.start_index(), 100);
            assert_eq!(index.len(), 6);

            assert_eq!(index.get(100), Some(NodeRef::new(0, 10)));
            assert_eq!(index.get(101), Some(NodeRef::new(0, 20)));
            assert_eq!(index.get(102), None); // NULL
            assert_eq!(index.get(105), Some(NodeRef::new(1, 5)));

            assert_eq!(index.get(99), None); // Below start_index
        }
    }

    #[test]
    fn test_reverse_index_growth() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test_grow.idx");

        // Note: create() uses max(initial_capacity, DEFAULT_INITIAL_CAPACITY=1024)
        let mut index = VocabReverseIndex::create(&path, 0, 10).unwrap();
        let initial_capacity = index.capacity();
        assert_eq!(
            initial_capacity, DEFAULT_INITIAL_CAPACITY,
            "Initial capacity should be DEFAULT_INITIAL_CAPACITY"
        );

        // Add entries beyond initial capacity (must exceed DEFAULT_INITIAL_CAPACITY)
        let count = (DEFAULT_INITIAL_CAPACITY + 500) as u64;
        for i in 0..count {
            index.set(i, NodeRef::new(0, i as u32)).unwrap();
        }

        assert!(
            index.capacity() > initial_capacity,
            "Capacity should have grown"
        );
        assert_eq!(index.len(), count);

        // Verify all entries
        for i in 0..count {
            assert_eq!(index.get(i), Some(NodeRef::new(0, i as u32)));
        }
    }

    #[test]
    fn test_reverse_index_iter() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test_iter.idx");

        let mut index = VocabReverseIndex::create(&path, 10, 100).unwrap();
        index.set(10, NodeRef::new(0, 1)).unwrap();
        index.set(12, NodeRef::new(0, 2)).unwrap();
        index.set(15, NodeRef::new(0, 3)).unwrap();

        let entries: Vec<_> = index.iter().collect();
        assert_eq!(entries.len(), 3);
        assert!(entries.contains(&(10, NodeRef::new(0, 1))));
        assert!(entries.contains(&(12, NodeRef::new(0, 2))));
        assert!(entries.contains(&(15, NodeRef::new(0, 3))));
    }
}
