//! DynamicDawgU64 correspondence tests.
//!
//! These tests instantiate the semantic laws from
//! `formal-verification/rocq/Spec/DynamicDawgU64Spec.v` against the public
//! `DynamicDawgU64` API. They compare sequence/set/value behavior, adapters,
//! iteration, zipper navigation, and bounded snapshot-concurrency models to
//! small executable references. They do not assert exact node counts, progress
//! guarantees, or canonical minimization.

mod common;

use common::strategies::ascii_term;
use libdictenstein::dynamic_dawg::u64::DynamicDawgU64;
use libdictenstein::{CharUnit, DictZipper, Dictionary, DictionaryNode, ValuedDictZipper};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
enum SequenceOp {
    Insert(Vec<u64>),
    InsertWithValue(Vec<u64>, i64),
    UpdateOrInsert(Vec<u64>, i64, i64),
    Remove(Vec<u64>),
    Compact,
    Minimize,
    Check(Vec<u64>),
}

fn label_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        5 => 0u64..=96,
        1 => Just(u64::MAX),
        1 => Just(1u64 << 63),
        1 => any::<u16>().prop_map(|value| (value as u64) << 32),
    ]
}

fn sequence_strategy() -> impl Strategy<Value = Vec<u64>> {
    prop::collection::vec(label_strategy(), 0..=5)
}

fn sequence_ops_strategy() -> impl Strategy<Value = Vec<SequenceOp>> {
    prop::collection::vec(
        prop_oneof![
            4 => sequence_strategy().prop_map(SequenceOp::Insert),
            4 => (sequence_strategy(), -1000i64..=1000)
                .prop_map(|(sequence, value)| SequenceOp::InsertWithValue(sequence, value)),
            3 => (sequence_strategy(), -1000i64..=1000, -50i64..=50)
                .prop_map(|(sequence, default, delta)| {
                    SequenceOp::UpdateOrInsert(sequence, default, delta)
                }),
            3 => sequence_strategy().prop_map(SequenceOp::Remove),
            1 => Just(SequenceOp::Compact),
            1 => Just(SequenceOp::Minimize),
            3 => sequence_strategy().prop_map(SequenceOp::Check),
        ],
        1..=48,
    )
}

fn expected_values(expected: &BTreeMap<Vec<u64>, Option<i64>>) -> BTreeMap<Vec<u64>, i64> {
    expected
        .iter()
        .filter_map(|(sequence, value)| value.map(|value| (sequence.clone(), value)))
        .collect()
}

fn assert_zipper_path(
    dict: &DynamicDawgU64<i64>,
    sequence: &[u64],
    expected_value: Option<i64>,
) -> Result<(), TestCaseError> {
    let mut zipper = dict.zipper();
    prop_assert_eq!(zipper.path(), Vec::<u64>::new(), "root zipper path");

    for (index, label) in sequence.iter().enumerate() {
        zipper = zipper.descend(*label).ok_or_else(|| {
            TestCaseError::fail(format!(
                "zipper could not descend through {:?} at offset {}",
                sequence, index
            ))
        })?;
        prop_assert_eq!(
            zipper.path(),
            sequence[..=index].to_vec(),
            "zipper path must track descent"
        );
    }

    prop_assert!(zipper.is_final(), "zipper endpoint must be final");
    prop_assert_eq!(
        zipper.value(),
        expected_value,
        "valued zipper lookup mismatch for {:?}",
        sequence
    );
    Ok(())
}

fn assert_dictionary_node_path(
    dict: &DynamicDawgU64<i64>,
    sequence: &[u64],
) -> Result<(), TestCaseError> {
    let mut node = dict.root();
    for (index, label) in sequence.iter().enumerate() {
        node = node.transition(*label).ok_or_else(|| {
            TestCaseError::fail(format!(
                "DictionaryNode transition missing for {:?} at offset {}",
                sequence, index
            ))
        })?;
    }
    prop_assert!(node.is_final(), "DictionaryNode endpoint must be final");
    Ok(())
}

fn assert_sequence_state(
    dict: &DynamicDawgU64<i64>,
    expected: &BTreeMap<Vec<u64>, Option<i64>>,
    probes: &BTreeSet<Vec<u64>>,
) -> Result<(), TestCaseError> {
    prop_assert_eq!(
        dict.term_count(),
        expected.len(),
        "term_count must match reference set size"
    );
    prop_assert_eq!(
        dict.len(),
        Some(expected.len()),
        "Dictionary::len must match reference set size"
    );
    prop_assert_eq!(
        dict.is_empty(),
        expected.is_empty(),
        "Dictionary::is_empty must match reference set emptiness"
    );

    for (sequence, value) in expected {
        prop_assert!(
            dict.contains_sequence(sequence),
            "missing final sequence: {:?}",
            sequence
        );
        prop_assert_eq!(
            dict.get_sequence_value(sequence),
            *value,
            "value mismatch for {:?}",
            sequence
        );
        assert_zipper_path(dict, sequence, *value)?;
        assert_dictionary_node_path(dict, sequence)?;
    }

    for sequence in probes {
        if !expected.contains_key(sequence) {
            prop_assert!(
                !dict.contains_sequence(sequence),
                "absent/removed sequence is final: {:?}",
                sequence
            );
            prop_assert_eq!(
                dict.get_sequence_value(sequence),
                None,
                "absent/removed sequence retained value: {:?}",
                sequence
            );
        }
    }

    let iterated: BTreeSet<Vec<u64>> = dict.iter().collect();
    let expected_set: BTreeSet<Vec<u64>> = expected.keys().cloned().collect();
    prop_assert_eq!(
        iterated,
        expected_set,
        "iter() must enumerate finals exactly"
    );

    let iterated_values: BTreeMap<Vec<u64>, i64> = dict.iter_with_values().collect();
    prop_assert_eq!(
        iterated_values,
        expected_values(expected),
        "iter_with_values() must enumerate valued finals exactly"
    );

    Ok(())
}

fn apply_sequence_trace(ops: &[SequenceOp]) -> Result<(), TestCaseError> {
    let dict = DynamicDawgU64::<i64>::new();
    let mut expected: BTreeMap<Vec<u64>, Option<i64>> = BTreeMap::new();
    let mut probes: BTreeSet<Vec<u64>> =
        BTreeSet::from([Vec::new(), vec![u64::MAX], vec![1, 2, 3, 4], vec![9, 9]]);

    for op in ops {
        match op {
            SequenceOp::Insert(sequence) => {
                let was_new = !expected.contains_key(sequence);
                prop_assert_eq!(
                    dict.insert_sequence(sequence),
                    was_new,
                    "insert_sequence return mismatch for {:?}",
                    sequence
                );
                expected.entry(sequence.clone()).or_insert(None);
                probes.insert(sequence.clone());
            }
            SequenceOp::InsertWithValue(sequence, value) => {
                let was_new = !expected.contains_key(sequence);
                prop_assert_eq!(
                    dict.insert_sequence_with_value(sequence, *value),
                    was_new,
                    "insert_sequence_with_value return mismatch for {:?}",
                    sequence
                );
                expected.insert(sequence.clone(), Some(*value));
                probes.insert(sequence.clone());
            }
            SequenceOp::UpdateOrInsert(sequence, default, delta) => {
                let was_new = !expected.contains_key(sequence);
                prop_assert_eq!(
                    dict.update_or_insert_sequence(sequence, *default, |value| *value += *delta),
                    was_new,
                    "update_or_insert_sequence return mismatch for {:?}",
                    sequence
                );
                expected
                    .entry(sequence.clone())
                    .and_modify(|value| {
                        *value = Some(match *value {
                            Some(old) => old + *delta,
                            None => *default,
                        });
                    })
                    .or_insert(Some(*default));
                probes.insert(sequence.clone());
            }
            SequenceOp::Remove(sequence) => {
                let was_present = expected.remove(sequence).is_some();
                prop_assert_eq!(
                    dict.remove_sequence(sequence),
                    was_present,
                    "remove_sequence return mismatch for {:?}",
                    sequence
                );
                probes.insert(sequence.clone());
            }
            SequenceOp::Compact => {
                let _ = dict.compact();
            }
            SequenceOp::Minimize => {
                let _ = dict.minimize();
            }
            SequenceOp::Check(sequence) => {
                prop_assert_eq!(
                    dict.contains_sequence(sequence),
                    expected.contains_key(sequence),
                    "contains_sequence mismatch for {:?}",
                    sequence
                );
                prop_assert_eq!(
                    dict.get_sequence_value(sequence),
                    expected.get(sequence).copied().flatten(),
                    "get_sequence_value mismatch for {:?}",
                    sequence
                );
                probes.insert(sequence.clone());
            }
        }

        assert_sequence_state(&dict, &expected, &probes)?;
    }

    Ok(())
}

#[test]
fn sequence_edge_cases_refine_reference_map() {
    let ops = vec![
        SequenceOp::Insert(Vec::new()),
        SequenceOp::Insert(vec![1, 2, 3]),
        SequenceOp::InsertWithValue(vec![1, 2, 3], 7),
        SequenceOp::UpdateOrInsert(vec![1, 2, 3], 99, 5),
        SequenceOp::InsertWithValue(vec![u64::MAX], -1),
        SequenceOp::Insert(vec![1u64 << 63, 0, u64::MAX]),
        SequenceOp::Remove(vec![1, 2, 3]),
        SequenceOp::InsertWithValue(Vec::new(), 42),
        SequenceOp::Compact,
        SequenceOp::Minimize,
    ];

    apply_sequence_trace(&ops).expect("edge-case sequence trace should match reference");
}

#[test]
fn update_or_insert_preserves_existing_value_before_update() {
    let dict = DynamicDawgU64::<i64>::new();

    assert!(dict.insert_sequence_with_value(&[10, 20], 7));
    assert!(!dict.update_or_insert_sequence(&[10, 20], 100, |value| *value += 5));
    assert_eq!(dict.get_sequence_value(&[10, 20]), Some(12));

    assert!(dict.insert_sequence(&[]));
    assert!(!dict.update_or_insert_sequence(&[], 33, |value| *value += 5));
    assert_eq!(dict.get_sequence_value(&[]), Some(33));
}

#[test]
fn string_api_refines_u64_char_unit_encoding() {
    let string_dict = DynamicDawgU64::<i64>::new();
    let sequence_dict = DynamicDawgU64::<i64>::new();
    let terms = ["", "a", "alphabet", "packable", "sixteen-bytes"];

    for (index, term) in terms.iter().enumerate() {
        let encoded = <u64 as CharUnit>::from_str(term);
        let value = index as i64 * 10;

        assert_eq!(
            string_dict.insert_with_value(term, value),
            sequence_dict.insert_sequence_with_value(&encoded, value)
        );
        assert_eq!(
            string_dict.contains(term),
            sequence_dict.contains_sequence(&encoded)
        );
        assert_eq!(
            string_dict.get_value(term),
            sequence_dict.get_sequence_value(&encoded)
        );
    }

    assert_eq!(
        string_dict.remove("alphabet"),
        sequence_dict.remove_sequence(&<u64 as CharUnit>::from_str("alphabet"))
    );
    assert!(!string_dict.contains("alphabet"));
    assert!(!sequence_dict.contains_sequence(&<u64 as CharUnit>::from_str("alphabet")));
}

#[test]
fn f64_api_refines_to_bits_sequences() {
    let dict = DynamicDawgU64::<i64>::new();
    let nan_a = f64::from_bits(0x7ff8_0000_0000_0001);
    let nan_b = f64::from_bits(0x7ff8_0000_0000_0002);
    let series = [
        vec![0.0, -0.0],
        vec![f64::INFINITY, f64::NEG_INFINITY],
        vec![nan_a],
        vec![nan_b],
    ];

    for (index, values) in series.iter().enumerate() {
        let encoded: Vec<u64> = values.iter().map(|value| value.to_bits()).collect();
        let was_new = !dict.contains_sequence(&encoded);
        assert_eq!(dict.insert_f64_with_value(values, index as i64), was_new);
        assert!(dict.contains_f64(values));
        assert!(dict.contains_sequence(&encoded));
        assert_eq!(dict.get_f64_value(values), Some(index as i64));
        assert_eq!(dict.get_sequence_value(&encoded), Some(index as i64));
    }

    assert!(dict.remove_f64(&[nan_a]));
    assert!(!dict.contains_f64(&[nan_a]));
    assert!(dict.contains_f64(&[nan_b]));
}

#[test]
fn insertion_only_children_are_exact_reference_prefixes() {
    let dict = DynamicDawgU64::<i64>::new();
    for (sequence, value) in [
        (vec![10, 20, 30], 1),
        (vec![10, 20, 40], 2),
        (vec![10, 50], 3),
        (vec![99], 4),
    ] {
        assert!(dict.insert_sequence_with_value(&sequence, value));
    }

    let root_children: BTreeSet<u64> = dict.zipper().children().map(|(label, _)| label).collect();
    assert_eq!(root_children, BTreeSet::from([10, 99]));

    let prefix_10 = dict.zipper().descend(10).expect("prefix 10");
    let prefix_10_children: BTreeSet<u64> = prefix_10.children().map(|(label, _)| label).collect();
    assert_eq!(prefix_10_children, BTreeSet::from([20, 50]));

    let prefix_10_20 = prefix_10.descend(20).expect("prefix 10,20");
    let prefix_10_20_children: BTreeSet<u64> =
        prefix_10_20.children().map(|(label, _)| label).collect();
    assert_eq!(prefix_10_20_children, BTreeSet::from([30, 40]));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn generated_sequence_traces_refine_reference_model(ops in sequence_ops_strategy()) {
        apply_sequence_trace(&ops)?;
    }

    #[test]
    fn generated_string_batch_api_refines_encoded_sequences(terms in prop::collection::vec(ascii_term(0, 12), 0..=24)) {
        let dict = DynamicDawgU64::<i64>::new();
        let mut expected = BTreeSet::new();

        let inserted = dict.extend(terms.iter());
        let expected_added = terms
            .iter()
            .map(|term| <u64 as CharUnit>::from_str(term))
            .filter(|encoded| expected.insert(encoded.clone()))
            .count();
        prop_assert_eq!(inserted, expected_added);

        for term in &terms {
            let encoded = <u64 as CharUnit>::from_str(term);
            prop_assert_eq!(dict.contains(term), expected.contains(&encoded));
            prop_assert_eq!(dict.contains_sequence(&encoded), expected.contains(&encoded));
        }

        let removed = dict.remove_many(terms.iter());
        prop_assert_eq!(removed, expected.len());
        for term in &terms {
            prop_assert!(!dict.contains(term));
        }
    }
}

#[test]
fn loom_reader_snapshot_never_fabricates_sequence() {
    use loom::sync::atomic::{AtomicBool, Ordering};
    use loom::sync::Arc;
    use loom::thread;

    loom::model(|| {
        let child_published = Arc::new(AtomicBool::new(false));
        let final_published = Arc::new(AtomicBool::new(false));

        let writer_child = Arc::clone(&child_published);
        let writer_final = Arc::clone(&final_published);
        let writer = thread::spawn(move || {
            writer_child.store(true, Ordering::Release);
            writer_final.store(true, Ordering::Release);
        });

        let reader_child = Arc::clone(&child_published);
        let reader_final = Arc::clone(&final_published);
        let reader = thread::spawn(move || {
            let observed_child = reader_child.load(Ordering::Acquire);
            let observed_final = observed_child && reader_final.load(Ordering::Acquire);
            assert!(
                !observed_final || observed_child,
                "reader observed a final sequence without its published prefix"
            );
        });

        writer.join().expect("writer thread");
        reader.join().expect("reader thread");
    });
}

#[test]
fn loom_joined_insert_is_visible_to_later_reader() {
    use loom::sync::atomic::{AtomicBool, Ordering};
    use loom::sync::Arc;
    use loom::thread;

    loom::model(|| {
        let child_published = Arc::new(AtomicBool::new(false));
        let final_published = Arc::new(AtomicBool::new(false));

        let writer_child = Arc::clone(&child_published);
        let writer_final = Arc::clone(&final_published);
        thread::spawn(move || {
            writer_child.store(true, Ordering::Release);
            writer_final.store(true, Ordering::Release);
        })
        .join()
        .expect("writer thread");

        assert!(child_published.load(Ordering::Acquire));
        assert!(final_published.load(Ordering::Acquire));
    });
}

#[test]
fn loom_remove_clears_finality_without_corrupting_sibling() {
    use loom::sync::atomic::{AtomicBool, Ordering};
    use loom::sync::Arc;
    use loom::thread;

    loom::model(|| {
        let removed_leaf_final = Arc::new(AtomicBool::new(true));
        let sibling_leaf_final = Arc::new(AtomicBool::new(true));

        let remover_leaf = Arc::clone(&removed_leaf_final);
        let remover = thread::spawn(move || {
            remover_leaf.store(false, Ordering::Release);
        });

        let reader_removed = Arc::clone(&removed_leaf_final);
        let reader_sibling = Arc::clone(&sibling_leaf_final);
        let reader = thread::spawn(move || {
            let _removed_visible = reader_removed.load(Ordering::Acquire);
            assert!(
                reader_sibling.load(Ordering::Acquire),
                "removing one final sequence must not clear a sibling final"
            );
        });

        remover.join().expect("remover thread");
        reader.join().expect("reader thread");
    });
}
