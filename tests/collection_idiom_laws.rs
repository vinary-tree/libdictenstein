//! Reference laws and compile-time matrices for Rust collection idioms.

use libdictenstein::bijective::{BijectiveMap, InsertError};
use libdictenstein::difference_zipper::DifferenceZipperExt;
use libdictenstein::double_array_trie::zipper::DoubleArrayTrieZipper;
use libdictenstein::double_array_trie::{DoubleArrayTrie, DoubleArrayTrieChar};
use libdictenstein::dynamic_dawg::{DynamicDawg, DynamicDawgChar, DynamicDawgU64};
use libdictenstein::intersection_zipper::IntersectionZipperExt;
use libdictenstein::scdawg::{Scdawg, ScdawgChar};
use libdictenstein::suffix_automaton::{SuffixAutomaton, SuffixAutomatonChar};
use libdictenstein::symmetric_difference_zipper::SymmetricDifferenceZipperExt;
use libdictenstein::union_zipper::UnionZipperExt;
use libdictenstein::{
    DictionaryEntries, DictionaryEntry, DictionaryKeys, DictionaryTerms, DictionaryValues,
    ValuedZipperCollection, ZipperCollection,
};

fn assert_collection_views<D>()
where
    D: DictionaryEntries + DictionaryTerms + DictionaryKeys + DictionaryValues,
{
}

fn assert_from_iterator<T, Item>()
where
    T: FromIterator<Item>,
{
}

fn assert_extend<T, Item>()
where
    T: Extend<Item>,
{
}

#[test]
fn collection_view_trait_matrix_compiles() {
    assert_collection_views::<DynamicDawg<u32>>();
    assert_collection_views::<DynamicDawgChar<u32>>();
    assert_collection_views::<DynamicDawgU64<u32>>();
    assert_collection_views::<DoubleArrayTrie<u32>>();
    assert_collection_views::<DoubleArrayTrieChar<u32>>();
    assert_collection_views::<BijectiveMap<u32>>();
    assert_collection_views::<Scdawg<u32>>();
    assert_collection_views::<ScdawgChar<u32>>();
    assert_collection_views::<SuffixAutomaton<u32>>();
    assert_collection_views::<SuffixAutomatonChar<u32>>();

    #[cfg(feature = "pathmap-backend")]
    {
        use libdictenstein::pathmap::{
            PathMapDictionary, PathMapDictionaryChar, PathMapSnapshot, PathMapSnapshotChar,
        };
        assert_collection_views::<PathMapDictionary<u32>>();
        assert_collection_views::<PathMapDictionaryChar<u32>>();
        assert_collection_views::<PathMapSnapshot<u32>>();
        assert_collection_views::<PathMapSnapshotChar<u32>>();
    }

    #[cfg(feature = "persistent-artrie")]
    {
        use libdictenstein::persistent_artrie::char::PersistentARTrieChar;
        use libdictenstein::persistent_artrie::scdawg::{PersistentScdawg, PersistentScdawgChar};
        use libdictenstein::persistent_artrie::suffix_automaton::{
            PersistentSuffixAutomaton, PersistentSuffixAutomatonChar,
        };
        use libdictenstein::persistent_artrie::suffix_tree::{
            PersistentSuffixTree, PersistentSuffixTreeChar,
        };
        use libdictenstein::persistent_artrie::u64::PersistentARTrieU64;
        use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
        use libdictenstein::persistent_artrie::PersistentARTrie;

        assert_collection_views::<PersistentARTrie<u32>>();
        assert_collection_views::<PersistentARTrieChar<u32>>();
        assert_collection_views::<PersistentARTrieU64<u32>>();
        assert_collection_views::<PersistentVocabARTrie>();
        assert_collection_views::<PersistentSuffixAutomaton<u32>>();
        assert_collection_views::<PersistentSuffixAutomatonChar<u32>>();
        assert_collection_views::<PersistentSuffixTree<u32>>();
        assert_collection_views::<PersistentSuffixTreeChar<u32>>();
        assert_collection_views::<PersistentScdawg<u32>>();
        assert_collection_views::<PersistentScdawgChar<u32>>();
        assert_collection_views::<std::sync::Arc<PersistentARTrie<u32>>>();
    }
}

#[test]
fn fold_keys_and_values_are_lossless_and_early_error_is_honored() {
    let dictionary = DynamicDawg::<u32>::new();
    dictionary.insert_with_value("alpha", 1);
    dictionary.insert("beta");
    dictionary.insert_with_value("gamma", 3);

    let keys: Vec<_> = dictionary.keys().collect();
    assert_eq!(keys, dictionary.terms().collect::<Vec<_>>());
    assert_eq!(
        dictionary.values().collect::<Vec<_>>(),
        [Some(1), None, Some(3)]
    );

    let total_key_units = dictionary.fold_entries(0, |total, key, _| total + key.len());
    assert_eq!(
        total_key_units,
        "alpha".len() + "beta".len() + "gamma".len()
    );

    let mut visited = Vec::new();
    let result = dictionary.try_fold_entries(0, |count, key, value| {
        let key = String::from_utf8(key.to_vec()).unwrap();
        visited.push((key.clone(), value));
        if key == "beta" {
            Err("stop")
        } else {
            Ok(count + 1)
        }
    });
    assert_eq!(result, Err("stop"));
    assert_eq!(visited, [("alpha".into(), Some(1)), ("beta".into(), None)]);
}

#[test]
fn infallible_in_memory_construction_matrix_and_laws() {
    assert_from_iterator::<Scdawg<u32>, String>();
    assert_from_iterator::<Scdawg<u32>, &'static str>();
    assert_from_iterator::<Scdawg<u32>, (String, u32)>();
    assert_from_iterator::<Scdawg<u32>, (&'static str, u32)>();
    assert_from_iterator::<ScdawgChar<u32>, String>();
    assert_from_iterator::<ScdawgChar<u32>, &'static str>();
    assert_from_iterator::<ScdawgChar<u32>, (String, u32)>();
    assert_from_iterator::<ScdawgChar<u32>, (&'static str, u32)>();
    assert_extend::<Scdawg<u32>, String>();
    assert_extend::<Scdawg<u32>, &'static str>();
    assert_extend::<Scdawg<u32>, (String, u32)>();
    assert_extend::<Scdawg<u32>, (&'static str, u32)>();
    assert_extend::<ScdawgChar<u32>, String>();
    assert_extend::<ScdawgChar<u32>, &'static str>();
    assert_extend::<ScdawgChar<u32>, (String, u32)>();
    assert_extend::<ScdawgChar<u32>, (&'static str, u32)>();

    assert_from_iterator::<SuffixAutomaton<u32>, String>();
    assert_from_iterator::<SuffixAutomaton<u32>, &'static str>();
    assert_from_iterator::<SuffixAutomatonChar<u32>, String>();
    assert_from_iterator::<SuffixAutomatonChar<u32>, &'static str>();
    assert_from_iterator::<SuffixAutomaton<u32>, (String, u32)>();
    assert_from_iterator::<SuffixAutomaton<u32>, (&'static str, u32)>();
    assert_from_iterator::<SuffixAutomatonChar<u32>, (String, u32)>();
    assert_from_iterator::<SuffixAutomatonChar<u32>, (&'static str, u32)>();
    assert_extend::<SuffixAutomaton<u32>, String>();
    assert_extend::<SuffixAutomaton<u32>, &'static str>();
    assert_extend::<SuffixAutomaton<u32>, (String, u32)>();
    assert_extend::<SuffixAutomaton<u32>, (&'static str, u32)>();
    assert_extend::<SuffixAutomatonChar<u32>, String>();
    assert_extend::<SuffixAutomatonChar<u32>, &'static str>();
    assert_extend::<SuffixAutomatonChar<u32>, (String, u32)>();
    assert_extend::<SuffixAutomatonChar<u32>, (&'static str, u32)>();

    let mut scdawg: Scdawg<u32> = [("same", 1), ("other", 2), ("same", 3)]
        .into_iter()
        .collect();
    assert_eq!(scdawg.get_value("same"), Some(3));
    std::iter::Extend::extend(&mut scdawg, [("same", 4), ("new", 5)]);
    assert_eq!(scdawg.get_value("same"), Some(4));

    let suffix: SuffixAutomaton<u32> = [("same", 1), ("same", 2)].into_iter().collect();
    assert_eq!(
        suffix.entries().count(),
        2,
        "source records are sequence-like"
    );

    #[cfg(feature = "pathmap-backend")]
    {
        use libdictenstein::pathmap::{PathMapDictionary, PathMapDictionaryChar};
        assert_from_iterator::<PathMapDictionary<u32>, String>();
        assert_from_iterator::<PathMapDictionary<u32>, &'static str>();
        assert_from_iterator::<PathMapDictionary<u32>, Vec<u8>>();
        assert_from_iterator::<PathMapDictionary<u32>, &'static [u8]>();
        assert_from_iterator::<PathMapDictionary<u32>, (String, u32)>();
        assert_from_iterator::<PathMapDictionary<u32>, (&'static str, u32)>();
        assert_from_iterator::<PathMapDictionary<u32>, (Vec<u8>, u32)>();
        assert_from_iterator::<PathMapDictionary<u32>, (&'static [u8], u32)>();
        assert_from_iterator::<PathMapDictionaryChar<u32>, String>();
        assert_from_iterator::<PathMapDictionaryChar<u32>, &'static str>();
        assert_from_iterator::<PathMapDictionaryChar<u32>, Vec<char>>();
        assert_from_iterator::<PathMapDictionaryChar<u32>, &'static [char]>();
        assert_from_iterator::<PathMapDictionaryChar<u32>, (String, u32)>();
        assert_from_iterator::<PathMapDictionaryChar<u32>, (&'static str, u32)>();
        assert_from_iterator::<PathMapDictionaryChar<u32>, (Vec<char>, u32)>();
        assert_from_iterator::<PathMapDictionaryChar<u32>, (&'static [char], u32)>();
        assert_extend::<PathMapDictionary<u32>, String>();
        assert_extend::<PathMapDictionary<u32>, &'static str>();
        assert_extend::<PathMapDictionary<u32>, Vec<u8>>();
        assert_extend::<PathMapDictionary<u32>, &'static [u8]>();
        assert_extend::<PathMapDictionary<u32>, (String, u32)>();
        assert_extend::<PathMapDictionary<u32>, (&'static str, u32)>();
        assert_extend::<PathMapDictionary<u32>, (Vec<u8>, u32)>();
        assert_extend::<PathMapDictionary<u32>, (&'static [u8], u32)>();
        assert_extend::<PathMapDictionaryChar<u32>, String>();
        assert_extend::<PathMapDictionaryChar<u32>, &'static str>();
        assert_extend::<PathMapDictionaryChar<u32>, Vec<char>>();
        assert_extend::<PathMapDictionaryChar<u32>, &'static [char]>();
        assert_extend::<PathMapDictionaryChar<u32>, (String, u32)>();
        assert_extend::<PathMapDictionaryChar<u32>, (&'static str, u32)>();
        assert_extend::<PathMapDictionaryChar<u32>, (Vec<char>, u32)>();
        assert_extend::<PathMapDictionaryChar<u32>, (&'static [char], u32)>();

        let mut map: PathMapDictionary<u32> = [(vec![0, 0xff], 1), (vec![0, 0xff], 2)]
            .into_iter()
            .collect();
        assert_eq!(
            map.entries()
                .find(|entry| entry.key == [0, 0xff])
                .and_then(|entry| entry.value),
            Some(2)
        );
        std::iter::Extend::extend(&mut map, [(vec![0, 0xff], 3), (vec![7], 4)]);
        assert_eq!(
            map.entries()
                .find(|entry| entry.key == [0, 0xff])
                .and_then(|entry| entry.value),
            Some(3)
        );
    }
}

#[test]
fn lazy_zipper_collection_matches_reference_boolean_algebra() {
    let left =
        DoubleArrayTrie::<u32>::from_terms_with_values([("alpha", 1), ("common", 2), ("left", 3)]);
    let right =
        DoubleArrayTrie::<u32>::from_terms_with_values([("beta", 4), ("common", 5), ("right", 6)]);
    let zipper = || DoubleArrayTrieZipper::new_from_dict(&left);
    let other = || DoubleArrayTrieZipper::new_from_dict(&right);
    let strings = |terms: Vec<Vec<u8>>| {
        terms
            .into_iter()
            .map(|term| String::from_utf8(term).unwrap())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        strings(zipper().union_with(other()).terms().collect()),
        ["alpha", "beta", "common", "left", "right"]
    );
    assert_eq!(
        strings(zipper().intersection_with(other()).keys().collect()),
        ["common"]
    );
    assert_eq!(
        strings(zipper().difference_from(other()).terms().collect()),
        ["alpha", "left"]
    );
    assert_eq!(
        strings(
            zipper()
                .symmetric_difference_with(other())
                .terms()
                .collect(),
        ),
        ["alpha", "beta", "left", "right"]
    );

    let term_only = DynamicDawg::<u32>::new();
    term_only.insert("plain");
    term_only.insert_with_value("valued", 9);
    let empty = DynamicDawg::<u32>::new();
    let entries: Vec<_> =
        libdictenstein::dynamic_dawg::zipper::DynamicDawgZipper::new_from_dict(&term_only)
            .difference_from(
                libdictenstein::dynamic_dawg::zipper::DynamicDawgZipper::new_from_dict(&empty),
            )
            .entries()
            .map(DictionaryEntry::into_pair)
            .collect();
    assert_eq!(
        entries,
        [(b"plain".to_vec(), None), (b"valued".to_vec(), Some(9))]
    );
}

#[cfg(feature = "pathmap-backend")]
#[test]
fn public_snapshots_are_consuming_collections_of_the_captured_revision() {
    use libdictenstein::pathmap::PathMapDictionary;

    let dictionary: PathMapDictionary<u32> = [("alpha", 1), ("beta", 2)].into_iter().collect();
    let snapshot = dictionary.snapshot();
    dictionary.insert_with_value("gamma", 3);
    let got: Vec<_> = snapshot
        .into_iter()
        .map(|entry| (String::from_utf8(entry.key).unwrap(), entry.value))
        .collect();
    assert_eq!(got, [("alpha".into(), Some(1)), ("beta".into(), Some(2))]);
}

#[test]
fn bijective_named_bulk_api_reports_errors_and_prefix_commit() {
    let map = BijectiveMap::try_from_iter([("alpha", 1), ("beta", 2)]).unwrap();
    assert_eq!(map.get_term(&2), Some("beta".into()));
    let error = map.try_extend([("gamma", 3), ("duplicate-value", 2)]);
    assert_eq!(error, Err(InsertError::DuplicateValue));
    assert_eq!(map.get_value("gamma"), Some(3));
}

#[cfg(feature = "persistent-artrie")]
#[test]
fn persistent_named_fallible_collection_matrix_and_laws() {
    use libdictenstein::persistent_artrie::char::PersistentARTrieChar;
    use libdictenstein::persistent_artrie::scdawg::{PersistentScdawg, PersistentScdawgChar};
    use libdictenstein::persistent_artrie::suffix_automaton::{
        PersistentSuffixAutomaton, PersistentSuffixAutomatonChar,
    };
    use libdictenstein::persistent_artrie::suffix_tree::{
        PersistentSuffixTree, PersistentSuffixTreeChar,
    };
    use libdictenstein::persistent_artrie::u64::{EncodedPersistentARTrieU64, PersistentARTrieU64};
    use libdictenstein::persistent_artrie::vocab::PersistentVocabARTrie;
    use libdictenstein::persistent_artrie::PersistentARTrie;

    macro_rules! assert_string_backend {
        ($dictionary:ty, $applied:expr) => {{
            let dictionary = <$dictionary>::try_from_iter_sorted(["beta", "alpha"]).unwrap();
            assert_eq!(dictionary.entries().count(), 2);
            assert_eq!(
                dictionary
                    .try_extend_entries_sorted([("alpha", 7), ("gamma", 9)])
                    .unwrap(),
                $applied
            );
        }};
    }

    assert_string_backend!(PersistentARTrie<u32>, 1);
    assert_string_backend!(PersistentARTrieChar<u32>, 1);
    assert_string_backend!(PersistentSuffixAutomaton<u32>, 2);
    assert_string_backend!(PersistentSuffixAutomatonChar<u32>, 2);
    assert_string_backend!(PersistentSuffixTree<u32>, 2);
    assert_string_backend!(PersistentSuffixTreeChar<u32>, 2);
    assert_string_backend!(PersistentScdawg<u32>, 1);
    assert_string_backend!(PersistentScdawgChar<u32>, 1);

    let sequences =
        PersistentARTrieU64::<u32>::try_from_entries_sorted([(vec![7], 1), (vec![0, u64::MAX], 2)])
            .unwrap();
    assert_eq!(sequences.entries().count(), 2);

    let encoded = EncodedPersistentARTrieU64::<u32>::try_from_entries_sorted([
        (vec![7], 1),
        (vec![0, u64::MAX], 2),
    ])
    .unwrap();
    assert!(encoded.contains_sequence(&[0, u64::MAX]));

    let directory = tempfile::tempdir().unwrap();
    let vocabulary = PersistentVocabARTrie::try_from_entries_sorted(
        directory.path().join("collection.vocab"),
        [("beta", 8), ("alpha", 7)],
    )
    .unwrap();
    assert_eq!(vocabulary.get_index("alpha"), Some(7));
}
