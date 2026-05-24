//! Correspondence tests for feature-gated protobuf and compression serializers.
//!
//! Rocq model:
//! `formal-verification/rocq/Spec/SerializationRoundtripSpec.v`
//!
//! Rust obligations checked here:
//! - gzip-wrapped serializers preserve dictionary membership;
//! - protobuf V1, V2, DAT, and suffix-automaton formats preserve semantics;
//! - malformed protobuf and gzip payloads fail closed.

#![cfg(all(
    feature = "serialization",
    feature = "protobuf",
    feature = "compression"
))]

mod common;

use common::strategies::ascii_term;
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::serialization::{
    BincodeSerializer, DatProtobufSerializer, DictionaryFromTerms, DictionarySerializer,
    GzipSerializer, JsonSerializer, OptimizedProtobufSerializer, ProtobufSerializer,
    SuffixAutomatonProtobufSerializer,
};
use libdictenstein::suffix_automaton::SuffixAutomaton;
use libdictenstein::{Dictionary, DictionaryNode};
use proptest::prelude::*;

fn base_terms() -> Vec<String> {
    vec![
        "alpha".to_string(),
        "alpine".to_string(),
        "beta".to_string(),
        "betamax".to_string(),
        "caf\u{e9}".to_string(),
        "\u{65e5}本".to_string(),
    ]
}

fn assert_membership<D>(dict: &D, expected: &[String])
where
    D: Dictionary,
{
    for term in expected {
        assert!(dict.contains(term), "roundtrip lost term {term:?}");
    }
    assert!(
        !dict.contains("__definitely_absent__"),
        "roundtrip created an unrelated sentinel term"
    );
    if let Some(len) = dict.len() {
        assert_eq!(len, expected.len(), "roundtrip changed dictionary size");
    }
}

fn serializer_roundtrip<S, D>(dict: &D) -> D
where
    S: DictionarySerializer,
    D: Dictionary + DictionaryFromTerms,
    D::Node: DictionaryNode<Unit = u8>,
{
    let mut buf = Vec::new();
    S::serialize(dict, &mut buf).expect("serialize");
    S::deserialize(&buf[..]).expect("deserialize")
}

fn gzip_roundtrip<S, D>(dict: &D) -> D
where
    S: DictionarySerializer,
    D: Dictionary + DictionaryFromTerms,
    D::Node: DictionaryNode<Unit = u8>,
{
    let mut buf = Vec::new();
    GzipSerializer::<S>::serialize(dict, &mut buf).expect("gzip serialize");
    GzipSerializer::<S>::deserialize(&buf[..]).expect("gzip deserialize")
}

fn varint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            return bytes;
        }
    }
}

fn field_varint(field: u64, value: u64) -> Vec<u8> {
    let mut bytes = varint(field << 3);
    bytes.extend(varint(value));
    bytes
}

fn field_len(field: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = varint((field << 3) | 2);
    bytes.extend(varint(payload.len() as u64));
    bytes.extend(payload);
    bytes
}

fn packed_varints(field: u64, values: &[u64]) -> Vec<u8> {
    let mut payload = Vec::new();
    for value in values {
        payload.extend(varint(*value));
    }
    field_len(field, &payload)
}

fn edge_message(source: u64, label: u64, target: u64) -> Vec<u8> {
    let mut edge = Vec::new();
    edge.extend(field_varint(1, source));
    edge.extend(field_varint(2, label));
    edge.extend(field_varint(3, target));
    edge
}

fn protobuf_v1_payload(nodes: &[u64], finals: &[u64], edges: &[Vec<u8>], size: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(packed_varints(1, nodes));
    bytes.extend(packed_varints(2, finals));
    for edge in edges {
        bytes.extend(field_len(3, edge));
    }
    bytes.extend(field_varint(4, 0));
    bytes.extend(field_varint(5, size));
    bytes
}

fn protobuf_v2_payload(
    final_deltas: &[u64],
    edge_data: &[u64],
    size: u64,
    edge_count: u64,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(packed_varints(1, final_deltas));
    bytes.extend(packed_varints(2, edge_data));
    bytes.extend(field_varint(3, 0));
    bytes.extend(field_varint(4, size));
    bytes.extend(field_varint(5, edge_count));
    bytes
}

fn dat_payload(edge_data: &[u8], term_count: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(field_len(4, edge_data));
    bytes.extend(field_varint(6, term_count));
    bytes
}

fn suffix_payload(source: &str, declared_count: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(field_len(1, source.as_bytes()));
    bytes.extend(field_varint(2, declared_count));
    bytes
}

#[test]
fn gzip_wrapped_serializers_roundtrip_membership() {
    let terms = base_terms();

    let dawg: DynamicDawg<()> = DynamicDawg::from_terms(terms.clone());
    let restored: DynamicDawg<()> = gzip_roundtrip::<BincodeSerializer, _>(&dawg);
    assert_membership(&restored, &terms);

    let dat = DoubleArrayTrie::from_terms(terms.clone());
    let restored: DoubleArrayTrie = gzip_roundtrip::<JsonSerializer, _>(&dat);
    assert_membership(&restored, &terms);
}

#[test]
fn protobuf_graph_formats_roundtrip_utf8_membership() {
    let terms = base_terms();
    let dat = DoubleArrayTrie::from_terms(terms.clone());

    let restored_v1: DoubleArrayTrie = serializer_roundtrip::<ProtobufSerializer, _>(&dat);
    assert_membership(&restored_v1, &terms);

    let restored_v2: DoubleArrayTrie = serializer_roundtrip::<OptimizedProtobufSerializer, _>(&dat);
    assert_membership(&restored_v2, &terms);
}

#[test]
fn dat_protobuf_roundtrip_preserves_delimiter_terms() {
    let terms = vec![
        "alpha".to_string(),
        "line\nbreak".to_string(),
        "omega".to_string(),
    ];
    let dat = DoubleArrayTrie::from_terms(terms.clone());

    let mut buf = Vec::new();
    DatProtobufSerializer::serialize_dat(&dat, &mut buf).expect("DAT protobuf serialize");
    let restored = DatProtobufSerializer::deserialize_dat(&buf[..]).expect("DAT protobuf decode");

    assert_membership(&restored, &terms);
}

#[test]
fn suffix_automaton_protobuf_preserves_source_language() {
    let sources = vec![
        "quick brown fox".to_string(),
        "suffix automata index substrings".to_string(),
    ];
    let dict = SuffixAutomaton::from_texts(sources.clone());

    let mut buf = Vec::new();
    SuffixAutomatonProtobufSerializer::serialize_suffix_automaton(&dict, &mut buf)
        .expect("suffix protobuf serialize");
    let restored = SuffixAutomatonProtobufSerializer::deserialize_suffix_automaton(&buf[..])
        .expect("suffix protobuf decode");

    for source in &sources {
        assert!(restored.source_texts().contains(source));
    }
    for substring in ["quick", "brown fox", "automata", "index substrings"] {
        assert!(
            restored.contains(substring),
            "missing substring {substring:?}"
        );
    }
    assert!(!restored.contains("not present"));
}

#[test]
fn malformed_feature_payloads_fail_closed() {
    assert!(
        GzipSerializer::<BincodeSerializer>::deserialize::<DoubleArrayTrie, _>(
            b"not a gzip stream".as_slice()
        )
        .is_err()
    );

    let invalid_v1_label =
        protobuf_v1_payload(&[0, 1], &[1], &[edge_message(0, u8::MAX as u64 + 1, 1)], 1);
    assert!(ProtobufSerializer::deserialize::<DoubleArrayTrie, _>(&invalid_v1_label[..]).is_err());

    let cyclic_v1 = protobuf_v1_payload(&[0], &[0], &[edge_message(0, b'a' as u64, 0)], 1);
    assert!(ProtobufSerializer::deserialize::<DoubleArrayTrie, _>(&cyclic_v1[..]).is_err());

    let invalid_v2_triplets = protobuf_v2_payload(&[], &[0], 0, 1);
    assert!(
        OptimizedProtobufSerializer::deserialize::<DoubleArrayTrie, _>(&invalid_v2_triplets[..])
            .is_err()
    );

    let invalid_v2_label = protobuf_v2_payload(&[1], &[0, u8::MAX as u64 + 1, 1], 1, 1);
    assert!(
        OptimizedProtobufSerializer::deserialize::<DoubleArrayTrie, _>(&invalid_v2_label[..])
            .is_err()
    );

    let cyclic_v2 = protobuf_v2_payload(&[0], &[0, b'a' as u64, 0], 1, 1);
    assert!(
        OptimizedProtobufSerializer::deserialize::<DoubleArrayTrie, _>(&cyclic_v2[..]).is_err()
    );

    let mut truncated_dat_terms = b"LDT1".to_vec();
    truncated_dat_terms.extend_from_slice(&8u32.to_le_bytes());
    truncated_dat_terms.extend_from_slice(b"abc");
    assert!(
        DatProtobufSerializer::deserialize_dat(&dat_payload(&truncated_dat_terms, 1)[..]).is_err()
    );

    assert!(
        SuffixAutomatonProtobufSerializer::deserialize_suffix_automaton(
            &suffix_payload("hello", 2)[..]
        )
        .is_err()
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn generated_sets_roundtrip_through_gzip_and_protobuf(
        terms in prop::collection::btree_set(ascii_term(1, 12), 1..=32)
    ) {
        let expected: Vec<String> = terms.iter().cloned().collect();
        let dat = DoubleArrayTrie::from_terms(expected.clone());

        let gzip_restored: DoubleArrayTrie = gzip_roundtrip::<BincodeSerializer, _>(&dat);
        for term in &expected {
            prop_assert!(gzip_restored.contains(term), "gzip lost term {:?}", term);
        }
        prop_assert_eq!(gzip_restored.len(), Some(expected.len()));

        let protobuf_restored: DoubleArrayTrie =
            serializer_roundtrip::<OptimizedProtobufSerializer, _>(&dat);
        for term in &expected {
            prop_assert!(protobuf_restored.contains(term), "protobuf lost term {:?}", term);
        }
        prop_assert_eq!(protobuf_restored.len(), Some(expected.len()));
    }
}
