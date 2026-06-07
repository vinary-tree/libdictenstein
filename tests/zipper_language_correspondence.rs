//! Executable correspondence checks for zipper language laws.
//!
//! These tests execute the law surface stated in
//! `formal-verification/rocq/Spec/ZipperLanguageSpec.v`: descent, child
//! iteration, finality, term iteration, valued lookup, and zipper combinators
//! refine finite reference languages/maps.

mod common;

use common::strategies::{ascii_term, overlapping_term_sets, unicode_term};
use libdictenstein::difference_zipper::DifferenceZipperExt;
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
use libdictenstein::double_array_trie_char_zipper::DoubleArrayTrieCharZipper;
use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::dynamic_dawg_char::DynamicDawgChar;
use libdictenstein::dynamic_dawg_char_zipper::DynamicDawgCharZipper;
use libdictenstein::dynamic_dawg_zipper::DynamicDawgZipper;
use libdictenstein::excluding_prefix_zipper::ExcludingPrefixZipper;
use libdictenstein::intersection_zipper::IntersectionZipperExt;
use libdictenstein::prefix_zipper::PrefixZipper;
use libdictenstein::scdawg::Scdawg;
use libdictenstein::scdawg_char::ScdawgChar;
use libdictenstein::suffix_automaton::SuffixAutomaton;
use libdictenstein::suffix_automaton_char::SuffixAutomatonChar;
use libdictenstein::suffix_automaton_char_zipper::SuffixAutomatonCharZipper;
use libdictenstein::suffix_automaton_zipper::SuffixAutomatonZipper;
use libdictenstein::symmetric_difference_zipper::SymmetricDifferenceZipperExt;
use libdictenstein::union_zipper::UnionZipperExt;
use libdictenstein::value_diff_zipper::ValueDiffZipperExt;
use libdictenstein::{
    DictZipper, Dictionary, DictionaryNode, DictionaryTermIterator, ValuedDictZipper,
};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use std::collections::{BTreeMap, BTreeSet};

fn as_set(terms: Vec<String>) -> BTreeSet<String> {
    terms.into_iter().collect()
}

fn navigate_zipper<Z>(root: &Z, path: &[Z::Unit]) -> Option<Z>
where
    Z: DictZipper,
    Z::Unit: Copy,
{
    path.iter()
        .copied()
        .try_fold(root.clone(), |node, unit| node.descend(unit))
}

fn navigate_node<N>(root: &N, path: &[N::Unit]) -> Option<N>
where
    N: DictionaryNode,
    N::Unit: Copy,
{
    path.iter()
        .copied()
        .try_fold(root.clone(), |node, unit| node.transition(unit))
}

fn collect_byte_terms<Z>(root: Z) -> BTreeSet<String>
where
    Z: DictZipper<Unit = u8>,
{
    DictionaryTermIterator::new(root)
        .map(|path| String::from_utf8(path).expect("test terms are UTF-8"))
        .collect()
}

fn collect_char_terms<Z>(root: Z) -> BTreeSet<String>
where
    Z: DictZipper<Unit = char>,
{
    DictionaryTermIterator::new(root)
        .map(|path| path.into_iter().collect())
        .collect()
}

fn collect_byte_values<Z>(root: Z) -> BTreeMap<String, i32>
where
    Z: ValuedDictZipper<Unit = u8, Value = i32>,
{
    libdictenstein::DictionaryIterator::new(root)
        .map(|(path, value)| {
            (
                String::from_utf8(path).expect("test terms are UTF-8"),
                value,
            )
        })
        .collect()
}

fn expected_byte_children(terms: &BTreeSet<String>, prefix: &[u8]) -> BTreeSet<u8> {
    terms
        .iter()
        .filter_map(|term| {
            let bytes = term.as_bytes();
            bytes
                .starts_with(prefix)
                .then(|| bytes.get(prefix.len()).copied())
                .flatten()
        })
        .collect()
}

fn expected_char_children(terms: &BTreeSet<String>, prefix: &[char]) -> BTreeSet<char> {
    terms
        .iter()
        .filter_map(|term| {
            let chars: Vec<_> = term.chars().collect();
            chars
                .starts_with(prefix)
                .then(|| chars.get(prefix.len()).copied())
                .flatten()
        })
        .collect()
}

fn byte_prefixes(terms: &BTreeSet<String>) -> BTreeSet<Vec<u8>> {
    let mut prefixes = BTreeSet::from([Vec::new()]);
    for term in terms {
        let bytes = term.as_bytes();
        for end in 1..=bytes.len() {
            prefixes.insert(bytes[..end].to_vec());
        }
    }
    prefixes
}

fn char_prefixes(terms: &BTreeSet<String>) -> BTreeSet<Vec<char>> {
    let mut prefixes = BTreeSet::from([Vec::new()]);
    for term in terms {
        let chars: Vec<_> = term.chars().collect();
        for end in 1..=chars.len() {
            prefixes.insert(chars[..end].to_vec());
        }
    }
    prefixes
}

fn assert_byte_zipper_language<Z>(
    root: Z,
    expected: &BTreeSet<String>,
    probes: &[&str],
) -> Result<(), TestCaseError>
where
    Z: DictZipper<Unit = u8>,
{
    prop_assert_eq!(
        collect_byte_terms(root.clone()),
        expected.clone(),
        "iterator output must equal the reference language"
    );

    for term in expected {
        let bytes = term.as_bytes();
        let node = navigate_zipper(&root, bytes).expect("accepted term must be reachable");
        prop_assert_eq!(node.path(), bytes.to_vec());
        prop_assert!(node.is_final(), "accepted term must be final: {}", term);
    }

    for prefix in byte_prefixes(expected) {
        let node = navigate_zipper(&root, &prefix).expect("live prefix must be reachable");
        let actual_children: BTreeSet<_> = node.children().map(|(label, _)| label).collect();
        prop_assert_eq!(
            actual_children,
            expected_byte_children(expected, &prefix),
            "children must match one-step reference extensions for {:?}",
            prefix
        );
    }

    for probe in probes {
        let bytes = probe.as_bytes();
        let reachable = bytes.is_empty()
            || expected
                .iter()
                .any(|term| term.as_bytes().starts_with(bytes));
        match navigate_zipper(&root, bytes) {
            Some(node) => {
                prop_assert!(reachable, "dead probe unexpectedly reachable: {}", probe);
                prop_assert_eq!(node.is_final(), expected.contains(*probe));
            }
            None => {
                prop_assert!(!reachable, "live prefix probe was not reachable: {}", probe);
            }
        }
    }

    Ok(())
}

fn assert_char_zipper_language<Z>(
    root: Z,
    expected: &BTreeSet<String>,
    probes: &[&str],
) -> Result<(), TestCaseError>
where
    Z: DictZipper<Unit = char>,
{
    prop_assert_eq!(
        collect_char_terms(root.clone()),
        expected.clone(),
        "iterator output must equal the reference language"
    );

    for term in expected {
        let chars: Vec<_> = term.chars().collect();
        let node = navigate_zipper(&root, &chars).expect("accepted term must be reachable");
        prop_assert_eq!(node.path(), chars);
        prop_assert!(node.is_final(), "accepted term must be final: {}", term);
    }

    for prefix in char_prefixes(expected) {
        let node = navigate_zipper(&root, &prefix).expect("live prefix must be reachable");
        let actual_children: BTreeSet<_> = node.children().map(|(label, _)| label).collect();
        prop_assert_eq!(
            actual_children,
            expected_char_children(expected, &prefix),
            "children must match one-step reference extensions for {:?}",
            prefix
        );
    }

    for probe in probes {
        let chars: Vec<_> = probe.chars().collect();
        let reachable = chars.is_empty()
            || expected.iter().any(|term| {
                let term_chars: Vec<_> = term.chars().collect();
                term_chars.starts_with(&chars)
            });
        match navigate_zipper(&root, &chars) {
            Some(node) => {
                prop_assert!(reachable, "dead probe unexpectedly reachable: {}", probe);
                prop_assert_eq!(node.is_final(), expected.contains(*probe));
            }
            None => {
                prop_assert!(!reachable, "live prefix probe was not reachable: {}", probe);
            }
        }
    }

    Ok(())
}

fn all_byte_substrings(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let mut substrings = BTreeSet::new();
    for start in 0..bytes.len() {
        for end in start + 1..=bytes.len() {
            substrings.insert(
                String::from_utf8(bytes[start..end].to_vec()).expect("ASCII fixture substring"),
            );
        }
    }
    substrings
}

fn all_char_substrings(text: &str) -> BTreeSet<String> {
    let chars: Vec<_> = text.chars().collect();
    let mut substrings = BTreeSet::new();
    for start in 0..chars.len() {
        for end in start + 1..=chars.len() {
            substrings.insert(chars[start..end].iter().collect());
        }
    }
    substrings
}

fn collect_zipper_iter<Z>(iter: impl Iterator<Item = (Vec<u8>, Z)>) -> BTreeSet<String>
where
    Z: DictZipper<Unit = u8>,
{
    iter.map(|(path, _)| String::from_utf8(path).expect("test terms are UTF-8"))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn byte_backend_zippers_refine_exact_reference_language(
        terms in prop::collection::vec(ascii_term(1, 12), 0..=32)
    ) {
        let expected = as_set(terms);
        let probes = ["", "missing", "zzzz", "notpresent"];

        let dat = DoubleArrayTrie::from_terms(expected.iter());
        assert_byte_zipper_language(
            DoubleArrayTrieZipper::new_from_dict(&dat),
            &expected,
            &probes,
        )?;

        let dawg: DynamicDawg<()> = DynamicDawg::from_terms(expected.iter());
        assert_byte_zipper_language(
            DynamicDawgZipper::new_from_dict(&dawg),
            &expected,
            &probes,
        )?;
    }

    #[test]
    fn char_backend_zippers_refine_exact_reference_language(
        terms in prop::collection::vec(unicode_term(1, 8), 0..=24)
    ) {
        let expected = as_set(terms);
        let probes = ["", "missing", "zzzz", "notpresent"];

        let dat = DoubleArrayTrieChar::from_terms(expected.iter());
        assert_char_zipper_language(
            DoubleArrayTrieCharZipper::new_from_dict(&dat),
            &expected,
            &probes,
        )?;

        let dawg: DynamicDawgChar<()> = DynamicDawgChar::from_terms(expected.iter());
        assert_char_zipper_language(
            DynamicDawgCharZipper::new_from_dict(&dawg),
            &expected,
            &probes,
        )?;
    }

    #[test]
    fn valued_byte_zippers_refine_reference_maps(
        pairs in prop::collection::vec((ascii_term(1, 12), -1000i32..=1000), 0..=32)
    ) {
        let expected: BTreeMap<String, i32> = pairs.into_iter().collect();
        let expected_terms: BTreeSet<_> = expected.keys().cloned().collect();
        let probes = ["", "missing", "zzzz", "notpresent"];

        let dat = DoubleArrayTrie::from_terms_with_values(
            expected.iter().map(|(term, value)| (term.as_str(), *value))
        );
        let dat_zipper = DoubleArrayTrieZipper::new_from_dict(&dat);
        assert_byte_zipper_language(dat_zipper.clone(), &expected_terms, &probes)?;
        prop_assert_eq!(collect_byte_values(dat_zipper), expected.clone());

        let dawg: DynamicDawg<i32> = DynamicDawg::from_terms_with_values(
            expected.iter().map(|(term, value)| (term.as_str(), *value))
        );
        let dawg_zipper = DynamicDawgZipper::new_from_dict(&dawg);
        assert_byte_zipper_language(dawg_zipper.clone(), &expected_terms, &probes)?;
        prop_assert_eq!(collect_byte_values(dawg_zipper), expected);
    }

    #[test]
    fn byte_zipper_set_combinators_refine_reference_algebra(
        (terms_a, terms_b) in overlapping_term_sets(18, 0.4)
    ) {
        let set_a: BTreeSet<_> = terms_a.into_iter().collect();
        let set_b: BTreeSet<_> = terms_b.into_iter().collect();
        let dict_a = DoubleArrayTrie::from_terms(set_a.iter());
        let dict_b = DoubleArrayTrie::from_terms(set_b.iter());

        let root_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
        let root_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

        let union = collect_zipper_iter(root_a.clone().union_with(root_b.clone()).iter());
        let expected_union: BTreeSet<_> = set_a.union(&set_b).cloned().collect();
        prop_assert_eq!(union, expected_union);

        let intersection = collect_zipper_iter(
            root_a.clone().intersection_with(root_b.clone()).iter()
        );
        let expected_intersection: BTreeSet<_> = set_a.intersection(&set_b).cloned().collect();
        prop_assert_eq!(intersection, expected_intersection);

        let difference = collect_zipper_iter(root_a.clone().difference_from(root_b.clone()).iter());
        let expected_difference: BTreeSet<_> = set_a.difference(&set_b).cloned().collect();
        prop_assert_eq!(difference, expected_difference);

        let symmetric_difference = collect_zipper_iter(
            root_a.symmetric_difference_with(root_b).iter()
        );
        let expected_symmetric_difference: BTreeSet<_> =
            set_a.symmetric_difference(&set_b).cloned().collect();
        prop_assert_eq!(symmetric_difference, expected_symmetric_difference);
    }
}

#[test]
fn prefix_and_excluding_zippers_refine_filtered_reference_languages() {
    let terms = BTreeSet::from([
        "api".to_string(),
        "api_get".to_string(),
        "api_post".to_string(),
        "api__hidden".to_string(),
        "app".to_string(),
        "cat".to_string(),
    ]);
    let dict = DoubleArrayTrie::from_terms(terms.iter());
    let root = DoubleArrayTrieZipper::new_from_dict(&dict);

    let prefix_terms = collect_zipper_iter(root.with_prefix(b"api").expect("prefix exists"));
    let expected_prefix: BTreeSet<_> = terms
        .iter()
        .filter(|term| term.starts_with("api"))
        .cloned()
        .collect();
    assert_eq!(prefix_terms, expected_prefix);

    let excluded: &[&[u8]] = &[b"api__"];
    let visible_terms = collect_zipper_iter(root.iter_excluding(excluded));
    let expected_visible: BTreeSet<_> = terms
        .iter()
        .filter(|term| !term.starts_with("api__"))
        .cloned()
        .collect();
    assert_eq!(visible_terms, expected_visible);

    let scoped_visible = collect_zipper_iter(
        root.with_prefix_excluding(b"api", excluded)
            .expect("prefix exists"),
    );
    let expected_scoped_visible: BTreeSet<_> = terms
        .iter()
        .filter(|term| term.starts_with("api") && !term.starts_with("api__"))
        .cloned()
        .collect();
    assert_eq!(scoped_visible, expected_scoped_visible);
}

#[test]
fn value_diff_zipper_refines_reference_map_difference() {
    let left = BTreeMap::from([
        ("cat".to_string(), 1),
        ("dog".to_string(), 2),
        ("fish".to_string(), 3),
    ]);
    let right = BTreeMap::from([
        ("bird".to_string(), 4),
        ("cat".to_string(), 1),
        ("dog".to_string(), 20),
        ("fish".to_string(), 30),
    ]);

    let left_dict = DoubleArrayTrie::from_terms_with_values(
        left.iter().map(|(term, value)| (term.as_str(), *value)),
    );
    let right_dict = DoubleArrayTrie::from_terms_with_values(
        right.iter().map(|(term, value)| (term.as_str(), *value)),
    );

    let diffs: BTreeMap<String, (i32, i32)> = DoubleArrayTrieZipper::new_from_dict(&left_dict)
        .iter_value_diffs(DoubleArrayTrieZipper::new_from_dict(&right_dict))
        .map(|diff| {
            (
                String::from_utf8(diff.path).expect("test terms are UTF-8"),
                (diff.left_value, diff.right_value),
            )
        })
        .collect();

    assert_eq!(
        diffs,
        BTreeMap::from([("dog".to_string(), (2, 20)), ("fish".to_string(), (3, 30)),])
    );
}

#[test]
fn suffix_automaton_zippers_refine_substring_reference_languages() {
    let text = "ababa";
    let expected = all_byte_substrings(text);
    let suffix = SuffixAutomaton::<()>::from_text(text);
    assert_byte_zipper_language(
        SuffixAutomatonZipper::new_from_dict(&suffix),
        &expected,
        &["", "abb", "baa", "z"],
    )
    .unwrap();

    let char_text = "abcab";
    let expected_char = all_char_substrings(char_text);
    let suffix_char = SuffixAutomatonChar::<()>::from_text(char_text);
    assert_char_zipper_language(
        SuffixAutomatonCharZipper::new_from_dict(&suffix_char),
        &expected_char,
        &["", "ac", "caa", "z"],
    )
    .unwrap();
}

#[test]
fn scdawg_handles_refine_exact_and_substring_queries() {
    let expected = BTreeMap::from([
        ("alpha".to_string(), 1),
        ("alpine".to_string(), 2),
        ("beta".to_string(), 3),
    ]);
    let scdawg = Scdawg::from_terms_with_values(
        expected.iter().map(|(term, value)| (term.as_str(), *value)),
    );

    for (term, value) in &expected {
        assert!(Dictionary::contains(&scdawg, term));
        assert_eq!(scdawg.get_value(term), Some(*value));
    }
    assert!(!Dictionary::contains(&scdawg, "alp"));
    assert!(scdawg.contains_substring("lph"));
    assert!(scdawg.find("lph").is_some());

    let char_terms = ["cafe", "cane", "delta"];
    let scdawg_char = ScdawgChar::<()>::from_terms(char_terms);
    for term in char_terms {
        assert!(Dictionary::contains(&scdawg_char, term));
        let chars: Vec<_> = term.chars().collect();
        let node = navigate_node(&scdawg_char.root(), &chars).expect("term path is reachable");
        assert!(node.is_final(), "exact SCDAWG char term must be final");
    }
    assert!(scdawg_char.contains_substring("af"));
    assert!(scdawg_char.find("af").is_some());
}

#[cfg(feature = "persistent-artrie")]
mod persistent {
    use super::*;
    use libdictenstein::persistent_artrie::{
        PersistentARTrie, PersistentARTrieZipper, SharedARTrie,
    };
    use libdictenstein::persistent_artrie_char::{
        PersistentARTrieChar, PersistentARTrieCharZipper,
    };

    use std::sync::Arc;

    #[test]
    fn persistent_zippers_refine_reference_languages() {
        let byte_terms = BTreeSet::from([
            "alpha".to_string(),
            "alpine".to_string(),
            "beta".to_string(),
        ]);
        #[allow(deprecated)]
        let byte_trie = PersistentARTrie::<()>::new();
        for term in &byte_terms {
            assert!(byte_trie.insert(term), "insert byte term: {term}");
        }
        // F4: `SharedARTrie` is now a bare `Arc<…>` (the outer `RwLock` is deleted).
        let shared: SharedARTrie<()> = Arc::new(byte_trie);
        assert_byte_zipper_language(
            PersistentARTrieZipper::new(shared),
            &byte_terms,
            &["", "alp", "missing", "zzzz"],
        )
        .unwrap();

        let char_terms =
            BTreeSet::from(["cafe".to_string(), "cane".to_string(), "delta".to_string()]);
        let char_trie = PersistentARTrieChar::<()>::new();
        for term in &char_terms {
            char_trie.insert(term).expect("insert char term");
        }
        assert_char_zipper_language(
            PersistentARTrieCharZipper::new(&char_trie),
            &char_terms,
            &["", "ca", "missing", "zzzz"],
        )
        .unwrap();
    }
}
