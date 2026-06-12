//! DynamicDawg mutation correspondence tests.
//!
//! These tests instantiate the operation laws from
//! `formal-verification/rocq/Spec/DynamicDawgMutationSpec.v` against the public
//! byte and character DAWG APIs.  They check semantic preservation for
//! insert/update/remove, batch extend/remove-many, compaction, minimization, and
//! Bloom-filter-backed lookup.  They do not assert exact node counts.

mod common;

use common::strategies::{ascii_term, unicode_term};
use libdictenstein::dynamic_dawg::char::DynamicDawgChar;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::{
    CompactableDictionary, Dictionary, MappedDictionary, MutableDictionary, MutableMappedDictionary,
};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
enum MappedMutationOp {
    InsertWithValue(String, i32),
    UpdateOrInsert(String, i32, i32),
    Remove(String),
    Compact,
    Minimize,
    Check(String),
}

#[derive(Debug, Clone)]
enum SetMutationOp {
    Insert(String),
    Remove(String),
    Extend(Vec<String>),
    RemoveMany(Vec<String>),
    Compact,
    Minimize,
    Check(String),
}

fn mapped_ops_strategy(
    term: BoxedStrategy<String>,
    count: usize,
) -> impl Strategy<Value = Vec<MappedMutationOp>> {
    prop::collection::vec(
        prop_oneof![
            4 => (term.clone(), -1000i32..=1000)
                .prop_map(|(term, value)| MappedMutationOp::InsertWithValue(term, value)),
            3 => (term.clone(), -1000i32..=1000, -50i32..=50)
                .prop_map(|(term, default, delta)| {
                    MappedMutationOp::UpdateOrInsert(term, default, delta)
                }),
            2 => term.clone().prop_map(MappedMutationOp::Remove),
            1 => Just(MappedMutationOp::Compact),
            1 => Just(MappedMutationOp::Minimize),
            2 => term.prop_map(MappedMutationOp::Check),
        ],
        1..=count,
    )
}

fn set_ops_strategy(
    term: BoxedStrategy<String>,
    count: usize,
) -> impl Strategy<Value = Vec<SetMutationOp>> {
    prop::collection::vec(
        prop_oneof![
            3 => term.clone().prop_map(SetMutationOp::Insert),
            2 => term.clone().prop_map(SetMutationOp::Remove),
            2 => prop::collection::vec(term.clone(), 0..=8).prop_map(SetMutationOp::Extend),
            2 => prop::collection::vec(term.clone(), 0..=8).prop_map(SetMutationOp::RemoveMany),
            1 => Just(SetMutationOp::Compact),
            1 => Just(SetMutationOp::Minimize),
            2 => term.prop_map(SetMutationOp::Check),
        ],
        1..=count,
    )
}

fn assert_mapped_state<D>(
    dict: &D,
    expected: &BTreeMap<String, i32>,
    probes: &BTreeSet<String>,
) -> Result<(), TestCaseError>
where
    D: Dictionary + MappedDictionary<Value = i32>,
{
    if let Some(len) = dict.len() {
        prop_assert_eq!(len, expected.len(), "mapped len() must match model");
    }

    for (term, expected_value) in expected {
        prop_assert!(dict.contains(term), "mapped term is missing: {}", term);
        prop_assert_eq!(
            dict.get_value(term),
            Some(*expected_value),
            "mapped value mismatch for {}",
            term
        );
        prop_assert!(
            dict.contains_with_value(term, |actual| *actual == *expected_value),
            "contains_with_value rejected the model value for {}",
            term
        );
    }

    for term in probes {
        if !expected.contains_key(term) {
            prop_assert!(
                !dict.contains(term),
                "removed/absent term is visible: {}",
                term
            );
            prop_assert_eq!(
                dict.get_value(term),
                None,
                "removed/absent term retained a value: {}",
                term
            );
            prop_assert!(
                !dict.contains_with_value(term, |_| true),
                "removed/absent term satisfied a value predicate: {}",
                term
            );
        }
    }

    Ok(())
}

fn assert_set_state<D>(
    dict: &D,
    expected: &BTreeSet<String>,
    probes: &BTreeSet<String>,
) -> Result<(), TestCaseError>
where
    D: Dictionary,
{
    if let Some(len) = dict.len() {
        prop_assert_eq!(len, expected.len(), "set len() must match model");
    }

    for term in expected {
        prop_assert!(dict.contains(term), "set term is missing: {}", term);
    }

    for term in probes {
        if !expected.contains(term) {
            prop_assert!(
                !dict.contains(term),
                "removed/absent term is visible: {}",
                term
            );
        }
    }

    Ok(())
}

fn run_mapped_trace<D>(dict: &D, ops: &[MappedMutationOp]) -> Result<(), TestCaseError>
where
    D: Dictionary
        + MappedDictionary<Value = i32>
        + MutableDictionary
        + MutableMappedDictionary<Value = i32>
        + CompactableDictionary,
{
    let mut expected = BTreeMap::new();
    let mut probes = BTreeSet::from([
        String::new(),
        "absent".to_string(),
        "missing".to_string(),
        "not-present".to_string(),
    ]);

    for op in ops {
        match op {
            MappedMutationOp::InsertWithValue(term, value) => {
                let was_new = !expected.contains_key(term);
                prop_assert_eq!(
                    MutableMappedDictionary::insert_with_value(dict, term, *value),
                    was_new,
                    "insert_with_value return mismatch for {}",
                    term
                );
                expected.insert(term.clone(), *value);
                probes.insert(term.clone());
            }
            MappedMutationOp::UpdateOrInsert(term, default, delta) => {
                let was_new = !expected.contains_key(term);
                prop_assert_eq!(
                    MutableMappedDictionary::update_or_insert(dict, term, *default, |value| {
                        *value += *delta;
                    }),
                    was_new,
                    "update_or_insert return mismatch for {}",
                    term
                );
                expected
                    .entry(term.clone())
                    .and_modify(|value| *value += *delta)
                    .or_insert(*default);
                probes.insert(term.clone());
            }
            MappedMutationOp::Remove(term) => {
                let was_present = expected.remove(term).is_some();
                prop_assert_eq!(
                    MutableDictionary::remove(dict, term),
                    was_present,
                    "remove return mismatch for {}",
                    term
                );
                if was_present {
                    prop_assert!(
                        CompactableDictionary::needs_compaction(dict),
                        "successful remove should mark the DAWG for compaction"
                    );
                }
                probes.insert(term.clone());
            }
            MappedMutationOp::Compact => {
                let _ = CompactableDictionary::compact(dict);
                prop_assert!(
                    !CompactableDictionary::needs_compaction(dict),
                    "compact must clear the compaction flag"
                );
            }
            MappedMutationOp::Minimize => {
                let _ = CompactableDictionary::minimize(dict);
                prop_assert!(
                    !CompactableDictionary::needs_compaction(dict),
                    "minimize must clear the compaction flag"
                );
            }
            MappedMutationOp::Check(term) => {
                prop_assert_eq!(
                    dict.contains(term),
                    expected.contains_key(term),
                    "contains mismatch for {}",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(term),
                    expected.get(term).copied(),
                    "get_value mismatch for {}",
                    term
                );
                probes.insert(term.clone());
            }
        }

        assert_mapped_state(dict, &expected, &probes)?;
    }

    Ok(())
}

fn run_set_trace<D>(dict: &D, ops: &[SetMutationOp]) -> Result<(), TestCaseError>
where
    D: Dictionary + MutableDictionary + CompactableDictionary,
{
    let mut expected = BTreeSet::new();
    let mut probes = BTreeSet::from([
        String::new(),
        "absent".to_string(),
        "missing".to_string(),
        "not-present".to_string(),
    ]);

    for op in ops {
        match op {
            SetMutationOp::Insert(term) => {
                let was_new = expected.insert(term.clone());
                prop_assert_eq!(
                    MutableDictionary::insert(dict, term),
                    was_new,
                    "insert return mismatch for {}",
                    term
                );
                probes.insert(term.clone());
            }
            SetMutationOp::Remove(term) => {
                let was_present = expected.remove(term);
                prop_assert_eq!(
                    MutableDictionary::remove(dict, term),
                    was_present,
                    "remove return mismatch for {}",
                    term
                );
                if was_present {
                    prop_assert!(
                        CompactableDictionary::needs_compaction(dict),
                        "successful remove should mark the DAWG for compaction"
                    );
                }
                probes.insert(term.clone());
            }
            SetMutationOp::Extend(terms) => {
                let added = terms
                    .iter()
                    .filter(|term| expected.insert((*term).clone()))
                    .count();
                prop_assert_eq!(
                    MutableDictionary::extend(dict, terms.iter()),
                    added,
                    "extend return must count distinct newly added terms"
                );
                if added > 0 {
                    prop_assert!(
                        !CompactableDictionary::needs_compaction(dict),
                        "extend with additions compacts before returning"
                    );
                }
                probes.extend(terms.iter().cloned());
            }
            SetMutationOp::RemoveMany(terms) => {
                let removed = terms.iter().filter(|term| expected.remove(*term)).count();
                prop_assert_eq!(
                    MutableDictionary::remove_many(dict, terms.iter()),
                    removed,
                    "remove_many return must count distinct removed terms"
                );
                if removed > 0 {
                    prop_assert!(
                        !CompactableDictionary::needs_compaction(dict),
                        "remove_many with removals compacts before returning"
                    );
                }
                probes.extend(terms.iter().cloned());
            }
            SetMutationOp::Compact => {
                let _ = CompactableDictionary::compact(dict);
                prop_assert!(
                    !CompactableDictionary::needs_compaction(dict),
                    "compact must clear the compaction flag"
                );
            }
            SetMutationOp::Minimize => {
                let _ = CompactableDictionary::minimize(dict);
                prop_assert!(
                    !CompactableDictionary::needs_compaction(dict),
                    "minimize must clear the compaction flag"
                );
            }
            SetMutationOp::Check(term) => {
                prop_assert_eq!(
                    dict.contains(term),
                    expected.contains(term),
                    "contains mismatch for {}",
                    term
                );
                probes.insert(term.clone());
            }
        }

        assert_set_state(dict, &expected, &probes)?;
    }

    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn byte_dynamic_dawg_mapped_mutations_refine_reference_map(
        ops in mapped_ops_strategy(ascii_term(0, 16).boxed(), 80)
    ) {
        let dict: DynamicDawg<i32> = DynamicDawg::new();
        run_mapped_trace(&dict, &ops)?;
    }

    #[test]
    fn char_dynamic_dawg_mapped_mutations_refine_reference_map(
        ops in mapped_ops_strategy(unicode_term(0, 12).boxed(), 80)
    ) {
        let dict: DynamicDawgChar<i32> = DynamicDawgChar::new();
        run_mapped_trace(&dict, &ops)?;
    }

    #[test]
    fn byte_dynamic_dawg_batch_set_mutations_refine_reference_set(
        ops in set_ops_strategy(ascii_term(0, 16).boxed(), 80)
    ) {
        let dict: DynamicDawg<()> = DynamicDawg::new();
        run_set_trace(&dict, &ops)?;
    }

    #[test]
    fn char_dynamic_dawg_batch_set_mutations_refine_reference_set(
        ops in set_ops_strategy(unicode_term(0, 12).boxed(), 80)
    ) {
        let dict: DynamicDawgChar<()> = DynamicDawgChar::new();
        run_set_trace(&dict, &ops)?;
    }
}

#[test]
fn bloom_backed_dynamic_dawg_has_no_false_negatives_after_rebuilds() {
    let byte: DynamicDawg<i32> = DynamicDawg::with_config(1.05, Some(256));
    let char_dawg: DynamicDawgChar<i32> = DynamicDawgChar::with_config(1.05, Some(256));
    let terms = [
        ("alpha", 1),
        ("alphabet", 2),
        ("alphanumeric", 3),
        ("beta", 4),
        ("béτα", 5),
        ("gamma", 6),
        ("一二三", 7),
        ("suffix", 8),
        ("presuffix", 9),
    ];
    let removed = ["beta", "suffix"];

    for (term, value) in terms {
        assert!(byte.insert_with_value(term, value));
        assert!(char_dawg.insert_with_value(term, value));
    }

    assert!(!byte.update_or_insert("alphabet", 0, |value| *value += 20));
    assert!(!char_dawg.update_or_insert("alphabet", 0, |value| *value += 20));

    for term in removed {
        assert!(byte.remove(term));
        assert!(char_dawg.remove(term));
    }

    let _ = byte.compact();
    let _ = byte.minimize();
    let _ = char_dawg.compact();
    let _ = char_dawg.minimize();

    for (term, original_value) in terms {
        if removed.contains(&term) {
            assert!(!byte.contains(term));
            assert!(!char_dawg.contains(term));
            assert_eq!(byte.get_value(term), None);
            assert_eq!(char_dawg.get_value(term), None);
        } else {
            let expected_value = if term == "alphabet" {
                original_value + 20
            } else {
                original_value
            };
            assert!(byte.contains(term), "byte DAWG lost {}", term);
            assert!(char_dawg.contains(term), "char DAWG lost {}", term);
            assert_eq!(byte.get_value(term), Some(expected_value));
            assert_eq!(char_dawg.get_value(term), Some(expected_value));
        }
    }

    for absent in ["alp", "delta", "一二", "suffixes"] {
        assert!(!byte.contains(absent));
        assert!(!char_dawg.contains(absent));
        assert_eq!(byte.get_value(absent), None);
        assert_eq!(char_dawg.get_value(absent), None);
    }
}
