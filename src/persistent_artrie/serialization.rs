//! Node Serialization for Persistent ART
//!
//! This module provides binary serialization and deserialization for ART nodes.
//! The format is designed to be:
//! - **Compact**: Minimize disk space usage
//! - **Fast**: Efficient encoding/decoding with minimal allocations
//! - **Versioned**: Support future format evolution
//! - **Aligned**: Cache-line friendly where possible
//!
//! # Serialization Format
//!
//! All nodes share a common header followed by type-specific data:
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────────┐
//! │ SerializedNodeHeader (16 bytes)                                     │
//! ├───────────┬───────────┬───────────┬───────────┬────────────────────┤
//! │ magic[4]  │ version   │ node_type │ flags     │ reserved[2]        │
//! │ "ART\0"   │ u8        │ u8        │ u8        │ [u8; 2]            │
//! ├───────────┴───────────┴───────────┴───────────┴────────────────────┤
//! │ num_children: u16     │ prefix_len: u8        │ _padding: u8       │
//! ├───────────────────────┴───────────────────────┴────────────────────┤
//! │ data_size: u32 (size of type-specific data)                        │
//! └────────────────────────────────────────────────────────────────────┘
//! │ CompressedPrefix (12 bytes, if prefix_len > 0)                     │
//! └────────────────────────────────────────────────────────────────────┘
//! │ Type-specific data (variable size)                                 │
//! └────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Type-Specific Layouts
//!
//! Serialized child pointers are 64-bit disk/null `SwizzledPtr` state words.
//! In-memory `SwizzledPtr` values keep pointer provenance in a separate runtime
//! slot and cannot be reconstructed from serialized integers.
//!
//! ## Node4
//! ```text
//! │ keys: [u8; 4]         │ 4 bytes                                    │
//! │ children: [u64; 4]    │ 32 bytes (disk/null SwizzledPtr state)     │
//! Total: 36 bytes + header
//! ```
//!
//! ## Node16
//! ```text
//! │ keys: [u8; 16]        │ 16 bytes                                   │
//! │ children: [u64; 16]   │ 128 bytes (disk/null SwizzledPtr state)    │
//! Total: 144 bytes + header
//! ```
//!
//! ## Node48
//! ```text
//! │ index: [u8; 256]      │ 256 bytes                                  │
//! │ children: [u64; 48]   │ 384 bytes (disk/null SwizzledPtr state)    │
//! Total: 640 bytes + header
//! ```
//!
//! ## Node256
//! ```text
//! │ children: [u64; 256]  │ 2048 bytes (only non-null written)         │
//! │ bitmap: [u64; 4]      │ 32 bytes (256 bits for presence)           │
//! Total: variable (32 + 8*num_children) bytes + header
//! ```

use super::error::{PersistentARTrieError, Result};
use super::nodes::node48::NO_CHILD;
use super::nodes::{
    CompressedPrefix, Node, Node16, Node256, Node4, Node48, NodeHeader, MAX_PREFIX_LEN,
};
use super::swizzled_ptr::{NodeType, SwizzledPtr};
use std::io::{Read, Write};

// Relative encoding support (feature-gated)
use super::arena_manager::ArenaSlot;
use super::relative_encoding::{
    encode_children, encode_sequential_siblings, try_decode_children,
    try_decode_sequential_siblings, RelativeEncodingError,
};

/// Helper to convert io::Error to PersistentARTrieError for serialization operations
fn io_err(e: std::io::Error) -> PersistentARTrieError {
    PersistentARTrieError::io_error("serialization", "<buffer>", e)
}

/// Magic bytes identifying an ART node in the serialized format
pub const NODE_MAGIC: [u8; 4] = *b"ART\0";

/// Current serialization format version
pub const FORMAT_VERSION: u8 = 1;

/// Format version 2: Supports relative offset encoding
pub const FORMAT_VERSION_V2: u8 = 2;

/// Serialized header size in bytes
pub const SERIALIZED_HEADER_SIZE: usize = 16;

/// Header flags for encoding modes
pub mod encoding_flags {
    /// Children use relative offset encoding (vs fixed 8-byte pointers)
    pub const RELATIVE_OFFSETS: u8 = 0x80;
    /// Children are stored sequentially (store first_child + count)
    pub const SEQUENTIAL_SIBLINGS: u8 = 0x40;
    /// Node record carries an optional value blob appended after the node-type
    /// data (M4a / D-VAL): a 4-byte little-endian length prefix + that many value
    /// bytes, at offset `SERIALIZED_HEADER_SIZE + data_size`.
    ///
    /// # Back-compat (value-less records stay byte-identical)
    ///
    /// This bit lives in the serialization-only `encoding_flags` byte (offset 7;
    /// dropped after deserialization). Every prior byte node record left it CLEAR,
    /// and when CLEAR nothing is appended — a value-less node serializes to exactly
    /// the bytes it always did, so existing files round-trip byte-identically. When
    /// SET, the appended `value_len: u32` + bytes carry a valued ART leaf's value
    /// (produced only by the overlay-checkpoint capture). Old binaries never read an
    /// Overlay-regime file's node arena (the WAL `MAGIC_OVERLAY` tripwire fails them
    /// closed first), so a SET bit is never presented to a reader predating it.
    pub const HAS_VALUE: u8 = 0x20;
}

/// Node type discriminants for serialization
pub mod node_types {
    pub const NODE4: u8 = 4;
    pub const NODE16: u8 = 16;
    pub const NODE48: u8 = 48;
    pub const NODE256: u8 = 0; // Uses 0 to match in-memory representation
}

/// Serialized node header (fixed 16 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SerializedNodeHeader {
    /// Magic bytes "ART\0"
    pub magic: [u8; 4],
    /// Format version
    pub version: u8,
    /// Node type (4, 16, 48, 0 for Node256)
    pub node_type: u8,
    /// Node flags (is_final, is_dirty, is_leaf)
    pub flags: u8,
    /// Encoding flags (v2+): RELATIVE_OFFSETS, SEQUENTIAL_SIBLINGS
    pub encoding_flags: u8,
    /// Number of children
    pub num_children: u16,
    /// Compressed prefix length
    pub prefix_len: u8,
    /// Padding for alignment
    pub _padding: u8,
    /// Size of the type-specific data following this header
    pub data_size: u32,
}

impl SerializedNodeHeader {
    /// Create a header from a NodeHeader (v1 format, fixed pointers)
    pub fn from_node_header(header: &NodeHeader, data_size: u32) -> Self {
        Self {
            magic: NODE_MAGIC,
            version: FORMAT_VERSION,
            node_type: header.node_type,
            flags: header.flags,
            encoding_flags: 0,
            num_children: header.num_children,
            prefix_len: header.prefix_len,
            _padding: 0,
            data_size,
        }
    }

    /// Create a header from a NodeHeader with encoding flags (v2 format)
    pub fn from_node_header_v2(header: &NodeHeader, data_size: u32, encoding_flags: u8) -> Self {
        Self {
            magic: NODE_MAGIC,
            version: FORMAT_VERSION_V2,
            node_type: header.node_type,
            flags: header.flags,
            encoding_flags,
            num_children: header.num_children,
            prefix_len: header.prefix_len,
            _padding: 0,
            data_size,
        }
    }

    /// Check if this header uses relative offset encoding
    pub fn uses_relative_offsets(&self) -> bool {
        self.version >= FORMAT_VERSION_V2
            && (self.encoding_flags & encoding_flags::RELATIVE_OFFSETS) != 0
    }

    /// Check if this header uses sequential sibling storage
    pub fn uses_sequential_siblings(&self) -> bool {
        self.version >= FORMAT_VERSION_V2
            && (self.encoding_flags & encoding_flags::SEQUENTIAL_SIBLINGS) != 0
    }

    /// Convert to a NodeHeader
    pub fn to_node_header(&self) -> NodeHeader {
        NodeHeader {
            node_type: self.node_type,
            prefix_len: self.prefix_len,
            flags: self.flags,
            _padding: 0,
            num_children: self.num_children,
            _padding2: [0; 2],
            version: 0, // Version is runtime-only
        }
    }

    /// Validate the header
    pub fn validate(&self) -> Result<()> {
        if self.magic != NODE_MAGIC {
            return Err(PersistentARTrieError::InvalidMagic {
                expected: u64::from_le_bytes([
                    NODE_MAGIC[0],
                    NODE_MAGIC[1],
                    NODE_MAGIC[2],
                    NODE_MAGIC[3],
                    0,
                    0,
                    0,
                    0,
                ]),
                found: u64::from_le_bytes([
                    self.magic[0],
                    self.magic[1],
                    self.magic[2],
                    self.magic[3],
                    0,
                    0,
                    0,
                    0,
                ]),
            });
        }
        if self.version > FORMAT_VERSION_V2 {
            return Err(PersistentARTrieError::UnsupportedVersion {
                max_supported: FORMAT_VERSION_V2 as u32,
                found: self.version as u32,
            });
        }
        match self.node_type {
            node_types::NODE4 | node_types::NODE16 | node_types::NODE48 | node_types::NODE256 => {}
            _ => {
                return Err(PersistentARTrieError::corrupted(format!(
                    "invalid node type: {}",
                    self.node_type
                )));
            }
        }
        if self.prefix_len as usize > MAX_PREFIX_LEN {
            return Err(PersistentARTrieError::corrupted(format!(
                "prefix length {} exceeds maximum {}",
                self.prefix_len, MAX_PREFIX_LEN
            )));
        }
        Ok(())
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> [u8; SERIALIZED_HEADER_SIZE] {
        let mut bytes = [0u8; SERIALIZED_HEADER_SIZE];
        bytes[0..4].copy_from_slice(&self.magic);
        bytes[4] = self.version;
        bytes[5] = self.node_type;
        bytes[6] = self.flags;
        bytes[7] = self.encoding_flags;
        bytes[8..10].copy_from_slice(&self.num_children.to_le_bytes());
        bytes[10] = self.prefix_len;
        bytes[11] = self._padding;
        bytes[12..16].copy_from_slice(&self.data_size.to_le_bytes());
        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8; SERIALIZED_HEADER_SIZE]) -> Self {
        Self {
            magic: [bytes[0], bytes[1], bytes[2], bytes[3]],
            version: bytes[4],
            node_type: bytes[5],
            flags: bytes[6],
            encoding_flags: bytes[7],
            num_children: u16::from_le_bytes([bytes[8], bytes[9]]),
            prefix_len: bytes[10],
            _padding: bytes[11],
            data_size: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        }
    }
}

/// Calculate the serialized size of a node
pub fn serialized_size(node: &Node) -> usize {
    SERIALIZED_HEADER_SIZE + prefix_size(node) + node_data_size(node)
}

fn prefix_size(node: &Node) -> usize {
    if node.header().prefix_len > 0 {
        MAX_PREFIX_LEN
    } else {
        0
    }
}

fn node_data_size(node: &Node) -> usize {
    match node {
        Node::N4(_) => 4 + 4 * 8,     // 4 keys + 4 children (8 bytes each)
        Node::N16(_) => 16 + 16 * 8,  // 16 keys + 16 children
        Node::N48(_) => 256 + 48 * 8, // 256 index + 48 children
        Node::N256(n) => {
            // Bitmap (32 bytes) + non-null children (8 bytes each)
            32 + n.header.num_children as usize * 8
        }
    }
}

/// Serialize a Node to a writer
pub fn serialize_node<W: Write>(node: &Node, writer: &mut W) -> Result<usize> {
    let data_size = prefix_size(node) + node_data_size(node);
    let header = SerializedNodeHeader::from_node_header(node.header(), data_size as u32);

    // Write header
    writer.write_all(&header.to_bytes()).map_err(io_err)?;

    // Write prefix if present
    if node.header().prefix_len > 0 {
        writer.write_all(&node.prefix().bytes).map_err(io_err)?;
    }

    // Write type-specific data
    match node {
        Node::N4(n) => serialize_node4(n, writer)?,
        Node::N16(n) => serialize_node16(n, writer)?,
        Node::N48(n) => serialize_node48(n, writer)?,
        Node::N256(n) => serialize_node256(n, writer)?,
    }

    Ok(SERIALIZED_HEADER_SIZE + data_size)
}

fn serialize_node4<W: Write>(node: &Node4, writer: &mut W) -> Result<()> {
    // Write keys
    writer.write_all(&node.keys).map_err(io_err)?;

    // Write children as u64
    for child in &node.children {
        let raw = child.to_raw();
        writer.write_all(&raw.to_le_bytes()).map_err(io_err)?;
    }
    Ok(())
}

fn serialize_node16<W: Write>(node: &Node16, writer: &mut W) -> Result<()> {
    // Write keys
    writer.write_all(&node.keys).map_err(io_err)?;

    // Write children as u64
    for child in &node.children {
        let raw = child.to_raw();
        writer.write_all(&raw.to_le_bytes()).map_err(io_err)?;
    }
    Ok(())
}

fn serialize_node48<W: Write>(node: &Node48, writer: &mut W) -> Result<()> {
    // Write index array
    writer.write_all(&node.index).map_err(io_err)?;

    // Write children as u64
    for child in &node.children {
        let raw = child.to_raw();
        writer.write_all(&raw.to_le_bytes()).map_err(io_err)?;
    }
    Ok(())
}

fn serialize_node256<W: Write>(node: &Node256, writer: &mut W) -> Result<()> {
    // Build bitmap of non-null children
    let mut bitmap = [0u64; 4];
    for (i, child) in node.children.iter().enumerate() {
        if !child.is_null() {
            bitmap[i / 64] |= 1u64 << (i % 64);
        }
    }

    // Write bitmap
    for word in &bitmap {
        writer.write_all(&word.to_le_bytes()).map_err(io_err)?;
    }

    // Write only non-null children
    for child in &node.children {
        if !child.is_null() {
            let raw = child.to_raw();
            writer.write_all(&raw.to_le_bytes()).map_err(io_err)?;
        }
    }
    Ok(())
}

/// Deserialize a Node from a reader
pub fn deserialize_node<R: Read>(reader: &mut R) -> Result<Node> {
    // Read and validate header
    let mut header_bytes = [0u8; SERIALIZED_HEADER_SIZE];
    reader.read_exact(&mut header_bytes).map_err(io_err)?;
    let header = SerializedNodeHeader::from_bytes(&header_bytes);
    header.validate()?;

    // Read prefix if present
    let prefix = if header.prefix_len > 0 {
        let mut prefix_bytes = [0u8; MAX_PREFIX_LEN];
        reader.read_exact(&mut prefix_bytes).map_err(io_err)?;
        CompressedPrefix {
            bytes: prefix_bytes,
        }
    } else {
        CompressedPrefix::empty()
    };

    // Deserialize type-specific data
    match header.node_type {
        node_types::NODE4 => deserialize_node4(reader, &header, prefix),
        node_types::NODE16 => deserialize_node16(reader, &header, prefix),
        node_types::NODE48 => deserialize_node48(reader, &header, prefix),
        node_types::NODE256 => deserialize_node256(reader, &header, prefix),
        _ => Err(PersistentARTrieError::corrupted(format!(
            "invalid node type: {}",
            header.node_type
        ))),
    }
}

fn deserialize_node4<R: Read>(
    reader: &mut R,
    header: &SerializedNodeHeader,
    prefix: CompressedPrefix,
) -> Result<Node> {
    let mut node = Node4::new();
    node.header = header.to_node_header();
    node.prefix = prefix;

    // Read keys
    reader.read_exact(&mut node.keys).map_err(io_err)?;

    // Read children
    for child in &mut node.children {
        let mut raw_bytes = [0u8; 8];
        reader.read_exact(&mut raw_bytes).map_err(io_err)?;
        *child = SwizzledPtr::from_raw(u64::from_le_bytes(raw_bytes));
    }

    Ok(Node::N4(Box::new(node)))
}

fn deserialize_node16<R: Read>(
    reader: &mut R,
    header: &SerializedNodeHeader,
    prefix: CompressedPrefix,
) -> Result<Node> {
    let mut node = Node16::new();
    node.header = header.to_node_header();
    node.prefix = prefix;

    // Read keys
    reader.read_exact(&mut node.keys).map_err(io_err)?;

    // Read children
    for child in &mut node.children {
        let mut raw_bytes = [0u8; 8];
        reader.read_exact(&mut raw_bytes).map_err(io_err)?;
        *child = SwizzledPtr::from_raw(u64::from_le_bytes(raw_bytes));
    }

    Ok(Node::N16(Box::new(node)))
}

fn deserialize_node48<R: Read>(
    reader: &mut R,
    header: &SerializedNodeHeader,
    prefix: CompressedPrefix,
) -> Result<Node> {
    let mut node = Node48::new();
    node.header = header.to_node_header();
    node.prefix = prefix;

    // Read index array
    reader.read_exact(&mut node.index).map_err(io_err)?;

    // Read children
    for child in &mut node.children {
        let mut raw_bytes = [0u8; 8];
        reader.read_exact(&mut raw_bytes).map_err(io_err)?;
        *child = SwizzledPtr::from_raw(u64::from_le_bytes(raw_bytes));
    }

    Ok(Node::N48(Box::new(node)))
}

fn deserialize_node256<R: Read>(
    reader: &mut R,
    header: &SerializedNodeHeader,
    prefix: CompressedPrefix,
) -> Result<Node> {
    let mut node = Node256::new();
    node.header = header.to_node_header();
    node.prefix = prefix;

    // Read bitmap
    let mut bitmap = [0u64; 4];
    for word in &mut bitmap {
        let mut word_bytes = [0u8; 8];
        reader.read_exact(&mut word_bytes).map_err(io_err)?;
        *word = u64::from_le_bytes(word_bytes);
    }

    // Read non-null children
    for i in 0..256 {
        if bitmap[i / 64] & (1u64 << (i % 64)) != 0 {
            let mut raw_bytes = [0u8; 8];
            reader.read_exact(&mut raw_bytes).map_err(io_err)?;
            node.children[i] = SwizzledPtr::from_raw(u64::from_le_bytes(raw_bytes));
        }
    }

    Ok(Node::N256(Box::new(node)))
}

/// Serialize a Node to a byte vector
pub fn to_bytes(node: &Node) -> Result<Vec<u8>> {
    let mut buffer = Vec::with_capacity(serialized_size(node));
    serialize_node(node, &mut buffer)?;
    Ok(buffer)
}

/// Deserialize a Node from a byte slice
pub fn from_bytes(bytes: &[u8]) -> Result<Node> {
    let mut reader = std::io::Cursor::new(bytes);
    deserialize_node(&mut reader)
}

// =============================================================================
// V2 Serialization with Relative Offset Encoding
// =============================================================================

pub mod v2 {
    use super::*;

    /// Context for relative encoding during serialization
    #[derive(Debug, Clone)]
    pub struct SerializationContext {
        /// Parent's arena slot (used for relative offset calculation)
        pub parent_slot: ArenaSlot,
        /// Whether to use relative offsets (vs fixed 8-byte pointers)
        pub use_relative: bool,
        /// Whether children are stored sequentially
        pub use_sequential: bool,
        /// First child slot (for sequential mode)
        pub first_child_slot: Option<ArenaSlot>,
    }

    impl SerializationContext {
        /// Create a context for relative encoding
        pub fn new(parent_slot: ArenaSlot) -> Self {
            Self {
                parent_slot,
                use_relative: true,
                use_sequential: false,
                first_child_slot: None,
            }
        }

        /// Create a context for sequential sibling storage
        pub fn sequential(parent_slot: ArenaSlot, first_child_slot: ArenaSlot) -> Self {
            Self {
                parent_slot,
                use_relative: true,
                use_sequential: true,
                first_child_slot: Some(first_child_slot),
            }
        }

        /// Get the encoding flags for the header
        pub fn encoding_flags(&self) -> u8 {
            let mut flags = 0u8;
            if self.use_relative {
                flags |= encoding_flags::RELATIVE_OFFSETS;
            }
            if self.use_sequential {
                flags |= encoding_flags::SEQUENTIAL_SIBLINGS;
            }
            flags
        }
    }

    /// Context for deserialization
    #[derive(Debug, Clone)]
    pub struct DeserializationContext {
        /// Parent's arena slot (used to reconstruct absolute slots from relative offsets)
        pub parent_slot: ArenaSlot,
    }

    impl DeserializationContext {
        pub fn new(parent_slot: ArenaSlot) -> Self {
            Self { parent_slot }
        }
    }

    fn relative_decode_err(err: RelativeEncodingError) -> PersistentARTrieError {
        PersistentARTrieError::corrupted(format!("invalid relative child encoding: {}", err))
    }

    fn decode_v2_child_slots(
        data: &[u8],
        parent: ArenaSlot,
        count: usize,
        uses_sequential: bool,
    ) -> Result<(Vec<ArenaSlot>, usize)> {
        if uses_sequential {
            try_decode_sequential_siblings(data, parent, count).map_err(relative_decode_err)
        } else {
            try_decode_children(data, parent, count).map_err(relative_decode_err)
        }
    }

    fn read_v2_node_type(data: &[u8], offset: usize) -> Result<NodeType> {
        let byte = *data.get(offset).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "missing relative child node type at offset {} in {} byte node payload",
                offset,
                data.len()
            ))
        })?;
        Ok(NodeType::try_from(byte).unwrap_or(NodeType::Node4))
    }

    /// Collect child slots from a node for relative encoding
    ///
    /// Returns only valid child slots (filters out null and in-memory pointers).
    pub fn collect_child_slots(node: &Node) -> Vec<ArenaSlot> {
        let mut slots = Vec::new();
        match node {
            Node::N4(n) => {
                for i in 0..n.header.num_children as usize {
                    if let Some(slot) = n.children[i].as_arena_slot() {
                        slots.push(slot);
                    }
                }
            }
            Node::N16(n) => {
                for i in 0..n.header.num_children as usize {
                    if let Some(slot) = n.children[i].as_arena_slot() {
                        slots.push(slot);
                    }
                }
            }
            Node::N48(n) => {
                for i in 0..48 {
                    if let Some(slot) = n.children[i].as_arena_slot() {
                        slots.push(slot);
                    }
                }
            }
            Node::N256(n) => {
                for child in &n.children {
                    if let Some(slot) = child.as_arena_slot() {
                        slots.push(slot);
                    }
                }
            }
        }
        slots
    }

    /// Collect child slots and node types from a node for relative encoding with type preservation.
    ///
    /// Returns (ArenaSlot, NodeType) pairs for valid child pointers.
    pub fn collect_child_slots_and_types(node: &Node) -> Vec<(ArenaSlot, NodeType)> {
        let mut result = Vec::new();
        match node {
            Node::N4(n) => {
                for i in 0..n.header.num_children as usize {
                    if let (Some(slot), Some(node_type)) = (
                        n.children[i].as_arena_slot(),
                        n.children[i].disk_location().map(|loc| loc.node_type),
                    ) {
                        result.push((slot, node_type));
                    }
                }
            }
            Node::N16(n) => {
                for i in 0..n.header.num_children as usize {
                    if let (Some(slot), Some(node_type)) = (
                        n.children[i].as_arena_slot(),
                        n.children[i].disk_location().map(|loc| loc.node_type),
                    ) {
                        result.push((slot, node_type));
                    }
                }
            }
            Node::N48(n) => {
                for i in 0..48 {
                    if let (Some(slot), Some(node_type)) = (
                        n.children[i].as_arena_slot(),
                        n.children[i].disk_location().map(|loc| loc.node_type),
                    ) {
                        result.push((slot, node_type));
                    }
                }
            }
            Node::N256(n) => {
                for child in &n.children {
                    if let (Some(slot), Some(node_type)) = (
                        child.as_arena_slot(),
                        child.disk_location().map(|loc| loc.node_type),
                    ) {
                        result.push((slot, node_type));
                    }
                }
            }
        }
        result
    }

    /// Estimate the serialized size with relative encoding
    pub fn estimate_serialized_size_v2(node: &Node, ctx: &SerializationContext) -> usize {
        let header_size = SERIALIZED_HEADER_SIZE;
        let prefix_size = if node.header().prefix_len > 0 {
            MAX_PREFIX_LEN
        } else {
            0
        };

        let num_children = node.header().num_children as usize;

        let (children_size, node_types_size) = if ctx.use_sequential {
            // Sequential: just first_child reference + count is in header
            let encoded_size = if let Some(first_child) = ctx.first_child_slot {
                super::super::relative_encoding::encoded_size(ctx.parent_slot, first_child)
            } else {
                0
            };
            // Add 1 byte per child for node type
            (encoded_size, num_children)
        } else if ctx.use_relative {
            // Relative: sum of encoded sizes for each child
            let child_slots = collect_child_slots(node);
            let encoded_size: usize = child_slots
                .iter()
                .map(|&child| super::super::relative_encoding::encoded_size(ctx.parent_slot, child))
                .sum();
            // Add 1 byte per child for node type
            (encoded_size, num_children)
        } else {
            // Fixed: 8 bytes per child (no separate node types needed - they're in the SwizzledPtr)
            (num_children * 8, 0)
        };

        let keys_size = match node {
            Node::N4(_) => 4,
            Node::N16(_) => 16,
            Node::N48(_) => 256, // index array
            Node::N256(_) => 32, // bitmap only
        };

        header_size + prefix_size + keys_size + children_size + node_types_size
    }

    /// Serialize a node with relative encoding to a byte vector
    pub fn serialize_node_v2(node: &Node, ctx: &SerializationContext) -> Result<Vec<u8>> {
        let estimated_size = estimate_serialized_size_v2(node, ctx);
        let mut buffer = Vec::with_capacity(estimated_size);

        // Collect child slots and their node types (needed for type preservation)
        let child_slots_and_types = collect_child_slots_and_types(node);
        let child_slots: Vec<ArenaSlot> = child_slots_and_types.iter().map(|(s, _)| *s).collect();

        // Encode children with relative offsets
        let mut children_buf = Vec::new();
        if ctx.use_sequential {
            if let Some(first_child) = ctx.first_child_slot {
                encode_sequential_siblings(ctx.parent_slot, first_child, &mut children_buf);
            }
        } else {
            encode_children(ctx.parent_slot, &child_slots, &mut children_buf);
        }

        // Calculate data size (keys + encoded children + node types)
        let prefix_size = if node.header().prefix_len > 0 {
            MAX_PREFIX_LEN
        } else {
            0
        };
        let keys_size = match node {
            Node::N4(_) => 4,
            Node::N16(_) => 16,
            Node::N48(_) => 256,
            Node::N256(_) => 32,
        };
        // Add 1 byte per child for node type when using relative/sequential encoding
        let node_types_size = if ctx.use_sequential || !child_slots.is_empty() {
            child_slots_and_types.len()
        } else {
            0
        };
        let data_size = prefix_size + keys_size + children_buf.len() + node_types_size;

        // Build header
        let header = SerializedNodeHeader::from_node_header_v2(
            node.header(),
            data_size as u32,
            ctx.encoding_flags(),
        );

        // Write header
        buffer.extend_from_slice(&header.to_bytes());

        // Write prefix if present
        if node.header().prefix_len > 0 {
            buffer.extend_from_slice(&node.prefix().bytes);
        }

        // Write keys and encoded children
        match node {
            Node::N4(n) => {
                buffer.extend_from_slice(&n.keys);
            }
            Node::N16(n) => {
                buffer.extend_from_slice(&n.keys);
            }
            Node::N48(n) => {
                buffer.extend_from_slice(&n.index);
            }
            Node::N256(n) => {
                // Write bitmap
                let mut bitmap = [0u64; 4];
                for (i, child) in n.children.iter().enumerate() {
                    if !child.is_null() {
                        bitmap[i / 64] |= 1u64 << (i % 64);
                    }
                }
                for word in &bitmap {
                    buffer.extend_from_slice(&word.to_le_bytes());
                }
            }
        }

        // Write encoded children
        buffer.extend_from_slice(&children_buf);

        // Write node types for each child (1 byte each) - required for relative/sequential encoding
        // This allows us to reconstruct the correct SwizzledPtr with proper node type during deserialization
        for (_, node_type) in &child_slots_and_types {
            buffer.push(*node_type as u8);
        }

        Ok(buffer)
    }

    /// Append an optional value blob to a node record produced by
    /// [`serialize_node_v2`] (M4a / D-VAL). When `value_bytes` is `None` the buffer
    /// is returned UNCHANGED (the `HAS_VALUE` bit stays clear → value-less records
    /// are byte-identical to before). When `Some`, set `HAS_VALUE` in the
    /// `encoding_flags` byte (offset 7) and append `value_len: u32` (LE) + the bytes.
    /// The value sits AFTER the node-type data, at offset
    /// `SERIALIZED_HEADER_SIZE + data_size`, so it never perturbs the node parse.
    pub fn append_node_value(mut node_bytes: Vec<u8>, value_bytes: Option<&[u8]>) -> Vec<u8> {
        if let Some(vb) = value_bytes {
            // encoding_flags lives at byte 7 (see SerializedNodeHeader::to_bytes).
            node_bytes[7] |= encoding_flags::HAS_VALUE;
            node_bytes.extend_from_slice(&(vb.len() as u32).to_le_bytes());
            node_bytes.extend_from_slice(vb);
        }
        node_bytes
    }

    /// Read the optional value blob from a node record (the inverse of
    /// [`append_node_value`]). Returns `None` if the `HAS_VALUE` bit is clear (every
    /// pre-M4a record) or the trailing bytes are absent/truncated. The value starts
    /// at `SERIALIZED_HEADER_SIZE + data_size` (`data_size` is the node-data size from
    /// the header at bytes 12..16; `encoding_flags` is byte 7).
    pub fn read_node_value(data: &[u8]) -> Option<Vec<u8>> {
        if data.len() < SERIALIZED_HEADER_SIZE {
            return None;
        }
        if data[7] & encoding_flags::HAS_VALUE == 0 {
            return None;
        }
        let data_size =
            u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;
        let off = SERIALIZED_HEADER_SIZE + data_size;
        if data.len() < off + 4 {
            return None;
        }
        let len = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
            as usize;
        if data.len() < off + 4 + len {
            return None;
        }
        Some(data[off + 4..off + 4 + len].to_vec())
    }

    /// Deserialize a node with v2 encoding (handles both relative and fixed)
    pub fn deserialize_node_v2(data: &[u8], ctx: &DeserializationContext) -> Result<Node> {
        let mut reader = std::io::Cursor::new(data);

        // Read header
        let mut header_bytes = [0u8; SERIALIZED_HEADER_SIZE];
        reader.read_exact(&mut header_bytes).map_err(io_err)?;
        let header = SerializedNodeHeader::from_bytes(&header_bytes);
        header.validate()?;

        // Read prefix if present
        let prefix = if header.prefix_len > 0 {
            let mut prefix_bytes = [0u8; MAX_PREFIX_LEN];
            reader.read_exact(&mut prefix_bytes).map_err(io_err)?;
            CompressedPrefix {
                bytes: prefix_bytes,
            }
        } else {
            CompressedPrefix::empty()
        };

        let remaining = &data[reader.position() as usize..];

        // Decode based on node type and encoding flags
        match header.node_type {
            node_types::NODE4 => deserialize_node4_v2(&header, prefix, remaining, ctx),
            node_types::NODE16 => deserialize_node16_v2(&header, prefix, remaining, ctx),
            node_types::NODE48 => deserialize_node48_v2(&header, prefix, remaining, ctx),
            node_types::NODE256 => deserialize_node256_v2(&header, prefix, remaining, ctx),
            _ => Err(PersistentARTrieError::corrupted(format!(
                "invalid node type: {}",
                header.node_type
            ))),
        }
    }

    fn deserialize_node4_v2(
        header: &SerializedNodeHeader,
        prefix: CompressedPrefix,
        data: &[u8],
        ctx: &DeserializationContext,
    ) -> Result<Node> {
        let mut node = Node4::new();
        node.header = header.to_node_header();
        node.prefix = prefix;

        // Read keys
        node.keys.copy_from_slice(&data[..4]);

        let num_children = header.num_children as usize;

        // Decode children based on encoding mode
        if header.uses_sequential_siblings() {
            let (children, bytes_consumed) =
                decode_v2_child_slots(&data[4..], ctx.parent_slot, num_children, true)?;
            // Read node types after encoded children
            let types_start = 4 + bytes_consumed;
            for (i, slot) in children.into_iter().enumerate() {
                let node_type = read_v2_node_type(data, types_start + i)?;
                node.children[i] = SwizzledPtr::from_arena_slot(slot, node_type);
            }
        } else if header.uses_relative_offsets() {
            let (children, bytes_consumed) =
                decode_v2_child_slots(&data[4..], ctx.parent_slot, num_children, false)?;
            // Read node types after encoded children
            let types_start = 4 + bytes_consumed;
            for (i, slot) in children.into_iter().enumerate() {
                let node_type = read_v2_node_type(data, types_start + i)?;
                node.children[i] = SwizzledPtr::from_arena_slot(slot, node_type);
            }
        } else {
            // Fixed 8-byte pointers (node type is in the pointer itself)
            for i in 0..num_children {
                let offset = 4 + i * 8;
                let raw = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                node.children[i] = SwizzledPtr::from_raw(raw);
            }
        }

        Ok(Node::N4(Box::new(node)))
    }

    fn deserialize_node16_v2(
        header: &SerializedNodeHeader,
        prefix: CompressedPrefix,
        data: &[u8],
        ctx: &DeserializationContext,
    ) -> Result<Node> {
        let mut node = Node16::new();
        node.header = header.to_node_header();
        node.prefix = prefix;

        // Read keys
        node.keys.copy_from_slice(&data[..16]);

        let num_children = header.num_children as usize;

        // Decode children based on encoding mode
        if header.uses_sequential_siblings() {
            let (children, bytes_consumed) =
                decode_v2_child_slots(&data[16..], ctx.parent_slot, num_children, true)?;
            // Read node types after encoded children
            let types_start = 16 + bytes_consumed;
            for (i, slot) in children.into_iter().enumerate() {
                let node_type = read_v2_node_type(data, types_start + i)?;
                node.children[i] = SwizzledPtr::from_arena_slot(slot, node_type);
            }
        } else if header.uses_relative_offsets() {
            let (children, bytes_consumed) =
                decode_v2_child_slots(&data[16..], ctx.parent_slot, num_children, false)?;
            // Read node types after encoded children
            let types_start = 16 + bytes_consumed;
            for (i, slot) in children.into_iter().enumerate() {
                let node_type = read_v2_node_type(data, types_start + i)?;
                node.children[i] = SwizzledPtr::from_arena_slot(slot, node_type);
            }
        } else {
            // Fixed 8-byte pointers (node type is in the pointer itself)
            for i in 0..num_children {
                let offset = 16 + i * 8;
                let raw = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                node.children[i] = SwizzledPtr::from_raw(raw);
            }
        }

        Ok(Node::N16(Box::new(node)))
    }

    fn deserialize_node48_v2(
        header: &SerializedNodeHeader,
        prefix: CompressedPrefix,
        data: &[u8],
        ctx: &DeserializationContext,
    ) -> Result<Node> {
        let mut node = Node48::new();
        node.header = header.to_node_header();
        node.prefix = prefix;

        // Read index array
        node.index.copy_from_slice(&data[..256]);

        let num_children = header.num_children as usize;

        // Build a sorted list of used slots from the index array.
        // During serialization, children are collected in slot order (0..48),
        // so we must place them back at their original slot positions.
        let mut used_slots: Vec<u8> = Vec::with_capacity(num_children);
        for key in 0..256usize {
            let slot = node.index[key];
            if slot != NO_CHILD && !used_slots.contains(&slot) {
                used_slots.push(slot);
            }
        }
        used_slots.sort_unstable();

        // Decode children based on encoding mode
        if header.uses_sequential_siblings() {
            let (children, bytes_consumed) =
                decode_v2_child_slots(&data[256..], ctx.parent_slot, num_children, true)?;
            // Read node types after encoded children
            let types_start = 256 + bytes_consumed;
            for (i, child_slot) in children.into_iter().enumerate() {
                if i >= used_slots.len() {
                    return Err(PersistentARTrieError::corrupted(format!(
                        "node48 relative child count {} exceeds index entries {}",
                        num_children,
                        used_slots.len()
                    )));
                }
                let actual_slot = used_slots[i] as usize;
                let node_type = read_v2_node_type(data, types_start + i)?;
                node.children[actual_slot] = SwizzledPtr::from_arena_slot(child_slot, node_type);
            }
        } else if header.uses_relative_offsets() {
            let (children, bytes_consumed) =
                decode_v2_child_slots(&data[256..], ctx.parent_slot, num_children, false)?;
            // Read node types after encoded children
            let types_start = 256 + bytes_consumed;
            for (i, child_slot) in children.into_iter().enumerate() {
                if i >= used_slots.len() {
                    return Err(PersistentARTrieError::corrupted(format!(
                        "node48 relative child count {} exceeds index entries {}",
                        num_children,
                        used_slots.len()
                    )));
                }
                let actual_slot = used_slots[i] as usize;
                let node_type = read_v2_node_type(data, types_start + i)?;
                node.children[actual_slot] = SwizzledPtr::from_arena_slot(child_slot, node_type);
            }
        } else {
            // Fixed 8-byte pointers (node type is in the pointer itself)
            for i in 0..num_children {
                let actual_slot = used_slots[i] as usize;
                let offset = 256 + i * 8;
                let raw = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                node.children[actual_slot] = SwizzledPtr::from_raw(raw);
            }
        }

        Ok(Node::N48(Box::new(node)))
    }

    fn deserialize_node256_v2(
        header: &SerializedNodeHeader,
        prefix: CompressedPrefix,
        data: &[u8],
        ctx: &DeserializationContext,
    ) -> Result<Node> {
        let mut node = Node256::new();
        node.header = header.to_node_header();
        node.prefix = prefix;

        // Read bitmap
        let mut bitmap = [0u64; 4];
        for (i, word) in bitmap.iter_mut().enumerate() {
            let offset = i * 8;
            *word = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        }

        let num_children = header.num_children as usize;
        let children_start = 32; // After bitmap

        // Decode children based on encoding mode
        if header.uses_sequential_siblings() {
            let (children, bytes_consumed) = decode_v2_child_slots(
                &data[children_start..],
                ctx.parent_slot,
                num_children,
                true,
            )?;
            // Read node types after encoded children
            let types_start = children_start + bytes_consumed;
            let mut child_idx = 0;
            for i in 0..256 {
                if bitmap[i / 64] & (1u64 << (i % 64)) != 0 {
                    if child_idx >= children.len() {
                        return Err(PersistentARTrieError::corrupted(format!(
                            "node256 bitmap references more children than header count {}",
                            num_children
                        )));
                    }
                    let node_type = read_v2_node_type(data, types_start + child_idx)?;
                    node.children[i] = SwizzledPtr::from_arena_slot(children[child_idx], node_type);
                    child_idx += 1;
                }
            }
        } else if header.uses_relative_offsets() {
            let (children, bytes_consumed) = decode_v2_child_slots(
                &data[children_start..],
                ctx.parent_slot,
                num_children,
                false,
            )?;
            // Read node types after encoded children
            let types_start = children_start + bytes_consumed;
            let mut child_idx = 0;
            for i in 0..256 {
                if bitmap[i / 64] & (1u64 << (i % 64)) != 0 {
                    if child_idx >= children.len() {
                        return Err(PersistentARTrieError::corrupted(format!(
                            "node256 bitmap references more children than header count {}",
                            num_children
                        )));
                    }
                    let node_type = read_v2_node_type(data, types_start + child_idx)?;
                    node.children[i] = SwizzledPtr::from_arena_slot(children[child_idx], node_type);
                    child_idx += 1;
                }
            }
        } else {
            // Fixed 8-byte pointers (node type is in the pointer itself)
            let mut child_idx = 0;
            for i in 0..256 {
                if bitmap[i / 64] & (1u64 << (i % 64)) != 0 {
                    let offset = children_start + child_idx * 8;
                    let raw = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                    node.children[i] = SwizzledPtr::from_raw(raw);
                    child_idx += 1;
                }
            }
        }

        Ok(Node::N256(Box::new(node)))
    }
}

// Re-export v2 types for convenience
pub use v2::{
    collect_child_slots, deserialize_node_v2, estimate_serialized_size_v2, serialize_node_v2,
    DeserializationContext, SerializationContext,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_artrie::nodes::{flags, ArtNode};
    use crate::persistent_artrie::NodeType;

    #[test]
    fn test_header_roundtrip() {
        let header = SerializedNodeHeader {
            magic: NODE_MAGIC,
            version: FORMAT_VERSION,
            node_type: node_types::NODE4,
            flags: flags::IS_FINAL,
            encoding_flags: 0,
            num_children: 3,
            prefix_len: 5,
            _padding: 0,
            data_size: 100,
        };

        let bytes = header.to_bytes();
        let restored = SerializedNodeHeader::from_bytes(&bytes);

        assert_eq!(restored.magic, NODE_MAGIC);
        assert_eq!(restored.version, FORMAT_VERSION);
        assert_eq!(restored.node_type, node_types::NODE4);
        assert_eq!(restored.flags, flags::IS_FINAL);
        assert_eq!(restored.num_children, 3);
        assert_eq!(restored.prefix_len, 5);
        assert_eq!(restored.data_size, 100);
    }

    #[test]
    fn test_header_validation() {
        let mut header = SerializedNodeHeader {
            magic: NODE_MAGIC,
            version: FORMAT_VERSION,
            node_type: node_types::NODE4,
            flags: 0,
            encoding_flags: 0,
            num_children: 0,
            prefix_len: 0,
            _padding: 0,
            data_size: 0,
        };

        // Valid header
        assert!(header.validate().is_ok());

        // Invalid magic
        header.magic = *b"BAD\0";
        assert!(matches!(
            header.validate(),
            Err(PersistentARTrieError::InvalidMagic { .. })
        ));
        header.magic = NODE_MAGIC;

        // Future version
        header.version = 255;
        assert!(matches!(
            header.validate(),
            Err(PersistentARTrieError::UnsupportedVersion { .. })
        ));
        header.version = FORMAT_VERSION;

        // Invalid node type
        header.node_type = 99;
        assert!(matches!(
            header.validate(),
            Err(PersistentARTrieError::CorruptedFile { .. })
        ));
        header.node_type = node_types::NODE4;

        // Invalid prefix length
        header.prefix_len = 20;
        assert!(matches!(
            header.validate(),
            Err(PersistentARTrieError::CorruptedFile { .. })
        ));
    }

    #[test]
    fn test_node4_roundtrip() {
        let mut node4 = Node4::new();
        node4.prefix = CompressedPrefix::from_bytes(b"test");
        node4.header.prefix_len = 4;
        node4.header.set_final(true);

        // Add some children
        node4
            .add_child(b'a', SwizzledPtr::on_disk(100, 0, NodeType::Node4))
            .expect("add child a");
        node4
            .add_child(b'b', SwizzledPtr::on_disk(200, 0, NodeType::Node16))
            .expect("add child b");

        let node = Node::N4(Box::new(node4));
        let bytes = to_bytes(&node).expect("serialize");
        let restored = from_bytes(&bytes).expect("deserialize");

        assert!(matches!(restored, Node::N4(_)));
        assert_eq!(restored.header().prefix_len, 4);
        assert!(restored.header().is_final());
        assert_eq!(restored.header().num_children, 2);
        assert!(restored.find_child(b'a').is_some());
        assert!(restored.find_child(b'b').is_some());
        assert!(restored.find_child(b'c').is_none());
    }

    #[test]
    fn test_node16_roundtrip() {
        let mut node16 = Node16::new();
        node16.prefix = CompressedPrefix::from_bytes(b"prefix");
        node16.header.prefix_len = 6;

        // Add some children
        for i in 0..8 {
            node16
                .add_child(b'a' + i, SwizzledPtr::on_disk(i as u32, 0, NodeType::Node4))
                .expect("add child");
        }

        let node = Node::N16(Box::new(node16));
        let bytes = to_bytes(&node).expect("serialize");
        let restored = from_bytes(&bytes).expect("deserialize");

        assert!(matches!(restored, Node::N16(_)));
        assert_eq!(restored.header().prefix_len, 6);
        assert_eq!(restored.header().num_children, 8);

        for i in 0..8 {
            assert!(restored.find_child(b'a' + i).is_some());
        }
    }

    #[test]
    fn test_node48_roundtrip() {
        let mut node48 = Node48::new();

        // Add children at sparse positions
        for key in [0, 50, 100, 150, 200, 255u8] {
            node48
                .add_child(key, SwizzledPtr::on_disk(key as u32, 0, NodeType::Node4))
                .expect("add child");
        }

        let node = Node::N48(Box::new(node48));
        let bytes = to_bytes(&node).expect("serialize");
        let restored = from_bytes(&bytes).expect("deserialize");

        assert!(matches!(restored, Node::N48(_)));
        assert_eq!(restored.header().num_children, 6);

        for key in [0, 50, 100, 150, 200, 255u8] {
            assert!(
                restored.find_child(key).is_some(),
                "should find key {}",
                key
            );
        }
    }

    #[test]
    fn test_node256_roundtrip() {
        let mut node256 = Node256::new();

        // Add children at various positions
        for key in [0, 64, 128, 192, 255u8] {
            node256
                .add_child(key, SwizzledPtr::on_disk(key as u32, 0, NodeType::Node4))
                .expect("add child");
        }

        let node = Node::N256(Box::new(node256));
        let bytes = to_bytes(&node).expect("serialize");
        let restored = from_bytes(&bytes).expect("deserialize");

        assert!(matches!(restored, Node::N256(_)));
        assert_eq!(restored.header().num_children, 5);

        for key in [0, 64, 128, 192, 255u8] {
            assert!(
                restored.find_child(key).is_some(),
                "should find key {}",
                key
            );
        }
        assert!(restored.find_child(1).is_none());
    }

    #[test]
    fn test_node256_sparse_bitmap() {
        let mut node256 = Node256::new();

        // Add only two children at extreme positions
        node256
            .add_child(0, SwizzledPtr::on_disk(1, 0, NodeType::Node4))
            .expect("add child 0");
        node256
            .add_child(255, SwizzledPtr::on_disk(2, 0, NodeType::Node4))
            .expect("add child 255");

        let node = Node::N256(Box::new(node256));
        let bytes = to_bytes(&node).expect("serialize");

        // Check that only 2 children are serialized (bitmap + 2 * 8 bytes)
        // Header: 16, Prefix: 0, Bitmap: 32, Children: 16
        // Total: 64 bytes
        assert_eq!(bytes.len(), 16 + 32 + 16);

        let restored = from_bytes(&bytes).expect("deserialize");
        assert_eq!(restored.header().num_children, 2);
        assert!(restored.find_child(0).is_some());
        assert!(restored.find_child(255).is_some());
        assert!(restored.find_child(128).is_none());
    }

    #[test]
    fn test_serialized_size_calculation() {
        // Node4 without prefix
        let node4 = Node::N4(Box::new(Node4::new()));
        assert_eq!(serialized_size(&node4), 16 + 0 + (4 + 32)); // header + prefix + data

        // Node4 with prefix
        let mut node4_with_prefix = Node4::new();
        node4_with_prefix.prefix = CompressedPrefix::from_bytes(b"test");
        node4_with_prefix.header.prefix_len = 4;
        let node4_p = Node::N4(Box::new(node4_with_prefix));
        assert_eq!(serialized_size(&node4_p), 16 + 12 + (4 + 32)); // header + MAX_PREFIX_LEN + data

        // Node16
        let node16 = Node::N16(Box::new(Node16::new()));
        assert_eq!(serialized_size(&node16), 16 + 0 + (16 + 128));

        // Node48
        let node48 = Node::N48(Box::new(Node48::new()));
        assert_eq!(serialized_size(&node48), 16 + 0 + (256 + 384));

        // Node256 with 5 children
        let mut node256 = Node256::new();
        for i in 0..5 {
            node256
                .add_child(i, SwizzledPtr::on_disk(i as u32, 0, NodeType::Node4))
                .expect("add");
        }
        let node256_node = Node::N256(Box::new(node256));
        assert_eq!(serialized_size(&node256_node), 16 + 0 + (32 + 5 * 8)); // bitmap + 5 children
    }

    #[test]
    fn test_empty_node_roundtrip() {
        // Test that empty nodes serialize and deserialize correctly
        for create_node in [
            || Node::N4(Box::new(Node4::new())),
            || Node::N16(Box::new(Node16::new())),
            || Node::N48(Box::new(Node48::new())),
            || Node::N256(Box::new(Node256::new())),
        ] {
            let node = create_node();
            let bytes = to_bytes(&node).expect("serialize");
            let restored = from_bytes(&bytes).expect("deserialize");
            assert_eq!(restored.header().num_children, 0);
        }
    }

    // =========================================================================
    // Serialization Error Path Tests
    //
    // These tests verify that deserialization handles truncated and invalid
    // data correctly, returning appropriate errors.
    // =========================================================================

    #[test]
    fn test_deserialize_truncated_header() {
        // Data too short for header (header is 16 bytes)
        let truncated = vec![0u8; 10];
        let result = from_bytes(&truncated);
        assert!(result.is_err());
    }

    #[test]
    fn test_deserialize_invalid_magic() {
        // Valid length but invalid magic bytes
        let mut data = vec![0u8; 32];
        // Set invalid magic (first 4 bytes)
        data[0..4].copy_from_slice(b"BAD!");

        let result = from_bytes(&data);
        assert!(matches!(
            result,
            Err(PersistentARTrieError::InvalidMagic { .. })
        ));
    }

    #[test]
    fn test_deserialize_unsupported_version() {
        // Create valid header with future version
        let mut header = SerializedNodeHeader {
            magic: NODE_MAGIC,
            version: 255, // Future version
            node_type: node_types::NODE4,
            flags: 0,
            encoding_flags: 0,
            num_children: 0,
            prefix_len: 0,
            _padding: 0,
            data_size: 0,
        };
        let bytes = header.to_bytes();
        let result = from_bytes(&bytes);
        assert!(matches!(
            result,
            Err(PersistentARTrieError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn test_deserialize_invalid_node_type() {
        // Valid header but invalid node type
        let mut header = SerializedNodeHeader {
            magic: NODE_MAGIC,
            version: FORMAT_VERSION,
            node_type: 99, // Invalid type
            flags: 0,
            encoding_flags: 0,
            num_children: 0,
            prefix_len: 0,
            _padding: 0,
            data_size: 0,
        };
        let bytes = header.to_bytes();
        let result = from_bytes(&bytes);
        assert!(matches!(
            result,
            Err(PersistentARTrieError::CorruptedFile { .. })
        ));
    }

    #[test]
    fn test_deserialize_truncated_prefix() {
        // Header claims prefix_len=8 but data is truncated
        let mut header = SerializedNodeHeader {
            magic: NODE_MAGIC,
            version: FORMAT_VERSION,
            node_type: node_types::NODE4,
            flags: 0,
            encoding_flags: 0,
            num_children: 0,
            prefix_len: 8,
            _padding: 0,
            data_size: 50,
        };
        let header_bytes = header.to_bytes();

        // Only include header + 4 bytes of prefix (claims 8)
        let mut data = Vec::new();
        data.extend_from_slice(&header_bytes);
        data.extend_from_slice(&[0u8; 4]); // Truncated prefix

        let result = from_bytes(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_deserialize_truncated_children_node4() {
        // Header claims 2 children but data is truncated
        let node4 = Node::N4(Box::new(Node4::new()));
        let mut bytes = to_bytes(&node4).expect("serialize");

        // Corrupt header to claim more children exist
        let header_arr: [u8; SERIALIZED_HEADER_SIZE] = bytes[0..SERIALIZED_HEADER_SIZE]
            .try_into()
            .expect("header slice should be 16 bytes");
        let mut header = SerializedNodeHeader::from_bytes(&header_arr);
        header.num_children = 4;
        bytes[0..SERIALIZED_HEADER_SIZE].copy_from_slice(&header.to_bytes());

        // Truncate the data
        bytes.truncate(20);

        let result = from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_deserialize_empty_data() {
        let result = from_bytes(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_roundtrip_with_max_prefix() {
        let mut node4 = Node4::new();
        // MAX_PREFIX_LEN is 8 for byte nodes
        node4.prefix = CompressedPrefix::from_bytes(b"12345678");
        node4.header.prefix_len = 8;
        node4.header.set_final(true);

        let node = Node::N4(Box::new(node4));
        let bytes = to_bytes(&node).expect("serialize");
        let restored = from_bytes(&bytes).expect("deserialize");

        assert_eq!(restored.header().prefix_len, 8);
        assert!(restored.header().is_final());
    }

    #[test]
    fn test_deserialize_invalid_prefix_len() {
        // prefix_len > MAX_PREFIX_LEN (8)
        let mut header = SerializedNodeHeader {
            magic: NODE_MAGIC,
            version: FORMAT_VERSION,
            node_type: node_types::NODE4,
            flags: 0,
            encoding_flags: 0,
            num_children: 0,
            prefix_len: 20, // Too long
            _padding: 0,
            data_size: 50,
        };
        let bytes = header.to_bytes();
        let result = from_bytes(&bytes);
        assert!(matches!(
            result,
            Err(PersistentARTrieError::CorruptedFile { .. })
        ));
    }

    #[test]
    fn test_serialize_all_node_types() {
        // Test that all node types can be serialized and deserialized
        let nodes: Vec<Node> = vec![
            Node::N4(Box::new(Node4::new())),
            Node::N16(Box::new(Node16::new())),
            Node::N48(Box::new(Node48::new())),
            Node::N256(Box::new(Node256::new())),
        ];

        for node in nodes {
            let bytes = to_bytes(&node).expect("serialize");
            assert!(!bytes.is_empty());
            let restored = from_bytes(&bytes).expect("deserialize");
            assert_eq!(restored.header().num_children, node.header().num_children);
        }
    }

    #[test]
    fn test_node_type_constants() {
        // Verify node type constants match the defined values
        assert_eq!(node_types::NODE4, 4);
        assert_eq!(node_types::NODE16, 16);
        assert_eq!(node_types::NODE48, 48);
        assert_eq!(node_types::NODE256, 0); // Uses 0 to match in-memory representation
    }

    #[test]
    fn test_header_size() {
        // Verify header size is as expected (16 bytes)
        assert_eq!(SERIALIZED_HEADER_SIZE, 16);
        let header = SerializedNodeHeader {
            magic: NODE_MAGIC,
            version: 1,
            node_type: node_types::NODE4,
            flags: 0,
            encoding_flags: 0,
            num_children: 0,
            prefix_len: 0,
            _padding: 0,
            data_size: 0,
        };
        assert_eq!(header.to_bytes().len(), SERIALIZED_HEADER_SIZE);
    }

    #[test]
    fn test_all_flag_combinations() {
        // Test serialization with various flag combinations
        let flag_combinations = [
            0u8,
            flags::IS_FINAL,
            flags::IS_DIRTY,
            flags::IS_FINAL | flags::IS_DIRTY,
        ];

        for flags_val in flag_combinations {
            let mut node4 = Node4::new();
            node4.header.flags = flags_val;

            let node = Node::N4(Box::new(node4));
            let bytes = to_bytes(&node).expect("serialize");
            let restored = from_bytes(&bytes).expect("deserialize");

            // IS_DIRTY should not be preserved in serialization (it's runtime state)
            // Only IS_FINAL should be preserved
            if flags_val & flags::IS_FINAL != 0 {
                assert!(restored.header().is_final());
            }
        }
    }

    // =========================================================================
    // M4a / D-VAL — codec-level value-blob tests (`append_node_value` /
    // `read_node_value`).
    //
    // These exercise the on-disk FORMAT directly — the round-trip + back-compat
    // properties the durable fix rests on — independent of the full
    // overlay-checkpoint→reopen pipeline (covered by the lockfree_cas.rs
    // integration tests `m4a_*`). Cross-validated against an independent
    // re-derivation of M4a (worktree agent a63d0aa8) that, from a `_with_value`
    // codec instead of this append/read layer, landed on the IDENTICAL wire
    // format: HAS_VALUE = 0x20 at encoding-flags byte 7, blob appended last as
    // `[len: u32 LE][bytes]`, value-less records byte-identical. That agreement
    // is strong evidence the format (not just one implementation) is correct.
    // =========================================================================

    /// True iff the record's `HAS_VALUE` encoding-flags bit (byte 7) is set.
    fn record_has_value_flag(record: &[u8]) -> bool {
        record.len() > 7 && (record[7] & encoding_flags::HAS_VALUE) != 0
    }

    /// Build the four node types, each with `child_count` relative-encoded arena
    /// children (so the record exercises the node-type-byte tail that the value
    /// blob is appended after). `child_count == 0` covers the childless case.
    fn sample_nodes_with_children(parent: ArenaSlot, child_count: usize) -> Vec<Node> {
        let make = |mut add: Box<dyn FnMut(u8, SwizzledPtr)>| {
            for i in 0..child_count {
                let slot = ArenaSlot::new(parent.arena_id, parent.slot_id + 1 + i as u32);
                add(i as u8, SwizzledPtr::from_arena_slot(slot, NodeType::Node4));
            }
        };

        let mut n4 = Node4::new();
        make(Box::new(|k, p| {
            let _ = n4.add_child(k, p);
        }));
        let mut n16 = Node16::new();
        make(Box::new(|k, p| {
            let _ = n16.add_child(k, p);
        }));
        let mut n48 = Node48::new();
        make(Box::new(|k, p| {
            let _ = n48.add_child(k, p);
        }));
        let mut n256 = Node256::new();
        make(Box::new(|k, p| {
            let _ = n256.add_child(k, p);
        }));

        vec![
            Node::N4(Box::new(n4)),
            Node::N16(Box::new(n16)),
            Node::N48(Box::new(n48)),
            Node::N256(Box::new(n256)),
        ]
    }

    #[test]
    fn test_value_blob_roundtrip_all_node_types() {
        let parent = ArenaSlot::new(2, 10);
        let ser_ctx = SerializationContext::new(parent);
        let de_ctx = DeserializationContext::new(parent);

        // Opaque value bytes (bincode-of-i64 is 8 bytes, but the codec treats the
        // blob as arbitrary bytes); the embedded 0x00 proves it is not mistaken
        // for a terminator.
        let value: &[u8] = &[0x2A, 0x00, 0xFF, 0x01, 0x10, 0x20, 0x30, 0x40];

        for child_count in [0usize, 1, 3] {
            for node in sample_nodes_with_children(parent, child_count) {
                let node_ty = node.header().node_type;
                let bytes = v2::append_node_value(
                    serialize_node_v2(&node, &ser_ctx).expect("serialize"),
                    Some(value),
                );
                assert!(
                    record_has_value_flag(&bytes),
                    "HAS_VALUE must be set for a valued record (type {node_ty}, {child_count} children)"
                );
                assert_eq!(
                    v2::read_node_value(&bytes).as_deref(),
                    Some(value),
                    "value bytes must round-trip exactly (type {node_ty}, {child_count} children)"
                );
                // The structural node still parses — the value blob, appended
                // after the node-data, never perturbs the node parse.
                let restored = deserialize_node_v2(&bytes, &de_ctx).expect("deserialize");
                assert_eq!(
                    restored.header().num_children,
                    node.header().num_children,
                    "structure must survive (type {node_ty}, {child_count} children)"
                );
            }
        }
    }

    #[test]
    fn test_value_less_record_byte_identical() {
        // `append_node_value(.., None)` must return the legacy buffer UNCHANGED —
        // the back-compat guarantee that pre-M4a files (and every value-less node)
        // stay byte-for-byte identical on disk, so old binaries still read them.
        let parent = ArenaSlot::new(5, 100);
        let ser_ctx = SerializationContext::new(parent);

        for child_count in [0usize, 1, 3, 5] {
            for node in sample_nodes_with_children(parent, child_count) {
                let node_ty = node.header().node_type;
                let legacy = serialize_node_v2(&node, &ser_ctx).expect("legacy serialize");
                let via_none = v2::append_node_value(legacy.clone(), None);
                assert_eq!(
                    legacy, via_none,
                    "value-less record must be byte-identical to the legacy layout \
                     (type {node_ty}, {child_count} children)"
                );
                assert!(
                    !record_has_value_flag(&via_none),
                    "value-less record must NOT set HAS_VALUE (type {node_ty}, {child_count} children)"
                );
                assert!(
                    v2::read_node_value(&via_none).is_none(),
                    "value-less record must read back no value (type {node_ty}, {child_count} children)"
                );
            }
        }
    }

    #[test]
    fn test_legacy_value_less_record_reads_none() {
        // A record written WITHOUT a value (the only kind any pre-M4a binary ever
        // wrote) must read back through `read_node_value` as `None`, and still
        // deserialize structurally.
        let parent = ArenaSlot::new(0, 7);
        let ser_ctx = SerializationContext::new(parent);
        let de_ctx = DeserializationContext::new(parent);

        for child_count in [0usize, 2, 4] {
            for node in sample_nodes_with_children(parent, child_count) {
                let node_ty = node.header().node_type;
                let legacy_bytes = serialize_node_v2(&node, &ser_ctx).expect("legacy serialize");
                assert!(
                    v2::read_node_value(&legacy_bytes).is_none(),
                    "legacy value-less record must read back as no-value (type {node_ty})"
                );
                let restored = deserialize_node_v2(&legacy_bytes, &de_ctx).expect("legacy reader");
                assert_eq!(restored.header().num_children, node.header().num_children);
            }
        }
    }

    #[test]
    fn test_value_blob_empty_and_large() {
        let parent = ArenaSlot::new(1, 1);
        let ser_ctx = SerializationContext::new(parent);
        let node = Node::N4(Box::new(Node4::new()));
        let base = serialize_node_v2(&node, &ser_ctx).expect("serialize");

        // Empty value blob: `Some(&[])` must round-trip as `Some(vec![])` — a
        // present-but-empty value is DISTINCT from absent (`None`).
        let empty = v2::append_node_value(base.clone(), Some(&[]));
        assert!(record_has_value_flag(&empty), "empty value still sets HAS_VALUE");
        assert_eq!(
            v2::read_node_value(&empty),
            Some(Vec::new()),
            "empty value must round-trip as Some(empty), distinct from None"
        );

        // Large value blob (well past a Node256's ~2KB) must round-trip exactly,
        // proving the offset/length math holds for multi-KB blobs.
        let large: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let big = v2::append_node_value(base, Some(&large));
        assert_eq!(
            v2::read_node_value(&big).as_deref(),
            Some(large.as_slice()),
            "large value must round-trip exactly"
        );
    }

    #[test]
    fn test_valued_record_only_grows_by_value_blob() {
        // A valued record must be EXACTLY `4 + value_len` bytes longer than the
        // value-less record (the u32 length prefix + the bytes), and differ ONLY
        // in the encoding-flags byte (offset 7) gaining HAS_VALUE — i.e. the value
        // blob is the sole layout change.
        let parent = ArenaSlot::new(3, 30);
        let ser_ctx = SerializationContext::new(parent);
        let value: &[u8] = &[1, 2, 3, 4, 5, 6, 7];

        for node in sample_nodes_with_children(parent, 2) {
            let node_ty = node.header().node_type;
            let less = serialize_node_v2(&node, &ser_ctx).expect("value-less");
            let valued = v2::append_node_value(less.clone(), Some(value));
            assert_eq!(
                valued.len(),
                less.len() + 4 + value.len(),
                "valued record must grow by exactly the value blob (type {node_ty})"
            );
            // Header bytes before encoding_flags (offset 7) are unchanged.
            assert_eq!(
                &valued[..7],
                &less[..7],
                "header bytes before encoding_flags must be unchanged (type {node_ty})"
            );
            assert_eq!(
                valued[7],
                less[7] | encoding_flags::HAS_VALUE,
                "encoding_flags must gain exactly the HAS_VALUE bit (type {node_ty})"
            );
            // The structural bytes after the flags byte (rest of header + payload)
            // are unchanged — the value blob is strictly appended, nothing spliced.
            assert_eq!(
                &valued[8..less.len()],
                &less[8..],
                "structural bytes after the flags byte must be unchanged (type {node_ty})"
            );
        }
    }
}
