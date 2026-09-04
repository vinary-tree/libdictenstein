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
    /// A payload digit must fit in the seven-bit ULEB128 payload.
    InvalidPayload,
}

impl fmt::Display for Uleb128Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "empty ULEB128 atom",
            Self::Unterminated => "unterminated ULEB128 atom",
            Self::NonCanonical => "non-canonical ULEB128 atom",
            Self::InvalidPayload => "ULEB128 payload digit exceeds seven bits",
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

/// Borrowed validated view of a canonical ULEB128 atom.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Uleb128Ref<'a>(&'a [u8]);

/// Iterator over concatenated canonical ULEB128 atoms in one immutable image.
pub struct Uleb128Stream<'a> {
    remaining: &'a [u8],
    failed: bool,
}

/// Stable metadata identifying a variable-width wire profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct VariableWidthProfile {
    /// Globally names the logical encoding family.
    pub name: &'static str,
    /// Evolves when canonical wire semantics change.
    pub version: u16,
}

impl VariableWidthProfile {
    /// Construct profile identity metadata.
    pub const fn new(name: &'static str, version: u16) -> Self {
        Self { name, version }
    }
}

/// Codec contract for variable-width logical atoms.
pub trait VariableWidthCodec {
    /// Stable profile identity that must be persisted with dictionary images.
    const PROFILE: VariableWidthProfile;
    /// Owned atom used when a value must outlive its source image.
    type Owned: Clone + Eq + Ord;
    /// Borrowed validated view used by zero-copy traversal.
    type View<'a>: Copy
    where
        Self: 'a;

    /// Validate and borrow one complete canonical atom.
    fn borrow<'a>(bytes: &'a [u8]) -> Result<Self::View<'a>, Uleb128Error>;
    /// Encode an owned atom into canonical wire bytes.
    fn encode(value: &Self::Owned) -> Vec<u8>;
    /// Materialise an owned atom from canonical wire bytes.
    fn decode(bytes: &[u8]) -> Result<Self::Owned, Uleb128Error>;
    /// Decode one atom at the front of a concatenated image and report bytes
    /// consumed, without interpreting any following atom.
    fn decode_prefix(bytes: &[u8]) -> Result<(Self::Owned, usize), Uleb128Error>;
}

/// ULEB128 implementation of [`VariableWidthCodec`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Uleb128Codec;

/// Canonical ULEB128 profile identity.
pub const ULEB128_PROFILE: VariableWidthProfile = VariableWidthProfile::new("uleb128", 1);

impl VariableWidthCodec for Uleb128Codec {
    const PROFILE: VariableWidthProfile = ULEB128_PROFILE;
    type Owned = Uleb128;
    type View<'a> = Uleb128Ref<'a>;

    #[inline]
    fn borrow<'a>(bytes: &'a [u8]) -> Result<Self::View<'a>, Uleb128Error> {
        Uleb128Ref::new(bytes)
    }

    #[inline]
    fn encode(value: &Self::Owned) -> Vec<u8> {
        value.as_bytes().to_vec()
    }

    #[inline]
    fn decode(bytes: &[u8]) -> Result<Self::Owned, Uleb128Error> {
        Uleb128::from_bytes(bytes)
    }

    #[inline]
    fn decode_prefix(bytes: &[u8]) -> Result<(Self::Owned, usize), Uleb128Error> {
        let (view, consumed) = Uleb128Ref::from_prefix(bytes)?;
        Ok((view.to_owned(), consumed))
    }
}

impl Default for Uleb128 {
    fn default() -> Self {
        Self(vec![0])
    }
}

impl fmt::Debug for Uleb128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Uleb128").field(&self.0).finish()
    }
}

impl Uleb128 {
    /// Validate and copy one canonical wire encoding.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Uleb128Error> {
        Uleb128Ref::try_from(bytes)?;
        Ok(Self(bytes.to_vec()))
    }

    /// Encode canonical base-128 payload digits in least-significant-first
    /// order.  This is the direct arbitrary-width form used by dictionary
    /// profiles; unlike `from_le_bytes`, it does not reinterpret digits as a
    /// base-256 magnitude.
    pub fn from_payload_digits(digits: &[u8]) -> Result<Self, Uleb128Error> {
        if digits.is_empty() {
            return Ok(Self(vec![0]));
        }
        if digits.iter().any(|&digit| digit >= 128) {
            return Err(Uleb128Error::InvalidPayload);
        }
        let mut bytes = digits.to_vec();
        while bytes.len() > 1 && bytes.last() == Some(&0) {
            bytes.pop();
        }
        let continuation_len = bytes.len().saturating_sub(1);
        for byte in &mut bytes[..continuation_len] {
            *byte |= 0x80;
        }
        Ok(Self(bytes))
    }

    /// Encode a machine-width value using the same canonical representation.
    pub fn from_u64(mut value: u64) -> Self {
        let mut bytes = Vec::with_capacity(10);
        loop {
            let payload = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                bytes.push(payload);
                break;
            }
            bytes.push(payload | 0x80);
        }
        Self(bytes)
    }

    /// Decode into `u64` when the atom fits; return `None` for wider values.
    pub fn to_u64(&self) -> Option<u64> {
        let mut value = 0u64;
        for (index, &byte) in self.0.iter().enumerate() {
            let shift = u32::try_from(index).ok()?.checked_mul(7)?;
            let payload = u64::from(byte & 0x7f);
            if shift >= u64::BITS || payload > (u64::MAX >> shift) {
                return None;
            }
            value = value.checked_add(payload << shift)?;
        }
        Some(value)
    }

    /// Return the base-128 payload digits in least-significant-first order.
    pub fn to_payload_digits(&self) -> Vec<u8> {
        self.0.iter().map(|byte| byte & 0x7f).collect()
    }

    /// Encode an unsigned magnitude represented as little-endian base-256
    /// bytes.  Most-significant zero bytes (the slice's trailing bytes) are
    /// ignored; an empty magnitude is zero.
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

    /// Iterate over payload digits without allocating or decoding.
    #[inline]
    pub fn payload_digits(&self) -> impl Iterator<Item = u8> + '_ {
        self.0.iter().map(|byte| byte & 0x7f)
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

impl AsRef<[u8]> for Uleb128 {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl<'a> AsRef<[u8]> for Uleb128Ref<'a> {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.0
    }
}

impl<'a> Uleb128Ref<'a> {
    /// Validate a borrowed canonical wire encoding without allocating.
    #[inline]
    pub fn new(bytes: &'a [u8]) -> Result<Self, Uleb128Error> {
        validate(bytes)?;
        Ok(Self(bytes))
    }

    /// Borrow exactly the first complete atom from a byte stream.
    ///
    /// The returned offset is the number of bytes consumed; bytes after the
    /// terminator are untouched and may contain the next logical atom.
    pub fn from_prefix(bytes: &'a [u8]) -> Result<(Self, usize), Uleb128Error> {
        let Some(end) = bytes.iter().position(|byte| byte & 0x80 == 0) else {
            return Err(if bytes.is_empty() {
                Uleb128Error::Empty
            } else {
                Uleb128Error::Unterminated
            });
        };
        let consumed = end + 1;
        let view = Self::new(&bytes[..consumed])?;
        Ok((view, consumed))
    }

    /// Construct a zero-allocation iterator over concatenated atoms.
    #[inline]
    pub fn stream(bytes: &'a [u8]) -> Uleb128Stream<'a> {
        Uleb128Stream {
            remaining: bytes,
            failed: false,
        }
    }

    /// Return the exact borrowed wire bytes.
    #[inline]
    pub fn as_bytes(self) -> &'a [u8] {
        self.0
    }

    /// Copy this view into an owned atom.
    #[inline]
    pub fn to_owned(self) -> Uleb128 {
        Uleb128(self.0.to_vec())
    }

    /// Compare arbitrary-width values without decoding them.
    #[inline]
    pub fn numeric_cmp(self, other: Self) -> Ordering {
        self.0.len().cmp(&other.0.len()).then_with(|| {
            self.0
                .iter()
                .rev()
                .map(|b| b & 0x7f)
                .cmp(other.0.iter().rev().map(|b| b & 0x7f))
        })
    }
}

impl<'a> Iterator for Uleb128Stream<'a> {
    type Item = Result<Uleb128Ref<'a>, Uleb128Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining.is_empty() {
            return None;
        }
        match Uleb128Ref::from_prefix(self.remaining) {
            Ok((atom, consumed)) => {
                self.remaining = &self.remaining[consumed..];
                Some(Ok(atom))
            }
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

impl<'a> TryFrom<&'a [u8]> for Uleb128Ref<'a> {
    type Error = Uleb128Error;

    fn try_from(bytes: &'a [u8]) -> Result<Self, Self::Error> {
        Self::new(bytes)
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

/// Owned sequence generic over any variable-width codec.
#[derive(Clone, Debug)]
pub struct VariableAtomSequence<C: VariableWidthCodec> {
    atoms: Vec<C::Owned>,
    marker: core::marker::PhantomData<C>,
}

impl<C: VariableWidthCodec> Default for VariableAtomSequence<C> {
    fn default() -> Self {
        Self {
            atoms: Vec::new(),
            marker: core::marker::PhantomData,
        }
    }
}

impl<C: VariableWidthCodec> PartialEq for VariableAtomSequence<C> {
    fn eq(&self, other: &Self) -> bool {
        self.atoms == other.atoms
    }
}

impl<C: VariableWidthCodec> Eq for VariableAtomSequence<C> {}

impl<C: VariableWidthCodec> VariableAtomSequence<C> {
    /// Construct an empty sequence.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a sequence from owned atoms.
    pub fn from_atoms<I>(atoms: I) -> Self
    where
        I: IntoIterator<Item = C::Owned>,
    {
        Self {
            atoms: atoms.into_iter().collect(),
            marker: core::marker::PhantomData,
        }
    }

    /// Decode a complete concatenated image.
    pub fn from_encoded(bytes: &[u8]) -> Result<Self, Uleb128Error> {
        let mut atoms = Vec::new();
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let (atom, consumed) = C::decode_prefix(remaining)?;
            if consumed == 0 || consumed > remaining.len() {
                return Err(Uleb128Error::Unterminated);
            }
            atoms.push(atom);
            remaining = &remaining[consumed..];
        }
        Ok(Self::from_atoms(atoms))
    }

    /// Append one atom.
    #[inline]
    pub fn push(&mut self, atom: C::Owned) {
        self.atoms.push(atom);
    }

    /// Number of logical atoms.
    #[inline]
    pub fn len(&self) -> usize {
        self.atoms.len()
    }

    /// Whether this sequence is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }

    /// Number of bytes in the encoded sequence.
    #[inline]
    pub fn encoded_len(&self) -> usize {
        self.atoms.iter().map(|atom| C::encode(atom).len()).sum()
    }

    /// Encode the sequence in logical order.
    pub fn to_encoded(&self) -> Vec<u8> {
        let mut encoded = Vec::new();
        for atom in &self.atoms {
            encoded.extend_from_slice(&C::encode(atom));
        }
        encoded
    }

    /// Iterate owned atoms without decoding.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &C::Owned> {
        self.atoms.iter()
    }
}

/// Backwards-compatible name for the ULEB128 specialization.
pub type Uleb128Sequence = VariableAtomSequence<Uleb128Codec>;

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

    #[test]
    fn borrowed_view_is_zero_copy_and_validated() {
        let wire = [0x80, 0x01];
        let view = Uleb128Ref::new(&wire).unwrap();
        assert_eq!(view.as_bytes().as_ptr(), wire.as_ptr());
        assert_eq!(view.to_owned().as_bytes(), &wire);
    }

    #[test]
    fn payload_digit_form_matches_profile_reference() {
        let value = Uleb128::from_payload_digits(&[3, 4]).unwrap();
        assert_eq!(value.as_bytes(), &[0x83, 0x04]);
        assert_eq!(value.to_payload_digits(), vec![3, 4]);
        assert_eq!(
            Uleb128::from_payload_digits(&[128]),
            Err(Uleb128Error::InvalidPayload)
        );
    }

    #[test]
    fn codec_exposes_stable_profile_identity() {
        assert_eq!(Uleb128Codec::PROFILE, ULEB128_PROFILE);
        assert_eq!(ULEB128_PROFILE.name, "uleb128");
        assert_eq!(ULEB128_PROFILE.version, 1);
    }

    #[test]
    fn bounded_u64_fast_path_refines_arbitrary_width_form() {
        for value in [0, 1, 127, 128, u64::MAX] {
            let atom = Uleb128::from_u64(value);
            assert_eq!(atom.to_u64(), Some(value));
            assert_eq!(Uleb128::from_bytes(atom.as_bytes()).unwrap(), atom);
        }
        let wide = Uleb128::from_payload_digits(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 2]).unwrap();
        assert_eq!(wide.to_u64(), None);
    }

    #[test]
    fn known_uleb_encoding_and_zero_copy_payload_iteration() {
        let value = Uleb128::from_u64(624_485);
        assert_eq!(value.as_bytes(), &[0xe5, 0x8e, 0x26]);
        assert_eq!(value.payload_digits().collect::<Vec<_>>(), vec![0x65, 0x0e, 0x26]);
        assert_eq!(AsRef::<[u8]>::as_ref(&value), value.as_bytes());
    }

    #[test]
    fn borrowed_prefix_parser_is_bounded_and_preserves_suffix() {
        let stream = [0x83, 0x04, 0x01];
        let (first, consumed) = Uleb128Ref::from_prefix(&stream).unwrap();
        assert_eq!(first.as_bytes(), &[0x83, 0x04]);
        assert_eq!(&stream[consumed..], &[0x01]);
        assert_eq!(Uleb128Ref::from_prefix(&[0x80]), Err(Uleb128Error::Unterminated));
    }

    #[test]
    fn stream_iterator_preserves_atom_boundaries_and_fails_closed() {
        let stream = [0x83, 0x04, 0x01, 0x80];
        let mut atoms = Uleb128Ref::stream(&stream);
        assert_eq!(atoms.next().unwrap().unwrap().as_bytes(), &[0x83, 0x04]);
        assert_eq!(atoms.next().unwrap().unwrap().as_bytes(), &[0x01]);
        assert_eq!(atoms.next(), Some(Err(Uleb128Error::Unterminated)));
        assert_eq!(atoms.next(), None);
    }

    #[test]
    fn owned_sequence_round_trips_concatenated_atoms() {
        let sequence = Uleb128Sequence::from_atoms([
            Uleb128::from_u64(1),
            Uleb128::from_payload_digits(&[3, 4]).unwrap(),
            Uleb128::from_u64(u64::MAX),
        ]);
        let encoded = sequence.to_encoded();
        let decoded = Uleb128Sequence::from_encoded(&encoded).unwrap();
        assert_eq!(decoded, sequence);
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded.encoded_len(), encoded.len());
    }
}
