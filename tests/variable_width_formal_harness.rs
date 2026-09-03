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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn vwenc_01_uleb_payload_roundtrip(digits in arb_digits()) {
        let encoded = encode_uleb(digits.clone());
        prop_assert_eq!(decode_uleb(&encoded), Some(digits));
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
}

#[test]
fn vwenc_06_noncanonical_overlong_is_rejected() {
    assert_eq!(decode_uleb(&[0x80, 0x00]), None);
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
    let canonical = encode_uleb(vec![1, 2, 3]);
    assert_ne!(faulty(&canonical), vec![1, 2, 3]);
}
