//! Compact variable-width encoding for space-efficient byte node serialization.
//!
//! This module provides encoding utilities that minimize storage overhead by:
//! 1. Using variable-width integers (varint) for small values
//! 2. Packing node metadata into a compact header
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
//! For byte-keyed nodes, keys are always 1 byte (u8), so key_width is implicit.
//!
//! ```text
//! Byte 0:
//!   bits 0-2: ptr_width   (0=1B, 1=2B, 2=3B, 3=4B, 4=5B, 5=6B)
//!   bits 3-5: num_children (for N4: 0-7)
//!   bit 6:    has_value
//!   bit 7:    has_prefix
//!
//! Byte 1:
//!   bits 0-3: prefix_len  (0-12)
//!   bits 4-5: node_type   (0=N4, 1=N16, 2=N48, 3=N256)
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
    pub const N256: u8 = 3;
}

// Re-export node type constants at top level for convenience
pub const COMPACT_NODE_TYPE_N4: u8 = compact_node_types::N4;
pub const COMPACT_NODE_TYPE_N16: u8 = compact_node_types::N16;
pub const COMPACT_NODE_TYPE_N48: u8 = compact_node_types::N48;
pub const COMPACT_NODE_TYPE_N256: u8 = compact_node_types::N256;

/// Compact node header (2 bytes for byte-keyed nodes)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactHeader {
    /// Width of pointer values in bytes (1-6)
    pub ptr_width: u8,
    /// Number of children
    pub num_children: u8,
    /// Whether this node has a value
    pub has_value: bool,
    /// Whether this node has a prefix
    pub has_prefix: bool,
    /// Prefix length (0-12)
    pub prefix_len: u8,
    /// Node type (N4, N16, N48, N256)
    pub node_type: u8,
}

/// Size of compact header in bytes (for byte-keyed nodes)
pub const COMPACT_HEADER_SIZE: usize = 2;

impl CompactHeader {
    /// Create a new compact header
    pub fn new(
        ptr_width: u8,
        num_children: u8,
        has_value: bool,
        prefix_len: u8,
        node_type: u8,
    ) -> Self {
        debug_assert!(ptr_width >= 1 && ptr_width <= 6);
        debug_assert!(prefix_len <= 12);
        debug_assert!(node_type <= 3);

        Self {
            ptr_width,
            num_children,
            has_value,
            has_prefix: prefix_len > 0,
            prefix_len,
            node_type,
        }
    }

    /// Encode header to 2 bytes (plus optional byte for large num_children)
    pub fn to_bytes(&self) -> [u8; 2] {
        // For num_children > 7, we store 7 as a sentinel
        let stored_children = self.num_children.min(7);

        let b0 = ((self.ptr_width - 1) & 0x07)
            | ((stored_children & 0x07) << 3)
            | ((self.has_value as u8) << 6)
            | ((self.has_prefix as u8) << 7);

        let b1 = (self.prefix_len & 0x0F)
            | ((self.node_type & 0x03) << 4)
            | if self.num_children > 7 { 0x40 } else { 0 }; // Extended count flag

        [b0, b1]
    }

    /// Encode header with extended num_children support
    pub fn to_bytes_with_extended(&self) -> ([u8; 2], Option<u8>) {
        let bytes = self.to_bytes();
        if self.num_children > 7 {
            (bytes, Some(self.num_children))
        } else {
            (bytes, None)
        }
    }

    /// Decode header from 2 bytes
    pub fn from_bytes(bytes: [u8; 2]) -> Self {
        let b0 = bytes[0];
        let b1 = bytes[1];

        let ptr_width = (b0 & 0x07) + 1;
        let stored_children = (b0 >> 3) & 0x07;
        let has_value = (b0 >> 6) & 0x01 != 0;
        let has_prefix = (b0 >> 7) != 0;

        let prefix_len = b1 & 0x0F;
        let node_type = (b1 >> 4) & 0x03;
        let has_extended = (b1 >> 6) & 0x01 != 0;

        let num_children = if has_extended { 7 } else { stored_children };

        Self {
            ptr_width,
            num_children,
            has_value,
            has_prefix,
            prefix_len,
            node_type,
        }
    }

    /// Decode header from bytes with optional extended num_children
    pub fn from_bytes_with_extended(data: &[u8], offset: &mut usize) -> Self {
        let mut header = Self::from_bytes([data[*offset], data[*offset + 1]]);
        *offset += COMPACT_HEADER_SIZE;

        // Check if extended count is needed
        if header.num_children == 7 && (data[*offset - 1] & 0x40) != 0 && *offset < data.len() {
            header.num_children = data[*offset];
            *offset += 1;
        }

        header
    }

    /// Check if this header requires extended num_children encoding
    pub fn needs_extended_count(&self) -> bool {
        self.num_children > 7
    }

    /// Calculate the total size of encoded data for this header
    ///
    /// For byte-keyed nodes: key_width is always 1
    pub fn data_size(&self) -> usize {
        COMPACT_HEADER_SIZE
            + if self.needs_extended_count() { 1 } else { 0 }
            + self.prefix_len as usize  // prefix (1 byte per char)
            + self.num_children as usize  // keys (1 byte per key)
            + (self.num_children as usize * self.ptr_width as usize) // children
            + if self.has_value { self.ptr_width as usize } else { 0 } // value_ptr
    }
}

// =============================================================================
// Varint Encoding (PathMap-style branchless)
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

/// Branchless varint read (from PathMap)
///
/// This implementation minimizes branching for better CPU pipelining
#[inline]
pub fn read_varint_branchless(data: &[u8]) -> (u64, usize) {
    let first = data[0];
    if first <= VARINT_LEN_BIAS {
        return (first as u64, 1);
    }
    let len = (first - VARINT_LEN_BIAS) as usize;
    // Read up to 8 bytes unaligned, then mask based on actual length
    let rest = if data.len() >= 9 {
        unsafe { data.as_ptr().add(1).cast::<u64>().read_unaligned() }
    } else {
        let mut buf = [0u8; 8];
        let copy_len = (data.len() - 1).min(8);
        buf[..copy_len].copy_from_slice(&data[1..1 + copy_len]);
        u64::from_le_bytes(buf)
    };
    let zeros = 64 - (len * 8);
    let value = if zeros < 64 {
        (rest << zeros) >> zeros
    } else {
        rest
    };
    (value, len + 1)
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
pub fn read_n_values_from_slice(
    data: &[u8],
    offset: &mut usize,
    count: usize,
    width: u8,
) -> Vec<u64> {
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
// Compact Node Serialization (Byte-keyed)
// =============================================================================

/// Serialize a byte-keyed node using compact encoding
///
/// # Arguments
/// * `header` - Pre-computed compact header with widths and metadata
/// * `prefix` - Prefix bytes
/// * `keys` - Child key bytes
/// * `children` - Child pointers/offsets
/// * `value_ptr` - Optional value pointer
///
/// # Returns
/// Encoded bytes
pub fn encode_compact_byte_node(
    header: &CompactHeader,
    prefix: &[u8],
    keys: &[u8],
    children: &[u64],
    value_ptr: Option<u64>,
) -> Vec<u8> {
    assert_eq!(keys.len(), children.len());

    let mut out = Vec::with_capacity(header.data_size());

    // Write header (with extended count if needed)
    let (header_bytes, extended_count) = header.to_bytes_with_extended();
    out.extend_from_slice(&header_bytes);

    // Write extended num_children byte if needed
    if let Some(count) = extended_count {
        out.push(count);
    }

    // Write prefix (1 byte per char for byte nodes)
    out.extend_from_slice(prefix);

    // Write keys (1 byte per key for byte nodes)
    out.extend_from_slice(keys);

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

/// Serialize a byte-keyed node using compact encoding (auto-compute widths)
pub fn encode_compact_byte_node_auto(
    node_type: u8,
    prefix: &[u8],
    keys: &[u8],
    children: &[u64],
    value_ptr: Option<u64>,
    max_arena_offset: u64,
) -> Vec<u8> {
    assert_eq!(keys.len(), children.len());

    // Determine optimal ptr_width
    let max_ptr = children
        .iter()
        .copied()
        .max()
        .unwrap_or(0)
        .max(value_ptr.unwrap_or(0))
        .max(max_arena_offset);

    let ptr_width = determine_ptr_width(max_ptr);

    // Build header
    let header = CompactHeader::new(
        ptr_width,
        keys.len() as u8,
        value_ptr.is_some(),
        prefix.len() as u8,
        node_type,
    );

    encode_compact_byte_node(&header, prefix, keys, children, value_ptr)
}

/// Decoded compact byte node data
#[derive(Debug, Clone)]
pub struct DecodedCompactByteNode {
    /// Header information
    pub header: CompactHeader,
    /// Prefix bytes
    pub prefix: Vec<u8>,
    /// Child keys
    pub keys: Vec<u8>,
    /// Child pointers
    pub children: Vec<u64>,
    /// Value pointer (None = no value)
    pub value_ptr: Option<u64>,
}

/// Decode a compact-encoded byte node from bytes
pub fn decode_compact_byte_node(data: &[u8]) -> DecodedCompactByteNode {
    let mut offset = 0;

    // Read header
    let header = CompactHeader::from_bytes_with_extended(data, &mut offset);

    // Read prefix
    let prefix: Vec<u8> = data[offset..offset + header.prefix_len as usize].to_vec();
    offset += header.prefix_len as usize;

    // Read keys
    let keys: Vec<u8> = data[offset..offset + header.num_children as usize].to_vec();
    offset += header.num_children as usize;

    // Read children
    let children = read_n_values_from_slice(
        data,
        &mut offset,
        header.num_children as usize,
        header.ptr_width,
    );

    // Read value_ptr if present
    let value_ptr = if header.has_value {
        Some(read_fixed_width_from_slice(
            data,
            &mut offset,
            header.ptr_width,
        ))
    } else {
        None
    };

    DecodedCompactByteNode {
        header,
        prefix,
        keys,
        children,
        value_ptr,
    }
}

/// Calculate the encoded size of a compact byte node without encoding
pub fn compact_byte_node_size(
    prefix_len: usize,
    num_children: usize,
    has_value: bool,
    max_ptr: u64,
) -> usize {
    let ptr_width = determine_ptr_width(max_ptr) as usize;

    COMPACT_HEADER_SIZE
        + if num_children > 7 { 1 } else { 0 }  // extended count
        + prefix_len  // prefix (1 byte per char)
        + num_children  // keys (1 byte per key)
        + (num_children * ptr_width)  // children
        + if has_value { ptr_width } else { 0 } // value_ptr
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
        let values = [
            248u64,
            255,
            256,
            1000,
            65535,
            65536,
            0xFFFFFF,
            0xFFFFFFFF,
            u64::MAX,
        ];
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
    fn test_varint_branchless() {
        let values = [0u64, 1, 100, 247, 248, 1000, 65535, 0xFFFFFF, u64::MAX];
        for &v in &values {
            let mut buf = Vec::new();
            write_varint_to_vec(v, &mut buf);
            // Pad for branchless read
            buf.extend_from_slice(&[0u8; 8]);
            let (decoded, consumed) = read_varint_branchless(&buf);
            assert_eq!(decoded, v);
            assert_eq!(consumed, varint_size(v));
        }
    }

    #[test]
    fn test_compact_header_roundtrip() {
        let header = CompactHeader::new(3, 4, true, 5, compact_node_types::N4);
        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), 2);
        let decoded = CompactHeader::from_bytes(bytes);

        assert_eq!(decoded.ptr_width, 3);
        assert_eq!(decoded.num_children, 4);
        assert_eq!(decoded.has_value, true);
        assert_eq!(decoded.prefix_len, 5);
        assert_eq!(decoded.node_type, compact_node_types::N4);
    }

    #[test]
    fn test_encode_decode_compact_byte_node() {
        let prefix = b"hel".to_vec();
        let keys = b"lp".to_vec();
        let children = vec![100u64, 200u64];
        let value_ptr = Some(300u64);

        let encoded = encode_compact_byte_node_auto(
            compact_node_types::N4,
            &prefix,
            &keys,
            &children,
            value_ptr,
            1000,
        );

        // Should be compact: 2 (header) + 3 (prefix) + 2 (keys) + 2×2 (children) + 2 (value)
        // = 2 + 3 + 2 + 4 + 2 = 13 bytes
        assert_eq!(encoded.len(), 13);

        let decoded = decode_compact_byte_node(&encoded);
        assert_eq!(decoded.prefix, prefix);
        assert_eq!(decoded.keys, keys);
        assert_eq!(decoded.children, children);
        assert_eq!(decoded.value_ptr, Some(300));
        assert!(decoded.header.has_value);
    }

    #[test]
    fn test_space_savings_byte_node() {
        // Typical byte node: fixed ~40+ bytes, compact ~8-12 bytes
        let prefix = b"abc".to_vec();
        let keys = b"de".to_vec();
        let children = vec![100u64, 200u64];

        let encoded = encode_compact_byte_node_auto(
            compact_node_types::N4,
            &prefix,
            &keys,
            &children,
            None,
            1000,
        );

        // 2 (header) + 3 (prefix) + 2 (keys) + 4 (children) = 11 bytes
        assert_eq!(encoded.len(), 11);
        assert!(encoded.len() < 15);
    }
}
