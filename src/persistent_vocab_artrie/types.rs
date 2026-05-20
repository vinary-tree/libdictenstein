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

use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
use crate::persistent_artrie_char::nodes::CharNode;

// Re-export NodeRef from persistent_artrie_char
pub use crate::persistent_artrie_char::types::NodeRef;

/// Magic bytes for vocab trie file: "VOCB"
pub const VOCAB_TRIE_MAGIC: [u8; 4] = *b"VOCB";

/// File header size in bytes (extended from 64 to 96 bytes)
pub const VOCAB_FILE_HEADER_SIZE: usize = 96;

/// Header format version 1
pub const VOCAB_HEADER_VERSION_V1: u8 = 1;

/// Default buffer pool size (number of pages)
pub const DEFAULT_VOCAB_BUFFER_POOL_SIZE: usize = 256;

/// Default LRU cache size for reverse lookups
pub const DEFAULT_REVERSE_CACHE_SIZE: usize = 50_000;

/// Serialization flag indicating node has parent pointer data
pub const FLAG_HAS_PARENT_POINTER: u8 = 0x10;

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
                bytes[8], bytes[9], bytes[10], bytes[11],
                bytes[12], bytes[13], bytes[14], bytes[15],
            ]),
            entry_count: u64::from_le_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19],
                bytes[20], bytes[21], bytes[22], bytes[23],
            ]),
            block_count: u32::from_le_bytes([
                bytes[24], bytes[25], bytes[26], bytes[27],
            ]),
            _pad1: u32::from_le_bytes([
                bytes[28], bytes[29], bytes[30], bytes[31],
            ]),
            checkpoint_lsn: u64::from_le_bytes([
                bytes[32], bytes[33], bytes[34], bytes[35],
                bytes[36], bytes[37], bytes[38], bytes[39],
            ]),
            header_checksum: u32::from_le_bytes([
                bytes[40], bytes[41], bytes[42], bytes[43],
            ]),
            _padding: {
                let mut arr = [0u8; 20];
                arr.copy_from_slice(&bytes[44..64]);
                arr
            },
            start_index: u64::from_le_bytes([
                bytes[64], bytes[65], bytes[66], bytes[67],
                bytes[68], bytes[69], bytes[70], bytes[71],
            ]),
            next_index: u64::from_le_bytes([
                bytes[72], bytes[73], bytes[74], bytes[75],
                bytes[76], bytes[77], bytes[78], bytes[79],
            ]),
            reverse_index_capacity: u64::from_le_bytes([
                bytes[80], bytes[81], bytes[82], bytes[83],
                bytes[84], bytes[85], bytes[86], bytes[87],
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
                VOCAB_TRIE_MAGIC[0], VOCAB_TRIE_MAGIC[1],
                VOCAB_TRIE_MAGIC[2], VOCAB_TRIE_MAGIC[3],
                0, 0, 0, 0,
            ]);
            let found = u64::from_le_bytes([
                self.magic[0], self.magic[1], self.magic[2], self.magic[3],
                0, 0, 0, 0,
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

/// A vocabulary trie node with parent pointers for reverse lookup.
///
/// This node type extends the base `CharNode` with parent pointer information
/// to enable O(k) term reconstruction via backtracking.
///
/// # Memory Layout
///
/// | Field        | Size   | Description                              |
/// |--------------|--------|------------------------------------------|
/// | inner        | varies | Underlying CharNode (N4/N16/N48/Bucket)  |
/// | parent       | 8B     | NodeRef to parent node                   |
/// | parent_edge  | 4B     | Unicode code point of edge from parent   |
/// | value        | 8B+    | Optional vocabulary index                |
///
/// Total overhead vs `CharTrieNodeInner<V>`: ~12 bytes per node
pub struct VocabTrieNode {
    /// The underlying char node (reuses CharNode4/16/48/Bucket)
    pub inner: CharNode,
    /// Reference to parent node (NULL for root)
    pub parent: NodeRef,
    /// Edge label from parent (Unicode code point)
    pub parent_edge: u32,
    /// Vocabulary index if this is a final node (None if not final)
    pub value: Option<u64>,
}

impl std::fmt::Debug for VocabTrieNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VocabTrieNode")
            .field("is_final", &self.inner.is_final())
            .field("children_count", &self.inner.num_children())
            .field("has_value", &self.value.is_some())
            .field("parent_is_null", &self.parent.is_null())
            .field("parent_edge", &char::from_u32(self.parent_edge))
            .finish()
    }
}

impl Clone for VocabTrieNode {
    fn clone(&self) -> Self {
        // Create a new node with the same type
        let mut new_node = match &self.inner {
            CharNode::N4(_) => CharNode::new_node4(),
            CharNode::N16(_) => CharNode::new_node16(),
            CharNode::N48(_) => CharNode::new_node48(),
            CharNode::Bucket(_) => CharNode::new_bucket(),
        };

        // Copy the final flag
        new_node.header_mut().set_final(self.inner.is_final());

        // Deep clone all children
        for (key, child_ptr) in self.inner.iter_children() {
            if let Some(ptr) = child_ptr.as_ptr::<VocabTrieNode>() {
                // Safety: ptr is valid because we control all SwizzledPtr creation
                let child_ref = unsafe { &*ptr };
                let cloned_child = Box::new(child_ref.clone());
                let cloned_ptr = SwizzledPtr::in_memory(Box::into_raw(cloned_child));
                new_node.add_child_growing(key, cloned_ptr).expect(
                    "invariant violation: VocabTrieNode::clone is cloning into a node of the \
                     same type/capacity as the source, so add_child_growing cannot exceed \
                     capacity — if this fires, a Node*::grow capacity-tracking bug has \
                     corrupted the cloned subtree",
                );
            }
        }

        Self {
            inner: new_node,
            parent: self.parent,
            parent_edge: self.parent_edge,
            value: self.value,
        }
    }
}

impl Drop for VocabTrieNode {
    fn drop(&mut self) {
        // Collect child pointers first to avoid iterator invalidation
        let child_ptrs: Vec<_> = self.inner.iter_children()
            .filter_map(|(_, ptr)| ptr.as_ptr::<VocabTrieNode>())
            .collect();

        // Free each child
        for ptr in child_ptrs {
            // Safety: We created these pointers via Box::into_raw() during insertion
            unsafe {
                drop(Box::from_raw(ptr as *mut VocabTrieNode));
            }
        }
    }
}

impl Default for VocabTrieNode {
    fn default() -> Self {
        Self::new()
    }
}

impl VocabTrieNode {
    /// Create a new empty node
    pub fn new() -> Self {
        Self {
            inner: CharNode::new_node4(),
            parent: NodeRef::NULL,
            parent_edge: 0,
            value: None,
        }
    }

    /// Create a new node with parent information
    pub fn with_parent(parent: NodeRef, parent_edge: char) -> Self {
        Self {
            inner: CharNode::new_node4(),
            parent,
            parent_edge: parent_edge as u32,
            value: None,
        }
    }

    /// Check if this node is final (has a vocabulary index)
    #[inline]
    pub fn is_final(&self) -> bool {
        self.inner.is_final()
    }

    /// Set the final flag
    #[inline]
    pub fn set_final(&mut self, is_final: bool) {
        self.inner.header_mut().set_final(is_final);
    }

    /// Get the vocabulary index if this is a final node
    #[inline]
    pub fn get_value(&self) -> Option<u64> {
        self.value
    }

    /// Set the vocabulary index
    #[inline]
    pub fn set_value(&mut self, value: u64) {
        self.value = Some(value);
        self.set_final(true);
    }

    /// Get the number of children
    #[inline]
    pub fn num_children(&self) -> usize {
        self.inner.num_children()
    }

    /// Get a child by character
    pub fn get_child(&self, c: char) -> Option<&VocabTrieNode> {
        self.inner.find_child(c as u32)
            .and_then(|ptr| ptr.as_ptr::<VocabTrieNode>())
            .map(|ptr| {
                // Safety: We control all SwizzledPtr creation; ptr is valid
                unsafe { &*ptr }
            })
    }

    /// Get a child mutably by character
    pub fn get_child_mut(&mut self, c: char) -> Option<&mut VocabTrieNode> {
        self.inner.find_child(c as u32)
            .and_then(|ptr| ptr.as_ptr::<VocabTrieNode>())
            .map(|ptr| {
                // Safety: We control all SwizzledPtr creation; ptr is valid
                unsafe { &mut *(ptr as *mut VocabTrieNode) }
            })
    }

    /// Get or create a child for the given character, setting parent information
    pub fn get_or_create_child(&mut self, c: char, self_ref: NodeRef) -> &mut VocabTrieNode {
        let key = c as u32;

        // Check if child already exists
        if self.inner.find_child(key).is_some() {
            return self.get_child_mut(c).expect("child should exist");
        }

        // Create new child with parent pointer
        let mut new_child = VocabTrieNode::new();
        new_child.parent = self_ref;
        new_child.parent_edge = key;

        let new_child_box = Box::new(new_child);
        let ptr = Box::into_raw(new_child_box);
        let swizzled = SwizzledPtr::in_memory(ptr);

        // Add to node, handling potential growth
        match self.inner.add_child_growing(key, swizzled) {
            Ok(Some(grown)) => {
                self.inner = grown;
            }
            Ok(None) => {
                // No growth needed
            }
            Err(_) => {
                // Key already exists (shouldn't happen, but handle gracefully)
                unsafe { drop(Box::from_raw(ptr)); }
                return self.get_child_mut(c).expect("child should exist");
            }
        }

        // Safety: We just inserted this pointer
        unsafe { &mut *ptr }
    }

    /// Insert a child, returning the old child if it existed
    pub fn insert_child(&mut self, c: char, child: VocabTrieNode) -> Option<Box<VocabTrieNode>> {
        let key = c as u32;

        // Check if child already exists
        if let Some(existing_ptr) = self.inner.find_child(key) {
            if let Some(ptr) = existing_ptr.as_ptr::<VocabTrieNode>() {
                // Remove old child and recover the Box
                if let Some((_, shrunk)) = self.inner.remove_child_shrinking(key) {
                    if let Some(new_node) = shrunk {
                        self.inner = new_node;
                    }
                }
                // Safety: ptr was created via Box::into_raw()
                let old_child = unsafe { Box::from_raw(ptr as *mut VocabTrieNode) };

                // Insert the new child
                let new_ptr = SwizzledPtr::in_memory(Box::into_raw(Box::new(child)));
                if let Ok(Some(grown)) = self.inner.add_child_growing(key, new_ptr) {
                    self.inner = grown;
                }

                return Some(old_child);
            }
        }

        // No existing child, just insert
        let new_ptr = SwizzledPtr::in_memory(Box::into_raw(Box::new(child)));
        if let Ok(Some(grown)) = self.inner.add_child_growing(key, new_ptr) {
            self.inner = grown;
        }

        None
    }

    /// Remove a child by character, returning the removed child if it existed
    pub fn remove_child(&mut self, c: char) -> Option<Box<VocabTrieNode>> {
        let key = c as u32;

        // Check if child exists and get its pointer
        let ptr = self.inner.find_child(key)
            .and_then(|p| p.as_ptr::<VocabTrieNode>())?;

        // Remove from node
        if let Some((_, shrunk)) = self.inner.remove_child_shrinking(key) {
            if let Some(new_node) = shrunk {
                self.inner = new_node;
            }
        }

        // Safety: ptr was created via Box::into_raw()
        Some(unsafe { Box::from_raw(ptr as *mut VocabTrieNode) })
    }

    /// Iterate over children
    pub fn iter_children(&self) -> impl Iterator<Item = (char, &VocabTrieNode)> {
        self.inner.iter_children()
            .filter_map(|(key, ptr)| {
                ptr.as_ptr::<VocabTrieNode>()
                    .map(|p| {
                        let c = char::from_u32(key).unwrap_or('\u{FFFD}');
                        // Safety: We control all SwizzledPtr creation; ptr is valid
                        let child_ref = unsafe { &*p };
                        (c, child_ref)
                    })
            })
    }

    /// Reconstruct the term by backtracking parent pointers.
    ///
    /// This function is used for reverse lookup (index → term).
    /// It walks up the tree via parent pointers, collecting edge labels,
    /// then reverses to get the term.
    ///
    /// # Arguments
    ///
    /// * `node_lookup` - Function to look up a node by its NodeRef
    ///
    /// # Returns
    ///
    /// The reconstructed term string
    pub fn reconstruct_term<'a, F>(&'a self, node_lookup: F) -> String
    where
        F: Fn(NodeRef) -> Option<&'a VocabTrieNode>,
    {
        let mut chars: Vec<char> = Vec::new();
        let mut current = self;

        // Walk up the tree collecting edge labels
        while !current.parent.is_null() {
            if let Some(c) = char::from_u32(current.parent_edge) {
                chars.push(c);
            }
            match node_lookup(current.parent) {
                Some(parent) => current = parent,
                None => break,
            }
        }

        // Reverse to get correct order
        chars.reverse();
        chars.into_iter().collect()
    }
}

/// Root node type for vocabulary trie
pub enum VocabTrieRoot {
    /// Empty trie (no root yet)
    Empty,
    /// Root is a trie node
    Node(Box<VocabTrieNode>),
}

impl std::fmt::Debug for VocabTrieRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VocabTrieRoot::Empty => write!(f, "VocabTrieRoot::Empty"),
            VocabTrieRoot::Node(node) => {
                write!(f, "VocabTrieRoot::Node({} children)", node.num_children())
            }
        }
    }
}

impl Default for VocabTrieRoot {
    fn default() -> Self {
        VocabTrieRoot::Empty
    }
}

impl Clone for VocabTrieRoot {
    fn clone(&self) -> Self {
        match self {
            VocabTrieRoot::Empty => VocabTrieRoot::Empty,
            VocabTrieRoot::Node(node) => VocabTrieRoot::Node(Box::new((**node).clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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

    #[test]
    fn test_vocab_trie_node() {
        let mut root = VocabTrieNode::new();
        assert!(!root.is_final());
        assert_eq!(root.num_children(), 0);

        // Insert a child
        let child = VocabTrieNode::new();
        root.insert_child('a', child);
        assert_eq!(root.num_children(), 1);

        // Get child
        let c = root.get_child('a');
        assert!(c.is_some());

        // Get or create
        let root_ref = NodeRef::new(0, 0);
        let child_mut = root.get_or_create_child('b', root_ref);
        child_mut.set_final(true);
        child_mut.set_value(42);
        assert_eq!(root.num_children(), 2);

        // Check child has correct parent info
        let child_b = root.get_child('b').unwrap();
        assert_eq!(child_b.parent, root_ref);
        assert_eq!(child_b.parent_edge, 'b' as u32);
        assert_eq!(child_b.get_value(), Some(42));

        // Remove child
        let removed = root.remove_child('a');
        assert!(removed.is_some());
        assert_eq!(root.num_children(), 1);
    }

    #[test]
    fn test_vocab_trie_root() {
        let root: VocabTrieRoot = VocabTrieRoot::Empty;
        assert!(matches!(root, VocabTrieRoot::Empty));

        let node = VocabTrieNode::new();
        let root = VocabTrieRoot::Node(Box::new(node));
        assert!(matches!(root, VocabTrieRoot::Node(_)));
    }

    #[test]
    fn test_term_reconstruction() {
        // Build a small trie: "abc"
        let mut root = VocabTrieNode::new();
        let root_ref = NodeRef::new(0, 0);

        // Create 'a' -> 'b' -> 'c'
        let a_child = root.get_or_create_child('a', root_ref);
        let a_ref = NodeRef::new(0, 1);
        a_child.parent = root_ref;
        a_child.parent_edge = 'a' as u32;

        let b_child = a_child.get_or_create_child('b', a_ref);
        let b_ref = NodeRef::new(0, 2);
        b_child.parent = a_ref;
        b_child.parent_edge = 'b' as u32;

        let c_child = b_child.get_or_create_child('c', b_ref);
        c_child.parent = b_ref;
        c_child.parent_edge = 'c' as u32;
        c_child.set_value(0);

        // Create a lookup map for reconstruction
        let mut nodes: HashMap<NodeRef, &VocabTrieNode> = HashMap::new();
        nodes.insert(root_ref, &root);

        // For this test, we need references to intermediate nodes
        // This is simplified - in practice, the disk-backed impl would use arena lookups
        let a_node = root.get_child('a').unwrap();
        nodes.insert(a_ref, a_node);

        let b_node = a_node.get_child('b').unwrap();
        nodes.insert(b_ref, b_node);

        let c_node = b_node.get_child('c').unwrap();

        // Reconstruct term from 'c' node
        let term = c_node.reconstruct_term(|node_ref| nodes.get(&node_ref).copied());
        assert_eq!(term, "abc");
    }
}
