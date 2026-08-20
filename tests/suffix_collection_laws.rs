use std::fmt::Debug;

use libdictenstein::collection::{DictionaryEntries, DictionaryLanguageEntries};
use libdictenstein::scdawg::{Scdawg, ScdawgChar};
use libdictenstein::suffix_automaton::{SuffixAutomaton, SuffixAutomatonChar};
use libdictenstein::MutableMappedDictionary;

fn assert_byte_snapshot_law<D, F>(dictionary: &D, mutate: F)
where
    D: DictionaryEntries<Unit = u8, Value = u64>,
    D::Entries: ExactSizeIterator,
    F: FnOnce(),
{
    let mut snapshot = dictionary.entries();
    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot.size_hint(), (2, Some(2)));

    mutate();

    let entries: Vec<_> = snapshot
        .by_ref()
        .map(|entry| (String::from_utf8(entry.key).unwrap(), entry.value))
        .collect();
    assert_eq!(
        entries,
        vec![("alpha".to_string(), Some(7)), ("beta".to_string(), None)]
    );
    assert_eq!(snapshot.next(), None);
    assert_eq!(snapshot.next(), None);

    let current: Vec<_> = dictionary
        .entries()
        .map(|entry| (String::from_utf8(entry.key).unwrap(), entry.value))
        .collect();
    assert_eq!(
        current,
        vec![
            ("alpha".to_string(), Some(7)),
            ("beta".to_string(), None),
            ("gamma".to_string(), Some(11)),
        ]
    );
}

fn assert_char_snapshot_law<D, F>(dictionary: &D, mutate: F)
where
    D: DictionaryEntries<Unit = char, Value = u64>,
    D::Entries: ExactSizeIterator,
    F: FnOnce(),
{
    let mut snapshot = dictionary.entries();
    assert_eq!(snapshot.len(), 2);
    assert_eq!(snapshot.size_hint(), (2, Some(2)));

    mutate();

    let entries: Vec<_> = snapshot
        .by_ref()
        .map(|entry| (entry.key.into_iter().collect::<String>(), entry.value))
        .collect();
    assert_eq!(
        entries,
        vec![("alpha".to_string(), Some(7)), ("βeta".to_string(), None)]
    );
    assert_eq!(snapshot.next(), None);
    assert_eq!(snapshot.next(), None);

    let current: Vec<_> = dictionary
        .entries()
        .map(|entry| (entry.key.into_iter().collect::<String>(), entry.value))
        .collect();
    assert_eq!(
        current,
        vec![
            ("alpha".to_string(), Some(7)),
            ("βeta".to_string(), None),
            ("γamma".to_string(), Some(11)),
        ]
    );
}

fn assert_eq_debug<T: Eq + Debug>(left: T, right: T) {
    assert_eq!(left, right);
}

#[test]
fn in_memory_suffix_families_are_revision_pinned_lossless_and_sorted() {
    let automaton = SuffixAutomaton::<u64>::new();
    automaton.insert("beta");
    automaton.insert_with_value("alpha", 7);
    assert_byte_snapshot_law(&automaton, || {
        automaton.insert_with_value("gamma", 11);
    });

    let automaton_char = SuffixAutomatonChar::<u64>::new();
    automaton_char.insert("βeta");
    automaton_char.insert_with_value("alpha", 7);
    assert_char_snapshot_law(&automaton_char, || {
        automaton_char.insert_with_value("γamma", 11);
    });

    let scdawg = Scdawg::<u64>::new();
    scdawg.insert("beta");
    scdawg.insert_with_value("alpha", 7);
    assert_byte_snapshot_law(&scdawg, || {
        scdawg.insert_with_value("gamma", 11);
    });

    let scdawg_char = ScdawgChar::<u64>::new();
    scdawg_char.insert("βeta");
    scdawg_char.insert_with_value("alpha", 7);
    assert_char_snapshot_law(&scdawg_char, || {
        scdawg_char.insert_with_value("γamma", 11);
    });
}

#[test]
fn suffix_sources_preserve_duplicate_records_and_remove_one_at_a_time() {
    let byte = SuffixAutomaton::<u64>::new();
    byte.insert_with_value("duplicate", 5);
    byte.insert("duplicate");
    assert_eq!(byte.entries().len(), 2);
    assert_eq_debug(
        byte.entries()
            .map(|entry| (String::from_utf8(entry.key).unwrap(), entry.value))
            .collect::<Vec<_>>(),
        vec![
            ("duplicate".to_string(), Some(5)),
            ("duplicate".to_string(), None),
        ],
    );
    assert!(byte.remove("duplicate"));
    assert_eq!(byte.entries().len(), 1);
    assert_eq!(byte.entries().next().unwrap().value, None);

    let unicode = SuffixAutomatonChar::<u64>::new();
    unicode.insert_with_value("重複", 8);
    unicode.insert("重複");
    assert_eq!(unicode.entries().len(), 2);
    assert_eq!(
        unicode
            .entries()
            .map(|entry| entry.value)
            .collect::<Vec<_>>(),
        vec![Some(8), None]
    );
    assert!(unicode.remove("重複"));
    assert_eq!(unicode.entries().len(), 1);
    assert_eq!(unicode.entries().next().unwrap().value, None);
}

#[test]
fn stored_records_are_separate_from_the_recognized_substring_language() {
    let byte = SuffixAutomaton::<u64>::from_text("banana");
    let stored: Vec<_> = byte
        .entries()
        .map(|entry| String::from_utf8(entry.key).unwrap())
        .collect();
    let language: Vec<_> = byte
        .language_entries()
        .map(|entry| String::from_utf8(entry.key).unwrap())
        .collect();
    assert_eq!(stored, vec!["banana"]);
    assert!(language.iter().any(|term| term == "ana"));
    assert!(!stored.iter().any(|term| term == "ana"));

    let unicode = SuffixAutomatonChar::<u64>::from_text("東京カフェ");
    let stored: Vec<_> = unicode
        .entries()
        .map(|entry| entry.key.into_iter().collect::<String>())
        .collect();
    let language: Vec<_> = unicode
        .language_entries()
        .map(|entry| entry.key.into_iter().collect::<String>())
        .collect();
    assert_eq!(stored, vec!["東京カフェ"]);
    assert!(language.iter().any(|term| term == "京カ"));
    assert!(!stored.iter().any(|term| term == "京カ"));
}

#[cfg(feature = "persistent-artrie")]
#[test]
fn persistent_suffix_families_are_revision_pinned_lossless_and_sorted() {
    use libdictenstein::persistent_artrie::{
        PersistentScdawg, PersistentScdawgChar, PersistentSuffixAutomaton,
        PersistentSuffixAutomatonChar, PersistentSuffixTree, PersistentSuffixTreeChar,
    };

    let automaton = PersistentSuffixAutomaton::<u64>::new();
    automaton.insert("beta");
    automaton.insert_with_value("alpha", 7);
    assert_byte_snapshot_law(&automaton, || {
        automaton.insert_with_value("gamma", 11);
    });

    let automaton_char = PersistentSuffixAutomatonChar::<u64>::new();
    automaton_char.insert("βeta");
    automaton_char.insert_with_value("alpha", 7);
    assert_char_snapshot_law(&automaton_char, || {
        automaton_char.insert_with_value("γamma", 11);
    });

    let tree = PersistentSuffixTree::<u64>::new();
    tree.insert("beta");
    tree.insert_with_value("alpha", 7);
    assert_byte_snapshot_law(&tree, || {
        tree.insert_with_value("gamma", 11);
    });

    let tree_char = PersistentSuffixTreeChar::<u64>::new();
    tree_char.insert("βeta");
    tree_char.insert_with_value("alpha", 7);
    assert_char_snapshot_law(&tree_char, || {
        tree_char.insert_with_value("γamma", 11);
    });

    let scdawg = PersistentScdawg::<u64>::new();
    scdawg.insert("beta");
    scdawg.insert_with_value("alpha", 7);
    assert_byte_snapshot_law(&scdawg, || {
        scdawg.insert_with_value("gamma", 11);
    });

    let scdawg_char = PersistentScdawgChar::<u64>::new();
    scdawg_char.insert("βeta");
    scdawg_char.insert_with_value("alpha", 7);
    assert_char_snapshot_law(&scdawg_char, || {
        scdawg_char.insert_with_value("γamma", 11);
    });
}

#[cfg(feature = "persistent-artrie")]
#[test]
fn persistent_suffix_sources_preserve_duplicates_across_remove_and_compact() {
    use libdictenstein::persistent_artrie::{PersistentSuffixAutomaton, PersistentSuffixTree};

    let automaton = PersistentSuffixAutomaton::<u64>::new();
    automaton.insert_with_value("duplicate", 1);
    automaton.insert("duplicate");
    let old = automaton.entries();
    assert!(automaton.remove("duplicate"));
    automaton.compact();
    assert_eq!(
        old.map(|entry| entry.value).collect::<Vec<_>>(),
        vec![Some(1), None]
    );
    assert_eq!(automaton.entries().len(), 1);

    let tree = PersistentSuffixTree::<u64>::new();
    tree.insert_with_value("duplicate", 1);
    tree.insert("duplicate");
    let old = tree.entries();
    assert!(tree.remove("duplicate"));
    tree.compact();
    assert_eq!(
        old.map(|entry| entry.value).collect::<Vec<_>>(),
        vec![Some(1), None]
    );
    assert_eq!(tree.entries().len(), 1);
}
