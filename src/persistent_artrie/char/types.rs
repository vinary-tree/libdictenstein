//! Shared types for PersistentARTrieChar implementations.
//!
//! This module contains the core types used by both the in-memory and disk-backed
//! variants of the character-level trie.

use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
use crate::value::DictionaryValue;

use super::nodes::CharNode;

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

/// Reference to a node in the trie for parent pointer backtracking.
///
/// Used for reverse lookups (value → term) by storing the location
/// of the node that contains each value, enabling O(k) reconstruction
/// of the term by backtracking parent pointers.
///
/// # Layout (8 bytes)
///
/// ```text
/// ┌─────────────────┬─────────────────┐
/// │ arena_id (u32)  │ slot_index (u32)│
/// └─────────────────┴─────────────────┘
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct NodeRef {
    /// Arena ID where the node resides (u32::MAX = NULL)
    pub arena_id: u32,
    /// Slot index within the arena (u32::MAX = NULL)
    pub slot_index: u32,
}

impl NodeRef {
    /// Null reference (no node)
    pub const NULL: Self = Self {
        arena_id: u32::MAX,
        slot_index: u32::MAX,
    };

    /// Create a new NodeRef from arena and slot indices.
    #[inline]
    pub const fn new(arena_id: u32, slot_index: u32) -> Self {
        Self {
            arena_id,
            slot_index,
        }
    }

    /// Check if this is a null reference.
    #[inline]
    pub const fn is_null(&self) -> bool {
        self.arena_id == u32::MAX && self.slot_index == u32::MAX
    }

    /// Convert to bytes for serialization.
    #[inline]
    pub fn to_bytes(&self) -> [u8; 8] {
        let mut bytes = [0u8; 8];
        bytes[0..4].copy_from_slice(&self.arena_id.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.slot_index.to_le_bytes());
        bytes
    }

    /// Create from bytes for deserialization.
    #[inline]
    pub fn from_bytes(bytes: &[u8; 8]) -> Self {
        Self {
            arena_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            slot_index: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        }
    }
}

impl Default for NodeRef {
    fn default() -> Self {
        Self::NULL
    }
}

/// Mode of enhanced recovery (with epoch/per-node logging integration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnhancedRecoveryMode {
    /// File was created new (didn't exist before)
    CreatedNew,
    /// Normal open, no recovery needed
    Normal,
    /// Recovered from WAL after last checkpoint
    WalReplay,
    /// Rebuilt from WAL archive segments
    RebuiltFromWal,
    /// Rebuilt from WAL archive files
    RebuiltFromArchives,
    /// Recovered using epoch-based checkpointing
    EpochRecovery,
    /// Recovered using per-node logging (O(dirty nodes))
    PerNodeRecovery,
}

impl EnhancedRecoveryMode {
    /// Returns true if recovery required rebuilding from WAL
    pub fn required_rebuild(&self) -> bool {
        matches!(
            self,
            EnhancedRecoveryMode::RebuiltFromWal | EnhancedRecoveryMode::RebuiltFromArchives
        )
    }

    /// Returns true if this was a normal open (no recovery)
    pub fn is_normal(&self) -> bool {
        matches!(
            self,
            EnhancedRecoveryMode::Normal | EnhancedRecoveryMode::CreatedNew
        )
    }
}

/// Statistics from enhanced recovery.
#[derive(Debug, Clone)]
pub struct EnhancedRecoveryStats {
    /// The recovery mode used
    pub mode: EnhancedRecoveryMode,
    /// Total time for recovery in milliseconds
    pub duration_ms: u64,
    /// Number of WAL records replayed
    pub records_replayed: usize,
    /// Number of epochs recovered (for epoch-based recovery)
    pub epochs_recovered: usize,
    /// Number of dirty nodes recovered (for per-node logging)
    pub dirty_nodes_recovered: usize,
    /// Number of archive segments used
    pub archive_segments_used: usize,
}

impl EnhancedRecoveryStats {
    /// Create stats for normal open (no recovery)
    pub fn normal() -> Self {
        Self {
            mode: EnhancedRecoveryMode::Normal,
            duration_ms: 0,
            records_replayed: 0,
            epochs_recovered: 0,
            dirty_nodes_recovered: 0,
            archive_segments_used: 0,
        }
    }

    /// Create stats for new file creation
    pub fn created_new() -> Self {
        Self {
            mode: EnhancedRecoveryMode::CreatedNew,
            duration_ms: 0,
            records_replayed: 0,
            epochs_recovered: 0,
            dirty_nodes_recovered: 0,
            archive_segments_used: 0,
        }
    }
}

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
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
            entry_count: u64::from_le_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22],
                bytes[23],
            ]),
            checkpoint_lsn: u64::from_le_bytes([
                bytes[24], bytes[25], bytes[26], bytes[27], bytes[28], bytes[29], bytes[30],
                bytes[31],
            ]),
            header_checksum: u32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]),
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
    pub fn from_bytes_verified(
        bytes: &[u8; CHAR_FILE_HEADER_SIZE],
    ) -> crate::persistent_artrie::error::Result<Self> {
        use crate::persistent_artrie::error::PersistentARTrieError;

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
    pub fn validate(&self) -> crate::persistent_artrie::error::Result<()> {
        use crate::persistent_artrie::error::PersistentARTrieError;

        if self.magic != CHAR_TRIE_MAGIC {
            // Convert [u8; 4] to u64 for the error type
            let expected = u64::from_le_bytes([
                CHAR_TRIE_MAGIC[0],
                CHAR_TRIE_MAGIC[1],
                CHAR_TRIE_MAGIC[2],
                CHAR_TRIE_MAGIC[3],
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

impl Default for CharTrieFileHeader {
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

/// A term with its arena location for page-aware batching.
///
/// Used by `iter_prefix_with_arena()` to enable I/O-efficient batch operations
/// by grouping terms that reside in the same disk arena/page.
#[derive(Debug, Clone)]
pub struct PrefixTermWithArena {
    /// The term string
    pub term: String,
    /// The arena ID where this term's node resides (None for in-memory nodes)
    pub arena_id: Option<u32>,
}

/// A term with its value and arena location for page-aware merge operations.
///
/// Used by `iter_prefix_with_values_and_arena()` to enable I/O-efficient batch
/// operations by grouping terms that reside in the same disk arena/page.
/// This is the same pattern used by `remove_prefix_batched()`.
#[derive(Debug, Clone)]
pub struct PrefixTermWithValueAndArena<V> {
    /// The term string
    pub term: String,
    /// The value associated with this term
    pub value: V,
    /// The arena ID where this term's node resides (None for in-memory nodes)
    pub arena_id: Option<u32>,
}

/// A trie node for the char trie (CharNode-based implementation)
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
/// # Ownership invariant
///
/// This is an exclusively owned serialization tree, not the live `Arc`-shared
/// overlay. Its raw child slots obey all of the following rules:
///
/// - Every in-memory slot contains the address of one `Box<CharTrieNodeInner<V>>`
///   transferred by `Box::into_raw`.
/// - That address occurs in exactly one owning slot. Cloning allocates a fresh
///   box for every in-memory descendant; it never copies an owning address.
/// - An on-disk slot owns no allocation and is never dereferenced or reclaimed
///   as a box.
/// - Replacement and removal detach the slot before reconstructing its box, and
///   reconstruct a given box at most once.
/// - Shared child references are tied to `&self`. Exclusive child references
///   are created only by `get_or_create_child` while `&mut self` excludes every
///   shared or competing exclusive borrow of the complete owned tree.
///
/// The fields and raw-slot insertion seam are crate-private so safe downstream
/// code cannot forge a second owning slot. The live concurrent representation
/// uses `Child::InMem(Arc<_>)` instead and never relies on this invariant.
pub struct CharTrieNodeInner<V: DictionaryValue> {
    /// The adaptive radix node structure (N4/N16/N48/Bucket)
    /// Children are stored as raw pointers encoded in the CharNode's SwizzledPtr fields.
    pub(crate) node: CharNode,
    /// Optional value associated with this node (stored separately from CharNode)
    pub(crate) value: Option<V>,
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
                new_node.add_child_growing(key, cloned_ptr).expect(
                    "invariant violation: CharTrieNodeInner::clone is cloning into a node of \
                     the same type/capacity as the source, so add_child_growing cannot exceed \
                     capacity — if this fires, a CharNode*::grow capacity-tracking bug has \
                     corrupted the cloned subtree",
                );
            }
        }

        Self {
            node: new_node,
            value: self.value.clone(),
        }
    }
}

// Iterative Drop: reclaim each uniquely owned child box exactly once without
// recursing down a potentially long serialization spine.
impl<V: DictionaryValue> Drop for CharTrieNodeInner<V> {
    fn drop(&mut self) {
        // Detach our complete child store first. Re-entrant `Drop` on a box
        // popped below therefore observes an empty node and cannot recurse.
        let root_node = std::mem::replace(&mut self.node, CharNode::new_node4());
        let mut worklist: Vec<*mut CharTrieNodeInner<V>> = root_node
            .iter_children()
            .filter_map(|(_, ptr)| {
                ptr.as_ptr::<CharTrieNodeInner<V>>()
                    .map(|raw| raw.cast_mut())
            })
            .collect();

        while let Some(raw) = worklist.pop() {
            // SAFETY: the ownership invariant gives this detached worklist the
            // only owning occurrence of `raw`. Reconstruct exactly one Box.
            let mut child = unsafe { Box::from_raw(raw) };
            let child_node = std::mem::replace(&mut child.node, CharNode::new_node4());
            worklist.extend(child_node.iter_children().filter_map(|(_, ptr)| {
                ptr.as_ptr::<CharTrieNodeInner<V>>()
                    .map(|descendant| descendant.cast_mut())
            }));
            // `child.node` is empty, so dropping `child` is constant-stack.
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

    /// Get a shared child by character.
    ///
    /// The returned reference cannot outlive or overlap mutation of `self`.
    /// Crate-private fields prevent safe code from duplicating an owning slot,
    /// so the pointee remains allocated for the complete borrow.
    pub fn get_child(&self, c: char) -> Option<&CharTrieNodeInner<V>> {
        self.node
            .find_child(c as u32)
            .and_then(|ptr| ptr.as_ptr::<CharTrieNodeInner<V>>())
            .map(|ptr| {
                // SAFETY: the ownership invariant makes every in-memory slot a
                // live unique Box allocation owned transitively by `self`.
                unsafe { &*ptr }
            })
    }

    /// Insert a child, returning the old child if it existed
    pub fn insert_child(
        &mut self,
        c: char,
        child: CharTrieNodeInner<V>,
    ) -> Option<Box<CharTrieNodeInner<V>>> {
        let key = c as u32;

        // Detach an existing slot before reconstructing an owned in-memory Box.
        // An on-disk slot owns no Box and is simply replaced.
        let old_ptr = self.node.remove_child_shrinking(key).map(|(old, shrunk)| {
            if let Some(new_node) = shrunk {
                self.node = new_node;
            }
            old
        });

        let new_raw = Box::into_raw(Box::new(child));
        let new_ptr = SwizzledPtr::in_memory(new_raw);
        match self.node.add_child_growing(key, new_ptr) {
            Ok(Some(grown)) => self.node = grown,
            Ok(None) => {}
            Err(_) => {
                // SAFETY: publication failed, so `new_raw` never entered an
                // owning slot and remains the caller's unique allocation.
                drop(unsafe { Box::from_raw(new_raw) });
                panic!("invariant violation: detached child key remained present");
            }
        }

        old_ptr
            .and_then(|ptr| ptr.as_ptr::<CharTrieNodeInner<V>>())
            .map(|raw| {
                // SAFETY: the old slot was detached above and was its unique
                // owning occurrence. Ownership transfers to the returned Box.
                unsafe { Box::from_raw(raw.cast_mut()) }
            })
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
    pub(crate) fn insert_child_ptr(
        &mut self,
        c: char,
        child_ptr: SwizzledPtr,
    ) -> Option<SwizzledPtr> {
        assert!(
            !child_ptr.is_swizzled(),
            "raw in-memory ownership must enter through insert_child"
        );
        let key = c as u32;

        // Check if child already exists
        if let Some(existing) = self.node.find_child(key) {
            assert!(
                !existing.is_swizzled(),
                "insert_child_ptr cannot detach an owned in-memory child; use insert_child"
            );
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
            let ptr = self
                .node
                .find_child(key)
                .and_then(|slot| slot.as_ptr::<CharTrieNodeInner<V>>())
                .expect("owned child slot must contain an in-memory allocation");
            // SAFETY: `&mut self` excludes every shared or competing mutable
            // borrow of this exclusively owned tree; the invariant makes `ptr`
            // the sole owning child slot for the duration of the returned borrow.
            return unsafe { &mut *ptr.cast_mut() };
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
                unsafe {
                    drop(Box::from_raw(ptr));
                }
                let existing = self
                    .node
                    .find_child(key)
                    .and_then(|slot| slot.as_ptr::<CharTrieNodeInner<V>>())
                    .expect("existing owned child must be in memory");
                // SAFETY: same unique-parent argument as the existing-child arm.
                return unsafe { &mut *existing.cast_mut() };
            }
        }

        // SAFETY: `ptr` was freshly allocated and has exactly one owning slot;
        // `&mut self` excludes aliases for the returned borrow.
        unsafe { &mut *ptr }
    }

    /// Remove a child by character, returning the removed child if it existed
    pub fn remove_child(&mut self, c: char) -> Option<Box<CharTrieNodeInner<V>>> {
        let key = c as u32;

        // Only in-memory slots own a Box. Leave on-disk references untouched.
        self.node
            .find_child(key)
            .and_then(|slot| slot.as_ptr::<CharTrieNodeInner<V>>())?;

        let (removed, shrunk) = self.node.remove_child_shrinking(key)?;
        if let Some(new_node) = shrunk {
            self.node = new_node;
        }
        let ptr = removed
            .as_ptr::<CharTrieNodeInner<V>>()
            .expect("detached owned child must remain in memory");

        // SAFETY: removal detached the sole owning slot before reconstruction.
        Some(unsafe { Box::from_raw(ptr.cast_mut()) })
    }

    /// Iterate over children
    ///
    /// Returns an iterator over `(char, &CharTrieNodeInner<V>)` pairs.
    pub fn iter_children(&self) -> impl Iterator<Item = (char, &CharTrieNodeInner<V>)> {
        self.node.iter_children().filter_map(|(key, ptr)| {
            ptr.as_ptr::<CharTrieNodeInner<V>>().map(|p| {
                let c = char::from_u32(key).unwrap_or('\u{FFFD}');
                // SAFETY: shared access is tied to `&self`, and the ownership
                // invariant keeps the unique child allocation live.
                let child_ref = unsafe { &*p };
                (c, child_ref)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_char_trie_file_header() {
        let mut header = CharTrieFileHeader::new();
        header.root_ptr = 42;
        header.entry_count = 100;
        header.checkpoint_lsn = 50;

        let bytes = header.to_bytes_with_checksum();
        let header2 = CharTrieFileHeader::from_bytes(&bytes);

        assert_eq!(header2.magic, CHAR_TRIE_MAGIC);
        assert_eq!(header2.root_ptr, 42);
        assert_eq!(header2.entry_count, 100);
        assert_eq!(header2.checkpoint_lsn, 50);
        assert!(header2.verify_checksum());
    }

    #[test]
    fn test_char_trie_node_inner() {
        let mut root = CharTrieNodeInner::<i32>::new();
        assert!(!root.is_final());
        assert_eq!(root.num_children(), 0);

        // Insert a child
        let child = CharTrieNodeInner::new();
        root.insert_child('a', child);
        assert_eq!(root.num_children(), 1);

        // Get child
        let c = root.get_child('a');
        assert!(c.is_some());

        // Get or create
        let child_mut = root.get_or_create_child('b');
        child_mut.set_final(true);
        assert_eq!(root.num_children(), 2);

        // Remove child
        let removed = root.remove_child('a');
        assert!(removed.is_some());
        assert_eq!(root.num_children(), 1);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "large constrained-stack stress is covered outside Miri"
    )]
    fn owned_serialization_tree_drop_is_stack_safe() {
        std::thread::Builder::new()
            .name("owned-char-tree-drop-stack-gate".to_string())
            .stack_size(64 * 1024)
            .spawn(|| {
                let mut root = CharTrieNodeInner::<u64>::new();
                let mut cursor = &mut root;
                for _ in 0..16_384 {
                    cursor = cursor.get_or_create_child('x');
                }
                cursor.set_final(true);
                // `root` is dropped on this constrained stack. Iterative Drop
                // must reclaim the complete unique-box chain without recursion.
            })
            .expect("spawn constrained-stack thread")
            .join()
            .expect("owned char tree drop must not overflow its stack");
    }
}
