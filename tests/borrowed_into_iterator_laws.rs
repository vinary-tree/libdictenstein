use std::iter::FusedIterator;

use libdictenstein::collection::DictionaryEntry;
use libdictenstein::double_array_trie::{DoubleArrayTrie, DoubleArrayTrieChar};
use libdictenstein::dynamic_dawg::{DynamicDawg, DynamicDawgChar, DynamicDawgU64};
use libdictenstein::scdawg::{Scdawg, ScdawgChar};
use libdictenstein::suffix_automaton::{SuffixAutomaton, SuffixAutomatonChar};
use libdictenstein::MutableMappedDictionary;

fn assert_fused<I: FusedIterator>(_: &I) {}

macro_rules! assert_exact_byte_snapshot {
    ($dictionary:expr, $mutate:expr) => {{
        let dictionary = &$dictionary;
        let mut snapshot = dictionary.into_iter();
        assert_fused(&snapshot);
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot.size_hint(), (2, Some(2)));
        ($mutate)();

        let entries: Vec<_> = snapshot
            .by_ref()
            .map(|entry| (String::from_utf8(entry.key).unwrap(), entry.value))
            .collect();
        assert_eq!(
            entries,
            vec![("alpha".to_string(), None), ("beta".to_string(), Some(7))]
        );
        assert_eq!(snapshot.next(), None);
        assert_eq!(snapshot.next(), None);

        assert_eq!(dictionary.into_iter().len(), 3);
    }};
}

macro_rules! assert_exact_char_snapshot {
    ($dictionary:expr, $mutate:expr) => {{
        let dictionary = &$dictionary;
        let mut snapshot = dictionary.into_iter();
        assert_fused(&snapshot);
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot.size_hint(), (2, Some(2)));
        ($mutate)();

        let entries: Vec<_> = snapshot
            .by_ref()
            .map(|entry| (entry.key.into_iter().collect::<String>(), entry.value))
            .collect();
        assert_eq!(
            entries,
            vec![("alpha".to_string(), None), ("βeta".to_string(), Some(7))]
        );
        assert_eq!(snapshot.next(), None);
        assert_eq!(snapshot.next(), None);

        assert_eq!(dictionary.into_iter().len(), 3);
    }};
}

macro_rules! assert_exact_u64_snapshot {
    ($dictionary:expr, $mutate:expr) => {{
        let dictionary = &$dictionary;
        let mut snapshot = dictionary.into_iter();
        assert_fused(&snapshot);
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot.size_hint(), (2, Some(2)));
        ($mutate)();

        let entries: Vec<_> = snapshot.by_ref().map(DictionaryEntry::into_pair).collect();
        assert_eq!(entries, vec![(vec![1, 2], None), (vec![2, 3], Some(7))]);
        assert_eq!(snapshot.next(), None);
        assert_eq!(snapshot.next(), None);

        assert_eq!(dictionary.into_iter().len(), 3);
    }};
}

#[test]
fn dynamic_dawgs_borrow_into_lossless_revision_snapshots() {
    let byte = DynamicDawg::<u64>::new();
    byte.insert("alpha");
    byte.insert_with_value("beta", 7);
    assert_exact_byte_snapshot!(byte, || {
        byte.insert_with_value("gamma", 11);
    });

    let unicode = DynamicDawgChar::<u64>::new();
    unicode.insert("alpha");
    unicode.insert_with_value("βeta", 7);
    assert_exact_char_snapshot!(unicode, || {
        unicode.insert_with_value("γamma", 11);
    });

    let sequences = DynamicDawgU64::<u64>::new();
    sequences.insert_sequence(&[1, 2]);
    sequences.insert_sequence_with_value(&[2, 3], 7);
    assert_exact_u64_snapshot!(sequences, || {
        sequences.insert_sequence_with_value(&[3, 4], 11);
    });
}

#[test]
fn suffix_automata_borrow_into_stored_records_not_substrings() {
    let byte = SuffixAutomaton::<u64>::new();
    byte.insert("alpha");
    byte.insert_with_value("beta", 7);
    assert_exact_byte_snapshot!(byte, || {
        byte.insert_with_value("gamma", 11);
    });

    let unicode = SuffixAutomatonChar::<u64>::new();
    unicode.insert("alpha");
    unicode.insert_with_value("βeta", 7);
    assert_exact_char_snapshot!(unicode, || {
        unicode.insert_with_value("γamma", 11);
    });
}

#[test]
fn scdawgs_borrow_into_exact_terms() {
    let byte = Scdawg::<u64>::new();
    byte.insert("alpha");
    byte.insert_with_value("beta", 7);
    assert_exact_byte_snapshot!(byte, || {
        byte.insert_with_value("gamma", 11);
    });

    let unicode = ScdawgChar::<u64>::new();
    unicode.insert("alpha");
    unicode.insert_with_value("βeta", 7);
    assert_exact_char_snapshot!(unicode, || {
        unicode.insert_with_value("γamma", 11);
    });
}

#[test]
fn double_array_tries_have_exact_lossless_borrowed_iteration() {
    let byte_terms: DoubleArrayTrie<u64> = ["alpha", "beta"].into_iter().collect();
    let byte_entries: Vec<_> = (&byte_terms)
        .into_iter()
        .map(DictionaryEntry::into_pair)
        .collect();
    assert_eq!(
        byte_entries,
        vec![(b"alpha".to_vec(), None), (b"beta".to_vec(), None)]
    );

    let byte_mapped = DoubleArrayTrie::from_terms_with_values([("alpha", 3u64), ("beta", 7)]);
    assert_eq!((&byte_mapped).into_iter().len(), 2);
    assert!((&byte_mapped)
        .into_iter()
        .all(|entry| entry.value.is_some()));

    let char_terms: DoubleArrayTrieChar<u64> = ["alpha", "βeta"].into_iter().collect();
    assert_eq!((&char_terms).into_iter().len(), 2);
    assert!((&char_terms).into_iter().all(|entry| entry.value.is_none()));
}

#[test]
fn bijective_map_uses_the_common_entry_model() {
    let dictionary = libdictenstein::BijectiveMap::<u64>::new();
    dictionary.insert("alpha", 3);
    dictionary.insert("beta", 7);
    let mut snapshot = (&dictionary).into_iter();
    assert_fused(&snapshot);
    dictionary.insert("gamma", 11);
    let entries: Vec<_> = snapshot.by_ref().collect();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry.value.is_some()));
    assert_eq!(snapshot.next(), None);
    assert_eq!((&dictionary).into_iter().len(), 3);
}

#[cfg(feature = "pathmap-backend")]
#[test]
fn pathmap_dictionary_snapshot_and_ref_share_the_entry_model() {
    use libdictenstein::pathmap::{
        PathMapDictionary, PathMapDictionaryChar, PathMapRef, PathMapRefChar, PathMapSnapshot,
        PathMapSnapshotChar,
    };
    use pathmap::PathMap;

    let byte = PathMapDictionary::<u64>::new();
    byte.insert_with_value("alpha", 3);
    byte.insert_with_value("beta", 7);
    let mut old = (&byte).into_iter();
    assert_eq!(old.len(), 2);
    byte.insert_with_value("gamma", 11);
    assert_eq!(old.by_ref().count(), 2);
    assert_eq!(old.next(), None);
    assert_eq!((&byte).into_iter().len(), 3);

    let unicode = PathMapDictionaryChar::<u64>::new();
    unicode.insert_with_value("alpha", 3);
    unicode.insert_with_value("βeta", 7);
    let mut old = (&unicode).into_iter();
    unicode.insert_with_value("γamma", 11);
    assert_eq!(old.by_ref().count(), 2);
    assert_eq!(old.next(), None);
    assert_eq!((&unicode).into_iter().len(), 3);

    let mut map = PathMap::new();
    map.insert(b"alpha", 3u64);
    map.insert(b"beta", 7u64);

    let snapshot = PathMapSnapshot::from_map_ref(&map).with_len(2);
    let snapshot_char = PathMapSnapshotChar::from_map_ref(&map).with_len(2);
    let borrowed = PathMapRef::from_map(&map).with_len(2);
    let borrowed_char = PathMapRefChar::from_map(&map).with_len(2);
    for size_hint in [
        (&snapshot).into_iter().size_hint(),
        (&snapshot_char).into_iter().size_hint(),
        (&borrowed).into_iter().size_hint(),
        (&borrowed_char).into_iter().size_hint(),
    ] {
        assert_eq!(size_hint, (2, Some(2)));
    }
    assert_eq!((&snapshot).into_iter().count(), 2);
    assert_eq!((&snapshot_char).into_iter().count(), 2);
    assert_eq!((&borrowed).into_iter().count(), 2);
    assert_eq!((&borrowed_char).into_iter().count(), 2);
}

#[cfg(feature = "persistent-artrie")]
#[test]
fn persistent_prefix_tries_borrow_into_lossless_revision_snapshots() {
    use libdictenstein::persistent_artrie::{PersistentARTrie, PersistentARTrieU64};
    use libdictenstein::PersistentARTrieChar;

    let byte = PersistentARTrie::<u64>::default();
    byte.insert("alpha");
    byte.insert_with_value("beta", 7);
    assert_exact_byte_snapshot!(byte, || {
        byte.insert_with_value("gamma", 11);
    });

    let unicode = PersistentARTrieChar::<u64>::new();
    unicode.insert("alpha").unwrap();
    unicode.insert_with_value("βeta", 7).unwrap();
    assert_exact_char_snapshot!(unicode, || {
        unicode.insert_with_value("γamma", 11).unwrap();
    });

    let sequences = PersistentARTrieU64::<u64>::new();
    sequences.insert_sequence(&[1, 2]);
    sequences.insert_sequence_with_value(&[2, 3], 7);
    assert_exact_u64_snapshot!(sequences, || {
        sequences.insert_sequence_with_value(&[3, 4], 11);
    });
}

#[cfg(feature = "persistent-artrie")]
#[test]
fn persistent_vocab_borrow_into_exact_indexed_snapshot() {
    use libdictenstein::PersistentVocabARTrie;

    let directory = tempfile::tempdir().unwrap();
    let vocab = PersistentVocabARTrie::create(directory.path().join("borrowed.vocab")).unwrap();
    assert_eq!(vocab.insert("alpha").unwrap(), 0);
    assert_eq!(vocab.insert("βeta").unwrap(), 1);

    let mut snapshot = (&vocab).into_iter();
    assert_fused(&snapshot);
    assert_eq!(snapshot.len(), 2);
    assert_eq!(vocab.insert("γamma").unwrap(), 2);

    let entries: Vec<_> = snapshot
        .by_ref()
        .map(|entry| (entry.key.into_iter().collect::<String>(), entry.value))
        .collect();
    assert_eq!(
        entries,
        vec![
            ("alpha".to_string(), Some(0)),
            ("βeta".to_string(), Some(1))
        ]
    );
    assert_eq!(snapshot.next(), None);
    assert_eq!((&vocab).into_iter().len(), 3);
}

#[cfg(feature = "persistent-artrie")]
#[test]
fn persistent_suffix_families_borrow_into_stored_records() {
    use libdictenstein::persistent_artrie::{
        PersistentScdawg, PersistentScdawgChar, PersistentSuffixAutomaton,
        PersistentSuffixAutomatonChar, PersistentSuffixTree, PersistentSuffixTreeChar,
    };

    let automaton = PersistentSuffixAutomaton::<u64>::new();
    automaton.insert("alpha");
    automaton.insert_with_value("beta", 7);
    assert_exact_byte_snapshot!(automaton, || {
        automaton.insert_with_value("gamma", 11);
    });

    let automaton_char = PersistentSuffixAutomatonChar::<u64>::new();
    automaton_char.insert("alpha");
    automaton_char.insert_with_value("βeta", 7);
    assert_exact_char_snapshot!(automaton_char, || {
        automaton_char.insert_with_value("γamma", 11);
    });

    let tree = PersistentSuffixTree::<u64>::new();
    tree.insert("alpha");
    tree.insert_with_value("beta", 7);
    assert_exact_byte_snapshot!(tree, || {
        tree.insert_with_value("gamma", 11);
    });

    let tree_char = PersistentSuffixTreeChar::<u64>::new();
    tree_char.insert("alpha");
    tree_char.insert_with_value("βeta", 7);
    assert_exact_char_snapshot!(tree_char, || {
        tree_char.insert_with_value("γamma", 11);
    });

    let scdawg = PersistentScdawg::<u64>::new();
    scdawg.insert("alpha");
    scdawg.insert_with_value("beta", 7);
    assert_exact_byte_snapshot!(scdawg, || {
        scdawg.insert_with_value("gamma", 11);
    });

    let scdawg_char = PersistentScdawgChar::<u64>::new();
    scdawg_char.insert("alpha");
    scdawg_char.insert_with_value("βeta", 7);
    assert_exact_char_snapshot!(scdawg_char, || {
        scdawg_char.insert_with_value("γamma", 11);
    });
}
