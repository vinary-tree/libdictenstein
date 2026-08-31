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
    encode_children, encode_sequential_siblings, try_decode_child_pointer, RelativeEncodingError,
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
        if self.version < FORMAT_VERSION || self.version > FORMAT_VERSION_V2 {
            return Err(PersistentARTrieError::UnsupportedVersion {
                max_supported: FORMAT_VERSION_V2 as u32,
                found: self.version as u32,
            });
        }
        if self._padding != 0 {
            return Err(PersistentARTrieError::corrupted(format!(
                "node header contains nonzero reserved byte {:#04x}",
                self._padding
            )));
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
        let maximum_children = match self.node_type {
            node_types::NODE4 => 4,
            node_types::NODE16 => 16,
            node_types::NODE48 => 48,
            node_types::NODE256 => 256,
            _ => unreachable!("node type was validated above"),
        };
        if self.num_children as usize > maximum_children {
            return Err(PersistentARTrieError::corrupted(format!(
                "node type {} declares {} children, exceeding capacity {}",
                self.node_type, self.num_children, maximum_children
            )));
        }

        const KNOWN_NODE_FLAGS: u8 = super::nodes::flags::IS_FINAL
            | super::nodes::flags::IS_DIRTY
            | super::nodes::flags::IS_LEAF
            | super::nodes::flags::HAS_DIRTY_DESCENDANTS;
        let unknown_node_flags = self.flags & !KNOWN_NODE_FLAGS;
        if unknown_node_flags != 0 {
            return Err(PersistentARTrieError::corrupted(format!(
                "unknown byte node flags {unknown_node_flags:#04x}"
            )));
        }

        const KNOWN_ENCODING_FLAGS: u8 = encoding_flags::RELATIVE_OFFSETS
            | encoding_flags::SEQUENTIAL_SIBLINGS
            | encoding_flags::HAS_VALUE;
        let unknown_flags = self.encoding_flags & !KNOWN_ENCODING_FLAGS;
        if unknown_flags != 0 {
            return Err(PersistentARTrieError::corrupted(format!(
                "unknown node encoding flags {unknown_flags:#04x}"
            )));
        }
        if self.version < FORMAT_VERSION_V2 && self.encoding_flags != 0 {
            return Err(PersistentARTrieError::corrupted(format!(
                "format version {} cannot carry encoding flags {:#04x}",
                self.version, self.encoding_flags
            )));
        }
        if self.uses_sequential_siblings() && !self.uses_relative_offsets() {
            return Err(PersistentARTrieError::corrupted(
                "sequential sibling encoding requires relative offsets",
            ));
        }
        if self.uses_sequential_siblings() && self.num_children == 0 {
            return Err(PersistentARTrieError::corrupted(
                "sequential sibling encoding requires at least one child",
            ));
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

    fn checked_end(offset: usize, len: usize, section: &str) -> Result<usize> {
        offset.checked_add(len).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "{section} byte-range arithmetic overflow: offset {offset}, length {len}"
            ))
        })
    }

    fn checked_slice<'a>(
        data: &'a [u8],
        offset: usize,
        len: usize,
        section: &str,
    ) -> Result<&'a [u8]> {
        let end = checked_end(offset, len, section)?;
        data.get(offset..end).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "truncated {section}: need byte range {offset}..{end}, record has {} bytes",
                data.len()
            ))
        })
    }

    fn checked_u64(data: &[u8], offset: usize, section: &str) -> Result<u64> {
        let bytes = checked_slice(data, offset, 8, section)?;
        let mut raw = [0u8; 8];
        raw.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(raw))
    }

    fn require_exact_payload_len(data: &[u8], expected: usize, section: &str) -> Result<()> {
        if data.len() != expected {
            return Err(PersistentARTrieError::corrupted(format!(
                "invalid {section} payload length: header provides {} bytes, layout consumes {expected}",
                data.len()
            )));
        }
        Ok(())
    }

    fn validate_sorted_keys(keys: &[u8], count: usize, section: &str) -> Result<()> {
        let active = keys.get(..count).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "{section} declares {count} keys but stores only {}",
                keys.len()
            ))
        })?;
        if active.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(PersistentARTrieError::corrupted(format!(
                "{section} active keys are not strictly increasing"
            )));
        }
        let inactive = keys.get(count..).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "{section} active-key count exceeds its fixed key storage"
            ))
        })?;
        if let Some(index) = inactive.iter().position(|&key| key != 0) {
            let slot = count.checked_add(index).ok_or_else(|| {
                PersistentARTrieError::corrupted(format!(
                    "{section} inactive-key slot index overflows usize"
                ))
            })?;
            return Err(PersistentARTrieError::corrupted(format!(
                "{section} unused key slot {slot} is nonzero"
            )));
        }
        Ok(())
    }

    fn checked_byte_child_type(data: &[u8], offset: usize) -> Result<NodeType> {
        let byte = *data.get(offset).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "missing relative child node type at offset {} in {} byte node payload",
                offset,
                data.len()
            ))
        })?;
        let node_type = NodeType::try_from(byte).map_err(|_| {
            PersistentARTrieError::corrupted(format!(
                "invalid relative child node type {byte} at offset {offset}"
            ))
        })?;
        if !node_type.is_byte_level() {
            return Err(PersistentARTrieError::corrupted(format!(
                "byte node references non-byte child type {node_type:?} at offset {offset}"
            )));
        }
        Ok(node_type)
    }

    fn checked_arena_child(slot: ArenaSlot, node_type: NodeType) -> Result<SwizzledPtr> {
        SwizzledPtr::try_from_arena_slot(slot, node_type).map_err(|error| {
            PersistentARTrieError::corrupted(format!(
                "relative child slot {slot:?} is not representable: {error}"
            ))
        })
    }

    fn checked_fixed_child(data: &[u8], offset: usize, section: &str) -> Result<SwizzledPtr> {
        let raw = checked_u64(data, offset, section)?;
        let pointer = SwizzledPtr::from_raw(raw);
        let location = pointer.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "{section} contains null, in-memory, transitional, or invalid pointer {raw:#018x}"
            ))
        })?;
        if location.block_id == 0 {
            return Err(PersistentARTrieError::corrupted(format!(
                "{section} references reserved block zero"
            )));
        }
        if !location.node_type.is_byte_level() {
            return Err(PersistentARTrieError::corrupted(format!(
                "{section} references non-byte node type {:?}",
                location.node_type
            )));
        }
        Ok(pointer)
    }

    struct ValidatedRecordEnvelope<'a> {
        header: SerializedNodeHeader,
        structural: &'a [u8],
        value: Option<&'a [u8]>,
    }

    fn validated_record_envelope(data: &[u8]) -> Result<ValidatedRecordEnvelope<'_>> {
        let header_slice = checked_slice(data, 0, SERIALIZED_HEADER_SIZE, "node header")?;
        let mut header_bytes = [0u8; SERIALIZED_HEADER_SIZE];
        header_bytes.copy_from_slice(header_slice);
        let header = SerializedNodeHeader::from_bytes(&header_bytes);
        header.validate()?;

        let structural_end = checked_end(
            SERIALIZED_HEADER_SIZE,
            header.data_size as usize,
            "node structural payload",
        )?;
        let structural = data
            .get(SERIALIZED_HEADER_SIZE..structural_end)
            .ok_or_else(|| {
                PersistentARTrieError::corrupted(format!(
                    "truncated node structural payload: header declares {} bytes, record has {} bytes",
                    header.data_size,
                    data.len()
                ))
            })?;

        let value = if header.encoding_flags & encoding_flags::HAS_VALUE != 0 {
            let length_bytes = checked_slice(data, structural_end, 4, "node value length")?;
            let mut encoded_length = [0u8; 4];
            encoded_length.copy_from_slice(length_bytes);
            let value_len = u32::from_le_bytes(encoded_length) as usize;
            let value_start = checked_end(structural_end, 4, "node value prefix")?;
            let value_end = checked_end(value_start, value_len, "node value")?;
            let value = data.get(value_start..value_end).ok_or_else(|| {
                PersistentARTrieError::corrupted(format!(
                    "truncated node value: length prefix declares {value_len} bytes, record has {} bytes",
                    data.len()
                ))
            })?;
            if value_end != data.len() {
                return Err(PersistentARTrieError::corrupted(format!(
                    "valued node record has {} trailing bytes after its declared value",
                    data.len() - value_end
                )));
            }
            Some(value)
        } else {
            if structural_end != data.len() {
                return Err(PersistentARTrieError::corrupted(format!(
                    "value-less node record has {} trailing bytes",
                    data.len() - structural_end
                )));
            }
            None
        };

        Ok(ValidatedRecordEnvelope {
            header,
            structural,
            value,
        })
    }

    fn decode_v2_child_slots(
        data: &[u8],
        parent: ArenaSlot,
        count: usize,
        uses_sequential: bool,
    ) -> Result<(Vec<ArenaSlot>, usize)> {
        let mut children = Vec::new();
        children.try_reserve_exact(count).map_err(|error| {
            PersistentARTrieError::allocation_failed("byte v2 child-slot decoding", count, error)
        })?;

        if count == 0 {
            return Ok((children, 0));
        }

        if uses_sequential {
            let (first_child, bytes_consumed) =
                try_decode_child_pointer(data, parent).map_err(relative_decode_err)?;
            for index in 0..count {
                let offset = u32::try_from(index).map_err(|_| {
                    relative_decode_err(RelativeEncodingError::SequentialIndexTooLarge { index })
                })?;
                let slot_id = first_child.slot_id.checked_add(offset).ok_or_else(|| {
                    relative_decode_err(RelativeEncodingError::SequentialOverflow {
                        first_child,
                        index,
                    })
                })?;
                children.push(ArenaSlot::new(first_child.arena_id, slot_id));
            }
            return Ok((children, bytes_consumed));
        }

        let mut offset = 0usize;
        for _ in 0..count {
            let encoded = data.get(offset..).ok_or_else(|| {
                PersistentARTrieError::corrupted(format!(
                    "relative child offset {offset} exceeds {} byte payload",
                    data.len()
                ))
            })?;
            let (child, consumed) =
                try_decode_child_pointer(encoded, parent).map_err(relative_decode_err)?;
            offset = checked_end(offset, consumed, "relative child list")?;
            children.push(child);
        }
        Ok((children, offset))
    }

    /// Collect child slots from a node for relative encoding
    ///
    /// Returns only valid child slots (filters out null and in-memory pointers).
    pub fn collect_child_slots(node: &Node) -> Vec<ArenaSlot> {
        let mut slots = Vec::with_capacity(node.header().num_children as usize);
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
        let mut result = Vec::with_capacity(node.header().num_children as usize);
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

    fn collect_child_raws(node: &Node) -> Vec<u64> {
        let mut result = Vec::with_capacity(node.header().num_children as usize);
        match node {
            Node::N4(n) => {
                for i in 0..n.header.num_children as usize {
                    result.push(n.children[i].to_raw());
                }
            }
            Node::N16(n) => {
                for i in 0..n.header.num_children as usize {
                    result.push(n.children[i].to_raw());
                }
            }
            Node::N48(n) => {
                for child in &n.children {
                    if !child.is_null() {
                        result.push(child.to_raw());
                    }
                }
            }
            Node::N256(n) => {
                for child in &n.children {
                    if !child.is_null() {
                        result.push(child.to_raw());
                    }
                }
            }
        }
        result
    }

    fn encoded_children_size(
        ctx: &SerializationContext,
        child_slots: &[ArenaSlot],
        fixed_child_count: usize,
    ) -> usize {
        if ctx.use_sequential {
            ctx.first_child_slot
                .map(|first_child| {
                    super::super::relative_encoding::encoded_size(ctx.parent_slot, first_child)
                })
                .unwrap_or(0)
        } else if ctx.use_relative {
            child_slots
                .iter()
                .map(|&child| super::super::relative_encoding::encoded_size(ctx.parent_slot, child))
                .sum()
        } else {
            fixed_child_count * 8
        }
    }

    fn validate_v2_serialization_context(
        node: &Node,
        ctx: &SerializationContext,
        child_slots: &[ArenaSlot],
    ) -> Result<()> {
        let declared_children = node.header().num_children as usize;
        if ctx.use_relative && child_slots.len() != declared_children {
            return Err(PersistentARTrieError::corrupted(format!(
                "byte v2 serialization saw {} disk children but header declares {}",
                child_slots.len(),
                declared_children
            )));
        }
        if ctx.use_sequential {
            if !ctx.use_relative {
                return Err(PersistentARTrieError::corrupted(
                    "byte v2 sequential serialization requires relative encoding",
                ));
            }
            if declared_children == 0 {
                return Err(PersistentARTrieError::corrupted(
                    "byte v2 sequential serialization requires at least one child",
                ));
            }
            let first_child = match ctx.first_child_slot {
                Some(first_child) => first_child,
                None => {
                    return Err(PersistentARTrieError::corrupted(
                        "byte v2 sequential serialization missing first child slot",
                    ));
                }
            };
            // Per-index contiguity re-check (parity with char's serializer; defense-in-depth).
            // The sequential decoder reconstructs child `i` as (first_child.arena_id,
            // first_child.slot_id + i) and pairs it with the i-th key/node-type (written here in
            // `child_slots` order), so the collected child slots MUST equal that progression.
            // Fail loud rather than silently writing a (first_child, count) record whose children
            // the reader would mis-resolve. (check_sequential_children now only selects sequential
            // when the children are consecutive in this order, so this should never fire.)
            for (idx, slot) in child_slots.iter().enumerate() {
                let offset = match u32::try_from(idx) {
                    Ok(offset) => offset,
                    Err(_) => {
                        return Err(PersistentARTrieError::corrupted(
                            "byte v2 sequential child index exceeds u32 slot range",
                        ));
                    }
                };
                let expected_slot = match first_child.slot_id.checked_add(offset) {
                    Some(expected_slot) => expected_slot,
                    None => {
                        return Err(PersistentARTrieError::corrupted(
                            "byte v2 sequential child range overflows u32 slot range",
                        ));
                    }
                };
                if slot.arena_id != first_child.arena_id || slot.slot_id != expected_slot {
                    return Err(PersistentARTrieError::corrupted(format!(
                        "byte v2 sequential child mismatch at index {}: got {:?}, expected arena {} slot {}",
                        idx, slot, first_child.arena_id, expected_slot
                    )));
                }
            }
        }
        Ok(())
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
            let encoded_size = encoded_children_size(ctx, &[], 0);
            // Add 1 byte per child for node type
            (encoded_size, num_children)
        } else if ctx.use_relative {
            // Relative: sum of encoded sizes for each child
            let child_slots = collect_child_slots(node);
            let encoded_size = encoded_children_size(ctx, &child_slots, 0);
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
        // Collect child slots and their node types (needed for type preservation)
        let uses_encoded_children = ctx.use_relative || ctx.use_sequential;
        let child_slots_and_types = if uses_encoded_children {
            collect_child_slots_and_types(node)
        } else {
            Vec::new()
        };
        let child_slots: Vec<ArenaSlot> = child_slots_and_types
            .iter()
            .map(|(slot, _)| *slot)
            .collect();
        validate_v2_serialization_context(node, ctx, &child_slots)?;
        let fixed_child_raws = if uses_encoded_children {
            Vec::new()
        } else {
            collect_child_raws(node)
        };

        // Encode children with relative offsets
        let mut children_buf = Vec::with_capacity(encoded_children_size(
            ctx,
            &child_slots,
            fixed_child_raws.len(),
        ));
        if ctx.use_sequential {
            if let Some(first_child) = ctx.first_child_slot {
                encode_sequential_siblings(ctx.parent_slot, first_child, &mut children_buf);
            }
        } else if ctx.use_relative {
            encode_children(ctx.parent_slot, &child_slots, &mut children_buf);
        } else {
            for raw in &fixed_child_raws {
                children_buf.extend_from_slice(&raw.to_le_bytes());
            }
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
        let node_types_size = if uses_encoded_children {
            child_slots_and_types.len()
        } else {
            0
        };
        let data_size = prefix_size + keys_size + children_buf.len() + node_types_size;
        let mut buffer = Vec::with_capacity(SERIALIZED_HEADER_SIZE + data_size);

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
    pub fn try_append_node_value(
        mut node_bytes: Vec<u8>,
        value_bytes: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        let Some(value_bytes) = value_bytes else {
            return Ok(node_bytes);
        };

        let existing_value = validated_record_envelope(&node_bytes)?.value;
        if existing_value.is_some() {
            return Err(PersistentARTrieError::corrupted(
                "cannot append a second value to an already-valued node record",
            ));
        }
        let value_len =
            u32::try_from(value_bytes.len()).map_err(|_| PersistentARTrieError::ValueTooLarge {
                size: value_bytes.len(),
                max_size: u32::MAX as usize,
            })?;
        let additional = 4usize.checked_add(value_bytes.len()).ok_or_else(|| {
            PersistentARTrieError::corrupted("node value record length overflows usize")
        })?;
        node_bytes.try_reserve_exact(additional).map_err(|error| {
            PersistentARTrieError::allocation_failed("byte v2 node value append", additional, error)
        })?;
        let encoding_byte = node_bytes.get_mut(7).ok_or_else(|| {
            PersistentARTrieError::corrupted("node record is missing encoding-flags byte")
        })?;
        *encoding_byte |= encoding_flags::HAS_VALUE;
        node_bytes.extend_from_slice(&value_len.to_le_bytes());
        node_bytes.extend_from_slice(value_bytes);
        Ok(node_bytes)
    }

    /// Compatibility wrapper around [`try_append_node_value`].
    ///
    /// Persistent write paths use the fallible API. This wrapper preserves the
    /// original public signature for callers that already possess a valid v2
    /// record and a value representable by the on-disk `u32` length field.
    pub fn append_node_value(node_bytes: Vec<u8>, value_bytes: Option<&[u8]>) -> Vec<u8> {
        try_append_node_value(node_bytes, value_bytes)
            .expect("append_node_value requires a canonical node record and representable value")
    }

    /// Read the optional value blob from a node record (the inverse of
    /// [`append_node_value`]). Returns `None` if the `HAS_VALUE` bit is clear (every
    /// pre-M4a record) or the trailing bytes are absent/truncated. The value starts
    /// at `SERIALIZED_HEADER_SIZE + data_size` (`data_size` is the node-data size from
    /// the header at bytes 12..16; `encoding_flags` is byte 7).
    pub fn try_read_node_value(data: &[u8]) -> Result<Option<Vec<u8>>> {
        let value = validated_record_envelope(data)?.value;
        let Some(value) = value else {
            return Ok(None);
        };
        let mut owned = Vec::new();
        owned.try_reserve_exact(value.len()).map_err(|error| {
            PersistentARTrieError::allocation_failed("byte v2 node value read", value.len(), error)
        })?;
        owned.extend_from_slice(value);
        Ok(Some(owned))
    }

    /// Compatibility reader that maps malformed records to `None`.
    ///
    /// Persistent fault and recovery paths use [`try_read_node_value`] so
    /// corruption is never mistaken for an absent value.
    pub fn read_node_value(data: &[u8]) -> Option<Vec<u8>> {
        try_read_node_value(data).ok().flatten()
    }

    /// Structural metadata decoded from one exact byte-arena record.
    ///
    /// The value payload is validated and skipped but never copied or
    /// deserialized. This is the representation used to reconstruct eviction
    /// topology without faulting application values into memory.
    #[derive(Debug)]
    pub(crate) struct DecodedByteNodeMetadata {
        pub(crate) node_type: NodeType,
        pub(crate) serialized_bytes: usize,
        pub(crate) prefix: Vec<u8>,
        pub(crate) children: Vec<(u8, SwizzledPtr)>,
    }

    fn decode_metadata_pointer_list(
        header: &SerializedNodeHeader,
        data: &[u8],
        children_start: usize,
        ctx: &DeserializationContext,
        legacy_capacity: usize,
        section: &'static str,
    ) -> Result<Vec<SwizzledPtr>> {
        let num_children = header.num_children as usize;
        let mut pointers = Vec::new();
        pointers.try_reserve_exact(num_children).map_err(|error| {
            PersistentARTrieError::allocation_failed(section, num_children, error)
        })?;

        if header.uses_relative_offsets() {
            let encoded = data.get(children_start..).ok_or_else(|| {
                PersistentARTrieError::corrupted(format!(
                    "{section} starts beyond its node payload"
                ))
            })?;
            let (slots, bytes_consumed) = decode_v2_child_slots(
                encoded,
                ctx.parent_slot,
                num_children,
                header.uses_sequential_siblings(),
            )?;
            let types_start = checked_end(children_start, bytes_consumed, section)?;
            let expected = checked_end(types_start, num_children, section)?;
            require_exact_payload_len(data, expected, section)?;
            for (index, slot) in slots.into_iter().enumerate() {
                let type_offset = checked_end(types_start, index, section)?;
                let node_type = checked_byte_child_type(data, type_offset)?;
                pointers.push(checked_arena_child(slot, node_type)?);
            }
            return Ok(pointers);
        }

        let stored_children = if header.version == FORMAT_VERSION {
            legacy_capacity
        } else {
            num_children
        };
        if stored_children < num_children {
            return Err(PersistentARTrieError::corrupted(format!(
                "{section} stores {stored_children} pointer slots for {num_children} children"
            )));
        }
        let child_bytes = stored_children.checked_mul(8).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!("{section} byte count overflows usize"))
        })?;
        let expected = checked_end(children_start, child_bytes, section)?;
        require_exact_payload_len(data, expected, section)?;
        for index in 0..stored_children {
            let relative_offset = index.checked_mul(8).ok_or_else(|| {
                PersistentARTrieError::corrupted(format!("{section} offset overflows usize"))
            })?;
            let offset = checked_end(children_start, relative_offset, section)?;
            if index < num_children {
                pointers.push(checked_fixed_child(data, offset, section)?);
            } else if checked_u64(data, offset, section)? != 0 {
                return Err(PersistentARTrieError::corrupted(format!(
                    "{section} unused pointer slot {index} is non-null"
                )));
            }
        }
        Ok(pointers)
    }

    fn decode_node48_metadata_pointers(
        header: &SerializedNodeHeader,
        data: &[u8],
        ctx: &DeserializationContext,
        used_slots: &[u8],
    ) -> Result<Vec<SwizzledPtr>> {
        if header.version != FORMAT_VERSION || header.uses_relative_offsets() {
            return decode_metadata_pointer_list(
                header,
                data,
                256,
                ctx,
                header.num_children as usize,
                "node48 metadata children",
            );
        }

        let expected = checked_end(256, 48usize * 8, "node48 legacy metadata children")?;
        require_exact_payload_len(data, expected, "node48 legacy metadata")?;
        let mut active_slots = [false; 48];
        for &slot in used_slots {
            active_slots[slot as usize] = true;
        }
        let mut pointers = Vec::new();
        pointers
            .try_reserve_exact(used_slots.len())
            .map_err(|error| {
                PersistentARTrieError::allocation_failed(
                    "node48 legacy metadata children",
                    used_slots.len(),
                    error,
                )
            })?;
        for (slot, active) in active_slots.into_iter().enumerate() {
            let offset = checked_end(
                256,
                slot.checked_mul(8).ok_or_else(|| {
                    PersistentARTrieError::corrupted("node48 legacy metadata child offset overflow")
                })?,
                "node48 legacy metadata child",
            )?;
            if active {
                pointers.push(checked_fixed_child(
                    data,
                    offset,
                    "node48 legacy metadata child",
                )?);
            } else if checked_u64(data, offset, "node48 legacy metadata unused child")? != 0 {
                return Err(PersistentARTrieError::corrupted(format!(
                    "node48 legacy metadata unused child slot {slot} is non-null"
                )));
            }
        }
        Ok(pointers)
    }

    fn try_pair_dense_metadata_children(
        keys: &[u8],
        pointers: Vec<SwizzledPtr>,
        section: &'static str,
    ) -> Result<Vec<(u8, SwizzledPtr)>> {
        validate_sorted_keys(keys, pointers.len(), section)?;
        let mut children = Vec::new();
        children
            .try_reserve_exact(pointers.len())
            .map_err(|error| {
                PersistentARTrieError::allocation_failed(section, pointers.len(), error)
            })?;
        for (&key, pointer) in keys.iter().zip(pointers) {
            children.push((key, pointer));
        }
        Ok(children)
    }

    /// Decode only path and child metadata from one exact persistent byte-node
    /// record. The optional value envelope is checked for canonical length but
    /// its bytes are not allocated, copied, or deserialized.
    pub(crate) fn decode_node_metadata(
        data: &[u8],
        ctx: &DeserializationContext,
        expected_type: Option<NodeType>,
    ) -> Result<DecodedByteNodeMetadata> {
        let ValidatedRecordEnvelope {
            header, structural, ..
        } = validated_record_envelope(data)?;
        let node_type = NodeType::try_from(header.node_type).map_err(|_| {
            PersistentARTrieError::corrupted(format!(
                "invalid byte-node metadata type {}",
                header.node_type
            ))
        })?;
        if !node_type.is_byte_level() || node_type == NodeType::Bucket {
            return Err(PersistentARTrieError::corrupted(format!(
                "byte-node record has non-ART type {node_type:?}"
            )));
        }
        if let Some(expected_type) = expected_type {
            if !expected_type.is_byte_level() || expected_type != node_type {
                return Err(PersistentARTrieError::NodeTypeMismatch {
                    expected: format!("{expected_type:?}"),
                    found: format!("{node_type:?}"),
                });
            }
        }

        let prefix_storage_len = if header.prefix_len > 0 {
            MAX_PREFIX_LEN
        } else {
            0
        };
        let stored_prefix = checked_slice(
            structural,
            0,
            prefix_storage_len,
            "byte-node metadata prefix",
        )?;
        let logical_prefix_len = header.prefix_len as usize;
        let logical_prefix = stored_prefix.get(..logical_prefix_len).ok_or_else(|| {
            PersistentARTrieError::corrupted(
                "byte-node logical prefix exceeds its fixed prefix storage",
            )
        })?;
        let mut prefix = Vec::new();
        prefix
            .try_reserve_exact(logical_prefix_len)
            .map_err(|error| {
                PersistentARTrieError::allocation_failed(
                    "byte-node metadata prefix",
                    logical_prefix_len,
                    error,
                )
            })?;
        prefix.extend_from_slice(logical_prefix);
        let payload = structural.get(prefix_storage_len..).ok_or_else(|| {
            PersistentARTrieError::corrupted("byte-node metadata prefix exceeds structural payload")
        })?;

        let children = match node_type {
            NodeType::Node4 => {
                let keys = checked_slice(payload, 0, 4, "node4 metadata keys")?;
                let pointers = decode_metadata_pointer_list(
                    &header,
                    payload,
                    4,
                    ctx,
                    4,
                    "node4 metadata children",
                )?;
                try_pair_dense_metadata_children(keys, pointers, "node4 metadata")?
            }
            NodeType::Node16 => {
                let keys = checked_slice(payload, 0, 16, "node16 metadata keys")?;
                let pointers = decode_metadata_pointer_list(
                    &header,
                    payload,
                    16,
                    ctx,
                    16,
                    "node16 metadata children",
                )?;
                try_pair_dense_metadata_children(keys, pointers, "node16 metadata")?
            }
            NodeType::Node48 => {
                let index_bytes = checked_slice(payload, 0, 256, "node48 metadata index")?;
                let mut index = [NO_CHILD; 256];
                index.copy_from_slice(index_bytes);
                let used_slots = collect_node48_used_slots(&index, header.num_children as usize)?;
                let pointers = decode_node48_metadata_pointers(&header, payload, ctx, &used_slots)?;
                let mut by_slot: [Option<SwizzledPtr>; 48] = std::array::from_fn(|_| None);
                for (&slot, pointer) in used_slots.iter().zip(pointers) {
                    by_slot[slot as usize] = Some(pointer);
                }
                let mut children = Vec::new();
                children
                    .try_reserve_exact(header.num_children as usize)
                    .map_err(|error| {
                        PersistentARTrieError::allocation_failed(
                            "node48 metadata edges",
                            header.num_children as usize,
                            error,
                        )
                    })?;
                for (key, &slot) in index.iter().enumerate() {
                    if slot == NO_CHILD {
                        continue;
                    }
                    let pointer = by_slot[slot as usize].take().ok_or_else(|| {
                        PersistentARTrieError::corrupted(format!(
                            "node48 metadata slot {slot} is referenced more than once"
                        ))
                    })?;
                    children.push((key as u8, pointer));
                }
                children
            }
            NodeType::Node256 => {
                let mut bitmap = [0u64; 4];
                for (index, word) in bitmap.iter_mut().enumerate() {
                    let offset = index.checked_mul(8).ok_or_else(|| {
                        PersistentARTrieError::corrupted("node256 metadata bitmap offset overflow")
                    })?;
                    *word = checked_u64(payload, offset, "node256 metadata bitmap")?;
                }
                let bitmap_children = bitmap.iter().try_fold(0usize, |count, word| {
                    count.checked_add(word.count_ones() as usize)
                });
                if bitmap_children != Some(header.num_children as usize) {
                    return Err(PersistentARTrieError::corrupted(format!(
                        "node256 metadata bitmap count {:?} differs from header count {}",
                        bitmap_children, header.num_children
                    )));
                }
                let pointers = decode_metadata_pointer_list(
                    &header,
                    payload,
                    32,
                    ctx,
                    header.num_children as usize,
                    "node256 metadata children",
                )?;
                let mut pointers = pointers.into_iter();
                let mut children = Vec::new();
                children
                    .try_reserve_exact(header.num_children as usize)
                    .map_err(|error| {
                        PersistentARTrieError::allocation_failed(
                            "node256 metadata edges",
                            header.num_children as usize,
                            error,
                        )
                    })?;
                for key in 0..256usize {
                    if bitmap[key / 64] & (1u64 << (key % 64)) == 0 {
                        continue;
                    }
                    let pointer = pointers.next().ok_or_else(|| {
                        PersistentARTrieError::corrupted(
                            "node256 metadata bitmap exceeds decoded child pointers",
                        )
                    })?;
                    children.push((key as u8, pointer));
                }
                if pointers.next().is_some() {
                    return Err(PersistentARTrieError::corrupted(
                        "node256 metadata decoded more pointers than its bitmap references",
                    ));
                }
                children
            }
            NodeType::Bucket
            | NodeType::CharNode4
            | NodeType::CharNode16
            | NodeType::CharNode48
            | NodeType::CharBucket => {
                return Err(PersistentARTrieError::corrupted(
                    "non-ART node type reached byte metadata decoder",
                ));
            }
        };

        Ok(DecodedByteNodeMetadata {
            node_type,
            serialized_bytes: data.len(),
            prefix,
            children,
        })
    }

    /// Deserialize a node with v2 encoding (handles both relative and fixed)
    pub fn deserialize_node_v2(data: &[u8], ctx: &DeserializationContext) -> Result<Node> {
        let ValidatedRecordEnvelope {
            header, structural, ..
        } = validated_record_envelope(data)?;
        let prefix_storage_len = if header.prefix_len > 0 {
            MAX_PREFIX_LEN
        } else {
            0
        };
        let prefix = if prefix_storage_len > 0 {
            let stored = checked_slice(
                structural,
                0,
                prefix_storage_len,
                "compressed byte-node prefix",
            )?;
            let mut prefix_bytes = [0u8; MAX_PREFIX_LEN];
            prefix_bytes.copy_from_slice(stored);
            CompressedPrefix {
                bytes: prefix_bytes,
            }
        } else {
            CompressedPrefix::empty()
        };
        let remaining = structural.get(prefix_storage_len..).ok_or_else(|| {
            PersistentARTrieError::corrupted(
                "compressed byte-node prefix exceeds structural payload",
            )
        })?;

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

        let num_children = header.num_children as usize;
        let key_bytes = checked_slice(data, 0, 4, "node4 keys")?;
        node.keys.copy_from_slice(key_bytes);
        validate_sorted_keys(&node.keys, num_children, "node4")?;

        // Decode children based on encoding mode
        if header.uses_relative_offsets() {
            let encoded = data.get(4..).ok_or_else(|| {
                PersistentARTrieError::corrupted("node4 child payload starts beyond record")
            })?;
            let (children, bytes_consumed) = decode_v2_child_slots(
                encoded,
                ctx.parent_slot,
                num_children,
                header.uses_sequential_siblings(),
            )?;
            let types_start = checked_end(4, bytes_consumed, "node4 child encodings")?;
            let expected = checked_end(types_start, num_children, "node4 child types")?;
            require_exact_payload_len(data, expected, "node4")?;
            for (i, slot) in children.into_iter().enumerate() {
                let type_offset = checked_end(types_start, i, "node4 child type index")?;
                let node_type = checked_byte_child_type(data, type_offset)?;
                node.children[i] = checked_arena_child(slot, node_type)?;
            }
        } else {
            let stored_children = if header.version == FORMAT_VERSION {
                4usize
            } else {
                num_children
            };
            let child_bytes = stored_children.checked_mul(8).ok_or_else(|| {
                PersistentARTrieError::corrupted("node4 fixed child byte count overflow")
            })?;
            let expected = checked_end(4, child_bytes, "node4 fixed children")?;
            require_exact_payload_len(data, expected, "node4")?;
            for i in 0..stored_children {
                let offset = checked_end(
                    4,
                    i.checked_mul(8).ok_or_else(|| {
                        PersistentARTrieError::corrupted("node4 child offset overflow")
                    })?,
                    "node4 child pointer",
                )?;
                if i < num_children {
                    node.children[i] = checked_fixed_child(data, offset, "node4 child")?;
                } else if checked_u64(data, offset, "node4 unused child")? != 0 {
                    return Err(PersistentARTrieError::corrupted(format!(
                        "node4 unused child slot {i} is non-null"
                    )));
                }
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

        let num_children = header.num_children as usize;
        let key_bytes = checked_slice(data, 0, 16, "node16 keys")?;
        node.keys.copy_from_slice(key_bytes);
        validate_sorted_keys(&node.keys, num_children, "node16")?;

        // Decode children based on encoding mode
        if header.uses_relative_offsets() {
            let encoded = data.get(16..).ok_or_else(|| {
                PersistentARTrieError::corrupted("node16 child payload starts beyond record")
            })?;
            let (children, bytes_consumed) = decode_v2_child_slots(
                encoded,
                ctx.parent_slot,
                num_children,
                header.uses_sequential_siblings(),
            )?;
            let types_start = checked_end(16, bytes_consumed, "node16 child encodings")?;
            let expected = checked_end(types_start, num_children, "node16 child types")?;
            require_exact_payload_len(data, expected, "node16")?;
            for (i, slot) in children.into_iter().enumerate() {
                let type_offset = checked_end(types_start, i, "node16 child type index")?;
                let node_type = checked_byte_child_type(data, type_offset)?;
                node.children[i] = checked_arena_child(slot, node_type)?;
            }
        } else {
            let stored_children = if header.version == FORMAT_VERSION {
                16usize
            } else {
                num_children
            };
            let child_bytes = stored_children.checked_mul(8).ok_or_else(|| {
                PersistentARTrieError::corrupted("node16 fixed child byte count overflow")
            })?;
            let expected = checked_end(16, child_bytes, "node16 fixed children")?;
            require_exact_payload_len(data, expected, "node16")?;
            for i in 0..stored_children {
                let offset = checked_end(
                    16,
                    i.checked_mul(8).ok_or_else(|| {
                        PersistentARTrieError::corrupted("node16 child offset overflow")
                    })?,
                    "node16 child pointer",
                )?;
                if i < num_children {
                    node.children[i] = checked_fixed_child(data, offset, "node16 child")?;
                } else if checked_u64(data, offset, "node16 unused child")? != 0 {
                    return Err(PersistentARTrieError::corrupted(format!(
                        "node16 unused child slot {i} is non-null"
                    )));
                }
            }
        }

        Ok(Node::N16(Box::new(node)))
    }

    fn collect_node48_used_slots(index: &[u8; 256], num_children: usize) -> Result<Vec<u8>> {
        let mut seen_slots = 0u64;
        let mut used_slots = Vec::new();
        used_slots
            .try_reserve_exact(num_children.min(48))
            .map_err(|error| {
                PersistentARTrieError::allocation_failed(
                    "node48 used-slot decoding",
                    num_children.min(48),
                    error,
                )
            })?;

        for &slot in index {
            if slot == NO_CHILD {
                continue;
            }
            if slot as usize >= 48 {
                return Err(PersistentARTrieError::corrupted(format!(
                    "node48 index references invalid child slot {}",
                    slot
                )));
            }

            let bit = 1u64 << slot;
            if seen_slots & bit == 0 {
                seen_slots |= bit;
                used_slots.push(slot);
            }
        }

        used_slots.sort_unstable();
        if used_slots.len() != num_children {
            return Err(PersistentARTrieError::corrupted(format!(
                "node48 index contains {} unique child slots but header declares {num_children}",
                used_slots.len()
            )));
        }
        Ok(used_slots)
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

        let num_children = header.num_children as usize;
        let index_bytes = checked_slice(data, 0, 256, "node48 index")?;
        node.index.copy_from_slice(index_bytes);

        // Build a sorted list of used slots from the index array.
        // During serialization, children are collected in slot order (0..48),
        // so we must place them back at their original slot positions.
        let used_slots = collect_node48_used_slots(&node.index, num_children)?;

        // Decode children based on encoding mode
        if header.uses_relative_offsets() {
            let encoded = data.get(256..).ok_or_else(|| {
                PersistentARTrieError::corrupted("node48 child payload starts beyond record")
            })?;
            let (children, bytes_consumed) = decode_v2_child_slots(
                encoded,
                ctx.parent_slot,
                num_children,
                header.uses_sequential_siblings(),
            )?;
            let types_start = checked_end(256, bytes_consumed, "node48 child encodings")?;
            let expected = checked_end(types_start, num_children, "node48 child types")?;
            require_exact_payload_len(data, expected, "node48")?;
            for (i, child_slot) in children.into_iter().enumerate() {
                let actual_slot = used_slots[i] as usize;
                let type_offset = checked_end(types_start, i, "node48 child type index")?;
                let node_type = checked_byte_child_type(data, type_offset)?;
                node.children[actual_slot] = checked_arena_child(child_slot, node_type)?;
            }
        } else if header.version == FORMAT_VERSION {
            let child_bytes = 48usize.checked_mul(8).ok_or_else(|| {
                PersistentARTrieError::corrupted("node48 legacy child byte count overflow")
            })?;
            let expected = checked_end(256, child_bytes, "node48 legacy children")?;
            require_exact_payload_len(data, expected, "node48")?;
            let mut active_slots = [false; 48];
            for &slot in &used_slots {
                active_slots[slot as usize] = true;
            }
            for (slot, active) in active_slots.into_iter().enumerate() {
                let offset = checked_end(
                    256,
                    slot.checked_mul(8).ok_or_else(|| {
                        PersistentARTrieError::corrupted("node48 legacy child offset overflow")
                    })?,
                    "node48 legacy child pointer",
                )?;
                if active {
                    node.children[slot] = checked_fixed_child(data, offset, "node48 legacy child")?;
                } else if checked_u64(data, offset, "node48 legacy unused child")? != 0 {
                    return Err(PersistentARTrieError::corrupted(format!(
                        "node48 unused child slot {slot} is non-null"
                    )));
                }
            }
        } else {
            let child_bytes = num_children.checked_mul(8).ok_or_else(|| {
                PersistentARTrieError::corrupted("node48 fixed child byte count overflow")
            })?;
            let expected = checked_end(256, child_bytes, "node48 fixed children")?;
            require_exact_payload_len(data, expected, "node48")?;
            for (i, &actual_slot) in used_slots.iter().take(num_children).enumerate() {
                let actual_slot = actual_slot as usize;
                let offset = checked_end(
                    256,
                    i.checked_mul(8).ok_or_else(|| {
                        PersistentARTrieError::corrupted("node48 fixed child offset overflow")
                    })?,
                    "node48 fixed child pointer",
                )?;
                node.children[actual_slot] =
                    checked_fixed_child(data, offset, "node48 fixed child")?;
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

        let mut bitmap = [0u64; 4];
        for (i, word) in bitmap.iter_mut().enumerate() {
            let offset = i.checked_mul(8).ok_or_else(|| {
                PersistentARTrieError::corrupted("node256 bitmap offset overflow")
            })?;
            *word = checked_u64(data, offset, "node256 bitmap")?;
        }

        let num_children = header.num_children as usize;
        let children_start = 32; // After bitmap
        let bitmap_children = bitmap
            .iter()
            .try_fold(0usize, |count, word| {
                count.checked_add(word.count_ones() as usize)
            })
            .ok_or_else(|| {
                PersistentARTrieError::corrupted("node256 bitmap child count overflow")
            })?;
        if bitmap_children != num_children {
            return Err(PersistentARTrieError::corrupted(format!(
                "node256 bitmap contains {bitmap_children} children but header declares {num_children}"
            )));
        }

        // Decode children based on encoding mode
        if header.uses_relative_offsets() {
            let encoded = data.get(children_start..).ok_or_else(|| {
                PersistentARTrieError::corrupted("node256 child payload starts beyond record")
            })?;
            let (children, bytes_consumed) = decode_v2_child_slots(
                encoded,
                ctx.parent_slot,
                num_children,
                header.uses_sequential_siblings(),
            )?;
            let types_start =
                checked_end(children_start, bytes_consumed, "node256 child encodings")?;
            let expected = checked_end(types_start, num_children, "node256 child types")?;
            require_exact_payload_len(data, expected, "node256")?;
            let mut child_idx = 0usize;
            for i in 0..256 {
                if bitmap[i / 64] & (1u64 << (i % 64)) != 0 {
                    let type_offset =
                        checked_end(types_start, child_idx, "node256 child type index")?;
                    let node_type = checked_byte_child_type(data, type_offset)?;
                    node.children[i] = checked_arena_child(children[child_idx], node_type)?;
                    child_idx += 1;
                }
            }
        } else {
            let child_bytes = num_children.checked_mul(8).ok_or_else(|| {
                PersistentARTrieError::corrupted("node256 fixed child byte count overflow")
            })?;
            let expected = checked_end(children_start, child_bytes, "node256 fixed children")?;
            require_exact_payload_len(data, expected, "node256")?;
            let mut child_idx = 0usize;
            for i in 0..256 {
                if bitmap[i / 64] & (1u64 << (i % 64)) != 0 {
                    let offset = checked_end(
                        children_start,
                        child_idx.checked_mul(8).ok_or_else(|| {
                            PersistentARTrieError::corrupted("node256 fixed child offset overflow")
                        })?,
                        "node256 fixed child pointer",
                    )?;
                    node.children[i] = checked_fixed_child(data, offset, "node256 fixed child")?;
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
    use super::v2::decode_node_metadata;
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
        let node4 = Node::N4(Box::default());
        assert_eq!(serialized_size(&node4), 16 + (4 + 32)); // header + prefix + data

        // Node4 with prefix
        let mut node4_with_prefix = Node4::new();
        node4_with_prefix.prefix = CompressedPrefix::from_bytes(b"test");
        node4_with_prefix.header.prefix_len = 4;
        let node4_p = Node::N4(Box::new(node4_with_prefix));
        assert_eq!(serialized_size(&node4_p), 16 + 12 + (4 + 32)); // header + MAX_PREFIX_LEN + data

        // Node16
        let node16 = Node::N16(Box::default());
        assert_eq!(serialized_size(&node16), 16 + (16 + 128));

        // Node48
        let node48 = Node::N48(Box::default());
        assert_eq!(serialized_size(&node48), 16 + (256 + 384));

        // Node256 with 5 children
        let mut node256 = Node256::new();
        for i in 0..5 {
            node256
                .add_child(i, SwizzledPtr::on_disk(i as u32, 0, NodeType::Node4))
                .expect("add");
        }
        let node256_node = Node::N256(Box::new(node256));
        assert_eq!(serialized_size(&node256_node), 16 + (32 + 5 * 8)); // bitmap + 5 children
    }

    #[test]
    fn test_empty_node_roundtrip() {
        // Test that empty nodes serialize and deserialize correctly
        for create_node in [
            || Node::N4(Box::default()),
            || Node::N16(Box::default()),
            || Node::N48(Box::default()),
            || Node::N256(Box::default()),
        ] {
            let node = create_node();
            let bytes = to_bytes(&node).expect("serialize");
            let restored = from_bytes(&bytes).expect("deserialize");
            assert_eq!(restored.header().num_children, 0);
        }
    }

    #[test]
    fn test_v2_fixed_encoding_roundtrip_uses_fixed_child_words() {
        let parent = ArenaSlot::new(7, 100);
        let child_a = SwizzledPtr::on_disk(3, 11, NodeType::Node16);
        let child_b = SwizzledPtr::on_disk(4, 22, NodeType::Node48);
        let child_a_raw = child_a.to_raw();
        let child_b_raw = child_b.to_raw();

        let mut node4 = Node4::new();
        node4.add_child(b'a', child_a).expect("add child a");
        node4.add_child(b'b', child_b).expect("add child b");
        let node = Node::N4(Box::new(node4));

        let mut ctx = SerializationContext::new(parent);
        ctx.use_relative = false;
        ctx.use_sequential = false;
        ctx.first_child_slot = None;

        let bytes = serialize_node_v2(&node, &ctx).expect("serialize fixed v2");
        let header_arr: [u8; SERIALIZED_HEADER_SIZE] = bytes[..SERIALIZED_HEADER_SIZE]
            .try_into()
            .expect("header slice should be 16 bytes");
        let header = SerializedNodeHeader::from_bytes(&header_arr);

        assert!(!header.uses_relative_offsets());
        assert!(!header.uses_sequential_siblings());
        assert_eq!(header.data_size as usize, 4 + 2 * 8);
        assert_eq!(bytes.len(), SERIALIZED_HEADER_SIZE + 4 + 2 * 8);

        let restored =
            deserialize_node_v2(&bytes, &DeserializationContext::new(parent)).expect("deserialize");
        assert_eq!(restored.header().num_children, 2);
        assert_eq!(
            restored.find_child(b'a').expect("child a").to_raw(),
            child_a_raw
        );
        assert_eq!(
            restored.find_child(b'b').expect("child b").to_raw(),
            child_b_raw
        );
    }

    #[test]
    fn test_v2_rejects_sequential_without_relative_encoding() {
        let parent = ArenaSlot::new(0, 10);
        let first_child = ArenaSlot::new(0, 11);
        let mut node4 = Node4::new();
        node4
            .add_child(
                b'a',
                SwizzledPtr::from_arena_slot(first_child, NodeType::Node4),
            )
            .expect("add child");
        let node = Node::N4(Box::new(node4));
        let ctx = SerializationContext {
            parent_slot: parent,
            use_relative: false,
            use_sequential: true,
            first_child_slot: Some(first_child),
        };

        assert!(serialize_node_v2(&node, &ctx).is_err());
    }

    #[test]
    fn test_v2_node48_rejects_invalid_index_slot_without_panic() {
        let parent = ArenaSlot::new(0, 100);
        let mut node48 = Node48::new();
        node48
            .add_child(
                7,
                SwizzledPtr::from_arena_slot(ArenaSlot::new(0, 101), NodeType::Node4),
            )
            .expect("add child");
        let node = Node::N48(Box::new(node48));

        let mut bytes =
            serialize_node_v2(&node, &SerializationContext::new(parent)).expect("serialize node48");
        bytes[SERIALIZED_HEADER_SIZE + 7] = 50;

        assert!(matches!(
            deserialize_node_v2(&bytes, &DeserializationContext::new(parent)),
            Err(PersistentARTrieError::CorruptedFile { .. })
        ));
    }

    #[test]
    fn test_v2_node48_fixed_count_mismatch_returns_error() {
        let parent = ArenaSlot::new(0, 100);
        let mut node48 = Node48::new();
        node48
            .add_child(7, SwizzledPtr::on_disk(3, 11, NodeType::Node4))
            .expect("add child");
        let node = Node::N48(Box::new(node48));
        let mut ctx = SerializationContext::new(parent);
        ctx.use_relative = false;
        ctx.use_sequential = false;

        let mut bytes = serialize_node_v2(&node, &ctx).expect("serialize fixed node48");
        let header_arr: [u8; SERIALIZED_HEADER_SIZE] = bytes[..SERIALIZED_HEADER_SIZE]
            .try_into()
            .expect("header slice should be 16 bytes");
        let mut header = SerializedNodeHeader::from_bytes(&header_arr);
        header.num_children = 2;
        bytes[..SERIALIZED_HEADER_SIZE].copy_from_slice(&header.to_bytes());

        assert!(matches!(
            deserialize_node_v2(&bytes, &DeserializationContext::new(parent)),
            Err(PersistentARTrieError::CorruptedFile { .. })
        ));
    }

    fn metadata_node4_record() -> (ArenaSlot, Vec<u8>) {
        let parent = ArenaSlot::new(0, 100);
        let mut node4 = Node4::new();
        node4.prefix = CompressedPrefix::from_bytes(b"xy");
        node4.header.prefix_len = 2;
        node4
            .add_child(
                b'a',
                SwizzledPtr::from_arena_slot(ArenaSlot::new(0, 90), NodeType::Node4),
            )
            .expect("add metadata child a");
        node4
            .add_child(
                b'b',
                SwizzledPtr::from_arena_slot(ArenaSlot::new(0, 91), NodeType::Node16),
            )
            .expect("add metadata child b");
        let bytes = serialize_node_v2(
            &Node::N4(Box::new(node4)),
            &SerializationContext::new(parent),
        )
        .expect("serialize metadata fixture");
        (parent, bytes)
    }

    #[test]
    fn metadata_decoder_matches_full_decoder_without_deserializing_values() {
        let (parent, bytes) = metadata_node4_record();
        let context = DeserializationContext::new(parent);
        let metadata =
            decode_node_metadata(&bytes, &context, Some(NodeType::Node4)).expect("decode metadata");
        let node = deserialize_node_v2(&bytes, &context).expect("decode full node");
        let full_children: Vec<(u8, u64)> = node
            .iter_children()
            .map(|(edge, pointer)| (edge, pointer.to_raw()))
            .collect();
        let metadata_children: Vec<(u8, u64)> = metadata
            .children
            .iter()
            .map(|(edge, pointer)| (*edge, pointer.to_raw()))
            .collect();

        assert_eq!(metadata.node_type, NodeType::Node4);
        assert_eq!(metadata.serialized_bytes, bytes.len());
        assert_eq!(metadata.prefix, b"xy");
        assert_eq!(metadata_children, full_children);
    }

    #[test]
    fn metadata_decoder_rejects_every_truncation_and_trailing_data() {
        let (parent, bytes) = metadata_node4_record();
        let context = DeserializationContext::new(parent);
        for end in 0..bytes.len() {
            assert!(
                decode_node_metadata(&bytes[..end], &context, Some(NodeType::Node4)).is_err(),
                "truncation at byte {end} was accepted"
            );
        }
        assert!(decode_node_metadata(&bytes, &context, Some(NodeType::Node4)).is_ok());

        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode_node_metadata(&trailing, &context, Some(NodeType::Node4)).is_err());
    }

    #[test]
    fn metadata_decoder_rejects_type_and_key_corruption() {
        let (parent, bytes) = metadata_node4_record();
        let context = DeserializationContext::new(parent);
        assert!(matches!(
            decode_node_metadata(&bytes, &context, Some(NodeType::Node16)),
            Err(PersistentARTrieError::NodeTypeMismatch { .. })
        ));

        let mut invalid_child_type = bytes.clone();
        *invalid_child_type
            .last_mut()
            .expect("relative metadata fixture has child type bytes") = 0xff;
        assert!(
            decode_node_metadata(&invalid_child_type, &context, Some(NodeType::Node4)).is_err()
        );

        let mut unsorted = bytes;
        let key_start = SERIALIZED_HEADER_SIZE + MAX_PREFIX_LEN;
        unsorted[key_start] = b'b';
        unsorted[key_start + 1] = b'a';
        assert!(decode_node_metadata(&unsorted, &context, Some(NodeType::Node4)).is_err());
    }

    #[test]
    fn metadata_decoder_never_panics_on_bounded_arbitrary_bytes() {
        let context = DeserializationContext::new(ArenaSlot::new(0, 17));
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for len in 0..=512usize {
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                bytes.push(state as u8);
            }
            let _ = decode_node_metadata(&bytes, &context, None);
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
        let header = SerializedNodeHeader {
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
        let header = SerializedNodeHeader {
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
        let header = SerializedNodeHeader {
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
        let node4 = Node::N4(Box::default());
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
        // MAX_PREFIX_LEN is 12 for byte nodes; this exercises an 8-byte prefix
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
        let header = SerializedNodeHeader {
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
            Node::N4(Box::default()),
            Node::N16(Box::default()),
            Node::N48(Box::default()),
            Node::N256(Box::default()),
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
        let node = Node::N4(Box::default());
        let base = serialize_node_v2(&node, &ser_ctx).expect("serialize");

        // Empty value blob: `Some(&[])` must round-trip as `Some(vec![])` — a
        // present-but-empty value is DISTINCT from absent (`None`).
        let empty = v2::append_node_value(base.clone(), Some(&[]));
        assert!(
            record_has_value_flag(&empty),
            "empty value still sets HAS_VALUE"
        );
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
