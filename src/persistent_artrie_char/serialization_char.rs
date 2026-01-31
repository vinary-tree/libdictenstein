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
//! ## CharNode4
//! ```text
//! │ keys: [u32; 4]        │ 16 bytes                                   │
//! │ children: [u64; 4]    │ 32 bytes (SwizzledPtr as u64)              │
//! │ value_ptr: u64        │ 8 bytes                                    │
//! Total: 56 bytes + header
//! ```
//!
//! ## CharNode16
//! ```text
//! │ keys: [u32; 16]       │ 64 bytes                                   │
//! │ children: [u64; 16]   │ 128 bytes (SwizzledPtr as u64)             │
//! │ value_ptr: u64        │ 8 bytes                                    │
//! Total: 200 bytes + header
//! ```
//!
//! ## CharNode48
//! ```text
//! │ keys: [u32; 48]       │ 192 bytes (sorted for binary search)       │
//! │ children: [u64; 48]   │ 384 bytes (SwizzledPtr as u64)             │
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

use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;

use super::nodes::{
    CharBucket, CharCompressedPrefix, CharNode, CharNode16, CharNode4, CharNode48,
    CharNodeHeader, CHAR_MAX_PREFIX_LEN,
};

use super::compact_encoding::{
    decode_compact_node, encode_compact_node, determine_key_width, determine_ptr_width,
    CompactHeader, DecodedCompactNode, COMPACT_NODE_TYPE_BUCKET, COMPACT_NODE_TYPE_N16,
    COMPACT_NODE_TYPE_N4, COMPACT_NODE_TYPE_N48,
};

use super::arena_manager::ArenaSlot;

use super::relative_encoding::{
    SerializationContext, encode_child_pointer, encode_sequential_siblings,
    decode_children, decode_sequential_siblings,
};

/// Helper to convert io::Error to PersistentARTrieError for serialization operations
fn io_err(e: std::io::Error) -> PersistentARTrieError {
    PersistentARTrieError::io_error("char serialization", "<buffer>", e)
}

/// Magic bytes identifying a char ART node in the serialized format
pub const CHAR_NODE_MAGIC: [u8; 4] = *b"ARC\0"; // ART + Char

/// Current serialization format version for char nodes
pub const CHAR_FORMAT_VERSION: u8 = 2;

/// Serialized header size in bytes
pub const CHAR_SERIALIZED_HEADER_SIZE: usize = 16;

/// Char node type discriminants for serialization
pub mod char_node_types {
    pub const CHARNODE4: u8 = 104;
    pub const CHARNODE16: u8 = 116;
    pub const CHARNODE48: u8 = 148;
    pub const CHARBUCKET: u8 = 101;
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
            version: CHAR_FORMAT_VERSION,
            node_type: header.node_type,
            flags: header.flags,
            reserved: 0,
            num_children: header.num_children,
            prefix_len: header.prefix_len,
            _padding: 0,
            data_size,
        }
    }

    /// Create a header with encoding flags (v2 format)
    ///
    /// The encoding_flags parameter contains:
    /// - Bit 7 (0x80): FLAG_RELATIVE_OFFSETS - children use relative offset encoding
    /// - Bit 6 (0x40): FLAG_SEQUENTIAL_SIBLINGS - children stored sequentially
    ///
    /// These flags are combined with the node's existing flags (bits 0-5).
    pub fn from_node_header_v2(
        header: &CharNodeHeader,
        data_size: u32,
        encoding_flags: u8,
    ) -> Self {
        Self {
            magic: CHAR_NODE_MAGIC,
            version: CHAR_FORMAT_VERSION,
            node_type: header.node_type,
            // Combine node flags (bits 0-5) with encoding flags (bits 6-7)
            flags: (header.flags & 0x3F) | (encoding_flags & 0xC0),
            reserved: 0,
            num_children: header.num_children,
            prefix_len: header.prefix_len,
            _padding: 0,
            data_size,
        }
    }

    /// Check if relative offsets encoding is used
    ///
    /// When true, child pointers are stored as relative offsets from the parent slot,
    /// enabling more compact varint encoding for same-arena children.
    #[inline]
    pub fn uses_relative_offsets(&self) -> bool {
        self.flags & 0x80 != 0 // FLAG_RELATIVE_OFFSETS
    }

    /// Check if sequential siblings encoding is used
    ///
    /// When true, children are stored contiguously and the node only stores
    /// (first_child_slot, count) instead of N separate pointers.
    #[inline]
    pub fn uses_sequential_siblings(&self) -> bool {
        self.flags & 0x40 != 0 // FLAG_SEQUENTIAL_SIBLINGS
    }

    /// Convert to a CharNodeHeader
    pub fn to_node_header(&self) -> CharNodeHeader {
        CharNodeHeader {
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
        if self.prefix_len as usize > CHAR_MAX_PREFIX_LEN {
            return Err(PersistentARTrieError::corrupted(format!(
                "prefix length {} exceeds maximum {}",
                self.prefix_len, CHAR_MAX_PREFIX_LEN
            )));
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

/// Serialize a CharNode to a writer
pub fn serialize_char_node<W: Write>(node: &CharNode, writer: &mut W) -> Result<usize> {
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
    let max_key = keys.iter().chain(prefix_chars.iter()).copied().max().unwrap_or(0);
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
    encode_compact_node(
        &header,
        &prefix_chars,
        &keys,
        &children,
        value_ptr,
    )
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

    let max_key = keys.iter().chain(prefix_chars.iter()).copied().max().unwrap_or(0);
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
            (keys, children, prefix_chars, value_ptr, COMPACT_NODE_TYPE_N4, n.header.flags)
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
            (keys, children, prefix_chars, value_ptr, COMPACT_NODE_TYPE_N16, n.header.flags)
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
            (keys, children, prefix_chars, value_ptr, COMPACT_NODE_TYPE_N48, n.header.flags)
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
            (keys, children, prefix_chars, value_ptr, COMPACT_NODE_TYPE_BUCKET, n.header.flags)
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
                node.entries.insert(key, SwizzledPtr::from_raw(decoded.children[i]));
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
    let mut slots = Vec::new();

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
            let mut entries: Vec<_> = n.entries.iter().collect();
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

/// Calculate v2 serialized data size with encoded children
///
/// # Arguments
/// * `node` - The CharNode to calculate size for
/// * `ctx` - The serialization context (determines encoding mode)
///
/// # Returns
/// Size in bytes of the type-specific data (excluding header and prefix)
fn char_node_data_size_v2(node: &CharNode, ctx: &SerializationContext) -> usize {
    let child_slots = collect_char_child_slots(node);
    let num_children = child_slots.len();

    if ctx.use_sequential && ctx.first_child_slot.is_some() {
        // Sequential mode: only store first_child reference
        // Encoded size depends on whether same arena as parent
        let first_child = ctx.first_child_slot.unwrap();
        let first_slot_size = if first_child.arena_id == ctx.parent_slot.arena_id {
            // Same arena: relative offset uses varint
            use super::relative_encoding::encoded_size;
            encoded_size(ctx.parent_slot, first_child)
        } else {
            // Cross arena: full 9-byte encoding
            super::relative_encoding::CROSS_ARENA_SIZE
        };

        match node {
            CharNode::N4(_) => 4 * 4 + first_slot_size + 8,    // 4 keys + first_slot + value_ptr
            CharNode::N16(_) => 16 * 4 + first_slot_size + 8,  // 16 keys + first_slot + value_ptr
            CharNode::N48(_) => 48 * 4 + first_slot_size + 8,  // 48 keys + first_slot + value_ptr
            CharNode::Bucket(n) => 4 + first_slot_size + 8 + n.entries.len() * 4, // num_entries + first_slot + value_ptr + keys
        }
    } else {
        // Relative mode: encode each child pointer individually
        let mut children_size = 0;
        for slot in &child_slots {
            use super::relative_encoding::encoded_size;
            children_size += encoded_size(ctx.parent_slot, *slot);
        }

        match node {
            CharNode::N4(_) => 4 * 4 + children_size + 8,          // 4 keys + children + value_ptr
            CharNode::N16(_) => 16 * 4 + children_size + 8,        // 16 keys + children + value_ptr
            CharNode::N48(_) => 48 * 4 + children_size + 8,        // 48 keys + children + value_ptr
            CharNode::Bucket(n) => 4 + children_size + 8 + n.entries.len() * 4, // num_entries + children + value_ptr + keys
        }
    }
}

/// Serialize a CharNode using v2 format with relative offsets/sequential siblings
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
pub fn serialize_char_node_v2<W: Write>(
    node: &CharNode,
    writer: &mut W,
    ctx: &SerializationContext,
) -> Result<usize> {
    let data_size = char_prefix_size(node) + char_node_data_size_v2(node, ctx);
    let header = SerializedCharNodeHeader::from_node_header_v2(
        node.header(),
        data_size as u32,
        ctx.encoding_flags(),
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

    // Encode children based on context
    let child_slots = collect_char_child_slots(node);
    let mut children_buf = Vec::new();

    if ctx.use_sequential {
        if let Some(first_child) = ctx.first_child_slot {
            encode_sequential_siblings(ctx.parent_slot, first_child, &mut children_buf);
        }
    } else {
        // Encode each child individually with relative offsets
        for &slot in &child_slots {
            encode_child_pointer(ctx.parent_slot, slot, &mut children_buf);
        }
    }

    // Write type-specific data with encoded children
    match node {
        CharNode::N4(n) => serialize_charnode4_v2(n, writer, &children_buf)?,
        CharNode::N16(n) => serialize_charnode16_v2(n, writer, &children_buf)?,
        CharNode::N48(n) => serialize_charnode48_v2(n, writer, &children_buf)?,
        CharNode::Bucket(n) => serialize_charbucket_v2(n, writer, &children_buf)?,
    }

    Ok(CHAR_SERIALIZED_HEADER_SIZE + data_size)
}

fn serialize_charnode4_v2<W: Write>(
    node: &CharNode4,
    writer: &mut W,
    encoded_children: &[u8],
) -> Result<()> {
    // Write keys (4 × u32)
    for key in &node.keys {
        writer.write_all(&key.to_le_bytes()).map_err(io_err)?;
    }

    // Write encoded children (relative offsets or sequential reference)
    writer.write_all(encoded_children).map_err(io_err)?;

    // Write value_ptr (still fixed 8 bytes for now - could be encoded too)
    let value_raw = node.value_ptr.to_raw();
    writer.write_all(&value_raw.to_le_bytes()).map_err(io_err)?;

    Ok(())
}

fn serialize_charnode16_v2<W: Write>(
    node: &CharNode16,
    writer: &mut W,
    encoded_children: &[u8],
) -> Result<()> {
    // Write keys (16 × u32)
    for key in &node.keys {
        writer.write_all(&key.to_le_bytes()).map_err(io_err)?;
    }

    // Write encoded children
    writer.write_all(encoded_children).map_err(io_err)?;

    // Write value_ptr
    let value_raw = node.value_ptr.to_raw();
    writer.write_all(&value_raw.to_le_bytes()).map_err(io_err)?;

    Ok(())
}

fn serialize_charnode48_v2<W: Write>(
    node: &CharNode48,
    writer: &mut W,
    encoded_children: &[u8],
) -> Result<()> {
    // Write keys (48 × u32, sorted)
    for key in &node.keys {
        writer.write_all(&key.to_le_bytes()).map_err(io_err)?;
    }

    // Write encoded children
    writer.write_all(encoded_children).map_err(io_err)?;

    // Write value_ptr
    let value_raw = node.value_ptr.to_raw();
    writer.write_all(&value_raw.to_le_bytes()).map_err(io_err)?;

    Ok(())
}

fn serialize_charbucket_v2<W: Write>(
    node: &CharBucket,
    writer: &mut W,
    encoded_children: &[u8],
) -> Result<()> {
    // Write number of entries
    let num_entries = node.entries.len() as u32;
    writer.write_all(&num_entries.to_le_bytes()).map_err(io_err)?;

    // Write value_ptr
    let value_raw = node.value_ptr.to_raw();
    writer.write_all(&value_raw.to_le_bytes()).map_err(io_err)?;

    // Write keys only (children are in encoded_children buffer)
    let mut entries: Vec<_> = node.entries.iter().collect();
    entries.sort_by_key(|&(k, _)| *k);
    for (&key, _) in entries {
        writer.write_all(&key.to_le_bytes()).map_err(io_err)?;
    }

    // Write encoded children
    writer.write_all(encoded_children).map_err(io_err)?;

    Ok(())
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
    match header.node_type {
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
    }
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

    if uses_sequential {
        // Read first_child reference and reconstruct sequential children
        let remaining_data = read_remaining_data(reader, header.data_size as usize, 4 * 4, prefix_size)?;
        let (children, bytes_consumed) = decode_sequential_siblings(
            &remaining_data,
            ctx.parent_slot,
            num_children,
        );

        for (i, slot) in children.iter().enumerate().take(4) {
            node.children[i] = arena_slot_to_ptr(*slot);
        }

        // Read value_ptr from remaining data
        let value_offset = bytes_consumed;
        if remaining_data.len() >= value_offset + 8 {
            let value_raw = u64::from_le_bytes(
                remaining_data[value_offset..value_offset + 8].try_into().unwrap()
            );
            node.value_ptr = SwizzledPtr::from_raw(value_raw);
        }
    } else if uses_relative {
        // Read relative-encoded children
        let remaining_data = read_remaining_data(reader, header.data_size as usize, 4 * 4, prefix_size)?;
        let (children, bytes_consumed) = decode_children(
            &remaining_data,
            ctx.parent_slot,
            num_children,
        );

        for (i, slot) in children.iter().enumerate().take(4) {
            node.children[i] = arena_slot_to_ptr(*slot);
        }

        // Read value_ptr from remaining data
        let value_offset = bytes_consumed;
        if remaining_data.len() >= value_offset + 8 {
            let value_raw = u64::from_le_bytes(
                remaining_data[value_offset..value_offset + 8].try_into().unwrap()
            );
            node.value_ptr = SwizzledPtr::from_raw(value_raw);
        }
    } else {
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

    if uses_sequential {
        let remaining_data = read_remaining_data(reader, header.data_size as usize, 16 * 4, prefix_size)?;
        let (children, bytes_consumed) = decode_sequential_siblings(
            &remaining_data,
            ctx.parent_slot,
            num_children,
        );

        for (i, slot) in children.iter().enumerate().take(16) {
            node.children[i] = arena_slot_to_ptr(*slot);
        }

        let value_offset = bytes_consumed;
        if remaining_data.len() >= value_offset + 8 {
            let value_raw = u64::from_le_bytes(
                remaining_data[value_offset..value_offset + 8].try_into().unwrap()
            );
            node.value_ptr = SwizzledPtr::from_raw(value_raw);
        }
    } else if uses_relative {
        let remaining_data = read_remaining_data(reader, header.data_size as usize, 16 * 4, prefix_size)?;
        let (children, bytes_consumed) = decode_children(
            &remaining_data,
            ctx.parent_slot,
            num_children,
        );

        for (i, slot) in children.iter().enumerate().take(16) {
            node.children[i] = arena_slot_to_ptr(*slot);
        }

        let value_offset = bytes_consumed;
        if remaining_data.len() >= value_offset + 8 {
            let value_raw = u64::from_le_bytes(
                remaining_data[value_offset..value_offset + 8].try_into().unwrap()
            );
            node.value_ptr = SwizzledPtr::from_raw(value_raw);
        }
    } else {
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

    if uses_sequential {
        let remaining_data = read_remaining_data(reader, header.data_size as usize, 48 * 4, prefix_size)?;
        let (children, bytes_consumed) = decode_sequential_siblings(
            &remaining_data,
            ctx.parent_slot,
            num_children,
        );

        for (i, slot) in children.iter().enumerate().take(48) {
            node.children[i] = arena_slot_to_ptr(*slot);
        }

        let value_offset = bytes_consumed;
        if remaining_data.len() >= value_offset + 8 {
            let value_raw = u64::from_le_bytes(
                remaining_data[value_offset..value_offset + 8].try_into().unwrap()
            );
            node.value_ptr = SwizzledPtr::from_raw(value_raw);
        }
    } else if uses_relative {
        let remaining_data = read_remaining_data(reader, header.data_size as usize, 48 * 4, prefix_size)?;
        let (children, bytes_consumed) = decode_children(
            &remaining_data,
            ctx.parent_slot,
            num_children,
        );

        for (i, slot) in children.iter().enumerate().take(48) {
            node.children[i] = arena_slot_to_ptr(*slot);
        }

        let value_offset = bytes_consumed;
        if remaining_data.len() >= value_offset + 8 {
            let value_raw = u64::from_le_bytes(
                remaining_data[value_offset..value_offset + 8].try_into().unwrap()
            );
            node.value_ptr = SwizzledPtr::from_raw(value_raw);
        }
    } else {
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
        let remaining_size = (header.data_size as usize)
            .saturating_sub(prefix_size)  // prefix already read
            .saturating_sub(4)            // num_entries
            .saturating_sub(8)            // value_ptr
            .saturating_sub(num_entries * 4); // keys
        let mut remaining_data = vec![0u8; remaining_size];
        reader.read_exact(&mut remaining_data).map_err(io_err)?;

        let children = if uses_sequential {
            let (children, _) = decode_sequential_siblings(
                &remaining_data,
                ctx.parent_slot,
                num_entries,
            );
            children
        } else {
            let (children, _) = decode_children(
                &remaining_data,
                ctx.parent_slot,
                num_entries,
            );
            children
        };

        for (key, slot) in keys.iter().zip(children.iter()) {
            node.entries.insert(*key, arena_slot_to_ptr(*slot));
        }
    } else {
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
    let remaining_size = data_size.saturating_sub(keys_size).saturating_sub(prefix_size);
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
fn arena_slot_to_ptr(slot: ArenaSlot) -> SwizzledPtr {
    use crate::persistent_artrie::NodeType;
    // Arena N is stored in Block N+1
    let block_id = slot.arena_id.saturating_add(1);
    SwizzledPtr::on_disk(block_id, slot.slot_id, NodeType::CharNode4) // Default type, will be overwritten
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistent_artrie::NodeType;
    use crate::persistent_artrie_char::nodes::flags;
    use crate::persistent_artrie_char::nodes::CharArtNode;

    #[test]
    fn test_header_roundtrip() {
        let header = SerializedCharNodeHeader {
            magic: CHAR_NODE_MAGIC,
            version: CHAR_FORMAT_VERSION,
            node_type: char_node_types::CHARNODE4,
            flags: flags::IS_FINAL,
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
        assert_eq!(restored.flags, flags::IS_FINAL);
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
            flags: 0,
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
            .add_child('a' as u32, SwizzledPtr::on_disk(100, 0, NodeType::CharNode4))
            .expect("add child a");
        node4
            .add_child('b' as u32, SwizzledPtr::on_disk(200, 0, NodeType::CharNode16))
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
                .add_child('a' as u32 + i, SwizzledPtr::on_disk(i as u32, 0, NodeType::CharNode4))
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
            || CharNode::N4(Box::new(CharNode4::new())),
            || CharNode::N16(Box::new(CharNode16::new())),
            || CharNode::N48(Box::new(CharNode48::new())),
            || CharNode::Bucket(Box::new(CharBucket::new())),
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
        let node4 = CharNode::N4(Box::new(CharNode4::new()));
        assert_eq!(char_serialized_size(&node4), 16 + 0 + 56);

        // CharNode4 with prefix: 16 header + 24 prefix + 56 data
        let mut node4_with_prefix = CharNode4::new();
        let prefix: Vec<u32> = "test".chars().map(|c| c as u32).collect();
        node4_with_prefix.prefix = CharCompressedPrefix::from_chars(&prefix);
        node4_with_prefix.header.prefix_len = 4;
        let node4_p = CharNode::N4(Box::new(node4_with_prefix));
        assert_eq!(char_serialized_size(&node4_p), 16 + 24 + 56);

        // CharNode16 without prefix: 16 + 0 + 200
        let node16 = CharNode::N16(Box::new(CharNode16::new()));
        assert_eq!(char_serialized_size(&node16), 16 + 0 + 200);

        // CharNode48 without prefix: 16 + 0 + 584
        let node48 = CharNode::N48(Box::new(CharNode48::new()));
        assert_eq!(char_serialized_size(&node48), 16 + 0 + 584);

        // CharBucket with 5 entries: 16 + 0 + (4 + 8 + 5*12)
        let mut bucket = CharBucket::new();
        for i in 0..5 {
            bucket
                .add_child(i, SwizzledPtr::on_disk(i as u32, 0, NodeType::CharNode4))
                .expect("add");
        }
        let bucket_node = CharNode::Bucket(Box::new(bucket));
        assert_eq!(char_serialized_size(&bucket_node), 16 + 0 + (4 + 8 + 5 * 12));
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
            let loc = n.value_ptr.disk_location().expect("should have disk location");
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
                .add_child('a' as u32, SwizzledPtr::on_disk(100, 0, NodeType::CharNode4))
                .expect("add child a");
            node4
                .add_child('b' as u32, SwizzledPtr::on_disk(200, 0, NodeType::CharNode16))
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
                    .add_child('a' as u32 + i, SwizzledPtr::on_disk(i as u32, 0, NodeType::CharNode4))
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
                .add_child('a' as u32, SwizzledPtr::on_disk(100, 0, NodeType::CharNode4))
                .expect("add");
            node4
                .add_child('b' as u32, SwizzledPtr::on_disk(200, 0, NodeType::CharNode4))
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
                || CharNode::N4(Box::new(CharNode4::new())),
                || CharNode::N16(Box::new(CharNode16::new())),
                || CharNode::N48(Box::new(CharNode48::new())),
                || CharNode::Bucket(Box::new(CharBucket::new())),
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
                .add_child('a' as u32, SwizzledPtr::on_disk(100, 0, NodeType::CharNode4))
                .expect("add");
            node4
                .add_child('b' as u32, SwizzledPtr::on_disk(200, 0, NodeType::CharNode4))
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
                .add_child('a' as u32, SwizzledPtr::on_disk(0x7FFFFF, 0x3FFFFF, NodeType::CharNode4))
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
                .add_child('b' as u32, SwizzledPtr::on_disk(1, 20, NodeType::CharNode4))
                .expect("add child b");

            let node = CharNode::N4(Box::new(node4));

            // Parent at slot 100 in arena 0
            let parent_slot = ArenaSlot::new(0, 100);
            let ctx = SerializationContext::new(parent_slot);

            let mut buffer = Vec::new();
            let bytes_written = serialize_char_node_v2(&node, &mut buffer, &ctx)
                .expect("serialize");

            assert!(bytes_written > 0);

            // Check that header has relative offsets flag
            let header = SerializedCharNodeHeader::from_bytes(
                buffer[..16].try_into().unwrap()
            );
            assert!(header.uses_relative_offsets());
            assert!(!header.uses_sequential_siblings());

            // Deserialize and verify
            let deser_ctx = DeserializationContext::new(parent_slot);
            let mut cursor = std::io::Cursor::new(&buffer);
            let restored = deserialize_char_node_v2(&mut cursor, &deser_ctx)
                .expect("deserialize");

            assert!(matches!(restored, CharNode::N4(_)));
            assert_eq!(restored.header().num_children, 2);
            assert!(restored.find_child('a' as u32).is_some());
            assert!(restored.find_child('b' as u32).is_some());
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
                .add_child('b' as u32, SwizzledPtr::on_disk(1, 11, NodeType::CharNode4))
                .expect("add child b");
            node4
                .add_child('c' as u32, SwizzledPtr::on_disk(1, 12, NodeType::CharNode4))
                .expect("add child c");

            let node = CharNode::N4(Box::new(node4));

            // Parent at slot 100, first child at slot 10
            let parent_slot = ArenaSlot::new(0, 100);
            let first_child_slot = ArenaSlot::new(0, 10);
            let ctx = SerializationContext::sequential(parent_slot, first_child_slot);

            let mut buffer = Vec::new();
            let bytes_written = serialize_char_node_v2(&node, &mut buffer, &ctx)
                .expect("serialize");

            assert!(bytes_written > 0);

            // Check that header has both flags set
            let header = SerializedCharNodeHeader::from_bytes(
                buffer[..16].try_into().unwrap()
            );
            assert!(header.uses_relative_offsets());
            assert!(header.uses_sequential_siblings());

            // Deserialize and verify
            let deser_ctx = DeserializationContext::new(parent_slot);
            let mut cursor = std::io::Cursor::new(&buffer);
            let restored = deserialize_char_node_v2(&mut cursor, &deser_ctx)
                .expect("deserialize");

            assert!(matches!(restored, CharNode::N4(_)));
            assert_eq!(restored.header().num_children, 3);
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
                    .add_child(('a' as u32) + i, SwizzledPtr::on_disk(1, 10 + i, NodeType::CharNode4))
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
    }
}
