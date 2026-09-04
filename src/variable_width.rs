//! Canonical, arbitrary-width unsigned LEB128 values.
//!
//! `Uleb128` deliberately stores the encoded atom rather than eagerly
//! materialising a machine integer.  This keeps dictionary edges usable for
//! values wider than any built-in type while retaining an allocation-free
//! borrowed view for traversal and equality checks.

use core::cmp::Ordering;
use core::fmt;

/// Errors reported when a ULEB128 atom is not a complete canonical encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Uleb128Error {
    /// An atom must contain at least one byte.
    Empty,
    /// The final byte must terminate the encoding.
    Unterminated,
    /// A multi-byte atom may not contain a redundant zero group.
    NonCanonical,
}

impl fmt::Display for Uleb128Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "empty ULEB128 atom",
            Self::Unterminated => "unterminated ULEB128 atom",
            Self::NonCanonical => "non-canonical ULEB128 atom",
        })
    }
}

impl std::error::Error for Uleb128Error {}

/// A validated canonical ULEB128 unsigned integer of arbitrary width.
///
/// The bytes are retained in wire order.  Consequently `as_bytes` is a
/// zero-copy representation suitable for a variable-width dictionary edge;
/// no conversion to `u128` (or any other bounded type) is required.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct Uleb128(Vec<u8>);

impl fmt::Debug for Uleb128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Uleb128").field(&self.0).finish()
    }
}

impl Uleb128 {
    /// Validate and copy one canonical wire encoding.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Uleb128Error> {
        validate(bytes)?;
        Ok(Self(bytes.to_vec()))
    }

    /// Encode an unsigned magnitude represented as little-endian base-256
    /// bytes.  Leading zero bytes are ignored; an empty magnitude is zero.
    pub fn from_le_bytes(magnitude: &[u8]) -> Self {
        let mut limbs = Vec::with_capacity(magnitude.len().saturating_mul(8) / 7 + 1);
        let mut accumulator = 0u16;
        let mut bits = 0u8;
        for &byte in magnitude {
            accumulator |= u16::from(byte) << bits;
            bits += 8;
            while bits >= 7 {
                limbs.push((accumulator & 0x7f) as u8);
                accumulator >>= 7;
                bits -= 7;
            }
        }
        if bits != 0 {
            limbs.push(accumulator as u8);
        }
        while limbs.len() > 1 && limbs.last() == Some(&0) {
            limbs.pop();
        }
        if limbs.is_empty() {
            limbs.push(0);
        }
        let continuation_len = limbs.len().saturating_sub(1);
        for byte in &mut limbs[..continuation_len] {
            *byte |= 0x80;
        }
        Self(limbs)
    }

    /// Return the canonical wire bytes without allocating.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Decode to a little-endian base-256 magnitude.
    pub fn to_le_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut accumulator = 0u32;
        let mut bits = 0u8;
        for &byte in &self.0 {
            accumulator |= u32::from(byte & 0x7f) << bits;
            bits += 7;
            while bits >= 8 {
                out.push((accumulator & 0xff) as u8);
                accumulator >>= 8;
                bits -= 8;
            }
        }
        if bits != 0 {
            out.push(accumulator as u8);
        }
        while out.len() > 1 && out.last() == Some(&0) {
            out.pop();
        }
        if out.is_empty() {
            out.push(0);
        }
        out
    }

    /// Compare two arbitrary-width unsigned values without decoding them.
    pub fn numeric_cmp(&self, other: &Self) -> Ordering {
        match self.0.len().cmp(&other.0.len()) {
            Ordering::Equal => self
                .0
                .iter()
                .rev()
                .map(|b| b & 0x7f)
                .cmp(other.0.iter().rev().map(|b| b & 0x7f)),
            order => order,
        }
    }
}

impl Ord for Uleb128 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.numeric_cmp(other)
    }
}

impl PartialOrd for Uleb128 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn validate(bytes: &[u8]) -> Result<(), Uleb128Error> {
    let Some(&last) = bytes.last() else {
        return Err(Uleb128Error::Empty);
    };
    if last & 0x80 != 0 {
        return Err(Uleb128Error::Unterminated);
    }
    if bytes.len() > 1 && last & 0x7f == 0 {
        return Err(Uleb128Error::NonCanonical);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_width_round_trip() {
        let magnitude = vec![0x00, 0xff, 0x01, 0x80, 0x7f, 0x42];
        let value = Uleb128::from_le_bytes(&magnitude);
        assert_eq!(value.to_le_bytes(), vec![0x00, 0xff, 0x01, 0x80, 0x7f, 0x42]);
        assert_eq!(Uleb128::from_bytes(value.as_bytes()).unwrap(), value);
    }

    #[test]
    fn malformed_encodings_are_rejected() {
        assert_eq!(Uleb128::from_bytes(&[]), Err(Uleb128Error::Empty));
        assert_eq!(Uleb128::from_bytes(&[0x80]), Err(Uleb128Error::Unterminated));
        assert_eq!(Uleb128::from_bytes(&[0x80, 0]), Err(Uleb128Error::NonCanonical));
    }

    #[test]
    fn numeric_order_does_not_use_lexical_wire_order() {
        let one = Uleb128::from_bytes(&[1]).unwrap();
        let one_twenty_eight = Uleb128::from_bytes(&[0x80, 1]).unwrap();
        assert!(one < one_twenty_eight);
    }
}
