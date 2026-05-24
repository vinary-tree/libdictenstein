//! Executable correspondence checks for the DoubleArrayTrie proof boundary.
//!
//! These tests exercise the law surface in
//! `formal-verification/rocq/Spec/DoubleArrayTrieSpec.v` against the public
//! byte and Unicode DAT APIs.

mod common;

use common::strategies::ascii_term;
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
use libdictenstein::double_array_trie_char_zipper::DoubleArrayTrieCharZipper;
use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
use libdictenstein::{
    CharUnit, DictZipper, Dictionary, DictionaryNode, DictionaryTermIterator, ValuedDictZipper,
};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use std::collections::{BTreeMap, BTreeSet};

fn small_unicode_term(min_len: usize, max_len: usize) -> impl Strategy<Value = String> {
    prop::string::string_regex(&format!("[a-zA-Zéèêïöüñαβγδλμ]{{{min_len},{max_len}}}"))
        .expect("valid regex for small unicode terms")
}

fn expected_set(terms: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    terms.into_iter().collect()
}

fn expected_map(pairs: impl IntoIterator<Item = (String, i32)>) -> BTreeMap<String, i32> {
    let mut expected = BTreeMap::new();
    for (term, value) in pairs {
        expected.insert(term, value);
    }
    expected
}

fn absent_probes() -> Vec<String> {
    ["absent", "zzzzzz", "not-present", "λabsent"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn reaches_final<D: Dictionary>(dict: &D, term: &str) -> bool {
    let mut node = dict.root();
    for unit in <D::Node as DictionaryNode>::Unit::iter_str(term) {
        match node.transition(unit) {
            Some(next) => node = next,
            None => return false,
        }
    }
    node.is_final()
}

fn collect_byte_terms(dict: &DoubleArrayTrie<()>) -> BTreeSet<String> {
    dict.iter_terms()
        .map(|bytes| String::from_utf8(bytes).expect("test terms are UTF-8"))
        .collect()
}

fn collect_char_terms(dict: &DoubleArrayTrieChar<()>) -> BTreeSet<String> {
    DictionaryTermIterator::new(DoubleArrayTrieCharZipper::new_from_dict(dict))
        .map(|chars| chars.into_iter().collect())
        .collect()
}

fn descend_byte_zipper(
    zipper: &DoubleArrayTrieZipper<i32>,
    term: &str,
) -> Option<DoubleArrayTrieZipper<i32>> {
    let mut cursor = zipper.clone();
    for byte in term.bytes() {
        cursor = cursor.descend(byte)?;
    }
    Some(cursor)
}

fn descend_char_zipper(
    zipper: &DoubleArrayTrieCharZipper<i32>,
    term: &str,
) -> Option<DoubleArrayTrieCharZipper<i32>> {
    let mut cursor = zipper.clone();
    for ch in term.chars() {
        cursor = cursor.descend(ch)?;
    }
    Some(cursor)
}

fn assert_byte_set_refinement(terms: Vec<String>, probes: &[String]) -> Result<(), TestCaseError> {
    let expected = expected_set(terms);
    let dict = DoubleArrayTrie::from_terms(expected.iter());

    prop_assert_eq!(dict.len(), Some(expected.len()));
    prop_assert_eq!(dict.is_empty(), expected.is_empty());
    prop_assert_eq!(collect_byte_terms(&dict), expected.clone());

    for term in &expected {
        prop_assert!(dict.contains(term), "byte DAT missing term {term}");
        prop_assert!(
            reaches_final(&dict, term),
            "node walk did not end final for {term}"
        );
    }

    for probe in probes {
        if !expected.contains(probe) {
            prop_assert!(
                !dict.contains(probe),
                "byte DAT accepted absent term {probe}"
            );
            prop_assert!(
                !reaches_final(&dict, probe),
                "node walk ended final for absent term {probe}"
            );
        }
    }

    Ok(())
}

fn assert_char_set_refinement(terms: Vec<String>, probes: &[String]) -> Result<(), TestCaseError> {
    let expected = expected_set(terms);
    let dict = DoubleArrayTrieChar::from_terms(expected.iter());

    prop_assert_eq!(dict.len(), Some(expected.len()));
    prop_assert_eq!(collect_char_terms(&dict), expected.clone());

    for term in &expected {
        prop_assert!(dict.contains(term), "char DAT missing term {term}");
        prop_assert!(
            reaches_final(&dict, term),
            "node walk did not end final for {term}"
        );
    }

    for probe in probes {
        if !expected.contains(probe) {
            prop_assert!(
                !dict.contains(probe),
                "char DAT accepted absent term {probe}"
            );
            prop_assert!(
                !reaches_final(&dict, probe),
                "node walk ended final for absent term {probe}"
            );
        }
    }

    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn byte_dat_set_construction_and_node_walk_refine_reference(
        terms in prop::collection::vec(ascii_term(0, 12), 0..=48)
    ) {
        assert_byte_set_refinement(terms, &absent_probes())?;
    }

    #[test]
    fn byte_dat_mapped_lookup_and_iteration_refine_last_wins_map(
        pairs in prop::collection::vec((ascii_term(0, 12), -10_000i32..=10_000), 0..=48)
    ) {
        let expected = expected_map(pairs.clone());
        let dict = DoubleArrayTrie::from_terms_with_values(
            pairs.iter().map(|(term, value)| (term.as_str(), *value))
        );

        prop_assert_eq!(dict.len(), Some(expected.len()));
        let iterated: BTreeMap<String, i32> = dict.iter().collect();
        prop_assert_eq!(iterated, expected.clone());

        for (term, value) in &expected {
            prop_assert!(dict.contains(term), "byte DAT missing mapped term {term}");
            prop_assert_eq!(dict.get_value(term), Some(*value));
        }

        for probe in absent_probes() {
            if !expected.contains_key(&probe) {
                prop_assert_eq!(dict.get_value(&probe), None);
                prop_assert!(!dict.contains(&probe));
            }
        }
    }

    #[test]
    fn char_dat_set_construction_and_node_walk_refine_reference(
        terms in prop::collection::vec(small_unicode_term(0, 8), 0..=32)
    ) {
        assert_char_set_refinement(terms, &absent_probes())?;
    }

    #[test]
    fn char_dat_mapped_lookup_and_iteration_refine_last_wins_map(
        pairs in prop::collection::vec((small_unicode_term(0, 8), -10_000i32..=10_000), 0..=32)
    ) {
        let expected = expected_map(pairs.clone());
        let dict = DoubleArrayTrieChar::from_terms_with_values(
            pairs.iter().map(|(term, value)| (term.as_str(), *value))
        );

        prop_assert_eq!(dict.len(), Some(expected.len()));
        let iterated: BTreeMap<String, i32> = dict.iter().collect();
        prop_assert_eq!(iterated, expected.clone());

        for (term, value) in &expected {
            prop_assert!(dict.contains(term), "char DAT missing mapped term {term}");
            prop_assert_eq!(dict.get_value(term), Some(*value));
        }

        for probe in absent_probes() {
            if !expected.contains_key(&probe) {
                prop_assert_eq!(dict.get_value(&probe), None);
                prop_assert!(!dict.contains(&probe));
            }
        }
    }
}

#[test]
fn byte_dat_membership_is_independent_of_input_order_and_duplicates() {
    let input_a = vec!["delta", "alpha", "alphabet", "alpha", "beta", ""];
    let input_b = vec!["", "beta", "alpha", "delta", "alphabet", "delta"];
    let expected: BTreeSet<String> = input_a.iter().map(|term| (*term).to_string()).collect();

    let dict_a = DoubleArrayTrie::from_terms(input_a);
    let dict_b = DoubleArrayTrie::from_terms(input_b);

    assert_eq!(collect_byte_terms(&dict_a), expected);
    assert_eq!(collect_byte_terms(&dict_b), expected);
    assert_eq!(dict_a.len(), dict_b.len());
}

#[test]
fn mapped_duplicate_values_keep_the_later_value() {
    let byte_pairs = vec![
        ("alpha", 1),
        ("beta", 2),
        ("alpha", 7),
        ("", 9),
        ("beta", 11),
    ];
    let byte = DoubleArrayTrie::from_terms_with_values(byte_pairs);
    assert_eq!(byte.get_value("alpha"), Some(7));
    assert_eq!(byte.get_value("beta"), Some(11));
    assert_eq!(byte.get_value(""), Some(9));

    let char_pairs = vec![
        ("café", 1),
        ("λambda", 2),
        ("café", 7),
        ("", 9),
        ("λambda", 11),
    ];
    let chr = DoubleArrayTrieChar::from_terms_with_values(char_pairs);
    assert_eq!(chr.get_value("café"), Some(7));
    assert_eq!(chr.get_value("λambda"), Some(11));
    assert_eq!(chr.get_value(""), Some(9));
}

#[test]
fn sorted_char_constructor_matches_regular_constructor_for_sorted_entries() {
    let sorted_pairs = vec![
        ("", 0),
        ("alpha", 1),
        ("alpha", 5),
        ("café", 8),
        ("λambda", 13),
        ("λambda", 21),
    ];
    let expected = expected_map(
        sorted_pairs
            .iter()
            .map(|(term, value)| ((*term).to_string(), *value)),
    );

    let regular = DoubleArrayTrieChar::from_terms_with_values(sorted_pairs.clone());
    let sorted = DoubleArrayTrieChar::from_sorted_terms_with_values(sorted_pairs);

    assert_eq!(regular.len(), Some(expected.len()));
    assert_eq!(sorted.len(), Some(expected.len()));
    for (term, value) in expected {
        assert_eq!(regular.get_value(&term), Some(value));
        assert_eq!(sorted.get_value(&term), Some(value));
    }
}

#[test]
fn byte_and_char_zippers_match_base_check_traversal_and_values() {
    let byte = DoubleArrayTrie::from_terms_with_values(vec![
        ("", 0),
        ("car", 1),
        ("cart", 2),
        ("cat", 3),
        ("dog", 4),
    ]);
    let byte_root = DoubleArrayTrieZipper::new_from_dict(&byte);

    for (term, value) in [("", 0), ("car", 1), ("cart", 2), ("cat", 3), ("dog", 4)] {
        let zipper = descend_byte_zipper(&byte_root, term).expect("byte zipper path exists");
        assert!(zipper.is_final());
        assert_eq!(zipper.value(), Some(value));
        assert_eq!(zipper.path(), term.as_bytes());
    }

    let car = descend_byte_zipper(&byte_root, "car").expect("car path");
    let child_labels: Vec<u8> = car.children().map(|(label, _)| label).collect();
    let unique: BTreeSet<u8> = child_labels.iter().copied().collect();
    assert_eq!(unique.len(), child_labels.len());
    assert!(child_labels.contains(&b't'));

    let chr = DoubleArrayTrieChar::from_terms_with_values(vec![
        ("", 0),
        ("café", 1),
        ("cane", 2),
        ("λambda", 3),
    ]);
    let char_root = DoubleArrayTrieCharZipper::new_from_dict(&chr);

    for (term, value) in [("", 0), ("café", 1), ("cane", 2), ("λambda", 3)] {
        let zipper = descend_char_zipper(&char_root, term).expect("char zipper path exists");
        assert!(zipper.is_final());
        assert_eq!(zipper.value(), Some(value));
        assert_eq!(zipper.path().into_iter().collect::<String>(), term);
    }

    let ca = descend_char_zipper(&char_root, "ca").expect("ca path");
    let child_labels: Vec<char> = ca.children().map(|(label, _)| label).collect();
    let unique: BTreeSet<char> = child_labels.iter().copied().collect();
    assert_eq!(unique.len(), child_labels.len());
    assert!(child_labels.contains(&'f'));
    assert!(child_labels.contains(&'n'));
}
