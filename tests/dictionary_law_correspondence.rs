//! Public dictionary-law correspondence tests.
//!
//! These tests execute the law surface stated in
//! `formal-verification/rocq/Spec/DictionaryLawSpec.v` against the public Rust
//! APIs: exact membership, mapped lookup/domain preservation, set-zippers,
//! substring backends, and bijective maps.

mod common;

use common::strategies::{
    ascii_term, dict_ops_strategy, overlapping_term_sets, unicode_term, DictOp,
};
use libdictenstein::bijective::{BijectiveMap, InsertError};
use libdictenstein::difference_zipper::DifferenceZipperExt;
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
use libdictenstein::double_array_trie_char_zipper::DoubleArrayTrieCharZipper;
use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::dynamic_dawg_char::DynamicDawgChar;
use libdictenstein::intersection_zipper::IntersectionZipperExt;
use libdictenstein::scdawg::Scdawg;
use libdictenstein::suffix_automaton::SuffixAutomaton;
use libdictenstein::suffix_automaton_char::SuffixAutomatonChar;
use libdictenstein::symmetric_difference_zipper::SymmetricDifferenceZipperExt;
use libdictenstein::union_zipper::UnionZipperExt;
use libdictenstein::{
    DictZipper, Dictionary, DictionaryTermIterator, MappedDictionary, MutableDictionary,
    MutableMappedDictionary,
};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use std::collections::{BTreeMap, BTreeSet, HashMap};

fn to_set(terms: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    terms.into_iter().collect()
}

fn absent_probes() -> BTreeSet<String> {
    ["", "absent0", "absent1", "not_present"]
        .into_iter()
        .map(String::from)
        .collect()
}

fn assert_exact_dictionary_laws<D: Dictionary>(
    dict: &D,
    expected: &BTreeSet<String>,
    probes: &BTreeSet<String>,
) -> Result<(), TestCaseError> {
    if let Some(len) = dict.len() {
        prop_assert_eq!(len, expected.len(), "len() must match reference set");
    }
    prop_assert_eq!(
        dict.is_empty(),
        expected.is_empty(),
        "is_empty() must agree with len/reference set"
    );

    for term in expected {
        prop_assert!(dict.contains(term), "expected term is missing: {}", term);
    }

    for term in probes {
        if !expected.contains(term) {
            prop_assert!(!dict.contains(term), "unexpected term is visible: {}", term);
        }
    }

    Ok(())
}

fn assert_mapped_dictionary_laws<D: MappedDictionary<Value = i32>>(
    dict: &D,
    expected: &BTreeMap<String, i32>,
    probes: &BTreeSet<String>,
) -> Result<(), TestCaseError> {
    let expected_terms: BTreeSet<String> = expected.keys().cloned().collect();
    assert_exact_dictionary_laws(dict, &expected_terms, probes)?;

    for (term, value) in expected {
        prop_assert_eq!(
            dict.get_value(term),
            Some(*value),
            "value lookup must match reference map for {}",
            term
        );
        prop_assert!(
            dict.contains_with_value(term, |actual| *actual == *value),
            "contains_with_value must accept the stored value for {}",
            term
        );
        prop_assert!(
            !dict.contains_with_value(term, |actual| *actual != *value),
            "contains_with_value must reject nonmatching predicates for {}",
            term
        );
    }

    for term in probes {
        if !expected.contains_key(term) {
            prop_assert_eq!(
                dict.get_value(term),
                None,
                "absent term must not have a value: {}",
                term
            );
            prop_assert!(
                !dict.contains_with_value(term, |_| true),
                "absent term must not satisfy value predicates: {}",
                term
            );
        }
    }

    Ok(())
}

fn collect_dat_terms(dict: &DoubleArrayTrie) -> BTreeSet<String> {
    dict.iter_terms()
        .map(|bytes| String::from_utf8(bytes).expect("test terms are UTF-8"))
        .collect()
}

fn collect_dat_char_terms(dict: &DoubleArrayTrieChar) -> BTreeSet<String> {
    let zipper = DoubleArrayTrieCharZipper::new_from_dict(dict);
    DictionaryTermIterator::new(zipper)
        .map(|chars| chars.into_iter().collect())
        .collect()
}

fn collect_byte_zipper_terms<Z: DictZipper<Unit = u8>>(
    iter: impl Iterator<Item = (Vec<u8>, Z)>,
) -> BTreeSet<String> {
    iter.map(|(path, _)| String::from_utf8(path).expect("test terms are UTF-8"))
        .collect()
}

fn run_mutation_trace<D>(dict: &D, ops: Vec<DictOp<i32>>) -> Result<(), TestCaseError>
where
    D: MutableDictionary + MutableMappedDictionary<Value = i32>,
{
    let mut expected = BTreeMap::new();
    let mut probes = absent_probes();

    for op in ops {
        match op {
            DictOp::Insert(term, value) => {
                let was_new = !expected.contains_key(&term);
                prop_assert_eq!(
                    MutableMappedDictionary::insert_with_value(dict, &term, value),
                    was_new,
                    "insert_with_value return must report whether {} was new",
                    term
                );
                expected.insert(term.clone(), value);
                probes.insert(term);
            }
            DictOp::Remove(term) => {
                let was_present = expected.remove(&term).is_some();
                prop_assert_eq!(
                    MutableDictionary::remove(dict, &term),
                    was_present,
                    "remove return must report whether {} was present",
                    term
                );
                probes.insert(term);
            }
            DictOp::Contains(term) => {
                prop_assert_eq!(
                    dict.contains(&term),
                    expected.contains_key(&term),
                    "contains must match reference map for {}",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(&term),
                    expected.get(&term).copied(),
                    "get_value must match reference map for {}",
                    term
                );
                probes.insert(term);
            }
        }

        assert_mapped_dictionary_laws(dict, &expected, &probes)?;
    }

    Ok(())
}

fn unicode_dict_ops_strategy(count: usize) -> impl Strategy<Value = Vec<DictOp<i32>>> {
    prop::collection::vec(
        prop_oneof![
            (unicode_term(1, 8), -1000i32..=1000)
                .prop_map(|(term, value)| DictOp::Insert(term, value)),
            unicode_term(1, 8).prop_map(DictOp::Remove),
            unicode_term(1, 8).prop_map(DictOp::Contains),
        ],
        1..=count,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn exact_static_backends_match_reference_set(
        terms in prop::collection::vec(ascii_term(1, 16), 0..=40)
    ) {
        let expected = to_set(terms);
        let probes = absent_probes();

        let dat = DoubleArrayTrie::from_terms(expected.iter());
        assert_exact_dictionary_laws(&dat, &expected, &probes)?;
        prop_assert_eq!(collect_dat_terms(&dat), expected.clone());

        let dawg: DynamicDawg<()> = DynamicDawg::from_terms(expected.iter());
        assert_exact_dictionary_laws(&dawg, &expected, &probes)?;
        let dawg_terms: BTreeSet<String> = dawg.iter().map(|(term, _)| term).collect();
        prop_assert_eq!(dawg_terms, expected.clone());

        let scdawg: Scdawg<()> = Scdawg::from_terms(expected.iter());
        assert_exact_dictionary_laws(&scdawg, &expected, &probes)?;
        let scdawg_terms: BTreeSet<String> = scdawg.iter().collect();
        prop_assert_eq!(scdawg_terms, expected);
    }

    #[test]
    fn exact_unicode_static_backends_match_reference_set(
        terms in prop::collection::vec(unicode_term(1, 8), 0..=30)
    ) {
        let expected = to_set(terms);
        let probes = absent_probes();

        let dat = DoubleArrayTrieChar::from_terms(expected.iter());
        assert_exact_dictionary_laws(&dat, &expected, &probes)?;
        prop_assert_eq!(collect_dat_char_terms(&dat), expected.clone());

        let dawg: DynamicDawgChar<()> = DynamicDawgChar::from_terms(expected.iter());
        assert_exact_dictionary_laws(&dawg, &expected, &probes)?;
        let dawg_terms: BTreeSet<String> = dawg.iter().map(|(term, _)| term).collect();
        prop_assert_eq!(dawg_terms, expected);
    }

    #[test]
    fn mapped_static_backends_match_reference_map(
        pairs in prop::collection::vec((ascii_term(1, 16), -10_000i32..=10_000), 0..=40)
    ) {
        let expected: BTreeMap<String, i32> = pairs.into_iter().collect();
        let mut probes = absent_probes();
        probes.extend(expected.keys().cloned());

        let dat = DoubleArrayTrie::from_terms_with_values(
            expected.iter().map(|(term, value)| (term.as_str(), *value))
        );
        assert_mapped_dictionary_laws(&dat, &expected, &probes)?;
        let dat_entries: BTreeMap<String, i32> = dat.iter().collect();
        prop_assert_eq!(dat_entries, expected.clone());

        let dawg: DynamicDawg<i32> = DynamicDawg::from_terms_with_values(
            expected.iter().map(|(term, value)| (term.as_str(), *value))
        );
        assert_mapped_dictionary_laws(&dawg, &expected, &probes)?;
        let dawg_entries: BTreeMap<String, i32> = dawg.iter().collect();
        prop_assert_eq!(dawg_entries, expected.clone());

        let scdawg: Scdawg<i32> = Scdawg::from_terms_with_values(
            expected.iter().map(|(term, value)| (term.as_str(), *value))
        );
        assert_mapped_dictionary_laws(&scdawg, &expected, &probes)?;
    }

    #[test]
    fn dynamic_dawg_mutation_trace_matches_reference_map(
        ops in dict_ops_strategy(36, 0.65, -1000i32..=1000)
    ) {
        let dict: DynamicDawg<i32> = DynamicDawg::new();
        run_mutation_trace(&dict, ops)?;
    }

    #[test]
    fn dynamic_dawg_char_mutation_trace_matches_reference_map(
        ops in unicode_dict_ops_strategy(30)
    ) {
        let dict: DynamicDawgChar<i32> = DynamicDawgChar::new();
        run_mutation_trace(&dict, ops)?;
    }

    #[test]
    fn zipper_boolean_algebra_matches_reference_sets(
        (terms_a, terms_b) in overlapping_term_sets(16, 0.4)
    ) {
        let set_a: BTreeSet<String> = terms_a.into_iter().collect();
        let set_b: BTreeSet<String> = terms_b.into_iter().collect();
        let dict_a = DoubleArrayTrie::from_terms(set_a.iter());
        let dict_b = DoubleArrayTrie::from_terms(set_b.iter());

        let union = collect_byte_zipper_terms(
            DoubleArrayTrieZipper::new_from_dict(&dict_a)
                .union_with(DoubleArrayTrieZipper::new_from_dict(&dict_b))
                .iter(),
        );
        let expected_union: BTreeSet<String> = set_a.union(&set_b).cloned().collect();
        prop_assert_eq!(union, expected_union);

        let intersection = collect_byte_zipper_terms(
            DoubleArrayTrieZipper::new_from_dict(&dict_a)
                .intersection_with(DoubleArrayTrieZipper::new_from_dict(&dict_b))
                .iter(),
        );
        let expected_intersection: BTreeSet<String> =
            set_a.intersection(&set_b).cloned().collect();
        prop_assert_eq!(intersection, expected_intersection);

        let difference = collect_byte_zipper_terms(
            DoubleArrayTrieZipper::new_from_dict(&dict_a)
                .difference_from(DoubleArrayTrieZipper::new_from_dict(&dict_b))
                .iter(),
        );
        let expected_difference: BTreeSet<String> =
            set_a.difference(&set_b).cloned().collect();
        prop_assert_eq!(difference, expected_difference);

        let symmetric_difference = collect_byte_zipper_terms(
            DoubleArrayTrieZipper::new_from_dict(&dict_a)
                .symmetric_difference_with(DoubleArrayTrieZipper::new_from_dict(&dict_b))
                .iter(),
        );
        let expected_symmetric_difference: BTreeSet<String> =
            set_a.symmetric_difference(&set_b).cloned().collect();
        prop_assert_eq!(symmetric_difference, expected_symmetric_difference);
    }

    #[test]
    fn suffix_backends_expose_substring_laws(
        text in ascii_term(3, 24)
    ) {
        let substring = &text[1..text.len() - 1];

        let suffix: SuffixAutomaton<()> = SuffixAutomaton::from_text(&text);
        prop_assert!(suffix.is_suffix_based());
        prop_assert!(suffix.contains(&text));
        prop_assert!(suffix.contains(substring));
        prop_assert_eq!(suffix.len(), Some(1));

        let scdawg: Scdawg<()> = Scdawg::from_terms([text.as_str()]);
        prop_assert!(!scdawg.is_suffix_based());
        prop_assert!(scdawg.contains(&text));
        prop_assert!(scdawg.contains_substring(substring));
    }

    #[test]
    fn bijective_map_matches_forward_reverse_reference(
        pairs in prop::collection::vec((ascii_term(1, 16), -1000i32..=1000), 0..=40)
    ) {
        let bimap: BijectiveMap<i32> = BijectiveMap::new();
        let mut forward = BTreeMap::new();
        let mut reverse = HashMap::new();

        for (term, value) in pairs {
            let expected_insert = if forward.contains_key(&term) {
                Err(InsertError::DuplicateTerm)
            } else if reverse.contains_key(&value) {
                Err(InsertError::DuplicateValue)
            } else {
                Ok(())
            };

            prop_assert_eq!(bimap.try_insert(&term, value), expected_insert);

            if expected_insert.is_ok() {
                forward.insert(term.clone(), value);
                reverse.insert(value, term);
            }

            prop_assert_eq!(bimap.len(), forward.len());
            prop_assert_eq!(bimap.is_empty(), forward.is_empty());

            for (term, value) in &forward {
                prop_assert!(bimap.contains_term(term));
                prop_assert!(bimap.contains_value(value));
                prop_assert_eq!(bimap.get_value(term), Some(*value));
                let recovered = bimap.get_term(value);
                prop_assert_eq!(recovered.as_deref(), Some(term.as_str()));
            }

            for (value, term) in &reverse {
                let recovered = bimap.get_term(value);
                prop_assert_eq!(recovered.as_deref(), Some(term.as_str()));
            }
        }

        let iterated: BTreeMap<String, i32> = bimap.iter().collect();
        prop_assert_eq!(iterated, forward);
    }
}

#[test]
fn suffix_char_backend_exposes_unicode_substring_laws() {
    let suffix: SuffixAutomatonChar<()> = SuffixAutomatonChar::new();
    assert!(suffix.insert("cafe alpha"));
    assert!(suffix.insert("delta beta"));

    assert!(suffix.is_suffix_based());
    assert!(suffix.contains("alpha"));
    assert!(suffix.contains("beta"));
    assert!(suffix.contains("cafe"));
    assert!(!suffix.contains("gamma"));
    assert_eq!(suffix.len(), Some(2));
}

#[cfg(feature = "persistent-artrie")]
mod persistent {
    use super::*;

    use libdictenstein::persistent_artrie::PersistentARTrie;
    use libdictenstein::persistent_vocab_artrie::PersistentVocabARTrie;
    use libdictenstein::BijectiveDictionary;
    use tempfile::tempdir;

    #[test]
    fn persistent_artrie_public_map_laws_match_reference() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("dict.part");
        let trie = PersistentARTrie::<i32>::create(&path).expect("create trie");

        let mut expected = BTreeMap::new();
        let mut probes = absent_probes();

        assert!(trie.insert_with_value("alpha", 1));
        expected.insert("alpha".to_string(), 1);
        probes.insert("alpha".to_string());
        assert_mapped_dictionary_laws(&trie, &expected, &probes).unwrap();

        assert!(trie.insert_with_value("beta", 2));
        expected.insert("beta".to_string(), 2);
        probes.insert("beta".to_string());
        assert_mapped_dictionary_laws(&trie, &expected, &probes).unwrap();

        assert!(!trie.insert_with_value("alpha", 7));
        expected.insert("alpha".to_string(), 7);
        assert_mapped_dictionary_laws(&trie, &expected, &probes).unwrap();

        assert!(trie.remove("beta"));
        expected.remove("beta");
        assert_mapped_dictionary_laws(&trie, &expected, &probes).unwrap();
    }

    #[test]
    fn persistent_vocab_public_bijection_laws_match_reference_indices() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("vocab.dict");
        let mut vocab = PersistentVocabARTrie::create(&path).expect("create vocab");

        let terms = ["alpha", "beta", "gamma", "delta"];
        for (expected_index, term) in terms.iter().enumerate() {
            let index = vocab.insert(term).expect("insert vocab term");
            assert_eq!(index, expected_index as u64);
            assert_eq!(vocab.get_value(term), Some(index));
            assert_eq!(
                BijectiveDictionary::get_term(&vocab, &index).as_deref(),
                Some(*term)
            );
        }

        let duplicate = vocab.insert("beta").expect("insert duplicate vocab term");
        assert_eq!(duplicate, 1);
        assert_eq!(Dictionary::len(&vocab), Some(terms.len()));
        assert!(BijectiveDictionary::get_term(&vocab, &99).is_none());
    }
}
