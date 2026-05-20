//! File-header layout for disk-backed char tries.
//!
//! Split out of char `dict_impl_char.rs` (lines ~105-332) as part of the
//! Phase-6 decomposition. Holds the `CharTrieFileHeader` struct, its V1/V2
//! layout helpers (V1 = no checksum, V2 = CRC32 over bytes 0-31), and the
//! file-local `crc32_header` helper used by checksum compute/verify paths.
//!
//! The associated magic / size / version constants
//! (`CHAR_TRIE_MAGIC`, `CHAR_FILE_HEADER_SIZE`, `CHAR_HEADER_VERSION_V1`,
//! `CHAR_HEADER_VERSION_V2`) remain in `dict_impl_char.rs` so the
//! many internal consumers there do not have to re-route through this
//! sub-module.

use super::dict_impl_char::{
    CHAR_FILE_HEADER_SIZE, CHAR_HEADER_VERSION_V1, CHAR_HEADER_VERSION_V2, CHAR_TRIE_MAGIC,
};
use crate::persistent_artrie::error::{PersistentARTrieError, Result};

/// File header for disk-backed char trie
///
/// # Layout (64 bytes total)
///
/// ```text
/// Offset  Size  Field
/// ------  ----  -----
///   0       4   magic ("ARTC")
///   4       1   version (1 = no checksum, 2 = with checksum)
///   5       3   reserved
///   8       8   root_ptr (block ID of root node)
///  16       8   entry_count
///  24       8   checkpoint_lsn
///  32       4   header_checksum (V2+: CRC32 of bytes 0-31)
///  36      28   padding
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CharTrieFileHeader {
    /// Magic bytes "ARTC"
    pub magic: [u8; 4],
    /// Format version (1 = no checksum, 2 = with checksum)
    pub version: u8,
    /// Reserved bytes
    pub _reserved: [u8; 3],
    /// Root node pointer (block ID)
    pub root_ptr: u64,
    /// Number of entries in the trie
    pub entry_count: u64,
    /// Checkpoint LSN (for WAL truncation)
    pub checkpoint_lsn: u64,
    /// CRC32 checksum of bytes 0-31 (V2+ only, 0 for V1)
    pub header_checksum: u32,
    /// Padding to 64 bytes
    pub _padding: [u8; 28],
}

impl CharTrieFileHeader {
    /// Create a new file header (V2 format with checksum)
    pub fn new() -> Self {
        Self {
            magic: CHAR_TRIE_MAGIC,
            version: CHAR_HEADER_VERSION_V2,
            _reserved: [0; 3],
            root_ptr: 0,
            entry_count: 0,
            checkpoint_lsn: 0,
            header_checksum: 0,
            _padding: [0; 28],
        }
    }

    /// Create a V1 header (for backward compatibility testing)
    #[cfg(test)]
    pub fn new_v1() -> Self {
        Self {
            magic: CHAR_TRIE_MAGIC,
            version: CHAR_HEADER_VERSION_V1,
            _reserved: [0; 3],
            root_ptr: 0,
            entry_count: 0,
            checkpoint_lsn: 0,
            header_checksum: 0,
            _padding: [0; 28],
        }
    }

    /// Check if this header version supports checksums
    pub fn has_checksum(&self) -> bool {
        self.version >= CHAR_HEADER_VERSION_V2
    }

    /// Compute the header checksum (CRC32 of bytes 0-31)
    pub fn compute_checksum(&self) -> u32 {
        let mut bytes = [0u8; 32];
        bytes[0..4].copy_from_slice(&self.magic);
        bytes[4] = self.version;
        bytes[5..8].copy_from_slice(&self._reserved);
        bytes[8..16].copy_from_slice(&self.root_ptr.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.entry_count.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.checkpoint_lsn.to_le_bytes());
        crc32_header(&bytes)
    }

    /// Update the checksum to match current header values
    pub fn finalize_checksum(&mut self) {
        if self.has_checksum() {
            self.header_checksum = self.compute_checksum();
        }
    }

    /// Verify the header checksum
    ///
    /// Returns true if:
    /// - V1 header (no checksum, always valid)
    /// - V2+ header with matching checksum
    pub fn verify_checksum(&self) -> bool {
        if !self.has_checksum() {
            // V1 headers don't have checksums, consider valid
            return true;
        }
        self.header_checksum == self.compute_checksum()
    }

    /// Serialize to bytes (does NOT auto-finalize checksum)
    ///
    /// Call `finalize_checksum()` before serializing to ensure checksum is valid.
    pub fn to_bytes(&self) -> [u8; CHAR_FILE_HEADER_SIZE] {
        let mut bytes = [0u8; CHAR_FILE_HEADER_SIZE];
        bytes[0..4].copy_from_slice(&self.magic);
        bytes[4] = self.version;
        bytes[5..8].copy_from_slice(&self._reserved);
        bytes[8..16].copy_from_slice(&self.root_ptr.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.entry_count.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.checkpoint_lsn.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.header_checksum.to_le_bytes());
        bytes[36..64].copy_from_slice(&self._padding);
        bytes
    }

    /// Serialize to bytes with checksum finalization
    pub fn to_bytes_with_checksum(&mut self) -> [u8; CHAR_FILE_HEADER_SIZE] {
        self.finalize_checksum();
        self.to_bytes()
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8; CHAR_FILE_HEADER_SIZE]) -> Self {
        Self {
            magic: [bytes[0], bytes[1], bytes[2], bytes[3]],
            version: bytes[4],
            _reserved: [bytes[5], bytes[6], bytes[7]],
            root_ptr: u64::from_le_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11],
                bytes[12], bytes[13], bytes[14], bytes[15],
            ]),
            entry_count: u64::from_le_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19],
                bytes[20], bytes[21], bytes[22], bytes[23],
            ]),
            checkpoint_lsn: u64::from_le_bytes([
                bytes[24], bytes[25], bytes[26], bytes[27],
                bytes[28], bytes[29], bytes[30], bytes[31],
            ]),
            header_checksum: u32::from_le_bytes([
                bytes[32], bytes[33], bytes[34], bytes[35],
            ]),
            _padding: {
                let mut arr = [0u8; 28];
                arr.copy_from_slice(&bytes[36..64]);
                arr
            },
        }
    }

    /// Deserialize from bytes and verify checksum
    ///
    /// Returns `Err` if checksum verification fails (V2+ only).
    pub fn from_bytes_verified(bytes: &[u8; CHAR_FILE_HEADER_SIZE]) -> Result<Self> {
        let header = Self::from_bytes(bytes);
        if header.has_checksum() && !header.verify_checksum() {
            return Err(PersistentARTrieError::CorruptedFile {
                reason: format!(
                    "Header checksum mismatch: stored={:#x}, computed={:#x}",
                    header.header_checksum,
                    header.compute_checksum()
                ),
            });
        }
        Ok(header)
    }

    /// Validate the header (magic + version + checksum)
    pub fn validate(&self) -> Result<()> {
        if self.magic != CHAR_TRIE_MAGIC {
            // Convert [u8; 4] to u64 for the error type
            let expected = u64::from_le_bytes([
                CHAR_TRIE_MAGIC[0], CHAR_TRIE_MAGIC[1], CHAR_TRIE_MAGIC[2], CHAR_TRIE_MAGIC[3],
                0, 0, 0, 0,
            ]);
            let found = u64::from_le_bytes([
                self.magic[0], self.magic[1], self.magic[2], self.magic[3],
                0, 0, 0, 0,
            ]);
            return Err(PersistentARTrieError::InvalidMagic { expected, found });
        }
        if self.has_checksum() && !self.verify_checksum() {
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

    /// Upgrade V1 header to V2 format with checksum
    pub fn upgrade_to_v2(&mut self) {
        if self.version < CHAR_HEADER_VERSION_V2 {
            self.version = CHAR_HEADER_VERSION_V2;
            self.finalize_checksum();
        }
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

impl Default for CharTrieFileHeader {
    fn default() -> Self {
        Self::new()
    }
}
