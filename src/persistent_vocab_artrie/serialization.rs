//! Node Serialization for PersistentVocabARTrie.
//!
//! This module provides binary serialization and deserialization for vocabulary
//! trie nodes, including parent pointer data for reverse lookups.
//!
//! # Serialization Format
//!
//! Vocabulary nodes extend the char node format with parent pointer data:
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────────────┐
//! │ SerializedVocabNodeHeader (24 bytes)                               │
//! ├───────────┬───────────┬───────────┬───────────┬────────────────────┤
//! │ magic[4]  │ version   │ node_type │ flags     │ reserved           │
//! │ "VARN"    │ u8        │ u8        │ u8        │ u8                 │
//! ├───────────┴───────────┴───────────┴───────────┴────────────────────┤
//! │ num_children: u16     │ prefix_len: u8        │ _padding: u8       │
//! ├───────────────────────┴───────────────────────┴────────────────────┤
//! │ data_size: u32 (size of type-specific data)                        │
//! ├────────────────────────────────────────────────────────────────────┤
//! │ parent: NodeRef (8 bytes)                                          │
//! ├────────────────────────────────────────────────────────────────────┤
//! │ parent_edge: u32                                                   │
//! └────────────────────────────────────────────────────────────────────┘
//! │ Value (if has value): u64 vocabulary index                         │
//! └────────────────────────────────────────────────────────────────────┘
//! │ CharCompressedPrefix (24 bytes, if prefix_len > 0)                 │
//! └────────────────────────────────────────────────────────────────────┘
//! │ Type-specific data (variable size)                                 │
//! └────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Flags
//!
//! The `flags` field includes:
//! - Bit 0 (0x01): IS_FINAL - node is a final state
//! - Bit 4 (0x10): HAS_PARENT_POINTER - parent data is present
//! - Bit 5 (0x20): HAS_VALUE - vocabulary index is present

use std::io::{Read, Write};

use crate::persistent_artrie::error::{PersistentARTrieError, Result};
use crate::persistent_artrie::swizzled_ptr::SwizzledPtr;
use crate::persistent_artrie_char::nodes::{
    CharBucket, CharCompressedPrefix, CharNode, CharNode16, CharNode4, CharNode48,
    CharNodeHeader, CHAR_MAX_PREFIX_LEN,
};
use crate::persistent_artrie_char::types::NodeRef;
use crate::persistent_artrie_char::serialization_char::char_node_types;

use super::types::{VocabTrieNode, FLAG_HAS_PARENT_POINTER};

/// Magic bytes identifying a vocab ART node in the serialized format: "VARN"
pub const VOCAB_NODE_MAGIC: [u8; 4] = *b"VARN";

/// Current serialization format version for vocab nodes
pub const VOCAB_FORMAT_VERSION: u8 = 1;

/// Serialized header size in bytes (extended to include parent data)
pub const VOCAB_SERIALIZED_HEADER_SIZE: usize = 24;

/// Flag indicating node has a vocabulary index value
pub const FLAG_HAS_VALUE: u8 = 0x20;

/// Helper to convert io::Error to PersistentARTrieError
fn io_err(e: std::io::Error) -> PersistentARTrieError {
    PersistentARTrieError::io_error("vocab serialization", "<buffer>", e)
}

/// Serialized vocab node header (24 bytes)
///
/// This header extends the char node header with parent pointer information.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SerializedVocabNodeHeader {
    /// Magic bytes "VARN"
    pub magic: [u8; 4],
    /// Format version
    pub version: u8,
    /// Node type (104=N4, 116=N16, 148=N48, 101=Bucket)
    pub node_type: u8,
    /// Node flags (is_final, has_parent, has_value)
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
    /// Parent node reference (8 bytes)
    pub parent: NodeRef,
    /// Edge label from parent (Unicode code point)
    pub parent_edge: u32,
}

impl SerializedVocabNodeHeader {
    /// Create a header from a VocabTrieNode
    pub fn from_node(node: &VocabTrieNode, data_size: u32) -> Self {
        let inner_header = node.inner.header();
        let mut flags = inner_header.flags;

        // Set HAS_PARENT_POINTER flag if parent is not null
        if !node.parent.is_null() {
            flags |= FLAG_HAS_PARENT_POINTER;
        }

        // Set HAS_VALUE flag if value is present
        if node.value.is_some() {
            flags |= FLAG_HAS_VALUE;
        }

        Self {
            magic: VOCAB_NODE_MAGIC,
            version: VOCAB_FORMAT_VERSION,
            node_type: inner_header.node_type,
            flags,
            reserved: 0,
            num_children: inner_header.num_children,
            prefix_len: inner_header.prefix_len,
            _padding: 0,
            data_size,
            parent: node.parent,
            parent_edge: node.parent_edge,
        }
    }

    /// Check if node is final
    #[inline]
    pub fn is_final(&self) -> bool {
        self.flags & 0x01 != 0
    }

    /// Check if node has parent pointer data
    #[inline]
    pub fn has_parent(&self) -> bool {
        self.flags & FLAG_HAS_PARENT_POINTER != 0
    }

    /// Check if node has a vocabulary index value
    #[inline]
    pub fn has_value(&self) -> bool {
        self.flags & FLAG_HAS_VALUE != 0
    }

    /// Convert to a CharNodeHeader (for inner node reconstruction)
    pub fn to_char_node_header(&self) -> CharNodeHeader {
        CharNodeHeader {
            node_type: self.node_type,
            prefix_len: self.prefix_len,
            // Strip vocab-specific flags, keep only node flags
            flags: self.flags & 0x0F,
            _padding: 0,
            num_children: self.num_children,
            _padding2: [0; 2],
            version: 0,
        }
    }

    /// Validate the header
    pub fn validate(&self) -> Result<()> {
        if self.magic != VOCAB_NODE_MAGIC {
            return Err(PersistentARTrieError::InvalidMagic {
                expected: u64::from_le_bytes([
                    VOCAB_NODE_MAGIC[0],
                    VOCAB_NODE_MAGIC[1],
                    VOCAB_NODE_MAGIC[2],
                    VOCAB_NODE_MAGIC[3],
                    0, 0, 0, 0,
                ]),
                found: u64::from_le_bytes([
                    self.magic[0],
                    self.magic[1],
                    self.magic[2],
                    self.magic[3],
                    0, 0, 0, 0,
                ]),
            });
        }
        if self.version > VOCAB_FORMAT_VERSION {
            return Err(PersistentARTrieError::UnsupportedVersion {
                max_supported: VOCAB_FORMAT_VERSION as u32,
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
                    "invalid vocab node type: {}",
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
    pub fn to_bytes(&self) -> [u8; VOCAB_SERIALIZED_HEADER_SIZE] {
        let mut bytes = [0u8; VOCAB_SERIALIZED_HEADER_SIZE];
        bytes[0..4].copy_from_slice(&self.magic);
        bytes[4] = self.version;
        bytes[5] = self.node_type;
        bytes[6] = self.flags;
        bytes[7] = self.reserved;
        bytes[8..10].copy_from_slice(&self.num_children.to_le_bytes());
        bytes[10] = self.prefix_len;
        bytes[11] = self._padding;
        bytes[12..16].copy_from_slice(&self.data_size.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.parent.to_bytes());
        // Note: parent_edge goes after the header in the data section
        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8; VOCAB_SERIALIZED_HEADER_SIZE]) -> Self {
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
            parent: NodeRef::from_bytes(&[
                bytes[16], bytes[17], bytes[18], bytes[19],
                bytes[20], bytes[21], bytes[22], bytes[23],
            ]),
            parent_edge: 0, // Read separately from data section
        }
    }
}

/// Calculate the serialized size of a vocab trie node
pub fn vocab_serialized_size(node: &VocabTrieNode) -> usize {
    VOCAB_SERIALIZED_HEADER_SIZE
        + 4 // parent_edge
        + if node.value.is_some() { 8 } else { 0 } // value
        + vocab_prefix_size(node)
        + vocab_node_data_size(node)
}

fn vocab_prefix_size(node: &VocabTrieNode) -> usize {
    if node.inner.header().prefix_len > 0 {
        CHAR_MAX_PREFIX_LEN * 4 // 6 chars × 4 bytes = 24 bytes
    } else {
        0
    }
}

fn vocab_node_data_size(node: &VocabTrieNode) -> usize {
    match &node.inner {
        CharNode::N4(_) => 4 * 4 + 4 * 8 + 8, // 56 bytes
        CharNode::N16(_) => 16 * 4 + 16 * 8 + 8, // 200 bytes
        CharNode::N48(_) => 48 * 4 + 48 * 8 + 8, // 584 bytes
        CharNode::Bucket(n) => 4 + 8 + n.entries.len() * 12, // 12 + 12n bytes
    }
}

/// Serialize a VocabTrieNode to a writer
pub fn serialize_vocab_node<W: Write>(node: &VocabTrieNode, writer: &mut W) -> Result<usize> {
    let data_size = (4 // parent_edge
        + if node.value.is_some() { 8 } else { 0 }
        + vocab_prefix_size(node)
        + vocab_node_data_size(node)) as u32;

    let header = SerializedVocabNodeHeader::from_node(node, data_size);

    // Write header
    writer.write_all(&header.to_bytes()).map_err(io_err)?;

    // Write parent_edge
    writer.write_all(&node.parent_edge.to_le_bytes()).map_err(io_err)?;

    // Write value if present
    if let Some(value) = node.value {
        writer.write_all(&value.to_le_bytes()).map_err(io_err)?;
    }

    // Write prefix if present
    if node.inner.header().prefix_len > 0 {
        let prefix = node.inner.prefix();
        for &c in &prefix.chars {
            writer.write_all(&c.to_le_bytes()).map_err(io_err)?;
        }
    }

    // Write type-specific data
    match &node.inner {
        CharNode::N4(n) => serialize_charnode4(n, writer)?,
        CharNode::N16(n) => serialize_charnode16(n, writer)?,
        CharNode::N48(n) => serialize_charnode48(n, writer)?,
        CharNode::Bucket(n) => serialize_charbucket(n, writer)?,
    }

    Ok(VOCAB_SERIALIZED_HEADER_SIZE + data_size as usize)
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
    writer.write_all(&num_entries.to_le_bytes()).map_err(io_err)?;

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

/// Deserialize a VocabTrieNode from a reader
pub fn deserialize_vocab_node<R: Read>(reader: &mut R) -> Result<VocabTrieNode> {
    // Read and validate header
    let mut header_bytes = [0u8; VOCAB_SERIALIZED_HEADER_SIZE];
    reader.read_exact(&mut header_bytes).map_err(io_err)?;
    let mut header = SerializedVocabNodeHeader::from_bytes(&header_bytes);
    header.validate()?;

    // Read parent_edge
    let mut parent_edge_bytes = [0u8; 4];
    reader.read_exact(&mut parent_edge_bytes).map_err(io_err)?;
    header.parent_edge = u32::from_le_bytes(parent_edge_bytes);

    // Read value if present
    let value = if header.has_value() {
        let mut value_bytes = [0u8; 8];
        reader.read_exact(&mut value_bytes).map_err(io_err)?;
        Some(u64::from_le_bytes(value_bytes))
    } else {
        None
    };

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
    let inner = match header.node_type {
        char_node_types::CHARNODE4 => deserialize_charnode4(reader, &header, prefix)?,
        char_node_types::CHARNODE16 => deserialize_charnode16(reader, &header, prefix)?,
        char_node_types::CHARNODE48 => deserialize_charnode48(reader, &header, prefix)?,
        char_node_types::CHARBUCKET => deserialize_charbucket(reader, &header, prefix)?,
        _ => {
            return Err(PersistentARTrieError::corrupted(format!(
                "invalid vocab node type: {}",
                header.node_type
            )));
        }
    };

    Ok(VocabTrieNode {
        inner,
        parent: header.parent,
        parent_edge: header.parent_edge,
        value,
    })
}

fn deserialize_charnode4<R: Read>(
    reader: &mut R,
    header: &SerializedVocabNodeHeader,
    prefix: CharCompressedPrefix,
) -> Result<CharNode> {
    let mut node = CharNode4::new();
    node.header = header.to_char_node_header();
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
    header: &SerializedVocabNodeHeader,
    prefix: CharCompressedPrefix,
) -> Result<CharNode> {
    let mut node = CharNode16::new();
    node.header = header.to_char_node_header();
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
    header: &SerializedVocabNodeHeader,
    prefix: CharCompressedPrefix,
) -> Result<CharNode> {
    let mut node = CharNode48::new();
    node.header = header.to_char_node_header();
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
    header: &SerializedVocabNodeHeader,
    prefix: CharCompressedPrefix,
) -> Result<CharNode> {
    let mut node = CharBucket::new();
    node.header = header.to_char_node_header();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_vocab_node_header_roundtrip() {
        let mut node = VocabTrieNode::new();
        node.parent = NodeRef::new(1, 42);
        node.parent_edge = 'A' as u32;
        node.value = Some(100);
        node.set_final(true);

        let header = SerializedVocabNodeHeader::from_node(&node, 100);

        assert_eq!(header.magic, VOCAB_NODE_MAGIC);
        assert_eq!(header.version, VOCAB_FORMAT_VERSION);
        assert!(header.is_final());
        assert!(header.has_parent());
        assert!(header.has_value());
        assert_eq!(header.parent, NodeRef::new(1, 42));

        // Roundtrip
        let bytes = header.to_bytes();
        let header2 = SerializedVocabNodeHeader::from_bytes(&bytes);
        assert_eq!(header2.magic, header.magic);
        assert_eq!(header2.flags, header.flags);
        assert_eq!(header2.parent.arena_id, header.parent.arena_id);
        assert_eq!(header2.parent.slot_index, header.parent.slot_index);
    }

    #[test]
    fn test_vocab_node_serialization_roundtrip() {
        let mut node = VocabTrieNode::new();
        node.parent = NodeRef::new(0, 10);
        node.parent_edge = 'x' as u32;
        node.value = Some(42);
        node.set_final(true);

        // Serialize
        let mut buffer = Vec::new();
        let size = serialize_vocab_node(&node, &mut buffer).unwrap();
        assert_eq!(size, buffer.len());

        // Deserialize
        let mut cursor = Cursor::new(&buffer);
        let node2 = deserialize_vocab_node(&mut cursor).unwrap();

        assert_eq!(node2.parent, node.parent);
        assert_eq!(node2.parent_edge, node.parent_edge);
        assert_eq!(node2.value, node.value);
        assert_eq!(node2.is_final(), node.is_final());
    }

    #[test]
    fn test_vocab_node_no_parent_no_value() {
        let node = VocabTrieNode::new();
        assert!(node.parent.is_null());
        assert!(node.value.is_none());

        let mut buffer = Vec::new();
        serialize_vocab_node(&node, &mut buffer).unwrap();

        let mut cursor = Cursor::new(&buffer);
        let node2 = deserialize_vocab_node(&mut cursor).unwrap();

        assert!(node2.parent.is_null());
        assert!(node2.value.is_none());
    }
}
