//! Correspondence checks for the optional PathMap backends and factory dispatch.
//!
//! These tests connect `PathMapFactorySpec.v` to the Rust implementation:
//! PathMap dictionaries must refine a reference `BTreeMap`, PathMapChar node
//! traversal must enumerate Unicode character edges rather than leading UTF-8
//! bytes, and `DictionaryFactory` must preserve the requested backend tag.

#![cfg(feature = "pathmap-backend")]

use libdictenstein::factory::{DictionaryBackend, DictionaryFactory};
use libdictenstein::pathmap::char::PathMapDictionaryChar;
use libdictenstein::pathmap::zipper::PathMapZipper;
use libdictenstein::pathmap::PathMapDictionary;
use libdictenstein::{
    DictZipper, Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode,
    MutableDictionary, MutableMappedDictionary, ValuedDictZipper,
};
use std::collections::{BTreeMap, BTreeSet};

fn reference_entries() -> BTreeMap<String, i32> {
    let mut reference = BTreeMap::new();
    for (term, value) in [
        ("", 0),
        ("alpha", 1),
        ("alphabet", 2),
        ("cafe", 3),
        ("café", 4),
        ("cafê", 5),
        ("cafë", 6),
        ("é", 7),
        ("ê", 8),
        ("ë", 9),
        ("中", 10),
        ("丮", 11),
        ("日本語", 12),
        ("😀", 13),
        ("😁", 14),
    ] {
        reference.insert(term.to_string(), value);
    }
    reference.insert("café".to_string(), 40);
    reference.insert("日本語".to_string(), 120);
    reference
}

fn descend_bytes<N>(mut node: N, bytes: &[u8]) -> Option<N>
where
    N: DictionaryNode<Unit = u8>,
{
    for byte in bytes {
        node = node.transition(*byte)?;
    }
    Some(node)
}

fn descend_chars<N>(mut node: N, term: &str) -> Option<N>
where
    N: DictionaryNode<Unit = char>,
{
    for ch in term.chars() {
        node = node.transition(ch)?;
    }
    Some(node)
}

fn byte_entries(dict: &PathMapDictionary<i32>) -> BTreeMap<String, i32> {
    dict.iter()
        .map(|(term, value)| (term, value))
        .collect::<BTreeMap<_, _>>()
}

fn collect_char_entries<N>(root: N) -> BTreeMap<String, i32>
where
    N: MappedDictionaryNode<Unit = char, Value = i32>,
{
    let mut entries = BTreeMap::new();
    let mut stack = vec![(String::new(), root)];

    while let Some((prefix, node)) = stack.pop() {
        if node.is_final() {
            entries.insert(
                prefix.clone(),
                node.value()
                    .expect("final PathMapChar node must carry value"),
            );
        }

        let children: Vec<_> = node.edges().collect();
        for (ch, child) in children {
            let mut child_prefix = prefix.clone();
            child_prefix.push(ch);
            stack.push((child_prefix, child));
        }
    }

    entries
}

fn assert_mapped_dictionary_matches<D>(dict: &D, expected: &BTreeMap<String, i32>)
where
    D: Dictionary + MappedDictionary<Value = i32>,
{
    assert_eq!(dict.len(), Some(expected.len()));
    assert_eq!(dict.is_empty(), expected.is_empty());

    for (term, value) in expected {
        assert!(dict.contains(term), "missing term {term:?}");
        assert_eq!(dict.get_value(term), Some(*value), "value for {term:?}");
        assert!(
            dict.contains_with_value(term, |actual| *actual == *value),
            "stored value predicate should match for {term:?}"
        );
        assert!(
            !dict.contains_with_value(term, |actual| *actual != *value),
            "wrong value predicate should not match for {term:?}"
        );
    }

    for absent in ["missing", "caf", "日本", "😃", "zz_absent_zz"] {
        if !expected.contains_key(absent) {
            assert!(!dict.contains(absent), "unexpected term {absent:?}");
            assert_eq!(dict.get_value(absent), None, "absent value {absent:?}");
        }
    }
}

fn exercise_mutable_trait<D>(dict: &D)
where
    D: Dictionary
        + MappedDictionary<Value = i32>
        + MutableDictionary
        + MutableMappedDictionary<Value = i32>,
{
    let mut expected = BTreeMap::new();

    assert!(MutableMappedDictionary::insert_with_value(dict, "alpha", 1));
    expected.insert("alpha".to_string(), 1);
    assert!(!MutableMappedDictionary::insert_with_value(
        dict, "alpha", 2
    ));
    expected.insert("alpha".to_string(), 2);

    assert!(MutableMappedDictionary::update_or_insert(
        dict,
        "beta",
        10,
        |value| *value += 5,
    ));
    expected.insert("beta".to_string(), 15);

    assert!(!MutableMappedDictionary::update_or_insert(
        dict,
        "alpha",
        0,
        |value| *value *= 3,
    ));
    expected.insert("alpha".to_string(), 6);

    assert_mapped_dictionary_matches(dict, &expected);

    assert!(MutableDictionary::remove(dict, "beta"));
    expected.remove("beta");
    assert!(!MutableDictionary::remove(dict, "beta"));
    assert_mapped_dictionary_matches(dict, &expected);
}

#[test]
fn byte_pathmap_refines_reference_map_and_zipper_traversal() {
    let expected = reference_entries();
    let dict = PathMapDictionary::from_terms_with_values(
        expected.iter().map(|(term, value)| (term.as_str(), *value)),
    );

    assert_mapped_dictionary_matches(&dict, &expected);
    assert_eq!(byte_entries(&dict), expected);

    let root = dict.root();
    for (term, value) in &expected {
        let node = descend_bytes(root.clone(), term.as_bytes())
            .unwrap_or_else(|| panic!("missing byte path for {term:?}"));
        assert!(node.is_final(), "byte path should be final for {term:?}");
        assert_eq!(node.value(), Some(*value), "node value for {term:?}");
    }

    let caf = descend_bytes(root.clone(), b"caf").expect("caf prefix exists");
    let caf_edges: BTreeSet<u8> = caf.edges().map(|(byte, _)| byte).collect();
    assert!(caf_edges.contains(&b'e'), "ASCII edge after caf");
    assert!(caf_edges.contains(&0xC3), "UTF-8 lead byte edge after caf");

    let caf_lead = caf.transition(0xC3).expect("UTF-8 lead byte path exists");
    let continuation_edges: BTreeSet<u8> = caf_lead.edges().map(|(byte, _)| byte).collect();
    assert!(continuation_edges.contains(&0xA9), "é continuation");
    assert!(continuation_edges.contains(&0xAA), "ê continuation");
    assert!(continuation_edges.contains(&0xAB), "ë continuation");

    let zipper = PathMapZipper::new_from_dict(&dict);
    let cafe_zipper = b"caf\xc3\xa9"
        .iter()
        .fold(Some(zipper), |current, byte| current?.descend(*byte))
        .expect("zipper can descend to café");
    assert!(cafe_zipper.is_final());
    assert_eq!(cafe_zipper.value(), Some(40));
    assert_eq!(String::from_utf8(cafe_zipper.path()).unwrap(), "café");
}

#[test]
fn char_pathmap_edges_are_complete_for_sibling_unicode_scalars() {
    let expected = reference_entries();
    let dict = PathMapDictionaryChar::from_terms_with_values(
        expected.iter().map(|(term, value)| (term.as_str(), *value)),
    );

    assert_mapped_dictionary_matches(&dict, &expected);
    assert_eq!(collect_char_entries(dict.root()), expected);

    let root = dict.root();
    let root_edges: BTreeSet<char> = root.edges().map(|(ch, _)| ch).collect();
    for ch in ['a', 'c', 'é', 'ê', 'ë', '中', '丮', '日', '😀', '😁'] {
        assert!(root_edges.contains(&ch), "root should expose {ch:?}");
    }
    assert_eq!(root.edge_count(), Some(root_edges.len()));

    let caf = descend_chars(root.clone(), "caf").expect("caf prefix exists");
    let caf_edges: BTreeSet<char> = caf.edges().map(|(ch, _)| ch).collect();
    assert_eq!(
        caf_edges,
        BTreeSet::from(['e', 'é', 'ê', 'ë']),
        "PathMapChar edges must enumerate Unicode scalars, not collapse by UTF-8 lead byte"
    );
    assert_eq!(caf.edge_count(), Some(caf_edges.len()));

    for (term, value) in &expected {
        let node = descend_chars(root.clone(), term)
            .unwrap_or_else(|| panic!("missing char path for {term:?}"));
        assert!(node.is_final(), "char path should be final for {term:?}");
        assert_eq!(node.value(), Some(*value), "node value for {term:?}");
    }
}

#[test]
fn pathmap_mutation_and_union_refine_reference_maps() {
    let byte_dict = PathMapDictionary::<i32>::new();
    exercise_mutable_trait(&byte_dict);

    let char_dict = PathMapDictionaryChar::<i32>::new();
    exercise_mutable_trait(&char_dict);

    let left =
        PathMapDictionary::from_terms_with_values([("alpha", 1), ("shared", 2), ("café", 3)]);
    let right =
        PathMapDictionary::from_terms_with_values([("shared", 10), ("beta", 20), ("cafê", 30)]);

    let processed = left.union_with(&right, |left, right| left + right);
    assert_eq!(processed, 3);
    assert_eq!(
        byte_entries(&left),
        BTreeMap::from([
            ("alpha".to_string(), 1),
            ("beta".to_string(), 20),
            ("café".to_string(), 3),
            ("cafê".to_string(), 30),
            ("shared".to_string(), 12),
        ])
    );

    let left_char = PathMapDictionaryChar::from_terms_with_values([("é", 1), ("shared", 2)]);
    let right_char = PathMapDictionaryChar::from_terms_with_values([("ê", 3), ("shared", 5)]);

    let processed = left_char.union_with(&right_char, |left, right| left * 10 + right);
    assert_eq!(processed, 2);
    assert_eq!(left_char.get_value("é"), Some(1));
    assert_eq!(left_char.get_value("ê"), Some(3));
    assert_eq!(left_char.get_value("shared"), Some(25));
    assert_eq!(left_char.len(), Some(3));
}

#[test]
fn factory_preserves_requested_backend_and_feature_gated_availability() {
    let backends = DictionaryFactory::available_backends();
    assert_eq!(backends.len(), 11);
    assert!(backends.contains(&DictionaryBackend::PathMap));
    assert!(backends.contains(&DictionaryBackend::PathMapChar));

    let terms = vec!["alpha", "bravo", "café", "日本語", "rocket🚀"];

    for backend in backends {
        let empty = DictionaryFactory::empty(backend);
        assert_eq!(empty.backend(), backend, "empty backend tag for {backend}");
        assert_eq!(empty.len(), Some(0), "empty len for {backend}");
        assert!(empty.is_empty(), "empty is_empty for {backend}");
        assert!(!empty.contains("alpha"), "empty contains for {backend}");

        let dict = DictionaryFactory::create(backend, terms.clone());
        assert_eq!(dict.backend(), backend, "created backend tag for {backend}");
        assert_eq!(dict.len(), Some(terms.len()), "created len for {backend}");
        assert!(!dict.is_empty(), "created is_empty for {backend}");
        for term in &terms {
            assert!(dict.contains(term), "{backend} should contain {term:?}");
        }
        assert!(
            !dict.contains("zz_absent_zz"),
            "{backend} should reject an unrelated absent probe"
        );

        let description = DictionaryFactory::backend_description(backend);
        assert!(!description.trim().is_empty(), "description for {backend}");
    }
}
