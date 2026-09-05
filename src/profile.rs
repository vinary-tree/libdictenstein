//! Shared logical-atom profiles for generic dictionary families.

use crate::variable_width::{Uleb128, Uleb128Ref, VariableWidthProfile, ULEB128_PROFILE};
use core::marker::PhantomData;

/// Errors returned by logical-atom profile decoders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileError {
    /// The input does not contain one complete atom.
    InvalidLength,
    /// A scalar profile received a value outside the Unicode scalar range.
    InvalidScalar,
    /// The input is not one complete valid UTF-8 scalar encoding.
    InvalidUtf8,
    /// The input is not one complete canonical variable-width atom.
    InvalidEncoding,
}

/// Stable descriptor for the built-in logical alphabets.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ProfileKind {
    /// Raw bytes.
    Bytes,
    /// Unicode scalar values (not UTF-8 byte transitions).
    UnicodeScalar,
    /// UTF-8 encoded Unicode scalar values (one variable-width codeword per scalar).
    Utf8,
    /// Native 32-bit unsigned values.
    U32,
    /// Native 64-bit unsigned values.
    U64,
    /// IEEE-754 binary64 represented by raw `u64` bits.
    F64Bits,
    /// Arbitrary-width canonical ULEB128 atoms.
    Uleb128,
}

impl ProfileKind {
    /// Canonical persisted name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bytes => "bytes",
            Self::UnicodeScalar => "unicode-scalar",
            Self::Utf8 => "utf8",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::F64Bits => "f64-bits",
            Self::Uleb128 => "uleb128",
        }
    }

    /// Parse a canonical persisted name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "bytes" => Some(Self::Bytes),
            "unicode-scalar" => Some(Self::UnicodeScalar),
            "utf8" => Some(Self::Utf8),
            "u32" => Some(Self::U32),
            "u64" => Some(Self::U64),
            "f64-bits" => Some(Self::F64Bits),
            "uleb128" => Some(Self::Uleb128),
            _ => None,
        }
    }

    /// Legacy public type name, when one exists.  These names are aliases for
    /// source compatibility only and are not accepted as persisted metadata.
    pub const fn legacy_name(self) -> Option<&'static str> {
        match self {
            Self::Bytes => Some("DynamicDawg"),
            Self::UnicodeScalar => Some("DynamicDawgChar"),
            Self::Utf8 => None,
            Self::U64 => Some("DynamicDawgU64"),
            Self::U32 | Self::F64Bits | Self::Uleb128 => None,
        }
    }

    /// Resolve a profile only when both name and version match exactly.
    pub fn from_identity(identity: VariableWidthProfile) -> Option<Self> {
        let kind = Self::from_name(identity.name)?;
        (kind.identity() == identity).then_some(kind)
    }

    /// Stable persisted identity.
    pub const fn identity(self) -> VariableWidthProfile {
        match self {
            Self::Bytes => Bytes::PROFILE,
            Self::UnicodeScalar => UnicodeScalar::PROFILE,
            Self::Utf8 => Utf8::PROFILE,
            Self::U32 => U32::PROFILE,
            Self::U64 => U64::PROFILE,
            Self::F64Bits => F64Bits::PROFILE,
            Self::Uleb128 => ULEB128_PROFILE,
        }
    }

    /// Fixed wire width, or `None` for variable-width ULEB atoms.
    pub const fn width_bytes(self) -> Option<usize> {
        match self {
            Self::Uleb128 => None,
            Self::Utf8 => None,
            Self::Bytes => Bytes::WIDTH_BYTES,
            Self::UnicodeScalar => UnicodeScalar::WIDTH_BYTES,
            Self::U32 => U32::WIDTH_BYTES,
            Self::U64 | Self::F64Bits => Some(8),
        }
    }
}

/// Codec contract for one logical dictionary edge.
pub trait AtomProfile {
    /// Logical value represented by one edge.
    type Atom: Eq + Ord;

    /// Stable persisted profile identity.
    const PROFILE: VariableWidthProfile;
    /// Built-in kind corresponding to the profile identity.
    const KIND: ProfileKind;
    /// Fixed wire width in bytes, or `None` for variable-width profiles.
    const WIDTH_BYTES: Option<usize>;

    /// Encode one logical atom.
    fn encode(atom: &Self::Atom) -> Vec<u8>;
    /// Decode one atom from the beginning of `bytes`, returning the atom and
    /// the number of bytes consumed.
    fn decode(bytes: &[u8]) -> Result<(Self::Atom, usize), ProfileError>;
}

/// Owned logical sequence parameterized by an [`AtomProfile`].
#[derive(Clone, Debug)]
pub struct AtomSequence<P: AtomProfile> {
    atoms: Vec<P::Atom>,
    marker: PhantomData<P>,
}

/// Fail-closed iterator over logical atoms in an encoded profile stream.
pub struct AtomStream<'a, P: AtomProfile> {
    remaining: &'a [u8],
    failed: bool,
    marker: PhantomData<P>,
}

impl<P: AtomProfile> PartialEq for AtomSequence<P> {
    fn eq(&self, other: &Self) -> bool {
        self.atoms == other.atoms
    }
}

impl<P: AtomProfile> Eq for AtomSequence<P> {}

impl<P: AtomProfile> Default for AtomSequence<P> {
    fn default() -> Self {
        Self {
            atoms: Vec::new(),
            marker: PhantomData,
        }
    }
}

impl<P: AtomProfile> AtomSequence<P> {
    /// Construct an empty sequence.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stable profile identity for this sequence's wire representation.
    #[inline]
    pub const fn profile() -> VariableWidthProfile {
        P::PROFILE
    }

    /// Built-in profile kind corresponding to this sequence.
    #[inline]
    pub const fn profile_kind() -> ProfileKind {
        P::KIND
    }

    /// Wire width of one atom, or `None` for variable-width profiles.
    #[inline]
    pub const fn width_bytes() -> Option<usize> {
        P::WIDTH_BYTES
    }

    /// Build a sequence from logical atoms.
    pub fn from_atoms<I>(atoms: I) -> Self
    where
        I: IntoIterator<Item = P::Atom>,
    {
        Self {
            atoms: atoms.into_iter().collect(),
            marker: PhantomData,
        }
    }

    /// Decode a complete concatenated wire image.
    pub fn from_encoded(bytes: &[u8]) -> Result<Self, ProfileError> {
        let mut atoms = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let (atom, consumed) = P::decode(&bytes[offset..])?;
            if consumed == 0 || consumed > bytes.len() - offset {
                return Err(ProfileError::InvalidLength);
            }
            atoms.push(atom);
            offset += consumed;
        }
        Ok(Self::from_atoms(atoms))
    }

    /// Iterate logical atoms directly from an immutable encoded image.
    #[inline]
    pub fn stream(bytes: &[u8]) -> AtomStream<'_, P> {
        AtomStream {
            remaining: bytes,
            failed: false,
            marker: PhantomData,
        }
    }

    /// Append one logical atom.
    #[inline]
    pub fn push(&mut self, atom: P::Atom) {
        self.atoms.push(atom);
    }

    /// Number of logical atoms.
    #[inline]
    pub fn len(&self) -> usize {
        self.atoms.len()
    }

    /// Whether the sequence contains no logical atoms.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }

    /// Iterate over logical atoms without decoding.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &P::Atom> {
        self.atoms.iter()
    }

    /// Borrow the logical atom slice for dictionary kernels.
    #[inline]
    pub fn as_atoms(&self) -> &[P::Atom] {
        &self.atoms
    }

    /// Number of bytes in the encoded sequence.
    pub fn encoded_len(&self) -> usize {
        self.atoms.iter().map(|atom| P::encode(atom).len()).sum()
    }

    /// Encode the sequence in logical order.
    pub fn to_encoded(&self) -> Vec<u8> {
        let mut encoded = Vec::new();
        for atom in &self.atoms {
            encoded.extend_from_slice(&P::encode(atom));
        }
        encoded
    }
}

impl<'a, P: AtomProfile> Iterator for AtomStream<'a, P> {
    type Item = Result<P::Atom, ProfileError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.remaining.is_empty() {
            return None;
        }
        match P::decode(self.remaining) {
            Ok((atom, consumed)) if consumed > 0 && consumed <= self.remaining.len() => {
                self.remaining = &self.remaining[consumed..];
                Some(Ok(atom))
            }
            Ok(_) => {
                self.failed = true;
                Some(Err(ProfileError::InvalidLength))
            }
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

/// Raw byte profile (`DynamicDawg` compatibility semantics).
#[derive(Clone, Copy, Debug, Default)]
pub struct Bytes;

impl AtomProfile for Bytes {
    type Atom = u8;
    const PROFILE: VariableWidthProfile = VariableWidthProfile::new("bytes", 1);
    const KIND: ProfileKind = ProfileKind::Bytes;
    const WIDTH_BYTES: Option<usize> = Some(1);

    fn encode(atom: &u8) -> Vec<u8> {
        vec![*atom]
    }

    fn decode(bytes: &[u8]) -> Result<(u8, usize), ProfileError> {
        bytes
            .first()
            .copied()
            .map(|byte| (byte, 1))
            .ok_or(ProfileError::InvalidLength)
    }
}

/// Unicode scalar profile (`DynamicDawgChar` compatibility semantics).
#[derive(Clone, Copy, Debug, Default)]
pub struct UnicodeScalar;

/// Variable-width UTF-8 scalar profile.  Each logical atom is one Unicode
/// scalar and its canonical UTF-8 codeword; continuation bytes are never
/// semantic transitions.
#[derive(Clone, Copy, Debug, Default)]
pub struct Utf8;

impl AtomProfile for Utf8 {
    type Atom = char;
    const PROFILE: VariableWidthProfile = VariableWidthProfile::new("utf8", 1);
    const KIND: ProfileKind = ProfileKind::Utf8;
    const WIDTH_BYTES: Option<usize> = None;

    fn encode(atom: &char) -> Vec<u8> {
        let mut bytes = [0u8; 4];
        atom.encode_utf8(&mut bytes).as_bytes().to_vec()
    }

    fn decode(bytes: &[u8]) -> Result<(char, usize), ProfileError> {
        let first = *bytes.first().ok_or(ProfileError::InvalidLength)?;
        let width = match first {
            0x00..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Err(ProfileError::InvalidUtf8),
        };
        let slice = bytes.get(..width).ok_or(ProfileError::InvalidLength)?;
        let text = core::str::from_utf8(slice).map_err(|_| ProfileError::InvalidUtf8)?;
        let mut chars = text.chars();
        let atom = chars.next().ok_or(ProfileError::InvalidUtf8)?;
        if chars.next().is_some() {
            return Err(ProfileError::InvalidUtf8);
        }
        Ok((atom, width))
    }
}

impl AtomProfile for UnicodeScalar {
    type Atom = char;
    const PROFILE: VariableWidthProfile = VariableWidthProfile::new("unicode-scalar", 1);
    const KIND: ProfileKind = ProfileKind::UnicodeScalar;
    const WIDTH_BYTES: Option<usize> = Some(4);

    fn encode(atom: &char) -> Vec<u8> {
        (*atom as u32).to_le_bytes().to_vec()
    }

    fn decode(bytes: &[u8]) -> Result<(char, usize), ProfileError> {
        let bytes: [u8; 4] = bytes
            .get(..4)
            .ok_or(ProfileError::InvalidLength)?
            .try_into()
            .map_err(|_| ProfileError::InvalidLength)?;
        char::from_u32(u32::from_le_bytes(bytes))
            .map(|scalar| (scalar, 4))
            .ok_or(ProfileError::InvalidScalar)
    }
}

/// Native little-endian 32-bit unsigned profile.
#[derive(Clone, Copy, Debug, Default)]
pub struct U32;

impl AtomProfile for U32 {
    type Atom = u32;
    const PROFILE: VariableWidthProfile = VariableWidthProfile::new("u32", 1);
    const KIND: ProfileKind = ProfileKind::U32;
    const WIDTH_BYTES: Option<usize> = Some(4);

    fn encode(atom: &u32) -> Vec<u8> {
        atom.to_le_bytes().to_vec()
    }

    fn decode(bytes: &[u8]) -> Result<(u32, usize), ProfileError> {
        let bytes: [u8; 4] = bytes
            .get(..4)
            .ok_or(ProfileError::InvalidLength)?
            .try_into()
            .map_err(|_| ProfileError::InvalidLength)?;
        Ok((u32::from_le_bytes(bytes), 4))
    }
}

/// Native little-endian 64-bit unsigned profile.
#[derive(Clone, Copy, Debug, Default)]
pub struct U64;

impl AtomProfile for U64 {
    type Atom = u64;
    const PROFILE: VariableWidthProfile = VariableWidthProfile::new("u64", 1);
    const KIND: ProfileKind = ProfileKind::U64;
    const WIDTH_BYTES: Option<usize> = Some(8);

    fn encode(atom: &u64) -> Vec<u8> {
        atom.to_le_bytes().to_vec()
    }

    fn decode(bytes: &[u8]) -> Result<(u64, usize), ProfileError> {
        let bytes: [u8; 8] = bytes
            .get(..8)
            .ok_or(ProfileError::InvalidLength)?
            .try_into()
            .map_err(|_| ProfileError::InvalidLength)?;
        Ok((u64::from_le_bytes(bytes), 8))
    }
}

/// Raw-bit IEEE-754 binary64 profile; the logical atom is its `u64` bit
/// pattern so ordering and equality remain total and NaN payloads are exact.
#[derive(Clone, Copy, Debug, Default)]
pub struct F64Bits;

impl AtomProfile for F64Bits {
    type Atom = u64;
    const PROFILE: VariableWidthProfile = VariableWidthProfile::new("f64-bits", 1);
    const KIND: ProfileKind = ProfileKind::F64Bits;
    const WIDTH_BYTES: Option<usize> = Some(8);

    fn encode(atom: &u64) -> Vec<u8> {
        atom.to_le_bytes().to_vec()
    }

    fn decode(bytes: &[u8]) -> Result<(u64, usize), ProfileError> {
        let bytes: [u8; 8] = bytes
            .get(..8)
            .ok_or(ProfileError::InvalidLength)?
            .try_into()
            .map_err(|_| ProfileError::InvalidLength)?;
        Ok((u64::from_le_bytes(bytes), 8))
    }
}

/// Canonical arbitrary-width ULEB128 atom profile.
///
/// The owned atom retains its canonical bytes and is therefore usable for
/// values wider than any built-in integer.  Decoding reports the first
/// complete codeword and leaves following codewords to the sequence walker.
#[derive(Clone, Copy, Debug, Default)]
pub struct Uleb128Atom;

impl AtomProfile for Uleb128Atom {
    type Atom = Uleb128;
    const PROFILE: VariableWidthProfile = ULEB128_PROFILE;
    const KIND: ProfileKind = ProfileKind::Uleb128;
    const WIDTH_BYTES: Option<usize> = None;

    fn encode(atom: &Uleb128) -> Vec<u8> {
        atom.as_bytes().to_vec()
    }

    fn decode(bytes: &[u8]) -> Result<(Uleb128, usize), ProfileError> {
        let (view, consumed) = Uleb128Ref::from_prefix(bytes).map_err(|error| {
            match error {
                crate::variable_width::Uleb128Error::Empty
                | crate::variable_width::Uleb128Error::Unterminated => {
                    ProfileError::InvalidLength
                }
                crate::variable_width::Uleb128Error::NonCanonical
                | crate::variable_width::Uleb128Error::InvalidPayload => {
                    ProfileError::InvalidEncoding
                }
            }
        })?;
        Ok((view.to_owned(), consumed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_profiles_round_trip() {
        assert_eq!(Bytes::decode(&[7, 8]).unwrap(), (7, 1));
        assert_eq!(
            UnicodeScalar::decode(&('λ' as u32).to_le_bytes()).unwrap(),
            ('λ', 4)
        );
        assert_eq!(
            U32::decode(&0xdead_beefu32.to_le_bytes()).unwrap(),
            (0xdead_beef, 4)
        );
        assert_eq!(U64::decode(&u64::MAX.to_le_bytes()).unwrap(), (u64::MAX, 8));
        let nan_bits = 0x7ff8_0000_0000_0042u64;
        assert_eq!(
            F64Bits::decode(&F64Bits::encode(&nan_bits)).unwrap().0,
            nan_bits
        );
    }

    #[test]
    fn scalar_profile_rejects_surrogates_and_short_input() {
        assert_eq!(
            UnicodeScalar::decode(&[0; 3]),
            Err(ProfileError::InvalidLength)
        );
        assert_eq!(
            UnicodeScalar::decode(&0xd800u32.to_le_bytes()),
            Err(ProfileError::InvalidScalar)
        );
    }

    #[test]
    fn generic_atom_sequence_round_trips_each_fixed_profile() {
        let bytes = AtomSequence::<Bytes>::from_atoms([1, 2, 3]);
        assert_eq!(
            AtomSequence::<Bytes>::from_encoded(&bytes.to_encoded()).unwrap(),
            bytes
        );
        let words = AtomSequence::<U32>::from_atoms([1, u32::MAX]);
        assert_eq!(
            AtomSequence::<U32>::from_encoded(&words.to_encoded()).unwrap(),
            words
        );
        let chars = AtomSequence::<UnicodeScalar>::from_atoms(['a', 'λ']);
        assert_eq!(
            AtomSequence::<UnicodeScalar>::from_encoded(&chars.to_encoded()).unwrap(),
            chars
        );
    }

    #[test]
    fn atom_stream_exposes_logical_units_not_physical_bytes() {
        let sequence = AtomSequence::<U32>::from_atoms([0x0102_0304, 7]);
        let observed: Vec<_> = AtomSequence::<U32>::stream(&sequence.to_encoded())
            .map(|atom| atom.unwrap())
            .collect();
        assert_eq!(observed, vec![0x0102_0304, 7]);
    }

    #[test]
    fn fixed_sequence_rejects_truncated_images() {
        assert!(AtomSequence::<U64>::from_encoded(&[1, 2, 3]).is_err());
        assert!(AtomSequence::<Bytes>::from_encoded(&[]).unwrap().is_empty());
    }

    #[test]
    fn sequence_exposes_profile_identity_and_width() {
        assert_eq!(AtomSequence::<U64>::profile(), U64::PROFILE);
        assert_eq!(AtomSequence::<U64>::width_bytes(), Some(8));
        assert_eq!(AtomSequence::<Bytes>::width_bytes(), Some(1));
        assert_eq!(AtomSequence::<U64>::profile_kind(), ProfileKind::U64);
    }

    #[test]
    fn sequence_borrows_logical_atoms_directly() {
        let sequence = AtomSequence::<U64>::from_atoms([11, 22]);
        assert_eq!(sequence.as_atoms(), &[11, 22]);
        assert_eq!(sequence.encoded_len(), 16);
    }

    #[test]
    fn profile_kind_identity_and_width_are_total() {
        assert_eq!(ProfileKind::Bytes.width_bytes(), Some(1));
        assert_eq!(ProfileKind::UnicodeScalar.width_bytes(), Some(4));
        assert_eq!(ProfileKind::Uleb128.width_bytes(), None);
        assert_eq!(ProfileKind::Uleb128.identity(), ULEB128_PROFILE);
        for kind in [
            ProfileKind::Bytes,
            ProfileKind::UnicodeScalar,
            ProfileKind::Utf8,
            ProfileKind::U32,
            ProfileKind::U64,
            ProfileKind::F64Bits,
            ProfileKind::Uleb128,
        ] {
            assert_eq!(ProfileKind::from_name(kind.as_str()), Some(kind));
        }
        assert_eq!(ProfileKind::from_name("DynamicDawgChar"), None);
        assert_eq!(
            ProfileKind::UnicodeScalar.legacy_name(),
            Some("DynamicDawgChar")
        );
        assert_eq!(ProfileKind::Uleb128.legacy_name(), None);
        assert_eq!(ProfileKind::Utf8.width_bytes(), None);
        assert_eq!(
            ProfileKind::from_identity(Utf8::PROFILE),
            Some(ProfileKind::Utf8)
        );
        assert_eq!(
            ProfileKind::from_identity(VariableWidthProfile::new("u64", 1)),
            Some(ProfileKind::U64)
        );
        assert_eq!(
            ProfileKind::from_identity(VariableWidthProfile::new("u64", 2)),
            None
        );
    }

    #[test]
    fn utf8_profile_preserves_scalar_boundaries_and_rejects_malformed_input() {
        let sequence = AtomSequence::<Utf8>::from_atoms(['a', 'λ', '🎉']);
        let encoded = sequence.to_encoded();
        assert_eq!(
            AtomSequence::<Utf8>::from_encoded(&encoded).unwrap(),
            sequence
        );
        let observed: Vec<_> = AtomSequence::<Utf8>::stream(&encoded)
            .map(|atom| atom.unwrap())
            .collect();
        assert_eq!(observed, vec!['a', 'λ', '🎉']);
        assert!(AtomSequence::<Utf8>::from_encoded(&[0x80]).is_err());
    }

    #[test]
    fn uleb_profile_preserves_arbitrary_width_atoms_and_boundaries() {
        let wide = Uleb128::from_payload_digits(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 2]).unwrap();
        let sequence = AtomSequence::<Uleb128Atom>::from_atoms([
            Uleb128::from_u64(127),
            wide.clone(),
            Uleb128::from_u64(0),
        ]);
        let encoded = sequence.to_encoded();
        assert_eq!(AtomSequence::<Uleb128Atom>::from_encoded(&encoded).unwrap(), sequence);
        assert_eq!(
            AtomSequence::<Uleb128Atom>::stream(&encoded)
                .map(|atom| atom.unwrap())
                .collect::<Vec<_>>(),
            sequence.as_atoms()
        );
        assert!(wide.to_u64().is_none());
    }
}
