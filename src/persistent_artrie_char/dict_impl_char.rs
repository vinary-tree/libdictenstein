//! Disk-backed implementation of PersistentARTrieChar.
//!
//! This module provides disk persistence for the character-level trie,
//! supporting:
//! - Memory-mapped file storage
//! - Write-ahead logging (WAL) for crash recovery
//! - Buffer management for efficient I/O
//!
//! # Architecture
//!
//! The disk layout uses the char ART nodes (CharNode4/16/48/CharBucket)
//! for efficient storage of Unicode character keys.
//!
//! # File Layout
//!
//! ```text
//! ┌─────────────────────────────────────────────────┐
//! │ File Header (64 bytes)                          │
//! │ - Magic: "ARTC" (ART Char)                      │
//! │ - Version: u8                                   │
//! │ - Root pointer: u64                             │
//! │ - Entry count: u64                              │
//! │ - Checkpoint LSN: u64                           │
//! └─────────────────────────────────────────────────┘
//! │ Root Node (variable)                            │
//! └─────────────────────────────────────────────────┘
//! │ Child Nodes...                                  │
//! └─────────────────────────────────────────────────┘
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(feature = "parking_lot")]
use crate::sync_compat::RwLock;
#[cfg(not(feature = "parking_lot"))]
use std::sync::RwLock;

// SwizzledPtr is used unconditionally for in-memory CharNode children
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

#[cfg(feature = "persistent-artrie")]
use crate::persistent_artrie::buffer_manager::BufferManager;
#[cfg(feature = "persistent-artrie")]
use crate::persistent_artrie::disk_manager::DiskManager;
#[cfg(feature = "persistent-artrie")]
use crate::persistent_artrie::error::{PersistentARTrieError, Result};
#[cfg(feature = "persistent-artrie")]
use crate::persistent_artrie::wal::{WalConfig, WalReader, WalRecord, WalWriter};
#[cfg(feature = "persistent-artrie")]
use crate::persistent_artrie::concurrency::{
    EpochManager, OptimisticVersion, RetryStats, EpochGuard, OptimisticReadGuard,
};
#[cfg(feature = "persistent-artrie")]
use super::arena_manager::ArenaManager;
use crate::value::DictionaryValue;

// Import CharNode types for adaptive radix structure
use super::nodes::CharNode;
#[cfg(feature = "persistent-artrie")]
use crate::persistent_artrie::NodeType;

/// Magic bytes for char trie file
pub const CHAR_TRIE_MAGIC: [u8; 4] = *b"ARTC";

/// File header size in bytes
pub const CHAR_FILE_HEADER_SIZE: usize = 64;

/// Header format version 1 (original, no checksum)
pub const CHAR_HEADER_VERSION_V1: u8 = 1;

/// Header format version 2 (with checksum for crash recovery)
pub const CHAR_HEADER_VERSION_V2: u8 = 2;

/// Default buffer pool size (number of pages)
pub const DEFAULT_CHAR_BUFFER_POOL_SIZE: usize = 256;

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
    #[cfg(feature = "persistent-artrie")]
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
    #[cfg(feature = "persistent-artrie")]
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

/// A trie node for the disk-backed char trie (CharNode-based implementation)
///
/// Uses adaptive CharNode types (CharNode4/16/48/CharBucket) for efficient
/// child storage. Each child is stored as a raw pointer to a heap-allocated
/// CharTrieNodeInner, with the pointer stored in the CharNode's child slots.
///
/// # Memory Layout
///
/// Children are stored as raw `*mut CharTrieNodeInner<V>` pointers within
/// the CharNode structure. This enables:
/// - Adaptive node sizing (N4 → N16 → N48 → Bucket as children grow)
/// - Efficient SIMD lookups for CharNode16
/// - Binary search for CharNode48
/// - HashMap for CharBucket (>48 children)
///
/// # Safety
///
/// The raw pointers are managed carefully:
/// - Created via `Box::into_raw()` when inserting children
/// - Recovered via `Box::from_raw()` when dropping or removing
/// - The `Drop` implementation ensures all children are properly freed
pub struct CharTrieNodeInner<V: DictionaryValue> {
    /// The adaptive radix node structure (N4/N16/N48/Bucket)
    /// Children are stored as raw pointers encoded in the CharNode's SwizzledPtr fields.
    pub node: CharNode,
    /// Optional value associated with this node (stored separately from CharNode)
    pub value: Option<V>,
}

// Manual Debug implementation to avoid requiring Debug on V
impl<V: DictionaryValue> std::fmt::Debug for CharTrieNodeInner<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CharTrieNodeInner")
            .field("is_final", &self.node.is_final())
            .field("children_count", &self.node.num_children())
            .field("has_value", &self.value.is_some())
            .finish()
    }
}

// Manual Clone implementation - deep clones all children
impl<V: DictionaryValue> Clone for CharTrieNodeInner<V> {
    fn clone(&self) -> Self {
        // Create a new node with the same type
        let mut new_node = match &self.node {
            CharNode::N4(_) => CharNode::new_node4(),
            CharNode::N16(_) => CharNode::new_node16(),
            CharNode::N48(_) => CharNode::new_node48(),
            CharNode::Bucket(_) => CharNode::new_bucket(),
        };

        // Copy the final flag
        new_node.header_mut().set_final(self.node.is_final());

        // Deep clone all children
        for (key, child_ptr) in self.node.iter_children() {
            if let Some(ptr) = child_ptr.as_ptr::<CharTrieNodeInner<V>>() {
                // Safety: ptr is valid because we control all SwizzledPtr creation
                let child_ref = unsafe { &*ptr };
                let cloned_child = Box::new(child_ref.clone());
                let cloned_ptr = SwizzledPtr::in_memory(Box::into_raw(cloned_child));
                // Add to new node (may cause growth, but since we're cloning
                // the same size node, we should have capacity)
                if let Err(_) = new_node.add_child_growing(key, cloned_ptr) {
                    // This shouldn't happen for clone, but handle gracefully
                    panic!("Failed to clone child during CharTrieNodeInner clone");
                }
            }
        }

        Self {
            node: new_node,
            value: self.value.clone(),
        }
    }
}

// Drop implementation - must free all child nodes
impl<V: DictionaryValue> Drop for CharTrieNodeInner<V> {
    fn drop(&mut self) {
        // Collect child pointers first to avoid iterator invalidation
        let child_ptrs: Vec<_> = self.node.iter_children()
            .filter_map(|(_, ptr)| ptr.as_ptr::<CharTrieNodeInner<V>>())
            .collect();

        // Free each child
        for ptr in child_ptrs {
            // Safety: We created these pointers via Box::into_raw() during insertion
            unsafe {
                drop(Box::from_raw(ptr as *mut CharTrieNodeInner<V>));
            }
        }
    }
}

impl<V: DictionaryValue> Default for CharTrieNodeInner<V> {
    fn default() -> Self {
        Self {
            node: CharNode::new_node4(), // Start with smallest node type
            value: None,
        }
    }
}

impl<V: DictionaryValue> CharTrieNodeInner<V> {
    /// Create a new empty node
    pub fn new() -> Self {
        Self::default()
    }

    /// Check if this node is final (accepting state)
    #[inline]
    pub fn is_final(&self) -> bool {
        self.node.is_final()
    }

    /// Set the final flag
    #[inline]
    pub fn set_final(&mut self, is_final: bool) {
        self.node.header_mut().set_final(is_final);
    }

    /// Get the number of children
    #[inline]
    pub fn num_children(&self) -> usize {
        self.node.num_children()
    }

    /// Get a child by character
    pub fn get_child(&self, c: char) -> Option<&CharTrieNodeInner<V>> {
        self.node.find_child(c as u32)
            .and_then(|ptr| ptr.as_ptr::<CharTrieNodeInner<V>>())
            .map(|ptr| {
                // Safety: We control all SwizzledPtr creation; ptr is valid
                unsafe { &*ptr }
            })
    }

    /// Get a child mutably by character
    pub fn get_child_mut(&mut self, c: char) -> Option<&mut CharTrieNodeInner<V>> {
        self.node.find_child(c as u32)
            .and_then(|ptr| ptr.as_ptr::<CharTrieNodeInner<V>>())
            .map(|ptr| {
                // Safety: We control all SwizzledPtr creation; ptr is valid
                // Note: This is technically unsound for shared access, but
                // the mutable borrow of self prevents concurrent access
                unsafe { &mut *(ptr as *mut CharTrieNodeInner<V>) }
            })
    }

    /// Insert a child, returning the old child if it existed
    pub fn insert_child(&mut self, c: char, child: CharTrieNodeInner<V>) -> Option<Box<CharTrieNodeInner<V>>> {
        let key = c as u32;

        // Check if child already exists
        if let Some(existing_ptr) = self.node.find_child(key) {
            if let Some(ptr) = existing_ptr.as_ptr::<CharTrieNodeInner<V>>() {
                // Remove old child and recover the Box
                if let Some((_, shrunk)) = self.node.remove_child_shrinking(key) {
                    if let Some(new_node) = shrunk {
                        self.node = new_node;
                    }
                }
                // Safety: ptr was created via Box::into_raw()
                let old_child = unsafe { Box::from_raw(ptr as *mut CharTrieNodeInner<V>) };

                // Insert the new child
                let new_ptr = SwizzledPtr::in_memory(Box::into_raw(Box::new(child)));
                if let Ok(Some(grown)) = self.node.add_child_growing(key, new_ptr) {
                    self.node = grown;
                }

                return Some(old_child);
            }
        }

        // No existing child, just insert
        let new_ptr = SwizzledPtr::in_memory(Box::into_raw(Box::new(child)));
        if let Ok(Some(grown)) = self.node.add_child_growing(key, new_ptr) {
            self.node = grown;
        }

        None
    }

    /// Insert a raw SwizzledPtr as a child, returning the old pointer if it existed
    ///
    /// This is used for lazy loading where we want to store on-disk pointers
    /// without immediately loading them into memory.
    ///
    /// # Safety Note
    ///
    /// If the returned SwizzledPtr is in-memory, the caller is responsible for
    /// freeing the pointed-to memory (e.g., by calling Box::from_raw on it).
    pub fn insert_child_ptr(&mut self, c: char, child_ptr: SwizzledPtr) -> Option<SwizzledPtr> {
        let key = c as u32;

        // Check if child already exists
        if let Some(_existing_ptr) = self.node.find_child(key) {
            // Remove old child and get its pointer
            if let Some((old_ptr, shrunk)) = self.node.remove_child_shrinking(key) {
                if let Some(new_node) = shrunk {
                    self.node = new_node;
                }

                // Insert the new child
                if let Ok(Some(grown)) = self.node.add_child_growing(key, child_ptr) {
                    self.node = grown;
                }

                return Some(old_ptr);
            }
        }

        // No existing child, just insert
        if let Ok(Some(grown)) = self.node.add_child_growing(key, child_ptr) {
            self.node = grown;
        }

        None
    }

    /// Get or create a child for the given character
    pub fn get_or_create_child(&mut self, c: char) -> &mut CharTrieNodeInner<V> {
        let key = c as u32;

        // Check if child already exists
        if self.node.find_child(key).is_some() {
            // Child exists, return mutable reference
            return self.get_child_mut(c).expect("child should exist");
        }

        // Create new child
        let new_child = Box::new(CharTrieNodeInner::new());
        let ptr = Box::into_raw(new_child);
        let swizzled = SwizzledPtr::in_memory(ptr);

        // Add to node, handling potential growth
        match self.node.add_child_growing(key, swizzled) {
            Ok(Some(grown)) => {
                self.node = grown;
            }
            Ok(None) => {
                // No growth needed
            }
            Err(_) => {
                // Key already exists (shouldn't happen, but handle gracefully)
                // Free the newly allocated child
                unsafe { drop(Box::from_raw(ptr)); }
                return self.get_child_mut(c).expect("child should exist");
            }
        }

        // Safety: We just inserted this pointer
        unsafe { &mut *ptr }
    }

    /// Remove a child by character, returning the removed child if it existed
    pub fn remove_child(&mut self, c: char) -> Option<Box<CharTrieNodeInner<V>>> {
        let key = c as u32;

        // Check if child exists and get its pointer
        let ptr = self.node.find_child(key)
            .and_then(|p| p.as_ptr::<CharTrieNodeInner<V>>())?;

        // Remove from node
        if let Some((_, shrunk)) = self.node.remove_child_shrinking(key) {
            if let Some(new_node) = shrunk {
                self.node = new_node;
            }
        }

        // Safety: ptr was created via Box::into_raw()
        Some(unsafe { Box::from_raw(ptr as *mut CharTrieNodeInner<V>) })
    }

    /// Iterate over children
    ///
    /// Returns an iterator over (char, &CharTrieNodeInner<V>) pairs.
    pub fn iter_children(&self) -> impl Iterator<Item = (char, &CharTrieNodeInner<V>)> {
        self.node.iter_children()
            .filter_map(|(key, ptr)| {
                ptr.as_ptr::<CharTrieNodeInner<V>>()
                    .map(|p| {
                        let c = char::from_u32(key).unwrap_or('\u{FFFD}');
                        // Safety: We control all SwizzledPtr creation; ptr is valid
                        let child_ref = unsafe { &*p };
                        (c, child_ref)
                    })
            })
    }
}

/// Root node type for disk-backed char trie
pub enum CharTrieRoot<V: DictionaryValue> {
    /// Empty trie (no root yet)
    Empty,
    /// Root is a trie node
    Node(Box<CharTrieNodeInner<V>>),
}

// Manual Debug implementation to avoid requiring Debug on V
impl<V: DictionaryValue> std::fmt::Debug for CharTrieRoot<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CharTrieRoot::Empty => write!(f, "CharTrieRoot::Empty"),
            CharTrieRoot::Node(node) => write!(f, "CharTrieRoot::Node({} children)", node.num_children()),
        }
    }
}

/// Inner state for disk-backed PersistentARTrieChar
pub struct DiskBackedCharTrieInner<V: DictionaryValue> {
    /// Root of the trie
    pub root: CharTrieRoot<V>,
    /// Number of terms
    pub len: usize,
    /// Dirty flag (has unsaved changes)
    pub dirty: bool,

    // Storage infrastructure (optional - None for in-memory mode)
    #[cfg(feature = "persistent-artrie")]
    pub buffer_manager: Option<Arc<RwLock<BufferManager>>>,
    #[cfg(feature = "persistent-artrie")]
    pub wal_writer: Option<Arc<RwLock<WalWriter>>>,
    #[cfg(feature = "persistent-artrie")]
    /// WAL configuration (archive mode, segment limits, etc.)
    pub wal_config: WalConfig,
    #[cfg(feature = "persistent-artrie")]
    pub next_lsn: u64,
    #[cfg(feature = "persistent-artrie")]
    pub file_path: Option<PathBuf>,
    /// Arena manager for space-efficient node storage
    /// Packs multiple nodes into 256KB blocks instead of one node per block
    #[cfg(feature = "persistent-artrie")]
    pub arena_manager: Option<Arc<RwLock<ArenaManager>>>,

    // Concurrency infrastructure
    #[cfg(feature = "persistent-artrie")]
    /// Version for optimistic concurrency control
    pub version: OptimisticVersion,
    #[cfg(feature = "persistent-artrie")]
    /// Epoch manager for safe memory reclamation
    pub epoch_manager: EpochManager,
    #[cfg(feature = "persistent-artrie")]
    /// Retry statistics for monitoring
    pub retry_stats: RetryStats,

    /// Phantom for value type
    _phantom: std::marker::PhantomData<V>,
}

// Manual Debug implementation to avoid requiring Debug on BufferManager and WalWriter
impl<V: DictionaryValue> std::fmt::Debug for DiskBackedCharTrieInner<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskBackedCharTrieInner")
            .field("root", &self.root)
            .field("len", &self.len)
            .field("dirty", &self.dirty)
            .finish_non_exhaustive()
    }
}

impl<V: DictionaryValue> DiskBackedCharTrieInner<V> {
    /// Create a new empty trie (in-memory mode)
    pub fn new() -> Self {
        Self {
            root: CharTrieRoot::Empty,
            len: 0,
            dirty: false,
            #[cfg(feature = "persistent-artrie")]
            buffer_manager: None,
            #[cfg(feature = "persistent-artrie")]
            wal_writer: None,
            #[cfg(feature = "persistent-artrie")]
            wal_config: WalConfig::default(),
            #[cfg(feature = "persistent-artrie")]
            next_lsn: 1,
            #[cfg(feature = "persistent-artrie")]
            file_path: None,
            #[cfg(feature = "persistent-artrie")]
            arena_manager: None,
            #[cfg(feature = "persistent-artrie")]
            version: OptimisticVersion::new(),
            #[cfg(feature = "persistent-artrie")]
            epoch_manager: EpochManager::new(),
            #[cfg(feature = "persistent-artrie")]
            retry_stats: RetryStats::new(),
            _phantom: std::marker::PhantomData,
        }
    }

    /// Create a new disk-backed trie at the given path
    #[cfg(feature = "persistent-artrie")]
    pub fn create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        // Create disk manager
        let disk_manager = DiskManager::create(path)?;

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_CHAR_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create WAL file
        let wal_path = path.with_extension("wal");
        let wal_writer = WalWriter::create(&wal_path)
            .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
        let wal_writer = Arc::new(RwLock::new(wal_writer));

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        Ok(Self {
            root: CharTrieRoot::Empty,
            len: 0,
            dirty: false,
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn: 1,
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: EpochManager::new(),
            retry_stats: RetryStats::new(),
            _phantom: std::marker::PhantomData,
        })
    }

    /// Create a new disk-backed trie with custom WAL configuration
    #[cfg(feature = "persistent-artrie")]
    pub fn create_with_config<P: AsRef<Path>>(path: P, wal_config: WalConfig) -> Result<Self> {
        let path = path.as_ref();

        // Create disk manager
        let disk_manager = DiskManager::create(path)?;

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_CHAR_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Create WAL file
        let wal_path = path.with_extension("wal");
        let wal_writer = WalWriter::create(&wal_path)
            .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
        let wal_writer = Arc::new(RwLock::new(wal_writer));

        // Create archive directory if archive mode is enabled
        if wal_config.archive_enabled {
            let archive_dir = path.parent().unwrap_or(Path::new(".")).join(&wal_config.archive_dir);
            if !archive_dir.exists() {
                std::fs::create_dir_all(&archive_dir).map_err(|e| {
                    PersistentARTrieError::io_error("create archive directory", archive_dir.display().to_string(), e)
                })?;
            }
        }

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        Ok(Self {
            root: CharTrieRoot::Empty,
            len: 0,
            dirty: false,
            buffer_manager: Some(buffer_manager),
            wal_writer: Some(wal_writer),
            wal_config,
            next_lsn: 1,
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: EpochManager::new(),
            retry_stats: RetryStats::new(),
            _phantom: std::marker::PhantomData,
        })
    }

    /// Open an existing disk-backed trie
    #[cfg(feature = "persistent-artrie")]
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

        let path = path.as_ref();

        // Open disk manager
        let disk_manager = DiskManager::open(path)?;

        // Read root pointer and entry count from header
        let root_ptr = disk_manager.root_ptr()?;
        let entry_count = disk_manager.entry_count()?;

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_CHAR_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Open or create WAL file
        let wal_path = path.with_extension("wal");
        let (wal_writer, recovered_ops, next_lsn, checkpoint_lsn) = if wal_path.exists() {
            // Recover from WAL
            let mut reader = WalReader::new(&wal_path)
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;

            let mut records = Vec::new();
            let mut max_lsn = 0u64;
            let mut checkpoint_lsn = 0u64;
            while let Some(result) = reader.next_record() {
                match result {
                    Ok((lsn, record)) => {
                        max_lsn = max_lsn.max(lsn);
                        // Track the latest checkpoint LSN
                        if let WalRecord::Checkpoint { checkpoint_lsn: cp_lsn, .. } = &record {
                            checkpoint_lsn = checkpoint_lsn.max(*cp_lsn);
                        }
                        records.push((lsn, record));
                    }
                    Err(_) => break, // Stop on error
                }
            }

            let next_lsn = max_lsn + 1;
            let writer = WalWriter::open(&wal_path)
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;

            (writer, records, next_lsn, checkpoint_lsn)
        } else {
            let writer = WalWriter::create(&wal_path)
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            (writer, Vec::new(), 1, 0)
        };

        let wal_writer = Arc::new(RwLock::new(wal_writer));

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        let mut inner = Self {
            root: CharTrieRoot::Empty,
            len: 0, // Will be updated from disk or WAL replay
            dirty: false,
            buffer_manager: Some(buffer_manager.clone()),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn,
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: EpochManager::new(),
            retry_stats: RetryStats::new(),
            _phantom: std::marker::PhantomData,
        };

        // Try to load root from disk if root_ptr != 0
        // Default: lazy loading (eager_depth = None)
        let mut loaded_from_disk = false;
        if root_ptr != 0 {
            let root_swizzled = SwizzledPtr::from_raw(root_ptr);
            match inner.load_root_from_disk(&buffer_manager, &root_swizzled, None) {
                Ok((root, len)) => {
                    inner.root = root;
                    inner.len = len;
                    loaded_from_disk = true;
                }
                Err(e) => {
                    eprintln!("Warning: Failed to load root from disk: {:?}", e);
                    // Fall back to WAL replay
                }
            }
        }

        // Replay WAL records that came after the checkpoint
        // Skip records with LSN <= checkpoint_lsn (already persisted to disk)
        let mut skipped_all = true;
        for (lsn, record) in recovered_ops {
            // Skip if we loaded from disk and this record is from before checkpoint
            if loaded_from_disk && checkpoint_lsn > 0 && lsn <= checkpoint_lsn {
                continue;
            }
            skipped_all = false;

            match record {
                WalRecord::Insert { term, .. } => {
                    let term_str = String::from_utf8_lossy(&term);
                    inner.insert_impl_no_wal(&term_str);
                }
                WalRecord::Remove { term } => {
                    let term_str = String::from_utf8_lossy(&term);
                    inner.remove_impl_no_wal(&term_str);
                }
                WalRecord::Checkpoint { .. } => {
                    // Skip checkpoint records during replay
                }
                WalRecord::BeginTx { .. }
                | WalRecord::CommitTx { .. }
                | WalRecord::AbortTx { .. } => {
                    // Skip transaction records
                }
                WalRecord::Increment { term, result, .. } => {
                    // Replay increment: set the term to the result value
                    let term_str = String::from_utf8_lossy(&term);
                    // Create value from the result
                    if let Ok(value_bytes) = bincode::serialize(&result) {
                        if let Ok(v) = bincode::deserialize::<V>(&value_bytes) {
                            inner.insert_impl_no_wal_with_value(&term_str, v);
                        }
                    }
                }
                WalRecord::Upsert { term, value } => {
                    // Replay upsert: deserialize and insert the value
                    let term_str = String::from_utf8_lossy(&term);
                    if let Ok(v) = bincode::deserialize::<V>(&value) {
                        inner.insert_impl_no_wal_with_value(&term_str, v);
                    }
                }
                WalRecord::CompareAndSwap { term, new_value, success, .. } => {
                    // Only replay if the CAS was successful
                    if success {
                        let term_str = String::from_utf8_lossy(&term);
                        if let Ok(v) = bincode::deserialize::<V>(&new_value) {
                            inner.insert_impl_no_wal_with_value(&term_str, v);
                        }
                    }
                }
            }
        }

        // If we loaded from disk and skipped all WAL records, we can truncate the WAL
        // (This is safe because all data is already persisted)
        if loaded_from_disk && skipped_all && checkpoint_lsn > 0 {
            // WAL truncation would happen here if we implement it
            // For now, just note that we could truncate
        }

        Ok(inner)
    }

    /// Open an existing disk-backed trie with a specific loading depth.
    ///
    /// This allows control over the trade-off between open time and lookup latency:
    /// - `eager_depth = None` (or `Some(0)`): Lazy loading - fastest open, first lookups
    ///   load nodes on-demand
    /// - `eager_depth = Some(5)`: Load 5 levels eagerly - moderate open time, fast
    ///   lookups for common prefixes
    /// - `eager_depth = Some(usize::MAX)`: Fully eager - slowest open, fastest lookups
    ///
    /// # Arguments
    /// * `path` - Path to the trie directory
    /// * `eager_depth` - Number of levels to load eagerly. `None` means lazy loading.
    ///
    /// # Example
    /// ```ignore
    /// // Lazy loading (default behavior)
    /// let trie = DiskBackedCharTrieInner::<u64>::open_with_depth("my_trie", None)?;
    ///
    /// // Load first 5 levels eagerly
    /// let trie = DiskBackedCharTrieInner::<u64>::open_with_depth("my_trie", Some(5))?;
    ///
    /// // Fully eager loading
    /// let trie = DiskBackedCharTrieInner::<u64>::open_with_depth("my_trie", Some(usize::MAX))?;
    /// ```
    #[cfg(feature = "persistent-artrie")]
    pub fn open_with_depth<P: AsRef<Path>>(path: P, eager_depth: Option<usize>) -> Result<Self> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

        let path = path.as_ref();

        // Open disk manager
        let disk_manager = DiskManager::open(path)?;

        // Read root pointer and entry count from header
        let root_ptr = disk_manager.root_ptr()?;
        let entry_count = disk_manager.entry_count()?;

        // Create buffer manager (takes ownership of disk_manager)
        let buffer_manager = BufferManager::new(disk_manager, DEFAULT_CHAR_BUFFER_POOL_SIZE);
        let buffer_manager = Arc::new(RwLock::new(buffer_manager));

        // Open or create WAL file
        let wal_path = path.with_extension("wal");
        let (wal_writer, recovered_ops, next_lsn, checkpoint_lsn) = if wal_path.exists() {
            // Recover from WAL
            let mut reader = WalReader::new(&wal_path)
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;

            let mut records = Vec::new();
            let mut max_lsn = 0u64;
            let mut checkpoint_lsn = 0u64;
            while let Some(result) = reader.next_record() {
                match result {
                    Ok((lsn, record)) => {
                        max_lsn = max_lsn.max(lsn);
                        // Track the latest checkpoint LSN
                        if let WalRecord::Checkpoint { checkpoint_lsn: cp_lsn, .. } = &record {
                            checkpoint_lsn = checkpoint_lsn.max(*cp_lsn);
                        }
                        records.push((lsn, record));
                    }
                    Err(_) => break, // Stop on error
                }
            }

            let next_lsn = max_lsn + 1;
            let writer = WalWriter::open(&wal_path)
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;

            (writer, records, next_lsn, checkpoint_lsn)
        } else {
            let writer = WalWriter::create(&wal_path)
                .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            (writer, Vec::new(), 1, 0)
        };

        let wal_writer = Arc::new(RwLock::new(wal_writer));

        // Create arena manager for space-efficient node storage
        let arena_manager = ArenaManager::with_buffer_manager(Arc::clone(&buffer_manager));
        let arena_manager = Arc::new(RwLock::new(arena_manager));

        let mut inner = Self {
            root: CharTrieRoot::Empty,
            len: 0, // Will be updated from disk or WAL replay
            dirty: false,
            buffer_manager: Some(buffer_manager.clone()),
            wal_writer: Some(wal_writer),
            wal_config: WalConfig::default(),
            next_lsn,
            file_path: Some(path.to_path_buf()),
            arena_manager: Some(arena_manager),
            version: OptimisticVersion::new(),
            epoch_manager: EpochManager::new(),
            retry_stats: RetryStats::new(),
            _phantom: std::marker::PhantomData,
        };

        // Try to load root from disk if root_ptr != 0
        let mut loaded_from_disk = false;
        if root_ptr != 0 {
            let root_swizzled = SwizzledPtr::from_raw(root_ptr);
            match inner.load_root_from_disk(&buffer_manager, &root_swizzled, eager_depth) {
                Ok((root, len)) => {
                    inner.root = root;
                    inner.len = len;
                    loaded_from_disk = true;
                }
                Err(e) => {
                    eprintln!("Warning: Failed to load root from disk: {:?}", e);
                    // Fall back to WAL replay
                }
            }
        }

        // Replay WAL records that came after the checkpoint
        // Skip records with LSN <= checkpoint_lsn (already persisted to disk)
        for (lsn, record) in recovered_ops {
            // Skip if we loaded from disk and this record is from before checkpoint
            if loaded_from_disk && checkpoint_lsn > 0 && lsn <= checkpoint_lsn {
                continue;
            }

            match record {
                WalRecord::Insert { term, .. } => {
                    let term_str = String::from_utf8_lossy(&term);
                    inner.insert_impl_no_wal(&term_str);
                }
                WalRecord::Remove { term } => {
                    let term_str = String::from_utf8_lossy(&term);
                    inner.remove_impl_no_wal(&term_str);
                }
                WalRecord::Checkpoint { .. } => {
                    // Skip checkpoint records during replay
                }
                WalRecord::BeginTx { .. }
                | WalRecord::CommitTx { .. }
                | WalRecord::AbortTx { .. } => {
                    // Transaction control records - skip for now
                }
                WalRecord::Increment { term, result, .. } => {
                    // Replay increment: set the term to the result value
                    let term_str = String::from_utf8_lossy(&term);
                    if let Ok(value_bytes) = bincode::serialize(&result) {
                        if let Ok(v) = bincode::deserialize::<V>(&value_bytes) {
                            inner.insert_impl_no_wal_with_value(&term_str, v);
                        }
                    }
                }
                WalRecord::Upsert { term, value } => {
                    // Replay upsert: deserialize and insert the value
                    let term_str = String::from_utf8_lossy(&term);
                    if let Ok(v) = bincode::deserialize::<V>(&value) {
                        inner.insert_impl_no_wal_with_value(&term_str, v);
                    }
                }
                WalRecord::CompareAndSwap { term, new_value, success, .. } => {
                    // Only replay if the CAS was successful
                    if success {
                        let term_str = String::from_utf8_lossy(&term);
                        if let Ok(v) = bincode::deserialize::<V>(&new_value) {
                            inner.insert_impl_no_wal_with_value(&term_str, v);
                        }
                    }
                }
            }
        }

        Ok(inner)
    }

    /// Open an existing disk-backed trie with custom WAL configuration
    ///
    /// This allows specifying WAL archive settings for crash recovery.
    #[cfg(feature = "persistent-artrie")]
    pub fn open_with_config<P: AsRef<Path>>(path: P, wal_config: WalConfig) -> Result<Self> {
        let mut trie = Self::open(path.as_ref())?;

        // Create archive directory if archive mode is enabled
        if wal_config.archive_enabled {
            if let Some(ref file_path) = trie.file_path {
                let archive_dir = file_path.parent().unwrap_or(Path::new(".")).join(&wal_config.archive_dir);
                if !archive_dir.exists() {
                    std::fs::create_dir_all(&archive_dir).map_err(|e| {
                        PersistentARTrieError::io_error("create archive directory", archive_dir.display().to_string(), e)
                    })?;
                }
            }
        }

        trie.wal_config = wal_config;
        Ok(trie)
    }

    /// Open an existing disk-backed trie with automatic corruption detection and recovery.
    ///
    /// This is the recommended way to open a trie that may have been corrupted
    /// by a crash (OOM kill, power failure, etc.).
    ///
    /// # Recovery Process
    ///
    /// 1. **Check if file exists** - If not, create a new trie
    /// 2. **Detect corruption** - Check header checksum, arena checksums
    /// 3. **If corrupted** - Rebuild from WAL archive segments
    /// 4. **Return trie with recovery report**
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the trie data file
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_report) indicating what recovery was performed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use libdictenstein::persistent_artrie_char::DiskBackedCharTrieInner;
    ///
    /// let (trie, report) = DiskBackedCharTrieInner::<()>::open_with_recovery("words.artc")?;
    ///
    /// if !report.mode.is_normal() {
    ///     eprintln!("Recovered from crash: {} records replayed", report.records_replayed);
    /// }
    /// ```
    #[cfg(feature = "persistent-artrie")]
    pub fn open_with_recovery<P: AsRef<Path>>(path: P) -> Result<(Self, crate::persistent_artrie::recovery::RecoveryReport)> {
        Self::open_with_recovery_config(path, WalConfig::default())
    }

    /// Open with recovery and custom WAL configuration.
    ///
    /// Same as `open_with_recovery()` but allows specifying custom WAL settings.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the trie data file
    /// * `config` - WAL configuration for archive mode, segment limits, etc.
    ///
    /// # Returns
    ///
    /// Tuple of (trie, recovery_report) indicating what recovery was performed.
    #[cfg(feature = "persistent-artrie")]
    pub fn open_with_recovery_config<P: AsRef<Path>>(
        path: P,
        config: WalConfig,
    ) -> Result<(Self, crate::persistent_artrie::recovery::RecoveryReport)> {
        use crate::persistent_artrie::recovery::{
            detect_corruption, find_wal_archive_segments, RecoveryReport,
        };
        use std::time::Instant;

        let path = path.as_ref();
        let start_time = Instant::now();

        // Check if file exists
        if !path.exists() {
            // No file - create new and return CreatedNew report
            let trie = Self::create_with_config(path, config)?;
            return Ok((trie, RecoveryReport::created_new()));
        }

        // Check for corruption
        match detect_corruption(path, true) {
            Ok(None) => {
                // No corruption detected - open normally
                let trie = Self::open_with_config(path, config)?;
                Ok((trie, RecoveryReport::normal()))
            }
            Ok(Some(corruption)) => {
                // Corruption detected - attempt recovery from WAL archives
                let corruption_reason = corruption.to_string();

                // Find archive directory
                let archive_dir = path.parent().unwrap_or(Path::new(".")).join(&config.archive_dir);

                // Find WAL archive segments
                let segments = find_wal_archive_segments(&archive_dir);

                if segments.is_empty() {
                    // No archive segments - can't recover
                    return Err(PersistentARTrieError::RecoveryError {
                        reason: format!(
                            "Corruption detected ({}) but no WAL archive segments found in {:?}",
                            corruption_reason, archive_dir
                        ),
                    });
                }

                // Remove corrupted file
                let _ = std::fs::remove_file(path);

                // Also remove current WAL (we'll rebuild from archives)
                let wal_path = path.with_extension("wal");
                let _ = std::fs::remove_file(&wal_path);

                // Create fresh trie
                let mut trie = Self::create_with_config(path, config.clone())?;

                // Rebuild from WAL archive segments
                let mut records_replayed: u64 = 0;
                let mut terms_recovered: u64 = 0;
                let mut segments_used = Vec::new();

                for segment_path in &segments {
                    // Create reader for this segment
                    use crate::persistent_artrie::wal::WalReader;

                    let reader = match WalReader::new(segment_path) {
                        Ok(r) => r,
                        Err(_) => continue, // Skip unreadable segments
                    };

                    segments_used.push(segment_path.clone());

                    for result in reader.iter() {
                        let (_lsn, record) = match result {
                            Ok(r) => r,
                            Err(_) => continue, // Skip corrupted records
                        };

                        records_replayed += 1;

                        // Apply the record to the trie
                        use crate::persistent_artrie::wal::WalRecord;
                        match record {
                            WalRecord::Insert { term, value } => {
                                let term_str = String::from_utf8_lossy(&term);
                                if let Some(value_bytes) = value {
                                    if let Ok(v) = bincode::deserialize::<V>(&value_bytes) {
                                        trie.insert_impl_no_wal_with_value(&term_str, v);
                                        terms_recovered += 1;
                                    }
                                } else {
                                    trie.insert_impl_no_wal(&term_str);
                                    terms_recovered += 1;
                                }
                            }
                            WalRecord::Increment { term, delta, result: val } => {
                                // For increment, store the final result
                                let term_str = String::from_utf8_lossy(&term);
                                let value_bytes = bincode::serialize(&val).unwrap_or_default();
                                if let Ok(v) = bincode::deserialize::<V>(&value_bytes) {
                                    trie.insert_impl_no_wal_with_value(&term_str, v);
                                    terms_recovered += 1;
                                }
                            }
                            WalRecord::Upsert { term, value } => {
                                let term_str = String::from_utf8_lossy(&term);
                                if let Ok(v) = bincode::deserialize::<V>(&value) {
                                    trie.insert_impl_no_wal_with_value(&term_str, v);
                                    terms_recovered += 1;
                                }
                            }
                            WalRecord::CompareAndSwap { term, new_value, success, .. } => {
                                if success {
                                    let term_str = String::from_utf8_lossy(&term);
                                    if let Ok(v) = bincode::deserialize::<V>(&new_value) {
                                        trie.insert_impl_no_wal_with_value(&term_str, v);
                                        terms_recovered += 1;
                                    }
                                }
                            }
                            _ => {} // Skip transaction/checkpoint records
                        }
                    }
                }

                let duration_ms = start_time.elapsed().as_millis() as u64;

                let report = RecoveryReport::rebuild_from_wal(
                    path.to_path_buf(),
                    corruption_reason,
                    records_replayed,
                    terms_recovered,
                    segments_used,
                    duration_ms,
                );

                Ok((trie, report))
            }
            Err(e) => {
                // I/O error during corruption check
                Err(PersistentARTrieError::InternalError {
                    message: format!("Error during corruption check: {}", e),
                })
            }
        }
    }

    /// Load root from disk given the root descriptor pointer
    ///
    /// This function:
    /// 1. Reads the root descriptor block
    /// 2. Loads arena block IDs and populates the arena manager
    /// 3. Loads the root node (which can now read from arenas)
    ///
    /// # Arguments
    /// * `buffer_manager` - The buffer manager for disk I/O
    /// * `root_desc_ptr` - Pointer to the root descriptor block
    /// * `eager_depth` - Controls loading strategy:
    ///   - `None`: Fully lazy loading (only root node loaded)
    ///   - `Some(0)`: Same as None (lazy loading)
    ///   - `Some(n)`: Load n levels eagerly, rest lazy
    ///   - `Some(usize::MAX)`: Fully eager loading (all levels)
    #[cfg(feature = "persistent-artrie")]
    fn load_root_from_disk(
        &self,
        buffer_manager: &Arc<RwLock<BufferManager>>,
        root_desc_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
        eager_depth: Option<usize>,
    ) -> Result<(CharTrieRoot<V>, usize)> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

        // Read the root descriptor block
        #[cfg(feature = "parking_lot")]
        let bm = buffer_manager.read();
        #[cfg(not(feature = "parking_lot"))]
        let bm = buffer_manager.read().map_err(|_| {
            PersistentARTrieError::LockPoisoned {
                resource: "buffer_manager".to_string(),
            }
        })?;

        let disk_loc = root_desc_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::internal("Root descriptor pointer is swizzled or null")
        })?;
        let page_guard = bm.fetch_page(disk_loc.block_id)?;
        let page_data = page_guard.data();

        // Parse root descriptor (fixed 18 bytes)
        // Format:
        //   0: type (1 byte)
        //   1: is_final (1 byte)
        //   2-5: term_count (4 bytes, little endian)
        //   6-9: arena_count (4 bytes, little endian)
        //   10-17: root_ptr (8 bytes, little endian)
        let root_type = page_data[0];
        let _is_final = page_data[1] != 0;
        let term_count = u32::from_le_bytes([page_data[2], page_data[3], page_data[4], page_data[5]]) as usize;
        let arena_count = u32::from_le_bytes([page_data[6], page_data[7], page_data[8], page_data[9]]);
        let root_ptr = u64::from_le_bytes([
            page_data[10], page_data[11], page_data[12], page_data[13],
            page_data[14], page_data[15], page_data[16], page_data[17],
        ]);

        // Derive arena block IDs from sequential allocation
        // Block 0 = file header, Blocks 1..=arena_count = arenas
        let arena_block_ids: Vec<u32> = (1..=arena_count).collect();

        drop(page_guard);
        drop(bm);

        // Load arenas into the arena manager
        if arena_count > 0 {
            if let Some(ref arena_manager) = self.arena_manager {
                #[cfg(feature = "parking_lot")]
                {
                    let mut am = arena_manager.write();
                    // Clear the initial empty arena
                    am.clear_for_loading();
                    // Load each arena from disk
                    for block_id in arena_block_ids {
                        am.load_arena(block_id)?;
                    }
                    // Set active arena to the last one for new allocations
                    let count = am.arena_count();
                    am.set_active_arena(count.saturating_sub(1));
                }
                #[cfg(not(feature = "parking_lot"))]
                {
                    let mut am = arena_manager.write().map_err(|_| {
                        PersistentARTrieError::LockPoisoned {
                            resource: "arena_manager".to_string(),
                        }
                    })?;
                    // Clear the initial empty arena
                    am.clear_for_loading();
                    // Load each arena from disk
                    for block_id in arena_block_ids {
                        am.load_arena(block_id)?;
                    }
                    // Set active arena to the last one for new allocations
                    let count = am.arena_count();
                    am.set_active_arena(count.saturating_sub(1));
                }
            }
        }

        match root_type {
            ROOT_TYPE_EMPTY => {
                Ok((CharTrieRoot::Empty, 0))
            }
            ROOT_TYPE_NODE => {
                let root_swizzled = SwizzledPtr::from_raw(root_ptr);
                // Choose loading strategy based on eager_depth
                let node = match eager_depth {
                    None | Some(0) => {
                        // Fully lazy: only load root node, children on-demand
                        self.load_char_node_from_disk_lazy(buffer_manager, &root_swizzled)?
                    }
                    Some(depth) if depth >= usize::MAX / 2 => {
                        // Fully eager: load all levels
                        self.load_char_node_from_disk_iterative(buffer_manager, &root_swizzled)?
                    }
                    Some(depth) => {
                        // Depth-limited: load `depth` levels, rest lazy
                        self.load_char_node_from_disk_with_depth(buffer_manager, &root_swizzled, Some(depth))?
                    }
                };
                Ok((CharTrieRoot::Node(Box::new(node)), term_count))
            }
            _ => {
                Err(PersistentARTrieError::internal(format!(
                    "Unknown root type: {}",
                    root_type
                )))
            }
        }
    }

    /// Load a CharTrieNodeInner from disk
    ///
    /// Uses arena allocation for space-efficient reading. Nodes are packed
    /// into 256KB arena blocks, with SwizzledPtr encoding:
    /// - block_id = arena_id
    /// - offset = slot_id
    ///
    /// Disk format:
    /// ```text
    /// [CharNode serialized - 16-byte header + type-specific data]
    /// [value_len: u32]
    /// [value_bytes if value_len > 0]
    /// ```
    #[cfg(feature = "persistent-artrie")]
    fn load_char_node_from_disk(
        &self,
        _buffer_manager: &Arc<RwLock<BufferManager>>,
        node_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    ) -> Result<CharTrieNodeInner<V>> {
        use super::arena_manager::ArenaSlot;
        use super::serialization_char::{deserialize_char_node_v2, DeserializationContext};
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use std::io::Cursor;

        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk reading")
        })?;

        // Get arena slot from the disk location
        // block_id = arena_id + 1 (block 0 is file header)
        // offset = slot_id
        let disk_loc = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::internal("Node pointer is swizzled or null")
        })?;
        let arena_id = disk_loc.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::internal("Invalid block_id 0 for arena node")
        })?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);

        // Read from arena
        #[cfg(feature = "parking_lot")]
        let am = arena_manager.read();
        #[cfg(not(feature = "parking_lot"))]
        let am = arena_manager.read().map_err(|_| {
            PersistentARTrieError::LockPoisoned {
                resource: "arena_manager".to_string(),
            }
        })?;

        let node_data = am.read(slot)?;

        // Deserialize the CharNode using v2 format with context
        let deser_ctx = DeserializationContext::new(slot);
        let mut cursor = Cursor::new(node_data);
        let char_node = deserialize_char_node_v2(&mut cursor, &deser_ctx)?;

        // Use cursor position to find where value data starts (v2 format is variable size)
        let offset = cursor.position() as usize;

        // Read value_len and value_bytes
        let value_len = u32::from_le_bytes([
            node_data[offset],
            node_data[offset + 1],
            node_data[offset + 2],
            node_data[offset + 3],
        ]) as usize;

        let value: Option<V> = if value_len > 0 {
            let value_start = offset + 4;
            let value_end = value_start + value_len;
            let value_bytes = &node_data[value_start..value_end];
            Some(bincode::deserialize(value_bytes).map_err(|e| {
                PersistentARTrieError::internal(&format!("Failed to deserialize value: {}", e))
            })?)
        } else {
            None
        };

        // Collect child pointers from the CharNode
        let child_data: Vec<(u32, SwizzledPtr)> = char_node
            .iter_children()
            .map(|(key, ptr)| (key, ptr.clone()))
            .collect();

        // Drop the arena lock before recursive calls
        drop(am);

        // Create the result node with proper node type from disk
        let is_final = char_node.is_final();
        let mut result = CharTrieNodeInner::new();
        result.set_final(is_final);
        result.value = value;

        // Recursively load children and add them
        for (char_val, child_ptr) in child_data {
            if let Some(c) = char::from_u32(char_val) {
                let child_node = self.load_char_node_from_disk(_buffer_manager, &child_ptr)?;
                result.insert_child(c, child_node);
            }
        }

        Ok(result)
    }

    /// Load a CharTrieNodeInner from disk with lazy child loading
    ///
    /// Unlike `load_char_node_from_disk`, this version does NOT recursively load
    /// children. Instead, it stores the on-disk SwizzledPtrs directly, allowing
    /// children to be loaded on-demand when accessed.
    ///
    /// Uses arena allocation for space-efficient reading. Nodes are packed
    /// into 256KB arena blocks, with SwizzledPtr encoding:
    /// - block_id = arena_id
    /// - offset = slot_id
    ///
    /// This is the preferred loading method for large tries where loading
    /// everything upfront would be too expensive.
    ///
    /// Disk format:
    /// ```text
    /// [CharNode serialized - 16-byte header + type-specific data]
    /// [value_len: u32]
    /// [value_bytes if value_len > 0]
    /// ```
    #[cfg(feature = "persistent-artrie")]
    fn load_char_node_from_disk_lazy(
        &self,
        _buffer_manager: &Arc<RwLock<BufferManager>>,
        node_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    ) -> Result<CharTrieNodeInner<V>> {
        use super::arena_manager::ArenaSlot;
        use super::serialization_char::{deserialize_char_node_v2, DeserializationContext};
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use std::io::Cursor;

        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk reading")
        })?;

        // Get arena slot from the disk location
        // block_id = arena_id + 1 (block 0 is file header)
        // offset = slot_id
        let disk_loc = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::internal("Node pointer is swizzled or null")
        })?;
        let arena_id = disk_loc.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::internal("Invalid block_id 0 for arena node")
        })?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);

        // Read from arena
        #[cfg(feature = "parking_lot")]
        let am = arena_manager.read();
        #[cfg(not(feature = "parking_lot"))]
        let am = arena_manager.read().map_err(|_| {
            PersistentARTrieError::LockPoisoned {
                resource: "arena_manager".to_string(),
            }
        })?;

        let node_data = am.read(slot)?;

        // Deserialize the CharNode using v2 format with context
        let deser_ctx = DeserializationContext::new(slot);
        let mut cursor = Cursor::new(node_data);
        let char_node = deserialize_char_node_v2(&mut cursor, &deser_ctx)?;

        // Use cursor position to find where value data starts (v2 format is variable size)
        let offset = cursor.position() as usize;

        // Read value_len and value_bytes
        let value_len = u32::from_le_bytes([
            node_data[offset],
            node_data[offset + 1],
            node_data[offset + 2],
            node_data[offset + 3],
        ]) as usize;

        let value: Option<V> = if value_len > 0 {
            let value_start = offset + 4;
            let value_end = value_start + value_len;
            let value_bytes = &node_data[value_start..value_end];
            Some(bincode::deserialize(value_bytes).map_err(|e| {
                PersistentARTrieError::internal(&format!("Failed to deserialize value: {}", e))
            })?)
        } else {
            None
        };

        // Collect child pointers from the CharNode (as-is, for lazy loading)
        let child_data: Vec<(char, SwizzledPtr)> = char_node
            .iter_children()
            .filter_map(|(key, ptr)| {
                char::from_u32(key).map(|c| (c, ptr.clone()))
            })
            .collect();

        drop(am);

        // Create the node
        let is_final = char_node.is_final();
        let mut result = CharTrieNodeInner::new();
        result.set_final(is_final);
        result.value = value;

        // Insert children using insert_child_ptr (stores raw SwizzledPtrs without loading)
        for (c, child_ptr) in child_data {
            // If there's an old in-memory pointer, we'd need to free it,
            // but for fresh loading there shouldn't be any
            let _old = result.insert_child_ptr(c, child_ptr);
        }

        Ok(result)
    }

    /// Load a single CharTrieNodeInner's data from disk WITHOUT loading children.
    ///
    /// This is a helper for iterative loading. Returns the node (without children
    /// connected) and the list of child pointers that need to be loaded.
    ///
    /// The returned node has `is_final`, `value`, and an empty child set.
    /// Children must be connected by the caller after loading.
    #[cfg(feature = "persistent-artrie")]
    fn load_single_node_data(
        &self,
        node_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    ) -> Result<(CharTrieNodeInner<V>, Vec<(char, crate::persistent_artrie::swizzled_ptr::SwizzledPtr)>)> {
        use super::arena_manager::ArenaSlot;
        use super::serialization_char::{deserialize_char_node_v2, DeserializationContext};
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use std::io::Cursor;

        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk reading")
        })?;

        // Get arena slot from the disk location
        // block_id = arena_id + 1 (block 0 is file header)
        // offset = slot_id
        let disk_loc = node_ptr.disk_location().ok_or_else(|| {
            PersistentARTrieError::internal("Node pointer is swizzled or null")
        })?;
        let arena_id = disk_loc.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::internal("Invalid block_id 0 for arena node")
        })?;
        let slot = ArenaSlot::new(arena_id, disk_loc.offset);

        // Read from arena
        #[cfg(feature = "parking_lot")]
        let am = arena_manager.read();
        #[cfg(not(feature = "parking_lot"))]
        let am = arena_manager.read().map_err(|_| {
            PersistentARTrieError::LockPoisoned {
                resource: "arena_manager".to_string(),
            }
        })?;

        let node_data = am.read(slot)?;

        // Deserialize the CharNode using v2 format with context
        let deser_ctx = DeserializationContext::new(slot);
        let mut cursor = Cursor::new(node_data);
        let char_node = deserialize_char_node_v2(&mut cursor, &deser_ctx)?;

        // Use cursor position to find where value data starts (v2 format is variable size)
        let offset = cursor.position() as usize;

        // Read value_len and value_bytes
        let value_len = u32::from_le_bytes([
            node_data[offset],
            node_data[offset + 1],
            node_data[offset + 2],
            node_data[offset + 3],
        ]) as usize;

        let value: Option<V> = if value_len > 0 {
            let value_start = offset + 4;
            let value_end = value_start + value_len;
            let value_bytes = &node_data[value_start..value_end];
            Some(bincode::deserialize(value_bytes).map_err(|e| {
                PersistentARTrieError::internal(&format!("Failed to deserialize value: {}", e))
            })?)
        } else {
            None
        };

        // Collect child pointers from the CharNode
        let child_entries: Vec<(char, SwizzledPtr)> = char_node
            .iter_children()
            .filter_map(|(key, ptr)| {
                char::from_u32(key).map(|c| (c, ptr.clone()))
            })
            .collect();

        drop(am);

        // Create the result node with proper node type from disk (NO children connected)
        let is_final = char_node.is_final();
        let mut result = CharTrieNodeInner::new();
        result.set_final(is_final);
        result.value = value;

        Ok((result, child_entries))
    }

    /// Load a CharTrieNodeInner from disk using iterative (non-recursive) traversal.
    ///
    /// This avoids stack overflow for deep tries by using an explicit work stack
    /// instead of recursive function calls. Uses a two-phase algorithm:
    ///
    /// 1. **Phase 1**: Load all nodes into a vector (without connecting children)
    /// 2. **Phase 2**: Connect children to parents in reverse order (bottom-up)
    ///
    /// This maintains identical semantics to `load_char_node_from_disk` but can
    /// handle arbitrarily deep tries without stack overflow.
    #[cfg(feature = "persistent-artrie")]
    fn load_char_node_from_disk_iterative(
        &self,
        _buffer_manager: &Arc<RwLock<BufferManager>>,
        root_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
    ) -> Result<CharTrieNodeInner<V>> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use std::collections::HashMap;

        /// Information about a loaded node before children are connected
        struct LoadedNodeInfo<V: DictionaryValue> {
            /// The node with is_final and value set, but NO children
            node: CharTrieNodeInner<V>,
            /// Child entries that need to be loaded and connected
            child_entries: Vec<(char, SwizzledPtr)>,
        }

        // Stack for DFS traversal (avoids recursion)
        let mut work_stack: Vec<SwizzledPtr> = vec![root_ptr.clone()];

        // Results vector - nodes are stored in DFS pre-order
        let mut loaded_nodes: Vec<LoadedNodeInfo<V>> = Vec::new();

        // Map from disk pointer raw value to result index (for parent-child linking)
        let mut ptr_to_idx: HashMap<u64, usize> = HashMap::new();

        // Phase 1: Load all nodes without connecting children
        while let Some(node_ptr) = work_stack.pop() {
            // Skip if already loaded (handles potential shared subtrees)
            let ptr_raw = node_ptr.to_raw();
            if ptr_to_idx.contains_key(&ptr_raw) {
                continue;
            }

            // Load this node's data from disk (single I/O)
            let (node, child_entries) = self.load_single_node_data(&node_ptr)?;

            // Reserve result index
            let result_idx = loaded_nodes.len();
            ptr_to_idx.insert(ptr_raw, result_idx);

            // Store child entries for Phase 2
            let child_ptrs: Vec<SwizzledPtr> = child_entries.iter()
                .map(|(_, ptr)| ptr.clone())
                .collect();

            loaded_nodes.push(LoadedNodeInfo { node, child_entries });

            // Push children onto stack (reverse order for correct DFS ordering)
            // This ensures children are processed in the order they appear
            for child_ptr in child_ptrs.into_iter().rev() {
                work_stack.push(child_ptr);
            }
        }

        // Handle empty tree case
        if loaded_nodes.is_empty() {
            return Err(PersistentARTrieError::internal("No nodes loaded from disk"));
        }

        // Phase 2: Connect children to parents (bottom-up)
        // Process in reverse order so children are fully built before parents connect to them
        for idx in (0..loaded_nodes.len()).rev() {
            // Take child_entries out to avoid borrowing issues
            let child_entries = std::mem::take(&mut loaded_nodes[idx].child_entries);

            for (char_key, child_ptr) in child_entries {
                let child_idx = *ptr_to_idx.get(&child_ptr.to_raw())
                    .ok_or_else(|| PersistentARTrieError::internal(
                        "Child pointer not found in loaded nodes map"
                    ))?;

                // Take ownership of the child node (replace with empty placeholder)
                let child_node = std::mem::replace(
                    &mut loaded_nodes[child_idx].node,
                    CharTrieNodeInner::new()
                );

                // Connect child to parent
                loaded_nodes[idx].node.insert_child(char_key, child_node);
            }
        }

        // Root is at index 0 (first node pushed/processed)
        Ok(std::mem::replace(&mut loaded_nodes[0].node, CharTrieNodeInner::new()))
    }

    /// Load a CharTrieNodeInner with depth-limited eager loading.
    ///
    /// Loads the first `max_depth` levels of the trie eagerly (all at once),
    /// while keeping nodes beyond that depth as disk pointers for lazy loading.
    ///
    /// This provides a balance between:
    /// - Fully eager loading (fast lookups, slow open, high memory)
    /// - Fully lazy loading (fast open, slower first lookups)
    ///
    /// # Arguments
    /// * `buffer_manager` - The buffer manager for disk I/O
    /// * `root_ptr` - The root node's disk pointer
    /// * `max_depth` - Maximum depth to load eagerly. Nodes at this depth have
    ///   their children stored as disk pointers. `None` means fully eager.
    ///
    /// # Example Depths
    /// - `Some(0)`: Only root loaded, all children lazy (same as lazy loading)
    /// - `Some(3)`: Root + 2 levels loaded, 4th level and beyond lazy
    /// - `Some(10)`: First 10 levels loaded eagerly
    /// - `None`: All levels loaded (same as full iterative loading)
    #[cfg(feature = "persistent-artrie")]
    fn load_char_node_from_disk_with_depth(
        &self,
        _buffer_manager: &Arc<RwLock<BufferManager>>,
        root_ptr: &crate::persistent_artrie::swizzled_ptr::SwizzledPtr,
        max_depth: Option<usize>,
    ) -> Result<CharTrieNodeInner<V>> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use std::collections::HashMap;

        // If max_depth is 0, just do lazy loading (only load root)
        if max_depth == Some(0) {
            return self.load_char_node_from_disk_lazy(_buffer_manager, root_ptr);
        }

        /// Work item with depth tracking
        struct WorkItem {
            ptr: SwizzledPtr,
            depth: usize,
        }

        /// Information about a loaded node before children are connected
        struct LoadedNodeInfo<V: DictionaryValue> {
            node: CharTrieNodeInner<V>,
            /// Children to load eagerly (within depth limit)
            eager_children: Vec<(char, SwizzledPtr)>,
            /// Children to keep as disk pointers (beyond depth limit)
            lazy_children: Vec<(char, SwizzledPtr)>,
        }

        // Stack for DFS traversal with depth tracking
        let mut work_stack: Vec<WorkItem> = vec![WorkItem {
            ptr: root_ptr.clone(),
            depth: 0,
        }];

        // Results vector - nodes are stored in DFS pre-order
        let mut loaded_nodes: Vec<LoadedNodeInfo<V>> = Vec::new();

        // Map from disk pointer raw value to result index
        let mut ptr_to_idx: HashMap<u64, usize> = HashMap::new();

        // Phase 1: Load nodes up to depth limit
        while let Some(work_item) = work_stack.pop() {
            let ptr_raw = work_item.ptr.to_raw();
            if ptr_to_idx.contains_key(&ptr_raw) {
                continue;
            }

            // Load this node's data from disk
            let (node, child_entries) = self.load_single_node_data(&work_item.ptr)?;

            // Reserve result index
            let result_idx = loaded_nodes.len();
            ptr_to_idx.insert(ptr_raw, result_idx);

            // Determine which children to load eagerly vs lazily
            let at_depth_limit = max_depth.map_or(false, |max| work_item.depth >= max.saturating_sub(1));

            let (eager_children, lazy_children): (Vec<_>, Vec<_>) = if at_depth_limit {
                // At depth limit: all children become lazy
                (Vec::new(), child_entries)
            } else {
                // Within limit: all children loaded eagerly
                (child_entries, Vec::new())
            };

            // Push eager children to work stack (reverse order for correct DFS)
            for (_, child_ptr) in eager_children.iter().rev() {
                work_stack.push(WorkItem {
                    ptr: child_ptr.clone(),
                    depth: work_item.depth + 1,
                });
            }

            loaded_nodes.push(LoadedNodeInfo {
                node,
                eager_children,
                lazy_children,
            });
        }

        // Handle empty tree case
        if loaded_nodes.is_empty() {
            return Err(PersistentARTrieError::internal("No nodes loaded from disk"));
        }

        // Phase 2: Connect children (bottom-up)
        for idx in (0..loaded_nodes.len()).rev() {
            // First, insert lazy children as disk pointers
            let lazy_children = std::mem::take(&mut loaded_nodes[idx].lazy_children);
            for (char_key, child_ptr) in lazy_children {
                loaded_nodes[idx].node.insert_child_ptr(char_key, child_ptr);
            }

            // Then, connect eager children (already loaded)
            let eager_children = std::mem::take(&mut loaded_nodes[idx].eager_children);
            for (char_key, child_ptr) in eager_children {
                let child_idx = *ptr_to_idx.get(&child_ptr.to_raw())
                    .ok_or_else(|| PersistentARTrieError::internal(
                        "Child pointer not found in loaded nodes map"
                    ))?;

                // Take ownership of the child node
                let child_node = std::mem::replace(
                    &mut loaded_nodes[child_idx].node,
                    CharTrieNodeInner::new()
                );

                // Connect child to parent
                loaded_nodes[idx].node.insert_child(char_key, child_node);
            }
        }

        // Root is at index 0
        Ok(std::mem::replace(&mut loaded_nodes[0].node, CharTrieNodeInner::new()))
    }

    /// Get a child of a node with lazy loading support.
    ///
    /// If the child pointer is already swizzled (in-memory), returns the node directly.
    /// If on disk, loads the node lazily and atomically swizzles the pointer.
    ///
    /// Returns `Ok(None)` if the child doesn't exist.
    /// Returns `Err` if an I/O error occurs during lazy loading.
    #[cfg(feature = "persistent-artrie")]
    fn get_child_lazy(&self, node: &CharTrieNodeInner<V>, c: char) -> Result<Option<&CharTrieNodeInner<V>>> {
        match node.node.find_child(c as u32) {
            Some(ptr) => {
                if ptr.is_null() {
                    Ok(None)
                } else {
                    Ok(Some(self.resolve_swizzled_ptr(ptr)?))
                }
            }
            None => Ok(None),
        }
    }

    /// Get a mutable child reference of a node with lazy loading support.
    ///
    /// If the child pointer is already swizzled (in-memory), returns the node directly.
    /// If on disk, loads the node lazily and atomically swizzles the pointer.
    ///
    /// Returns `Ok(None)` if the child doesn't exist.
    /// Returns `Err` if an I/O error occurs during lazy loading.
    #[cfg(feature = "persistent-artrie")]
    fn get_child_mut_lazy(&self, node: &CharTrieNodeInner<V>, c: char) -> Result<Option<&mut CharTrieNodeInner<V>>> {
        match node.node.find_child(c as u32) {
            Some(ptr) => {
                if ptr.is_null() {
                    Ok(None)
                } else {
                    Ok(Some(self.resolve_swizzled_ptr_mut(ptr)?))
                }
            }
            None => Ok(None),
        }
    }

    /// Get or create a child with lazy loading support.
    ///
    /// If the child exists (in memory or on disk), returns a raw pointer to it.
    /// If on disk, loads the node lazily first.
    /// If the child doesn't exist, creates a new one.
    ///
    /// Returns `Err` if an I/O error occurs during lazy loading.
    ///
    /// # Safety
    ///
    /// The caller must ensure `node` is part of this trie's structure.
    /// The returned pointer is valid as long as the trie exists.
    #[cfg(feature = "persistent-artrie")]
    fn get_or_create_child_lazy_ptr(
        &self,
        node: &mut CharTrieNodeInner<V>,
        c: char,
    ) -> Result<*mut CharTrieNodeInner<V>> {
        let key = c as u32;

        // Check if child already exists
        if let Some(ptr) = node.node.find_child(key) {
            if !ptr.is_null() {
                // Child exists - ensure it's swizzled (load if on disk)
                let child_ref = self.resolve_swizzled_ptr_mut(ptr)?;
                return Ok(child_ref as *mut CharTrieNodeInner<V>);
            }
        }

        // Child doesn't exist - create new one
        let new_child = Box::new(CharTrieNodeInner::new());
        let ptr = Box::into_raw(new_child);
        let swizzled = SwizzledPtr::in_memory(ptr);

        // Add to node, handling potential growth
        match node.node.add_child_growing(key, swizzled) {
            Ok(Some(grown)) => {
                node.node = grown;
            }
            Ok(None) => {
                // No growth needed
            }
            Err(_) => {
                // Key already exists (shouldn't happen, but handle gracefully)
                unsafe { drop(Box::from_raw(ptr)); }
                // Try to get the existing child
                if let Some(existing_ptr) = node.node.find_child(key) {
                    let child_ref = self.resolve_swizzled_ptr_mut(existing_ptr)?;
                    return Ok(child_ref as *mut CharTrieNodeInner<V>);
                }
                return Err(PersistentARTrieError::internal("Failed to add or find child"));
            }
        }

        Ok(ptr)
    }

    /// Resolve a SwizzledPtr to a reference to a CharTrieNodeInner
    ///
    /// If the pointer is already swizzled (in-memory), returns the existing node.
    /// If on disk, loads the node lazily and atomically swizzles the pointer.
    ///
    /// This method handles the race condition where multiple threads try to load
    /// the same node simultaneously - only one allocation will survive.
    ///
    /// # Safety
    ///
    /// The returned reference is valid as long as the node is not evicted from
    /// memory. In the current implementation, nodes are never evicted.
    #[cfg(feature = "persistent-artrie")]
    fn resolve_swizzled_ptr(&self, ptr: &SwizzledPtr) -> Result<&CharTrieNodeInner<V>> {
        use crate::persistent_artrie::error::SwizzleError;

        // Fast path: already in memory
        if let Some(p) = ptr.as_ptr::<CharTrieNodeInner<V>>() {
            // Safety: We control all SwizzledPtr creation; ptr is valid
            return Ok(unsafe { &*p });
        }

        // Null pointer check
        if ptr.is_null() {
            return Err(PersistentARTrieError::internal("Cannot resolve null SwizzledPtr"));
        }

        // Slow path: load from disk
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for disk access")
        })?;

        // Load the node data (lazy - children are not recursively loaded)
        let loaded = self.load_char_node_from_disk_lazy(buffer_manager, ptr)?;
        let boxed = Box::new(loaded);
        let raw_ptr = Box::into_raw(boxed);

        // Try to swizzle atomically
        match ptr.swizzle(raw_ptr) {
            Ok(()) => {
                // We won the race
                Ok(unsafe { &*raw_ptr })
            }
            Err(SwizzleError::RaceCondition) | Err(SwizzleError::AlreadySwizzled) => {
                // Another thread won the race - free our copy and use theirs
                unsafe { drop(Box::from_raw(raw_ptr)); }
                // Safety: The winner has swizzled the pointer
                Ok(unsafe { &*ptr.as_ptr_unchecked::<CharTrieNodeInner<V>>() })
            }
            Err(e) => {
                // Something else went wrong - free our allocation
                unsafe { drop(Box::from_raw(raw_ptr)); }
                Err(PersistentARTrieError::internal(&format!("Swizzle failed: {:?}", e)))
            }
        }
    }

    /// Resolve a SwizzledPtr to a mutable reference to a CharTrieNodeInner
    ///
    /// Similar to `resolve_swizzled_ptr` but returns a mutable reference.
    ///
    /// # Safety
    ///
    /// The caller must ensure exclusive access to the node.
    #[cfg(feature = "persistent-artrie")]
    fn resolve_swizzled_ptr_mut(&self, ptr: &SwizzledPtr) -> Result<&mut CharTrieNodeInner<V>> {
        use crate::persistent_artrie::error::SwizzleError;

        // Fast path: already in memory
        if let Some(p) = ptr.as_ptr::<CharTrieNodeInner<V>>() {
            // Safety: We control all SwizzledPtr creation; caller ensures exclusive access
            return Ok(unsafe { &mut *(p as *mut CharTrieNodeInner<V>) });
        }

        // Null pointer check
        if ptr.is_null() {
            return Err(PersistentARTrieError::internal("Cannot resolve null SwizzledPtr"));
        }

        // Slow path: load from disk
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for disk access")
        })?;

        // Load the node data (lazy - children are not recursively loaded)
        let loaded = self.load_char_node_from_disk_lazy(buffer_manager, ptr)?;
        let boxed = Box::new(loaded);
        let raw_ptr = Box::into_raw(boxed);

        // Try to swizzle atomically
        match ptr.swizzle(raw_ptr) {
            Ok(()) => {
                // We won the race
                Ok(unsafe { &mut *raw_ptr })
            }
            Err(SwizzleError::RaceCondition) | Err(SwizzleError::AlreadySwizzled) => {
                // Another thread won the race - free our copy and use theirs
                unsafe { drop(Box::from_raw(raw_ptr)); }
                // Safety: The winner has swizzled the pointer
                Ok(unsafe { &mut *(ptr.as_ptr_unchecked::<CharTrieNodeInner<V>>() as *mut CharTrieNodeInner<V>) })
            }
            Err(e) => {
                // Something else went wrong - free our allocation
                unsafe { drop(Box::from_raw(raw_ptr)); }
                Err(PersistentARTrieError::internal(&format!("Swizzle failed: {:?}", e)))
            }
        }
    }

    /// Insert a term (internal, no WAL logging)
    #[cfg(feature = "persistent-artrie")]
    fn insert_impl_no_wal(&mut self, term: &str) -> bool {
        // Ensure we have a root node
        if matches!(self.root, CharTrieRoot::Empty) {
            self.root = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }

        // Navigate to the insertion point using raw pointer for traversal
        // This is safe because we maintain exclusive access through &mut self
        let root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => unreachable!(),
        };

        let mut current = root;
        for c in term.chars() {
            // Safety: current is valid and we have exclusive access through &mut self
            let node = unsafe { &mut *current };
            current = self.get_or_create_child_lazy_ptr(node, c)
                .expect("I/O error during lazy loading in insert");
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if already final
        if node.is_final() {
            return false;
        }

        // Mark as final
        node.set_final(true);
        self.len += 1;
        self.dirty = true;
        true
    }

    #[cfg(not(feature = "persistent-artrie"))]
    fn insert_impl_no_wal(&mut self, term: &str) -> bool {
        // Ensure we have a root node
        if matches!(self.root, CharTrieRoot::Empty) {
            self.root = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }

        // Navigate to the insertion point
        let root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut(),
            CharTrieRoot::Empty => unreachable!(),
        };

        let mut current = root;
        for c in term.chars() {
            current = current.get_or_create_child(c);
        }

        // Check if already final
        if current.is_final() {
            return false;
        }

        // Mark as final
        current.set_final(true);
        self.len += 1;
        self.dirty = true;
        true
    }

    /// Insert a term with value (internal, no WAL logging)
    #[cfg(feature = "persistent-artrie")]
    fn insert_impl_no_wal_with_value(&mut self, term: &str, value: V) -> bool {
        // Ensure we have a root node
        if matches!(self.root, CharTrieRoot::Empty) {
            self.root = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }

        // Navigate to the insertion point using raw pointer for traversal
        let root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => unreachable!(),
        };

        let mut current = root;
        for c in term.chars() {
            // Safety: current is valid and we have exclusive access through &mut self
            let node = unsafe { &mut *current };
            current = self.get_or_create_child_lazy_ptr(node, c)
                .expect("I/O error during lazy loading in insert");
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if already final
        if node.is_final() {
            // Update value if already exists
            node.value = Some(value);
            return false;
        }

        // Mark as final with value
        node.set_final(true);
        node.value = Some(value);
        self.len += 1;
        self.dirty = true;
        true
    }

    /// Insert a term with value (internal, no WAL logging)
    #[cfg(not(feature = "persistent-artrie"))]
    fn insert_impl_no_wal_with_value(&mut self, term: &str, value: V) -> bool {
        // Ensure we have a root node
        if matches!(self.root, CharTrieRoot::Empty) {
            self.root = CharTrieRoot::Node(Box::new(CharTrieNodeInner::new()));
        }

        // Navigate to the insertion point
        let root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut(),
            CharTrieRoot::Empty => unreachable!(),
        };

        let mut current = root;
        for c in term.chars() {
            current = current.get_or_create_child(c);
        }

        // Check if already final
        if current.is_final() {
            // Update value if already exists
            current.value = Some(value);
            return false;
        }

        // Mark as final with value
        current.set_final(true);
        current.value = Some(value);
        self.len += 1;
        self.dirty = true;
        true
    }

    /// Remove a term (internal, no WAL logging)
    #[cfg(feature = "persistent-artrie")]
    fn remove_impl_no_wal(&mut self, term: &str) -> bool {
        let root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut() as *mut CharTrieNodeInner<V>,
            CharTrieRoot::Empty => return false,
        };

        // Navigate to the node using raw pointer for traversal
        let chars: Vec<char> = term.chars().collect();
        let mut current = root;
        for &c in &chars {
            // Safety: current is valid and we have exclusive access through &mut self
            let node = unsafe { &*current };
            match self.get_child_mut_lazy(node, c) {
                Ok(Some(child)) => current = child as *mut CharTrieNodeInner<V>,
                Ok(None) => return false, // Term not found
                Err(_) => return false, // I/O error during lazy load
            }
        }

        // Safety: current is valid
        let node = unsafe { &mut *current };

        // Check if this node is final
        if !node.is_final() {
            return false;
        }

        // Mark as not final
        node.set_final(false);
        node.value = None;
        self.len -= 1;
        self.dirty = true;
        true
    }

    /// Remove a term (internal, no WAL logging)
    #[cfg(not(feature = "persistent-artrie"))]
    fn remove_impl_no_wal(&mut self, term: &str) -> bool {
        let root = match &mut self.root {
            CharTrieRoot::Node(node) => node.as_mut(),
            CharTrieRoot::Empty => return false,
        };

        // Navigate to the node
        let chars: Vec<char> = term.chars().collect();
        let mut current = root;
        for &c in &chars {
            match current.get_child_mut(c) {
                Some(child) => current = child,
                None => return false, // Term not found
            }
        }

        // Check if this node is final
        if !current.is_final() {
            return false;
        }

        // Mark as not final
        current.set_final(false);
        current.value = None;
        self.len -= 1;
        self.dirty = true;
        true
    }

    /// Check if a term exists in the trie
    ///
    /// For persistent tries with lazy loading, this will load nodes on-demand.
    /// I/O errors during lazy loading will cause a panic. Use `try_contains()`
    /// for explicit error handling.
    pub fn contains(&self, term: &str) -> bool {
        #[cfg(feature = "persistent-artrie")]
        {
            self.try_contains(term)
                .expect("I/O error during lazy loading in contains()")
        }
        #[cfg(not(feature = "persistent-artrie"))]
        {
            let root = match &self.root {
                CharTrieRoot::Node(node) => node.as_ref(),
                CharTrieRoot::Empty => return false,
            };

            let mut current = root;
            for c in term.chars() {
                match current.get_child(c) {
                    Some(child) => current = child,
                    None => return false,
                }
            }

            current.is_final()
        }
    }

    /// Check if a term exists in the trie with explicit error handling.
    ///
    /// This version returns a `Result` for lazy loading I/O errors.
    #[cfg(feature = "persistent-artrie")]
    pub fn try_contains(&self, term: &str) -> Result<bool> {
        let root = match &self.root {
            CharTrieRoot::Node(node) => node.as_ref(),
            CharTrieRoot::Empty => return Ok(false),
        };

        let mut current = root;
        for c in term.chars() {
            match self.get_child_lazy(current, c)? {
                Some(child) => current = child,
                None => return Ok(false),
            }
        }

        Ok(current.is_final())
    }

    /// Get a value by term
    ///
    /// For persistent tries with lazy loading, this will load nodes on-demand.
    /// I/O errors during lazy loading will cause a panic. Use `try_get()`
    /// for explicit error handling.
    pub fn get(&self, term: &str) -> Option<&V> {
        #[cfg(feature = "persistent-artrie")]
        {
            self.try_get(term)
                .expect("I/O error during lazy loading in get()")
        }
        #[cfg(not(feature = "persistent-artrie"))]
        {
            let root = match &self.root {
                CharTrieRoot::Node(node) => node.as_ref(),
                CharTrieRoot::Empty => return None,
            };

            let mut current = root;
            for c in term.chars() {
                match current.get_child(c) {
                    Some(child) => current = child,
                    None => return None,
                }
            }

            if current.is_final() {
                current.value.as_ref()
            } else {
                None
            }
        }
    }

    /// Get a value by term with explicit error handling.
    ///
    /// This version returns a `Result` for lazy loading I/O errors.
    #[cfg(feature = "persistent-artrie")]
    pub fn try_get(&self, term: &str) -> Result<Option<&V>> {
        let root = match &self.root {
            CharTrieRoot::Node(node) => node.as_ref(),
            CharTrieRoot::Empty => return Ok(None),
        };

        let mut current = root;
        for c in term.chars() {
            match self.get_child_lazy(current, c)? {
                Some(child) => current = child,
                None => return Ok(None),
            }
        }

        if current.is_final() {
            Ok(current.value.as_ref())
        } else {
            Ok(None)
        }
    }

    // ==================== Optimistic Concurrency Methods ====================

    /// Try an optimistic read for contains.
    ///
    /// Returns `Some(result)` if the read was consistent, `None` if a concurrent
    /// write occurred and the read should be retried.
    #[cfg(feature = "persistent-artrie")]
    pub fn try_contains_optimistic(&self, term: &str) -> Option<bool> {
        // Record the version before reading
        let guard = OptimisticReadGuard::new(&self.version);

        // Perform the read
        let result = self.contains(term);

        // Validate the version - if it changed, return None to signal retry
        if guard.validate() {
            Some(result)
        } else {
            None
        }
    }

    /// Optimistic contains with automatic retry.
    ///
    /// Retries up to `max_retries` times if concurrent writes occur.
    /// Returns the result if successful within retry limit.
    #[cfg(feature = "persistent-artrie")]
    pub fn contains_optimistic(&self, term: &str, max_retries: usize) -> Option<bool> {
        let mut retries = 0u64;
        for _ in 0..max_retries {
            if let Some(result) = self.try_contains_optimistic(term) {
                self.retry_stats.record_success(retries);
                return Some(result);
            }
            retries += 1;
            std::hint::spin_loop();
        }
        None
    }

    /// Try an optimistic read for get.
    ///
    /// Returns `Some(result)` if the read was consistent, `None` if retry needed.
    /// Note: Returns Option<Option<V>> - outer Option for consistency, inner for value.
    #[cfg(feature = "persistent-artrie")]
    pub fn try_get_optimistic(&self, term: &str) -> Option<Option<V>> {
        let guard = OptimisticReadGuard::new(&self.version);

        // Clone the value if found (to avoid holding reference during validation)
        let result = self.get(term).cloned();

        if guard.validate() {
            Some(result)
        } else {
            None
        }
    }

    /// Optimistic get with automatic retry.
    #[cfg(feature = "persistent-artrie")]
    pub fn get_optimistic(&self, term: &str, max_retries: usize) -> Option<Option<V>> {
        let mut retries = 0u64;
        for _ in 0..max_retries {
            if let Some(result) = self.try_get_optimistic(term) {
                self.retry_stats.record_success(retries);
                return Some(result);
            }
            retries += 1;
            std::hint::spin_loop();
        }
        None
    }

    /// Enter an epoch-protected read section.
    ///
    /// Returns an EpochGuard that must be held while reading. This ensures
    /// memory accessed during the read won't be reclaimed until the guard is dropped.
    #[cfg(feature = "persistent-artrie")]
    pub fn enter_epoch(&self) -> EpochGuard<'_> {
        EpochGuard::new(&self.epoch_manager)
    }

    /// Get the current read epoch.
    #[cfg(feature = "persistent-artrie")]
    pub fn current_epoch(&self) -> u64 {
        self.epoch_manager.current_epoch()
    }

    /// Advance the epoch (should be called periodically by a background task).
    #[cfg(feature = "persistent-artrie")]
    pub fn advance_epoch(&self) -> u64 {
        self.epoch_manager.advance()
    }

    /// Get the number of active readers.
    #[cfg(feature = "persistent-artrie")]
    pub fn active_readers(&self) -> usize {
        self.epoch_manager.active_reader_count()
    }

    /// Get retry statistics snapshot.
    #[cfg(feature = "persistent-artrie")]
    pub fn retry_stats_snapshot(&self) -> crate::persistent_artrie::concurrency::RetryStatsSnapshot {
        self.retry_stats.snapshot()
    }

    /// Check if the trie is currently being written to.
    #[cfg(feature = "persistent-artrie")]
    pub fn is_write_locked(&self) -> bool {
        !self.version.is_stable()
    }

    /// Get the current version (for debugging/monitoring).
    #[cfg(feature = "persistent-artrie")]
    pub fn current_version(&self) -> u64 {
        self.version.get()
    }

    // ==================== End Optimistic Concurrency Methods ====================

    /// Insert a term with WAL logging
    #[cfg(feature = "persistent-artrie")]
    pub fn insert(&mut self, term: &str) -> Result<bool> {
        // Log to WAL first
        if let Some(ref wal_writer) = self.wal_writer {
            let record = WalRecord::Insert {
                term: term.as_bytes().to_vec(),
                value: None,
            };
            #[cfg(feature = "parking_lot")]
            {
                wal_writer
                    .write()
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            }
            #[cfg(not(feature = "parking_lot"))]
            {
                wal_writer
                    .write()
                    .expect("WAL lock")
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            }
        }

        // Mark version as being written (odd = in-progress)
        self.version.begin_write();
        let result = self.insert_impl_no_wal(term);
        // Mark version as stable (even = complete)
        self.version.end_write();

        Ok(result)
    }

    /// Remove a term with WAL logging
    #[cfg(feature = "persistent-artrie")]
    pub fn remove(&mut self, term: &str) -> Result<bool> {
        // Log to WAL first
        if let Some(ref wal_writer) = self.wal_writer {
            let record = WalRecord::Remove {
                term: term.as_bytes().to_vec(),
            };
            #[cfg(feature = "parking_lot")]
            {
                wal_writer
                    .write()
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            }
            #[cfg(not(feature = "parking_lot"))]
            {
                wal_writer
                    .write()
                    .expect("WAL lock")
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            }
        }

        // Mark version as being written
        self.version.begin_write();
        let result = self.remove_impl_no_wal(term);
        self.version.end_write();

        Ok(result)
    }

    // ========================================================================
    // Prefix Operations
    // ========================================================================

    /// Navigate to the node at the given prefix path.
    ///
    /// Returns `Ok(Some(node))` if the prefix exists, `Ok(None)` if it doesn't.
    /// Returns `Err` if an I/O error occurs during lazy loading.
    #[cfg(feature = "persistent-artrie")]
    fn navigate_to_prefix(&self, prefix: &str) -> Result<Option<&CharTrieNodeInner<V>>> {
        let root = match &self.root {
            CharTrieRoot::Node(node) => node.as_ref(),
            CharTrieRoot::Empty => return Ok(None),
        };

        let mut current = root;
        for c in prefix.chars() {
            match self.get_child_lazy(current, c)? {
                Some(child) => current = child,
                None => return Ok(None),
            }
        }

        Ok(Some(current))
    }

    /// Collect all terms under a node via DFS traversal.
    ///
    /// This method eagerly collects terms. For memory efficiency when dealing
    /// with large subtrees, use `iter_prefix` with batched processing instead.
    #[cfg(feature = "persistent-artrie")]
    fn collect_terms_under_node(
        &self,
        node: &CharTrieNodeInner<V>,
        prefix: String,
        terms: &mut Vec<String>,
    ) -> Result<()> {
        // If this node is a final state, add the current prefix as a term
        if node.is_final() {
            terms.push(prefix.clone());
        }

        // Recursively traverse children
        for (c, child) in node.iter_children() {
            let mut child_prefix = prefix.clone();
            child_prefix.push(c);
            self.collect_terms_under_node(child, child_prefix, terms)?;
        }

        Ok(())
    }

    /// Collect terms under a node with a limit for batched processing.
    ///
    /// Stops collecting after `limit` terms have been found.
    #[cfg(feature = "persistent-artrie")]
    fn collect_terms_under_node_limited(
        &self,
        node: &CharTrieNodeInner<V>,
        prefix: String,
        terms: &mut Vec<String>,
        limit: usize,
    ) -> Result<bool> {
        if terms.len() >= limit {
            return Ok(true); // Signal that we're full
        }

        // If this node is a final state, add the current prefix as a term
        if node.is_final() {
            terms.push(prefix.clone());
            if terms.len() >= limit {
                return Ok(true);
            }
        }

        // Recursively traverse children
        for (c, child) in node.iter_children() {
            let mut child_prefix = prefix.clone();
            child_prefix.push(c);
            if self.collect_terms_under_node_limited(child, child_prefix, terms, limit)? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Collect terms with values under a node.
    #[cfg(feature = "persistent-artrie")]
    fn collect_terms_with_values_under_node(
        &self,
        node: &CharTrieNodeInner<V>,
        prefix: String,
        terms: &mut Vec<(String, V)>,
    ) -> Result<()>
    where
        V: Clone,
    {
        // If this node is a final state with a value, add it
        if node.is_final() {
            if let Some(value) = &node.value {
                terms.push((prefix.clone(), value.clone()));
            }
        }

        // Recursively traverse children
        for (c, child) in node.iter_children() {
            let mut child_prefix = prefix.clone();
            child_prefix.push(c);
            self.collect_terms_with_values_under_node(child, child_prefix, terms)?;
        }

        Ok(())
    }

    /// Iterate over all terms with the given prefix.
    ///
    /// Returns `Ok(None)` if the prefix path doesn't exist in the trie.
    /// Returns `Ok(Some(vec))` with all terms starting with the prefix.
    ///
    /// # Note
    ///
    /// This method collects all matching terms into a `Vec`. For very large
    /// subtrees, consider using `remove_prefix_batched` which processes
    /// terms in smaller batches.
    ///
    /// # Example
    /// ```rust,ignore
    /// let trie = DiskBackedCharTrieInner::open("data.artrie")?;
    /// if let Some(terms) = trie.iter_prefix("app")? {
    ///     for term in terms {
    ///         println!("{}", term);
    ///     }
    /// }
    /// ```
    #[cfg(feature = "persistent-artrie")]
    pub fn iter_prefix(&self, prefix: &str) -> Result<Option<Vec<String>>> {
        let node = match self.navigate_to_prefix(prefix)? {
            Some(n) => n,
            None => return Ok(None),
        };

        let mut terms = Vec::new();
        self.collect_terms_under_node(node, prefix.to_string(), &mut terms)?;
        Ok(Some(terms))
    }

    /// Iterate over all (term, value) pairs with the given prefix.
    ///
    /// Returns `Ok(None)` if the prefix path doesn't exist in the trie.
    /// Returns `Ok(Some(vec))` with all (term, value) pairs for terms starting with the prefix.
    #[cfg(feature = "persistent-artrie")]
    pub fn iter_prefix_with_values(&self, prefix: &str) -> Result<Option<Vec<(String, V)>>>
    where
        V: Clone,
    {
        let node = match self.navigate_to_prefix(prefix)? {
            Some(n) => n,
            None => return Ok(None),
        };

        let mut terms = Vec::new();
        self.collect_terms_with_values_under_node(node, prefix.to_string(), &mut terms)?;
        Ok(Some(terms))
    }

    /// Remove all terms with the given prefix.
    ///
    /// Uses a default batch size of 1024 to limit memory usage.
    /// Each removal is logged to WAL individually for crash recovery safety.
    ///
    /// # Returns
    ///
    /// The number of terms removed.
    #[cfg(feature = "persistent-artrie")]
    pub fn remove_prefix(&mut self, prefix: &str) -> Result<usize> {
        self.remove_prefix_batched(prefix, 1024)
    }

    /// Remove all terms with the given prefix using a custom batch size.
    ///
    /// Processes removals in batches to limit memory usage when dealing
    /// with large subtrees. Each removal is logged to WAL individually.
    ///
    /// # Arguments
    ///
    /// * `prefix` - The prefix to match
    /// * `batch_size` - Maximum terms to collect and remove per batch
    ///
    /// # Returns
    ///
    /// The number of terms removed.
    #[cfg(feature = "persistent-artrie")]
    pub fn remove_prefix_batched(&mut self, prefix: &str, batch_size: usize) -> Result<usize> {
        let batch_size = batch_size.max(1);
        let mut total_removed = 0;

        loop {
            // Collect a batch of terms matching the prefix
            let batch: Vec<String> = {
                let node = match self.navigate_to_prefix(prefix)? {
                    Some(n) => n,
                    None => break, // Prefix no longer exists
                };

                let mut terms = Vec::with_capacity(batch_size);
                self.collect_terms_under_node_limited(node, prefix.to_string(), &mut terms, batch_size)?;
                terms
            };

            if batch.is_empty() {
                break;
            }

            // Remove each term in the batch (with WAL logging)
            for term in batch {
                if self.remove(&term)? {
                    total_removed += 1;
                }
            }
        }

        Ok(total_removed)
    }

    /// Sync changes to disk
    #[cfg(feature = "persistent-artrie")]
    pub fn sync(&mut self) -> Result<()> {
        if let Some(ref wal_writer) = self.wal_writer {
            #[cfg(feature = "parking_lot")]
            {
                wal_writer
                    .write()
                    .sync()
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            }
            #[cfg(not(feature = "parking_lot"))]
            {
                wal_writer
                    .write()
                    .expect("WAL lock")
                    .sync()
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            }
        }
        Ok(())
    }

    // ========================================================================
    // Atomic Operations with WAL Support
    // ========================================================================

    /// Atomically increment a value by delta.
    ///
    /// If the term doesn't exist, inserts with `delta` as the initial value.
    /// The value must be serializable as an i64.
    ///
    /// # Returns
    ///
    /// The new value after incrementing.
    #[cfg(feature = "persistent-artrie")]
    pub fn increment(&mut self, term: &str, delta: i64) -> Result<i64> {
        // Get current value
        let current: i64 = if let Some(v) = self.get(term) {
            let bytes = bincode::serialize(&v).map_err(|e| {
                PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
            })?;
            if bytes.len() == 8 {
                i64::from_le_bytes(bytes.try_into().unwrap())
            } else {
                bincode::deserialize::<i64>(&bytes).map_err(|e| {
                    PersistentARTrieError::internal(format!("Failed to deserialize as i64: {}", e))
                })?
            }
        } else {
            0
        };

        let new_value = current + delta;

        // Create value from i64
        let value_bytes = bincode::serialize(&new_value).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to serialize new value: {}", e))
        })?;
        let v: V = bincode::deserialize(&value_bytes).map_err(|e| {
            PersistentARTrieError::internal(format!("Failed to deserialize as V: {}", e))
        })?;

        // Log to WAL first
        if let Some(ref wal_writer) = self.wal_writer {
            let record = WalRecord::Increment {
                term: term.as_bytes().to_vec(),
                delta,
                result: new_value,
            };
            #[cfg(feature = "parking_lot")]
            {
                wal_writer
                    .write()
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            }
            #[cfg(not(feature = "parking_lot"))]
            {
                wal_writer
                    .write()
                    .expect("WAL lock")
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            }
        }

        // Update the trie
        self.insert_impl_no_wal_with_value(term, v);

        Ok(new_value)
    }

    /// Atomically update or insert a value.
    ///
    /// # Returns
    ///
    /// `true` if a new term was inserted, `false` if an existing term was updated.
    #[cfg(feature = "persistent-artrie")]
    pub fn upsert(&mut self, term: &str, value: V) -> Result<bool> {
        let existed = self.contains(term);

        // Log to WAL first
        if let Some(ref wal_writer) = self.wal_writer {
            let value_bytes = bincode::serialize(&value).map_err(|e| {
                PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
            })?;
            let record = WalRecord::Upsert {
                term: term.as_bytes().to_vec(),
                value: value_bytes,
            };
            #[cfg(feature = "parking_lot")]
            {
                wal_writer
                    .write()
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            }
            #[cfg(not(feature = "parking_lot"))]
            {
                wal_writer
                    .write()
                    .expect("WAL lock")
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            }
        }

        // Update the trie
        self.insert_impl_no_wal_with_value(term, value);

        Ok(!existed)
    }

    /// Atomically compare and swap a value.
    ///
    /// Updates the value only if the current value matches `expected`.
    ///
    /// # Returns
    ///
    /// `true` if the swap succeeded, `false` if the current value didn't match expected.
    #[cfg(feature = "persistent-artrie")]
    pub fn compare_and_swap(&mut self, term: &str, expected: Option<V>, new_value: V) -> Result<bool> {
        let current = self.get(term).cloned();

        // Check if current matches expected
        let matches = match (&current, &expected) {
            (None, None) => true,
            (Some(c), Some(e)) => {
                let c_bytes = bincode::serialize(c).ok();
                let e_bytes = bincode::serialize(e).ok();
                c_bytes == e_bytes
            }
            _ => false,
        };

        if matches {
            // Log to WAL first
            if let Some(ref wal_writer) = self.wal_writer {
                let expected_bytes = expected
                    .as_ref()
                    .map(|e| bincode::serialize(e).ok())
                    .flatten();
                let new_value_bytes = bincode::serialize(&new_value).map_err(|e| {
                    PersistentARTrieError::internal(format!("Failed to serialize value: {}", e))
                })?;
                let record = WalRecord::CompareAndSwap {
                    term: term.as_bytes().to_vec(),
                    expected: expected_bytes,
                    new_value: new_value_bytes,
                    success: true,
                };
                #[cfg(feature = "parking_lot")]
                {
                    wal_writer
                        .write()
                        .append(record)
                        .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
                }
                #[cfg(not(feature = "parking_lot"))]
                {
                    wal_writer
                        .write()
                        .expect("WAL lock")
                        .append(record)
                        .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
                }
            }

            // Update the trie
            self.insert_impl_no_wal_with_value(term, new_value);
        }

        Ok(matches)
    }

    /// Get the current value and increment atomically (fetch-and-add).
    ///
    /// Returns the value *before* the increment.
    #[cfg(feature = "persistent-artrie")]
    pub fn fetch_add(&mut self, term: &str, delta: i64) -> Result<i64> {
        let new_value = self.increment(term, delta)?;
        Ok(new_value - delta)
    }

    /// Get or insert a default value atomically.
    ///
    /// If the term exists, returns its current value.
    /// If not, inserts the default value and returns it.
    #[cfg(feature = "persistent-artrie")]
    pub fn get_or_insert(&mut self, term: &str, default: V) -> Result<V> {
        if let Some(v) = self.get(term).cloned() {
            return Ok(v);
        }

        // Log to WAL first (using insert record)
        if let Some(ref wal_writer) = self.wal_writer {
            let value_bytes = bincode::serialize(&default).ok();
            let record = WalRecord::Insert {
                term: term.as_bytes().to_vec(),
                value: value_bytes,
            };
            #[cfg(feature = "parking_lot")]
            {
                wal_writer
                    .write()
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            }
            #[cfg(not(feature = "parking_lot"))]
            {
                wal_writer
                    .write()
                    .expect("WAL lock")
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
            }
        }

        // Insert the default value
        self.insert_impl_no_wal_with_value(term, default.clone());

        Ok(default)
    }

    /// Checkpoint: persist trie to disk and truncate WAL
    ///
    /// This is the verified checkpoint sequence that ensures data integrity
    /// before truncating the WAL:
    ///
    /// 1. persist_to_disk() - serialize and sync data
    /// 2. verify_checkpoint() - read back and verify header checksum
    /// 3. WAL checkpoint record - mark checkpoint in WAL
    /// 4. WAL sync - ensure checkpoint record is durable
    /// 5. WAL truncate - only after verification passes
    ///
    /// If verification fails at step 2, the WAL is NOT truncated,
    /// allowing recovery from the existing WAL on next open.
    #[cfg(feature = "persistent-artrie")]
    pub fn checkpoint(&mut self) -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        // Step 1: Persist trie to disk
        self.persist_to_disk()?;

        // Step 2: Verify checkpoint - re-read header and verify checksum
        // This ensures the sync() actually succeeded and data is durable
        self.verify_checkpoint()?;

        // Steps 3-5: WAL operations (only after verification passes)
        if let Some(ref wal_writer) = self.wal_writer {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let record = WalRecord::Checkpoint {
                checkpoint_lsn: self.next_lsn,
                timestamp,
            };
            #[cfg(feature = "parking_lot")]
            {
                // Step 3: Write checkpoint record
                wal_writer
                    .write()
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
                // Step 4: Sync WAL
                wal_writer
                    .write()
                    .sync()
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
                // Step 5: Archive or truncate WAL (only after all verification passes)
                if self.wal_config.archive_enabled {
                    // rotate_to_archive handles archive dir creation and pruning internally
                    wal_writer
                        .write()
                        .rotate_to_archive(&self.wal_config)
                        .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
                } else {
                    wal_writer
                        .write()
                        .truncate()
                        .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
                }
            }
            #[cfg(not(feature = "parking_lot"))]
            {
                // Step 3: Write checkpoint record
                wal_writer
                    .write()
                    .expect("WAL lock")
                    .append(record)
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
                // Step 4: Sync WAL
                wal_writer
                    .write()
                    .expect("WAL lock")
                    .sync()
                    .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
                // Step 5: Archive or truncate WAL (only after all verification passes)
                if self.wal_config.archive_enabled {
                    // rotate_to_archive handles archive dir creation and pruning internally
                    wal_writer
                        .write()
                        .expect("WAL lock")
                        .rotate_to_archive(&self.wal_config)
                        .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
                } else {
                    wal_writer
                        .write()
                        .expect("WAL lock")
                        .truncate()
                        .map_err(|e| PersistentARTrieError::WalError { reason: format!("{:?}", e) })?;
                }
            }
        }

        self.dirty = false;
        Ok(())
    }

    /// Verify checkpoint data integrity after persist_to_disk()
    ///
    /// Re-reads the file header from disk and verifies its checksum.
    /// This ensures the fsync() actually succeeded and data is durable.
    ///
    /// Returns an error if verification fails - the WAL should NOT be
    /// truncated in this case.
    #[cfg(feature = "persistent-artrie")]
    fn verify_checkpoint(&self) -> Result<()> {
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for checkpoint verification")
        })?;

        // Re-read header from disk and verify checksum
        #[cfg(feature = "parking_lot")]
        let bm = buffer_manager.read();
        #[cfg(not(feature = "parking_lot"))]
        let bm = buffer_manager.read().map_err(|_| {
            PersistentARTrieError::LockPoisoned {
                resource: "buffer_manager".to_string(),
            }
        })?;

        let dm = bm.disk_manager();

        // Read header and verify checksum
        let header = dm.read_header()?;
        if !header.verify_checksum() {
            return Err(PersistentARTrieError::CheckpointVerificationFailed {
                reason: format!(
                    "Header checksum mismatch after sync: stored={:#x}, computed={:#x}",
                    header.checksum,
                    header.compute_checksum()
                ),
            });
        }

        Ok(())
    }

    /// Persist the entire trie to disk
    ///
    /// This serializes the trie structure and writes it to the data file,
    /// updating the file header with the root pointer.
    #[cfg(feature = "persistent-artrie")]
    pub fn persist_to_disk(&mut self) -> Result<()> {
        use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
        use crate::persistent_artrie::NodeType;

        // Get buffer manager
        let buffer_manager = self.buffer_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No buffer manager for disk serialization")
        })?;

        // Serialize the trie root and get a descriptor
        let (root_type, root_ptr, is_final) = match &self.root {
            CharTrieRoot::Empty => {
                (ROOT_TYPE_EMPTY, 0u64, false)
            }
            CharTrieRoot::Node(node) => {
                // Recursively serialize the node and all children
                let ptr = self.serialize_char_node_to_disk(node.as_ref())?;
                (ROOT_TYPE_NODE, ptr.to_raw(), node.is_final())
            }
        };

        // Flush arenas to disk FIRST to get their block_ids
        // (writes dirty arenas to buffer manager)
        if let Some(ref arena_manager) = self.arena_manager {
            #[cfg(feature = "parking_lot")]
            arena_manager.write().flush()?;
            #[cfg(not(feature = "parking_lot"))]
            arena_manager
                .write()
                .map_err(|_| PersistentARTrieError::LockPoisoned {
                    resource: "arena_manager".to_string(),
                })?
                .flush()?;
        }

        // Get arena count after flushing (block IDs are derived from sequential allocation)
        let arena_count: u32 = if let Some(ref arena_manager) = self.arena_manager {
            #[cfg(feature = "parking_lot")]
            {
                arena_manager.read().arena_count() as u32
            }
            #[cfg(not(feature = "parking_lot"))]
            {
                arena_manager
                    .read()
                    .map_err(|_| PersistentARTrieError::LockPoisoned {
                        resource: "arena_manager".to_string(),
                    })?
                    .arena_count() as u32
            }
        } else {
            0
        };

        // Create root descriptor block (fixed 18 bytes)
        // Format:
        //   0: type (1 byte)
        //   1: is_final (1 byte)
        //   2-5: term_count (4 bytes, little endian)
        //   6-9: arena_count (4 bytes, little endian)
        //   10-17: root_ptr (8 bytes, little endian)
        //
        // Note: Arena block IDs are NOT stored - they are derived from sequential allocation:
        // Block 0 = file header, Blocks 1..=arena_count = arenas
        let mut descriptor = vec![0u8; 18];
        descriptor[0] = root_type;
        descriptor[1] = if is_final { 1 } else { 0 };
        descriptor[2..6].copy_from_slice(&(self.len as u32).to_le_bytes());
        descriptor[6..10].copy_from_slice(&arena_count.to_le_bytes());
        descriptor[10..18].copy_from_slice(&root_ptr.to_le_bytes());

        // Allocate a block for the descriptor and write it
        #[cfg(feature = "parking_lot")]
        let bm = buffer_manager.write();
        #[cfg(not(feature = "parking_lot"))]
        let bm = buffer_manager.write().map_err(|_| {
            PersistentARTrieError::LockPoisoned {
                resource: "buffer_manager".to_string(),
            }
        })?;

        let mut page_guard = bm.new_page()?;
        let block_id = page_guard.block_id();
        let page_data = page_guard.data_mut();
        page_data[..descriptor.len()].copy_from_slice(&descriptor);

        // Update the file header with the root pointer
        let dm = bm.disk_manager();
        let root_descriptor_ptr = SwizzledPtr::on_disk(block_id, 0, NodeType::Bucket);
        dm.set_root_ptr(root_descriptor_ptr.to_raw())?;
        dm.set_entry_count(self.len as u64)?;

        // Must drop page_guard first, then buffer_manager lock
        drop(page_guard);
        drop(bm);

        // Re-acquire buffer manager lock for final flush
        #[cfg(feature = "parking_lot")]
        let bm = buffer_manager.write();
        #[cfg(not(feature = "parking_lot"))]
        let bm = buffer_manager.write().map_err(|_| {
            PersistentARTrieError::LockPoisoned {
                resource: "buffer_manager".to_string(),
            }
        })?;

        // Flush all pages to ensure durability
        bm.flush_all()?;
        bm.disk_manager().sync()?;

        self.dirty = false;
        Ok(())
    }

    /// Check if serialized children are consecutive in the same arena.
    ///
    /// For sequential sibling storage optimization: if all children are in the same arena
    /// and have consecutive slot IDs, we can store just `(first_slot, count)` instead of
    /// N separate pointers.
    ///
    /// # Arguments
    /// * `child_ptrs` - Child (key, SwizzledPtr) pairs from serialization
    /// * `parent_arena_id` - Arena ID where parent will be allocated
    ///
    /// # Returns
    /// `Some(first_child_slot)` if children are consecutive in same arena as parent,
    /// `None` otherwise.
    #[cfg(feature = "persistent-artrie")]
    fn check_sequential_char_children(
        child_ptrs: &[(u32, SwizzledPtr)],
        parent_arena_id: u32,
    ) -> Option<super::arena_manager::ArenaSlot> {
        use super::arena_manager::ArenaSlot;

        if child_ptrs.len() < 2 {
            // Need at least 2 children for sequential optimization to be worthwhile
            return None;
        }

        // Collect arena slots from SwizzledPtrs
        let mut slots: Vec<ArenaSlot> = Vec::with_capacity(child_ptrs.len());
        for (_, ptr) in child_ptrs {
            // Get disk location from SwizzledPtr
            let loc = match ptr.disk_location() {
                Some(loc) => loc,
                None => return None, // All children must be on disk
            };
            let arena_id = loc.block_id;
            let slot_id = loc.offset;
            if arena_id != parent_arena_id {
                // All children must be in the same arena as parent
                return None;
            }
            slots.push(ArenaSlot::new(arena_id, slot_id));
        }

        // Sort by slot ID
        slots.sort_by_key(|s| s.slot_id);

        // Check if consecutive
        let first = slots[0];
        for (i, slot) in slots.iter().enumerate() {
            if slot.slot_id != first.slot_id + i as u32 {
                return None;
            }
        }

        Some(first)
    }

    /// Serialize a CharTrieNodeInner to disk and return its SwizzledPtr
    ///
    /// Uses arena allocation for space-efficient storage. Multiple nodes are
    /// packed into each 256KB arena block instead of wasting one block per node.
    ///
    /// Node format on disk:
    /// ```text
    /// [CharNode serialized - 16-byte header + type-specific data]
    /// [value_len: u32]
    /// [value_bytes if value_len > 0]
    /// ```
    ///
    /// The SwizzledPtr uses:
    /// - arena_id as block_id (23 bits, up to 8M arenas)
    /// - slot_id as offset (22 bits, up to 4M slots per arena)
    #[cfg(feature = "persistent-artrie")]
    fn serialize_char_node_to_disk(&self, node: &CharTrieNodeInner<V>) -> Result<SwizzledPtr> {
        use super::relative_encoding::SerializationContext;
        use super::serialization_char::serialize_char_node_v2;

        let arena_manager = self.arena_manager.as_ref().ok_or_else(|| {
            PersistentARTrieError::internal("No arena manager for disk serialization")
        })?;

        // Get the predicted parent slot for sequential sibling check
        #[cfg(feature = "parking_lot")]
        let parent_arena_id = arena_manager.read().next_slot().arena_id;
        #[cfg(not(feature = "parking_lot"))]
        let parent_arena_id = arena_manager
            .read()
            .map_err(|_| PersistentARTrieError::LockPoisoned {
                resource: "arena_manager".to_string(),
            })?
            .next_slot()
            .arena_id;

        // First, recursively serialize all children and collect their disk pointers
        let mut child_disk_ptrs: Vec<(u32, SwizzledPtr)> = Vec::with_capacity(node.num_children());
        for (c, child) in node.iter_children() {
            let ptr = self.serialize_char_node_to_disk(child)?;
            child_disk_ptrs.push((c as u32, ptr));
        }

        // Get the predicted parent slot for encoding children
        #[cfg(feature = "parking_lot")]
        let parent_slot = arena_manager.read().next_slot();
        #[cfg(not(feature = "parking_lot"))]
        let parent_slot = arena_manager
            .read()
            .map_err(|_| PersistentARTrieError::LockPoisoned {
                resource: "arena_manager".to_string(),
            })?
            .next_slot();

        // Check if children are consecutive (enables sequential sibling storage)
        // Create serialization context that determines encoding mode:
        // - Sequential: children stored as (first_slot, count) instead of N pointers
        // - Relative: child offsets encoded relative to parent (1-2 bytes vs 8 bytes)
        // - Full: absolute (arena_id, slot_id) for each child (9 bytes per child)
        //
        // IMPORTANT: If parent_slot.slot_id is small (especially 0), children serialized
        // in the previous arena(s) would have "negative" relative offsets, causing
        // decode underflow. Use full encoding to avoid this.
        let ctx = if parent_slot.slot_id < child_disk_ptrs.len() as u32 {
            // Parent slot is near the start of an arena - children likely in previous arena
            // Use full encoding to avoid relative offset underflow during decode
            SerializationContext::full_encoding(parent_slot)
        } else if let Some(first_child) =
            Self::check_sequential_char_children(&child_disk_ptrs, parent_arena_id)
        {
            // Children are consecutive in same arena: use sequential sibling encoding
            SerializationContext::sequential(parent_slot, first_child)
        } else {
            // Children are not consecutive: use relative encoding only
            SerializationContext::new(parent_slot)
        };

        // Build a CharNode with disk pointers for serialization
        let disk_node = self.build_disk_char_node(&node.node, &child_disk_ptrs);

        // Serialize the value using bincode (needed regardless of encoding)
        let value_bytes: Vec<u8> = if let Some(ref value) = node.value {
            bincode::serialize(value).map_err(|e| {
                PersistentARTrieError::internal(&format!("Failed to serialize value: {}", e))
            })?
        } else {
            Vec::new()
        };

        // Serialize the CharNode to a buffer using v2 format with relative offsets
        let mut node_buffer = Vec::new();
        serialize_char_node_v2(&disk_node, &mut node_buffer, &ctx)?;

        // Build complete serialized data:
        // [node_buffer] + [value_len: u32] + [value_bytes]
        let build_data = |node_buf: &[u8], value_buf: &[u8]| -> Vec<u8> {
            let total_size = node_buf.len() + 4 + value_buf.len();
            let mut data = Vec::with_capacity(total_size);
            data.extend_from_slice(node_buf);
            data.extend_from_slice(&(value_buf.len() as u32).to_le_bytes());
            data.extend_from_slice(value_buf);
            data
        };

        let data = build_data(&node_buffer, &value_bytes);

        // Allocate in arena (space-efficient: packs many nodes per 256KB block)
        #[cfg(feature = "parking_lot")]
        let slot = arena_manager.write().allocate(&data)?;
        #[cfg(not(feature = "parking_lot"))]
        let slot = arena_manager
            .write()
            .map_err(|_| PersistentARTrieError::LockPoisoned {
                resource: "arena_manager".to_string(),
            })?
            .allocate(&data)?;

        // Check if arena overflow caused slot mismatch
        // If so, re-serialize using the actual slot to prevent relative encoding underflow
        let final_slot = if slot != ctx.parent_slot {
            // Arena overflow detected - need to re-serialize with correct parent slot
            // This happens when the predicted slot was in arena N, but allocation
            // went to arena N+1 due to arena being full
            //
            // Children are now likely in a different arena than the parent, requiring
            // cross-arena encoding (9 bytes per child) instead of relative encoding.
            let corrected_ctx = SerializationContext::new(slot);
            let mut corrected_buffer = Vec::new();
            serialize_char_node_v2(&disk_node, &mut corrected_buffer, &corrected_ctx)?;
            let corrected_data = build_data(&corrected_buffer, &value_bytes);

            if corrected_data.len() == data.len() {
                // Same size - can update in-place
                #[cfg(feature = "parking_lot")]
                arena_manager.write().update(slot, &corrected_data)?;
                #[cfg(not(feature = "parking_lot"))]
                arena_manager
                    .write()
                    .map_err(|_| PersistentARTrieError::LockPoisoned {
                        resource: "arena_manager".to_string(),
                    })?
                    .update(slot, &corrected_data)?;
                slot
            } else {
                // Different size (cross-arena encoding is larger) - allocate new slot
                // The original slot becomes wasted space (acceptable for rare overflow cases)
                #[cfg(feature = "parking_lot")]
                let new_slot = arena_manager.write().allocate(&corrected_data)?;
                #[cfg(not(feature = "parking_lot"))]
                let new_slot = arena_manager
                    .write()
                    .map_err(|_| PersistentARTrieError::LockPoisoned {
                        resource: "arena_manager".to_string(),
                    })?
                    .allocate(&corrected_data)?;
                new_slot
            }
        } else {
            slot
        };

        // Return pointer using arena addressing:
        // - block_id = arena_id + 1 (block 0 is file header, arena N is in block N+1)
        // - offset = slot_id
        let node_type = self.char_node_to_node_type(&disk_node);
        Ok(SwizzledPtr::on_disk(final_slot.arena_id + 1, final_slot.slot_id, node_type))
    }

    /// Build a CharNode with disk SwizzledPtrs for serialization
    ///
    /// Creates a new CharNode of the same type as the original, but with
    /// children pointing to disk locations instead of in-memory nodes.
    #[cfg(feature = "persistent-artrie")]
    fn build_disk_char_node(
        &self,
        original: &CharNode,
        disk_children: &[(u32, SwizzledPtr)],
    ) -> CharNode {
        use super::nodes::{CharBucket, CharNode16, CharNode4, CharNode48};

        // Create a new node of the same type
        let mut new_node = match original {
            CharNode::N4(_) => CharNode::N4(Box::new(CharNode4::new())),
            CharNode::N16(_) => CharNode::N16(Box::new(CharNode16::new())),
            CharNode::N48(_) => CharNode::N48(Box::new(CharNode48::new())),
            CharNode::Bucket(_) => CharNode::Bucket(Box::new(CharBucket::new())),
        };

        // Copy header properties
        {
            let new_header = new_node.header_mut();
            let orig_header = original.header();
            new_header.prefix_len = orig_header.prefix_len;
            new_header.flags = orig_header.flags;
            new_header.version = orig_header.version;
        }

        // Copy prefix
        *new_node.prefix_mut() = *original.prefix();

        // Add disk children
        for &(key, ref ptr) in disk_children {
            // Use add_child_growing to handle insertions properly
            // Note: We're inserting disk SwizzledPtrs, not memory pointers
            if let Err(e) = new_node.add_child_growing(key, ptr.clone()) {
                // This shouldn't happen since we're rebuilding the same structure
                panic!("Failed to add disk child during serialization: {:?}", e);
            }
        }

        new_node
    }

    /// Map CharNode type to NodeType for SwizzledPtr
    #[cfg(feature = "persistent-artrie")]
    fn char_node_to_node_type(&self, node: &CharNode) -> NodeType {
        match node {
            CharNode::N4(_) => NodeType::CharNode4,
            CharNode::N16(_) => NodeType::CharNode16,
            CharNode::N48(_) => NodeType::CharNode48,
            CharNode::Bucket(_) => NodeType::CharBucket,
        }
    }
}

/// Root descriptor type constants
const ROOT_TYPE_EMPTY: u8 = 0;
const ROOT_TYPE_NODE: u8 = 1;

impl<V: DictionaryValue> Default for DiskBackedCharTrieInner<V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_header_roundtrip() {
        let mut header = CharTrieFileHeader {
            magic: CHAR_TRIE_MAGIC,
            version: CHAR_HEADER_VERSION_V2,
            _reserved: [0; 3],
            root_ptr: 12345,
            entry_count: 67890,
            checkpoint_lsn: 111,
            header_checksum: 0,
            _padding: [0; 28],
        };
        header.finalize_checksum();

        let bytes = header.to_bytes();
        let restored = CharTrieFileHeader::from_bytes(&bytes);

        assert_eq!(restored.magic, CHAR_TRIE_MAGIC);
        assert_eq!(restored.version, CHAR_HEADER_VERSION_V2);
        assert_eq!(restored.root_ptr, 12345);
        assert_eq!(restored.entry_count, 67890);
        assert_eq!(restored.checkpoint_lsn, 111);
        assert!(restored.verify_checksum());
    }

    #[test]
    fn test_file_header_v1_roundtrip() {
        // V1 headers have no checksum
        let header = CharTrieFileHeader {
            magic: CHAR_TRIE_MAGIC,
            version: CHAR_HEADER_VERSION_V1,
            _reserved: [0; 3],
            root_ptr: 12345,
            entry_count: 67890,
            checkpoint_lsn: 111,
            header_checksum: 0,
            _padding: [0; 28],
        };

        let bytes = header.to_bytes();
        let restored = CharTrieFileHeader::from_bytes(&bytes);

        assert_eq!(restored.magic, CHAR_TRIE_MAGIC);
        assert_eq!(restored.version, CHAR_HEADER_VERSION_V1);
        assert_eq!(restored.root_ptr, 12345);
        assert!(!restored.has_checksum());
        assert!(restored.verify_checksum()); // V1 always valid
    }

    #[test]
    fn test_file_header_checksum() {
        let mut header = CharTrieFileHeader::new();
        header.root_ptr = 12345;
        header.entry_count = 67890;

        // Before finalize, checksum is 0
        assert_eq!(header.header_checksum, 0);
        assert!(!header.verify_checksum()); // Checksum doesn't match

        // After finalize, checksum is valid
        header.finalize_checksum();
        assert_ne!(header.header_checksum, 0);
        assert!(header.verify_checksum());

        // Modify a field and checksum becomes invalid
        header.root_ptr = 99999;
        assert!(!header.verify_checksum());

        // Finalize again to fix
        header.finalize_checksum();
        assert!(header.verify_checksum());
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_file_header_validation() {
        let mut header = CharTrieFileHeader::new();
        header.finalize_checksum();
        assert!(header.validate().is_ok());

        // Invalid magic
        header.magic = *b"XXXX";
        assert!(header.validate().is_err());

        // Restore magic, corrupt checksum
        header.magic = CHAR_TRIE_MAGIC;
        header.header_checksum = 0xDEADBEEF;
        assert!(header.validate().is_err());
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_file_header_from_bytes_verified() {
        let mut header = CharTrieFileHeader::new();
        header.root_ptr = 12345;
        header.finalize_checksum();

        let bytes = header.to_bytes();

        // Valid bytes should succeed
        let restored = CharTrieFileHeader::from_bytes_verified(&bytes);
        assert!(restored.is_ok());

        // Corrupt bytes should fail
        let mut corrupted = bytes;
        corrupted[8] = 0xFF; // Corrupt root_ptr
        let result = CharTrieFileHeader::from_bytes_verified(&corrupted);
        assert!(result.is_err());
    }

    #[test]
    fn test_file_header_upgrade_to_v2() {
        let mut header = CharTrieFileHeader::new_v1();
        assert!(!header.has_checksum());

        header.root_ptr = 12345;
        header.upgrade_to_v2();

        assert!(header.has_checksum());
        assert!(header.verify_checksum());
        assert_eq!(header.version, CHAR_HEADER_VERSION_V2);
    }

    #[test]
    fn test_inner_new() {
        let inner: DiskBackedCharTrieInner<()> = DiskBackedCharTrieInner::new();
        assert_eq!(inner.len, 0);
        assert!(!inner.dirty);
        assert!(matches!(inner.root, CharTrieRoot::Empty));
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_create_and_open() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.trie");

        // Create a new trie
        {
            let mut inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::create(&path).expect("create");
            inner.insert("hello").expect("insert");
            inner.insert("world").expect("insert");
            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            // WAL replay should have reconstructed the state
            assert_eq!(inner.len, 2);
        }
    }

    #[test]
    fn test_insert_and_contains() {
        let mut inner: DiskBackedCharTrieInner<()> = DiskBackedCharTrieInner::new();

        // Insert some terms
        assert!(inner.insert_impl_no_wal("hello"));
        assert!(inner.insert_impl_no_wal("world"));
        assert!(inner.insert_impl_no_wal("hello world"));

        // Verify contains
        assert!(inner.contains("hello"));
        assert!(inner.contains("world"));
        assert!(inner.contains("hello world"));
        assert!(!inner.contains("hell"));
        assert!(!inner.contains("hello worl"));

        assert_eq!(inner.len, 3);
    }

    #[test]
    fn test_insert_duplicate() {
        let mut inner: DiskBackedCharTrieInner<()> = DiskBackedCharTrieInner::new();

        // First insert should succeed
        assert!(inner.insert_impl_no_wal("hello"));

        // Duplicate insert should fail
        assert!(!inner.insert_impl_no_wal("hello"));

        // Length should still be 1
        assert_eq!(inner.len, 1);
    }

    #[test]
    fn test_remove() {
        let mut inner: DiskBackedCharTrieInner<()> = DiskBackedCharTrieInner::new();

        // Insert some terms
        inner.insert_impl_no_wal("hello");
        inner.insert_impl_no_wal("world");
        assert_eq!(inner.len, 2);

        // Remove one
        assert!(inner.remove_impl_no_wal("hello"));
        assert_eq!(inner.len, 1);
        assert!(!inner.contains("hello"));
        assert!(inner.contains("world"));

        // Remove again should fail
        assert!(!inner.remove_impl_no_wal("hello"));

        // Remove the other
        assert!(inner.remove_impl_no_wal("world"));
        assert_eq!(inner.len, 0);
    }

    #[test]
    fn test_unicode_support() {
        let mut inner: DiskBackedCharTrieInner<()> = DiskBackedCharTrieInner::new();

        // Test various Unicode characters
        let terms = vec![
            "こんにちは",     // Japanese
            "你好",           // Chinese
            "안녕하세요",     // Korean
            "مرحبا",          // Arabic
            "שלום",           // Hebrew
            "🎉🎊🎋",        // Emoji
            "café",           // Latin with diacritics
            "naïve",          // Latin with diacritics
        ];

        for term in &terms {
            assert!(inner.insert_impl_no_wal(term), "should insert: {}", term);
        }

        assert_eq!(inner.len, terms.len());

        // Verify all are present
        for term in &terms {
            assert!(inner.contains(term), "should contain: {}", term);
        }

        // Verify partial terms are not present
        assert!(!inner.contains("こん"));
        assert!(!inner.contains("你"));
        assert!(!inner.contains("🎉"));
    }

    #[test]
    fn test_prefix_sharing() {
        let mut inner: DiskBackedCharTrieInner<()> = DiskBackedCharTrieInner::new();

        // Terms that share prefixes
        inner.insert_impl_no_wal("a");
        inner.insert_impl_no_wal("ab");
        inner.insert_impl_no_wal("abc");
        inner.insert_impl_no_wal("abd");
        inner.insert_impl_no_wal("abcd");

        assert_eq!(inner.len, 5);

        // All should be present
        assert!(inner.contains("a"));
        assert!(inner.contains("ab"));
        assert!(inner.contains("abc"));
        assert!(inner.contains("abd"));
        assert!(inner.contains("abcd"));

        // Partial paths should not be final
        assert!(!inner.contains("abce"));
    }

    #[test]
    fn test_empty_string() {
        let mut inner: DiskBackedCharTrieInner<()> = DiskBackedCharTrieInner::new();

        // Empty string is valid
        assert!(inner.insert_impl_no_wal(""));
        assert!(inner.contains(""));
        assert_eq!(inner.len, 1);

        // Add another term
        inner.insert_impl_no_wal("hello");
        assert_eq!(inner.len, 2);
        assert!(inner.contains(""));
        assert!(inner.contains("hello"));
    }

    #[test]
    fn test_get_value() {
        let mut inner: DiskBackedCharTrieInner<i32> = DiskBackedCharTrieInner::new();

        inner.insert_impl_no_wal_with_value("one", 1);
        inner.insert_impl_no_wal_with_value("two", 2);
        inner.insert_impl_no_wal_with_value("three", 3);

        assert_eq!(inner.get("one"), Some(&1));
        assert_eq!(inner.get("two"), Some(&2));
        assert_eq!(inner.get("three"), Some(&3));
        assert_eq!(inner.get("four"), None);
    }

    #[test]
    fn test_value_update() {
        let mut inner: DiskBackedCharTrieInner<i32> = DiskBackedCharTrieInner::new();

        // First insert
        assert!(inner.insert_impl_no_wal_with_value("key", 100));
        assert_eq!(inner.get("key"), Some(&100));

        // Update (insert returns false but value is updated)
        assert!(!inner.insert_impl_no_wal_with_value("key", 200));
        assert_eq!(inner.get("key"), Some(&200));

        // Length unchanged
        assert_eq!(inner.len, 1);
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_wal_recovery_with_values() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_values.trie");

        // Create and insert with values
        {
            let mut inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::create(&path).expect("create");
            inner.insert("alpha").expect("insert");
            inner.insert("beta").expect("insert");
            inner.insert("gamma").expect("insert");
            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert_eq!(inner.len, 3);
            assert!(inner.contains("alpha"));
            assert!(inner.contains("beta"));
            assert!(inner.contains("gamma"));
            assert!(!inner.contains("delta"));
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_wal_recovery_mixed_operations() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_mixed.trie");

        // Create with mixed insert/remove
        {
            let mut inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::create(&path).expect("create");
            inner.insert("a").expect("insert");
            inner.insert("b").expect("insert");
            inner.insert("c").expect("insert");
            inner.remove("b").expect("remove");
            inner.insert("d").expect("insert");
            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert_eq!(inner.len, 3);
            assert!(inner.contains("a"));
            assert!(!inner.contains("b"));
            assert!(inner.contains("c"));
            assert!(inner.contains("d"));
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_checkpoint_and_disk_loading() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_checkpoint.trie");

        // Create, insert terms, and checkpoint
        {
            let mut inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::create(&path).expect("create");
            inner.insert("apple").expect("insert");
            inner.insert("banana").expect("insert");
            inner.insert("cherry").expect("insert");
            inner.checkpoint().expect("checkpoint");
        }

        // Reopen and verify data was loaded from disk
        {
            let inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert_eq!(inner.len, 3);
            assert!(inner.contains("apple"));
            assert!(inner.contains("banana"));
            assert!(inner.contains("cherry"));
            assert!(!inner.contains("date"));
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_checkpoint_with_unicode() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_unicode_checkpoint.trie");

        // Create with Unicode terms and checkpoint
        {
            let mut inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::create(&path).expect("create");
            inner.insert("こんにちは").expect("insert");
            inner.insert("你好").expect("insert");
            inner.insert("🎉").expect("insert");
            inner.insert("café").expect("insert");
            inner.checkpoint().expect("checkpoint");
        }

        // Reopen and verify Unicode data
        {
            let inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert_eq!(inner.len, 4);
            assert!(inner.contains("こんにちは"));
            assert!(inner.contains("你好"));
            assert!(inner.contains("🎉"));
            assert!(inner.contains("café"));
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_checkpoint_then_more_inserts() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_post_checkpoint.trie");

        // Create, checkpoint, then add more
        {
            let mut inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::create(&path).expect("create");
            inner.insert("first").expect("insert");
            inner.insert("second").expect("insert");
            inner.checkpoint().expect("checkpoint");

            // Add more after checkpoint
            inner.insert("third").expect("insert");
            inner.insert("fourth").expect("insert");
            inner.sync().expect("sync");
        }

        // Reopen - should have all 4 (disk + WAL replay)
        {
            let inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert_eq!(inner.len, 4);
            assert!(inner.contains("first"));
            assert!(inner.contains("second"));
            assert!(inner.contains("third"));
            assert!(inner.contains("fourth"));
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_checkpoint_empty_trie() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_empty_checkpoint.trie");

        // Create empty trie and checkpoint
        {
            let mut inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::create(&path).expect("create");
            inner.checkpoint().expect("checkpoint");
        }

        // Reopen empty trie
        {
            let inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert_eq!(inner.len, 0);
            assert!(!inner.contains("anything"));
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_multiple_checkpoints() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_multi_checkpoint.trie");

        // Create with multiple checkpoint cycles
        {
            let mut inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::create(&path).expect("create");

            inner.insert("one").expect("insert");
            inner.checkpoint().expect("checkpoint 1");

            inner.insert("two").expect("insert");
            inner.checkpoint().expect("checkpoint 2");

            inner.insert("three").expect("insert");
            inner.checkpoint().expect("checkpoint 3");
        }

        // Reopen and verify all data
        {
            let inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert_eq!(inner.len, 3);
            assert!(inner.contains("one"));
            assert!(inner.contains("two"));
            assert!(inner.contains("three"));
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_deep_trie_checkpoint() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_deep_checkpoint.trie");

        // Create with deeply nested terms
        {
            let mut inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::create(&path).expect("create");
            inner.insert("a").expect("insert");
            inner.insert("ab").expect("insert");
            inner.insert("abc").expect("insert");
            inner.insert("abcd").expect("insert");
            inner.insert("abcde").expect("insert");
            inner.insert("abcdef").expect("insert");
            inner.insert("abcdefg").expect("insert");
            inner.insert("abcdefgh").expect("insert");
            inner.checkpoint().expect("checkpoint");
        }

        // Reopen and verify all levels
        {
            let inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert_eq!(inner.len, 8);
            assert!(inner.contains("a"));
            assert!(inner.contains("ab"));
            assert!(inner.contains("abc"));
            assert!(inner.contains("abcd"));
            assert!(inner.contains("abcde"));
            assert!(inner.contains("abcdef"));
            assert!(inner.contains("abcdefg"));
            assert!(inner.contains("abcdefgh"));
            assert!(!inner.contains("abcdefghi"));
        }
    }

    // ==================== Phase C6: Atomic Operations with WAL ====================

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_increment_with_wal() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_increment.trie");

        // Create and increment
        {
            let mut inner: DiskBackedCharTrieInner<i64> =
                DiskBackedCharTrieInner::create(&path).expect("create");

            // First increment creates value
            let result = inner.increment("counter", 10).expect("increment");
            assert_eq!(result, 10);

            // Second increment adds to existing
            let result = inner.increment("counter", 5).expect("increment");
            assert_eq!(result, 15);

            // Negative increment
            let result = inner.increment("counter", -3).expect("increment");
            assert_eq!(result, 12);

            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: DiskBackedCharTrieInner<i64> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert!(inner.contains("counter"));
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_upsert_with_wal() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_upsert.trie");

        // Create and upsert
        {
            let mut inner: DiskBackedCharTrieInner<String> =
                DiskBackedCharTrieInner::create(&path).expect("create");

            // First upsert inserts
            let inserted = inner
                .upsert("key", "value1".to_string())
                .expect("upsert");
            assert!(inserted);
            assert!(inner.contains("key"));

            // Second upsert updates
            let inserted = inner
                .upsert("key", "value2".to_string())
                .expect("upsert");
            assert!(!inserted);
            assert!(inner.contains("key"));

            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: DiskBackedCharTrieInner<String> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert!(inner.contains("key"));
            assert_eq!(inner.len, 1);
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_compare_and_swap_with_wal() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_cas.trie");

        // Create and CAS
        {
            let mut inner: DiskBackedCharTrieInner<i32> =
                DiskBackedCharTrieInner::create(&path).expect("create");

            // CAS on non-existent key (expected None) should succeed
            let success = inner.compare_and_swap("key", None, 100).expect("cas");
            assert!(success);
            assert!(inner.contains("key"));

            // CAS with wrong expected value should fail
            let success = inner.compare_and_swap("key", Some(50), 200).expect("cas");
            assert!(!success);

            // CAS with correct expected value should succeed
            let success = inner.compare_and_swap("key", Some(100), 200).expect("cas");
            assert!(success);

            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: DiskBackedCharTrieInner<i32> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert!(inner.contains("key"));
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_fetch_add_with_wal() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_fetch_add.trie");

        // Create and fetch_add
        {
            let mut inner: DiskBackedCharTrieInner<i64> =
                DiskBackedCharTrieInner::create(&path).expect("create");

            // First fetch_add on non-existent key returns 0
            let old = inner.fetch_add("counter", 10).expect("fetch_add");
            assert_eq!(old, 0);

            // Second fetch_add returns previous value
            let old = inner.fetch_add("counter", 5).expect("fetch_add");
            assert_eq!(old, 10);

            // Third fetch_add
            let old = inner.fetch_add("counter", -3).expect("fetch_add");
            assert_eq!(old, 15);

            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: DiskBackedCharTrieInner<i64> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert!(inner.contains("counter"));
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_get_or_insert_with_wal() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_get_or_insert.trie");

        // Create and get_or_insert
        {
            let mut inner: DiskBackedCharTrieInner<String> =
                DiskBackedCharTrieInner::create(&path).expect("create");

            // First get_or_insert inserts
            let value = inner
                .get_or_insert("key", "default".to_string())
                .expect("get_or_insert");
            assert_eq!(value, "default");
            assert!(inner.contains("key"));

            // Second get_or_insert returns existing (does not insert)
            let value = inner
                .get_or_insert("key", "other".to_string())
                .expect("get_or_insert");
            assert_eq!(value, "default"); // Still the original

            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: DiskBackedCharTrieInner<String> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert!(inner.contains("key"));
            assert_eq!(inner.len, 1);
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_atomic_ops_recovery() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_atomic_recovery.trie");

        // Create with various atomic operations
        {
            let mut inner: DiskBackedCharTrieInner<i64> =
                DiskBackedCharTrieInner::create(&path).expect("create");

            // Use increment
            inner.increment("counter1", 100).expect("increment");
            inner.increment("counter1", 50).expect("increment");

            // Use fetch_add
            inner.fetch_add("counter2", 200).expect("fetch_add");
            inner.fetch_add("counter2", 25).expect("fetch_add");

            inner.sync().expect("sync");
        }

        // Reopen and verify recovery
        {
            let inner: DiskBackedCharTrieInner<i64> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert!(inner.contains("counter1"));
            assert!(inner.contains("counter2"));
            assert_eq!(inner.len, 2);
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_atomic_ops_with_checkpoint() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_atomic_checkpoint.trie");

        // Create, checkpoint, then more atomic ops
        {
            let mut inner: DiskBackedCharTrieInner<i64> =
                DiskBackedCharTrieInner::create(&path).expect("create");

            inner.increment("before_cp", 100).expect("increment");
            inner.checkpoint().expect("checkpoint");

            inner.increment("after_cp", 200).expect("increment");
            inner.sync().expect("sync");
        }

        // Reopen - should have both (disk + WAL replay)
        {
            let inner: DiskBackedCharTrieInner<i64> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert!(inner.contains("before_cp"));
            assert!(inner.contains("after_cp"));
            assert_eq!(inner.len, 2);
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_unicode_atomic_ops() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_unicode_atomic.trie");

        // Create with Unicode keys
        {
            let mut inner: DiskBackedCharTrieInner<i64> =
                DiskBackedCharTrieInner::create(&path).expect("create");

            inner.increment("カウンター", 10).expect("increment");
            inner.increment("计数器", 20).expect("increment");
            inner.increment("🔢", 30).expect("increment");

            inner.sync().expect("sync");
        }

        // Reopen and verify
        {
            let inner: DiskBackedCharTrieInner<i64> =
                DiskBackedCharTrieInner::open(&path).expect("open");
            assert!(inner.contains("カウンター"));
            assert!(inner.contains("计数器"));
            assert!(inner.contains("🔢"));
            assert_eq!(inner.len, 3);
        }
    }

    // ==================== Phase C7: Concurrency Tests ====================

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_optimistic_contains() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_optimistic_contains.trie");

        let mut inner: DiskBackedCharTrieInner<()> =
            DiskBackedCharTrieInner::create(&path).expect("create");

        inner.insert("hello").expect("insert");
        inner.insert("world").expect("insert");

        // Test optimistic reads
        let result = inner.contains_optimistic("hello", 10);
        assert_eq!(result, Some(true));

        let result = inner.contains_optimistic("world", 10);
        assert_eq!(result, Some(true));

        let result = inner.contains_optimistic("missing", 10);
        assert_eq!(result, Some(false));
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_optimistic_get() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_optimistic_get.trie");

        let mut inner: DiskBackedCharTrieInner<i64> =
            DiskBackedCharTrieInner::create(&path).expect("create");

        inner.increment("counter", 100).expect("increment");

        // Test optimistic get
        let result = inner.get_optimistic("counter", 10);
        assert!(result.is_some());
        let value = result.unwrap();
        assert_eq!(value, Some(100));

        let result = inner.get_optimistic("missing", 10);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), None);
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_version_tracking() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_version.trie");

        let mut inner: DiskBackedCharTrieInner<()> =
            DiskBackedCharTrieInner::create(&path).expect("create");

        let v0 = inner.current_version();
        assert_eq!(v0, 0); // Initial version

        inner.insert("a").expect("insert");
        let v1 = inner.current_version();
        assert_eq!(v1, 2); // After one write (begin + end = +2)

        inner.insert("b").expect("insert");
        let v2 = inner.current_version();
        assert_eq!(v2, 4); // After two writes

        // Not write-locked when idle
        assert!(!inner.is_write_locked());
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_epoch_management() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_epoch.trie");

        let inner: DiskBackedCharTrieInner<()> =
            DiskBackedCharTrieInner::create(&path).expect("create");

        // Initial state
        assert_eq!(inner.current_epoch(), 0);
        assert_eq!(inner.active_readers(), 0);

        // Enter epoch
        {
            let _guard = inner.enter_epoch();
            assert_eq!(inner.active_readers(), 1);

            // Can have multiple readers
            {
                let _guard2 = inner.enter_epoch();
                assert_eq!(inner.active_readers(), 2);
            }

            // One reader left
            assert_eq!(inner.active_readers(), 1);
        }

        // No readers left
        assert_eq!(inner.active_readers(), 0);

        // Advance epoch
        let old = inner.advance_epoch();
        assert_eq!(old, 0);
        assert_eq!(inner.current_epoch(), 1);
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_retry_stats() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_stats.trie");

        let mut inner: DiskBackedCharTrieInner<()> =
            DiskBackedCharTrieInner::create(&path).expect("create");

        inner.insert("test").expect("insert");

        // Perform some optimistic reads
        for _ in 0..10 {
            let _ = inner.contains_optimistic("test", 5);
        }

        let stats = inner.retry_stats_snapshot();
        assert!(stats.successful >= 10); // At least 10 successful reads
        // Retry count should be low (no concurrent writers)
        assert_eq!(stats.retries, 0);
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_concurrent_readers() {
        use std::sync::Arc;
        use std::thread;
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_concurrent.trie");

        // Create and populate
        {
            let mut inner: DiskBackedCharTrieInner<()> =
                DiskBackedCharTrieInner::create(&path).expect("create");

            for i in 0..100 {
                inner.insert(&format!("term{}", i)).expect("insert");
            }
            inner.sync().expect("sync");
        }

        // Reopen and spawn multiple reader threads
        let inner = Arc::new(
            DiskBackedCharTrieInner::<()>::open(&path).expect("open")
        );

        let handles: Vec<_> = (0..4)
            .map(|t| {
                let inner = inner.clone();
                thread::spawn(move || {
                    let mut found = 0;
                    for i in 0..100 {
                        let _guard = inner.enter_epoch();
                        if let Some(true) = inner.contains_optimistic(&format!("term{}", i), 10) {
                            found += 1;
                        }
                    }
                    (t, found)
                })
            })
            .collect();

        for handle in handles {
            let (thread_id, found) = handle.join().expect("thread join");
            assert_eq!(found, 100, "Thread {} should find all 100 terms", thread_id);
        }
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_try_contains_optimistic() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_try_contains.trie");

        let mut inner: DiskBackedCharTrieInner<()> =
            DiskBackedCharTrieInner::create(&path).expect("create");

        inner.insert("apple").expect("insert");

        // Single optimistic read should succeed
        let result = inner.try_contains_optimistic("apple");
        assert_eq!(result, Some(true));

        let result = inner.try_contains_optimistic("banana");
        assert_eq!(result, Some(false));
    }

    #[cfg(feature = "persistent-artrie")]
    #[test]
    fn test_unicode_optimistic() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test_unicode_optimistic.trie");

        let mut inner: DiskBackedCharTrieInner<()> =
            DiskBackedCharTrieInner::create(&path).expect("create");

        inner.insert("日本語").expect("insert");
        inner.insert("中文").expect("insert");
        inner.insert("🎉🎊🎋").expect("insert");

        // Test optimistic reads with Unicode
        assert_eq!(inner.contains_optimistic("日本語", 10), Some(true));
        assert_eq!(inner.contains_optimistic("中文", 10), Some(true));
        assert_eq!(inner.contains_optimistic("🎉🎊🎋", 10), Some(true));
        assert_eq!(inner.contains_optimistic("한글", 10), Some(false));
    }
}
