//! Node Serialization for Persistent ART (Character-Level)
//!
//! This module provides binary serialization and deserialization for char ART nodes.
//! The format is designed to be:
//! - **Compact**: Minimize disk space usage
//! - **Fast**: Efficient encoding/decoding with minimal allocations
//! - **Versioned**: Support future format evolution
//! - **Unicode-aware**: Proper handling of 4-byte character keys
//!
//! # Serialization Format
//!
//! All nodes share a common header followed by type-specific data:
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────────┐
//! │ SerializedCharNodeHeader (16 bytes)                                │
//! ├───────────┬───────────┬───────────┬───────────┬────────────────────┤
//! │ magic[4]  │ version   │ node_type │ flags     │ reserved[2]        │
//! │ "ARC\0"   │ u8        │ u8        │ u8        │ [u8; 2]            │
//! ├───────────┴───────────┴───────────┴───────────┴────────────────────┤
//! │ num_children: u16     │ prefix_len: u8        │ _padding: u8       │
//! ├───────────────────────┴───────────────────────┴────────────────────┤
//! │ data_size: u32 (size of type-specific data)                        │
//! └────────────────────────────────────────────────────────────────────┘
//! │ CharCompressedPrefix (24 bytes, if prefix_len > 0)                 │
//! └────────────────────────────────────────────────────────────────────┘
//! │ Type-specific data (variable size)                                 │
//! └────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Type-Specific Layouts
//!
//! Serialized child and value pointers are 64-bit disk/null `SwizzledPtr`
//! state words. In-memory `SwizzledPtr` values keep pointer provenance in a
//! separate runtime slot and cannot be reconstructed from serialized integers.
//!
//! ## CharNode4
//! ```text
//! │ keys: [u32; 4]        │ 16 bytes                                   │
//! │ children: [u64; 4]    │ 32 bytes (disk/null SwizzledPtr state)     │
//! │ value_ptr: u64        │ 8 bytes                                    │
//! Total: 56 bytes + header
//! ```
//!
//! ## CharNode16
//! ```text
//! │ keys: [u32; 16]       │ 64 bytes                                   │
//! │ children: [u64; 16]   │ 128 bytes (disk/null SwizzledPtr state)    │
//! │ value_ptr: u64        │ 8 bytes                                    │
//! Total: 200 bytes + header
//! ```
//!
//! ## CharNode48
//! ```text
//! │ keys: [u32; 48]       │ 192 bytes (sorted for binary search)       │
//! │ children: [u64; 48]   │ 384 bytes (disk/null SwizzledPtr state)    │
//! │ value_ptr: u64        │ 8 bytes                                    │
//! Total: 584 bytes + header
//! ```
//!
//! ## CharBucket
//! ```text
//! │ num_entries: u32      │ 4 bytes                                    │
//! │ value_ptr: u64        │ 8 bytes                                    │
//! │ entries: [(u32, u64)] │ 12 bytes × num_entries                     │
//! Total: 12 + 12*num_entries bytes + header
//! ```

use std::io::{Read, Write};

use smallvec::SmallVec;

use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::swizzled_ptr::{NodeType, SwizzledPtr, MAX_BLOCK_ID, MAX_OFFSET};

use super::nodes::{
    CharBucket, CharCompressedPrefix, CharNode, CharNode16, CharNode4, CharNode48, CharNodeHeader,
    CHAR_BUCKET_TAG, CHAR_MAX_PREFIX_LEN, CHAR_NODE16_TAG, CHAR_NODE48_TAG, CHAR_NODE4_TAG,
};

use super::compact_encoding::{
    decode_compact_node, determine_key_width, determine_ptr_width, encode_compact_node,
    write_varint, CompactHeader, DecodedCompactNode, COMPACT_NODE_TYPE_BUCKET,
    COMPACT_NODE_TYPE_N16, COMPACT_NODE_TYPE_N4, COMPACT_NODE_TYPE_N48,
};

use super::arena_manager::ArenaSlot;

use super::relative_encoding::{
    encode_child_pointer, encoded_size, try_decode_children, try_decode_relative,
    try_decode_sequential_siblings, RelativeEncodingError, SerializationContext, CROSS_ARENA_SIZE,
};
use super::types::ValidatedBorrowedCharNode;

/// Helper to convert io::Error to PersistentARTrieError for serialization operations
fn io_err(e: std::io::Error) -> PersistentARTrieError {
    PersistentARTrieError::io_error("char serialization", "<buffer>", e)
}

/// Magic bytes identifying a char ART node in the serialized format
pub const CHAR_NODE_MAGIC: [u8; 4] = *b"ARC\0"; // ART + Char

/// Established fixed-width and type-erasing relative character-node format.
pub const CHAR_FORMAT_VERSION_V2: u8 = 2;
/// Current character-node format with exact compact child types.
pub const CHAR_FORMAT_VERSION_V3: u8 = 3;
/// Current serialization format version for newly written relative char nodes.
pub const CHAR_FORMAT_VERSION: u8 = CHAR_FORMAT_VERSION_V3;

/// Serialized header size in bytes
pub const CHAR_SERIALIZED_HEADER_SIZE: usize = 16;

/// Child references use parent-relative arena slots.
const FLAG_RELATIVE_OFFSETS: u8 = 0x80;
/// A relative record stores one first-child slot for a contiguous sibling run.
const FLAG_SEQUENTIAL_SIBLINGS: u8 = 0x40;
/// V3 unresolved child types are homogeneous and need no payload bytes.
const FLAG_HOMOGENEOUS_CHILD_TYPES: u8 = 0x20;
const NODE_FLAGS_MASK: u8 =
    super::nodes::flags::IS_FINAL | super::nodes::flags::IS_DIRTY | super::nodes::flags::IS_LEAF;
const V2_ENCODING_FLAGS_MASK: u8 = FLAG_RELATIVE_OFFSETS | FLAG_SEQUENTIAL_SIBLINGS;
const V3_ENCODING_FLAGS_MASK: u8 = V2_ENCODING_FLAGS_MASK | FLAG_HOMOGENEOUS_CHILD_TYPES;
const CHILD_TYPES_PER_HEADER: usize = 8;
const CHILD_TYPES_PER_PAYLOAD_BYTE: usize = 4;

/// Char node type discriminants for serialization
pub mod char_node_types {
    pub const CHARNODE4: u8 = super::CHAR_NODE4_TAG;
    pub const CHARNODE16: u8 = super::CHAR_NODE16_TAG;
    pub const CHARNODE48: u8 = super::CHAR_NODE48_TAG;
    pub const CHARBUCKET: u8 = super::CHAR_BUCKET_TAG;
}

/// Serialized char node header (fixed 16 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SerializedCharNodeHeader {
    /// Magic bytes "ARC\0"
    pub magic: [u8; 4],
    /// Format version
    pub version: u8,
    /// Node type (104, 116, 148, 101)
    pub node_type: u8,
    /// Node flags (is_final, is_dirty, is_leaf)
    pub flags: u8,
    /// Reserved for future use
    pub reserved: u8,
    /// Number of children
    pub num_children: u16,
    /// Compressed prefix length (0-6 chars)
    pub prefix_len: u8,
    /// Padding for alignment
    pub _padding: u8,
    /// Size of the type-specific data following this header
    pub data_size: u32,
}

impl SerializedCharNodeHeader {
    /// Create a header from a CharNodeHeader
    pub fn from_node_header(header: &CharNodeHeader, data_size: u32) -> Self {
        Self {
            magic: CHAR_NODE_MAGIC,
            version: CHAR_FORMAT_VERSION_V2,
            node_type: header.node_type,
            flags: header.flags,
            reserved: 0,
            num_children: header.num_children,
            prefix_len: header.prefix_len,
            _padding: 0,
            data_size,
        }
    }

    /// Create an established V2 header with relative-location flags.
    ///
    /// The encoding_flags parameter contains:
    /// - Bit 7 (0x80): `FLAG_RELATIVE_OFFSETS` - children use relative offsets
    /// - Bit 6 (0x40): `FLAG_SEQUENTIAL_SIBLINGS` - children are contiguous
    ///
    /// These flags are combined with the three defined runtime node flags.
    pub fn from_node_header_v2(
        header: &CharNodeHeader,
        data_size: u32,
        encoding_flags: u8,
    ) -> Self {
        Self {
            magic: CHAR_NODE_MAGIC,
            version: CHAR_FORMAT_VERSION_V2,
            node_type: header.node_type,
            flags: (header.flags & NODE_FLAGS_MASK) | (encoding_flags & V2_ENCODING_FLAGS_MASK),
            reserved: 0,
            num_children: header.num_children,
            prefix_len: header.prefix_len,
            _padding: 0,
            data_size,
        }
    }

    /// Create a V3 relative header with its packed type extension.
    fn from_node_header_v3(
        header: &CharNodeHeader,
        data_size: u32,
        encoding_flags: u8,
        type_extension: u16,
    ) -> Self {
        Self {
            magic: CHAR_NODE_MAGIC,
            version: CHAR_FORMAT_VERSION_V3,
            node_type: header.node_type,
            flags: (header.flags & NODE_FLAGS_MASK) | (encoding_flags & V3_ENCODING_FLAGS_MASK),
            reserved: type_extension as u8,
            num_children: header.num_children,
            prefix_len: header.prefix_len,
            _padding: (type_extension >> 8) as u8,
            data_size,
        }
    }

    /// Check if relative offsets encoding is used
    ///
    /// When true, child pointers are stored as relative offsets from the parent slot,
    /// enabling more compact varint encoding for same-arena children.
    #[inline]
    pub fn uses_relative_offsets(&self) -> bool {
        self.flags & FLAG_RELATIVE_OFFSETS != 0
    }

    /// Check if sequential siblings encoding is used
    ///
    /// When true, children are stored contiguously and the node only stores
    /// (first_child_slot, count) instead of N separate pointers.
    #[inline]
    pub fn uses_sequential_siblings(&self) -> bool {
        self.flags & FLAG_SEQUENTIAL_SIBLINGS != 0
    }

    /// Check whether V3 uses its zero-payload homogeneous type codec.
    #[inline]
    fn uses_homogeneous_child_types(&self) -> bool {
        self.flags & FLAG_HOMOGENEOUS_CHILD_TYPES != 0
    }

    /// The two already-paid header bytes used by V3 for packed type codes.
    #[inline]
    fn child_type_extension(&self) -> u16 {
        u16::from(self.reserved) | (u16::from(self._padding) << 8)
    }

    /// Convert to a CharNodeHeader
    pub fn to_node_header(&self) -> CharNodeHeader {
        CharNodeHeader {
            node_type: self.node_type,
            prefix_len: self.prefix_len,
            flags: self.flags & NODE_FLAGS_MASK,
            _padding: 0,
            num_children: self.num_children,
            _padding2: [0; 2],
            version: 0, // Version is runtime-only
        }
    }

    /// Validate the header
    pub fn validate(&self) -> Result<()> {
        if self.magic != CHAR_NODE_MAGIC {
            return Err(PersistentARTrieError::InvalidMagic {
                expected: u64::from_le_bytes([
                    CHAR_NODE_MAGIC[0],
                    CHAR_NODE_MAGIC[1],
                    CHAR_NODE_MAGIC[2],
                    CHAR_NODE_MAGIC[3],
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
        if self.version > CHAR_FORMAT_VERSION {
            return Err(PersistentARTrieError::UnsupportedVersion {
                max_supported: CHAR_FORMAT_VERSION as u32,
                found: self.version as u32,
            });
        }
        match self.node_type {
            char_node_types::CHARNODE4
            | char_node_types::CHARNODE16
            | char_node_types::CHARNODE48
            | char_node_types::CHARBUCKET => {}
            _ => {
                return Err(PersistentARTrieError::corrupted(format!(
                    "invalid char node type: {}",
                    self.node_type
                )));
            }
        }
        if self.version <= CHAR_FORMAT_VERSION_V2 && (self.reserved != 0 || self._padding != 0) {
            return Err(PersistentARTrieError::corrupted(format!(
                "nonzero reserved char node header bytes: reserved={}, padding={}",
                self.reserved, self._padding
            )));
        }
        let encoding_flags = if self.version <= CHAR_FORMAT_VERSION_V2 {
            V2_ENCODING_FLAGS_MASK
        } else {
            V3_ENCODING_FLAGS_MASK
        };
        let unknown_flags = self.flags & !(NODE_FLAGS_MASK | encoding_flags);
        if unknown_flags != 0 {
            return Err(PersistentARTrieError::corrupted(format!(
                "unknown char node flags {unknown_flags:#04x}"
            )));
        }
        if self.uses_sequential_siblings() && !self.uses_relative_offsets() {
            return Err(PersistentARTrieError::corrupted(
                "char sequential-sibling flag requires relative-offset flag",
            ));
        }
        if self.uses_sequential_siblings() && self.num_children == 0 {
            return Err(PersistentARTrieError::corrupted(
                "char sequential-sibling layout requires at least one child",
            ));
        }
        if self.uses_homogeneous_child_types() && !self.uses_relative_offsets() {
            return Err(PersistentARTrieError::corrupted(
                "char V3 homogeneous child-type flag requires relative-offset flag",
            ));
        }
        if self.version >= CHAR_FORMAT_VERSION_V3 && !self.uses_relative_offsets() {
            return Err(PersistentARTrieError::corrupted(
                "char V3 requires relative-offset encoding",
            ));
        }
        if self.prefix_len as usize > CHAR_MAX_PREFIX_LEN {
            return Err(PersistentARTrieError::corrupted(format!(
                "prefix length {} exceeds maximum {}",
                self.prefix_len, CHAR_MAX_PREFIX_LEN
            )));
        }
        if let Some(max_children) = fixed_node_child_capacity(self.node_type) {
            if self.num_children as usize > max_children {
                return Err(PersistentARTrieError::corrupted(format!(
                    "char node type {} declares {} children, capacity is {}",
                    self.node_type, self.num_children, max_children
                )));
            }
        }
        Ok(())
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> [u8; CHAR_SERIALIZED_HEADER_SIZE] {
        let mut bytes = [0u8; CHAR_SERIALIZED_HEADER_SIZE];
        bytes[0..4].copy_from_slice(&self.magic);
        bytes[4] = self.version;
        bytes[5] = self.node_type;
        bytes[6] = self.flags;
        bytes[7] = self.reserved;
        bytes[8..10].copy_from_slice(&self.num_children.to_le_bytes());
        bytes[10] = self.prefix_len;
        bytes[11] = self._padding;
        bytes[12..16].copy_from_slice(&self.data_size.to_le_bytes());
        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8; CHAR_SERIALIZED_HEADER_SIZE]) -> Self {
        Self {
            magic: [bytes[0], bytes[1], bytes[2], bytes[3]],
            version: bytes[4],
            node_type: bytes[5],
            flags: bytes[6],
            reserved: bytes[7],
            num_children: u16::from_le_bytes([bytes[8], bytes[9]]),
            prefix_len: bytes[10],
            _padding: bytes[11],
            data_size: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        }
    }
}

fn fixed_node_child_capacity(node_type: u8) -> Option<usize> {
    match node_type {
        char_node_types::CHARNODE4 => Some(4),
        char_node_types::CHARNODE16 => Some(16),
        char_node_types::CHARNODE48 => Some(48),
        char_node_types::CHARBUCKET => None,
        _ => None,
    }
}

fn checked_layout_add(left: usize, right: usize, context: &str) -> Result<usize> {
    left.checked_add(right).ok_or_else(|| {
        PersistentARTrieError::corrupted(format!("char node layout size overflow: {context}"))
    })
}

fn ensure_fixed_node_data_size(
    header: &SerializedCharNodeHeader,
    key_bytes: usize,
    child_capacity: usize,
) -> Result<()> {
    let prefix_size = header_prefix_size(header);
    let child_bytes = child_capacity
        .checked_mul(8)
        .ok_or_else(|| PersistentARTrieError::corrupted("char fixed child layout size overflow"))?;
    let expected = checked_layout_add(prefix_size, key_bytes, "fixed keys")?;
    let expected = checked_layout_add(expected, child_bytes, "fixed children")?;
    let expected = checked_layout_add(expected, 8, "fixed value pointer")?;
    if header.data_size as usize != expected {
        return Err(PersistentARTrieError::corrupted(format!(
            "noncanonical char fixed node data_size: got {}, expected {}",
            header.data_size, expected
        )));
    }
    Ok(())
}

fn ensure_bucket_entry_count(header: &SerializedCharNodeHeader, num_entries: usize) -> Result<()> {
    if num_entries != header.num_children as usize {
        return Err(PersistentARTrieError::corrupted(format!(
            "char bucket header declares {} children but payload has {} entries",
            header.num_children, num_entries
        )));
    }
    Ok(())
}

fn ensure_bucket_fixed_data_size(
    header: &SerializedCharNodeHeader,
    num_entries: usize,
) -> Result<()> {
    let prefix_size = header_prefix_size(header);
    let entry_bytes = num_entries.checked_mul(12).ok_or_else(|| {
        PersistentARTrieError::corrupted("char bucket fixed entry layout size overflow")
    })?;
    let expected = checked_layout_add(prefix_size, 4, "bucket entry count")?;
    let expected = checked_layout_add(expected, 8, "bucket value pointer")?;
    let expected = checked_layout_add(expected, entry_bytes, "bucket entries")?;
    if header.data_size as usize != expected {
        return Err(PersistentARTrieError::corrupted(format!(
            "noncanonical char bucket fixed data_size: got {}, expected {}",
            header.data_size, expected
        )));
    }
    Ok(())
}

/// Calculate the serialized size of a char node
pub fn char_serialized_size(node: &CharNode) -> usize {
    CHAR_SERIALIZED_HEADER_SIZE + char_prefix_size(node) + char_node_data_size(node)
}

fn char_prefix_size(node: &CharNode) -> usize {
    if node.header().prefix_len > 0 {
        CHAR_MAX_PREFIX_LEN * 4 // 6 chars × 4 bytes = 24 bytes
    } else {
        0
    }
}

fn char_node_data_size(node: &CharNode) -> usize {
    match node {
        // CharNode4: 4 keys × 4 + 4 children × 8 + value_ptr × 8 = 56
        CharNode::N4(_) => 4 * 4 + 4 * 8 + 8,
        // CharNode16: 16 keys × 4 + 16 children × 8 + value_ptr × 8 = 200
        CharNode::N16(_) => 16 * 4 + 16 * 8 + 8,
        // CharNode48: 48 keys × 4 + 48 children × 8 + value_ptr × 8 = 584
        CharNode::N48(_) => 48 * 4 + 48 * 8 + 8,
        // CharBucket: num_entries × 4 + value_ptr × 8 + entries × (4 + 8) = 12 + 12n
        CharNode::Bucket(n) => 4 + 8 + n.entries.len() * 12,
    }
}

fn validate_char_node_representation(node: &CharNode) -> Result<()> {
    let expected = node.representation_type();
    let found = node.header().node_type;
    if found != expected as u8 {
        return Err(PersistentARTrieError::NodeTypeMismatch {
            expected: format!("{expected:?} ({})", expected as u8),
            found: format!("header tag {found}"),
        });
    }
    Ok(())
}

/// Serialize a CharNode to a writer
pub fn serialize_char_node<W: Write>(node: &CharNode, writer: &mut W) -> Result<usize> {
    validate_char_node_representation(node)?;
    let data_size = char_prefix_size(node) + char_node_data_size(node);
    let header = SerializedCharNodeHeader::from_node_header(node.header(), data_size as u32);

    // Write header
    writer.write_all(&header.to_bytes()).map_err(io_err)?;

    // Write prefix if present
    if node.header().prefix_len > 0 {
        let prefix = node.prefix();
        for &c in &prefix.chars {
            writer.write_all(&c.to_le_bytes()).map_err(io_err)?;
        }
    }

    // Write type-specific data
    match node {
        CharNode::N4(n) => serialize_charnode4(n, writer)?,
        CharNode::N16(n) => serialize_charnode16(n, writer)?,
        CharNode::N48(n) => serialize_charnode48(n, writer)?,
        CharNode::Bucket(n) => serialize_charbucket(n, writer)?,
    }

    Ok(CHAR_SERIALIZED_HEADER_SIZE + data_size)
}

fn serialize_charnode4<W: Write>(node: &CharNode4, writer: &mut W) -> Result<()> {
    // Write keys (4 × u32)
    for key in &node.keys {
        writer.write_all(&key.to_le_bytes()).map_err(io_err)?;
    }

    // Write children as u64
    for child in &node.children {
        let raw = child.to_raw();
        writer.write_all(&raw.to_le_bytes()).map_err(io_err)?;
    }

    // Write value_ptr
    let value_raw = node.value_ptr.to_raw();
    writer.write_all(&value_raw.to_le_bytes()).map_err(io_err)?;

    Ok(())
}

fn serialize_charnode16<W: Write>(node: &CharNode16, writer: &mut W) -> Result<()> {
    // Write keys (16 × u32)
    for key in &node.keys {
        writer.write_all(&key.to_le_bytes()).map_err(io_err)?;
    }

    // Write children as u64
    for child in &node.children {
        let raw = child.to_raw();
        writer.write_all(&raw.to_le_bytes()).map_err(io_err)?;
    }

    // Write value_ptr
    let value_raw = node.value_ptr.to_raw();
    writer.write_all(&value_raw.to_le_bytes()).map_err(io_err)?;

    Ok(())
}

fn serialize_charnode48<W: Write>(node: &CharNode48, writer: &mut W) -> Result<()> {
    // Write keys (48 × u32, sorted)
    for key in &node.keys {
        writer.write_all(&key.to_le_bytes()).map_err(io_err)?;
    }

    // Write children as u64
    for child in &node.children {
        let raw = child.to_raw();
        writer.write_all(&raw.to_le_bytes()).map_err(io_err)?;
    }

    // Write value_ptr
    let value_raw = node.value_ptr.to_raw();
    writer.write_all(&value_raw.to_le_bytes()).map_err(io_err)?;

    Ok(())
}

fn serialize_charbucket<W: Write>(node: &CharBucket, writer: &mut W) -> Result<()> {
    // Write number of entries
    let num_entries = node.entries.len() as u32;
    writer
        .write_all(&num_entries.to_le_bytes())
        .map_err(io_err)?;

    // Write value_ptr
    let value_raw = node.value_ptr.to_raw();
    writer.write_all(&value_raw.to_le_bytes()).map_err(io_err)?;

    // Write entries as (key: u32, child: u64) pairs
    // Sort entries for deterministic serialization
    let mut entries: Vec<_> = node.entries.iter().collect();
    entries.sort_by_key(|&(k, _)| *k);

    for (&key, child) in entries {
        writer.write_all(&key.to_le_bytes()).map_err(io_err)?;
        let child_raw = child.to_raw();
        writer.write_all(&child_raw.to_le_bytes()).map_err(io_err)?;
    }

    Ok(())
}

/// Deserialize a CharNode from a reader
pub fn deserialize_char_node<R: Read>(reader: &mut R) -> Result<CharNode> {
    // Read and validate header
    let mut header_bytes = [0u8; CHAR_SERIALIZED_HEADER_SIZE];
    reader.read_exact(&mut header_bytes).map_err(io_err)?;
    let header = SerializedCharNodeHeader::from_bytes(&header_bytes);
    header.validate()?;

    // Read prefix if present
    let prefix = if header.prefix_len > 0 {
        let mut chars = [0u32; CHAR_MAX_PREFIX_LEN];
        for c in &mut chars {
            let mut bytes = [0u8; 4];
            reader.read_exact(&mut bytes).map_err(io_err)?;
            *c = u32::from_le_bytes(bytes);
        }
        CharCompressedPrefix { chars }
    } else {
        CharCompressedPrefix::empty()
    };

    // Deserialize type-specific data
    match header.node_type {
        char_node_types::CHARNODE4 => deserialize_charnode4(reader, &header, prefix),
        char_node_types::CHARNODE16 => deserialize_charnode16(reader, &header, prefix),
        char_node_types::CHARNODE48 => deserialize_charnode48(reader, &header, prefix),
        char_node_types::CHARBUCKET => deserialize_charbucket(reader, &header, prefix),
        _ => Err(PersistentARTrieError::corrupted(format!(
            "invalid char node type: {}",
            header.node_type
        ))),
    }
}

fn deserialize_charnode4<R: Read>(
    reader: &mut R,
    header: &SerializedCharNodeHeader,
    prefix: CharCompressedPrefix,
) -> Result<CharNode> {
    let mut node = CharNode4::new();
    node.header = header.to_node_header();
    node.prefix = prefix;

    // Read keys
    for key in &mut node.keys {
        let mut bytes = [0u8; 4];
        reader.read_exact(&mut bytes).map_err(io_err)?;
        *key = u32::from_le_bytes(bytes);
    }

    // Read children
    for child in &mut node.children {
        let mut raw_bytes = [0u8; 8];
        reader.read_exact(&mut raw_bytes).map_err(io_err)?;
        *child = SwizzledPtr::from_raw(u64::from_le_bytes(raw_bytes));
    }

    // Read value_ptr
    let mut value_bytes = [0u8; 8];
    reader.read_exact(&mut value_bytes).map_err(io_err)?;
    node.value_ptr = SwizzledPtr::from_raw(u64::from_le_bytes(value_bytes));

    Ok(CharNode::N4(Box::new(node)))
}

fn deserialize_charnode16<R: Read>(
    reader: &mut R,
    header: &SerializedCharNodeHeader,
    prefix: CharCompressedPrefix,
) -> Result<CharNode> {
    let mut node = CharNode16::new();
    node.header = header.to_node_header();
    node.prefix = prefix;

    // Read keys
    for key in &mut node.keys {
        let mut bytes = [0u8; 4];
        reader.read_exact(&mut bytes).map_err(io_err)?;
        *key = u32::from_le_bytes(bytes);
    }

    // Read children
    for child in &mut node.children {
        let mut raw_bytes = [0u8; 8];
        reader.read_exact(&mut raw_bytes).map_err(io_err)?;
        *child = SwizzledPtr::from_raw(u64::from_le_bytes(raw_bytes));
    }

    // Read value_ptr
    let mut value_bytes = [0u8; 8];
    reader.read_exact(&mut value_bytes).map_err(io_err)?;
    node.value_ptr = SwizzledPtr::from_raw(u64::from_le_bytes(value_bytes));

    Ok(CharNode::N16(Box::new(node)))
}

fn deserialize_charnode48<R: Read>(
    reader: &mut R,
    header: &SerializedCharNodeHeader,
    prefix: CharCompressedPrefix,
) -> Result<CharNode> {
    let mut node = CharNode48::new();
    node.header = header.to_node_header();
    node.prefix = prefix;

    // Read keys
    for key in &mut node.keys {
        let mut bytes = [0u8; 4];
        reader.read_exact(&mut bytes).map_err(io_err)?;
        *key = u32::from_le_bytes(bytes);
    }

    // Read children
    for child in &mut node.children {
        let mut raw_bytes = [0u8; 8];
        reader.read_exact(&mut raw_bytes).map_err(io_err)?;
        *child = SwizzledPtr::from_raw(u64::from_le_bytes(raw_bytes));
    }

    // Read value_ptr
    let mut value_bytes = [0u8; 8];
    reader.read_exact(&mut value_bytes).map_err(io_err)?;
    node.value_ptr = SwizzledPtr::from_raw(u64::from_le_bytes(value_bytes));

    Ok(CharNode::N48(Box::new(node)))
}

fn deserialize_charbucket<R: Read>(
    reader: &mut R,
    header: &SerializedCharNodeHeader,
    prefix: CharCompressedPrefix,
) -> Result<CharNode> {
    let mut node = CharBucket::new();
    node.header = header.to_node_header();
    node.prefix = prefix;

    // Read number of entries
    let mut num_entries_bytes = [0u8; 4];
    reader.read_exact(&mut num_entries_bytes).map_err(io_err)?;
    let num_entries = u32::from_le_bytes(num_entries_bytes) as usize;

    // Read value_ptr
    let mut value_bytes = [0u8; 8];
    reader.read_exact(&mut value_bytes).map_err(io_err)?;
    node.value_ptr = SwizzledPtr::from_raw(u64::from_le_bytes(value_bytes));

    // Read entries
    for _ in 0..num_entries {
        let mut key_bytes = [0u8; 4];
        reader.read_exact(&mut key_bytes).map_err(io_err)?;
        let key = u32::from_le_bytes(key_bytes);

        let mut child_bytes = [0u8; 8];
        reader.read_exact(&mut child_bytes).map_err(io_err)?;
        let child = SwizzledPtr::from_raw(u64::from_le_bytes(child_bytes));

        node.entries.insert(key, child);
    }

    Ok(CharNode::Bucket(Box::new(node)))
}

/// Serialize a CharNode to a byte vector
pub fn char_to_bytes(node: &CharNode) -> Result<Vec<u8>> {
    let mut buffer = Vec::with_capacity(char_serialized_size(node));
    serialize_char_node(node, &mut buffer)?;
    Ok(buffer)
}

/// Deserialize a CharNode from a byte slice
pub fn char_from_bytes(bytes: &[u8]) -> Result<CharNode> {
    let mut reader = std::io::Cursor::new(bytes);
    deserialize_char_node(&mut reader)
}

// =============================================================================
// Compact Encoding Support (Variable-Width)
// =============================================================================

/// Serialize a CharNode using compact variable-width encoding
///
/// This achieves 70-90% space reduction compared to fixed-width encoding
/// by using variable-width integers for keys and pointers based on actual values.
///
/// # Arguments
/// * `node` - The CharNode to serialize
/// * `max_ptr_value` - The maximum pointer value in the trie (used to determine ptr_width)
///
/// # Returns
/// A vector of bytes containing the compact-encoded node
pub fn char_to_bytes_compact(node: &CharNode, max_ptr_value: u64) -> Vec<u8> {
    // Extract data from node
    let (keys, children, prefix_chars, value_ptr, node_type, flags) = extract_node_data(node);

    // Determine optimal widths
    let max_key = keys
        .iter()
        .chain(prefix_chars.iter())
        .copied()
        .max()
        .unwrap_or(0);
    let key_width = determine_key_width(max_key);
    let ptr_width = determine_ptr_width(max_ptr_value);

    // Build header
    let header = CompactHeader {
        key_width,
        ptr_width,
        num_children: children.len() as u8,
        has_value: value_ptr.is_some(),
        prefix_len: prefix_chars.len() as u8,
        node_type,
        flags,
    };

    // Encode
    encode_compact_node(&header, &prefix_chars, &keys, &children, value_ptr)
}

/// Deserialize a CharNode from compact variable-width encoding
///
/// # Arguments
/// * `bytes` - The compact-encoded byte slice
///
/// # Returns
/// The deserialized CharNode
pub fn char_from_bytes_compact(bytes: &[u8]) -> Result<CharNode> {
    let decoded = decode_compact_node(bytes);
    reconstruct_node_from_decoded(decoded)
}

/// Calculate the compact serialized size of a CharNode
///
/// This estimates the size without actually serializing, useful for
/// pre-allocating buffers or checking if a node fits in an arena slot.
pub fn char_compact_serialized_size(node: &CharNode, max_ptr_value: u64) -> usize {
    let (keys, children, prefix_chars, value_ptr, _node_type, _flags) = extract_node_data(node);

    let max_key = keys
        .iter()
        .chain(prefix_chars.iter())
        .copied()
        .max()
        .unwrap_or(0);
    let key_width = determine_key_width(max_key) as usize;
    let ptr_width = determine_ptr_width(max_ptr_value) as usize;

    // Header: 3 bytes (COMPACT_HEADER_SIZE) + optional extended num_children byte
    // Prefix: prefix_len * key_width
    // Keys: num_children * key_width
    // Children: num_children * ptr_width
    // Value: ptr_width if has_value
    use super::compact_encoding::COMPACT_HEADER_SIZE;
    let num_children = children.len();
    COMPACT_HEADER_SIZE
        + if num_children > 15 { 1 } else { 0 }  // extended num_children byte
        + (prefix_chars.len() * key_width)
        + (num_children * key_width)
        + (num_children * ptr_width)
        + if value_ptr.is_some() { ptr_width } else { 0 }
}

/// Extract data from a CharNode into arrays suitable for compact encoding
fn extract_node_data(node: &CharNode) -> (Vec<u32>, Vec<u64>, Vec<u32>, Option<u64>, u8, u8) {
    match node {
        CharNode::N4(n) => {
            let num_children = n.header.num_children as usize;
            let keys: Vec<u32> = n.keys[..num_children].to_vec();
            let children: Vec<u64> = n.children[..num_children]
                .iter()
                .map(|p| p.to_raw())
                .collect();
            let prefix_chars: Vec<u32> = n.prefix.chars[..n.header.prefix_len as usize].to_vec();
            let value_ptr = if n.value_ptr.is_null() {
                None
            } else {
                Some(n.value_ptr.to_raw())
            };
            (
                keys,
                children,
                prefix_chars,
                value_ptr,
                COMPACT_NODE_TYPE_N4,
                n.header.flags,
            )
        }
        CharNode::N16(n) => {
            let num_children = n.header.num_children as usize;
            let keys: Vec<u32> = n.keys[..num_children].to_vec();
            let children: Vec<u64> = n.children[..num_children]
                .iter()
                .map(|p| p.to_raw())
                .collect();
            let prefix_chars: Vec<u32> = n.prefix.chars[..n.header.prefix_len as usize].to_vec();
            let value_ptr = if n.value_ptr.is_null() {
                None
            } else {
                Some(n.value_ptr.to_raw())
            };
            (
                keys,
                children,
                prefix_chars,
                value_ptr,
                COMPACT_NODE_TYPE_N16,
                n.header.flags,
            )
        }
        CharNode::N48(n) => {
            let num_children = n.header.num_children as usize;
            let keys: Vec<u32> = n.keys[..num_children].to_vec();
            let children: Vec<u64> = n.children[..num_children]
                .iter()
                .map(|p| p.to_raw())
                .collect();
            let prefix_chars: Vec<u32> = n.prefix.chars[..n.header.prefix_len as usize].to_vec();
            let value_ptr = if n.value_ptr.is_null() {
                None
            } else {
                Some(n.value_ptr.to_raw())
            };
            (
                keys,
                children,
                prefix_chars,
                value_ptr,
                COMPACT_NODE_TYPE_N48,
                n.header.flags,
            )
        }
        CharNode::Bucket(n) => {
            // Bucket uses HashMap, collect entries sorted by key
            let mut entries: Vec<_> = n.entries.iter().collect();
            entries.sort_by_key(|&(k, _)| *k);
            let keys: Vec<u32> = entries.iter().map(|(&k, _)| k).collect();
            let children: Vec<u64> = entries.iter().map(|(_, p)| p.to_raw()).collect();
            let prefix_chars: Vec<u32> = n.prefix.chars[..n.header.prefix_len as usize].to_vec();
            let value_ptr = if n.value_ptr.is_null() {
                None
            } else {
                Some(n.value_ptr.to_raw())
            };
            (
                keys,
                children,
                prefix_chars,
                value_ptr,
                COMPACT_NODE_TYPE_BUCKET,
                n.header.flags,
            )
        }
    }
}

/// Reconstruct a CharNode from decoded compact data
fn reconstruct_node_from_decoded(decoded: DecodedCompactNode) -> Result<CharNode> {
    let prefix = CharCompressedPrefix::from_chars(&decoded.prefix);

    match decoded.header.node_type {
        COMPACT_NODE_TYPE_N4 => {
            let mut node = CharNode4::new();
            node.header.prefix_len = decoded.header.prefix_len;
            node.header.flags = decoded.header.flags;
            node.header.num_children = decoded.header.num_children as u16;
            node.prefix = prefix;

            // Copy keys and children
            for (i, &key) in decoded.keys.iter().enumerate() {
                if i < 4 {
                    node.keys[i] = key;
                    node.children[i] = SwizzledPtr::from_raw(decoded.children[i]);
                }
            }

            // Set value_ptr
            if let Some(v) = decoded.value_ptr {
                node.value_ptr = SwizzledPtr::from_raw(v);
            }

            Ok(CharNode::N4(Box::new(node)))
        }
        COMPACT_NODE_TYPE_N16 => {
            let mut node = CharNode16::new();
            node.header.prefix_len = decoded.header.prefix_len;
            node.header.flags = decoded.header.flags;
            node.header.num_children = decoded.header.num_children as u16;
            node.prefix = prefix;

            // Copy keys and children
            for (i, &key) in decoded.keys.iter().enumerate() {
                if i < 16 {
                    node.keys[i] = key;
                    node.children[i] = SwizzledPtr::from_raw(decoded.children[i]);
                }
            }

            // Set value_ptr
            if let Some(v) = decoded.value_ptr {
                node.value_ptr = SwizzledPtr::from_raw(v);
            }

            Ok(CharNode::N16(Box::new(node)))
        }
        COMPACT_NODE_TYPE_N48 => {
            let mut node = CharNode48::new();
            node.header.prefix_len = decoded.header.prefix_len;
            node.header.flags = decoded.header.flags;
            node.header.num_children = decoded.header.num_children as u16;
            node.prefix = prefix;

            // Copy keys and children
            for (i, &key) in decoded.keys.iter().enumerate() {
                if i < 48 {
                    node.keys[i] = key;
                    node.children[i] = SwizzledPtr::from_raw(decoded.children[i]);
                }
            }

            // Set value_ptr
            if let Some(v) = decoded.value_ptr {
                node.value_ptr = SwizzledPtr::from_raw(v);
            }

            Ok(CharNode::N48(Box::new(node)))
        }
        COMPACT_NODE_TYPE_BUCKET => {
            let mut node = CharBucket::new();
            node.header.prefix_len = decoded.header.prefix_len;
            node.header.flags = decoded.header.flags;
            node.header.num_children = decoded.header.num_children as u16;
            node.prefix = prefix;

            // Insert all entries into the bucket's HashMap
            for (i, &key) in decoded.keys.iter().enumerate() {
                node.entries
                    .insert(key, SwizzledPtr::from_raw(decoded.children[i]));
            }

            // Set value_ptr
            if let Some(v) = decoded.value_ptr {
                node.value_ptr = SwizzledPtr::from_raw(v);
            }

            Ok(CharNode::Bucket(Box::new(node)))
        }
        _ => Err(PersistentARTrieError::corrupted(format!(
            "invalid compact node type: {}",
            decoded.header.node_type
        ))),
    }
}

// =============================================================================
// V2 Serialization with Relative Offsets and Sequential Siblings
// =============================================================================

/// Collect ArenaSlots from a CharNode's children
///
/// Extracts the ArenaSlot for each non-null child pointer.
/// For use with relative offset encoding during serialization.
///
/// # Arguments
/// * `node` - The CharNode to extract children from
///
/// # Returns
/// Vector of ArenaSlots for all non-null children (sorted by key for determinism)
pub fn collect_char_child_slots(node: &CharNode) -> Vec<ArenaSlot> {
    let mut slots = Vec::with_capacity(node.header().num_children as usize);

    match node {
        CharNode::N4(n) => {
            for i in 0..n.header.num_children as usize {
                if !n.children[i].is_null() {
                    if let Some(slot) = ptr_to_arena_slot(&n.children[i]) {
                        slots.push(slot);
                    }
                }
            }
        }
        CharNode::N16(n) => {
            for i in 0..n.header.num_children as usize {
                if !n.children[i].is_null() {
                    if let Some(slot) = ptr_to_arena_slot(&n.children[i]) {
                        slots.push(slot);
                    }
                }
            }
        }
        CharNode::N48(n) => {
            for i in 0..n.header.num_children as usize {
                if !n.children[i].is_null() {
                    if let Some(slot) = ptr_to_arena_slot(&n.children[i]) {
                        slots.push(slot);
                    }
                }
            }
        }
        CharNode::Bucket(n) => {
            // Sort by key for deterministic serialization
            let mut entries = Vec::with_capacity(n.entries.len());
            entries.extend(n.entries.iter());
            entries.sort_by_key(|&(k, _)| *k);
            for (_, child) in entries {
                if !child.is_null() {
                    if let Some(slot) = ptr_to_arena_slot(child) {
                        slots.push(slot);
                    }
                }
            }
        }
    }

    slots
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CharChildReference {
    key: u32,
    slot: ArenaSlot,
    node_type: NodeType,
}

#[inline]
fn char_node_type_code(node_type: NodeType) -> Result<u8> {
    match node_type {
        NodeType::CharNode4 => Ok(0),
        NodeType::CharNode16 => Ok(1),
        NodeType::CharNode48 => Ok(2),
        NodeType::CharBucket => Ok(3),
        _ => Err(PersistentARTrieError::corrupted(format!(
            "non-char node type in V3 child reference: {node_type:?}"
        ))),
    }
}

#[inline]
fn char_node_type_from_code(code: u8) -> Result<NodeType> {
    match code {
        0 => Ok(NodeType::CharNode4),
        1 => Ok(NodeType::CharNode16),
        2 => Ok(NodeType::CharNode48),
        3 => Ok(NodeType::CharBucket),
        _ => Err(PersistentARTrieError::corrupted(format!(
            "invalid two-bit char node-type code: {code}"
        ))),
    }
}

#[inline]
fn uses_relative_location(parent: ArenaSlot, child: ArenaSlot) -> bool {
    parent.arena_id == child.arena_id && child.slot_id <= parent.slot_id
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CharV3EncodingPlan {
    encoded_locations_size: usize,
    unresolved_types: usize,
    type_extension: u16,
    type_payload_bytes: usize,
    homogeneous_type: Option<u8>,
}

impl CharV3EncodingPlan {
    #[inline]
    fn encoding_flags(self, ctx: &SerializationContext) -> u8 {
        ctx.encoding_flags()
            | if self.homogeneous_type.is_some() {
                FLAG_HOMOGENEOUS_CHILD_TYPES
            } else {
                0
            }
    }

    #[inline]
    fn encoded_children_size(self) -> Result<usize> {
        checked_layout_add(
            self.encoded_locations_size,
            self.type_payload_bytes,
            "V3 child locations and packed types",
        )
    }
}

#[inline]
fn child_type_is_unresolved(
    ctx: &SerializationContext,
    children: &[CharChildReference],
    index: usize,
) -> bool {
    if ctx.use_sequential {
        index != 0
            || ctx
                .first_child_slot
                .is_some_and(|first| uses_relative_location(ctx.parent_slot, first))
    } else {
        uses_relative_location(ctx.parent_slot, children[index].slot)
    }
}

#[inline]
fn accumulate_char_v3_type(
    code: u8,
    unresolved_types: &mut usize,
    packed_extension: &mut u16,
    first_code: &mut u8,
    variation: &mut u8,
) -> Result<()> {
    if *unresolved_types == 0 {
        *first_code = code;
    } else {
        *variation |= code ^ *first_code;
    }
    if *unresolved_types < CHILD_TYPES_PER_HEADER {
        *packed_extension |= u16::from(code) << (2 * *unresolved_types);
    }
    *unresolved_types = unresolved_types.checked_add(1).ok_or_else(|| {
        PersistentARTrieError::corrupted("V3 unresolved child-type count overflows usize")
    })?;
    Ok(())
}

fn build_char_v3_encoding_plan(
    ctx: &SerializationContext,
    children: &[CharChildReference],
) -> Result<CharV3EncodingPlan> {
    let mut encoded_locations_size = 0usize;
    let mut unresolved_types = 0usize;
    let mut packed_extension = 0u16;
    let mut first_code = 0u8;
    let mut variation = 0u8;

    if ctx.use_sequential {
        encoded_locations_size = ctx
            .first_child_slot
            .map(|first_child| encoded_size(ctx.parent_slot, first_child))
            .unwrap_or(0);
        let first_is_relative = ctx
            .first_child_slot
            .is_some_and(|first| uses_relative_location(ctx.parent_slot, first));
        for (index, child) in children.iter().enumerate() {
            let code = char_node_type_code(child.node_type)?;
            if index != 0 || first_is_relative {
                accumulate_char_v3_type(
                    code,
                    &mut unresolved_types,
                    &mut packed_extension,
                    &mut first_code,
                    &mut variation,
                )?;
            }
        }
    } else {
        for child in children {
            encoded_locations_size = encoded_locations_size
                .checked_add(encoded_size(ctx.parent_slot, child.slot))
                .ok_or_else(|| {
                    PersistentARTrieError::corrupted(
                        "char relative child encoding size overflows usize",
                    )
                })?;
            let code = char_node_type_code(child.node_type)?;
            if uses_relative_location(ctx.parent_slot, child.slot) {
                accumulate_char_v3_type(
                    code,
                    &mut unresolved_types,
                    &mut packed_extension,
                    &mut first_code,
                    &mut variation,
                )?;
            }
        }
    }

    let homogeneous_type =
        (unresolved_types > CHILD_TYPES_PER_HEADER && variation == 0).then_some(first_code);
    let type_extension = homogeneous_type.map_or(packed_extension, u16::from);
    let type_payload_bytes = if homogeneous_type.is_some() {
        0
    } else {
        unresolved_types
            .saturating_sub(CHILD_TYPES_PER_HEADER)
            .checked_add(CHILD_TYPES_PER_PAYLOAD_BYTE - 1)
            .ok_or_else(|| {
                PersistentARTrieError::corrupted("V3 child-type payload size overflows usize")
            })?
            / CHILD_TYPES_PER_PAYLOAD_BYTE
    };

    Ok(CharV3EncodingPlan {
        encoded_locations_size,
        unresolved_types,
        type_extension,
        type_payload_bytes,
        homogeneous_type,
    })
}

fn char_child_reference(key: u32, pointer: &SwizzledPtr) -> Option<CharChildReference> {
    let location = pointer.disk_location()?;
    let arena_id = location.block_id.checked_sub(1)?;
    Some(CharChildReference {
        key,
        slot: ArenaSlot::new(arena_id, location.offset),
        node_type: location.node_type,
    })
}

/// Collect the address and exact type of every active child in key order.
///
/// The single vector replaces the address-only vector previously allocated by
/// the relative writer; adding the type sideband therefore needs no additional
/// container allocation.
fn collect_char_child_references(node: &CharNode) -> Result<Vec<CharChildReference>> {
    let mut children = Vec::new();
    children
        .try_reserve_exact(node.header().num_children as usize)
        .map_err(|error| {
            PersistentARTrieError::allocation_failed(
                "char V3 child-reference plan",
                node.header().num_children as usize,
                error,
            )
        })?;
    match node {
        CharNode::N4(n) => {
            for index in 0..n.header.num_children as usize {
                if let Some(child) = char_child_reference(n.keys[index], &n.children[index]) {
                    children.push(child);
                }
            }
        }
        CharNode::N16(n) => {
            for index in 0..n.header.num_children as usize {
                if let Some(child) = char_child_reference(n.keys[index], &n.children[index]) {
                    children.push(child);
                }
            }
        }
        CharNode::N48(n) => {
            for index in 0..n.header.num_children as usize {
                if let Some(child) = char_child_reference(n.keys[index], &n.children[index]) {
                    children.push(child);
                }
            }
        }
        CharNode::Bucket(n) => {
            children.extend(
                n.entries
                    .iter()
                    .filter_map(|(&key, pointer)| char_child_reference(key, pointer)),
            );
            children.sort_unstable_by_key(|child| child.key);
        }
    }
    Ok(children)
}

/// Convert a SwizzledPtr to a char ArenaSlot
///
/// The SwizzledPtr uses the byte version's ArenaSlot internally,
/// so we extract the fields and create a char ArenaSlot.
fn ptr_to_arena_slot(ptr: &SwizzledPtr) -> Option<ArenaSlot> {
    // Get disk location from SwizzledPtr
    let loc = ptr.disk_location()?;
    // Arena N is stored in Block N+1 (block 0 is file header)
    let arena_id = loc.block_id.checked_sub(1)?;
    Some(ArenaSlot::new(arena_id, loc.offset))
}

/// Calculate V3 serialized data size with encoded locations and packed types.
///
/// # Arguments
/// * `node` - The CharNode to calculate size for
/// * `ctx` - The serialization context (determines encoding mode)
///
/// # Returns
/// Size in bytes of the type-specific data (excluding header and prefix)
fn char_node_data_size_v3(node: &CharNode, plan: CharV3EncodingPlan) -> Result<usize> {
    let fixed_size = match node {
        CharNode::N4(_) => 4 * 4 + 8,
        CharNode::N16(_) => 16 * 4 + 8,
        CharNode::N48(_) => 48 * 4 + 8,
        CharNode::Bucket(n) => n
            .entries
            .len()
            .checked_mul(4)
            .and_then(|keys| keys.checked_add(4 + 8))
            .ok_or_else(|| {
                PersistentARTrieError::corrupted("char bucket V3 layout size overflows usize")
            })?,
    };
    checked_layout_add(
        fixed_size,
        plan.encoded_children_size()?,
        "V3 child locations and packed types",
    )
}

#[inline]
fn write_char_v3_child_pointer<W: Write>(
    parent: ArenaSlot,
    child: CharChildReference,
    writer: &mut W,
) -> Result<usize> {
    if uses_relative_location(parent, child.slot) {
        let delta = parent
            .slot_id
            .checked_sub(child.slot.slot_id)
            .expect("relative-location classification establishes subtraction");
        return write_varint(u64::from(delta) << 1, writer).map_err(io_err);
    }

    let code = char_node_type_code(child.node_type)?;
    let mut encoded = [0u8; CROSS_ARENA_SIZE];
    encoded[0] = 1 + 2 * code;
    encoded[1..5].copy_from_slice(&child.slot.arena_id.to_le_bytes());
    encoded[5..9].copy_from_slice(&child.slot.slot_id.to_le_bytes());
    writer.write_all(&encoded).map_err(io_err)?;
    Ok(CROSS_ARENA_SIZE)
}

fn write_char_v3_type_payload<W: Write>(
    ctx: &SerializationContext,
    children: &[CharChildReference],
    plan: CharV3EncodingPlan,
    writer: &mut W,
) -> Result<()> {
    if plan.type_payload_bytes == 0 {
        return Ok(());
    }

    let mut unresolved_index = 0usize;
    let mut payload_byte = 0u8;
    let mut payload_bytes = 0usize;
    for (index, child) in children.iter().enumerate() {
        if !child_type_is_unresolved(ctx, children, index) {
            continue;
        }
        let code = char_node_type_code(child.node_type)?;
        if unresolved_index >= CHILD_TYPES_PER_HEADER {
            let payload_index = unresolved_index - CHILD_TYPES_PER_HEADER;
            let shift = 2 * (payload_index % CHILD_TYPES_PER_PAYLOAD_BYTE);
            payload_byte |= code << shift;
            if shift == 2 * (CHILD_TYPES_PER_PAYLOAD_BYTE - 1) {
                writer.write_all(&[payload_byte]).map_err(io_err)?;
                payload_bytes += 1;
                payload_byte = 0;
            }
        }
        unresolved_index += 1;
    }
    if unresolved_index > CHILD_TYPES_PER_HEADER
        && !(unresolved_index - CHILD_TYPES_PER_HEADER).is_multiple_of(CHILD_TYPES_PER_PAYLOAD_BYTE)
    {
        writer.write_all(&[payload_byte]).map_err(io_err)?;
        payload_bytes += 1;
    }
    if unresolved_index != plan.unresolved_types {
        return Err(PersistentARTrieError::internal(
            "V3 child-type plan changed between sizing and encoding",
        ));
    }
    if payload_bytes != plan.type_payload_bytes {
        return Err(PersistentARTrieError::internal(
            "V3 child-type payload changed between sizing and encoding",
        ));
    }
    Ok(())
}

fn write_char_v3_children<W: Write>(
    ctx: &SerializationContext,
    children: &[CharChildReference],
    plan: CharV3EncodingPlan,
    writer: &mut W,
) -> Result<()> {
    let locations_written = if ctx.use_sequential {
        let Some(first_child) = ctx.first_child_slot else {
            return Err(PersistentARTrieError::corrupted(
                "char v2 sequential serialization missing first child slot",
            ));
        };
        let first = *children.first().ok_or_else(|| {
            PersistentARTrieError::corrupted(
                "char V3 sequential serialization has no first child reference",
            )
        })?;
        if first.slot != first_child {
            return Err(PersistentARTrieError::internal(
                "char V3 sequential first child changed after validation",
            ));
        }
        write_char_v3_child_pointer(ctx.parent_slot, first, writer)?
    } else {
        let mut written = 0usize;
        for &child in children {
            written = written
                .checked_add(write_char_v3_child_pointer(ctx.parent_slot, child, writer)?)
                .ok_or_else(|| {
                    PersistentARTrieError::internal("V3 child-location byte-count overflow")
                })?;
        }
        written
    };
    if locations_written != plan.encoded_locations_size {
        return Err(PersistentARTrieError::internal(
            "V3 child locations changed between sizing and encoding",
        ));
    }
    write_char_v3_type_payload(ctx, children, plan, writer)
}

#[inline]
fn append_char_v3_child_pointer(
    parent: ArenaSlot,
    child: CharChildReference,
    output: &mut Vec<u8>,
) -> Result<usize> {
    if uses_relative_location(parent, child.slot) {
        return Ok(encode_child_pointer(parent, child.slot, output));
    }
    let code = char_node_type_code(child.node_type)?;
    output.push(1 + 2 * code);
    output.extend_from_slice(&child.slot.arena_id.to_le_bytes());
    output.extend_from_slice(&child.slot.slot_id.to_le_bytes());
    Ok(CROSS_ARENA_SIZE)
}

fn append_char_v3_type_payload(
    ctx: &SerializationContext,
    children: &[CharChildReference],
    plan: CharV3EncodingPlan,
    output: &mut Vec<u8>,
) -> Result<()> {
    if plan.type_payload_bytes == 0 {
        return Ok(());
    }
    let mut unresolved_index = 0usize;
    for (index, child) in children.iter().enumerate() {
        if !child_type_is_unresolved(ctx, children, index) {
            continue;
        }
        let code = char_node_type_code(child.node_type)?;
        if unresolved_index >= CHILD_TYPES_PER_HEADER {
            let payload_index = unresolved_index - CHILD_TYPES_PER_HEADER;
            let shift = 2 * (payload_index % CHILD_TYPES_PER_PAYLOAD_BYTE);
            if shift == 0 {
                output.push(code);
            } else {
                *output.last_mut().ok_or_else(|| {
                    PersistentARTrieError::internal("V3 packed child-type byte was not initialized")
                })? |= code << shift;
            }
        }
        unresolved_index += 1;
    }
    if unresolved_index != plan.unresolved_types {
        return Err(PersistentARTrieError::internal(
            "V3 child-type plan changed between sizing and encoding",
        ));
    }
    Ok(())
}

fn validate_v2_serialization_context(
    node: &CharNode,
    ctx: &SerializationContext,
    children: &[CharChildReference],
) -> Result<()> {
    let declared_children = node.header().num_children as usize;
    if children.len() != declared_children {
        return Err(PersistentARTrieError::corrupted(format!(
            "char v2 serialization saw {} disk children but header declares {}",
            children.len(),
            declared_children
        )));
    }
    if let CharNode::Bucket(bucket) = node {
        if bucket.entries.len() != declared_children {
            return Err(PersistentARTrieError::corrupted(format!(
                "char v2 bucket header declares {} children but entries contain {}",
                declared_children,
                bucket.entries.len()
            )));
        }
    }
    if ctx.use_sequential {
        if !ctx.use_relative {
            return Err(PersistentARTrieError::corrupted(
                "char v2 sequential serialization requires relative encoding",
            ));
        }
        if declared_children == 0 {
            return Err(PersistentARTrieError::corrupted(
                "char v2 sequential serialization requires at least one child",
            ));
        }
        let first_child = ctx.first_child_slot.ok_or_else(|| {
            PersistentARTrieError::corrupted(
                "char v2 sequential serialization missing first child slot",
            )
        })?;
        for (idx, child) in children.iter().enumerate() {
            let offset = u32::try_from(idx).map_err(|_| {
                PersistentARTrieError::corrupted(
                    "char v2 sequential child index exceeds u32 slot range",
                )
            })?;
            let expected_slot = first_child.slot_id.checked_add(offset).ok_or_else(|| {
                PersistentARTrieError::corrupted(
                    "char v2 sequential child range overflows u32 slot range",
                )
            })?;
            if child.slot.arena_id != first_child.arena_id || child.slot.slot_id != expected_slot {
                return Err(PersistentARTrieError::corrupted(format!(
                    "char v2 sequential child mismatch at index {}: got {:?}, expected arena {} slot {}",
                    idx, child.slot, first_child.arena_id, expected_slot
                )));
            }
        }
    }
    Ok(())
}

/// Serialize a char node using established V2 fixed width or current V3
/// relative/sequential encoding.
///
/// This format uses compact encoding for child pointers:
/// - Relative offsets for same-arena children (typically 1-2 bytes vs 8 bytes)
/// - Sequential sibling storage when children are consecutive (1 reference vs N)
///
/// # Arguments
/// * `node` - The CharNode to serialize
/// * `writer` - Output writer
/// * `ctx` - Serialization context with parent slot and encoding mode
///
/// # Returns
/// Number of bytes written
#[inline]
pub(crate) fn serialize_validated_char_node_v2<W: Write>(
    node: &ValidatedBorrowedCharNode<'_>,
    writer: &mut W,
    ctx: &SerializationContext,
) -> Result<usize> {
    serialize_char_node_v2(node.as_node(), writer, ctx)
}

pub fn serialize_char_node_v2<W: Write>(
    node: &CharNode,
    writer: &mut W,
    ctx: &SerializationContext,
) -> Result<usize> {
    if !ctx.use_relative && !ctx.use_sequential {
        return serialize_char_node(node, writer);
    }

    validate_char_node_representation(node)?;

    let children = collect_char_child_references(node)?;
    validate_v2_serialization_context(node, ctx, &children)?;
    let plan = build_char_v3_encoding_plan(ctx, &children)?;

    let data_size = checked_layout_add(
        char_prefix_size(node),
        char_node_data_size_v3(node, plan)?,
        "char V3 structural payload",
    )?;
    let data_size_u32 = u32::try_from(data_size)
        .map_err(|_| PersistentARTrieError::corrupted("char V3 structural payload exceeds u32"))?;
    let header = SerializedCharNodeHeader::from_node_header_v3(
        node.header(),
        data_size_u32,
        plan.encoding_flags(ctx),
        plan.type_extension,
    );

    // Write header
    writer.write_all(&header.to_bytes()).map_err(io_err)?;

    // Write prefix if present
    if node.header().prefix_len > 0 {
        let prefix = node.prefix();
        for &c in &prefix.chars {
            writer.write_all(&c.to_le_bytes()).map_err(io_err)?;
        }
    }

    if let CharNode::N16(node) = node {
        let children_capacity = plan.encoded_children_size()?;
        let mut encoded_children = Vec::new();
        encoded_children
            .try_reserve_exact(children_capacity)
            .map_err(|error| {
                PersistentARTrieError::allocation_failed(
                    "char V3 child location and type buffer",
                    children_capacity,
                    error,
                )
            })?;
        if ctx.use_sequential {
            let first = *children.first().ok_or_else(|| {
                PersistentARTrieError::corrupted(
                    "char V3 sequential serialization has no first child reference",
                )
            })?;
            append_char_v3_child_pointer(ctx.parent_slot, first, &mut encoded_children)?;
        } else {
            for &child in &children {
                append_char_v3_child_pointer(ctx.parent_slot, child, &mut encoded_children)?;
            }
        }
        append_char_v3_type_payload(ctx, &children, plan, &mut encoded_children)?;
        debug_assert_eq!(encoded_children.len(), children_capacity);
        write_char_keys(&node.keys, writer)?;
        writer.write_all(&encoded_children).map_err(io_err)?;
        write_char_v3_value(&node.value_ptr, writer)?;
    } else {
        write_char_v3_body(node, writer, ctx, &children, plan)?;
    }

    Ok(CHAR_SERIALIZED_HEADER_SIZE + data_size)
}

#[inline]
fn write_char_keys<W: Write>(keys: &[u32], writer: &mut W) -> Result<()> {
    for key in keys {
        writer.write_all(&key.to_le_bytes()).map_err(io_err)?;
    }
    Ok(())
}

#[inline]
fn write_char_v3_value<W: Write>(value_ptr: &SwizzledPtr, writer: &mut W) -> Result<()> {
    writer
        .write_all(&value_ptr.to_raw().to_le_bytes())
        .map_err(io_err)
}

fn write_char_v3_body<W: Write>(
    node: &CharNode,
    writer: &mut W,
    ctx: &SerializationContext,
    children: &[CharChildReference],
    plan: CharV3EncodingPlan,
) -> Result<()> {
    match node {
        CharNode::N4(node) => {
            write_char_keys(&node.keys, writer)?;
            write_char_v3_children(ctx, children, plan, writer)?;
            write_char_v3_value(&node.value_ptr, writer)
        }
        CharNode::N16(node) => {
            write_char_keys(&node.keys, writer)?;
            write_char_v3_children(ctx, children, plan, writer)?;
            write_char_v3_value(&node.value_ptr, writer)
        }
        CharNode::N48(node) => {
            write_char_keys(&node.keys, writer)?;
            write_char_v3_children(ctx, children, plan, writer)?;
            write_char_v3_value(&node.value_ptr, writer)
        }
        CharNode::Bucket(node) => {
            let num_entries = u32::try_from(node.entries.len()).map_err(|_| {
                PersistentARTrieError::corrupted(
                    "char bucket entry count exceeds the u32 wire range",
                )
            })?;
            writer
                .write_all(&num_entries.to_le_bytes())
                .map_err(io_err)?;
            write_char_v3_value(&node.value_ptr, writer)?;
            for child in children {
                writer.write_all(&child.key.to_le_bytes()).map_err(io_err)?;
            }
            write_char_v3_children(ctx, children, plan, writer)
        }
    }
}

// =============================================================================
// V2 Deserialization with Relative Offsets and Sequential Siblings
// =============================================================================

/// Context for v2 deserialization with relative offset decoding
#[derive(Debug, Clone)]
pub struct DeserializationContext {
    /// Parent's arena slot (used for relative offset reconstruction)
    pub parent_slot: ArenaSlot,
}

impl DeserializationContext {
    /// Create a new deserialization context
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecodedCharV3Location {
    slot: ArenaSlot,
    inline_type: Option<NodeType>,
}

#[inline]
fn validate_char_v3_slot(slot: ArenaSlot) -> Result<()> {
    let block_id = slot.arena_id.checked_add(1).ok_or_else(|| {
        PersistentARTrieError::corrupted("V3 child arena id exceeds persistent block range")
    })?;
    if block_id > MAX_BLOCK_ID {
        return Err(PersistentARTrieError::corrupted(format!(
            "V3 child block id {block_id} exceeds packed maximum {MAX_BLOCK_ID}"
        )));
    }
    if slot.slot_id > MAX_OFFSET {
        return Err(PersistentARTrieError::corrupted(format!(
            "V3 child slot {} exceeds packed maximum {MAX_OFFSET}",
            slot.slot_id
        )));
    }
    Ok(())
}

#[inline]
fn validated_char_v3_slot_to_ptr(slot: ArenaSlot, node_type: NodeType) -> SwizzledPtr {
    debug_assert!(slot.arena_id < MAX_BLOCK_ID);
    debug_assert!(slot.slot_id <= MAX_OFFSET);
    debug_assert!(node_type.is_char_level());
    SwizzledPtr::on_disk(slot.arena_id + 1, slot.slot_id, node_type)
}

#[inline]
fn decode_char_v3_location(
    data: &[u8],
    parent: ArenaSlot,
) -> Result<(DecodedCharV3Location, usize)> {
    let first = *data
        .first()
        .ok_or_else(|| relative_decode_err(RelativeEncodingError::EmptyInput))?;
    if matches!(first, 1 | 3 | 5 | 7) {
        const CHAR_NODE_TYPES: [NodeType; 4] = [
            NodeType::CharNode4,
            NodeType::CharNode16,
            NodeType::CharNode48,
            NodeType::CharBucket,
        ];
        let inline_type = CHAR_NODE_TYPES[usize::from((first - 1) >> 1)];
        if data.len() < CROSS_ARENA_SIZE {
            return Err(relative_decode_err(
                RelativeEncodingError::TruncatedFullPointer {
                    actual_len: data.len(),
                },
            ));
        }
        let slot = ArenaSlot::new(
            u32::from_le_bytes(data[1..5].try_into().unwrap()),
            u32::from_le_bytes(data[5..9].try_into().unwrap()),
        );
        validate_char_v3_slot(slot)?;
        return Ok((
            DecodedCharV3Location {
                slot,
                inline_type: Some(inline_type),
            },
            CROSS_ARENA_SIZE,
        ));
    }

    let (delta, consumed) = try_decode_relative(data).map_err(relative_decode_err)?;
    let slot_id = parent.slot_id.checked_sub(delta).ok_or_else(|| {
        relative_decode_err(RelativeEncodingError::RelativeUnderflow { parent, delta })
    })?;
    let slot = ArenaSlot::new(parent.arena_id, slot_id);
    validate_char_v3_slot(slot)?;
    Ok((
        DecodedCharV3Location {
            slot,
            inline_type: None,
        },
        consumed,
    ))
}

fn decode_char_v3_locations(
    data: &[u8],
    parent: ArenaSlot,
    count: usize,
    uses_sequential: bool,
    section: &str,
) -> Result<(SmallVec<[DecodedCharV3Location; 16]>, usize, usize)> {
    let mut locations = SmallVec::new();
    locations
        .try_reserve_exact(count)
        .map_err(|error| PersistentARTrieError::allocation_failed(section, count, error))?;
    if uses_sequential {
        let (first, consumed) = decode_char_v3_location(data, parent)?;
        let last_index = count.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "{section} sequential V3 layout has zero children"
            ))
        })?;
        let last_offset = u32::try_from(last_index).map_err(|_| {
            PersistentARTrieError::corrupted(format!(
                "{section} sequential V3 child count exceeds u32 slot range"
            ))
        })?;
        let last_slot_id = first.slot.slot_id.checked_add(last_offset).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "{section} sequential V3 child range overflows u32 slot range"
            ))
        })?;
        validate_char_v3_slot(ArenaSlot::new(first.slot.arena_id, last_slot_id))?;
        for index in 0..count {
            let offset = u32::try_from(index).map_err(|_| {
                PersistentARTrieError::corrupted(format!(
                    "{section} sequential V3 child index exceeds u32 slot range"
                ))
            })?;
            locations.push(DecodedCharV3Location {
                slot: ArenaSlot::new(first.slot.arena_id, first.slot.slot_id + offset),
                inline_type: (index == 0).then_some(first.inline_type).flatten(),
            });
        }
        let unresolved = count - usize::from(first.inline_type.is_some());
        return Ok((locations, consumed, unresolved));
    }

    let mut consumed = 0usize;
    let mut unresolved = 0usize;
    for _ in 0..count {
        let remaining = data.get(consumed..).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "{section} V3 child location offset exceeds its payload"
            ))
        })?;
        let (location, width) = decode_char_v3_location(remaining, parent)?;
        consumed = consumed.checked_add(width).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "{section} V3 child location length overflows usize"
            ))
        })?;
        unresolved += usize::from(location.inline_type.is_none());
        locations.push(location);
    }
    Ok((locations, consumed, unresolved))
}

fn decode_char_v3_children_direct<F>(
    header: &SerializedCharNodeHeader,
    data: &[u8],
    parent: ArenaSlot,
    count: usize,
    uses_sequential: bool,
    section: &str,
    mut visit: F,
) -> Result<usize>
where
    F: FnMut(usize, ArenaSlot, NodeType) -> Result<()>,
{
    let extension = header.child_type_extension();
    let homogeneous = header.uses_homogeneous_child_types();
    if homogeneous && extension & !0b11 != 0 {
        return Err(PersistentARTrieError::corrupted(format!(
            "noncanonical {section} homogeneous V3 child types"
        )));
    }
    let homogeneous_type = homogeneous
        .then(|| char_node_type_from_code((extension & 0b11) as u8))
        .transpose()?;
    let mut unresolved = 0usize;
    let mut consume_location = |index: usize, location: DecodedCharV3Location| -> Result<()> {
        let node_type = if let Some(node_type) = location.inline_type {
            node_type
        } else {
            let node_type = if let Some(node_type) = homogeneous_type {
                node_type
            } else {
                char_node_type_from_code(((extension >> (2 * unresolved)) & 0b11) as u8)?
            };
            unresolved += 1;
            node_type
        };
        visit(index, location.slot, node_type)
    };

    let consumed = if uses_sequential {
        let (first, consumed) = decode_char_v3_location(data, parent)?;
        let last_index = count.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "{section} sequential V3 layout has zero children"
            ))
        })?;
        let last_offset = u32::try_from(last_index).map_err(|_| {
            PersistentARTrieError::corrupted(format!(
                "{section} sequential V3 child count exceeds u32 slot range"
            ))
        })?;
        let last_slot_id = first.slot.slot_id.checked_add(last_offset).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "{section} sequential V3 child range overflows u32 slot range"
            ))
        })?;
        validate_char_v3_slot(ArenaSlot::new(first.slot.arena_id, last_slot_id))?;
        for index in 0..count {
            consume_location(
                index,
                DecodedCharV3Location {
                    slot: ArenaSlot::new(first.slot.arena_id, first.slot.slot_id + index as u32),
                    inline_type: (index == 0).then_some(first.inline_type).flatten(),
                },
            )?;
        }
        consumed
    } else {
        let mut consumed = 0usize;
        for index in 0..count {
            let remaining = data.get(consumed..).ok_or_else(|| {
                PersistentARTrieError::corrupted(format!(
                    "{section} V3 child location offset exceeds its payload"
                ))
            })?;
            let (location, width) = decode_char_v3_location(remaining, parent)?;
            consumed = consumed.checked_add(width).ok_or_else(|| {
                PersistentARTrieError::corrupted(format!(
                    "{section} V3 child location length overflows usize"
                ))
            })?;
            consume_location(index, location)?;
        }
        consumed
    };
    if homogeneous {
        if unresolved <= CHILD_TYPES_PER_HEADER {
            return Err(PersistentARTrieError::corrupted(format!(
                "noncanonical {section} homogeneous V3 child types"
            )));
        }
    } else if unresolved < CHILD_TYPES_PER_HEADER && extension >> (2 * unresolved) != 0 {
        return Err(PersistentARTrieError::corrupted(format!(
            "nonzero {section} unused V3 header type bits"
        )));
    }
    Ok(consumed)
}

fn decode_char_v3_children<F>(
    header: &SerializedCharNodeHeader,
    data: &[u8],
    parent: ArenaSlot,
    count: usize,
    uses_sequential: bool,
    section: &str,
    mut visit: F,
) -> Result<usize>
where
    F: FnMut(usize, ArenaSlot, NodeType) -> Result<()>,
{
    if header.uses_homogeneous_child_types() || count <= CHILD_TYPES_PER_HEADER {
        return decode_char_v3_children_direct(
            header,
            data,
            parent,
            count,
            uses_sequential,
            section,
            visit,
        );
    }
    let (locations, locations_end, unresolved_count) =
        decode_char_v3_locations(data, parent, count, uses_sequential, section)?;
    let extension = header.child_type_extension();
    if unresolved_count < CHILD_TYPES_PER_HEADER && extension >> (2 * unresolved_count) != 0 {
        return Err(PersistentARTrieError::corrupted(format!(
            "nonzero {section} unused V3 header type bits"
        )));
    }
    let payload_bytes = unresolved_count
        .saturating_sub(CHILD_TYPES_PER_HEADER)
        .checked_add(CHILD_TYPES_PER_PAYLOAD_BYTE - 1)
        .ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "{section} V3 child-type payload length overflows usize"
            ))
        })?
        / CHILD_TYPES_PER_PAYLOAD_BYTE;
    let types_end = locations_end.checked_add(payload_bytes).ok_or_else(|| {
        PersistentARTrieError::corrupted(format!(
            "{section} V3 child-type payload end overflows usize"
        ))
    })?;
    let payload = data.get(locations_end..types_end).ok_or_else(|| {
        PersistentARTrieError::corrupted(format!(
            "truncated {section} V3 child-type payload: need {types_end} bytes, have {}",
            data.len()
        ))
    })?;
    let trailing_codes = unresolved_count.saturating_sub(CHILD_TYPES_PER_HEADER);
    let final_codes = trailing_codes % CHILD_TYPES_PER_PAYLOAD_BYTE;
    if final_codes != 0 {
        let final_byte = *payload.last().ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "{section} V3 child-type payload omits its final byte"
            ))
        })?;
        if final_byte >> (2 * final_codes) != 0 {
            return Err(PersistentARTrieError::corrupted(format!(
                "nonzero {section} unused V3 payload type bits"
            )));
        }
    }

    let mut unresolved_index = 0usize;
    for (index, location) in locations.into_iter().enumerate() {
        let node_type = if let Some(node_type) = location.inline_type {
            node_type
        } else {
            let node_type = if unresolved_index < CHILD_TYPES_PER_HEADER {
                char_node_type_from_code(((extension >> (2 * unresolved_index)) & 0b11) as u8)?
            } else {
                let payload_index = unresolved_index - CHILD_TYPES_PER_HEADER;
                char_node_type_from_code(
                    (payload[payload_index / CHILD_TYPES_PER_PAYLOAD_BYTE]
                        >> (2 * (payload_index % CHILD_TYPES_PER_PAYLOAD_BYTE)))
                        & 0b11,
                )?
            };
            unresolved_index += 1;
            node_type
        };
        visit(index, location.slot, node_type)?;
    }
    debug_assert_eq!(unresolved_index, unresolved_count);
    Ok(types_end)
}

fn decode_relative_char_children<F>(
    header: &SerializedCharNodeHeader,
    data: &[u8],
    parent: ArenaSlot,
    count: usize,
    uses_sequential: bool,
    section: &str,
    mut visit: F,
) -> Result<usize>
where
    F: FnMut(usize, SwizzledPtr) -> Result<()>,
{
    if header.version >= CHAR_FORMAT_VERSION_V3 {
        return decode_char_v3_children(
            header,
            data,
            parent,
            count,
            uses_sequential,
            section,
            |index, slot, node_type| visit(index, validated_char_v3_slot_to_ptr(slot, node_type)),
        );
    }

    let (slots, consumed) = decode_v2_child_slots(data, parent, count, uses_sequential)?;
    for (index, slot) in slots.into_iter().enumerate() {
        // The established low-level V2 API was type-erasing. Production
        // recovery resolves exact types from target record headers instead.
        visit(index, arena_slot_to_ptr(slot, NodeType::CharNode4)?)?;
    }
    Ok(consumed)
}

fn read_value_ptr_after_children(data: &[u8], value_offset: usize) -> Result<SwizzledPtr> {
    let end = value_offset
        .checked_add(8)
        .ok_or_else(|| PersistentARTrieError::corrupted("char v2 value pointer offset overflow"))?;
    if data.len() < end {
        return Err(PersistentARTrieError::corrupted(format!(
            "truncated char v2 value pointer: child bytes consumed {}, remaining data length {}",
            value_offset,
            data.len()
        )));
    }
    if data.len() != end {
        return Err(PersistentARTrieError::corrupted(format!(
            "noncanonical char v2 data_size: value pointer ends at {}, remaining data length {}",
            end,
            data.len()
        )));
    }
    let value_raw = u64::from_le_bytes(data[value_offset..end].try_into().unwrap());
    Ok(SwizzledPtr::from_raw(value_raw))
}

/// Address-bearing child metadata without synthesizing a false node type for
/// the type-erasing relative char format.
#[derive(Debug)]
pub(crate) enum DecodedCharMetadataChild {
    Typed(SwizzledPtr),
    Untyped(ArenaSlot),
}

/// Structural metadata for one exact char arena record. Application value
/// bytes are validated and skipped, never deserialized.
#[derive(Debug)]
pub(crate) struct DecodedCharNodeMetadata {
    pub(crate) node_type: NodeType,
    pub(crate) serialized_bytes: usize,
    pub(crate) prefix: Vec<u32>,
    pub(crate) children: Vec<(u32, DecodedCharMetadataChild)>,
}

fn metadata_checked_end(offset: usize, len: usize, section: &str) -> Result<usize> {
    offset.checked_add(len).ok_or_else(|| {
        PersistentARTrieError::corrupted(format!(
            "{section} byte-range arithmetic overflow: offset {offset}, length {len}"
        ))
    })
}

fn metadata_checked_slice<'a>(
    data: &'a [u8],
    offset: usize,
    len: usize,
    section: &str,
) -> Result<&'a [u8]> {
    let end = metadata_checked_end(offset, len, section)?;
    data.get(offset..end).ok_or_else(|| {
        PersistentARTrieError::corrupted(format!(
            "truncated {section}: need byte range {offset}..{end}, record has {} bytes",
            data.len()
        ))
    })
}

fn metadata_u32(data: &[u8], offset: usize, section: &str) -> Result<u32> {
    let bytes = metadata_checked_slice(data, offset, 4, section)?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(raw))
}

fn metadata_u64(data: &[u8], offset: usize, section: &str) -> Result<u64> {
    let bytes = metadata_checked_slice(data, offset, 8, section)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(raw))
}

fn char_metadata_node_type(encoded: u8) -> Result<NodeType> {
    match encoded {
        char_node_types::CHARNODE4 => Ok(NodeType::CharNode4),
        char_node_types::CHARNODE16 => Ok(NodeType::CharNode16),
        char_node_types::CHARNODE48 => Ok(NodeType::CharNode48),
        char_node_types::CHARBUCKET => Ok(NodeType::CharBucket),
        _ => Err(PersistentARTrieError::corrupted(format!(
            "invalid char metadata node type {encoded}"
        ))),
    }
}

fn validate_unicode_scalar(unit: u32, section: &str, index: usize) -> Result<()> {
    if char::from_u32(unit).is_none() {
        return Err(PersistentARTrieError::corrupted(format!(
            "{section} contains non-Unicode-scalar unit {unit:#010x} at index {index}"
        )));
    }
    Ok(())
}

fn decode_char_metadata_keys(
    data: &[u8],
    capacity: usize,
    count: usize,
    section: &'static str,
) -> Result<Vec<u32>> {
    let mut keys = Vec::new();
    keys.try_reserve_exact(count)
        .map_err(|error| PersistentARTrieError::allocation_failed(section, count, error))?;
    let key_bytes = capacity.checked_mul(4).ok_or_else(|| {
        PersistentARTrieError::corrupted(format!("{section} key byte count overflow"))
    })?;
    metadata_checked_slice(data, 0, key_bytes, section)?;
    for index in 0..count {
        let offset = index.checked_mul(4).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!("{section} key offset overflow"))
        })?;
        let key = metadata_u32(data, offset, section)?;
        validate_unicode_scalar(key, section, index)?;
        if keys.last().is_some_and(|previous| *previous >= key) {
            return Err(PersistentARTrieError::corrupted(format!(
                "{section} active keys are not strictly increasing"
            )));
        }
        keys.push(key);
    }
    for index in count..capacity {
        let offset = index.checked_mul(4).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!("{section} unused key offset overflow"))
        })?;
        if metadata_u32(data, offset, section)? != 0 {
            return Err(PersistentARTrieError::corrupted(format!(
                "{section} unused key slot {index} is nonzero"
            )));
        }
    }
    Ok(keys)
}

fn validate_char_value_pointer(raw: u64, section: &str) -> Result<()> {
    if raw == 0 {
        return Ok(());
    }
    let pointer = SwizzledPtr::from_raw(raw);
    let location = pointer.disk_location().ok_or_else(|| {
        PersistentARTrieError::corrupted(format!(
            "{section} is in-memory, transitional, null, or malformed"
        ))
    })?;
    if location.block_id == 0 || location.node_type != NodeType::Bucket {
        return Err(PersistentARTrieError::corrupted(format!(
            "{section} must be null or a nonzero-block byte bucket pointer"
        )));
    }
    Ok(())
}

fn decode_fixed_char_child(raw: u64, section: &str) -> Result<SwizzledPtr> {
    let pointer = SwizzledPtr::from_raw(raw);
    let location = pointer.disk_location().ok_or_else(|| {
        PersistentARTrieError::corrupted(format!(
            "{section} is null, in-memory, transitional, or malformed"
        ))
    })?;
    if location.block_id == 0 || !location.node_type.is_char_level() {
        return Err(PersistentARTrieError::corrupted(format!(
            "{section} must reference a nonzero-block char node"
        )));
    }
    Ok(pointer)
}

fn decode_dense_char_metadata_children(
    header: &SerializedCharNodeHeader,
    payload: &[u8],
    key_capacity: usize,
    ctx: &DeserializationContext,
    section: &'static str,
) -> Result<Vec<(u32, DecodedCharMetadataChild)>> {
    let count = header.num_children as usize;
    let keys = decode_char_metadata_keys(payload, key_capacity, count, section)?;
    let keys_bytes = key_capacity.checked_mul(4).ok_or_else(|| {
        PersistentARTrieError::corrupted(format!("{section} key byte count overflow"))
    })?;
    let mut children = Vec::new();
    children
        .try_reserve_exact(count)
        .map_err(|error| PersistentARTrieError::allocation_failed(section, count, error))?;

    if header.uses_relative_offsets() {
        let value_offset = payload.len().checked_sub(8).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!("{section} omits its value pointer"))
        })?;
        if value_offset < keys_bytes {
            return Err(PersistentARTrieError::corrupted(format!(
                "{section} payload is shorter than its key array"
            )));
        }
        let encoded_children = payload.get(keys_bytes..value_offset).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!("{section} child range is invalid"))
        })?;
        validate_char_value_pointer(
            metadata_u64(payload, value_offset, section)?,
            "char node value pointer",
        )?;
        let consumed = if header.version >= CHAR_FORMAT_VERSION_V3 {
            decode_char_v3_children(
                header,
                encoded_children,
                ctx.parent_slot,
                count,
                header.uses_sequential_siblings(),
                section,
                |index, slot, node_type| {
                    children.push((
                        keys[index],
                        DecodedCharMetadataChild::Typed(validated_char_v3_slot_to_ptr(
                            slot, node_type,
                        )),
                    ));
                    Ok(())
                },
            )?
        } else {
            let (slots, consumed) = decode_v2_child_slots(
                encoded_children,
                ctx.parent_slot,
                count,
                header.uses_sequential_siblings(),
            )?;
            for (key, slot) in keys.into_iter().zip(slots) {
                // Validate representability even though legacy metadata retains
                // the address without inventing a concrete child type.
                arena_slot_to_ptr(slot, NodeType::CharNode4)?;
                children.push((key, DecodedCharMetadataChild::Untyped(slot)));
            }
            consumed
        };
        if consumed != encoded_children.len() {
            return Err(PersistentARTrieError::corrupted(format!(
                "{section} child locations and types consume {consumed} of {} bytes",
                encoded_children.len()
            )));
        }
    } else {
        let child_bytes = key_capacity.checked_mul(8).ok_or_else(|| {
            PersistentARTrieError::corrupted(format!("{section} child byte count overflow"))
        })?;
        let value_offset = metadata_checked_end(keys_bytes, child_bytes, section)?;
        let expected = metadata_checked_end(value_offset, 8, section)?;
        if payload.len() != expected {
            return Err(PersistentARTrieError::corrupted(format!(
                "noncanonical {section} payload length {}, expected {expected}",
                payload.len()
            )));
        }
        validate_char_value_pointer(
            metadata_u64(payload, value_offset, section)?,
            "char node value pointer",
        )?;
        for (index, key) in keys.into_iter().enumerate() {
            let pointer_offset = metadata_checked_end(
                keys_bytes,
                index.checked_mul(8).ok_or_else(|| {
                    PersistentARTrieError::corrupted(format!(
                        "{section} child pointer offset overflow"
                    ))
                })?,
                section,
            )?;
            let pointer =
                decode_fixed_char_child(metadata_u64(payload, pointer_offset, section)?, section)?;
            children.push((key, DecodedCharMetadataChild::Typed(pointer)));
        }
        for index in count..key_capacity {
            let pointer_offset = metadata_checked_end(
                keys_bytes,
                index.checked_mul(8).ok_or_else(|| {
                    PersistentARTrieError::corrupted(format!(
                        "{section} unused child offset overflow"
                    ))
                })?,
                section,
            )?;
            if metadata_u64(payload, pointer_offset, section)? != 0 {
                return Err(PersistentARTrieError::corrupted(format!(
                    "{section} unused child slot {index} is non-null"
                )));
            }
        }
    }
    Ok(children)
}

fn decode_bucket_char_metadata_children(
    header: &SerializedCharNodeHeader,
    payload: &[u8],
    ctx: &DeserializationContext,
) -> Result<Vec<(u32, DecodedCharMetadataChild)>> {
    let count = header.num_children as usize;
    let encoded_count = metadata_u32(payload, 0, "char bucket entry count")? as usize;
    if encoded_count != count {
        return Err(PersistentARTrieError::corrupted(format!(
            "char bucket payload declares {encoded_count} entries but header declares {count}"
        )));
    }
    validate_char_value_pointer(
        metadata_u64(payload, 4, "char bucket value pointer")?,
        "char bucket value pointer",
    )?;

    let mut children = Vec::new();
    children.try_reserve_exact(count).map_err(|error| {
        PersistentARTrieError::allocation_failed("char bucket metadata", count, error)
    })?;
    if header.uses_relative_offsets() {
        let keys_bytes = count.checked_mul(4).ok_or_else(|| {
            PersistentARTrieError::corrupted("char bucket key byte count overflow")
        })?;
        let children_start = metadata_checked_end(12, keys_bytes, "char bucket keys")?;
        let encoded_children = payload.get(children_start..).ok_or_else(|| {
            PersistentARTrieError::corrupted("char bucket child range is invalid")
        })?;
        let mut previous = None;
        let mut visit_child =
            |index: usize, slot: ArenaSlot, child: DecodedCharMetadataChild| -> Result<()> {
                let key_offset = metadata_checked_end(
                    12,
                    index.checked_mul(4).ok_or_else(|| {
                        PersistentARTrieError::corrupted("char bucket key offset overflow")
                    })?,
                    "char bucket key",
                )?;
                let key = metadata_u32(payload, key_offset, "char bucket key")?;
                validate_unicode_scalar(key, "char bucket key", index)?;
                if previous.is_some_and(|previous| previous >= key) {
                    return Err(PersistentARTrieError::corrupted(
                        "char bucket keys are not strictly increasing",
                    ));
                }
                previous = Some(key);
                if let DecodedCharMetadataChild::Untyped(_) = child {
                    arena_slot_to_ptr(slot, NodeType::CharNode4)?;
                }
                children.push((key, child));
                Ok(())
            };
        let consumed = if header.version >= CHAR_FORMAT_VERSION_V3 {
            decode_char_v3_children(
                header,
                encoded_children,
                ctx.parent_slot,
                count,
                header.uses_sequential_siblings(),
                "char bucket metadata",
                |index, slot, node_type| {
                    visit_child(
                        index,
                        slot,
                        DecodedCharMetadataChild::Typed(validated_char_v3_slot_to_ptr(
                            slot, node_type,
                        )),
                    )
                },
            )?
        } else {
            let (slots, consumed) = decode_v2_child_slots(
                encoded_children,
                ctx.parent_slot,
                count,
                header.uses_sequential_siblings(),
            )?;
            for (index, slot) in slots.into_iter().enumerate() {
                visit_child(index, slot, DecodedCharMetadataChild::Untyped(slot))?;
            }
            consumed
        };
        if consumed != encoded_children.len() {
            return Err(PersistentARTrieError::corrupted(format!(
                "char bucket child locations and types consume {consumed} of {} bytes",
                encoded_children.len()
            )));
        }
    } else {
        let entry_bytes = count.checked_mul(12).ok_or_else(|| {
            PersistentARTrieError::corrupted("char bucket entry byte count overflow")
        })?;
        let expected = metadata_checked_end(12, entry_bytes, "char bucket entries")?;
        if payload.len() != expected {
            return Err(PersistentARTrieError::corrupted(format!(
                "noncanonical char bucket payload length {}, expected {expected}",
                payload.len()
            )));
        }
        let mut previous = None;
        for index in 0..count {
            let entry_offset = metadata_checked_end(
                12,
                index.checked_mul(12).ok_or_else(|| {
                    PersistentARTrieError::corrupted("char bucket entry offset overflow")
                })?,
                "char bucket entry",
            )?;
            let key = metadata_u32(payload, entry_offset, "char bucket key")?;
            validate_unicode_scalar(key, "char bucket key", index)?;
            if previous.is_some_and(|previous| previous >= key) {
                return Err(PersistentARTrieError::corrupted(
                    "char bucket keys are not strictly increasing",
                ));
            }
            previous = Some(key);
            let pointer_offset = metadata_checked_end(entry_offset, 4, "char bucket child")?;
            let pointer = decode_fixed_char_child(
                metadata_u64(payload, pointer_offset, "char bucket child")?,
                "char bucket child",
            )?;
            children.push((key, DecodedCharMetadataChild::Typed(pointer)));
        }
    }
    Ok(children)
}

/// Decode only path and child metadata from one exact persistent char-node
/// arena record. The trailing application value envelope is length-checked but
/// never allocated, copied, or deserialized.
pub(crate) fn decode_char_node_metadata(
    data: &[u8],
    ctx: &DeserializationContext,
    expected_type: Option<NodeType>,
) -> Result<DecodedCharNodeMetadata> {
    let header_slice =
        metadata_checked_slice(data, 0, CHAR_SERIALIZED_HEADER_SIZE, "char node header")?;
    let mut header_bytes = [0u8; CHAR_SERIALIZED_HEADER_SIZE];
    header_bytes.copy_from_slice(header_slice);
    let header = SerializedCharNodeHeader::from_bytes(&header_bytes);
    header.validate()?;
    let node_type = char_metadata_node_type(header.node_type)?;
    if let Some(expected_type) = expected_type {
        if expected_type != node_type {
            return Err(PersistentARTrieError::NodeTypeMismatch {
                expected: format!("{expected_type:?}"),
                found: format!("{node_type:?}"),
            });
        }
    }

    let structural_end = metadata_checked_end(
        CHAR_SERIALIZED_HEADER_SIZE,
        header.data_size as usize,
        "char node structural payload",
    )?;
    let structural = data
        .get(CHAR_SERIALIZED_HEADER_SIZE..structural_end)
        .ok_or_else(|| {
            PersistentARTrieError::corrupted(format!(
                "truncated char node structural payload: header declares {} bytes",
                header.data_size
            ))
        })?;
    let value_len = metadata_u32(data, structural_end, "char node value length")? as usize;
    let value_start = metadata_checked_end(structural_end, 4, "char node value prefix")?;
    let value_end = metadata_checked_end(value_start, value_len, "char node value")?;
    metadata_checked_slice(data, value_start, value_len, "char node value")?;
    if value_end != data.len() {
        return Err(PersistentARTrieError::corrupted(format!(
            "char node record has {} trailing bytes after its declared value",
            data.len().saturating_sub(value_end)
        )));
    }

    let prefix_storage_len = header_prefix_size(&header);
    let mut prefix = Vec::new();
    let prefix_len = header.prefix_len as usize;
    prefix.try_reserve_exact(prefix_len).map_err(|error| {
        PersistentARTrieError::allocation_failed("char metadata prefix", prefix_len, error)
    })?;
    if prefix_storage_len > 0 {
        metadata_checked_slice(structural, 0, prefix_storage_len, "char metadata prefix")?;
        for index in 0..prefix_len {
            let offset = index.checked_mul(4).ok_or_else(|| {
                PersistentARTrieError::corrupted("char metadata prefix offset overflow")
            })?;
            let unit = metadata_u32(structural, offset, "char metadata prefix")?;
            validate_unicode_scalar(unit, "char metadata prefix", index)?;
            prefix.push(unit);
        }
    }
    let payload = structural.get(prefix_storage_len..).ok_or_else(|| {
        PersistentARTrieError::corrupted("char metadata prefix exceeds structural payload")
    })?;
    let children = match node_type {
        NodeType::CharNode4 => {
            decode_dense_char_metadata_children(&header, payload, 4, ctx, "char node4 metadata")?
        }
        NodeType::CharNode16 => {
            decode_dense_char_metadata_children(&header, payload, 16, ctx, "char node16 metadata")?
        }
        NodeType::CharNode48 => {
            decode_dense_char_metadata_children(&header, payload, 48, ctx, "char node48 metadata")?
        }
        NodeType::CharBucket => decode_bucket_char_metadata_children(&header, payload, ctx)?,
        _ => {
            return Err(PersistentARTrieError::corrupted(
                "non-char node type reached char metadata decoder",
            ));
        }
    };

    Ok(DecodedCharNodeMetadata {
        node_type,
        serialized_bytes: data.len(),
        prefix,
        children,
    })
}

/// Deserialize a CharNode using v2 format with relative offset decoding
///
/// Handles both relative offset and sequential sibling encodings based on
/// header flags.
///
/// # Arguments
/// * `reader` - Input reader
/// * `ctx` - Deserialization context with parent slot for offset reconstruction
///
/// # Returns
/// The deserialized CharNode
pub fn deserialize_char_node_v2<R: Read>(
    reader: &mut R,
    ctx: &DeserializationContext,
) -> Result<CharNode> {
    deserialize_char_node_record(reader, ctx).map(|(node, _)| node)
}

/// Deserialize one persistent character record while retaining its validated
/// wire header for production recovery decisions.
pub(crate) fn deserialize_char_node_record<R: Read>(
    reader: &mut R,
    ctx: &DeserializationContext,
) -> Result<(CharNode, SerializedCharNodeHeader)> {
    // Read and validate header
    let mut header_bytes = [0u8; CHAR_SERIALIZED_HEADER_SIZE];
    reader.read_exact(&mut header_bytes).map_err(io_err)?;
    let header = SerializedCharNodeHeader::from_bytes(&header_bytes);
    header.validate()?;

    // Read prefix if present
    let prefix = if header.prefix_len > 0 {
        let mut chars = [0u32; CHAR_MAX_PREFIX_LEN];
        for c in &mut chars {
            let mut bytes = [0u8; 4];
            reader.read_exact(&mut bytes).map_err(io_err)?;
            *c = u32::from_le_bytes(bytes);
        }
        CharCompressedPrefix { chars }
    } else {
        CharCompressedPrefix::empty()
    };

    // Check encoding flags
    let uses_sequential = header.uses_sequential_siblings();
    let uses_relative = header.uses_relative_offsets();

    // Deserialize type-specific data
    let node = match header.node_type {
        char_node_types::CHARNODE4 => {
            deserialize_charnode4_v2(reader, &header, prefix, ctx, uses_sequential, uses_relative)
        }
        char_node_types::CHARNODE16 => {
            deserialize_charnode16_v2(reader, &header, prefix, ctx, uses_sequential, uses_relative)
        }
        char_node_types::CHARNODE48 => {
            deserialize_charnode48_v2(reader, &header, prefix, ctx, uses_sequential, uses_relative)
        }
        char_node_types::CHARBUCKET => {
            deserialize_charbucket_v2(reader, &header, prefix, ctx, uses_sequential, uses_relative)
        }
        _ => Err(PersistentARTrieError::corrupted(format!(
            "invalid char node type: {}",
            header.node_type
        ))),
    }?;
    Ok((node, header))
}

/// Resolve the authoritative representation tag from a target arena record.
pub(crate) fn char_record_node_type(data: &[u8]) -> Result<NodeType> {
    let header_bytes: &[u8; CHAR_SERIALIZED_HEADER_SIZE] = data
        .get(..CHAR_SERIALIZED_HEADER_SIZE)
        .ok_or_else(|| {
            PersistentARTrieError::corrupted("truncated target character-record header")
        })?
        .try_into()
        .unwrap();
    let header = SerializedCharNodeHeader::from_bytes(header_bytes);
    header.validate()?;
    char_metadata_node_type(header.node_type)
}

/// Restore child types erased by established relative V2 records.
///
/// The caller supplies a direct target-record header resolver. The walk is
/// iterative and allocation-free; V3 and fixed-width V2 records return after
/// one format/flag check.
pub(crate) fn resolve_legacy_v2_child_types<F>(
    header: &SerializedCharNodeHeader,
    node: &mut CharNode,
    mut resolve: F,
) -> Result<()>
where
    F: FnMut(ArenaSlot) -> Result<NodeType>,
{
    if header.version > CHAR_FORMAT_VERSION_V2 || !header.uses_relative_offsets() {
        return Ok(());
    }

    let mut resolve_pointer = |pointer: &mut SwizzledPtr| -> Result<()> {
        let location = pointer.disk_location().ok_or_else(|| {
            PersistentARTrieError::corrupted(
                "legacy relative V2 child is null, resident, transitional, or malformed",
            )
        })?;
        let arena_id = location.block_id.checked_sub(1).ok_or_else(|| {
            PersistentARTrieError::corrupted("legacy relative V2 child targets reserved block zero")
        })?;
        let slot = ArenaSlot::new(arena_id, location.offset);
        *pointer = arena_slot_to_ptr(slot, resolve(slot)?)?;
        Ok(())
    };

    match node {
        CharNode::N4(node) => {
            for child in &mut node.children[..node.header.num_children as usize] {
                resolve_pointer(child)?;
            }
        }
        CharNode::N16(node) => {
            for child in &mut node.children[..node.header.num_children as usize] {
                resolve_pointer(child)?;
            }
        }
        CharNode::N48(node) => {
            for child in &mut node.children[..node.header.num_children as usize] {
                resolve_pointer(child)?;
            }
        }
        CharNode::Bucket(node) => {
            for child in node.entries.values_mut() {
                resolve_pointer(child)?;
            }
        }
    }
    Ok(())
}

fn deserialize_charnode4_v2<R: Read>(
    reader: &mut R,
    header: &SerializedCharNodeHeader,
    prefix: CharCompressedPrefix,
    ctx: &DeserializationContext,
    uses_sequential: bool,
    uses_relative: bool,
) -> Result<CharNode> {
    let mut node = CharNode4::new();
    node.header = header.to_node_header();
    node.prefix = prefix;

    // Read keys
    for key in &mut node.keys {
        let mut bytes = [0u8; 4];
        reader.read_exact(&mut bytes).map_err(io_err)?;
        *key = u32::from_le_bytes(bytes);
    }

    // Read children based on encoding flags
    let num_children = header.num_children as usize;

    let prefix_size = header_prefix_size(header);

    if uses_sequential || uses_relative {
        let remaining_data =
            read_remaining_data(reader, header.data_size as usize, 4 * 4, prefix_size)?;
        let encoded_children_end = decode_relative_char_children(
            header,
            &remaining_data,
            ctx.parent_slot,
            num_children,
            uses_sequential,
            "char node4",
            |index, child| {
                node.children[index] = child;
                Ok(())
            },
        )?;

        node.value_ptr = read_value_ptr_after_children(&remaining_data, encoded_children_end)?;
    } else {
        ensure_fixed_node_data_size(header, 4 * 4, 4)?;
        // Legacy fixed-width encoding
        for child in &mut node.children {
            let mut raw_bytes = [0u8; 8];
            reader.read_exact(&mut raw_bytes).map_err(io_err)?;
            *child = SwizzledPtr::from_raw(u64::from_le_bytes(raw_bytes));
        }

        // Read value_ptr
        let mut value_bytes = [0u8; 8];
        reader.read_exact(&mut value_bytes).map_err(io_err)?;
        node.value_ptr = SwizzledPtr::from_raw(u64::from_le_bytes(value_bytes));
    }

    Ok(CharNode::N4(Box::new(node)))
}

fn deserialize_charnode16_v2<R: Read>(
    reader: &mut R,
    header: &SerializedCharNodeHeader,
    prefix: CharCompressedPrefix,
    ctx: &DeserializationContext,
    uses_sequential: bool,
    uses_relative: bool,
) -> Result<CharNode> {
    let mut node = CharNode16::new();
    node.header = header.to_node_header();
    node.prefix = prefix;

    // Read keys
    for key in &mut node.keys {
        let mut bytes = [0u8; 4];
        reader.read_exact(&mut bytes).map_err(io_err)?;
        *key = u32::from_le_bytes(bytes);
    }

    let num_children = header.num_children as usize;
    let prefix_size = header_prefix_size(header);

    if uses_sequential || uses_relative {
        let remaining_data =
            read_remaining_data(reader, header.data_size as usize, 16 * 4, prefix_size)?;
        let encoded_children_end = decode_relative_char_children(
            header,
            &remaining_data,
            ctx.parent_slot,
            num_children,
            uses_sequential,
            "char node16",
            |index, child| {
                node.children[index] = child;
                Ok(())
            },
        )?;

        node.value_ptr = read_value_ptr_after_children(&remaining_data, encoded_children_end)?;
    } else {
        ensure_fixed_node_data_size(header, 16 * 4, 16)?;
        for child in &mut node.children {
            let mut raw_bytes = [0u8; 8];
            reader.read_exact(&mut raw_bytes).map_err(io_err)?;
            *child = SwizzledPtr::from_raw(u64::from_le_bytes(raw_bytes));
        }

        let mut value_bytes = [0u8; 8];
        reader.read_exact(&mut value_bytes).map_err(io_err)?;
        node.value_ptr = SwizzledPtr::from_raw(u64::from_le_bytes(value_bytes));
    }

    Ok(CharNode::N16(Box::new(node)))
}

fn deserialize_charnode48_v2<R: Read>(
    reader: &mut R,
    header: &SerializedCharNodeHeader,
    prefix: CharCompressedPrefix,
    ctx: &DeserializationContext,
    uses_sequential: bool,
    uses_relative: bool,
) -> Result<CharNode> {
    let mut node = CharNode48::new();
    node.header = header.to_node_header();
    node.prefix = prefix;

    // Read keys
    for key in &mut node.keys {
        let mut bytes = [0u8; 4];
        reader.read_exact(&mut bytes).map_err(io_err)?;
        *key = u32::from_le_bytes(bytes);
    }

    let num_children = header.num_children as usize;
    let prefix_size = header_prefix_size(header);

    if uses_sequential || uses_relative {
        let remaining_data =
            read_remaining_data(reader, header.data_size as usize, 48 * 4, prefix_size)?;
        let encoded_children_end = decode_relative_char_children(
            header,
            &remaining_data,
            ctx.parent_slot,
            num_children,
            uses_sequential,
            "char node48",
            |index, child| {
                node.children[index] = child;
                Ok(())
            },
        )?;

        node.value_ptr = read_value_ptr_after_children(&remaining_data, encoded_children_end)?;
    } else {
        ensure_fixed_node_data_size(header, 48 * 4, 48)?;
        for child in &mut node.children {
            let mut raw_bytes = [0u8; 8];
            reader.read_exact(&mut raw_bytes).map_err(io_err)?;
            *child = SwizzledPtr::from_raw(u64::from_le_bytes(raw_bytes));
        }

        let mut value_bytes = [0u8; 8];
        reader.read_exact(&mut value_bytes).map_err(io_err)?;
        node.value_ptr = SwizzledPtr::from_raw(u64::from_le_bytes(value_bytes));
    }

    Ok(CharNode::N48(Box::new(node)))
}

fn deserialize_charbucket_v2<R: Read>(
    reader: &mut R,
    header: &SerializedCharNodeHeader,
    prefix: CharCompressedPrefix,
    ctx: &DeserializationContext,
    uses_sequential: bool,
    uses_relative: bool,
) -> Result<CharNode> {
    let mut node = CharBucket::new();
    node.header = header.to_node_header();
    node.prefix = prefix;

    // Read number of entries
    let mut num_entries_bytes = [0u8; 4];
    reader.read_exact(&mut num_entries_bytes).map_err(io_err)?;
    let num_entries = u32::from_le_bytes(num_entries_bytes) as usize;
    ensure_bucket_entry_count(header, num_entries)?;

    // Read value_ptr
    let mut value_bytes = [0u8; 8];
    reader.read_exact(&mut value_bytes).map_err(io_err)?;
    node.value_ptr = SwizzledPtr::from_raw(u64::from_le_bytes(value_bytes));

    let prefix_size = header_prefix_size(header);

    if uses_sequential || uses_relative {
        // Read keys first
        let mut keys = Vec::with_capacity(num_entries);
        for _ in 0..num_entries {
            let mut key_bytes = [0u8; 4];
            reader.read_exact(&mut key_bytes).map_err(io_err)?;
            keys.push(u32::from_le_bytes(key_bytes));
        }

        // Read remaining data for children
        // data_size includes prefix, but prefix was already read before this function was called
        let entries_key_bytes = num_entries.checked_mul(4).ok_or_else(|| {
            PersistentARTrieError::corrupted("char bucket key layout size overflow")
        })?;
        let consumed_before_children = checked_layout_add(prefix_size, 4, "bucket count")?;
        let consumed_before_children =
            checked_layout_add(consumed_before_children, 8, "bucket value pointer")?;
        let consumed_before_children =
            checked_layout_add(consumed_before_children, entries_key_bytes, "bucket keys")?;
        let remaining_size = (header.data_size as usize)
            .checked_sub(consumed_before_children)
            .ok_or_else(|| {
                PersistentARTrieError::corrupted(format!(
                    "char bucket data_size {} is smaller than fixed payload {}",
                    header.data_size, consumed_before_children
                ))
            })?;
        let mut remaining_data = vec![0u8; remaining_size];
        reader.read_exact(&mut remaining_data).map_err(io_err)?;

        let encoded_children_end = decode_relative_char_children(
            header,
            &remaining_data,
            ctx.parent_slot,
            num_entries,
            uses_sequential,
            "char bucket",
            |index, child| {
                node.entries.insert(keys[index], child);
                Ok(())
            },
        )?;
        if encoded_children_end != remaining_data.len() {
            return Err(PersistentARTrieError::corrupted(format!(
                "char bucket child locations and types consume {} bytes from {} bytes",
                encoded_children_end,
                remaining_data.len()
            )));
        }
    } else {
        ensure_bucket_fixed_data_size(header, num_entries)?;
        // Legacy fixed-width encoding
        for _ in 0..num_entries {
            let mut key_bytes = [0u8; 4];
            reader.read_exact(&mut key_bytes).map_err(io_err)?;
            let key = u32::from_le_bytes(key_bytes);

            let mut child_bytes = [0u8; 8];
            reader.read_exact(&mut child_bytes).map_err(io_err)?;
            let child = SwizzledPtr::from_raw(u64::from_le_bytes(child_bytes));

            node.entries.insert(key, child);
        }
    }

    Ok(CharNode::Bucket(Box::new(node)))
}

/// Read remaining data from a reader after prefix and keys have been read
///
/// # Arguments
/// * `reader` - Input reader positioned after prefix and keys
/// * `data_size` - Total data size from header (includes prefix + keys + children + value_ptr)
/// * `keys_size` - Size of keys already read
/// * `prefix_size` - Size of prefix already read (24 bytes if prefix_len > 0, else 0)
fn read_remaining_data<R: Read>(
    reader: &mut R,
    data_size: usize,
    keys_size: usize,
    prefix_size: usize,
) -> Result<Vec<u8>> {
    let consumed = checked_layout_add(prefix_size, keys_size, "node keys")?;
    let remaining_size = data_size.checked_sub(consumed).ok_or_else(|| {
        PersistentARTrieError::corrupted(format!(
            "char v2 data_size {} is smaller than prefix+keys {}",
            data_size, consumed
        ))
    })?;
    let mut data = vec![0u8; remaining_size];
    reader.read_exact(&mut data).map_err(io_err)?;
    Ok(data)
}

/// Calculate the serialized prefix size from header
#[inline]
fn header_prefix_size(header: &SerializedCharNodeHeader) -> usize {
    if header.prefix_len > 0 {
        CHAR_MAX_PREFIX_LEN * 4 // 6 chars × 4 bytes = 24 bytes
    } else {
        0
    }
}

/// Convert an ArenaSlot back to a SwizzledPtr
///
/// Creates a disk-based SwizzledPtr from arena coordinates.
fn arena_slot_to_ptr(slot: ArenaSlot, node_type: NodeType) -> Result<SwizzledPtr> {
    // Arena N is stored in Block N+1
    let block_id = slot.arena_id.checked_add(1).ok_or_else(|| {
        PersistentARTrieError::corrupted(
            "relative char child arena id exceeds persistent block range",
        )
    })?;
    if !node_type.is_char_level() {
        return Err(PersistentARTrieError::corrupted(format!(
            "relative char child has non-char node type {node_type:?}"
        )));
    }
    SwizzledPtr::try_on_disk(block_id, slot.slot_id, node_type).map_err(|error| {
        PersistentARTrieError::corrupted(format!(
            "relative char child address is not representable: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_artrie::char::nodes::flags;
    use crate::persistent_artrie::char::nodes::CharArtNode;
    use crate::persistent_artrie::NodeType;

    #[test]
    fn serializers_reject_header_tag_that_disagrees_with_representation() {
        let mut node = CharNode::new_node4();
        node.header_mut().node_type = char_node_types::CHARNODE16;

        let mut legacy = Vec::new();
        assert!(matches!(
            serialize_char_node(&node, &mut legacy),
            Err(PersistentARTrieError::NodeTypeMismatch { .. })
        ));
        assert!(legacy.is_empty(), "validation must precede every write");

        let mut relative = Vec::new();
        let context = SerializationContext::new(ArenaSlot::new(0, 1));
        assert!(matches!(
            serialize_char_node_v2(&node, &mut relative, &context),
            Err(PersistentARTrieError::NodeTypeMismatch { .. })
        ));
        assert!(relative.is_empty(), "validation must precede every write");
    }

    #[test]
    fn test_header_roundtrip() {
        let header = SerializedCharNodeHeader {
            magic: CHAR_NODE_MAGIC,
            version: CHAR_FORMAT_VERSION,
            node_type: char_node_types::CHARNODE4,
            flags: flags::IS_FINAL | FLAG_RELATIVE_OFFSETS,
            reserved: 0,
            num_children: 3,
            prefix_len: 5,
            _padding: 0,
            data_size: 100,
        };

        let bytes = header.to_bytes();
        let restored = SerializedCharNodeHeader::from_bytes(&bytes);

        assert_eq!(restored.magic, CHAR_NODE_MAGIC);
        assert_eq!(restored.version, CHAR_FORMAT_VERSION);
        assert_eq!(restored.node_type, char_node_types::CHARNODE4);
        assert_eq!(restored.flags, flags::IS_FINAL | FLAG_RELATIVE_OFFSETS);
        assert_eq!(restored.num_children, 3);
        assert_eq!(restored.prefix_len, 5);
        assert_eq!(restored.data_size, 100);
    }

    #[test]
    fn test_header_validation() {
        let mut header = SerializedCharNodeHeader {
            magic: CHAR_NODE_MAGIC,
            version: CHAR_FORMAT_VERSION,
            node_type: char_node_types::CHARNODE4,
            flags: FLAG_RELATIVE_OFFSETS,
            reserved: 0,
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
        header.magic = CHAR_NODE_MAGIC;

        // Future version
        header.version = 255;
        assert!(matches!(
            header.validate(),
            Err(PersistentARTrieError::UnsupportedVersion { .. })
        ));
        header.version = CHAR_FORMAT_VERSION;

        // Invalid node type
        header.node_type = 99;
        assert!(matches!(
            header.validate(),
            Err(PersistentARTrieError::CorruptedFile { .. })
        ));
        header.node_type = char_node_types::CHARNODE4;

        // Invalid prefix length
        header.prefix_len = 10;
        assert!(matches!(
            header.validate(),
            Err(PersistentARTrieError::CorruptedFile { .. })
        ));
    }

    #[test]
    fn test_charnode4_roundtrip() {
        let mut node4 = CharNode4::new();
        let prefix_chars: Vec<u32> = "test".chars().map(|c| c as u32).collect();
        node4.prefix = CharCompressedPrefix::from_chars(&prefix_chars);
        node4.header.prefix_len = 4;
        node4.header.set_final(true);

        // Add some children
        node4
            .add_child(
                'a' as u32,
                SwizzledPtr::on_disk(100, 0, NodeType::CharNode4),
            )
            .expect("add child a");
        node4
            .add_child(
                'b' as u32,
                SwizzledPtr::on_disk(200, 0, NodeType::CharNode16),
            )
            .expect("add child b");

        let node = CharNode::N4(Box::new(node4));
        let bytes = char_to_bytes(&node).expect("serialize");
        let restored = char_from_bytes(&bytes).expect("deserialize");

        assert!(matches!(restored, CharNode::N4(_)));
        assert_eq!(restored.header().prefix_len, 4);
        assert!(restored.header().is_final());
        assert_eq!(restored.header().num_children, 2);
        assert!(restored.find_child('a' as u32).is_some());
        assert!(restored.find_child('b' as u32).is_some());
        assert!(restored.find_child('c' as u32).is_none());
    }

    #[test]
    fn test_charnode16_roundtrip() {
        let mut node16 = CharNode16::new();
        let prefix_chars: Vec<u32> = "prefix".chars().map(|c| c as u32).collect();
        node16.prefix = CharCompressedPrefix::from_chars(&prefix_chars);
        node16.header.prefix_len = 6;

        // Add some children
        for i in 0..8 {
            node16
                .add_child(
                    'a' as u32 + i,
                    SwizzledPtr::on_disk(i, 0, NodeType::CharNode4),
                )
                .expect("add child");
        }

        let node = CharNode::N16(Box::new(node16));
        let bytes = char_to_bytes(&node).expect("serialize");
        let restored = char_from_bytes(&bytes).expect("deserialize");

        assert!(matches!(restored, CharNode::N16(_)));
        assert_eq!(restored.header().prefix_len, 6);
        assert_eq!(restored.header().num_children, 8);

        for i in 0..8 {
            assert!(restored.find_child('a' as u32 + i).is_some());
        }
    }

    #[test]
    fn test_charnode48_roundtrip() {
        let mut node48 = CharNode48::new();

        // Add children at various Unicode code points
        let keys: Vec<u32> = "αβγδεζηθ".chars().map(|c| c as u32).collect();
        for (i, &key) in keys.iter().enumerate() {
            node48
                .add_child(key, SwizzledPtr::on_disk(i as u32, 0, NodeType::CharNode4))
                .expect("add child");
        }

        let node = CharNode::N48(Box::new(node48));
        let bytes = char_to_bytes(&node).expect("serialize");
        let restored = char_from_bytes(&bytes).expect("deserialize");

        assert!(matches!(restored, CharNode::N48(_)));
        assert_eq!(restored.header().num_children, 8);

        for &key in &keys {
            assert!(
                restored.find_child(key).is_some(),
                "should find key {}",
                char::from_u32(key).unwrap_or('?')
            );
        }
    }

    #[test]
    fn test_charbucket_roundtrip() {
        let mut bucket = CharBucket::new();

        // Add many children (Unicode + emoji)
        let keys: Vec<u32> = "日本語中文한글🎉🎊🎋🎌🎍🎎🎏🎐🎑🎒🎓"
            .chars()
            .map(|c| c as u32)
            .collect();

        for (i, &key) in keys.iter().enumerate() {
            bucket
                .add_child(key, SwizzledPtr::on_disk(i as u32, 0, NodeType::CharNode4))
                .expect("add child");
        }

        bucket.header.set_final(true);

        let node = CharNode::Bucket(Box::new(bucket));
        let bytes = char_to_bytes(&node).expect("serialize");
        let restored = char_from_bytes(&bytes).expect("deserialize");

        assert!(matches!(restored, CharNode::Bucket(_)));
        assert!(restored.header().is_final());
        assert_eq!(restored.header().num_children, keys.len() as u16);

        for &key in &keys {
            assert!(
                restored.find_child(key).is_some(),
                "should find key {}",
                char::from_u32(key).unwrap_or('?')
            );
        }
    }

    #[test]
    fn test_empty_node_roundtrip() {
        // Test that empty nodes serialize and deserialize correctly
        for create_node in [
            || CharNode::N4(Box::default()),
            || CharNode::N16(Box::default()),
            || CharNode::N48(Box::default()),
            || CharNode::Bucket(Box::default()),
        ] {
            let node = create_node();
            let bytes = char_to_bytes(&node).expect("serialize");
            let restored = char_from_bytes(&bytes).expect("deserialize");
            assert_eq!(restored.header().num_children, 0);
        }
    }

    #[test]
    fn test_serialized_size_calculation() {
        // CharNode4 without prefix: 16 header + 0 prefix + 56 data
        let node4 = CharNode::N4(Box::default());
        assert_eq!(char_serialized_size(&node4), 16 + 56);

        // CharNode4 with prefix: 16 header + 24 prefix + 56 data
        let mut node4_with_prefix = CharNode4::new();
        let prefix: Vec<u32> = "test".chars().map(|c| c as u32).collect();
        node4_with_prefix.prefix = CharCompressedPrefix::from_chars(&prefix);
        node4_with_prefix.header.prefix_len = 4;
        let node4_p = CharNode::N4(Box::new(node4_with_prefix));
        assert_eq!(char_serialized_size(&node4_p), 16 + 24 + 56);

        // CharNode16 without prefix: 16 + 0 + 200
        let node16 = CharNode::N16(Box::default());
        assert_eq!(char_serialized_size(&node16), 16 + 200);

        // CharNode48 without prefix: 16 + 0 + 584
        let node48 = CharNode::N48(Box::default());
        assert_eq!(char_serialized_size(&node48), 16 + 584);

        // CharBucket with 5 entries: 16 + 0 + (4 + 8 + 5*12)
        let mut bucket = CharBucket::new();
        for i in 0..5 {
            bucket
                .add_child(i, SwizzledPtr::on_disk(i, 0, NodeType::CharNode4))
                .expect("add");
        }
        let bucket_node = CharNode::Bucket(Box::new(bucket));
        assert_eq!(char_serialized_size(&bucket_node), 16 + (4 + 8 + 5 * 12));
    }

    #[test]
    fn test_unicode_prefix_roundtrip() {
        let mut node = CharNode4::new();
        let prefix: Vec<u32> = "日本🎉".chars().map(|c| c as u32).collect();
        node.prefix = CharCompressedPrefix::from_chars(&prefix);
        node.header.prefix_len = 3;

        let char_node = CharNode::N4(Box::new(node));
        let bytes = char_to_bytes(&char_node).expect("serialize");
        let restored = char_from_bytes(&bytes).expect("deserialize");

        assert_eq!(restored.header().prefix_len, 3);
        let restored_chars = restored.prefix().to_chars(3);
        assert_eq!(restored_chars, vec!['日', '本', '🎉']);
    }

    #[test]
    fn test_value_ptr_roundtrip() {
        let mut node = CharNode4::new();
        node.value_ptr = SwizzledPtr::on_disk(999, 123, NodeType::Bucket);
        node.header.set_final(true);

        let char_node = CharNode::N4(Box::new(node));
        let bytes = char_to_bytes(&char_node).expect("serialize");
        let restored = char_from_bytes(&bytes).expect("deserialize");

        if let CharNode::N4(n) = restored {
            let loc = n
                .value_ptr
                .disk_location()
                .expect("should have disk location");
            assert_eq!(loc.block_id, 999);
            assert_eq!(loc.offset, 123);
        } else {
            panic!("Expected CharNode::N4");
        }
    }

    // === Compact Encoding Tests ===

    mod compact_tests {
        use super::*;

        #[test]
        fn test_compact_charnode4_roundtrip() {
            let mut node4 = CharNode4::new();
            let prefix_chars: Vec<u32> = "test".chars().map(|c| c as u32).collect();
            node4.prefix = CharCompressedPrefix::from_chars(&prefix_chars);
            node4.header.prefix_len = 4;
            node4.header.set_final(true);

            // Add children
            node4
                .add_child(
                    'a' as u32,
                    SwizzledPtr::on_disk(100, 0, NodeType::CharNode4),
                )
                .expect("add child a");
            node4
                .add_child(
                    'b' as u32,
                    SwizzledPtr::on_disk(200, 0, NodeType::CharNode16),
                )
                .expect("add child b");

            let node = CharNode::N4(Box::new(node4));
            let bytes = char_to_bytes_compact(&node, 1000);
            let restored = char_from_bytes_compact(&bytes).expect("deserialize");

            assert!(matches!(restored, CharNode::N4(_)));
            assert_eq!(restored.header().prefix_len, 4);
            assert!(restored.header().is_final());
            assert_eq!(restored.header().num_children, 2);
            assert!(restored.find_child('a' as u32).is_some());
            assert!(restored.find_child('b' as u32).is_some());
        }

        #[test]
        fn test_compact_charnode16_roundtrip() {
            let mut node16 = CharNode16::new();
            let prefix_chars: Vec<u32> = "prefix".chars().map(|c| c as u32).collect();
            node16.prefix = CharCompressedPrefix::from_chars(&prefix_chars);
            node16.header.prefix_len = 6;

            for i in 0..8 {
                node16
                    .add_child(
                        'a' as u32 + i,
                        SwizzledPtr::on_disk(i, 0, NodeType::CharNode4),
                    )
                    .expect("add child");
            }

            let node = CharNode::N16(Box::new(node16));
            let bytes = char_to_bytes_compact(&node, 1000);
            let restored = char_from_bytes_compact(&bytes).expect("deserialize");

            assert!(matches!(restored, CharNode::N16(_)));
            assert_eq!(restored.header().prefix_len, 6);
            assert_eq!(restored.header().num_children, 8);

            for i in 0..8 {
                assert!(restored.find_child('a' as u32 + i).is_some());
            }
        }

        #[test]
        fn test_compact_charnode48_roundtrip() {
            let mut node48 = CharNode48::new();

            let keys: Vec<u32> = "αβγδεζηθ".chars().map(|c| c as u32).collect();
            for (i, &key) in keys.iter().enumerate() {
                node48
                    .add_child(key, SwizzledPtr::on_disk(i as u32, 0, NodeType::CharNode4))
                    .expect("add child");
            }

            let node = CharNode::N48(Box::new(node48));
            let bytes = char_to_bytes_compact(&node, 1000);
            let restored = char_from_bytes_compact(&bytes).expect("deserialize");

            assert!(matches!(restored, CharNode::N48(_)));
            assert_eq!(restored.header().num_children, 8);

            for &key in &keys {
                assert!(restored.find_child(key).is_some());
            }
        }

        #[test]
        fn test_compact_bucket_roundtrip() {
            let mut bucket = CharBucket::new();

            let keys: Vec<u32> = "日本語中文".chars().map(|c| c as u32).collect();
            for (i, &key) in keys.iter().enumerate() {
                bucket
                    .add_child(key, SwizzledPtr::on_disk(i as u32, 0, NodeType::CharNode4))
                    .expect("add child");
            }

            bucket.header.set_final(true);

            let node = CharNode::Bucket(Box::new(bucket));
            let bytes = char_to_bytes_compact(&node, 1000);
            let restored = char_from_bytes_compact(&bytes).expect("deserialize");

            assert!(matches!(restored, CharNode::Bucket(_)));
            assert!(restored.header().is_final());
            assert_eq!(restored.header().num_children, keys.len() as u16);

            for &key in &keys {
                assert!(restored.find_child(key).is_some());
            }
        }

        #[test]
        fn test_compact_space_savings() {
            // Create a typical node with ASCII keys and small pointers
            let mut node4 = CharNode4::new();
            node4
                .add_child(
                    'a' as u32,
                    SwizzledPtr::on_disk(100, 0, NodeType::CharNode4),
                )
                .expect("add");
            node4
                .add_child(
                    'b' as u32,
                    SwizzledPtr::on_disk(200, 0, NodeType::CharNode4),
                )
                .expect("add");

            let node = CharNode::N4(Box::new(node4));

            // Compare sizes
            let fixed_size = char_serialized_size(&node);
            let compact_size = char_to_bytes_compact(&node, 1000).len();

            // Fixed: 16 + 0 + 56 = 72 bytes
            // Compact: 2 header + 0 prefix + 2*1 keys + 2*2 children = 2 + 2 + 4 = 8 bytes
            assert!(
                compact_size < fixed_size,
                "compact {} should be less than fixed {}",
                compact_size,
                fixed_size
            );

            // Should be at least 50% smaller
            let savings = 1.0 - (compact_size as f64 / fixed_size as f64);
            assert!(
                savings > 0.5,
                "Expected >50% savings, got {:.1}%",
                savings * 100.0
            );
        }

        #[test]
        fn test_compact_empty_nodes() {
            for create_node in [
                || CharNode::N4(Box::default()),
                || CharNode::N16(Box::default()),
                || CharNode::N48(Box::default()),
                || CharNode::Bucket(Box::default()),
            ] {
                let node = create_node();
                let bytes = char_to_bytes_compact(&node, 1000);
                let restored = char_from_bytes_compact(&bytes).expect("deserialize");
                assert_eq!(restored.header().num_children, 0);
            }
        }

        #[test]
        fn test_compact_with_value_ptr() {
            let mut node = CharNode4::new();
            node.value_ptr = SwizzledPtr::on_disk(500, 10, NodeType::Bucket);
            node.header.set_final(true);

            let char_node = CharNode::N4(Box::new(node));
            let bytes = char_to_bytes_compact(&char_node, 1000);
            let restored = char_from_bytes_compact(&bytes).expect("deserialize");

            if let CharNode::N4(n) = restored {
                assert!(n.header.is_final());
                assert!(!n.value_ptr.is_null());
            } else {
                panic!("Expected CharNode::N4");
            }
        }

        #[test]
        fn test_compact_size_calculation() {
            let mut node4 = CharNode4::new();
            node4
                .add_child(
                    'a' as u32,
                    SwizzledPtr::on_disk(100, 0, NodeType::CharNode4),
                )
                .expect("add");
            node4
                .add_child(
                    'b' as u32,
                    SwizzledPtr::on_disk(200, 0, NodeType::CharNode4),
                )
                .expect("add");

            let node = CharNode::N4(Box::new(node4));
            let calculated_size = char_compact_serialized_size(&node, 1000);
            let actual_size = char_to_bytes_compact(&node, 1000).len();

            assert_eq!(
                calculated_size, actual_size,
                "calculated {} != actual {}",
                calculated_size, actual_size
            );
        }

        #[test]
        fn test_compact_unicode_prefix() {
            let mut node = CharNode4::new();
            let prefix: Vec<u32> = "日本🎉".chars().map(|c| c as u32).collect();
            node.prefix = CharCompressedPrefix::from_chars(&prefix);
            node.header.prefix_len = 3;

            let char_node = CharNode::N4(Box::new(node));
            let bytes = char_to_bytes_compact(&char_node, 1000);
            let restored = char_from_bytes_compact(&bytes).expect("deserialize");

            assert_eq!(restored.header().prefix_len, 3);
            let restored_chars = restored.prefix().to_chars(3);
            assert_eq!(restored_chars, vec!['日', '本', '🎉']);
        }

        #[test]
        fn test_compact_large_pointers() {
            // Test with large pointer values that require more bytes
            // Note: block_id is 23 bits max (0x7FFFFF = 8,388,607)
            //       offset is 22 bits max (0x3FFFFF = 4,194,303)
            let mut node4 = CharNode4::new();
            node4
                .add_child(
                    'a' as u32,
                    SwizzledPtr::on_disk(0x7FFFFF, 0x3FFFFF, NodeType::CharNode4),
                )
                .expect("add");

            let node = CharNode::N4(Box::new(node4));
            // Use a max_offset that requires larger ptr_width
            let bytes = char_to_bytes_compact(&node, 0xFFFFFFFF);
            let restored = char_from_bytes_compact(&bytes).expect("deserialize");

            assert!(matches!(restored, CharNode::N4(_)));
            assert!(restored.find_child('a' as u32).is_some());
        }
    }

    // =============================================================================
    // V2 Serialization Tests (Relative Offsets and Sequential Siblings)
    // =============================================================================

    mod v2_tests {
        use super::*;
        use proptest::prelude::*;

        #[derive(Default)]
        struct OneByteWriter(Vec<u8>);

        impl std::io::Write for OneByteWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(&byte) = bytes.first() {
                    self.0.push(byte);
                    Ok(1)
                } else {
                    Ok(0)
                }
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        fn child_node_type(node: &CharNode, key: char) -> NodeType {
            node.find_child(key as u32)
                .and_then(SwizzledPtr::disk_location)
                .map(|location| location.node_type)
                .expect("restored child must be an exact disk reference")
        }

        #[test]
        fn v3_streaming_is_exact_for_partial_writers() {
            let parent = ArenaSlot::new(0, 1_000);
            let types = [
                NodeType::CharNode4,
                NodeType::CharNode16,
                NodeType::CharNode48,
                NodeType::CharBucket,
            ];
            let mut node = CharNode4::new();
            for (index, node_type) in types.into_iter().enumerate() {
                node.add_child(
                    'a' as u32 + index as u32,
                    SwizzledPtr::on_disk(1, 900 + index as u32, node_type),
                )
                .expect("add streaming regression child");
            }
            let node = CharNode::N4(Box::new(node));
            let context = SerializationContext::new(parent);

            let mut contiguous = Vec::new();
            let contiguous_len = serialize_char_node_v2(&node, &mut contiguous, &context).unwrap();
            let mut fragmented = OneByteWriter::default();
            let fragmented_len = serialize_char_node_v2(&node, &mut fragmented, &context).unwrap();

            assert_eq!(fragmented_len, contiguous_len);
            assert_eq!(fragmented.0, contiguous);
        }

        fn dense_node4_location_range(bytes: &[u8], parent: ArenaSlot) -> std::ops::Range<usize> {
            let header = SerializedCharNodeHeader::from_bytes(
                bytes[..CHAR_SERIALIZED_HEADER_SIZE]
                    .try_into()
                    .expect("complete header"),
            );
            assert_eq!(header.node_type, char_node_types::CHARNODE4);
            assert!(header.uses_relative_offsets());
            let children_start = CHAR_SERIALIZED_HEADER_SIZE + header_prefix_size(&header) + 4 * 4;
            let structural_end = CHAR_SERIALIZED_HEADER_SIZE + header.data_size as usize;
            let remaining = &bytes[children_start..structural_end];
            let (_, locations_end, _) = decode_char_v3_locations(
                remaining,
                parent,
                header.num_children as usize,
                header.uses_sequential_siblings(),
                "test char node4",
            )
            .expect("decode child locations");
            children_start..children_start + locations_end
        }

        fn rewrite_header(bytes: &mut [u8], update: impl FnOnce(&mut SerializedCharNodeHeader)) {
            let mut header = SerializedCharNodeHeader::from_bytes(
                bytes[..CHAR_SERIALIZED_HEADER_SIZE]
                    .try_into()
                    .expect("complete header"),
            );
            update(&mut header);
            bytes[..CHAR_SERIALIZED_HEADER_SIZE].copy_from_slice(&header.to_bytes());
        }

        #[test]
        fn test_header_v2_encoding_flags() {
            let header = CharNodeHeader::new(char_node_types::CHARNODE4);

            // Test with no encoding flags
            let h1 = SerializedCharNodeHeader::from_node_header_v2(&header, 100, 0);
            assert!(!h1.uses_relative_offsets());
            assert!(!h1.uses_sequential_siblings());

            // Test with relative offsets flag
            let h2 = SerializedCharNodeHeader::from_node_header_v2(&header, 100, 0x80);
            assert!(h2.uses_relative_offsets());
            assert!(!h2.uses_sequential_siblings());

            // Test with sequential siblings flag
            let h3 = SerializedCharNodeHeader::from_node_header_v2(&header, 100, 0x40);
            assert!(!h3.uses_relative_offsets());
            assert!(h3.uses_sequential_siblings());

            // Test with both flags
            let h4 = SerializedCharNodeHeader::from_node_header_v2(&header, 100, 0xC0);
            assert!(h4.uses_relative_offsets());
            assert!(h4.uses_sequential_siblings());

            let v3 = SerializedCharNodeHeader::from_node_header_v3(
                &header,
                100,
                FLAG_RELATIVE_OFFSETS | FLAG_HOMOGENEOUS_CHILD_TYPES,
                1,
            );
            assert_eq!(v3.version, CHAR_FORMAT_VERSION_V3);
            assert!(v3.uses_relative_offsets());
            assert!(v3.uses_homogeneous_child_types());

            let mut typed_without_relative = v3;
            typed_without_relative.flags &= !FLAG_RELATIVE_OFFSETS;
            assert!(typed_without_relative.validate().is_err());
        }

        #[test]
        fn test_header_v2_preserves_node_flags() {
            let mut header = CharNodeHeader::new(char_node_types::CHARNODE4);
            header.flags = flags::IS_FINAL | flags::IS_DIRTY; // bits 0 and 1

            // Encoding flags should combine with node flags
            let h = SerializedCharNodeHeader::from_node_header_v2(&header, 100, 0xC0);

            // Node flags preserved
            assert!(h.flags & flags::IS_FINAL != 0);
            assert!(h.flags & flags::IS_DIRTY != 0);

            // Encoding flags present
            assert!(h.uses_relative_offsets());
            assert!(h.uses_sequential_siblings());
        }

        #[test]
        fn test_serialize_charnode4_v2_relative() {
            // Test v2 serialization with relative offsets
            let mut node4 = CharNode4::new();

            // Add children with disk pointers in same arena (arena_id = 0)
            // block_id = arena_id + 1 = 1
            node4
                .add_child('a' as u32, SwizzledPtr::on_disk(1, 10, NodeType::CharNode4))
                .expect("add child a");
            node4
                .add_child(
                    'b' as u32,
                    SwizzledPtr::on_disk(1, 20, NodeType::CharNode16),
                )
                .expect("add child b");
            node4
                .add_child(
                    'c' as u32,
                    SwizzledPtr::on_disk(1, 30, NodeType::CharNode48),
                )
                .expect("add child c");
            node4
                .add_child(
                    'd' as u32,
                    SwizzledPtr::on_disk(1, 40, NodeType::CharBucket),
                )
                .expect("add child d");

            let node = CharNode::N4(Box::new(node4));

            // Parent at slot 100 in arena 0
            let parent_slot = ArenaSlot::new(0, 100);
            let ctx = SerializationContext::new(parent_slot);

            let mut buffer = Vec::new();
            let bytes_written =
                serialize_char_node_v2(&node, &mut buffer, &ctx).expect("serialize");

            assert!(bytes_written > 0);

            // Check that header has relative offsets flag
            let header = SerializedCharNodeHeader::from_bytes(buffer[..16].try_into().unwrap());
            assert_eq!(header.version, CHAR_FORMAT_VERSION_V3);
            assert!(header.uses_relative_offsets());
            assert!(!header.uses_sequential_siblings());

            // Deserialize and verify
            let deser_ctx = DeserializationContext::new(parent_slot);
            let mut cursor = std::io::Cursor::new(&buffer);
            let restored = deserialize_char_node_v2(&mut cursor, &deser_ctx).expect("deserialize");

            assert!(matches!(restored, CharNode::N4(_)));
            assert_eq!(restored.header().num_children, 4);
            assert_eq!(child_node_type(&restored, 'a'), NodeType::CharNode4);
            assert_eq!(child_node_type(&restored, 'b'), NodeType::CharNode16);
            assert_eq!(child_node_type(&restored, 'c'), NodeType::CharNode48);
            assert_eq!(child_node_type(&restored, 'd'), NodeType::CharBucket);
        }

        #[test]
        fn test_serialize_charnode4_v2_sequential() {
            // Test v2 serialization with sequential siblings
            let mut node4 = CharNode4::new();

            // Add children with consecutive slots in same arena
            node4
                .add_child('a' as u32, SwizzledPtr::on_disk(1, 10, NodeType::CharNode4))
                .expect("add child a");
            node4
                .add_child(
                    'b' as u32,
                    SwizzledPtr::on_disk(1, 11, NodeType::CharNode16),
                )
                .expect("add child b");
            node4
                .add_child(
                    'c' as u32,
                    SwizzledPtr::on_disk(1, 12, NodeType::CharNode48),
                )
                .expect("add child c");
            node4
                .add_child(
                    'd' as u32,
                    SwizzledPtr::on_disk(1, 13, NodeType::CharBucket),
                )
                .expect("add child d");

            let node = CharNode::N4(Box::new(node4));

            // Parent at slot 100, first child at slot 10
            let parent_slot = ArenaSlot::new(0, 100);
            let first_child_slot = ArenaSlot::new(0, 10);
            let ctx = SerializationContext::sequential(parent_slot, first_child_slot);

            let mut buffer = Vec::new();
            let bytes_written =
                serialize_char_node_v2(&node, &mut buffer, &ctx).expect("serialize");

            assert!(bytes_written > 0);

            // Check that header has both flags set
            let header = SerializedCharNodeHeader::from_bytes(buffer[..16].try_into().unwrap());
            assert_eq!(header.version, CHAR_FORMAT_VERSION_V3);
            assert!(header.uses_relative_offsets());
            assert!(header.uses_sequential_siblings());

            // Deserialize and verify
            let deser_ctx = DeserializationContext::new(parent_slot);
            let mut cursor = std::io::Cursor::new(&buffer);
            let restored = deserialize_char_node_v2(&mut cursor, &deser_ctx).expect("deserialize");

            assert!(matches!(restored, CharNode::N4(_)));
            assert_eq!(restored.header().num_children, 4);
            assert_eq!(child_node_type(&restored, 'a'), NodeType::CharNode4);
            assert_eq!(child_node_type(&restored, 'b'), NodeType::CharNode16);
            assert_eq!(child_node_type(&restored, 'c'), NodeType::CharNode48);
            assert_eq!(child_node_type(&restored, 'd'), NodeType::CharBucket);
        }

        proptest! {
            #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

            #[test]
            fn generated_relative_layouts_preserve_every_child_type(
                child_types in prop::collection::vec(
                    prop_oneof![
                        Just(NodeType::CharNode4),
                        Just(NodeType::CharNode16),
                        Just(NodeType::CharNode48),
                        Just(NodeType::CharBucket),
                    ],
                    1..=4,
                ),
                first_slot in 1u32..10_000,
                sequential in any::<bool>(),
            ) {
                let mut node = CharNode4::new();
                for (index, node_type) in child_types.iter().copied().enumerate() {
                    let offset = if sequential { index as u32 } else { index as u32 * 3 };
                    node.add_child(
                        'a' as u32 + index as u32,
                        SwizzledPtr::on_disk(1, first_slot + offset, node_type),
                    )
                    .expect("add generated child");
                }
                let parent = ArenaSlot::new(0, first_slot + 100);
                let context = if sequential {
                    SerializationContext::sequential(parent, ArenaSlot::new(0, first_slot))
                } else {
                    SerializationContext::new(parent)
                };
                let mut bytes = Vec::new();
                serialize_char_node_v2(&CharNode::N4(Box::new(node)), &mut bytes, &context)
                    .expect("serialize generated typed layout");
                let restored = deserialize_char_node_v2(
                    &mut std::io::Cursor::new(bytes),
                    &DeserializationContext::new(parent),
                )
                .expect("deserialize generated typed layout");
                for (index, expected) in child_types.iter().copied().enumerate() {
                    let key = char::from_u32('a' as u32 + index as u32).expect("test key");
                    prop_assert_eq!(child_node_type(&restored, key), expected);
                }
            }
        }

        #[test]
        fn v3_child_types_reject_noncanonical_header_bits_and_location_lengths() {
            let parent = ArenaSlot::new(0, 100);
            let mut node = CharNode4::new();
            node.add_child(
                'a' as u32,
                SwizzledPtr::on_disk(1, 90, NodeType::CharNode16),
            )
            .expect("add typed child");
            let mut canonical = Vec::new();
            serialize_char_node_v2(
                &CharNode::N4(Box::new(node)),
                &mut canonical,
                &SerializationContext::new(parent),
            )
            .expect("serialize typed record");
            let location = dense_node4_location_range(&canonical, parent);

            let mut noncanonical_header = canonical.clone();
            rewrite_header(&mut noncanonical_header, |header| header.reserved |= 0b100);
            assert!(deserialize_char_node_v2(
                &mut std::io::Cursor::new(noncanonical_header),
                &DeserializationContext::new(parent),
            )
            .is_err());

            let mut missing = canonical.clone();
            missing.remove(location.start);
            rewrite_header(&mut missing, |header| header.data_size -= 1);
            assert!(deserialize_char_node_v2(
                &mut std::io::Cursor::new(missing),
                &DeserializationContext::new(parent),
            )
            .is_err());

            let mut extra = canonical;
            extra.insert(location.end, 0);
            rewrite_header(&mut extra, |header| header.data_size += 1);
            assert!(deserialize_char_node_v2(
                &mut std::io::Cursor::new(extra),
                &DeserializationContext::new(parent),
            )
            .is_err());
        }

        #[test]
        fn legacy_untyped_relative_charnode4_record_remains_readable() {
            let parent = ArenaSlot::new(0, 100);
            let mut node = CharNode4::new();
            node.add_child('a' as u32, SwizzledPtr::on_disk(1, 90, NodeType::CharNode4))
                .expect("add legacy-compatible child");
            let mut legacy = Vec::new();
            serialize_char_node_v2(
                &CharNode::N4(Box::new(node)),
                &mut legacy,
                &SerializationContext::new(parent),
            )
            .expect("serialize source record");
            rewrite_header(&mut legacy, |header| {
                header.version = CHAR_FORMAT_VERSION_V2;
                header.reserved = 0;
                header._padding = 0;
                header.flags &= V2_ENCODING_FLAGS_MASK | NODE_FLAGS_MASK;
            });

            let restored = deserialize_char_node_v2(
                &mut std::io::Cursor::new(&legacy),
                &DeserializationContext::new(parent),
            )
            .expect("read legacy untyped record");
            assert_eq!(child_node_type(&restored, 'a'), NodeType::CharNode4);

            legacy.extend_from_slice(&0u32.to_le_bytes());
            let metadata = decode_char_node_metadata(
                &legacy,
                &DeserializationContext::new(parent),
                Some(NodeType::CharNode4),
            )
            .expect("scan legacy untyped record");
            assert!(matches!(
                metadata.children.as_slice(),
                [(
                    key,
                    DecodedCharMetadataChild::Untyped(ArenaSlot {
                        arena_id: 0,
                        slot_id: 90
                    })
                )] if *key == 'a' as u32
            ));
        }

        #[test]
        fn legacy_v2_target_headers_restore_heterogeneous_child_types() {
            let parent = ArenaSlot::new(0, 100);
            let expected = [
                NodeType::CharNode4,
                NodeType::CharNode16,
                NodeType::CharNode48,
                NodeType::CharBucket,
            ];
            let mut node = CharNode4::new();
            for (index, node_type) in expected.iter().copied().enumerate() {
                node.add_child(
                    'a' as u32 + index as u32,
                    SwizzledPtr::on_disk(1, 90 + index as u32, node_type),
                )
                .expect("add heterogeneous legacy child");
            }
            let mut legacy = Vec::new();
            serialize_char_node_v2(
                &CharNode::N4(Box::new(node)),
                &mut legacy,
                &SerializationContext::new(parent),
            )
            .expect("serialize migration source");
            rewrite_header(&mut legacy, |header| {
                header.version = CHAR_FORMAT_VERSION_V2;
                header.reserved = 0;
                header._padding = 0;
                header.flags &= V2_ENCODING_FLAGS_MASK | NODE_FLAGS_MASK;
            });

            let (mut restored, header) = deserialize_char_node_record(
                &mut std::io::Cursor::new(&legacy),
                &DeserializationContext::new(parent),
            )
            .expect("decode type-erasing V2 parent");
            for index in 0..expected.len() {
                assert_eq!(
                    child_node_type(
                        &restored,
                        char::from_u32('a' as u32 + index as u32).unwrap()
                    ),
                    NodeType::CharNode4
                );
            }

            resolve_legacy_v2_child_types(&header, &mut restored, |slot| {
                let node_type = expected[(slot.slot_id - 90) as usize];
                let target = SerializedCharNodeHeader::from_node_header(
                    &CharNodeHeader::new(node_type as u8),
                    0,
                )
                .to_bytes();
                char_record_node_type(&target)
            })
            .expect("resolve authoritative target headers");
            for (index, expected_type) in expected.iter().copied().enumerate() {
                assert_eq!(
                    child_node_type(
                        &restored,
                        char::from_u32('a' as u32 + index as u32).unwrap()
                    ),
                    expected_type
                );
            }
        }

        #[test]
        fn v3_packed_n16_types_use_two_payload_bytes_and_roundtrip() {
            let parent = ArenaSlot::new(0, 1_000);
            let expected = [
                NodeType::CharNode4,
                NodeType::CharNode16,
                NodeType::CharNode48,
                NodeType::CharBucket,
            ];
            let mut node = CharNode16::new();
            for index in 0..16 {
                node.add_child(
                    'a' as u32 + index as u32,
                    SwizzledPtr::on_disk(1, 900 + index as u32, expected[index % 4]),
                )
                .expect("add packed V3 child");
            }
            let mut bytes = Vec::new();
            serialize_char_node_v2(
                &CharNode::N16(Box::new(node)),
                &mut bytes,
                &SerializationContext::new(parent),
            )
            .expect("serialize packed V3 node16");
            let header = SerializedCharNodeHeader::from_bytes(bytes[..16].try_into().unwrap());
            assert_eq!(header.version, CHAR_FORMAT_VERSION_V3);
            assert!(!header.uses_homogeneous_child_types());
            let children_start = CHAR_SERIALIZED_HEADER_SIZE + 16 * 4;
            let structural_end = CHAR_SERIALIZED_HEADER_SIZE + header.data_size as usize;
            let encoded = &bytes[children_start..structural_end - 8];
            let (_, locations_end, _) =
                decode_char_v3_locations(encoded, parent, 16, false, "packed node16 regression")
                    .unwrap();
            assert_eq!(encoded.len() - locations_end, 2);

            let restored = deserialize_char_node_v2(
                &mut std::io::Cursor::new(&bytes),
                &DeserializationContext::new(parent),
            )
            .expect("deserialize packed V3 node16");
            for index in 0..16 {
                assert_eq!(
                    child_node_type(
                        &restored,
                        char::from_u32('a' as u32 + index as u32).unwrap()
                    ),
                    expected[index % 4]
                );
            }
        }

        #[test]
        fn v3_homogeneous_bucket_has_zero_type_payload() {
            let parent = ArenaSlot::new(0, 10_000);
            let mut node = CharBucket::new();
            for index in 0..49 {
                node.add_child(
                    0x100 + index,
                    SwizzledPtr::on_disk(1, 9_000 + index, NodeType::CharNode48),
                )
                .expect("add homogeneous bucket child");
            }
            let mut bytes = Vec::new();
            serialize_char_node_v2(
                &CharNode::Bucket(Box::new(node)),
                &mut bytes,
                &SerializationContext::new(parent),
            )
            .expect("serialize homogeneous V3 bucket");
            let header = SerializedCharNodeHeader::from_bytes(bytes[..16].try_into().unwrap());
            assert!(header.uses_homogeneous_child_types());
            assert_eq!(header.child_type_extension(), 2);
            let children_start = CHAR_SERIALIZED_HEADER_SIZE + 12 + 49 * 4;
            let structural_end = CHAR_SERIALIZED_HEADER_SIZE + header.data_size as usize;
            let encoded = &bytes[children_start..structural_end];
            let (_, locations_end, _) = decode_char_v3_locations(
                encoded,
                parent,
                49,
                false,
                "homogeneous bucket regression",
            )
            .unwrap();
            assert_eq!(locations_end, encoded.len());

            let restored = deserialize_char_node_v2(
                &mut std::io::Cursor::new(&bytes),
                &DeserializationContext::new(parent),
            )
            .expect("deserialize homogeneous V3 bucket");
            assert!(restored.child_cursor().all(|(_, child)| child
                .disk_location()
                .unwrap()
                .node_type
                == NodeType::CharNode48));
        }

        #[test]
        fn v3_node48_and_typed_full_tags_roundtrip_exact_types() {
            let parent = ArenaSlot::new(0, 1_000);
            let expected = [
                NodeType::CharNode4,
                NodeType::CharNode16,
                NodeType::CharNode48,
                NodeType::CharBucket,
            ];
            let mut node48 = CharNode48::new();
            for index in 0..17 {
                node48
                    .add_child(
                        0x400 + index as u32,
                        SwizzledPtr::on_disk(
                            1,
                            800 + index as u32,
                            expected[index % expected.len()],
                        ),
                    )
                    .expect("add node48 child");
            }
            let mut node48_bytes = Vec::new();
            serialize_char_node_v2(
                &CharNode::N48(Box::new(node48)),
                &mut node48_bytes,
                &SerializationContext::new(parent),
            )
            .expect("serialize V3 node48");
            let restored = deserialize_char_node_v2(
                &mut std::io::Cursor::new(node48_bytes),
                &DeserializationContext::new(parent),
            )
            .expect("deserialize V3 node48");
            for index in 0..17 {
                let location = restored
                    .find_child(0x400 + index as u32)
                    .and_then(SwizzledPtr::disk_location)
                    .unwrap();
                assert_eq!(location.node_type, expected[index % expected.len()]);
            }

            let mut node4 = CharNode4::new();
            for (index, node_type) in expected.iter().copied().enumerate() {
                node4
                    .add_child(
                        'a' as u32 + index as u32,
                        SwizzledPtr::on_disk(2 + index as u32, 10 + index as u32, node_type),
                    )
                    .expect("add full-tag child");
            }
            let mut full_bytes = Vec::new();
            serialize_char_node_v2(
                &CharNode::N4(Box::new(node4)),
                &mut full_bytes,
                &SerializationContext::new(parent),
            )
            .expect("serialize typed full tags");
            let children_start = CHAR_SERIALIZED_HEADER_SIZE + 4 * 4;
            assert_eq!(
                [
                    full_bytes[children_start],
                    full_bytes[children_start + 9],
                    full_bytes[children_start + 18],
                    full_bytes[children_start + 27],
                ],
                [1, 3, 5, 7]
            );
            let restored = deserialize_char_node_v2(
                &mut std::io::Cursor::new(&full_bytes),
                &DeserializationContext::new(parent),
            )
            .expect("deserialize typed full tags");
            for (index, expected_type) in expected.iter().copied().enumerate() {
                assert_eq!(
                    child_node_type(
                        &restored,
                        char::from_u32('a' as u32 + index as u32).unwrap()
                    ),
                    expected_type
                );
            }

            full_bytes[children_start] = 9;
            assert!(deserialize_char_node_v2(
                &mut std::io::Cursor::new(full_bytes),
                &DeserializationContext::new(parent),
            )
            .is_err());
        }

        #[test]
        fn v3_locations_reject_unrepresentable_packed_coordinates() {
            let parent = ArenaSlot::new(0, 1_000);

            let mut oversized_block = vec![1];
            oversized_block.extend_from_slice(&MAX_BLOCK_ID.to_le_bytes());
            oversized_block.extend_from_slice(&0u32.to_le_bytes());
            assert!(decode_char_v3_location(&oversized_block, parent).is_err());

            let mut oversized_offset = vec![1];
            oversized_offset.extend_from_slice(&0u32.to_le_bytes());
            oversized_offset.extend_from_slice(&(MAX_OFFSET + 1).to_le_bytes());
            assert!(decode_char_v3_location(&oversized_offset, parent).is_err());

            let mut overflowing_sequence = vec![1];
            overflowing_sequence.extend_from_slice(&0u32.to_le_bytes());
            overflowing_sequence.extend_from_slice(&MAX_OFFSET.to_le_bytes());
            assert!(decode_char_v3_locations(
                &overflowing_sequence,
                parent,
                2,
                true,
                "sequential packed-coordinate regression",
            )
            .is_err());
        }

        #[test]
        fn v3_packed_payload_rejects_nonzero_unused_high_bits() {
            let parent = ArenaSlot::new(0, 1_000);
            let mut node = CharNode16::new();
            for index in 0..9 {
                node.add_child(
                    'a' as u32 + index,
                    SwizzledPtr::on_disk(1, 900 + index, NodeType::CharNode16),
                )
                .expect("add packed-padding child");
            }
            let mut bytes = Vec::new();
            serialize_char_node_v2(
                &CharNode::N16(Box::new(node)),
                &mut bytes,
                &SerializationContext::new(parent),
            )
            .expect("serialize packed-padding record");
            let header = SerializedCharNodeHeader::from_bytes(bytes[..16].try_into().unwrap());
            assert!(header.uses_homogeneous_child_types());

            // Force packed mode with the same canonical type vector, then set
            // one unused high bit in its single payload byte.
            rewrite_header(&mut bytes, |header| {
                header.flags &= !FLAG_HOMOGENEOUS_CHILD_TYPES;
                header.reserved = 0x55;
                header._padding = 0x55;
                header.data_size += 1;
            });
            let insert_at = bytes.len() - 8;
            bytes.insert(insert_at, 0b0000_0101);
            assert!(deserialize_char_node_v2(
                &mut std::io::Cursor::new(bytes),
                &DeserializationContext::new(parent),
            )
            .is_err());
        }

        #[test]
        fn v3_header_type_extension_exhaustively_roundtrips_eight_codes() {
            let parent = ArenaSlot::new(0, 100);
            let locations = [0u8; 8];
            let mut node_header = CharNodeHeader::new(char_node_types::CHARNODE16);
            node_header.num_children = 8;
            for extension in u16::MIN..=u16::MAX {
                let header = SerializedCharNodeHeader::from_node_header_v3(
                    &node_header,
                    0,
                    FLAG_RELATIVE_OFFSETS,
                    extension,
                );
                let mut seen = 0usize;
                let consumed = decode_char_v3_children(
                    &header,
                    &locations,
                    parent,
                    8,
                    false,
                    "exhaustive header type extension",
                    |index, _, node_type| {
                        assert_eq!(
                            char_node_type_code(node_type).unwrap(),
                            ((extension >> (2 * index)) & 0b11) as u8
                        );
                        seen += 1;
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(consumed, locations.len());
                assert_eq!(seen, 8);
            }
        }

        #[test]
        fn test_collect_char_child_slots() {
            let mut node4 = CharNode4::new();

            // Add children
            node4
                .add_child('x' as u32, SwizzledPtr::on_disk(1, 50, NodeType::CharNode4))
                .expect("add");
            node4
                .add_child('y' as u32, SwizzledPtr::on_disk(1, 60, NodeType::CharNode4))
                .expect("add");

            let node = CharNode::N4(Box::new(node4));
            let slots = collect_char_child_slots(&node);

            assert_eq!(slots.len(), 2);
            // Check that slots were extracted correctly
            assert!(slots.iter().any(|s| s.arena_id == 0 && s.slot_id == 50));
            assert!(slots.iter().any(|s| s.arena_id == 0 && s.slot_id == 60));
        }

        #[test]
        fn test_v2_size_smaller_than_v1() {
            // V2 format should be smaller when using relative offsets
            let mut node4 = CharNode4::new();

            // Add children in same arena with small deltas
            for i in 0..4 {
                node4
                    .add_child(
                        ('a' as u32) + i,
                        SwizzledPtr::on_disk(1, 10 + i, NodeType::CharNode4),
                    )
                    .expect("add");
            }

            let node = CharNode::N4(Box::new(node4));

            // V1 (fixed 8-byte pointers)
            let mut v1_buffer = Vec::new();
            serialize_char_node(&node, &mut v1_buffer).expect("v1");

            // V2 (relative offsets, small deltas)
            let parent_slot = ArenaSlot::new(0, 100);
            let ctx = SerializationContext::new(parent_slot);
            let mut v2_buffer = Vec::new();
            serialize_char_node_v2(&node, &mut v2_buffer, &ctx).expect("v2");

            // V2 should be smaller (relative offsets of ~90 encode to 1-2 bytes each)
            // V1: 4 children * 8 bytes = 32 bytes for pointers
            // V2: 4 children * ~2 bytes = ~8 bytes for pointers
            assert!(
                v2_buffer.len() <= v1_buffer.len(),
                "V2 size {} should be <= V1 size {}",
                v2_buffer.len(),
                v1_buffer.len()
            );
        }

        fn metadata_relative_record() -> (ArenaSlot, Vec<u8>) {
            let parent = ArenaSlot::new(0, 100);
            let mut node = CharNode4::new();
            node.header.prefix_len = 1;
            node.prefix = CharCompressedPrefix::from_chars(&['λ' as u32]);
            node.add_child(
                'a' as u32,
                SwizzledPtr::on_disk(1, 90, NodeType::CharNode16),
            )
            .expect("add metadata child a");
            node.add_child(
                'β' as u32,
                SwizzledPtr::on_disk(1, 91, NodeType::CharNode48),
            )
            .expect("add metadata child beta");
            let mut bytes = Vec::new();
            serialize_char_node_v2(
                &CharNode::N4(Box::new(node)),
                &mut bytes,
                &SerializationContext::new(parent),
            )
            .expect("serialize relative metadata fixture");
            bytes.extend_from_slice(&0u32.to_le_bytes());
            (parent, bytes)
        }

        #[test]
        fn metadata_decoder_preserves_typed_relative_child_addresses() {
            let (parent, bytes) = metadata_relative_record();
            let metadata = decode_char_node_metadata(
                &bytes,
                &DeserializationContext::new(parent),
                Some(NodeType::CharNode4),
            )
            .expect("decode relative char metadata");

            assert_eq!(metadata.node_type, NodeType::CharNode4);
            assert_eq!(metadata.serialized_bytes, bytes.len());
            assert_eq!(metadata.prefix, vec!['λ' as u32]);
            assert_eq!(metadata.children.len(), 2);
            assert_eq!(metadata.children[0].0, 'a' as u32);
            assert_eq!(metadata.children[1].0, 'β' as u32);
            let child_types: Vec<_> = metadata
                .children
                .iter()
                .map(|(_, child)| match child {
                    DecodedCharMetadataChild::Typed(pointer) => {
                        pointer
                            .disk_location()
                            .expect("typed metadata child location")
                            .node_type
                    }
                    DecodedCharMetadataChild::Untyped(_) => {
                        panic!("new relative record erased a child node type")
                    }
                })
                .collect();
            assert_eq!(
                child_types,
                vec![NodeType::CharNode16, NodeType::CharNode48]
            );
        }

        #[test]
        fn metadata_decoder_preserves_typed_fixed_children() {
            let parent = ArenaSlot::new(4, 7);
            let child = SwizzledPtr::on_disk(3, 11, NodeType::CharNode16);
            let expected_raw = child.to_raw();
            let mut node = CharNode4::new();
            node.add_child('δ' as u32, child)
                .expect("add fixed metadata child");
            let mut bytes = Vec::new();
            serialize_char_node_v2(
                &CharNode::N4(Box::new(node)),
                &mut bytes,
                &SerializationContext::default(),
            )
            .expect("serialize fixed metadata fixture");
            bytes.extend_from_slice(&0u32.to_le_bytes());

            let metadata = decode_char_node_metadata(
                &bytes,
                &DeserializationContext::new(parent),
                Some(NodeType::CharNode4),
            )
            .expect("decode fixed char metadata");
            match &metadata.children[0].1 {
                DecodedCharMetadataChild::Typed(pointer) => {
                    assert_eq!(pointer.to_raw(), expected_raw)
                }
                DecodedCharMetadataChild::Untyped(_) => {
                    panic!("fixed char child lost its encoded node type")
                }
            }
        }

        #[test]
        fn metadata_decoder_rejects_every_truncation_trailing_data_and_invalid_scalars() {
            let (parent, bytes) = metadata_relative_record();
            let context = DeserializationContext::new(parent);
            for end in 0..bytes.len() {
                assert!(
                    decode_char_node_metadata(&bytes[..end], &context, None).is_err(),
                    "char metadata truncation at byte {end} was accepted"
                );
            }
            assert!(decode_char_node_metadata(&bytes, &context, None).is_ok());

            let mut trailing = bytes.clone();
            trailing.push(0);
            assert!(decode_char_node_metadata(&trailing, &context, None).is_err());

            let mut invalid_prefix = bytes;
            invalid_prefix[CHAR_SERIALIZED_HEADER_SIZE..CHAR_SERIALIZED_HEADER_SIZE + 4]
                .copy_from_slice(&0x0000_d800u32.to_le_bytes());
            assert!(decode_char_node_metadata(&invalid_prefix, &context, None).is_err());
        }

        #[test]
        fn metadata_decoder_never_panics_on_bounded_arbitrary_bytes() {
            let context = DeserializationContext::new(ArenaSlot::new(0, 23));
            let mut state = 0xd1b5_4a32_d192_ed03u64;
            for len in 0..=768usize {
                let mut bytes = Vec::with_capacity(len);
                for _ in 0..len {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    bytes.push(state as u8);
                }
                let _ = decode_char_node_metadata(&bytes, &context, None);
            }
        }
    }
}
