//! Laws for the standard in-memory collection construction and mutation traits.

use libdictenstein::double_array_trie::{DoubleArrayTrie, DoubleArrayTrieChar};
use libdictenstein::dynamic_dawg::{DynamicDawg, DynamicDawgChar, DynamicDawgU64};
use libdictenstein::{
    Dictionary, DictionaryNode, MappedDictionary, MappedDictionaryNode, MutableDictionary,
};

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

fn contains_units<D>(dictionary: &D, key: &[<D::Node as DictionaryNode>::Unit]) -> bool
where
    D: Dictionary,
{
    let mut node = dictionary.root();
    for &unit in key {
        let Some(next) = node.transition(unit) else {
            return false;
        };
        node = next;
    }
    node.is_final()
}

fn value_at_units<D>(dictionary: &D, key: &[<D::Node as DictionaryNode>::Unit]) -> Option<D::Value>
where
    D: MappedDictionary,
    D::Node: MappedDictionaryNode<Value = D::Value>,
{
    let mut node = dictionary.root();
    for &unit in key {
        node = node.transition(unit)?;
    }
    node.is_final().then(|| node.value()).flatten()
}

#[test]
fn standard_trait_matrix_compiles_for_owned_and_borrowed_items() {
    assert_from_iterator::<DynamicDawg<()>, String>();
    assert_from_iterator::<DynamicDawg<()>, &'static str>();
    assert_from_iterator::<DynamicDawg<()>, Vec<u8>>();
    assert_from_iterator::<DynamicDawg<()>, &'static [u8]>();
    assert_from_iterator::<DynamicDawg<u32>, (String, u32)>();
    assert_from_iterator::<DynamicDawg<u32>, (&'static str, u32)>();
    assert_from_iterator::<DynamicDawg<u32>, (Vec<u8>, u32)>();
    assert_from_iterator::<DynamicDawg<u32>, (&'static [u8], u32)>();
    assert_from_iterator::<DynamicDawg<u32>, Vec<u8>>();

    assert_from_iterator::<DynamicDawgChar<()>, String>();
    assert_from_iterator::<DynamicDawgChar<()>, &'static str>();
    assert_from_iterator::<DynamicDawgChar<()>, Vec<char>>();
    assert_from_iterator::<DynamicDawgChar<()>, &'static [char]>();
    assert_from_iterator::<DynamicDawgChar<u32>, (String, u32)>();
    assert_from_iterator::<DynamicDawgChar<u32>, (&'static str, u32)>();
    assert_from_iterator::<DynamicDawgChar<u32>, (Vec<char>, u32)>();
    assert_from_iterator::<DynamicDawgChar<u32>, (&'static [char], u32)>();
    assert_from_iterator::<DynamicDawgChar<u32>, Vec<char>>();

    assert_from_iterator::<DynamicDawgU64<()>, String>();
    assert_from_iterator::<DynamicDawgU64<()>, &'static str>();
    assert_from_iterator::<DynamicDawgU64<()>, Vec<u64>>();
    assert_from_iterator::<DynamicDawgU64<()>, &'static [u64]>();
    assert_from_iterator::<DynamicDawgU64<u32>, (String, u32)>();
    assert_from_iterator::<DynamicDawgU64<u32>, (&'static str, u32)>();
    assert_from_iterator::<DynamicDawgU64<u32>, (Vec<u64>, u32)>();
    assert_from_iterator::<DynamicDawgU64<u32>, (&'static [u64], u32)>();
    assert_from_iterator::<DynamicDawgU64<u32>, Vec<u64>>();

    assert_from_iterator::<DoubleArrayTrie<()>, String>();
    assert_from_iterator::<DoubleArrayTrie<()>, &'static str>();
    assert_from_iterator::<DoubleArrayTrie<()>, Vec<u8>>();
    assert_from_iterator::<DoubleArrayTrie<()>, &'static [u8]>();
    assert_from_iterator::<DoubleArrayTrie<u32>, (String, u32)>();
    assert_from_iterator::<DoubleArrayTrie<u32>, (&'static str, u32)>();
    assert_from_iterator::<DoubleArrayTrie<u32>, (Vec<u8>, u32)>();
    assert_from_iterator::<DoubleArrayTrie<u32>, (&'static [u8], u32)>();
    assert_from_iterator::<DoubleArrayTrie<u32>, Vec<u8>>();

    assert_from_iterator::<DoubleArrayTrieChar<()>, String>();
    assert_from_iterator::<DoubleArrayTrieChar<()>, &'static str>();
    assert_from_iterator::<DoubleArrayTrieChar<()>, Vec<char>>();
    assert_from_iterator::<DoubleArrayTrieChar<()>, &'static [char]>();
    assert_from_iterator::<DoubleArrayTrieChar<u32>, (String, u32)>();
    assert_from_iterator::<DoubleArrayTrieChar<u32>, (&'static str, u32)>();
    assert_from_iterator::<DoubleArrayTrieChar<u32>, (Vec<char>, u32)>();
    assert_from_iterator::<DoubleArrayTrieChar<u32>, (&'static [char], u32)>();
    assert_from_iterator::<DoubleArrayTrieChar<u32>, Vec<char>>();

    assert_extend::<DynamicDawg<()>, String>();
    assert_extend::<DynamicDawg<()>, &'static str>();
    assert_extend::<DynamicDawg<()>, Vec<u8>>();
    assert_extend::<DynamicDawg<()>, &'static [u8]>();
    assert_extend::<DynamicDawg<u32>, (String, u32)>();
    assert_extend::<DynamicDawg<u32>, (&'static str, u32)>();
    assert_extend::<DynamicDawg<u32>, (Vec<u8>, u32)>();
    assert_extend::<DynamicDawg<u32>, (&'static [u8], u32)>();
    assert_extend::<DynamicDawg<u32>, Vec<u8>>();

    assert_extend::<DynamicDawgChar<()>, String>();
    assert_extend::<DynamicDawgChar<()>, &'static str>();
    assert_extend::<DynamicDawgChar<()>, Vec<char>>();
    assert_extend::<DynamicDawgChar<()>, &'static [char]>();
    assert_extend::<DynamicDawgChar<u32>, (String, u32)>();
    assert_extend::<DynamicDawgChar<u32>, (&'static str, u32)>();
    assert_extend::<DynamicDawgChar<u32>, (Vec<char>, u32)>();
    assert_extend::<DynamicDawgChar<u32>, (&'static [char], u32)>();
    assert_extend::<DynamicDawgChar<u32>, Vec<char>>();

    assert_extend::<DynamicDawgU64<()>, String>();
    assert_extend::<DynamicDawgU64<()>, &'static str>();
    assert_extend::<DynamicDawgU64<()>, Vec<u64>>();
    assert_extend::<DynamicDawgU64<()>, &'static [u64]>();
    assert_extend::<DynamicDawgU64<u32>, (String, u32)>();
    assert_extend::<DynamicDawgU64<u32>, (&'static str, u32)>();
    assert_extend::<DynamicDawgU64<u32>, (Vec<u64>, u32)>();
    assert_extend::<DynamicDawgU64<u32>, (&'static [u64], u32)>();
    assert_extend::<DynamicDawgU64<u32>, Vec<u64>>();
}

#[test]
fn from_iterator_keys_are_set_like_and_unit_native() {
    let byte_keys = vec![Vec::new(), vec![0, 0xff], b"shared".to_vec(), vec![0, 0xff]];
    let byte_dawg: DynamicDawg<()> = byte_keys.clone().into_iter().collect();
    let byte_dat: DoubleArrayTrie<()> = byte_keys.into_iter().collect();
    assert_eq!(Dictionary::len(&byte_dawg), Some(3));
    assert_eq!(Dictionary::len(&byte_dat), Some(3));
    assert!(byte_dawg.contains_bytes(&[0, 0xff]));
    assert!(contains_units(&byte_dat, &[0, 0xff]));

    let char_keys = vec![Vec::new(), vec!['é', '猫'], vec!['z'], vec!['é', '猫']];
    let char_dawg: DynamicDawgChar<()> = char_keys.clone().into_iter().collect();
    let char_dat: DoubleArrayTrieChar<()> = char_keys.into_iter().collect();
    assert_eq!(Dictionary::len(&char_dawg), Some(3));
    assert_eq!(Dictionary::len(&char_dat), Some(3));
    assert!(contains_units(&char_dawg, &['é', '猫']));
    assert!(contains_units(&char_dat, &['é', '猫']));

    let sequence_keys = vec![Vec::new(), vec![0, u64::MAX], vec![7], vec![0, u64::MAX]];
    let sequence_dawg: DynamicDawgU64<()> = sequence_keys.into_iter().collect();
    assert_eq!(Dictionary::len(&sequence_dawg), Some(3));
    assert!(sequence_dawg.contains_sequence(&[0, u64::MAX]));

    // Sorted DAWG collection construction publishes one privately built graph;
    // incremental insertion roots deliberately have no dense snapshot id.
    assert!(byte_dawg.root().snapshot_node_identity().is_some());
    assert!(char_dawg.root().snapshot_node_identity().is_some());
    assert!(sequence_dawg.root().snapshot_node_identity().is_some());
}

#[test]
fn from_iterator_pairs_are_last_value_wins() {
    let byte_entries = vec![
        (Vec::new(), 1_u32),
        (vec![0, 0xff], 2),
        (b"shared".to_vec(), 3),
        (vec![0, 0xff], 4),
    ];
    let byte_dawg: DynamicDawg<u32> = byte_entries.clone().into_iter().collect();
    let byte_dat: DoubleArrayTrie<u32> = byte_entries.into_iter().collect();
    assert_eq!(Dictionary::len(&byte_dawg), Some(3));
    assert_eq!(Dictionary::len(&byte_dat), Some(3));
    assert_eq!(value_at_units(&byte_dawg, &[0, 0xff]), Some(4));
    assert_eq!(value_at_units(&byte_dat, &[0, 0xff]), Some(4));

    let char_entries = vec![
        (Vec::new(), 1_u32),
        (vec!['é', '猫'], 2),
        (vec!['z'], 3),
        (vec!['é', '猫'], 4),
    ];
    let char_dawg: DynamicDawgChar<u32> = char_entries.clone().into_iter().collect();
    let char_dat: DoubleArrayTrieChar<u32> = char_entries.into_iter().collect();
    assert_eq!(value_at_units(&char_dawg, &['é', '猫']), Some(4));
    assert_eq!(value_at_units(&char_dat, &['é', '猫']), Some(4));

    let sequence_dawg: DynamicDawgU64<u32> =
        [(vec![0, u64::MAX], 1), (vec![7], 2), (vec![0, u64::MAX], 3)]
            .into_iter()
            .collect();
    assert_eq!(sequence_dawg.get_sequence_value(&[0, u64::MAX]), Some(3));

    let text_dat: DoubleArrayTrie<u32> = [("same", 1), ("other", 2), ("same", 3)]
        .into_iter()
        .collect();
    assert_eq!(text_dat.get_value("same"), Some(3));
}

#[test]
fn key_forms_on_mapped_dictionaries_create_term_only_entries() {
    let byte_dawg: DynamicDawg<u32> = [vec![0, 0xff]].into_iter().collect();
    let byte_dat: DoubleArrayTrie<u32> = [vec![0, 0xff]].into_iter().collect();
    assert!(contains_units(&byte_dawg, &[0, 0xff]));
    assert!(contains_units(&byte_dat, &[0, 0xff]));
    assert_eq!(value_at_units(&byte_dawg, &[0, 0xff]), None);
    assert_eq!(value_at_units(&byte_dat, &[0, 0xff]), None);

    let char_dawg: DynamicDawgChar<u32> = [vec!['é', '猫']].into_iter().collect();
    let char_dat: DoubleArrayTrieChar<u32> = [vec!['é', '猫']].into_iter().collect();
    assert!(contains_units(&char_dawg, &['é', '猫']));
    assert!(contains_units(&char_dat, &['é', '猫']));
    assert_eq!(value_at_units(&char_dawg, &['é', '猫']), None);
    assert_eq!(value_at_units(&char_dat, &['é', '猫']), None);

    let sequence_dawg: DynamicDawgU64<u32> = [vec![0, u64::MAX]].into_iter().collect();
    assert!(sequence_dawg.contains_sequence(&[0, u64::MAX]));
    assert_eq!(sequence_dawg.get_sequence_value(&[0, u64::MAX]), None);
}

#[test]
fn extend_is_available_only_on_mutable_dawgs_and_preserves_map_laws() {
    let byte_set = DynamicDawg::<()>::new();
    let added = MutableDictionary::extend(&byte_set, ["same", "same", "first"]);
    assert_eq!(added, 2);

    let mut byte_map = DynamicDawg::<u32>::new();
    // `DynamicDawg::extend` and `MutableDictionary::extend` are key-only,
    // count-returning APIs. Pair extension therefore uses explicit UFCS.
    std::iter::Extend::extend(
        &mut byte_map,
        [
            ("same".to_owned(), 1),
            ("other".to_owned(), 2),
            ("same".to_owned(), 3),
        ],
    );
    assert_eq!(byte_map.get_value("same"), Some(3));

    std::iter::Extend::extend(&mut byte_map, [(vec![0, 0xff], 4), (vec![0, 0xff], 5)]);
    assert_eq!(byte_map.get_bytes_value(&[0, 0xff]), Some(5));

    let mut char_map = DynamicDawgChar::<u32>::new();
    std::iter::Extend::extend(
        &mut char_map,
        [(vec!['é'], 1), (vec!['猫'], 2), (vec!['é'], 3)],
    );
    assert_eq!(char_map.get_value("é"), Some(3));

    let mut sequence_map = DynamicDawgU64::<u32>::new();
    std::iter::Extend::extend(
        &mut sequence_map,
        [(vec![0, u64::MAX], 1), (vec![7], 2), (vec![0, u64::MAX], 3)],
    );
    assert_eq!(sequence_map.get_sequence_value(&[0, u64::MAX]), Some(3));
}
