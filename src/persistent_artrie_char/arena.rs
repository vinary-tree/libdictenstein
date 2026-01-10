//! CharNodeArena - Slotted page for space-efficient char node storage
//!
//! This module provides arena-based allocation for CharTrieNode serialization,
//! packing multiple small nodes into a single 256KB block instead of wasting
//! one 256KB block per node.
//!
//! ## Page Layout
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │  ArenaHeader (64 bytes)                                        │
//! │  - magic: u64 ("CHARARNA")                                     │
//! │  - version: u16                                                │
//! │  - flags: u16                                                  │
//! │  - node_count: u32 (number of nodes in this arena)            │
//! │  - free_offset: u32 (next allocation offset, grows up)        │
//! │  - directory_start: u32 (directory start, grows down)         │
//! │  - checksum: u32                                               │
//! │  - reserved: [u8; 28]                                          │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  Data Area (grows upward from offset 64)                       │
//! │  - Node[0]: [serialized CharNode bytes...]                     │
//! │  - Node[1]: [serialized CharNode bytes...]                     │
//! │  - ...                                                         │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  Free Space                                                    │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  Directory (grows downward from block end)                     │
//! │  - Slot[n-1]: [offset: u32, len: u32]                         │
//! │  - Slot[n-2]: [offset: u32, len: u32]                         │
//! │  - ...                                                         │
//! │  - Slot[0]: [offset: u32, len: u32]                           │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

use super::compact_encoding::{read_varint_from_slice, varint_size, write_varint_to_vec};
use crate::persistent_artrie::disk_manager::BLOCK_SIZE;
use crate::persistent_artrie::PersistentARTrieError;

type Result<T> = std::result::Result<T, PersistentARTrieError>;

/// Magic number for arena identification: "CHARARNA"
pub const ARENA_MAGIC: u64 = 0x414E5241524148_43; // "CHARARNA" in little-endian

/// Magic number for V2 arena with varint directory: "CHARARV2"
pub const ARENA_MAGIC_V2: u64 = 0x32564152_4148_43; // "CHARARV2" in little-endian

/// Current arena format version
pub const ARENA_VERSION: u16 = 1;

/// Arena format version 2 with varint directory
pub const ARENA_VERSION_V2: u16 = 2;

/// Header size in bytes
pub const HEADER_SIZE: usize = 64;

/// Slot entry size in bytes (offset: u32 + len: u32) for V1 format
pub const SLOT_SIZE: usize = 8;

/// Minimum free space to keep (prevents fragmentation)
pub const MIN_FREE_SPACE: usize = 64;

/// Flag indicating varint directory format
pub const FLAG_VARINT_DIRECTORY: u16 = 0x0001;

/// Arena header structure (64 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ArenaHeader {
    /// Magic number for identification
    pub magic: u64,
    /// Format version
    pub version: u16,
    /// Flags (reserved for future use)
    pub flags: u16,
    /// Number of nodes stored in this arena
    pub node_count: u32,
    /// Offset of next free byte in data area (grows upward)
    pub free_offset: u32,
    /// Offset where directory starts (grows downward from block end)
    pub directory_start: u32,
    /// CRC32 checksum of arena data
    pub checksum: u32,
    /// Reserved for future use
    pub reserved: [u8; 28],
}

impl ArenaHeader {
    /// Create a new arena header
    pub fn new(block_size: usize) -> Self {
        Self {
            magic: ARENA_MAGIC,
            version: ARENA_VERSION,
            flags: 0,
            node_count: 0,
            free_offset: HEADER_SIZE as u32,
            directory_start: block_size as u32,
            checksum: 0,
            reserved: [0u8; 28],
        }
    }

    /// Read header from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_SIZE {
            return Err(PersistentARTrieError::corrupted(
                "Arena header too small",
            ));
        }

        let magic = u64::from_le_bytes(bytes[0..8].try_into().expect("8 bytes"));
        if magic != ARENA_MAGIC && magic != ARENA_MAGIC_V2 {
            return Err(PersistentARTrieError::corrupted(&format!(
                "Invalid arena magic: expected {:016x} or {:016x}, got {:016x}",
                ARENA_MAGIC, ARENA_MAGIC_V2, magic
            )));
        }

        let version = u16::from_le_bytes(bytes[8..10].try_into().expect("2 bytes"));
        if version != ARENA_VERSION && version != ARENA_VERSION_V2 {
            return Err(PersistentARTrieError::corrupted(&format!(
                "Unsupported arena version: {}",
                version
            )));
        }

        let flags = u16::from_le_bytes(bytes[10..12].try_into().expect("2 bytes"));
        let node_count = u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes"));
        let free_offset = u32::from_le_bytes(bytes[16..20].try_into().expect("4 bytes"));
        let directory_start = u32::from_le_bytes(bytes[20..24].try_into().expect("4 bytes"));
        let checksum = u32::from_le_bytes(bytes[24..28].try_into().expect("4 bytes"));

        let mut reserved = [0u8; 28];
        reserved.copy_from_slice(&bytes[28..56]);

        Ok(Self {
            magic,
            version,
            flags,
            node_count,
            free_offset,
            directory_start,
            checksum,
            reserved,
        })
    }

    /// Write header to bytes
    pub fn to_bytes(&self, out: &mut [u8]) {
        out[0..8].copy_from_slice(&self.magic.to_le_bytes());
        out[8..10].copy_from_slice(&self.version.to_le_bytes());
        out[10..12].copy_from_slice(&self.flags.to_le_bytes());
        out[12..16].copy_from_slice(&self.node_count.to_le_bytes());
        out[16..20].copy_from_slice(&self.free_offset.to_le_bytes());
        out[20..24].copy_from_slice(&self.directory_start.to_le_bytes());
        out[24..28].copy_from_slice(&self.checksum.to_le_bytes());
        out[28..56].copy_from_slice(&self.reserved);
    }

    /// Calculate available space for allocation
    pub fn available_space(&self) -> usize {
        if self.directory_start <= self.free_offset {
            0
        } else {
            (self.directory_start - self.free_offset) as usize
        }
    }
}

/// Slot entry in the directory (8 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SlotEntry {
    /// Offset of the data in the arena (from start of block)
    pub offset: u32,
    /// Length of the data in bytes
    pub len: u32,
}

impl SlotEntry {
    pub fn new(offset: u32, len: u32) -> Self {
        Self { offset, len }
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let offset = u32::from_le_bytes(bytes[0..4].try_into().expect("4 bytes"));
        let len = u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes"));
        Self { offset, len }
    }

    pub fn to_bytes(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.offset.to_le_bytes());
        out[4..8].copy_from_slice(&self.len.to_le_bytes());
    }
}

/// CharNodeArena - A slotted page that packs multiple CharNodes
///
/// This arena uses bump allocation for data (grows upward) and
/// a directory of slots (grows downward) to track allocations.
pub struct CharNodeArena {
    /// The raw data buffer (typically BLOCK_SIZE = 256KB)
    data: Vec<u8>,
    /// Cached header for fast access
    header: ArenaHeader,
    /// Block ID if persisted to disk
    pub block_id: Option<u32>,
    /// Dirty flag - true if modified since last sync
    dirty: bool,
    /// Maximum data offset seen (for compact encoding ptr_width selection)
    max_data_offset: u32,
}

impl CharNodeArena {
    /// Create a new empty arena with the specified size
    pub fn new(size: usize) -> Self {
        let mut data = vec![0u8; size];
        let header = ArenaHeader::new(size);
        header.to_bytes(&mut data[0..HEADER_SIZE]);

        Self {
            data,
            header,
            block_id: None,
            dirty: true,
            max_data_offset: HEADER_SIZE as u32,
        }
    }

    /// Create a new arena with default BLOCK_SIZE
    pub fn new_default() -> Self {
        Self::new(BLOCK_SIZE)
    }

    /// Load an arena from raw bytes
    pub fn from_bytes(bytes: &[u8], block_id: u32) -> Result<Self> {
        if bytes.len() < HEADER_SIZE {
            return Err(PersistentARTrieError::corrupted("Arena data too small"));
        }

        let header = ArenaHeader::from_bytes(bytes)?;
        let data = bytes.to_vec();

        // Calculate max_data_offset from the header
        let max_data_offset = header.free_offset;

        Ok(Self {
            data,
            header,
            block_id: Some(block_id),
            dirty: false,
            max_data_offset,
        })
    }

    /// Get the raw bytes of this arena
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Get mutable raw bytes (marks arena as dirty)
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        self.dirty = true;
        &mut self.data
    }

    /// Check if this arena can fit an allocation of the given size
    pub fn can_allocate(&self, size: usize) -> bool {
        // Need space for data + slot entry + minimum free space
        let needed = size + SLOT_SIZE + MIN_FREE_SPACE;
        self.header.available_space() >= needed
    }

    /// Allocate space for data and return the slot ID
    ///
    /// Returns `None` if there isn't enough space.
    pub fn allocate(&mut self, data: &[u8]) -> Option<u32> {
        let len = data.len();
        if !self.can_allocate(len) {
            return None;
        }

        // Allocate data space (bump upward)
        let data_offset = self.header.free_offset;
        self.data[data_offset as usize..(data_offset as usize + len)].copy_from_slice(data);
        self.header.free_offset += len as u32;

        // Track max data offset for compact encoding
        if self.header.free_offset > self.max_data_offset {
            self.max_data_offset = self.header.free_offset;
        }

        // Allocate slot entry (grow downward)
        self.header.directory_start -= SLOT_SIZE as u32;
        let slot_offset = self.header.directory_start as usize;
        let slot = SlotEntry::new(data_offset, len as u32);
        slot.to_bytes(&mut self.data[slot_offset..slot_offset + SLOT_SIZE]);

        // Update node count
        let slot_id = self.header.node_count;
        self.header.node_count += 1;

        // Write updated header
        self.header.to_bytes(&mut self.data[0..HEADER_SIZE]);
        self.dirty = true;

        Some(slot_id)
    }

    /// Read data for a given slot ID
    pub fn read(&self, slot_id: u32) -> Result<&[u8]> {
        if slot_id >= self.header.node_count {
            return Err(PersistentARTrieError::corrupted(&format!(
                "Invalid slot ID {} (arena has {} nodes)",
                slot_id, self.header.node_count
            )));
        }

        // Directory grows downward, so slot N is at:
        // block_end - (N + 1) * SLOT_SIZE
        let slot_offset = self.data.len() - ((slot_id as usize + 1) * SLOT_SIZE);
        let slot = SlotEntry::from_bytes(&self.data[slot_offset..slot_offset + SLOT_SIZE]);

        let start = slot.offset as usize;
        let end = start + slot.len as usize;

        if end > self.data.len() {
            return Err(PersistentARTrieError::corrupted(&format!(
                "Slot {} points outside arena: offset={}, len={}",
                slot_id, slot.offset, slot.len
            )));
        }

        Ok(&self.data[start..end])
    }

    /// Update data at the specified slot
    ///
    /// The new data must be exactly the same size as the original allocation.
    /// This is used for correcting relative encoding after arena overflow detection.
    pub fn update(&mut self, slot_id: u32, new_data: &[u8]) -> Result<()> {
        if slot_id >= self.header.node_count {
            return Err(PersistentARTrieError::corrupted(&format!(
                "Invalid slot ID {} (arena has {} nodes)",
                slot_id, self.header.node_count
            )));
        }

        // Directory grows downward, so slot N is at:
        // block_end - (N + 1) * SLOT_SIZE
        let slot_offset = self.data.len() - ((slot_id as usize + 1) * SLOT_SIZE);
        let slot = SlotEntry::from_bytes(&self.data[slot_offset..slot_offset + SLOT_SIZE]);

        let start = slot.offset as usize;
        let original_len = slot.len as usize;

        if new_data.len() != original_len {
            return Err(PersistentARTrieError::internal(&format!(
                "Update size mismatch: original={}, new={}",
                original_len, new_data.len()
            )));
        }

        let end = start + original_len;
        if end > self.data.len() {
            return Err(PersistentARTrieError::corrupted(&format!(
                "Slot {} points outside arena: offset={}, len={}",
                slot_id, slot.offset, slot.len
            )));
        }

        self.data[start..end].copy_from_slice(new_data);
        self.dirty = true;
        Ok(())
    }

    /// Get the number of nodes in this arena
    pub fn node_count(&self) -> u32 {
        self.header.node_count
    }

    /// Get available space in bytes
    pub fn available_space(&self) -> usize {
        self.header.available_space()
    }

    /// Check if arena is dirty (modified since last sync)
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark arena as clean (after syncing to disk)
    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    /// Get the size of this arena in bytes
    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// Set the block ID for this arena
    pub fn set_block_id(&mut self, block_id: u32) {
        self.block_id = Some(block_id);
    }

    /// Get the maximum data offset seen in this arena
    ///
    /// This is useful for determining the optimal ptr_width when using
    /// compact variable-width encoding.
    pub fn max_data_offset(&self) -> u32 {
        self.max_data_offset
    }

    /// Get the current free offset (next allocation position)
    pub fn free_offset(&self) -> u32 {
        self.header.free_offset
    }
}

// =============================================================================
// V2 Arena with Varint Slot Directory
// =============================================================================

/// Varint-encoded slot entry for CharNodeArenaV2
///
/// Instead of fixed 8-byte (offset: u32, len: u32), uses varint encoding:
/// - Small offsets (0-247): 1 byte each
/// - Larger offsets: 2-9 bytes each
///
/// Typical savings: 40-60% on slot directory overhead
#[derive(Debug, Clone, Copy)]
pub struct VarintSlotEntry {
    /// Offset of the data in the arena (from start of block)
    pub offset: u64,
    /// Length of the data in bytes
    pub len: u64,
}

impl VarintSlotEntry {
    pub fn new(offset: u64, len: u64) -> Self {
        Self { offset, len }
    }

    /// Calculate the encoded size of this entry
    pub fn encoded_size(&self) -> usize {
        varint_size(self.offset) + varint_size(self.len)
    }

    /// Write to a Vec using varint encoding
    pub fn write_to_vec(&self, out: &mut Vec<u8>) {
        write_varint_to_vec(self.offset, out);
        write_varint_to_vec(self.len, out);
    }

    /// Read from a byte slice, returns entry and bytes consumed
    pub fn read_from_slice(data: &[u8]) -> (Self, usize) {
        let (offset, consumed1) = read_varint_from_slice(data);
        let (len, consumed2) = read_varint_from_slice(&data[consumed1..]);
        (Self { offset, len }, consumed1 + consumed2)
    }
}

/// CharNodeArenaV2 - Arena with varint-encoded slot directory
///
/// V2 format stores the slot directory as a contiguous varint-encoded stream
/// at the end of the data area, rather than fixed-size entries growing downward.
pub struct CharNodeArenaV2 {
    /// The raw data buffer
    data: Vec<u8>,
    /// Cached header for fast access
    header: ArenaHeader,
    /// In-memory slot directory (offset, len) pairs
    slots: Vec<VarintSlotEntry>,
    /// Block ID if persisted to disk
    pub block_id: Option<u32>,
    /// Dirty flag
    dirty: bool,
    /// Maximum data offset seen
    max_data_offset: u32,
    /// Current data write position (grows upward from HEADER_SIZE)
    data_end: usize,
}

impl CharNodeArenaV2 {
    /// Create a new V2 arena
    pub fn new(size: usize) -> Self {
        let mut data = vec![0u8; size];
        let mut header = ArenaHeader::new(size);
        header.magic = ARENA_MAGIC_V2;
        header.version = ARENA_VERSION_V2;
        header.flags = FLAG_VARINT_DIRECTORY;
        header.to_bytes(&mut data[0..HEADER_SIZE]);

        Self {
            data,
            header,
            slots: Vec::new(),
            block_id: None,
            dirty: true,
            max_data_offset: HEADER_SIZE as u32,
            data_end: HEADER_SIZE,
        }
    }

    /// Create a new V2 arena with default BLOCK_SIZE
    pub fn new_default() -> Self {
        Self::new(BLOCK_SIZE)
    }

    /// Check if this arena can fit an allocation
    pub fn can_allocate(&self, size: usize) -> bool {
        let slot_overhead = 18 + MIN_FREE_SPACE;
        let needed = size + slot_overhead;
        let available = self.data.len() - self.data_end;
        available >= needed
    }

    /// Allocate space for data and return the slot ID
    pub fn allocate(&mut self, node_data: &[u8]) -> Option<u32> {
        let len = node_data.len();
        if !self.can_allocate(len) {
            return None;
        }

        let offset = self.data_end;
        self.data[offset..offset + len].copy_from_slice(node_data);
        self.data_end += len;

        if self.data_end as u32 > self.max_data_offset {
            self.max_data_offset = self.data_end as u32;
        }

        let slot_id = self.slots.len() as u32;
        self.slots.push(VarintSlotEntry::new(offset as u64, len as u64));

        self.header.node_count = self.slots.len() as u32;
        self.header.free_offset = self.data_end as u32;
        self.dirty = true;

        Some(slot_id)
    }

    /// Read data for a given slot ID
    pub fn read(&self, slot_id: u32) -> Result<&[u8]> {
        let slot = self.slots.get(slot_id as usize).ok_or_else(|| {
            PersistentARTrieError::corrupted(&format!(
                "Invalid slot ID {} (arena has {} nodes)",
                slot_id,
                self.slots.len()
            ))
        })?;

        let start = slot.offset as usize;
        let end = start + slot.len as usize;

        if end > self.data.len() {
            return Err(PersistentARTrieError::corrupted(&format!(
                "Slot {} points outside arena: offset={}, len={}",
                slot_id, slot.offset, slot.len
            )));
        }

        Ok(&self.data[start..end])
    }

    /// Finalize the arena for persistence
    pub fn finalize(&mut self) {
        let mut directory = Vec::new();
        for slot in &self.slots {
            slot.write_to_vec(&mut directory);
        }

        let dir_start = self.data_end;
        let dir_end = dir_start + directory.len();

        if dir_end > self.data.len() {
            return;
        }

        self.data[dir_start..dir_end].copy_from_slice(&directory);
        self.header.directory_start = dir_start as u32;
        self.header.to_bytes(&mut self.data[0..HEADER_SIZE]);
    }

    /// Get the raw bytes of this arena (call finalize() first!)
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Load a V2 arena from raw bytes
    pub fn from_bytes(bytes: &[u8], block_id: u32) -> Result<Self> {
        if bytes.len() < HEADER_SIZE {
            return Err(PersistentARTrieError::corrupted("Arena data too small"));
        }

        let header = ArenaHeader::from_bytes(bytes)?;

        if header.magic != ARENA_MAGIC_V2 && header.magic != ARENA_MAGIC {
            return Err(PersistentARTrieError::corrupted(&format!(
                "Invalid V2 arena magic: {:016x}",
                header.magic
            )));
        }

        let mut slots = Vec::with_capacity(header.node_count as usize);
        let mut offset = header.directory_start as usize;

        for _ in 0..header.node_count {
            if offset >= bytes.len() {
                break;
            }
            let (entry, consumed) = VarintSlotEntry::read_from_slice(&bytes[offset..]);
            slots.push(entry);
            offset += consumed;
        }

        let data_end = header.free_offset as usize;
        let max_data_offset = header.free_offset;

        Ok(Self {
            data: bytes.to_vec(),
            header,
            slots,
            block_id: Some(block_id),
            dirty: false,
            max_data_offset,
            data_end,
        })
    }

    pub fn node_count(&self) -> u32 {
        self.slots.len() as u32
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    pub fn size(&self) -> usize {
        self.data.len()
    }

    pub fn set_block_id(&mut self, block_id: u32) {
        self.block_id = Some(block_id);
    }

    pub fn max_data_offset(&self) -> u32 {
        self.max_data_offset
    }

    pub fn available_space(&self) -> usize {
        if self.data_end >= self.data.len() {
            0
        } else {
            self.data.len() - self.data_end - MIN_FREE_SPACE
        }
    }

    /// Calculate directory space savings compared to fixed-size entries
    pub fn directory_savings(&self) -> (usize, usize) {
        let fixed_size = self.slots.len() * SLOT_SIZE;
        let varint_size: usize = self.slots.iter().map(|s| s.encoded_size()).sum();
        (fixed_size, varint_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arena_creation() {
        let arena = CharNodeArena::new(4096);
        assert_eq!(arena.node_count(), 0);
        assert!(arena.available_space() > 0);
        assert!(arena.is_dirty());
    }

    #[test]
    fn test_arena_allocation() {
        let mut arena = CharNodeArena::new(4096);

        // Allocate some data
        let data1 = b"hello world";
        let slot1 = arena.allocate(data1).expect("allocation should succeed");
        assert_eq!(slot1, 0);
        assert_eq!(arena.node_count(), 1);

        // Read it back
        let read1 = arena.read(slot1).expect("read should succeed");
        assert_eq!(read1, data1);

        // Allocate more data
        let data2 = b"goodbye world";
        let slot2 = arena.allocate(data2).expect("allocation should succeed");
        assert_eq!(slot2, 1);
        assert_eq!(arena.node_count(), 2);

        // Read both back
        let read1 = arena.read(slot1).expect("read should succeed");
        let read2 = arena.read(slot2).expect("read should succeed");
        assert_eq!(read1, data1);
        assert_eq!(read2, data2);
    }

    #[test]
    fn test_arena_serialization() {
        let mut arena = CharNodeArena::new(4096);

        let data1 = b"test data 1";
        let data2 = b"test data 2 longer";
        let slot1 = arena.allocate(data1).unwrap();
        let slot2 = arena.allocate(data2).unwrap();

        // Serialize and deserialize
        let bytes = arena.as_bytes().to_vec();
        let loaded = CharNodeArena::from_bytes(&bytes, 0).expect("load should succeed");

        assert_eq!(loaded.node_count(), 2);
        assert_eq!(loaded.read(slot1).unwrap(), data1);
        assert_eq!(loaded.read(slot2).unwrap(), data2);
    }

    #[test]
    fn test_arena_full() {
        let mut arena = CharNodeArena::new(256); // Small arena

        // Fill it up
        let mut allocated = 0;
        while arena.can_allocate(10) {
            arena.allocate(&[0u8; 10]).unwrap();
            allocated += 1;
        }

        assert!(allocated > 0);
        assert!(!arena.can_allocate(10));
    }

    // ==========================================================================
    // V2 Arena Tests
    // ==========================================================================

    #[test]
    fn test_arena_v2_creation() {
        let arena = CharNodeArenaV2::new(4096);
        assert_eq!(arena.node_count(), 0);
        assert!(arena.available_space() > 0);
        assert!(arena.is_dirty());
    }

    #[test]
    fn test_arena_v2_allocation() {
        let mut arena = CharNodeArenaV2::new(4096);

        // Allocate some data
        let data1 = b"hello world";
        let slot1 = arena.allocate(data1).expect("allocation should succeed");
        assert_eq!(slot1, 0);
        assert_eq!(arena.node_count(), 1);

        // Read it back
        let read1 = arena.read(slot1).expect("read should succeed");
        assert_eq!(read1, data1);

        // Allocate more data
        let data2 = b"goodbye world";
        let slot2 = arena.allocate(data2).expect("allocation should succeed");
        assert_eq!(slot2, 1);
        assert_eq!(arena.node_count(), 2);

        // Read both back
        let read1 = arena.read(slot1).expect("read should succeed");
        let read2 = arena.read(slot2).expect("read should succeed");
        assert_eq!(read1, data1);
        assert_eq!(read2, data2);
    }

    #[test]
    fn test_arena_v2_serialization() {
        let mut arena = CharNodeArenaV2::new(4096);

        let data1 = b"test data 1";
        let data2 = b"test data 2 longer";
        let slot1 = arena.allocate(data1).unwrap();
        let slot2 = arena.allocate(data2).unwrap();

        // Finalize and serialize
        arena.finalize();
        let bytes = arena.as_bytes().to_vec();
        let loaded = CharNodeArenaV2::from_bytes(&bytes, 0).expect("load should succeed");

        assert_eq!(loaded.node_count(), 2);
        assert_eq!(loaded.read(slot1).unwrap(), data1);
        assert_eq!(loaded.read(slot2).unwrap(), data2);
    }

    #[test]
    fn test_arena_v2_varint_savings() {
        let mut arena = CharNodeArenaV2::new(8192);

        // Allocate many small entries (typical case)
        for i in 0..100u8 {
            let data = vec![i; 50]; // 50-byte entries
            arena.allocate(&data).unwrap();
        }

        let (fixed, varint) = arena.directory_savings();
        // Fixed: 100 * 8 = 800 bytes
        // Varint: ~200-300 bytes (offset ~1 byte, len = 1 byte for 50)
        assert_eq!(fixed, 800);
        assert!(varint < fixed, "Varint should be smaller: {} vs {}", varint, fixed);
        println!("V2 directory savings: {} -> {} bytes ({:.1}% reduction)",
            fixed, varint, (1.0 - varint as f64 / fixed as f64) * 100.0);
    }

    #[test]
    fn test_arena_v2_full() {
        let mut arena = CharNodeArenaV2::new(512); // Small arena

        // Fill it up
        let mut allocated = 0;
        while arena.can_allocate(20) {
            arena.allocate(&[0u8; 20]).unwrap();
            allocated += 1;
        }

        assert!(allocated > 0);
        assert!(!arena.can_allocate(20));
    }

    #[test]
    fn test_varint_slot_entry() {
        // Test small values (single byte encoding)
        let entry1 = VarintSlotEntry::new(100, 50);
        assert_eq!(entry1.encoded_size(), 2); // Both fit in single byte

        // Test larger values
        let entry2 = VarintSlotEntry::new(300, 1000);
        assert!(entry2.encoded_size() > 2); // Multi-byte encoding

        // Round-trip test
        let mut buf = Vec::new();
        entry1.write_to_vec(&mut buf);
        entry2.write_to_vec(&mut buf);

        let (read1, consumed1) = VarintSlotEntry::read_from_slice(&buf);
        let (read2, _consumed2) = VarintSlotEntry::read_from_slice(&buf[consumed1..]);

        assert_eq!(read1.offset, entry1.offset);
        assert_eq!(read1.len, entry1.len);
        assert_eq!(read2.offset, entry2.offset);
        assert_eq!(read2.len, entry2.len);
    }
}
