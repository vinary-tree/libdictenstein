//! Relative offset encoding for space-efficient child pointer storage.
//!
//! This module provides encoding utilities that minimize storage overhead by:
//! 1. Using relative offsets for same-arena child pointers
//! 2. Falling back to full encoding for cross-arena or forward same-arena pointers
//!
//! ## Encoding Scheme
//!
//! Child pointers use a flag bit to distinguish encoding modes:
//!
//! - **Bit 0 = 0**: Same-arena relative offset (remaining bits = varint delta)
//! - **Bit 0 = 1**: Full pointer (remaining bytes = arena_id:slot_id)
//!
//! ### Same-Arena Relative Offset
//!
//! When parent and child are in the same arena:
//! - Post-order serialization guarantees: `child_slot_id < parent_slot_id`
//! - Store `delta = parent_slot_id - child_slot_id` (always positive)
//! - Encode as varint: `(delta << 1) | 0` (flag bit = 0)
//!
//! Typical deltas are small (1-100), encoding to 1-2 bytes vs 8 bytes fixed.
//!
//! ### Full Pointer
//!
//! When parent and child are in different arenas, or when a same-arena child is
//! allocated after its parent:
//! - Store full (arena_id, slot_id) pair
//! - Encode as: `0x01 | arena_id (4 bytes) | slot_id (4 bytes)`
//!
//! ## Example
//!
//! ```rust,no_run
//! use libdictenstein::persistent_artrie::char::arena_manager::ArenaSlot;
//! use libdictenstein::persistent_artrie::char::relative_encoding::*;
//!
//! let parent = ArenaSlot::new(0, 100);
//! let child = ArenaSlot::new(0, 95);  // Same arena, delta = 5
//!
//! let mut buf = Vec::new();
//! encode_child_pointer(parent, child, &mut buf);
//!
//! // Encoded as 1 byte: (5 << 1) | 0 = 10
//! assert_eq!(buf.len(), 1);
//!
//! let (decoded, len) = decode_child_pointer(&buf, parent);
//! assert_eq!(decoded.arena_id, 0);
//! assert_eq!(decoded.slot_id, 95);
//! ```

use std::fmt;

use super::arena_manager::ArenaSlot;
use super::compact_encoding::{varint_size, write_varint_to_vec, VARINT_LEN_BIAS};

/// Flag bit indicating cross-arena encoding (bit 0 = 1)
pub const FLAG_CROSS_ARENA: u8 = 0x01;

/// Minimum size for cross-arena encoding: 1 byte flag + 4 bytes arena_id + 4 bytes slot_id
pub const CROSS_ARENA_SIZE: usize = 9;

/// Error returned by checked relative pointer encoding/decoding APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelativeEncodingError {
    /// The decoder was asked to read from an empty byte slice.
    EmptyInput,
    /// A full pointer was shorter than the required 9 bytes.
    TruncatedFullPointer { actual_len: usize },
    /// A varint header promised more payload bytes than were available.
    TruncatedVarint {
        expected_len: usize,
        actual_len: usize,
    },
    /// A full-pointer decoder was called on data with the wrong flag byte.
    InvalidFullPointerFlag { flag: u8 },
    /// Relative pointer payloads must have tag bit 0 cleared.
    OddRelativeTag { value: u64 },
    /// The decoded relative delta does not fit in a u32 slot offset.
    RelativeDeltaTooLarge { value: u64 },
    /// Applying the decoded delta would underflow the parent slot.
    RelativeUnderflow { parent: ArenaSlot, delta: u32 },
    /// Strict relative encoding requires the child to be at or before the parent.
    InvalidRelativeDirection { parent: ArenaSlot, child: ArenaSlot },
    /// A sequential child index cannot be represented as a u32 slot offset.
    SequentialIndexTooLarge { index: usize },
    /// Reconstructing sequential siblings would overflow u32.
    SequentialOverflow {
        first_child: ArenaSlot,
        index: usize,
    },
}

impl fmt::Display for RelativeEncodingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => write!(f, "empty relative pointer input"),
            Self::TruncatedFullPointer { actual_len } => write!(
                f,
                "truncated full pointer: got {actual_len} bytes, need {CROSS_ARENA_SIZE}"
            ),
            Self::TruncatedVarint {
                expected_len,
                actual_len,
            } => write!(
                f,
                "truncated varint: got {actual_len} bytes, need {expected_len}"
            ),
            Self::InvalidFullPointerFlag { flag } => {
                write!(f, "invalid full pointer flag byte: {flag:#04x}")
            }
            Self::OddRelativeTag { value } => {
                write!(f, "relative pointer has full-pointer tag bit set: {value}")
            }
            Self::RelativeDeltaTooLarge { value } => {
                write!(f, "relative delta exceeds u32 slot range: {value}")
            }
            Self::RelativeUnderflow { parent, delta } => write!(
                f,
                "relative delta {delta} underflows parent slot {:?}",
                parent
            ),
            Self::InvalidRelativeDirection { parent, child } => write!(
                f,
                "child slot {:?} cannot be relatively encoded from parent {:?}",
                child, parent
            ),
            Self::SequentialIndexTooLarge { index } => {
                write!(f, "sequential sibling index exceeds u32: {index}")
            }
            Self::SequentialOverflow { first_child, index } => write!(
                f,
                "sequential sibling index {index} overflows first child {:?}",
                first_child
            ),
        }
    }
}

impl std::error::Error for RelativeEncodingError {}

/// Result type for checked relative pointer operations.
pub type RelativeEncodingResult<T> = std::result::Result<T, RelativeEncodingError>;

// =============================================================================
// Header Flags for Relative Mode
// =============================================================================

/// Header flag indicating that child pointers use relative offset encoding
pub const FLAG_RELATIVE_OFFSETS: u8 = 0x80;

/// Header flag indicating sequential sibling storage
pub const FLAG_SEQUENTIAL_SIBLINGS: u8 = 0x40;

// =============================================================================
// Serialization Context
// =============================================================================

/// Context for relative encoding during serialization.
///
/// Coordinates the encoding mode (relative offsets, sequential siblings) and
/// provides the parent slot reference needed for relative encoding.
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
    /// Create a context for relative encoding.
    ///
    /// Uses relative offsets for same-arena children but stores each pointer individually.
    pub fn new(parent_slot: ArenaSlot) -> Self {
        Self {
            parent_slot,
            use_relative: true,
            use_sequential: false,
            first_child_slot: None,
        }
    }

    /// Create a context that forces full (absolute) encoding for all children.
    ///
    /// This is used when arena overflow is detected during serialization.
    /// Since the predicted parent slot may be in a different arena than the
    /// actual allocation, relative offsets would be invalid. Full encoding
    /// stores absolute (arena_id, slot_id) pairs for each child (9 bytes each).
    ///
    /// Note: This still uses the variable-width encoding format (FLAG_RELATIVE_OFFSETS
    /// is set), but all children will use cross-arena encoding since they're in
    /// different arenas than the parent.
    pub fn full_encoding(parent_slot: ArenaSlot) -> Self {
        Self {
            parent_slot,
            // Still use relative encoding format - it handles cross-arena with 9-byte encoding
            use_relative: true,
            use_sequential: false,
            first_child_slot: None,
        }
    }

    /// Create a context for sequential sibling storage.
    ///
    /// When children are consecutive in the same arena, stores only the first child slot
    /// plus a count instead of N separate pointers.
    pub fn sequential(parent_slot: ArenaSlot, first_child_slot: ArenaSlot) -> Self {
        Self {
            parent_slot,
            use_relative: true,
            use_sequential: true,
            first_child_slot: Some(first_child_slot),
        }
    }

    /// Returns true if using sequential sibling storage.
    #[inline]
    pub fn uses_sequential_siblings(&self) -> bool {
        self.use_sequential
    }

    /// Returns the encoding flags to set in the node header.
    pub fn encoding_flags(&self) -> u8 {
        let mut flags = 0u8;
        if self.use_relative {
            flags |= FLAG_RELATIVE_OFFSETS;
        }
        if self.use_sequential {
            flags |= FLAG_SEQUENTIAL_SIBLINGS;
        }
        flags
    }
}

impl Default for SerializationContext {
    fn default() -> Self {
        // Default context with no parent slot (uses relative=false)
        Self {
            parent_slot: ArenaSlot::new(0, 0),
            use_relative: false,
            use_sequential: false,
            first_child_slot: None,
        }
    }
}

// =============================================================================
// Encoding Functions
// =============================================================================

/// Encode a child pointer relative to the parent.
///
/// If parent and child are in the same arena and the child is at or before the
/// parent slot, encodes as a relative offset. Otherwise, encodes using a full
/// (arena_id, slot_id) pair.
///
/// # Arguments
/// * `parent` - Parent's arena slot (used for relative offset calculation)
/// * `child` - Child's arena slot to encode
/// * `out` - Output buffer to append encoded bytes
///
/// # Returns
/// Number of bytes written to `out`
pub fn encode_child_pointer(parent: ArenaSlot, child: ArenaSlot, out: &mut Vec<u8>) -> usize {
    if parent.arena_id == child.arena_id {
        if let Some(delta) = parent.slot_id.checked_sub(child.slot_id) {
            // Same arena, child allocated at or before the parent: use relative offset.
            encode_relative(delta, out)
        } else {
            // Same arena but not relatively representable. Fall back to full encoding
            // instead of saturating to zero, which would decode to the parent slot.
            encode_full(child, out)
        }
    } else {
        // Cross arena: use full encoding
        encode_full(child, out)
    }
}

/// Strictly encode a child pointer relative to the parent.
///
/// This checked variant rejects same-arena children that are allocated after
/// the parent. The infallible [`encode_child_pointer`] wrapper falls back to
/// full encoding for that case to preserve legacy call sites.
pub fn try_encode_child_pointer(
    parent: ArenaSlot,
    child: ArenaSlot,
    out: &mut Vec<u8>,
) -> RelativeEncodingResult<usize> {
    if parent.arena_id == child.arena_id {
        let delta = parent
            .slot_id
            .checked_sub(child.slot_id)
            .ok_or(RelativeEncodingError::InvalidRelativeDirection { parent, child })?;
        Ok(encode_relative(delta, out))
    } else {
        Ok(encode_full(child, out))
    }
}

/// Encode a relative offset (same-arena pointer).
///
/// Encodes `delta` as a varint with flag bit = 0 (indicating same-arena).
/// The value stored is `(delta << 1)` where bit 0 is always 0.
///
/// # Arguments
/// * `delta` - The offset difference: `parent.slot_id - child.slot_id`
/// * `out` - Output buffer to append encoded bytes
///
/// # Returns
/// Number of bytes written
#[inline]
pub fn encode_relative(delta: u32, out: &mut Vec<u8>) -> usize {
    // Shift left 1 bit, flag bit = 0 (same arena)
    let value = (delta as u64) << 1;
    let start_len = out.len();
    write_varint_to_vec(value, out);
    out.len() - start_len
}

/// Encode a full (arena_id, slot_id) pair (cross-arena pointer).
///
/// Format: `FLAG_CROSS_ARENA | arena_id (4 bytes LE) | slot_id (4 bytes LE)`
///
/// # Arguments
/// * `slot` - The arena slot to encode
/// * `out` - Output buffer to append encoded bytes
///
/// # Returns
/// Number of bytes written (always 9)
#[inline]
pub fn encode_full(slot: ArenaSlot, out: &mut Vec<u8>) -> usize {
    out.push(FLAG_CROSS_ARENA);
    out.extend_from_slice(&slot.arena_id.to_le_bytes());
    out.extend_from_slice(&slot.slot_id.to_le_bytes());
    CROSS_ARENA_SIZE
}

// =============================================================================
// Decoding Functions
// =============================================================================

/// Decode a child pointer that was encoded relative to the parent.
///
/// # Arguments
/// * `data` - Encoded bytes to decode from
/// * `parent` - Parent's arena slot (used to reconstruct absolute slot for relative encoding)
///
/// # Returns
/// Tuple of (decoded ArenaSlot, bytes consumed)
pub fn decode_child_pointer(data: &[u8], parent: ArenaSlot) -> (ArenaSlot, usize) {
    try_decode_child_pointer(data, parent).expect("invalid relative child pointer encoding")
}

/// Checked version of [`decode_child_pointer`].
///
/// Returns an error for empty, truncated, malformed, or underflowing encodings.
pub fn try_decode_child_pointer(
    data: &[u8],
    parent: ArenaSlot,
) -> RelativeEncodingResult<(ArenaSlot, usize)> {
    let first = *data.first().ok_or(RelativeEncodingError::EmptyInput)?;

    if first == FLAG_CROSS_ARENA {
        // Cross arena: full encoding (flag byte followed by arena_id and slot_id)
        try_decode_full(data)
    } else {
        // Same arena: relative offset (varint with bit 0 = 0)
        let (delta, len) = try_decode_relative(data)?;
        let child_slot = parent
            .slot_id
            .checked_sub(delta)
            .ok_or(RelativeEncodingError::RelativeUnderflow { parent, delta })?;
        Ok((ArenaSlot::new(parent.arena_id, child_slot), len))
    }
}

/// Decode a relative offset (same-arena pointer).
///
/// # Arguments
/// * `data` - Encoded varint bytes
///
/// # Returns
/// Tuple of (delta value, bytes consumed)
#[inline]
pub fn decode_relative(data: &[u8]) -> (u32, usize) {
    try_decode_relative(data).expect("invalid relative offset encoding")
}

/// Checked version of [`decode_relative`].
#[inline]
pub fn try_decode_relative(data: &[u8]) -> RelativeEncodingResult<(u32, usize)> {
    let (value, len) = try_read_varint_from_slice(data)?;
    if value & FLAG_CROSS_ARENA as u64 != 0 {
        return Err(RelativeEncodingError::OddRelativeTag { value });
    }
    let delta = value >> 1;
    if delta > u32::MAX as u64 {
        return Err(RelativeEncodingError::RelativeDeltaTooLarge { value: delta });
    }
    Ok((delta as u32, len))
}

/// Decode a full (arena_id, slot_id) pair (cross-arena pointer).
///
/// # Arguments
/// * `data` - Encoded bytes (must start with FLAG_CROSS_ARENA)
///
/// # Returns
/// Tuple of (decoded ArenaSlot, bytes consumed = 9)
#[inline]
pub fn decode_full(data: &[u8]) -> (ArenaSlot, usize) {
    try_decode_full(data).expect("invalid full child pointer encoding")
}

/// Checked version of [`decode_full`].
#[inline]
pub fn try_decode_full(data: &[u8]) -> RelativeEncodingResult<(ArenaSlot, usize)> {
    let first = *data.first().ok_or(RelativeEncodingError::EmptyInput)?;
    if first != FLAG_CROSS_ARENA {
        return Err(RelativeEncodingError::InvalidFullPointerFlag { flag: first });
    }
    if data.len() < CROSS_ARENA_SIZE {
        return Err(RelativeEncodingError::TruncatedFullPointer {
            actual_len: data.len(),
        });
    }

    let arena_id = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
    let slot_id = u32::from_le_bytes([data[5], data[6], data[7], data[8]]);

    Ok((ArenaSlot::new(arena_id, slot_id), CROSS_ARENA_SIZE))
}

#[inline]
fn try_read_varint_from_slice(data: &[u8]) -> RelativeEncodingResult<(u64, usize)> {
    let first = *data.first().ok_or(RelativeEncodingError::EmptyInput)?;
    if first <= VARINT_LEN_BIAS {
        Ok((first as u64, 1))
    } else {
        let len = (first - VARINT_LEN_BIAS) as usize;
        let expected_len = 1 + len;
        if data.len() < expected_len {
            return Err(RelativeEncodingError::TruncatedVarint {
                expected_len,
                actual_len: data.len(),
            });
        }
        let mut bytes = [0u8; 8];
        bytes[..len].copy_from_slice(&data[1..expected_len]);
        Ok((u64::from_le_bytes(bytes), expected_len))
    }
}

// =============================================================================
// Utility Functions
// =============================================================================

/// Check if a child pointer is in the same arena as the parent.
///
/// Same-arena children use relative encoding only when the child slot is at or
/// before the parent slot; otherwise encoding falls back to a full pointer.
#[inline]
pub fn is_same_arena(parent: ArenaSlot, child: ArenaSlot) -> bool {
    parent.arena_id == child.arena_id
}

/// Calculate the encoded size of a child pointer.
///
/// # Arguments
/// * `parent` - Parent's arena slot
/// * `child` - Child's arena slot
///
/// # Returns
/// Number of bytes required to encode the pointer
pub fn encoded_size(parent: ArenaSlot, child: ArenaSlot) -> usize {
    if parent.arena_id == child.arena_id && child.slot_id <= parent.slot_id {
        // Same arena: varint size of (delta << 1)
        let delta = parent.slot_id - child.slot_id;
        let value = (delta as u64) << 1;
        varint_size(value)
    } else {
        // Cross arena, or same arena but not relatively representable: fixed 9 bytes
        CROSS_ARENA_SIZE
    }
}

/// Calculate the size of the strict relative encoding.
///
/// Unlike [`encoded_size`], this rejects same-arena children that are allocated
/// after the parent instead of accounting for the full-encoding fallback.
pub fn try_encoded_size(parent: ArenaSlot, child: ArenaSlot) -> RelativeEncodingResult<usize> {
    if parent.arena_id == child.arena_id {
        let delta = parent
            .slot_id
            .checked_sub(child.slot_id)
            .ok_or(RelativeEncodingError::InvalidRelativeDirection { parent, child })?;
        Ok(varint_size((delta as u64) << 1))
    } else {
        Ok(CROSS_ARENA_SIZE)
    }
}

/// Encode multiple child pointers.
///
/// # Arguments
/// * `parent` - Parent's arena slot
/// * `children` - Child arena slots to encode
/// * `out` - Output buffer to append encoded bytes
///
/// # Returns
/// Total number of bytes written
pub fn encode_children(parent: ArenaSlot, children: &[ArenaSlot], out: &mut Vec<u8>) -> usize {
    let mut total = 0;
    for &child in children {
        total += encode_child_pointer(parent, child, out);
    }
    total
}

/// Strictly encode multiple child pointers.
pub fn try_encode_children(
    parent: ArenaSlot,
    children: &[ArenaSlot],
    out: &mut Vec<u8>,
) -> RelativeEncodingResult<usize> {
    let mut total = 0;
    for &child in children {
        total += try_encode_child_pointer(parent, child, out)?;
    }
    Ok(total)
}

/// Decode multiple child pointers.
///
/// # Arguments
/// * `data` - Encoded bytes
/// * `parent` - Parent's arena slot
/// * `count` - Number of children to decode
///
/// # Returns
/// Tuple of (decoded ArenaSlots, total bytes consumed)
pub fn decode_children(data: &[u8], parent: ArenaSlot, count: usize) -> (Vec<ArenaSlot>, usize) {
    try_decode_children(data, parent, count).expect("invalid relative child list encoding")
}

/// Checked version of [`decode_children`].
pub fn try_decode_children(
    data: &[u8],
    parent: ArenaSlot,
    count: usize,
) -> RelativeEncodingResult<(Vec<ArenaSlot>, usize)> {
    let mut children = Vec::with_capacity(count);
    let mut offset = 0;

    for _ in 0..count {
        let (child, len) = try_decode_child_pointer(&data[offset..], parent)?;
        children.push(child);
        offset += len;
    }

    Ok((children, offset))
}

// =============================================================================
// Sequential Siblings Support
// =============================================================================

/// Encode a sequential sibling reference (first_slot + count).
///
/// When children are allocated consecutively in the same arena:
/// - Store only the first child's slot reference
/// - Count is stored separately in the node header
///
/// Uses relative encoding if same arena as parent.
///
/// # Arguments
/// * `parent` - Parent's arena slot
/// * `first_child` - First child's arena slot
/// * `out` - Output buffer to append encoded bytes
///
/// # Returns
/// Number of bytes written
pub fn encode_sequential_siblings(
    parent: ArenaSlot,
    first_child: ArenaSlot,
    out: &mut Vec<u8>,
) -> usize {
    // Encode the first child slot using relative or full encoding
    encode_child_pointer(parent, first_child, out)
}

/// Strictly encode a sequential sibling reference.
pub fn try_encode_sequential_siblings(
    parent: ArenaSlot,
    first_child: ArenaSlot,
    out: &mut Vec<u8>,
) -> RelativeEncodingResult<usize> {
    try_encode_child_pointer(parent, first_child, out)
}

/// Decode a sequential sibling reference and reconstruct all child slots.
///
/// # Arguments
/// * `data` - Encoded bytes
/// * `parent` - Parent's arena slot
/// * `count` - Number of sequential children
///
/// # Returns
/// Tuple of (decoded child ArenaSlots, bytes consumed for first_slot encoding)
pub fn decode_sequential_siblings(
    data: &[u8],
    parent: ArenaSlot,
    count: usize,
) -> (Vec<ArenaSlot>, usize) {
    try_decode_sequential_siblings(data, parent, count)
        .expect("invalid sequential sibling encoding")
}

/// Checked version of [`decode_sequential_siblings`].
pub fn try_decode_sequential_siblings(
    data: &[u8],
    parent: ArenaSlot,
    count: usize,
) -> RelativeEncodingResult<(Vec<ArenaSlot>, usize)> {
    if count == 0 {
        return Ok((Vec::new(), 0));
    }

    // Decode first child slot
    let (first_child, bytes_consumed) = try_decode_child_pointer(data, parent)?;

    // Reconstruct all child slots (consecutive slot IDs)
    let mut children = Vec::with_capacity(count);
    for i in 0..count {
        let offset = u32::try_from(i)
            .map_err(|_| RelativeEncodingError::SequentialIndexTooLarge { index: i })?;
        let slot_id = first_child.slot_id.checked_add(offset).ok_or(
            RelativeEncodingError::SequentialOverflow {
                first_child,
                index: i,
            },
        )?;
        children.push(ArenaSlot::new(first_child.arena_id, slot_id));
    }

    Ok((children, bytes_consumed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_same_arena_relative_encoding() {
        let parent = ArenaSlot::new(0, 100);
        let child = ArenaSlot::new(0, 95); // Delta = 5

        let mut buf = Vec::new();
        let len = encode_child_pointer(parent, child, &mut buf);

        // (5 << 1) = 10, fits in single byte
        assert_eq!(len, 1);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], 10); // 5 << 1 = 10

        let (decoded, consumed) = decode_child_pointer(&buf, parent);
        assert_eq!(consumed, 1);
        assert_eq!(decoded.arena_id, 0);
        assert_eq!(decoded.slot_id, 95);
    }

    #[test]
    fn test_cross_arena_full_encoding() {
        let parent = ArenaSlot::new(0, 100);
        let child = ArenaSlot::new(1, 50); // Different arena

        let mut buf = Vec::new();
        let len = encode_child_pointer(parent, child, &mut buf);

        // Cross-arena: 1 + 4 + 4 = 9 bytes
        assert_eq!(len, CROSS_ARENA_SIZE);
        assert_eq!(buf.len(), CROSS_ARENA_SIZE);
        assert_eq!(buf[0], FLAG_CROSS_ARENA);

        let (decoded, consumed) = decode_child_pointer(&buf, parent);
        assert_eq!(consumed, CROSS_ARENA_SIZE);
        assert_eq!(decoded.arena_id, 1);
        assert_eq!(decoded.slot_id, 50);
    }

    #[test]
    fn test_zero_delta() {
        let parent = ArenaSlot::new(0, 100);
        let child = ArenaSlot::new(0, 100); // Same slot (edge case)

        let mut buf = Vec::new();
        let len = encode_child_pointer(parent, child, &mut buf);

        // Delta = 0, (0 << 1) = 0, single byte
        assert_eq!(len, 1);
        assert_eq!(buf[0], 0);

        let (decoded, _) = decode_child_pointer(&buf, parent);
        assert_eq!(decoded.slot_id, 100);
    }

    #[test]
    fn test_large_delta() {
        let parent = ArenaSlot::new(0, 100000);
        let child = ArenaSlot::new(0, 0); // Delta = 100000

        let mut buf = Vec::new();
        let len = encode_child_pointer(parent, child, &mut buf);

        // (100000 << 1) = 200000, needs 3-4 bytes varint
        assert!(len > 1 && len < CROSS_ARENA_SIZE);

        let (decoded, _) = decode_child_pointer(&buf, parent);
        assert_eq!(decoded.arena_id, 0);
        assert_eq!(decoded.slot_id, 0);
    }

    #[test]
    fn test_encoded_size() {
        let parent = ArenaSlot::new(0, 100);
        let child_same = ArenaSlot::new(0, 95);
        let child_cross = ArenaSlot::new(1, 50);

        // Same arena: small delta = 1 byte
        assert_eq!(encoded_size(parent, child_same), 1);

        // Cross arena: always 9 bytes
        assert_eq!(encoded_size(parent, child_cross), CROSS_ARENA_SIZE);
    }

    #[test]
    fn test_encode_decode_children() {
        let parent = ArenaSlot::new(0, 100);
        let children = vec![
            ArenaSlot::new(0, 90), // Same arena
            ArenaSlot::new(0, 80), // Same arena
            ArenaSlot::new(1, 50), // Different arena
        ];

        let mut buf = Vec::new();
        let total_len = encode_children(parent, &children, &mut buf);

        let (decoded, consumed) = decode_children(&buf, parent, children.len());

        assert_eq!(consumed, total_len);
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0], children[0]);
        assert_eq!(decoded[1], children[1]);
        assert_eq!(decoded[2], children[2]);
    }

    #[test]
    fn test_sequential_siblings() {
        let parent = ArenaSlot::new(0, 100);
        let first_child = ArenaSlot::new(0, 80);
        let count = 4;

        let mut buf = Vec::new();
        let len = encode_sequential_siblings(parent, first_child, &mut buf);

        // Should encode just the first child (relative)
        assert!(len < CROSS_ARENA_SIZE);

        let (children, consumed) = decode_sequential_siblings(&buf, parent, count);

        assert_eq!(consumed, len);
        assert_eq!(children.len(), count);
        assert_eq!(children[0], ArenaSlot::new(0, 80));
        assert_eq!(children[1], ArenaSlot::new(0, 81));
        assert_eq!(children[2], ArenaSlot::new(0, 82));
        assert_eq!(children[3], ArenaSlot::new(0, 83));
    }

    #[test]
    fn test_sequential_siblings_cross_arena() {
        let parent = ArenaSlot::new(0, 100);
        let first_child = ArenaSlot::new(1, 0); // Different arena
        let count = 3;

        let mut buf = Vec::new();
        let len = encode_sequential_siblings(parent, first_child, &mut buf);

        // Cross arena = full encoding
        assert_eq!(len, CROSS_ARENA_SIZE);

        let (children, _) = decode_sequential_siblings(&buf, parent, count);

        assert_eq!(children.len(), count);
        assert_eq!(children[0], ArenaSlot::new(1, 0));
        assert_eq!(children[1], ArenaSlot::new(1, 1));
        assert_eq!(children[2], ArenaSlot::new(1, 2));
    }

    #[test]
    fn test_space_savings() {
        // Compare space usage: relative vs fixed
        let parent = ArenaSlot::new(0, 1000);

        // Typical case: children allocated just before parent
        let children: Vec<ArenaSlot> = (990..1000).rev().map(|s| ArenaSlot::new(0, s)).collect();

        let mut buf = Vec::new();
        let relative_size = encode_children(parent, &children, &mut buf);

        // Fixed encoding would be 8 bytes per child = 80 bytes
        let fixed_size = children.len() * 8;

        // Relative should be much smaller (1-2 bytes per child for small deltas)
        assert!(relative_size < fixed_size);
        println!(
            "Space savings: {} bytes relative vs {} bytes fixed ({:.1}% reduction)",
            relative_size,
            fixed_size,
            (1.0 - relative_size as f64 / fixed_size as f64) * 100.0
        );
    }

    // =========================================================================
    // SerializationContext Tests
    // =========================================================================

    #[test]
    fn test_serialization_context_new() {
        let parent = ArenaSlot::new(5, 100);
        let ctx = SerializationContext::new(parent);

        assert_eq!(ctx.parent_slot.arena_id, 5);
        assert_eq!(ctx.parent_slot.slot_id, 100);
        assert!(ctx.use_relative);
        assert!(!ctx.use_sequential);
        assert!(ctx.first_child_slot.is_none());
        assert!(!ctx.uses_sequential_siblings());
    }

    #[test]
    fn test_serialization_context_sequential() {
        let parent = ArenaSlot::new(5, 100);
        let first_child = ArenaSlot::new(5, 90);
        let ctx = SerializationContext::sequential(parent, first_child);

        assert_eq!(ctx.parent_slot.arena_id, 5);
        assert_eq!(ctx.parent_slot.slot_id, 100);
        assert!(ctx.use_relative);
        assert!(ctx.use_sequential);
        assert!(ctx.first_child_slot.is_some());
        assert_eq!(ctx.first_child_slot.unwrap().slot_id, 90);
        assert!(ctx.uses_sequential_siblings());
    }

    #[test]
    fn test_serialization_context_encoding_flags() {
        let parent = ArenaSlot::new(0, 100);

        // Relative only
        let ctx_rel = SerializationContext::new(parent);
        assert_eq!(ctx_rel.encoding_flags(), FLAG_RELATIVE_OFFSETS);

        // Sequential (includes relative)
        let first = ArenaSlot::new(0, 90);
        let ctx_seq = SerializationContext::sequential(parent, first);
        assert_eq!(
            ctx_seq.encoding_flags(),
            FLAG_RELATIVE_OFFSETS | FLAG_SEQUENTIAL_SIBLINGS
        );

        // Default (no encoding)
        let ctx_def = SerializationContext::default();
        assert_eq!(ctx_def.encoding_flags(), 0);
    }

    #[test]
    fn test_serialization_context_default() {
        let ctx = SerializationContext::default();

        assert_eq!(ctx.parent_slot.arena_id, 0);
        assert_eq!(ctx.parent_slot.slot_id, 0);
        assert!(!ctx.use_relative);
        assert!(!ctx.use_sequential);
        assert!(ctx.first_child_slot.is_none());
    }
}
