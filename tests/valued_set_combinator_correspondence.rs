//! Executable correspondence checks for valued set-combinator merge laws.
//!
//! These tests exercise the law surface in
//! `formal-verification/rocq/Spec/ValuedSetCombinatorSpec.v` against the
//! public `UnionZipper` and `IntersectionZipper` APIs.

mod common;

use common::strategies::ascii_term;
use libdictenstein::double_array_trie::char::DoubleArrayTrieChar;
use libdictenstein::double_array_trie::char_zipper::DoubleArrayTrieCharZipper;
use libdictenstein::double_array_trie::zipper::DoubleArrayTrieZipper;
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::dynamic_dawg::zipper::DynamicDawgZipper;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::intersection_zipper::{IntersectionZipper, ValuedIntersectionIterator};
use libdictenstein::union_zipper::{
    FirstWins, LastWins, LatticeJoin, LatticeMeet, UnionZipper, ValueMergeStrategy,
    ValuedUnionIterator,
};
use libdictenstein::zipper::{DictZipper, ValuedDictZipper};
use proptest::prelude::*;
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Copy, Debug)]
struct Sum;

impl ValueMergeStrategy<i32> for Sum {
    fn merge(&self, existing: i32, new: i32) -> i32 {
        existing + new
    }
}

fn fixture_maps() -> Vec<BTreeMap<String, i32>> {
    [
        vec![("cat", 1), ("dog", 2), ("shared", 5), ("left", 9)],
        vec![("cat", 10), ("bird", 3), ("shared", 7), ("middle", 8)],
        vec![("cat", 100), ("dog", 20), ("fox", 4), ("shared", 11)],
    ]
    .into_iter()
    .map(|pairs| {
        pairs
            .into_iter()
            .map(|(term, value)| (term.to_string(), value))
            .collect()
    })
    .collect()
}

fn normalize_pairs(pairs: Vec<(String, i32)>) -> BTreeMap<String, i32> {
    pairs.into_iter().collect()
}

fn reference_union_i32<F>(maps: &[BTreeMap<String, i32>], merge: F) -> BTreeMap<String, i32>
where
    F: Fn(i32, i32) -> i32 + Copy,
{
    let mut out = BTreeMap::new();
    for map in maps {
        for (term, value) in map {
            out.entry(term.clone())
                .and_modify(|existing| *existing = merge(*existing, *value))
                .or_insert(*value);
        }
    }
    out
}

fn reference_intersection_i32<F>(maps: &[BTreeMap<String, i32>], merge: F) -> BTreeMap<String, i32>
where
    F: Fn(i32, i32) -> i32 + Copy,
{
    if maps.is_empty() {
        return BTreeMap::new();
    }

    let mut out = BTreeMap::new();
    for term in maps[0].keys() {
        let mut values = maps.iter().filter_map(|map| map.get(term).copied());
        let Some(first) = values.next() else {
            continue;
        };

        let mut count = 1usize;
        let mut merged = first;
        for value in values {
            count += 1;
            merged = merge(merged, value);
        }

        if count == maps.len() {
            out.insert(term.clone(), merged);
        }
    }
    out
}

fn collect_byte_union_i32<S>(maps: &[BTreeMap<String, i32>], strategy: S) -> BTreeMap<String, i32>
where
    S: ValueMergeStrategy<i32> + Clone + Send + Sync,
{
    let tries: Vec<DoubleArrayTrie<i32>> = maps
        .iter()
        .map(|map| {
            DoubleArrayTrie::from_terms_with_values(
                map.iter().map(|(term, value)| (term.as_str(), *value)),
            )
        })
        .collect();
    let zippers: Vec<_> = tries
        .iter()
        .map(DoubleArrayTrieZipper::new_from_dict)
        .collect();
    let union = UnionZipper::with_strategy(zippers, strategy);

    ValuedUnionIterator::new(union)
        .map(|(path, value)| {
            (
                String::from_utf8(path).expect("test terms are UTF-8"),
                value,
            )
        })
        .collect()
}

fn collect_byte_intersection_i32<S>(
    maps: &[BTreeMap<String, i32>],
    strategy: S,
) -> BTreeMap<String, i32>
where
    S: ValueMergeStrategy<i32> + Clone + Send + Sync,
{
    let tries: Vec<DoubleArrayTrie<i32>> = maps
        .iter()
        .map(|map| {
            DoubleArrayTrie::from_terms_with_values(
                map.iter().map(|(term, value)| (term.as_str(), *value)),
            )
        })
        .collect();
    let zippers: Vec<_> = tries
        .iter()
        .map(DoubleArrayTrieZipper::new_from_dict)
        .collect();
    let intersection = IntersectionZipper::with_strategy(zippers, strategy);

    ValuedIntersectionIterator::new(intersection)
        .map(|(path, value)| {
            (
                String::from_utf8(path).expect("test terms are UTF-8"),
                value,
            )
        })
        .collect()
}

fn collect_char_union_i32<S>(maps: &[BTreeMap<String, i32>], strategy: S) -> BTreeMap<String, i32>
where
    S: ValueMergeStrategy<i32> + Clone + Send + Sync,
{
    let tries: Vec<DoubleArrayTrieChar<i32>> = maps
        .iter()
        .map(|map| {
            DoubleArrayTrieChar::from_terms_with_values(
                map.iter().map(|(term, value)| (term.as_str(), *value)),
            )
        })
        .collect();
    let zippers: Vec<_> = tries
        .iter()
        .map(DoubleArrayTrieCharZipper::new_from_dict)
        .collect();
    let union = UnionZipper::with_strategy(zippers, strategy);

    ValuedUnionIterator::new(union)
        .map(|(path, value)| (path.into_iter().collect::<String>(), value))
        .collect()
}

fn collect_char_intersection_i32<S>(
    maps: &[BTreeMap<String, i32>],
    strategy: S,
) -> BTreeMap<String, i32>
where
    S: ValueMergeStrategy<i32> + Clone + Send + Sync,
{
    let tries: Vec<DoubleArrayTrieChar<i32>> = maps
        .iter()
        .map(|map| {
            DoubleArrayTrieChar::from_terms_with_values(
                map.iter().map(|(term, value)| (term.as_str(), *value)),
            )
        })
        .collect();
    let zippers: Vec<_> = tries
        .iter()
        .map(DoubleArrayTrieCharZipper::new_from_dict)
        .collect();
    let intersection = IntersectionZipper::with_strategy(zippers, strategy);

    ValuedIntersectionIterator::new(intersection)
        .map(|(path, value)| (path.into_iter().collect::<String>(), value))
        .collect()
}

fn collect_dawg_union_i32<S>(maps: &[BTreeMap<String, i32>], strategy: S) -> BTreeMap<String, i32>
where
    S: ValueMergeStrategy<i32> + Clone + Send + Sync,
{
    let dawgs: Vec<DynamicDawg<i32>> = maps
        .iter()
        .map(|map| {
            DynamicDawg::from_terms_with_values(
                map.iter().map(|(term, value)| (term.as_str(), *value)),
            )
        })
        .collect();
    let zippers: Vec<_> = dawgs.iter().map(DynamicDawgZipper::new_from_dict).collect();
    let union = UnionZipper::with_strategy(zippers, strategy);

    ValuedUnionIterator::new(union)
        .map(|(path, value)| {
            (
                String::from_utf8(path).expect("test terms are UTF-8"),
                value,
            )
        })
        .collect()
}

fn collect_dawg_intersection_i32<S>(
    maps: &[BTreeMap<String, i32>],
    strategy: S,
) -> BTreeMap<String, i32>
where
    S: ValueMergeStrategy<i32> + Clone + Send + Sync,
{
    let dawgs: Vec<DynamicDawg<i32>> = maps
        .iter()
        .map(|map| {
            DynamicDawg::from_terms_with_values(
                map.iter().map(|(term, value)| (term.as_str(), *value)),
            )
        })
        .collect();
    let zippers: Vec<_> = dawgs.iter().map(DynamicDawgZipper::new_from_dict).collect();
    let intersection = IntersectionZipper::with_strategy(zippers, strategy);

    ValuedIntersectionIterator::new(intersection)
        .map(|(path, value)| {
            (
                String::from_utf8(path).expect("test terms are UTF-8"),
                value,
            )
        })
        .collect()
}

fn byte_union_value_at<S>(maps: &[BTreeMap<String, i32>], strategy: S, term: &str) -> Option<i32>
where
    S: ValueMergeStrategy<i32> + Clone + Send + Sync,
{
    let tries: Vec<DoubleArrayTrie<i32>> = maps
        .iter()
        .map(|map| {
            DoubleArrayTrie::from_terms_with_values(
                map.iter().map(|(term, value)| (term.as_str(), *value)),
            )
        })
        .collect();
    let zippers: Vec<_> = tries
        .iter()
        .map(DoubleArrayTrieZipper::new_from_dict)
        .collect();
    let mut cursor = UnionZipper::with_strategy(zippers, strategy);
    for byte in term.bytes() {
        cursor = cursor.descend(byte)?;
    }
    cursor.value()
}

fn hset(values: &[u8]) -> HashSet<u8> {
    values.iter().copied().collect()
}

fn reference_union_sets(maps: &[BTreeMap<String, HashSet<u8>>]) -> BTreeMap<String, HashSet<u8>> {
    let mut out = BTreeMap::new();
    for map in maps {
        for (term, value) in map {
            out.entry(term.clone())
                .and_modify(|existing: &mut HashSet<u8>| {
                    *existing = existing.union(value).copied().collect();
                })
                .or_insert_with(|| value.clone());
        }
    }
    out
}

fn reference_intersection_sets(
    maps: &[BTreeMap<String, HashSet<u8>>],
) -> BTreeMap<String, HashSet<u8>> {
    if maps.is_empty() {
        return BTreeMap::new();
    }

    let mut out = BTreeMap::new();
    for term in maps[0].keys() {
        let mut values = maps.iter().filter_map(|map| map.get(term));
        let Some(first) = values.next() else {
            continue;
        };

        let mut count = 1usize;
        let mut merged = first.clone();
        for value in values {
            count += 1;
            merged = merged.intersection(value).copied().collect();
        }

        if count == maps.len() {
            out.insert(term.clone(), merged);
        }
    }
    out
}

fn collect_byte_union_sets<S>(
    maps: &[BTreeMap<String, HashSet<u8>>],
    strategy: S,
) -> BTreeMap<String, HashSet<u8>>
where
    S: ValueMergeStrategy<HashSet<u8>> + Clone + Send + Sync,
{
    let tries: Vec<DoubleArrayTrie<HashSet<u8>>> = maps
        .iter()
        .map(|map| {
            DoubleArrayTrie::from_terms_with_values(
                map.iter()
                    .map(|(term, value)| (term.as_str(), value.clone())),
            )
        })
        .collect();
    let zippers: Vec<_> = tries
        .iter()
        .map(DoubleArrayTrieZipper::new_from_dict)
        .collect();
    let union = UnionZipper::with_strategy(zippers, strategy);

    ValuedUnionIterator::new(union)
        .map(|(path, value)| {
            (
                String::from_utf8(path).expect("test terms are UTF-8"),
                value,
            )
        })
        .collect()
}

fn collect_byte_intersection_sets<S>(
    maps: &[BTreeMap<String, HashSet<u8>>],
    strategy: S,
) -> BTreeMap<String, HashSet<u8>>
where
    S: ValueMergeStrategy<HashSet<u8>> + Clone + Send + Sync,
{
    let tries: Vec<DoubleArrayTrie<HashSet<u8>>> = maps
        .iter()
        .map(|map| {
            DoubleArrayTrie::from_terms_with_values(
                map.iter()
                    .map(|(term, value)| (term.as_str(), value.clone())),
            )
        })
        .collect();
    let zippers: Vec<_> = tries
        .iter()
        .map(DoubleArrayTrieZipper::new_from_dict)
        .collect();
    let intersection = IntersectionZipper::with_strategy(zippers, strategy);

    ValuedIntersectionIterator::new(intersection)
        .map(|(path, value)| {
            (
                String::from_utf8(path).expect("test terms are UTF-8"),
                value,
            )
        })
        .collect()
}

#[test]
fn byte_union_strategies_match_reference_fold() {
    let maps = fixture_maps();

    assert_eq!(
        collect_byte_union_i32(&maps, FirstWins),
        reference_union_i32(&maps, |existing, _new| existing)
    );
    assert_eq!(
        collect_byte_union_i32(&maps, LastWins),
        reference_union_i32(&maps, |_existing, new| new)
    );
    assert_eq!(
        collect_byte_union_i32(&maps, Sum),
        reference_union_i32(&maps, |existing, new| existing + new)
    );

    assert_eq!(byte_union_value_at(&maps, Sum, "cat"), Some(111));
    assert_eq!(byte_union_value_at(&maps, Sum, "shared"), Some(23));
    assert_eq!(byte_union_value_at(&maps, Sum, "absent"), None);
}

#[test]
fn byte_intersection_strategies_match_reference_fold() {
    let maps = fixture_maps();

    assert_eq!(
        collect_byte_intersection_i32(&maps, FirstWins),
        reference_intersection_i32(&maps, |existing, _new| existing)
    );
    assert_eq!(
        collect_byte_intersection_i32(&maps, LastWins),
        reference_intersection_i32(&maps, |_existing, new| new)
    );
    assert_eq!(
        collect_byte_intersection_i32(&maps, Sum),
        reference_intersection_i32(&maps, |existing, new| existing + new)
    );
    assert_eq!(
        collect_byte_intersection_i32(&maps, LatticeJoin),
        reference_intersection_i32(&maps, i32::max)
    );
    assert_eq!(
        collect_byte_intersection_i32(&maps, LatticeMeet),
        reference_intersection_i32(&maps, i32::min)
    );
}

#[test]
fn unicode_char_zippers_match_reference_fold() {
    let maps: Vec<BTreeMap<String, i32>> = [
        vec![("café", 4), ("λambda", 8), ("共通", 2)],
        vec![("café", 7), ("naïve", 3), ("共通", 9)],
        vec![("café", 1), ("共通", 5), ("東京", 6)],
    ]
    .into_iter()
    .map(|pairs| {
        pairs
            .into_iter()
            .map(|(term, value)| (term.to_string(), value))
            .collect()
    })
    .collect();

    assert_eq!(
        collect_char_union_i32(&maps, LastWins),
        reference_union_i32(&maps, |_existing, new| new)
    );
    assert_eq!(
        collect_char_intersection_i32(&maps, LatticeJoin),
        reference_intersection_i32(&maps, i32::max)
    );
}

#[test]
fn dynamic_dawg_zippers_match_reference_fold() {
    let maps = fixture_maps();

    assert_eq!(
        collect_dawg_union_i32(&maps, Sum),
        reference_union_i32(&maps, |existing, new| existing + new)
    );
    assert_eq!(
        collect_dawg_intersection_i32(&maps, LatticeJoin),
        reference_intersection_i32(&maps, i32::max)
    );
}

#[test]
fn lattice_set_values_match_join_and_meet_references() {
    let maps: Vec<BTreeMap<String, HashSet<u8>>> = [
        vec![
            ("tag", hset(&[1, 2])),
            ("left", hset(&[9])),
            ("shared", hset(&[1, 2, 3])),
        ],
        vec![
            ("tag", hset(&[2, 3])),
            ("middle", hset(&[8])),
            ("shared", hset(&[2, 3, 4])),
        ],
        vec![
            ("tag", hset(&[3, 4])),
            ("right", hset(&[7])),
            ("shared", hset(&[3, 4, 5])),
        ],
    ]
    .into_iter()
    .map(|pairs| {
        pairs
            .into_iter()
            .map(|(term, value)| (term.to_string(), value))
            .collect()
    })
    .collect();

    assert_eq!(
        collect_byte_union_sets(&maps, LatticeJoin),
        reference_union_sets(&maps)
    );
    assert_eq!(
        collect_byte_intersection_sets(&maps, LatticeMeet),
        reference_intersection_sets(&maps)
    );
}

#[test]
fn empty_and_disjoint_inputs_follow_domain_laws() {
    let empty: BTreeMap<String, i32> = BTreeMap::new();
    let left = BTreeMap::from([("left".to_string(), 1)]);
    let right = BTreeMap::from([("right".to_string(), 2)]);
    let maps = vec![empty.clone(), left.clone(), right.clone()];

    assert_eq!(
        collect_byte_union_i32(&maps, FirstWins),
        reference_union_i32(&maps, |existing, _new| existing)
    );
    assert_eq!(
        collect_byte_intersection_i32(&maps, LatticeJoin),
        BTreeMap::new()
    );
    assert_eq!(
        collect_byte_intersection_i32(&[left, right], LatticeMeet),
        BTreeMap::new()
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn generated_byte_union_matches_reference_fold(
        first in prop::collection::vec((ascii_term(1, 8), -20i32..=20), 0..=12),
        second in prop::collection::vec((ascii_term(1, 8), -20i32..=20), 0..=12),
        third in prop::collection::vec((ascii_term(1, 8), -20i32..=20), 0..=12),
    ) {
        let maps = vec![
            normalize_pairs(first),
            normalize_pairs(second),
            normalize_pairs(third),
        ];

        prop_assert_eq!(
            collect_byte_union_i32(&maps, FirstWins),
            reference_union_i32(&maps, |existing, _new| existing)
        );
        prop_assert_eq!(
            collect_byte_union_i32(&maps, LastWins),
            reference_union_i32(&maps, |_existing, new| new)
        );
        prop_assert_eq!(
            collect_byte_union_i32(&maps, Sum),
            reference_union_i32(&maps, |existing, new| existing + new)
        );
    }
}
