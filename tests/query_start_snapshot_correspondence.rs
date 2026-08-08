//! Query-start snapshot correspondence for long-lived mutable-dictionary iterators.

use libdictenstein::dynamic_dawg::char::DynamicDawgChar;
use libdictenstein::dynamic_dawg::u64::DynamicDawgU64;
use libdictenstein::dynamic_dawg::DynamicDawg;
use proptest::prelude::*;
use std::collections::BTreeMap;

fn collect_map(iter: impl Iterator<Item = (String, u64)>) -> BTreeMap<String, u64> {
    iter.collect()
}

fn collect_byte_map(iter: impl Iterator<Item = (Vec<u8>, u64)>) -> BTreeMap<Vec<u8>, u64> {
    iter.collect()
}

fn collect_u64_map(iter: impl Iterator<Item = (Vec<u64>, u64)>) -> BTreeMap<Vec<u64>, u64> {
    iter.collect()
}

#[test]
fn long_lived_iterator_has_query_start_snapshot_semantics() {
    let dict = DynamicDawgChar::<u64>::from_terms_with_values([
        ("cat", 1),
        ("cot", 2),
        ("cut", 3),
        ("scat", 4),
    ]);
    let frozen = collect_map(dict.iter());

    let mut cursor = dict.iter();
    let first = cursor.next().expect("initial dictionary is non-empty");

    assert!(dict.remove("cot"));
    assert!(dict.insert_with_value("cit", 5));
    assert!(!dict.insert_with_value("cut", 30));
    dict.compact();

    let observed = collect_map(std::iter::once(first).chain(cursor));
    assert_eq!(observed, frozen, "the original iterator changed revision");

    assert_eq!(
        collect_map(dict.iter()),
        BTreeMap::from([
            ("cat".to_owned(), 1),
            ("cit".to_owned(), 5),
            ("cut".to_owned(), 30),
            ("scat".to_owned(), 4),
        ]),
        "a fresh iterator did not observe the published revision"
    );
}

#[test]
fn a_writer_completes_while_a_snapshot_iterator_is_held() {
    let dict =
        DynamicDawgChar::<u64>::from_terms_with_values([("alpha", 1), ("beta", 2), ("gamma", 3)]);
    let frozen = collect_map(dict.iter());
    let mut cursor = dict.iter();
    let first = cursor.next().expect("initial dictionary is non-empty");

    let writer_dict = dict.clone();
    let writer = std::thread::spawn(move || {
        writer_dict.remove("beta");
        writer_dict.insert_with_value("delta", 4);
        writer_dict.compact();
    });
    writer.join().expect("writer must not block on the cursor");

    assert_eq!(collect_map(std::iter::once(first).chain(cursor)), frozen);
    assert!(dict.contains("delta"));
    assert!(!dict.contains("beta"));
}

#[test]
fn raw_byte_iterator_retains_non_utf8_query_start_revision() {
    let dict = DynamicDawg::<u64>::new();
    for (term, value) in [
        (vec![0x00, 0x80], 1),
        (vec![0xff, 0x10], 2),
        (vec![0x7f, 0x00, 0xfe], 3),
    ] {
        assert!(dict.insert_bytes_with_value(&term, value));
    }
    let frozen = collect_byte_map(dict.iter_bytes_with_values());
    let mut cursor = dict.iter_bytes_with_values();
    let first = cursor.next().expect("initial byte dictionary is non-empty");

    assert!(dict.remove_bytes(&[0xff, 0x10]));
    assert!(dict.insert_bytes_with_value(&[0x81, 0x00], 4));
    assert!(!dict.insert_bytes_with_value(&[0x00, 0x80], 100));
    dict.compact();

    assert_eq!(
        collect_byte_map(std::iter::once(first).chain(cursor)),
        frozen
    );
    assert_ne!(collect_byte_map(dict.iter_bytes_with_values()), frozen);
}

#[test]
fn u64_iterator_retains_query_start_revision_and_values() {
    let dict = DynamicDawgU64::<u64>::new();
    for (term, value) in [
        (vec![10, 20], 1),
        (vec![10, 30], 2),
        (vec![u64::MAX, 0, 7], 3),
    ] {
        assert!(dict.insert_sequence_with_value(&term, value));
    }
    let frozen = collect_u64_map(dict.iter_with_values());
    let mut cursor = dict.iter_with_values();
    let first = cursor.next().expect("initial u64 dictionary is non-empty");

    assert!(dict.remove_sequence(&[10, 30]));
    assert!(dict.insert_sequence_with_value(&[11, 30], 4));
    assert!(!dict.insert_sequence_with_value(&[10, 20], 100));
    dict.compact();

    assert_eq!(
        collect_u64_map(std::iter::once(first).chain(cursor)),
        frozen
    );
    assert_ne!(collect_u64_map(dict.iter_with_values()), frozen);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(192))]

    #[test]
    fn arbitrary_mutations_do_not_change_one_long_lived_iterator(
        initial in prop::collection::vec(("[a-z]{1,10}", any::<u64>()), 2..48),
        mutations in prop::collection::vec((0u8..3, "[a-z]{1,10}", any::<u64>()), 0..48),
        prefix_seed in any::<usize>(),
    ) {
        let frozen: BTreeMap<String, u64> = initial.into_iter().collect();
        prop_assume!(frozen.len() >= 2);

        let dict = DynamicDawgChar::<u64>::from_terms_with_values(
            frozen.iter().map(|(term, value)| (term.as_str(), *value)),
        );
        let mut final_model = frozen.clone();
        let mut cursor = dict.iter();
        let prefix_len = 1 + prefix_seed % (frozen.len() - 1);
        let prefix: Vec<_> = cursor.by_ref().take(prefix_len).collect();
        prop_assert_eq!(prefix.len(), prefix_len);

        for (operation, term, value) in mutations {
            match operation {
                0 => {
                    dict.insert_with_value(&term, value);
                    final_model.insert(term, value);
                }
                1 => {
                    dict.remove(&term);
                    final_model.remove(&term);
                }
                _ => {
                    dict.insert_with_value(&term, value);
                    final_model.insert(term, value);
                }
            }
        }
        dict.compact();

        let observed = collect_map(prefix.into_iter().chain(cursor));
        prop_assert_eq!(observed, frozen);
        prop_assert_eq!(collect_map(dict.iter()), final_model);
    }

    #[test]
    fn arbitrary_raw_byte_mutations_do_not_change_one_long_lived_iterator(
        initial in prop::collection::vec(
            (prop::collection::vec(any::<u8>(), 0..12), any::<u64>()),
            2..48,
        ),
        mutations in prop::collection::vec(
            (0u8..3, prop::collection::vec(any::<u8>(), 0..12), any::<u64>()),
            0..48,
        ),
        prefix_seed in any::<usize>(),
    ) {
        let frozen: BTreeMap<Vec<u8>, u64> = initial.into_iter().collect();
        prop_assume!(frozen.len() >= 2);

        let dict = DynamicDawg::<u64>::new();
        for (term, value) in &frozen {
            dict.insert_bytes_with_value(term, *value);
        }
        let mut final_model = frozen.clone();
        let mut cursor = dict.iter_bytes_with_values();
        let prefix_len = 1 + prefix_seed % (frozen.len() - 1);
        let prefix: Vec<_> = cursor.by_ref().take(prefix_len).collect();
        prop_assert_eq!(prefix.len(), prefix_len);

        for (operation, term, value) in mutations {
            match operation {
                0 | 2 => {
                    dict.insert_bytes_with_value(&term, value);
                    final_model.insert(term, value);
                }
                _ => {
                    dict.remove_bytes(&term);
                    final_model.remove(&term);
                }
            }
        }
        dict.compact();

        prop_assert_eq!(
            collect_byte_map(prefix.into_iter().chain(cursor)),
            frozen,
        );
        prop_assert_eq!(collect_byte_map(dict.iter_bytes_with_values()), final_model);
    }

    #[test]
    fn arbitrary_u64_mutations_do_not_change_one_long_lived_iterator(
        initial in prop::collection::vec(
            (prop::collection::vec(any::<u64>(), 0..10), any::<u64>()),
            2..40,
        ),
        mutations in prop::collection::vec(
            (0u8..3, prop::collection::vec(any::<u64>(), 0..10), any::<u64>()),
            0..40,
        ),
        prefix_seed in any::<usize>(),
    ) {
        let frozen: BTreeMap<Vec<u64>, u64> = initial.into_iter().collect();
        prop_assume!(frozen.len() >= 2);

        let dict = DynamicDawgU64::<u64>::new();
        for (term, value) in &frozen {
            dict.insert_sequence_with_value(term, *value);
        }
        let mut final_model = frozen.clone();
        let mut cursor = dict.iter_with_values();
        let prefix_len = 1 + prefix_seed % (frozen.len() - 1);
        let prefix: Vec<_> = cursor.by_ref().take(prefix_len).collect();
        prop_assert_eq!(prefix.len(), prefix_len);

        for (operation, term, value) in mutations {
            match operation {
                0 | 2 => {
                    dict.insert_sequence_with_value(&term, value);
                    final_model.insert(term, value);
                }
                _ => {
                    dict.remove_sequence(&term);
                    final_model.remove(&term);
                }
            }
        }
        dict.compact();

        prop_assert_eq!(
            collect_u64_map(prefix.into_iter().chain(cursor)),
            frozen,
        );
        prop_assert_eq!(collect_u64_map(dict.iter_with_values()), final_model);
    }
}
