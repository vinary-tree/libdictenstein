//! Shared logical-atom profiles for generic dictionary families.

use crate::variable_width::VariableWidthProfile;

/// Errors returned by fixed-width profile decoders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileError {
    /// The input does not contain one complete atom.
    InvalidLength,
    /// A scalar profile received a value outside the Unicode scalar range.
    InvalidScalar,
}

/// Codec contract for one logical dictionary edge.
pub trait AtomProfile {
    /// Logical value represented by one edge.
    type Atom: Copy + Eq + Ord;

    /// Stable persisted profile identity.
    const PROFILE: VariableWidthProfile;
    /// Fixed wire width in bytes, or `None` for variable-width profiles.
    const WIDTH_BYTES: Option<usize>;

    /// Encode one logical atom.
    fn encode(atom: Self::Atom) -> Vec<u8>;
    /// Decode one atom from the beginning of `bytes`, returning the atom and
    /// the number of bytes consumed.
    fn decode(bytes: &[u8]) -> Result<(Self::Atom, usize), ProfileError>;
}

/// Raw byte profile (`DynamicDawg` compatibility semantics).
#[derive(Clone, Copy, Debug, Default)]
pub struct Bytes;

impl AtomProfile for Bytes {
    type Atom = u8;
    const PROFILE: VariableWidthProfile = VariableWidthProfile::new("bytes", 1);
    const WIDTH_BYTES: Option<usize> = Some(1);

    fn encode(atom: u8) -> Vec<u8> {
        vec![atom]
    }

    fn decode(bytes: &[u8]) -> Result<(u8, usize), ProfileError> {
        bytes.first().copied().map(|byte| (byte, 1)).ok_or(ProfileError::InvalidLength)
    }
}

/// Unicode scalar profile (`DynamicDawgChar` compatibility semantics).
#[derive(Clone, Copy, Debug, Default)]
pub struct UnicodeScalar;

impl AtomProfile for UnicodeScalar {
    type Atom = char;
    const PROFILE: VariableWidthProfile = VariableWidthProfile::new("unicode-scalar", 1);
    const WIDTH_BYTES: Option<usize> = Some(4);

    fn encode(atom: char) -> Vec<u8> {
        (atom as u32).to_le_bytes().to_vec()
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
    const WIDTH_BYTES: Option<usize> = Some(4);

    fn encode(atom: u32) -> Vec<u8> {
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
    const WIDTH_BYTES: Option<usize> = Some(8);

    fn encode(atom: u64) -> Vec<u8> {
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
    const WIDTH_BYTES: Option<usize> = Some(8);

    fn encode(atom: u64) -> Vec<u8> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_profiles_round_trip() {
        assert_eq!(Bytes::decode(&[7, 8]).unwrap(), (7, 1));
        assert_eq!(UnicodeScalar::decode(&('λ' as u32).to_le_bytes()).unwrap(), ('λ', 4));
        assert_eq!(U32::decode(&0xdead_beefu32.to_le_bytes()).unwrap(), (0xdead_beef, 4));
        assert_eq!(U64::decode(&u64::MAX.to_le_bytes()).unwrap(), (u64::MAX, 8));
        let nan_bits = 0x7ff8_0000_0000_0042u64;
        assert_eq!(F64Bits::decode(&F64Bits::encode(nan_bits)).unwrap().0, nan_bits);
    }

    #[test]
    fn scalar_profile_rejects_surrogates_and_short_input() {
        assert_eq!(UnicodeScalar::decode(&[0; 3]), Err(ProfileError::InvalidLength));
        assert_eq!(
            UnicodeScalar::decode(&0xd800u32.to_le_bytes()),
            Err(ProfileError::InvalidScalar)
        );
    }
}
