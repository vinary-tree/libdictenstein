//! Type definitions for PersistentVocabARTrie.
//!
//! This module defines the core types for the vocabulary-specialized ARTrie:
//! - [`VocabTrieNode`]: Trie node with parent pointers for backtracking
//! - [`VocabTrieRoot`]: Root node type enum
//! - [`VocabTrieFileHeader`]: Extended 96-byte file header
//!
//! # Parent Pointer Design
//!
//! Unlike the generic `CharTrieNodeInner<V>`, `VocabTrieNode` includes parent
//! pointers to enable O(k) reverse lookups (index → term) via backtracking:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      VocabTrieNode                          │
//! ├─────────────────────────────────────────────────────────────┤
//! │  inner: CharNode        // Reuses existing ART internals    │
//! │  parent: NodeRef        // Parent for backtracking (8B)     │
//! │  parent_edge: u32       // Edge label from parent (4B)      │
//! │  value: Option<u64>     // Vocabulary index (inline)        │
//! └─────────────────────────────────────────────────────────────┘
//! ```

// Re-export NodeRef from persistent_artrie_char
pub use crate::persistent_artrie_char::types::NodeRef;

/// Magic bytes for vocab trie file: "VOCB"
pub const VOCAB_TRIE_MAGIC: [u8; 4] = *b"VOCB";

/// File header size in bytes (extended from 64 to 96 bytes)
pub const VOCAB_FILE_HEADER_SIZE: usize = 96;

/// Header format version 1 — the legacy OWNED image (`root_ptr` = a `VocabTrieNode` `ArenaSlot`).
pub const VOCAB_HEADER_VERSION_V1: u8 = 1;

/// Header format version 2 — the OVERLAY image (the V4 flip). `root_ptr` is the root NODE
/// `SwizzledPtr.to_raw()` of the dense char-arena overlay image (read back by
/// `enumerate_overlay_terms_from_disk`), NOT an owned `VocabTrieNode` `ArenaSlot`. The reverse
/// index is derived (rebuilt in memory on reopen), so `reverse_index_capacity` is 0. Legacy v1
/// (owned) files are REBUILT, not migrated (owner decision — the C2 precedent).
pub const VOCAB_HEADER_VERSION_V2: u8 = 2;

/// Default buffer pool size (number of pages)
pub const DEFAULT_VOCAB_BUFFER_POOL_SIZE: usize = 256;

/// Default LRU cache size for reverse lookups
pub const DEFAULT_REVERSE_CACHE_SIZE: usize = 50_000;

/// Extended file header for vocabulary trie (96 bytes).
///
/// # Layout
///
/// This header is designed to be partially compatible with FileHeader at key
/// positions used by DiskManager (especially block_count at bytes 24-28).
///
/// ```text
/// Offset  Size  Field
/// ------  ----  -----
///   0       4   magic ("VOCB")
///   4       1   version
///   5       3   reserved
///   8       8   root_ptr (arena slot of root node)
///  16       8   entry_count (number of vocabulary entries)
///  24       4   block_count (for DiskManager compatibility)
///  28       4   _pad1
///  32       8   checkpoint_lsn
///  40       4   header_checksum (CRC32 of bytes 0-39)
///  44      20   padding (base header ends at 64)
///  64       8   start_index (first vocabulary index)
///  72       8   next_index (next index to assign)
///  80       8   reverse_index_capacity
///  88       8   _ext_padding
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VocabTrieFileHeader {
    // === Base Header (64 bytes) ===
    /// Magic bytes "VOCB"
    pub magic: [u8; 4],
    /// Format version
    pub version: u8,
    /// Reserved bytes
    pub _reserved: [u8; 3],
    /// Root node pointer (arena slot as u64)
    pub root_ptr: u64,
    /// Number of entries in the vocabulary
    pub entry_count: u64,
    /// Block count (for DiskManager::allocate_block compatibility)
    pub block_count: u32,
    /// Padding for alignment
    pub _pad1: u32,
    /// Checkpoint LSN (for WAL truncation)
    pub checkpoint_lsn: u64,
    /// CRC32 checksum of bytes 0-39
    pub header_checksum: u32,
    /// Padding to 64 bytes
    pub _padding: [u8; 20],

    // === Extended Header (32 bytes) ===
    /// Starting index for vocabulary (configurable, default 0)
    pub start_index: u64,
    /// Next index to assign
    pub next_index: u64,
    /// Capacity of the reverse index file
    pub reverse_index_capacity: u64,
    /// Extended padding
    pub _ext_padding: [u8; 8],
}

impl VocabTrieFileHeader {
    /// Create a new file header
    pub fn new() -> Self {
        Self {
            magic: VOCAB_TRIE_MAGIC,
            version: VOCAB_HEADER_VERSION_V1,
            _reserved: [0; 3],
            root_ptr: 0,
            entry_count: 0,
            block_count: 1, // Block 0 is the header
            _pad1: 0,
            checkpoint_lsn: 0,
            header_checksum: 0,
            _padding: [0; 20],
            start_index: 0,
            next_index: 0,
            reverse_index_capacity: 0,
            _ext_padding: [0; 8],
        }
    }

    /// Create a new header with a custom start index
    pub fn with_start_index(start_index: u64) -> Self {
        let mut header = Self::new();
        header.start_index = start_index;
        header.next_index = start_index;
        header
    }

    /// Compute the header checksum (CRC32 of bytes 0-39)
    pub fn compute_checksum(&self) -> u32 {
        let mut bytes = [0u8; 40];
        bytes[0..4].copy_from_slice(&self.magic);
        bytes[4] = self.version;
        bytes[5..8].copy_from_slice(&self._reserved);
        bytes[8..16].copy_from_slice(&self.root_ptr.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.entry_count.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.block_count.to_le_bytes());
        bytes[28..32].copy_from_slice(&self._pad1.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.checkpoint_lsn.to_le_bytes());
        crc32_header(&bytes)
    }

    /// Update the checksum to match current header values
    pub fn finalize_checksum(&mut self) {
        self.header_checksum = self.compute_checksum();
    }

    /// Verify the header checksum
    pub fn verify_checksum(&self) -> bool {
        self.header_checksum == self.compute_checksum()
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> [u8; VOCAB_FILE_HEADER_SIZE] {
        let mut bytes = [0u8; VOCAB_FILE_HEADER_SIZE];
        // Base header (0-63)
        bytes[0..4].copy_from_slice(&self.magic);
        bytes[4] = self.version;
        bytes[5..8].copy_from_slice(&self._reserved);
        bytes[8..16].copy_from_slice(&self.root_ptr.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.entry_count.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.block_count.to_le_bytes());
        bytes[28..32].copy_from_slice(&self._pad1.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.checkpoint_lsn.to_le_bytes());
        bytes[40..44].copy_from_slice(&self.header_checksum.to_le_bytes());
        bytes[44..64].copy_from_slice(&self._padding);
        // Extended header (64-95)
        bytes[64..72].copy_from_slice(&self.start_index.to_le_bytes());
        bytes[72..80].copy_from_slice(&self.next_index.to_le_bytes());
        bytes[80..88].copy_from_slice(&self.reverse_index_capacity.to_le_bytes());
        bytes[88..96].copy_from_slice(&self._ext_padding);
        bytes
    }

    /// Serialize to bytes with checksum finalization
    pub fn to_bytes_with_checksum(&mut self) -> [u8; VOCAB_FILE_HEADER_SIZE] {
        self.finalize_checksum();
        self.to_bytes()
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8; VOCAB_FILE_HEADER_SIZE]) -> Self {
        Self {
            magic: [bytes[0], bytes[1], bytes[2], bytes[3]],
            version: bytes[4],
            _reserved: [bytes[5], bytes[6], bytes[7]],
            root_ptr: u64::from_le_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
            entry_count: u64::from_le_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22],
                bytes[23],
            ]),
            block_count: u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
            _pad1: u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]),
            checkpoint_lsn: u64::from_le_bytes([
                bytes[32], bytes[33], bytes[34], bytes[35], bytes[36], bytes[37], bytes[38],
                bytes[39],
            ]),
            header_checksum: u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]),
            _padding: {
                let mut arr = [0u8; 20];
                arr.copy_from_slice(&bytes[44..64]);
                arr
            },
            start_index: u64::from_le_bytes([
                bytes[64], bytes[65], bytes[66], bytes[67], bytes[68], bytes[69], bytes[70],
                bytes[71],
            ]),
            next_index: u64::from_le_bytes([
                bytes[72], bytes[73], bytes[74], bytes[75], bytes[76], bytes[77], bytes[78],
                bytes[79],
            ]),
            reverse_index_capacity: u64::from_le_bytes([
                bytes[80], bytes[81], bytes[82], bytes[83], bytes[84], bytes[85], bytes[86],
                bytes[87],
            ]),
            _ext_padding: {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&bytes[88..96]);
                arr
            },
        }
    }

    /// Validate the header (magic + checksum)
    pub fn validate(&self) -> crate::persistent_artrie::error::Result<()> {
        use crate::persistent_artrie::error::PersistentARTrieError;

        if self.magic != VOCAB_TRIE_MAGIC {
            let expected = u64::from_le_bytes([
                VOCAB_TRIE_MAGIC[0],
                VOCAB_TRIE_MAGIC[1],
                VOCAB_TRIE_MAGIC[2],
                VOCAB_TRIE_MAGIC[3],
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
        if !self.verify_checksum() {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: format!(
                    "Header checksum mismatch: stored={:#x}, computed={:#x}",
                    self.header_checksum,
                    self.compute_checksum()
                ),
            });
        }
        Ok(())
    }
}

impl Default for VocabTrieFileHeader {
    fn default() -> Self {
        Self::new()
    }
}

/// CRC32 checksum (IEEE polynomial) for header integrity verification
fn crc32_header(data: &[u8]) -> u32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vocab_trie_file_header() {
        let mut header = VocabTrieFileHeader::new();
        header.root_ptr = 42;
        header.entry_count = 100;
        header.start_index = 5;
        header.next_index = 105;
        header.reverse_index_capacity = 1000;

        let bytes = header.to_bytes_with_checksum();
        let header2 = VocabTrieFileHeader::from_bytes(&bytes);

        assert_eq!(header2.magic, VOCAB_TRIE_MAGIC);
        assert_eq!(header2.root_ptr, 42);
        assert_eq!(header2.entry_count, 100);
        assert_eq!(header2.start_index, 5);
        assert_eq!(header2.next_index, 105);
        assert_eq!(header2.reverse_index_capacity, 1000);
        assert!(header2.verify_checksum());
    }
}
