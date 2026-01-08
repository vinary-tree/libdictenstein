//! Compact variable-width encoding for space-efficient node serialization.
//!
//! This module provides encoding utilities that minimize storage overhead by:
//! 1. Using variable-width integers (varint) for small values
//! 2. Packing node metadata into a 2-byte header that describes field widths
//! 3. Only storing fields that are present (conditional serialization)
//!
//! ## Encoding Scheme
//!
//! ### Varint Encoding (PathMap-style)
//!
//! Values are encoded using a self-delimiting format:
//! - Values 0-247: Single byte (direct value)
//! - Values 248+: Header byte indicates length, followed by data bytes
//!
//! ### Compact Node Header (2 bytes)
//!
//! ```text
//! Byte 0:
//!   bits 0-1: key_width   (0=1B, 1=2B, 2=3B, 3=4B)
//!   bits 2-4: ptr_width   (0=1B, 1=2B, 2=3B, 3=4B, 4=5B, 5=6B)
//!   bits 5-6: num_children (for N4: 0-3, for larger nodes: see byte 1)
//!   bit 7:    has_value
//!
//! Byte 1:
//!   bits 0-2: prefix_len  (0-6)
//!   bits 3-5: node_type   (0=N4, 1=N16, 2=N48, 3=Bucket)
//!   bits 6-7: reserved / extended child count
//! ```

use std::io::{Read, Write};

/// Bias value for varint encoding - values 0-247 are stored directly
pub const VARINT_LEN_BIAS: u8 = 247;

/// Maximum single-byte varint value
pub const VARINT_MAX_SINGLE_BYTE: u64 = VARINT_LEN_BIAS as u64;

/// Node type identifiers for compact header
pub mod compact_node_types {
    pub const N4: u8 = 0;
    pub const N16: u8 = 1;
    pub const N48: u8 = 2;
    pub const BUCKET: u8 = 3;
}

// Re-export node type constants at top level for convenience
pub const COMPACT_NODE_TYPE_N4: u8 = compact_node_types::N4;
pub const COMPACT_NODE_TYPE_N16: u8 = compact_node_types::N16;
pub const COMPACT_NODE_TYPE_N48: u8 = compact_node_types::N48;
pub const COMPACT_NODE_TYPE_BUCKET: u8 = compact_node_types::BUCKET;

/// Compact node header (3 bytes)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactHeader {
    /// Width of key values in bytes (1-4)
    pub key_width: u8,
    /// Width of pointer values in bytes (1-6)
    pub ptr_width: u8,
    /// Number of children (interpretation depends on node type)
    pub num_children: u8,
    /// Whether this node has a value
    pub has_value: bool,
    /// Prefix length (0-6)
    pub prefix_len: u8,
    /// Node type (N4, N16, N48, Bucket)
    pub node_type: u8,
    /// Node flags (IS_FINAL, etc.)
    pub flags: u8,
}

/// Size of compact header in bytes
pub const COMPACT_HEADER_SIZE: usize = 3;

impl CompactHeader {
    /// Create a new compact header
    pub fn new(
        key_width: u8,
        ptr_width: u8,
        num_children: u8,
        has_value: bool,
        prefix_len: u8,
        node_type: u8,
        flags: u8,
    ) -> Self {
        debug_assert!(key_width >= 1 && key_width <= 4);
        debug_assert!(ptr_width >= 1 && ptr_width <= 6);
        debug_assert!(prefix_len <= 6);
        debug_assert!(node_type <= 3);

        Self {
            key_width,
            ptr_width,
            num_children,
            has_value,
            prefix_len,
            node_type,
            flags,
        }
    }

    /// Encode header to 3 bytes (plus optional 4th byte for large num_children)
    ///
    /// For num_children 0-15: fits in 4 bits (2 in b0, 2 in b1)
    /// For num_children > 15: stored in separate byte after header
    pub fn to_bytes(&self) -> [u8; 3] {
        // For num_children > 15, we store 15 as a sentinel and the actual count
        // is stored externally (see to_bytes_with_extended)
        let stored_children = self.num_children.min(15);

        let b0 = ((self.key_width - 1) & 0x03)
            | (((self.ptr_width - 1) & 0x07) << 2)
            | ((stored_children & 0x03) << 5)  // Low 2 bits of num_children
            | ((self.has_value as u8) << 7);

        let b1 = (self.prefix_len & 0x07)
            | ((self.node_type & 0x07) << 3)
            | (((stored_children >> 2) & 0x03) << 6);  // High 2 bits of num_children

        let b2 = self.flags;

        [b0, b1, b2]
    }

    /// Encode header with extended num_children support
    ///
    /// Returns (header_bytes, optional_extended_count)
    pub fn to_bytes_with_extended(&self) -> ([u8; 3], Option<u8>) {
        let bytes = self.to_bytes();
        if self.num_children > 15 {
            (bytes, Some(self.num_children))
        } else {
            (bytes, None)
        }
    }

    /// Decode header from 3 bytes
    pub fn from_bytes(bytes: [u8; 3]) -> Self {
        let b0 = bytes[0];
        let b1 = bytes[1];
        let b2 = bytes[2];

        let key_width = (b0 & 0x03) + 1;
        let ptr_width = ((b0 >> 2) & 0x07) + 1;
        let children_low = (b0 >> 5) & 0x03;  // Low 2 bits
        let has_value = (b0 >> 7) != 0;

        let prefix_len = b1 & 0x07;
        let node_type = (b1 >> 3) & 0x07;
        let children_high = (b1 >> 6) & 0x03;  // High 2 bits

        // Combine 4 bits for num_children (0-15 range)
        let num_children = children_low | (children_high << 2);

        let flags = b2;

        Self {
            key_width,
            ptr_width,
            num_children,
            has_value,
            prefix_len,
            node_type,
            flags,
        }
    }

    /// Decode header from bytes with optional extended num_children
    ///
    /// If num_children == 15, read an additional byte for the actual count
    pub fn from_bytes_with_extended(data: &[u8], offset: &mut usize) -> Self {
        let mut header = Self::from_bytes([data[*offset], data[*offset + 1], data[*offset + 2]]);
        *offset += COMPACT_HEADER_SIZE;

        // Check if extended count is needed (sentinel value 15)
        if header.num_children == 15 && *offset < data.len() {
            header.num_children = data[*offset];
            *offset += 1;
        }

        header
    }

    /// Check if this header requires extended num_children encoding
    pub fn needs_extended_count(&self) -> bool {
        self.num_children > 15
    }

    /// Calculate the total size of encoded data for this header
    pub fn data_size(&self) -> usize {
        COMPACT_HEADER_SIZE // header (3 bytes)
            + if self.needs_extended_count() { 1 } else { 0 } // extended num_children byte
            + (self.prefix_len as usize * self.key_width as usize) // prefix
            + (self.num_children as usize * self.key_width as usize) // keys
            + (self.num_children as usize * self.ptr_width as usize) // children
            + if self.has_value { self.ptr_width as usize } else { 0 } // value_ptr
    }
}

// =============================================================================
// Varint Encoding
// =============================================================================

/// Write a varint-encoded value to a buffer
///
/// Returns the number of bytes written
pub fn write_varint<W: Write>(value: u64, writer: &mut W) -> std::io::Result<usize> {
    if value <= VARINT_MAX_SINGLE_BYTE {
        writer.write_all(&[value as u8])?;
        Ok(1)
    } else {
        let bytes = value.to_le_bytes();
        let len = required_bytes_for_value(value);
        writer.write_all(&[VARINT_LEN_BIAS + len as u8])?;
        writer.write_all(&bytes[..len])?;
        Ok(1 + len)
    }
}

/// Write a varint-encoded value to a Vec
pub fn write_varint_to_vec(value: u64, out: &mut Vec<u8>) {
    if value <= VARINT_MAX_SINGLE_BYTE {
        out.push(value as u8);
    } else {
        let bytes = value.to_le_bytes();
        let len = required_bytes_for_value(value);
        out.push(VARINT_LEN_BIAS + len as u8);
        out.extend_from_slice(&bytes[..len]);
    }
}

/// Read a varint-encoded value from a reader
///
/// Returns the value and number of bytes consumed
pub fn read_varint<R: Read>(reader: &mut R) -> std::io::Result<(u64, usize)> {
    let mut first = [0u8; 1];
    reader.read_exact(&mut first)?;

    if first[0] <= VARINT_LEN_BIAS {
        Ok((first[0] as u64, 1))
    } else {
        let len = (first[0] - VARINT_LEN_BIAS) as usize;
        let mut bytes = [0u8; 8];
        reader.read_exact(&mut bytes[..len])?;
        Ok((u64::from_le_bytes(bytes), 1 + len))
    }
}

/// Read a varint-encoded value from a byte slice
///
/// Returns the value and number of bytes consumed
pub fn read_varint_from_slice(data: &[u8]) -> (u64, usize) {
    let first = data[0];
    if first <= VARINT_LEN_BIAS {
        (first as u64, 1)
    } else {
        let len = (first - VARINT_LEN_BIAS) as usize;
        let mut bytes = [0u8; 8];
        bytes[..len].copy_from_slice(&data[1..1 + len]);
        (u64::from_le_bytes(bytes), 1 + len)
    }
}

/// Calculate the number of bytes required to store a value
pub fn required_bytes_for_value(value: u64) -> usize {
    if value == 0 {
        1
    } else {
        ((64 - value.leading_zeros()) as usize + 7) / 8
    }
}

/// Calculate the varint-encoded size of a value
pub fn varint_size(value: u64) -> usize {
    if value <= VARINT_MAX_SINGLE_BYTE {
        1
    } else {
        1 + required_bytes_for_value(value)
    }
}

// =============================================================================
// Fixed-Width Value Encoding
// =============================================================================

/// Determine optimal key width for a set of keys
pub fn determine_key_width(max_key: u32) -> u8 {
    match max_key {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x10000..=0xFFFFFF => 3,
        _ => 4,
    }
}

/// Determine optimal pointer width for arena offsets
pub fn determine_ptr_width(max_offset: u64) -> u8 {
    match max_offset {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x10000..=0xFFFFFF => 3,
        0x1000000..=0xFFFFFFFF => 4,
        0x100000000..=0xFFFFFFFFFF => 5,
        _ => 6,
    }
}

/// Write a value with the specified byte width
pub fn write_fixed_width<W: Write>(value: u64, width: u8, writer: &mut W) -> std::io::Result<()> {
    let bytes = value.to_le_bytes();
    writer.write_all(&bytes[..width as usize])
}

/// Write a value with the specified byte width to a Vec
pub fn write_fixed_width_to_vec(value: u64, width: u8, out: &mut Vec<u8>) {
    let bytes = value.to_le_bytes();
    out.extend_from_slice(&bytes[..width as usize]);
}

/// Read a value with the specified byte width
pub fn read_fixed_width<R: Read>(width: u8, reader: &mut R) -> std::io::Result<u64> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes[..width as usize])?;
    Ok(u64::from_le_bytes(bytes))
}

/// Read a value with the specified byte width from a slice
pub fn read_fixed_width_from_slice(data: &[u8], offset: &mut usize, width: u8) -> u64 {
    let mut bytes = [0u8; 8];
    let end = *offset + width as usize;
    bytes[..width as usize].copy_from_slice(&data[*offset..end]);
    *offset = end;
    u64::from_le_bytes(bytes)
}

/// Read N values with the specified byte width from a slice
pub fn read_n_values_from_slice(data: &[u8], offset: &mut usize, count: usize, width: u8) -> Vec<u64> {
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(read_fixed_width_from_slice(data, offset, width));
    }
    values
}

/// Write N values with the specified byte width to a Vec
pub fn write_n_values_to_vec(values: &[u64], width: u8, out: &mut Vec<u8>) {
    for &value in values {
        write_fixed_width_to_vec(value, width, out);
    }
}

// =============================================================================
// Compact Node Serialization
// =============================================================================

/// Serialize a node using compact encoding with pre-computed header
///
/// # Arguments
/// * `header` - Pre-computed compact header with widths and metadata
/// * `prefix` - Prefix characters as u32 values
/// * `keys` - Child key characters as u32 values
/// * `children` - Child pointers/offsets
/// * `value_ptr` - Optional value pointer
///
/// # Returns
/// Encoded bytes
pub fn encode_compact_node(
    header: &CompactHeader,
    prefix: &[u32],
    keys: &[u32],
    children: &[u64],
    value_ptr: Option<u64>,
) -> Vec<u8> {
    assert_eq!(keys.len(), children.len());

    // Encode
    let mut out = Vec::with_capacity(header.data_size());

    // Write header (with extended count if needed)
    let (header_bytes, extended_count) = header.to_bytes_with_extended();
    out.extend_from_slice(&header_bytes);

    // Write extended num_children byte if needed
    if let Some(count) = extended_count {
        out.push(count);
    }

    // Write prefix
    for &c in prefix {
        write_fixed_width_to_vec(c as u64, header.key_width, &mut out);
    }

    // Write keys
    for &k in keys {
        write_fixed_width_to_vec(k as u64, header.key_width, &mut out);
    }

    // Write children
    for &child in children {
        write_fixed_width_to_vec(child, header.ptr_width, &mut out);
    }

    // Write value_ptr if present
    if let Some(vp) = value_ptr {
        write_fixed_width_to_vec(vp, header.ptr_width, &mut out);
    }

    out
}

/// Serialize a node using compact encoding (auto-compute widths)
///
/// # Arguments
/// * `node_type` - Type of node (N4, N16, N48, Bucket)
/// * `prefix` - Prefix characters as u32 values
/// * `keys` - Child key characters as u32 values
/// * `children` - Child pointers/offsets
/// * `value_ptr` - Optional value pointer (0 = null)
/// * `max_arena_offset` - Maximum offset in arena (for ptr_width calculation)
/// * `flags` - Node flags (IS_FINAL, etc.)
///
/// # Returns
/// Encoded bytes
pub fn encode_compact_node_auto(
    node_type: u8,
    prefix: &[u32],
    keys: &[u32],
    children: &[u64],
    value_ptr: Option<u64>,
    max_arena_offset: u64,
    flags: u8,
) -> Vec<u8> {
    assert_eq!(keys.len(), children.len());

    // Determine optimal widths
    let max_key = keys.iter().copied().max().unwrap_or(0)
        .max(prefix.iter().copied().max().unwrap_or(0));
    let max_ptr = children.iter().copied().max().unwrap_or(0)
        .max(value_ptr.unwrap_or(0))
        .max(max_arena_offset);

    let key_width = determine_key_width(max_key);
    let ptr_width = determine_ptr_width(max_ptr);

    // Build header
    let header = CompactHeader::new(
        key_width,
        ptr_width,
        keys.len() as u8,
        value_ptr.is_some(),
        prefix.len() as u8,
        node_type,
        flags,
    );

    encode_compact_node(&header, prefix, keys, children, value_ptr)
}

/// Decoded compact node data
#[derive(Debug, Clone)]
pub struct DecodedCompactNode {
    /// Header information
    pub header: CompactHeader,
    /// Prefix characters
    pub prefix: Vec<u32>,
    /// Child keys
    pub keys: Vec<u32>,
    /// Child pointers
    pub children: Vec<u64>,
    /// Value pointer (None = no value, Some(v) = value at v)
    pub value_ptr: Option<u64>,
}

/// Decode a compact-encoded node from bytes
pub fn decode_compact_node(data: &[u8]) -> DecodedCompactNode {
    let mut offset = 0;

    // Read header (3 bytes + optional extended count byte)
    let header = CompactHeader::from_bytes_with_extended(data, &mut offset);

    // Read prefix
    let prefix_vals = read_n_values_from_slice(data, &mut offset, header.prefix_len as usize, header.key_width);
    let prefix: Vec<u32> = prefix_vals.iter().map(|&v| v as u32).collect();

    // Read keys
    let key_vals = read_n_values_from_slice(data, &mut offset, header.num_children as usize, header.key_width);
    let keys: Vec<u32> = key_vals.iter().map(|&v| v as u32).collect();

    // Read children
    let children = read_n_values_from_slice(data, &mut offset, header.num_children as usize, header.ptr_width);

    // Read value_ptr if present
    let value_ptr = if header.has_value {
        Some(read_fixed_width_from_slice(data, &mut offset, header.ptr_width))
    } else {
        None
    };

    DecodedCompactNode {
        header,
        prefix,
        keys,
        children,
        value_ptr,
    }
}

/// Calculate the encoded size of a compact node without actually encoding
pub fn compact_node_size(
    prefix_len: usize,
    num_children: usize,
    has_value: bool,
    max_key: u32,
    max_ptr: u64,
) -> usize {
    let key_width = determine_key_width(max_key) as usize;
    let ptr_width = determine_ptr_width(max_ptr) as usize;

    COMPACT_HEADER_SIZE // header (3 bytes)
        + if num_children > 15 { 1 } else { 0 } // extended num_children byte
        + (prefix_len * key_width)
        + (num_children * key_width)
        + (num_children * ptr_width)
        + if has_value { ptr_width } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_varint_single_byte() {
        for v in 0..=VARINT_MAX_SINGLE_BYTE {
            let mut buf = Vec::new();
            write_varint_to_vec(v, &mut buf);
            assert_eq!(buf.len(), 1);
            let (decoded, consumed) = read_varint_from_slice(&buf);
            assert_eq!(decoded, v);
            assert_eq!(consumed, 1);
        }
    }

    #[test]
    fn test_varint_multi_byte() {
        let values = [248u64, 255, 256, 1000, 65535, 65536, 0xFFFFFF, 0xFFFFFFFF, u64::MAX];
        for &v in &values {
            let mut buf = Vec::new();
            write_varint_to_vec(v, &mut buf);
            assert!(buf.len() > 1);
            let (decoded, consumed) = read_varint_from_slice(&buf);
            assert_eq!(decoded, v);
            assert_eq!(consumed, buf.len());
        }
    }

    #[test]
    fn test_determine_key_width() {
        assert_eq!(determine_key_width(0), 1);
        assert_eq!(determine_key_width(127), 1);
        assert_eq!(determine_key_width(255), 1);
        assert_eq!(determine_key_width(256), 2);
        assert_eq!(determine_key_width(65535), 2);
        assert_eq!(determine_key_width(65536), 3);
        assert_eq!(determine_key_width(0xFFFFFF), 3);
        assert_eq!(determine_key_width(0x1000000), 4);
    }

    #[test]
    fn test_determine_ptr_width() {
        assert_eq!(determine_ptr_width(0), 1);
        assert_eq!(determine_ptr_width(255), 1);
        assert_eq!(determine_ptr_width(256), 2);
        assert_eq!(determine_ptr_width(65535), 2);
        assert_eq!(determine_ptr_width(65536), 3);
        assert_eq!(determine_ptr_width(0xFFFFFF), 3);
        assert_eq!(determine_ptr_width(0x1000000), 4);
        assert_eq!(determine_ptr_width(0xFFFFFFFF), 4);
        assert_eq!(determine_ptr_width(0x100000000), 5);
    }

    #[test]
    fn test_compact_header_roundtrip() {
        let header = CompactHeader::new(2, 3, 4, true, 3, compact_node_types::N4, 0x42);
        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), 3);
        let decoded = CompactHeader::from_bytes(bytes);

        assert_eq!(decoded.key_width, 2);
        assert_eq!(decoded.ptr_width, 3);
        assert_eq!(decoded.has_value, true);
        assert_eq!(decoded.prefix_len, 3);
        assert_eq!(decoded.node_type, compact_node_types::N4);
        assert_eq!(decoded.flags, 0x42);
    }

    #[test]
    fn test_encode_decode_compact_node_ascii() {
        // ASCII keys, small arena
        let prefix = vec!['h' as u32, 'e' as u32, 'l' as u32];
        let keys = vec!['l' as u32, 'p' as u32];
        let children = vec![100u64, 200u64];
        let value_ptr = Some(300u64);

        let encoded = encode_compact_node_auto(
            compact_node_types::N4,
            &prefix,
            &keys,
            &children,
            value_ptr,
            1000, // max_arena_offset
            0,    // flags
        );

        // Should be very compact: 3 (header) + 3×1 (prefix) + 2×1 (keys) + 2×2 (children) + 2 (value)
        // = 3 + 3 + 2 + 4 + 2 = 14 bytes
        assert_eq!(encoded.len(), 14);

        let decoded = decode_compact_node(&encoded);
        assert_eq!(decoded.prefix, prefix);
        assert_eq!(decoded.keys, keys);
        assert_eq!(decoded.children, children);
        assert_eq!(decoded.value_ptr, Some(300));
        assert!(decoded.header.has_value);
    }

    #[test]
    fn test_encode_decode_compact_node_unicode() {
        // Unicode keys (Japanese - BMP characters, fit in 2 bytes)
        // 日 = U+65E5 = 26085, 本 = U+672C, 語 = U+8A9E = 35486, 人 = U+4EBA
        let prefix = vec!['日' as u32, '本' as u32];
        let keys = vec!['語' as u32, '人' as u32];
        let children = vec![100u64, 200u64];
        let value_ptr = None;

        let encoded = encode_compact_node_auto(
            compact_node_types::N4,
            &prefix,
            &keys,
            &children,
            value_ptr,
            1000,
            0, // flags
        );

        // BMP Unicode uses 2-byte keys: 3 + 2×2 + 2×2 + 2×2 + 0 = 3 + 4 + 4 + 4 = 15 bytes
        assert_eq!(encoded.len(), 15);

        let decoded = decode_compact_node(&encoded);
        assert_eq!(decoded.prefix, prefix);
        assert_eq!(decoded.keys, keys);
        assert_eq!(decoded.children, children);
        assert_eq!(decoded.value_ptr, None);
        assert!(!decoded.header.has_value);
    }

    #[test]
    fn test_encode_decode_no_prefix() {
        let prefix = vec![];
        let keys = vec!['a' as u32, 'b' as u32];
        let children = vec![50u64, 60u64];

        let encoded = encode_compact_node_auto(
            compact_node_types::N4,
            &prefix,
            &keys,
            &children,
            None,
            100,
            0, // flags
        );

        // 3 (header) + 0 (prefix) + 2×1 (keys) + 2×1 (children) = 7 bytes
        assert_eq!(encoded.len(), 7);

        let decoded = decode_compact_node(&encoded);
        assert!(decoded.prefix.is_empty());
        assert_eq!(decoded.keys.len(), 2);
    }

    #[test]
    fn test_encode_decode_empty_node() {
        let encoded = encode_compact_node_auto(
            compact_node_types::N4,
            &[],
            &[],
            &[],
            None,
            0,
            0, // flags
        );

        // Just header: 3 bytes
        assert_eq!(encoded.len(), 3);

        let decoded = decode_compact_node(&encoded);
        assert!(decoded.prefix.is_empty());
        assert!(decoded.keys.is_empty());
        assert!(decoded.children.is_empty());
        assert!(!decoded.header.has_value);
    }

    #[test]
    fn test_compact_node_size_calculation() {
        // ASCII, small arena, no value
        let size = compact_node_size(3, 2, false, 127, 255);
        assert_eq!(size, 3 + 3 + 2 + 2); // header (3) + prefix + keys + children (all 1-byte)

        // Unicode, larger arena, with value
        let size = compact_node_size(2, 2, true, 0x10000, 0x100000);
        // key_width=3, ptr_width=3
        // 3 + 2*3 + 2*3 + 2*3 + 3 = 3 + 6 + 6 + 6 + 3 = 24
        assert_eq!(size, 24);
    }

    #[test]
    fn test_fixed_width_roundtrip() {
        for width in 1..=6 {
            let max_val = (1u64 << (width * 8)) - 1;
            let values = [0u64, 1, max_val / 2, max_val];

            for &v in &values {
                let mut buf = Vec::new();
                write_fixed_width_to_vec(v, width, &mut buf);
                assert_eq!(buf.len(), width as usize);

                let mut offset = 0;
                let decoded = read_fixed_width_from_slice(&buf, &mut offset, width);
                assert_eq!(decoded, v);
                assert_eq!(offset, width as usize);
            }
        }
    }

    #[test]
    fn test_space_savings_ascii_small() {
        // Typical ASCII node in small arena: current ~96 bytes, compact ~10 bytes
        let prefix = vec!['h' as u32, 'e' as u32, 'l' as u32];
        let keys = vec!['l' as u32, 'p' as u32];
        let children = vec![100u64, 200u64];

        let encoded = encode_compact_node_auto(
            compact_node_types::N4,
            &prefix,
            &keys,
            &children,
            None,
            1000,
            0, // flags
        );

        // Current fixed encoding: ~96 bytes
        // Compact encoding: should be ~12 bytes (3 + 3 + 2 + 4)
        assert!(encoded.len() < 16);

        // Verify data integrity
        let decoded = decode_compact_node(&encoded);
        assert_eq!(decoded.prefix, prefix);
        assert_eq!(decoded.keys, keys);
        assert_eq!(decoded.children, children);
    }

    #[test]
    fn test_flags_preserved() {
        let prefix = vec!['a' as u32];
        let keys = vec!['b' as u32];
        let children = vec![100u64];
        let flags = 0x01 | 0x02; // Some flags

        let encoded = encode_compact_node_auto(
            compact_node_types::N4,
            &prefix,
            &keys,
            &children,
            Some(200),
            1000,
            flags,
        );

        let decoded = decode_compact_node(&encoded);
        assert_eq!(decoded.header.flags, flags);
        assert_eq!(decoded.value_ptr, Some(200));
    }
}
