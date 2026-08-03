//! Correspondence tests for the public serialization proof model.
//!
//! Rocq model:
//! `formal-verification/rocq/Spec/SerializationRoundtripSpec.v`
//!
//! Rust obligations checked here:
//! - term-only serializers preserve dictionary membership;
//! - value-aware serializers preserve mapped lookup values;
//! - legacy term-only serialization drops values but preserves domains;
//! - malformed payloads fail closed instead of producing a dictionary.

#![cfg(feature = "serialization")]

mod common;

use common::strategies::{ascii_term, unicode_term};
use libdictenstein::double_array_trie::char::DoubleArrayTrieChar;
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::dynamic_dawg::char::DynamicDawgChar;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::scdawg::char::ScdawgChar;
use libdictenstein::scdawg::Scdawg;
use libdictenstein::serialization::{
    BincodeSerializer, DictionaryFromTerms, DictionaryFromTermsWithValues, DictionarySerializer,
};
use libdictenstein::{Dictionary, DictionaryNode, MappedDictionary};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

fn byte_terms() -> Vec<String> {
    ["alpha", "alpine", "beta", "betamax", "gamma"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn byte_entries() -> Vec<(String, i32)> {
    vec![
        ("alpha".to_string(), 11),
        ("alpine".to_string(), 12),
        ("beta".to_string(), -7),
        ("betamax".to_string(), 42),
        ("gamma".to_string(), 0),
    ]
}

fn unicode_entries() -> Vec<(String, i32)> {
    vec![
        ("café".to_string(), 1),
        ("naïve".to_string(), 2),
        ("日本語".to_string(), 3),
        ("alphaβ".to_string(), -4),
    ]
}

fn expected_set(terms: &[String]) -> BTreeSet<String> {
    terms.iter().cloned().collect()
}

fn expected_map(entries: &[(String, i32)]) -> BTreeMap<String, i32> {
    entries.iter().cloned().collect()
}

fn assert_set_matches<D>(dict: &D, expected: &BTreeSet<String>)
where
    D: Dictionary,
{
    for term in expected {
        assert!(dict.contains(term), "round-trip lost term {term:?}");
    }
    assert!(
        !dict.contains("__definitely_absent__"),
        "round-trip created an unrelated sentinel term"
    );
    if let Some(len) = dict.len() {
        assert_eq!(len, expected.len(), "round-trip changed dictionary size");
    }
}

fn assert_map_matches<D>(dict: &D, expected: &BTreeMap<String, i32>)
where
    D: MappedDictionary<Value = i32>,
{
    for (term, value) in expected {
        assert_eq!(
            dict.get_value(term),
            Some(*value),
            "round-trip changed value for term {term:?}"
        );
    }
    assert_eq!(
        dict.get_value("__definitely_absent__"),
        None,
        "round-trip created an unrelated sentinel value"
    );
}

fn roundtrip_bincode_set<D>(dict: &D) -> D
where
    D: Dictionary + DictionaryFromTerms,
    D::Node: DictionaryNode<Unit = u8>,
{
    let mut buf = Vec::new();
    BincodeSerializer::serialize(dict, &mut buf).expect("bincode set serialize");
    BincodeSerializer::deserialize(&buf[..]).expect("bincode set deserialize")
}

fn roundtrip_bincode_map_byte<D>(dict: &D) -> D
where
    D: MappedDictionary<Value = i32> + DictionaryFromTermsWithValues<Value = i32>,
    D::Node: DictionaryNode<Unit = u8>,
{
    let mut buf = Vec::new();
    BincodeSerializer::serialize_with_values(dict, &mut buf).expect("bincode byte map serialize");
    BincodeSerializer::deserialize_with_values(&buf[..]).expect("bincode byte map deserialize")
}

fn roundtrip_bincode_map_char<D>(dict: &D) -> D
where
    D: MappedDictionary<Value = i32> + DictionaryFromTermsWithValues<Value = i32>,
    D::Node: DictionaryNode<Unit = char>,
{
    let mut buf = Vec::new();
    BincodeSerializer::serialize_with_values_char(dict, &mut buf)
        .expect("bincode char map serialize");
    BincodeSerializer::deserialize_with_values(&buf[..]).expect("bincode char map deserialize")
}

#[test]
fn byte_term_serializers_roundtrip_membership() {
    let terms = byte_terms();
    let expected = expected_set(&terms);

    let dat = DoubleArrayTrie::from_terms(terms.clone());
    assert_set_matches(&roundtrip_bincode_set::<DoubleArrayTrie>(&dat), &expected);

    let dawg: DynamicDawg<()> = DynamicDawg::from_terms(terms);
    assert_set_matches(&roundtrip_bincode_set::<DynamicDawg<()>>(&dawg), &expected);
}

#[test]
fn byte_value_serializers_roundtrip_lookup_values() {
    let entries = byte_entries();
    let expected = expected_map(&entries);

    let dat: DoubleArrayTrie<i32> = DoubleArrayTrie::from_terms_with_values(entries.clone());
    assert_map_matches(&roundtrip_bincode_map_byte(&dat), &expected);

    let dawg: DynamicDawg<i32> = DynamicDawg::from_terms_with_values(entries.clone());
    assert_map_matches(&roundtrip_bincode_map_byte(&dawg), &expected);

    let scdawg: Scdawg<i32> = Scdawg::from_terms_with_values(entries);
    assert_map_matches(&roundtrip_bincode_map_byte(&scdawg), &expected);
}

#[test]
fn char_value_serializers_roundtrip_unicode_lookup_values() {
    let entries = unicode_entries();
    let expected = expected_map(&entries);

    let dat: DoubleArrayTrieChar<i32> =
        DoubleArrayTrieChar::from_terms_with_values(entries.clone());
    assert_map_matches(&roundtrip_bincode_map_char(&dat), &expected);

    let dawg: DynamicDawgChar<i32> = DynamicDawgChar::from_terms_with_values(entries.clone());
    assert_map_matches(&roundtrip_bincode_map_char(&dawg), &expected);

    let scdawg: ScdawgChar<i32> = ScdawgChar::from_terms_with_values(entries);
    assert_map_matches(&roundtrip_bincode_map_char(&scdawg), &expected);
}

#[test]
fn legacy_term_serializers_drop_values_but_preserve_domain() {
    let entries = byte_entries();
    let expected = expected_map(&entries);
    let dict: DynamicDawg<i32> = DynamicDawg::from_terms_with_values(entries);

    let mut bincode = Vec::new();
    BincodeSerializer::serialize(&dict, &mut bincode).expect("legacy bincode serialize");
    let restored: DynamicDawg<i32> =
        BincodeSerializer::deserialize(&bincode[..]).expect("legacy bincode deserialize");
    assert_legacy_domain_without_values(&restored, &expected);
}

fn assert_legacy_domain_without_values<D>(dict: &D, expected: &BTreeMap<String, i32>)
where
    D: MappedDictionary<Value = i32>,
{
    for term in expected.keys() {
        assert!(dict.contains(term), "legacy serializer lost term {term:?}");
        assert_eq!(
            dict.get_value(term),
            None,
            "legacy serializer unexpectedly preserved value for term {term:?}"
        );
    }
}

#[test]
fn malformed_payloads_fail_closed() {
    let truncated_bincode = [0xff];
    assert!(BincodeSerializer::deserialize::<DoubleArrayTrie, _>(&truncated_bincode[..]).is_err());
    assert!(
        BincodeSerializer::deserialize_with_values::<DoubleArrayTrie<i32>, _>(
            &truncated_bincode[..]
        )
        .is_err()
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn bincode_term_roundtrip_preserves_generated_sets(
        terms in prop::collection::btree_set(ascii_term(1, 12), 1..=40)
    ) {
        let expected = terms.clone();
        let dict: DynamicDawg<()> = DynamicDawg::from_terms(terms);
        let restored: DynamicDawg<()> = roundtrip_bincode_set(&dict);

        for term in &expected {
            prop_assert!(restored.contains(term), "missing term {:?}", term);
        }
        prop_assert_eq!(restored.len(), Some(expected.len()));
    }

    #[test]
    fn char_bincode_value_roundtrip_preserves_generated_unicode_maps(
        entries in prop::collection::btree_map(unicode_term(1, 8), any::<i16>(), 1..=24)
    ) {
        let expected: BTreeMap<String, i32> = entries
            .iter()
            .map(|(term, value)| (term.clone(), i32::from(*value)))
            .collect();
        let dict: DoubleArrayTrieChar<i32> =
            DoubleArrayTrieChar::from_terms_with_values(expected.iter().map(|(k, v)| (k.clone(), *v)));
        let restored: DoubleArrayTrieChar<i32> = roundtrip_bincode_map_char(&dict);

        for (term, value) in &expected {
            prop_assert_eq!(restored.get_value(term), Some(*value), "term {:?}", term);
        }
        prop_assert_eq!(restored.len(), Some(expected.len()));
    }
}
