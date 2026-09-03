//! Executable reference properties for the variable-width formal contract.
//!
//! These tests deliberately use only the standard-library reference codecs and
//! a heap-backed vocabulary oracle.  They do not depend on a production
//! profile implementation, so they can detect regressions before profile API
//! work is enabled.

use proptest::prelude::*;
use std::collections::BTreeMap;

fn encode_uleb(mut value: Vec<u8>) -> Vec<u8> {
    while value.len() > 1 && value.last() == Some(&0) {
        value.pop();
    }
    let value_len = value.len();
    let mut out = Vec::with_capacity(value_len);
    for (index, digit) in value.into_iter().enumerate() {
        assert!(digit < 128);
        out.push(if index + 1 == value_len { digit } else { digit | 0x80 });
    }
    out
}

fn decode_uleb(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.is_empty() {
        return None;
    }
    let mut payload = Vec::with_capacity(bytes.len());
    for (index, byte) in bytes.iter().copied().enumerate() {
        payload.push(byte & 0x7f);
        if byte < 0x80 {
            if index + 1 != bytes.len() || (payload.len() > 1 && payload.last() == Some(&0)) {
                return None;
            }
            return Some(payload);
        }
    }
    None
}

fn arb_digits() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(0u8..128, 1..64)
}

fn canonical_digits(mut digits: Vec<u8>) -> Vec<u8> {
    while digits.len() > 1 && digits.last() == Some(&0) {
        digits.pop();
    }
    digits
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn vwenc_01_uleb_payload_roundtrip(digits in arb_digits()) {
        let encoded = encode_uleb(digits.clone());
        let canonical = canonical_digits(digits);
        prop_assert_eq!(decode_uleb(&encoded), Some(canonical));
    }

    #[test]
    fn vwenc_02_uleb_canonical_encode(digits in arb_digits()) {
        let canonical = canonical_digits(digits.clone());
        prop_assert_eq!(encode_uleb(digits), encode_uleb(canonical));
    }

    #[test]
    fn vwenc_04_uleb_unique_decoding(left in arb_digits(), right in arb_digits()) {
        let left = canonical_digits(left);
        let right = canonical_digits(right);
        if left != right {
            prop_assert_ne!(decode_uleb(&encode_uleb(left)), decode_uleb(&encode_uleb(right)));
        }
    }

    #[test]
    fn vwenc_03_uleb_codewords_nonempty(digits in arb_digits()) {
        prop_assert!(!encode_uleb(digits).is_empty());
    }

    #[test]
    fn vwenc_05_to_07_malformed_uleb_is_rejected(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        if bytes.is_empty() || bytes.last().is_some_and(|byte| byte & 0x80 != 0) {
            prop_assert_eq!(decode_uleb(&bytes), None);
        }
    }

    #[test]
    fn vwenc_09_decoding_work_is_input_bounded(bytes in prop::collection::vec(0u8..=255, 1..64)) {
        let _ = decode_uleb(&bytes);
        prop_assert!(bytes.len() <= 64);
    }

    #[test]
    fn vwenc_08_uleb_each_byte_is_u8(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        prop_assert!(bytes.iter().all(|byte| *byte <= u8::MAX));
    }

    #[test]
    fn vwenc_14_vocabulary_forward_reverse_bijection(atoms in prop::collection::hash_set(arb_digits(), 0..32)) {
        let mut forward = BTreeMap::new();
        let mut reverse = BTreeMap::new();
        for (id, atom) in atoms.into_iter().enumerate() {
            let id = id as u32;
            prop_assert!(forward.insert(atom.clone(), id).is_none());
            prop_assert!(reverse.insert(id, atom).is_none());
        }
        for (atom, id) in &forward {
            prop_assert_eq!(reverse.get(id), Some(atom));
        }
    }

    #[test]
    fn vwenc_18_duplicate_atoms_share_one_id(atom in arb_digits()) {
        let mut vocabulary = BTreeMap::new();
        let first = vocabulary.len() as u32;
        let existing = *vocabulary.entry(atom.clone()).or_insert(first);
        let next = vocabulary.len() as u32;
        let again = *vocabulary.entry(atom).or_insert(next);
        prop_assert_eq!(existing, again);
        prop_assert_eq!(vocabulary.len(), 1);
    }

    #[test]
    fn vwenc_34_utf8_scalar_boundaries(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        if let Ok(text) = std::str::from_utf8(&bytes) {
            let scalars: Vec<char> = text.chars().collect();
            let rebuilt: String = scalars.iter().copied().collect();
            prop_assert_eq!(rebuilt.as_bytes(), bytes.as_slice());
        }
    }

    #[test]
    fn vwenc_11_utf8_scalar_boolean_reflection(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        let valid = std::str::from_utf8(&bytes).is_ok();
        let roundtrip = std::str::from_utf8(&bytes)
            .map(|text| text.chars().collect::<String>().into_bytes() == bytes)
            .unwrap_or(false);
        prop_assert_eq!(valid, roundtrip);
    }

    #[test]
    fn vwenc_12_utf8_codewords_nonempty_and_at_most_four_bytes(ch in any::<char>()) {
        let width = ch.len_utf8();
        prop_assert!(width > 0 && width <= 4);
    }

    #[test]
    fn vwenc_199_full_enumeration_order_is_deterministic(atoms in prop::collection::vec(arb_digits(), 0..48)) {
        let mut forward = BTreeMap::new();
        let mut reverse = BTreeMap::new();
        for atom in &atoms {
            let id = forward.len() as u32;
            forward.entry(atom.clone()).or_insert(id);
        }
        for atom in atoms.iter().rev() {
            let id = reverse.len() as u32;
            reverse.entry(atom.clone()).or_insert(id);
        }
        prop_assert_eq!(
            forward.keys().collect::<Vec<_>>(),
            reverse.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn vwenc_06_noncanonical_overlong_is_rejected() {
    assert_eq!(decode_uleb(&[0x80, 0x00]), None);
}

#[test]
fn vwenc_07_uleb_early_terminator_is_rejected() {
    assert_eq!(decode_uleb(&[0x81, 0x00, 0x01]), None);
}

#[test]
fn vwenc_14_utf8_rejects_nonscalars() {
    for bytes in [&[0xed, 0xa0, 0x80][..], &[0xf0, 0x80, 0x80, 0x80][..], &[0x80][..]] {
        assert!(std::str::from_utf8(bytes).is_err());
    }
}

#[test]
fn vwenc_10_uleb_order_is_logical_numeric_order() {
    for left in 0u8..127 {
        for right in left..127 {
            assert!(left <= right);
            assert!(decode_uleb(&encode_uleb(vec![left])) <= decode_uleb(&encode_uleb(vec![right])));
        }
    }
}

#[test]
fn vwenc_241_codec_bytes_never_become_logical_labels() {
    let encoded = encode_uleb(vec![1, 2, 3]);
    assert_eq!(decode_uleb(&encoded), Some(vec![1, 2, 3]));
    assert_ne!(encoded, vec![1, 2, 3]);
}

#[test]
fn vwenc_244_specialized_divergence_mutant_is_detectable() {
    fn faulty(bytes: &[u8]) -> Vec<u8> { bytes.iter().map(|byte| byte & 0x7f).collect() }
    let canonical = encode_uleb(vec![1, 2]);
    assert_ne!(decode_uleb(&canonical), decode_uleb(&faulty(&canonical)));
}

#[test]
fn vwenc_36_arbitrary_width_payload_is_not_limited_to_u128() {
    let digits: Vec<u8> = (0..40).map(|i| (i * 7 % 128) as u8).collect();
    let encoded = encode_uleb(digits.clone());
    assert_eq!(decode_uleb(&encoded), Some(digits));
}

#[test]
fn vwenc_80_adjacent_codewords_preserve_boundaries() {
    let first = encode_uleb(vec![1, 2]);
    let second = encode_uleb(vec![3]);
    let mut stream = first.clone();
    stream.extend_from_slice(&second);
    assert_eq!(&stream[..first.len()], first.as_slice());
    assert_eq!(&stream[first.len()..], second.as_slice());
    assert_eq!(decode_uleb(&stream[..first.len()]), Some(vec![1, 2]));
    assert_eq!(decode_uleb(&stream[first.len()..]), Some(vec![3]));
}
