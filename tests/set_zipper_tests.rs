//! Comprehensive tests for set-theoretic zipper operations.
//!
//! This test suite validates:
//! - IntersectionZipper: A ∩ B
//! - DifferenceZipper: A \ B
//! - SymmetricDifferenceZipper: A Δ B
//! - ValueDiffZipper: terms with differing values
//! - Set-theoretic identities and properties
//! - Cross-backend consistency

use std::collections::HashSet;

use libdictenstein::difference_zipper::DifferenceZipperExt;
use libdictenstein::double_array_trie::char::DoubleArrayTrieChar;
use libdictenstein::double_array_trie::char_zipper::DoubleArrayTrieCharZipper;
use libdictenstein::double_array_trie::zipper::DoubleArrayTrieZipper;
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::dynamic_dawg::zipper::DynamicDawgZipper;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::intersection_zipper::{IntersectionZipper, IntersectionZipperExt};
use libdictenstein::symmetric_difference_zipper::SymmetricDifferenceZipperExt;
use libdictenstein::union_zipper::{LatticeJoin, UnionZipperExt};
use libdictenstein::value_diff_zipper::ValueDiffZipperExt;
use libdictenstein::zipper::{DictZipper, ValuedDictZipper};

// ============================================================================
// Helper Functions
// ============================================================================

fn sorted_results<T: Ord>(mut results: Vec<T>) -> Vec<T> {
    results.sort();
    results
}

fn collect_terms<Z: DictZipper<Unit = u8>>(
    iter: impl Iterator<Item = (Vec<u8>, Z)>,
) -> Vec<String> {
    sorted_results(
        iter.map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    )
}

// ============================================================================
// IntersectionZipper Tests - DoubleArrayTrie
// ============================================================================

#[test]
fn test_intersection_basic() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "fish", "bird"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let intersection = z1.intersection_with(z2);
    let results = collect_terms(intersection.iter());

    // Only "cat" and "fish" are in BOTH
    assert_eq!(results, vec!["cat", "fish"]);
}

#[test]
fn test_intersection_disjoint() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["fish", "bird"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let intersection = z1.intersection_with(z2);

    // No common terms
    assert_eq!(intersection.iter().count(), 0);
}

#[test]
fn test_intersection_identical() {
    let dict = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict);

    let intersection = z1.intersection_with(z2);
    let results = collect_terms(intersection.iter());

    // A ∩ A = A
    assert_eq!(results, vec!["cat", "dog", "fish"]);
}

#[test]
fn test_intersection_with_empty() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    let dict2: DoubleArrayTrie = DoubleArrayTrie::new();

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let intersection = z1.intersection_with(z2);

    // A ∩ ∅ = ∅
    assert_eq!(intersection.iter().count(), 0);
}

#[test]
fn test_intersection_three_dicts() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish", "bird"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "fish", "bird", "horse"].iter());
    let dict3 = DoubleArrayTrie::from_terms(vec!["cat", "bird", "snake"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

    let intersection = z1.intersection_all(vec![z2, z3]);
    let results = collect_terms(intersection.iter());

    // Only "bird" and "cat" are in ALL three
    assert_eq!(results, vec!["bird", "cat"]);
}

#[test]
fn test_intersection_with_values_lattice_meet() {
    let dict1 =
        DoubleArrayTrie::from_terms_with_values(vec![("score", 85u32), ("count", 100)].into_iter());
    let dict2 =
        DoubleArrayTrie::from_terms_with_values(vec![("score", 92u32), ("count", 50)].into_iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let intersection = IntersectionZipper::new(vec![z1, z2]);

    // Navigate to "score" - LatticeMeet gives min
    let score = intersection
        .descend(b's')
        .and_then(|z| z.descend(b'c'))
        .and_then(|z| z.descend(b'o'))
        .and_then(|z| z.descend(b'r'))
        .and_then(|z| z.descend(b'e'))
        .unwrap();

    assert_eq!(score.value(), Some(85)); // min(85, 92)

    let count = intersection
        .descend(b'c')
        .and_then(|z| z.descend(b'o'))
        .and_then(|z| z.descend(b'u'))
        .and_then(|z| z.descend(b'n'))
        .and_then(|z| z.descend(b't'))
        .unwrap();

    assert_eq!(count.value(), Some(50)); // min(100, 50)
}

#[test]
fn test_intersection_with_values_lattice_join() {
    let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("score", 85u32)].into_iter());
    let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("score", 92u32)].into_iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let intersection = IntersectionZipper::with_strategy(vec![z1, z2], LatticeJoin);

    let score = intersection
        .descend(b's')
        .and_then(|z| z.descend(b'c'))
        .and_then(|z| z.descend(b'o'))
        .and_then(|z| z.descend(b'r'))
        .and_then(|z| z.descend(b'e'))
        .unwrap();

    assert_eq!(score.value(), Some(92)); // max(85, 92)
}

// ============================================================================
// DifferenceZipper Tests - DoubleArrayTrie
// ============================================================================

#[test]
fn test_difference_basic() {
    let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
    let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "bird"].iter());

    let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    let difference = z_a.difference_from(z_b);
    let results = collect_terms(difference.iter());

    // "cat" and "fish" are in A but not B
    assert_eq!(results, vec!["cat", "fish"]);
}

#[test]
fn test_difference_a_minus_empty() {
    let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    let dict_b: DoubleArrayTrie = DoubleArrayTrie::new();

    let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    let difference = z_a.difference_from(z_b);
    let results = collect_terms(difference.iter());

    // A \ ∅ = A
    assert_eq!(results, vec!["cat", "dog"]);
}

#[test]
fn test_difference_empty_minus_b() {
    let dict_a: DoubleArrayTrie = DoubleArrayTrie::new();
    let dict_b = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());

    let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    let difference = z_a.difference_from(z_b);

    // ∅ \ B = ∅
    assert_eq!(difference.iter().count(), 0);
}

#[test]
fn test_difference_a_minus_a() {
    let dict = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());

    let z_a = DoubleArrayTrieZipper::new_from_dict(&dict);
    let z_b = DoubleArrayTrieZipper::new_from_dict(&dict);

    let difference = z_a.difference_from(z_b);

    // A \ A = ∅
    assert_eq!(difference.iter().count(), 0);
}

#[test]
fn test_difference_disjoint() {
    let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    let dict_b = DoubleArrayTrie::from_terms(vec!["fish", "bird"].iter());

    let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    let difference = z_a.difference_from(z_b);
    let results = collect_terms(difference.iter());

    // Disjoint: A \ B = A
    assert_eq!(results, vec!["cat", "dog"]);
}

#[test]
fn test_difference_with_values() {
    let dict_a = DoubleArrayTrie::from_terms_with_values(
        vec![("cat", 1usize), ("dog", 2), ("fish", 3)].into_iter(),
    );
    let dict_b = DoubleArrayTrie::from_terms_with_values(vec![("dog", 0usize)].into_iter());

    let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    let difference = z_a.difference_from(z_b);

    // Values come from A
    let cat = difference
        .descend(b'c')
        .and_then(|z| z.descend(b'a'))
        .and_then(|z| z.descend(b't'))
        .unwrap();

    assert!(cat.is_final());
    assert_eq!(cat.value(), Some(1));
}

// ============================================================================
// SymmetricDifferenceZipper Tests - DoubleArrayTrie
// ============================================================================

#[test]
fn test_symmetric_difference_basic() {
    let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
    let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "fish", "bird"].iter());

    let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    let sym_diff = z_a.symmetric_difference_with(z_b);
    let results = collect_terms(sym_diff.iter());

    // "cat" only in A, "bird" only in B
    assert_eq!(results, vec!["bird", "cat"]);
}

#[test]
fn test_symmetric_difference_identical() {
    let dict = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());

    let z_a = DoubleArrayTrieZipper::new_from_dict(&dict);
    let z_b = DoubleArrayTrieZipper::new_from_dict(&dict);

    let sym_diff = z_a.symmetric_difference_with(z_b);

    // A Δ A = ∅
    assert_eq!(sym_diff.iter().count(), 0);
}

#[test]
fn test_symmetric_difference_disjoint() {
    let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    let dict_b = DoubleArrayTrie::from_terms(vec!["fish", "bird"].iter());

    let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    let sym_diff = z_a.symmetric_difference_with(z_b);
    let results = collect_terms(sym_diff.iter());

    // Disjoint: A Δ B = A ∪ B
    assert_eq!(results, vec!["bird", "cat", "dog", "fish"]);
}

#[test]
fn test_symmetric_difference_with_empty() {
    let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    let dict_b: DoubleArrayTrie = DoubleArrayTrie::new();

    let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    let sym_diff = z_a.symmetric_difference_with(z_b);
    let results = collect_terms(sym_diff.iter());

    // A Δ ∅ = A
    assert_eq!(results, vec!["cat", "dog"]);
}

#[test]
fn test_symmetric_difference_three_dicts() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["a", "b", "c"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["b", "c", "d"].iter());
    let dict3 = DoubleArrayTrie::from_terms(vec!["c", "d", "e"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

    let sym_diff = z1.symmetric_difference_all(vec![z2, z3]);
    let results = collect_terms(sym_diff.iter());

    // Terms in exactly one dict: "a" (dict1), "e" (dict3)
    assert_eq!(results, vec!["a", "e"]);
}

#[test]
fn test_symmetric_difference_with_values() {
    let dict_a =
        DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize), ("dog", 2)].into_iter());
    let dict_b =
        DoubleArrayTrie::from_terms_with_values(vec![("dog", 20usize), ("fish", 3)].into_iter());

    let z_a = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    let sym_diff = z_a.symmetric_difference_with(z_b);

    // "cat" only in A, value = 1
    let cat = sym_diff
        .descend(b'c')
        .and_then(|z| z.descend(b'a'))
        .and_then(|z| z.descend(b't'))
        .unwrap();
    assert_eq!(cat.value(), Some(1));

    // "fish" only in B, value = 3
    let fish = sym_diff
        .descend(b'f')
        .and_then(|z| z.descend(b'i'))
        .and_then(|z| z.descend(b's'))
        .and_then(|z| z.descend(b'h'))
        .unwrap();
    assert_eq!(fish.value(), Some(3));
}

// ============================================================================
// ValueDiffZipper Tests - DoubleArrayTrie
// ============================================================================

#[test]
fn test_value_diff_basic() {
    let dict1 = DoubleArrayTrie::from_terms_with_values(
        vec![("cat", 10usize), ("dog", 20), ("fish", 30)].into_iter(),
    );
    let dict2 = DoubleArrayTrie::from_terms_with_values(
        vec![("cat", 10usize), ("dog", 25), ("fish", 35)].into_iter(),
    );

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let diff = z1.value_diff_with(z2);

    let mut results: Vec<_> = diff
        .iter_diffs()
        .map(|d| (d.path_string(), d.left_value, d.right_value))
        .collect();
    results.sort_by(|a, b| a.0.cmp(&b.0));

    // "cat" same value, excluded; "dog" and "fish" differ
    assert_eq!(
        results,
        vec![("dog".to_string(), 20, 25), ("fish".to_string(), 30, 35),]
    );
}

#[test]
fn test_value_diff_identical() {
    let dict1 =
        DoubleArrayTrie::from_terms_with_values(vec![("cat", 10usize), ("dog", 20)].into_iter());
    let dict2 =
        DoubleArrayTrie::from_terms_with_values(vec![("cat", 10usize), ("dog", 20)].into_iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let diff = z1.value_diff_with(z2);

    // All same, no diffs
    assert_eq!(diff.iter_diffs().count(), 0);
}

#[test]
fn test_value_diff_all_different() {
    let dict1 = DoubleArrayTrie::from_terms_with_values(
        vec![("a", 1usize), ("b", 2), ("c", 3)].into_iter(),
    );
    let dict2 = DoubleArrayTrie::from_terms_with_values(
        vec![("a", 10usize), ("b", 20), ("c", 30)].into_iter(),
    );

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let diff = z1.value_diff_with(z2);

    // All differ
    assert_eq!(diff.iter_diffs().count(), 3);
}

#[test]
fn test_value_diff_disjoint() {
    let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize)].into_iter());
    let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("dog", 2usize)].into_iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let diff = z1.value_diff_with(z2);

    // No common terms, no diffs
    assert_eq!(diff.iter_diffs().count(), 0);
}

#[test]
fn test_value_diff_navigation() {
    let dict1 =
        DoubleArrayTrie::from_terms_with_values(vec![("score", 85u32), ("count", 100)].into_iter());
    let dict2 =
        DoubleArrayTrie::from_terms_with_values(vec![("score", 92u32), ("count", 100)].into_iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let diff = z1.value_diff_with(z2);

    // "score" values differ
    let score = diff
        .descend(b's')
        .and_then(|z| z.descend(b'c'))
        .and_then(|z| z.descend(b'o'))
        .and_then(|z| z.descend(b'r'))
        .and_then(|z| z.descend(b'e'))
        .unwrap();

    assert!(score.is_final());
    assert_eq!(score.left_value(), Some(85));
    assert_eq!(score.right_value(), Some(92));

    // "count" values are same
    let count = diff
        .descend(b'c')
        .and_then(|z| z.descend(b'o'))
        .and_then(|z| z.descend(b'u'))
        .and_then(|z| z.descend(b'n'))
        .and_then(|z| z.descend(b't'))
        .unwrap();

    assert!(!count.is_final()); // Same values, not a diff
}

// ============================================================================
// Set-Theoretic Identity Tests
// ============================================================================

#[test]
fn test_identity_symmetric_difference_via_differences() {
    // Property: A Δ B = (A \ B) ∪ (B \ A)
    let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
    let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "fish", "bird"].iter());

    let z_a1 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b1 = DoubleArrayTrieZipper::new_from_dict(&dict_b);
    let z_a2 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b2 = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    // Compute A Δ B directly
    let sym_diff = z_a1.symmetric_difference_with(z_b1);
    let sym_diff_terms: HashSet<String> = sym_diff
        .iter()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    // Compute (A \ B) ∪ (B \ A)
    let a_minus_b: HashSet<String> = z_a2
        .clone()
        .difference_from(z_b2.clone())
        .iter()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    let b_minus_a: HashSet<String> = z_b2
        .difference_from(z_a2)
        .iter()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    let union_of_diffs: HashSet<String> = a_minus_b.union(&b_minus_a).cloned().collect();

    assert_eq!(sym_diff_terms, union_of_diffs);
}

#[test]
fn test_identity_symmetric_difference_via_complement() {
    // Property: A Δ B = (A ∪ B) \ (A ∩ B)
    let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
    let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "fish", "bird"].iter());

    let z_a1 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b1 = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    // Compute A Δ B directly
    let sym_diff = z_a1.clone().symmetric_difference_with(z_b1.clone());
    let sym_diff_terms: HashSet<String> = sym_diff
        .iter()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    // Compute (A ∪ B)
    let union_terms: HashSet<String> = z_a1
        .clone()
        .union_with(z_b1.clone())
        .iter()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    // Compute (A ∩ B)
    let intersection_terms: HashSet<String> = z_a1
        .intersection_with(z_b1)
        .iter()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    // (A ∪ B) \ (A ∩ B)
    let expected: HashSet<String> = union_terms
        .difference(&intersection_terms)
        .cloned()
        .collect();

    assert_eq!(sym_diff_terms, expected);
}

#[test]
fn test_identity_intersection_commutativity() {
    // Property: A ∩ B = B ∩ A
    let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
    let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "fish", "bird"].iter());

    let z_a1 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b1 = DoubleArrayTrieZipper::new_from_dict(&dict_b);
    let z_a2 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b2 = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    let a_inter_b: HashSet<String> = z_a1
        .intersection_with(z_b1)
        .iter()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    let b_inter_a: HashSet<String> = z_b2
        .intersection_with(z_a2)
        .iter()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    assert_eq!(a_inter_b, b_inter_a);
}

#[test]
fn test_identity_difference_not_commutative() {
    // Property: A \ B ≠ B \ A (in general)
    let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "fish"].iter());

    let z_a1 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b1 = DoubleArrayTrieZipper::new_from_dict(&dict_b);
    let z_a2 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b2 = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    let a_minus_b: HashSet<String> = z_a1
        .difference_from(z_b1)
        .iter()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    let b_minus_a: HashSet<String> = z_b2
        .difference_from(z_a2)
        .iter()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    // A \ B = {"cat"}, B \ A = {"fish"}
    assert_ne!(a_minus_b, b_minus_a);
    assert_eq!(a_minus_b, HashSet::from(["cat".to_string()]));
    assert_eq!(b_minus_a, HashSet::from(["fish".to_string()]));
}

#[test]
fn test_identity_symmetric_difference_commutativity() {
    // Property: A Δ B = B Δ A
    let dict_a = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
    let dict_b = DoubleArrayTrie::from_terms(vec!["dog", "fish", "bird"].iter());

    let z_a1 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b1 = DoubleArrayTrieZipper::new_from_dict(&dict_b);
    let z_a2 = DoubleArrayTrieZipper::new_from_dict(&dict_a);
    let z_b2 = DoubleArrayTrieZipper::new_from_dict(&dict_b);

    let a_delta_b: HashSet<String> = z_a1
        .symmetric_difference_with(z_b1)
        .iter()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    let b_delta_a: HashSet<String> = z_b2
        .symmetric_difference_with(z_a2)
        .iter()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    assert_eq!(a_delta_b, b_delta_a);
}

// ============================================================================
// Cross-Backend Tests - DynamicDawg
// ============================================================================

#[test]
fn test_intersection_dawg() {
    let dict1: DynamicDawg<()> = DynamicDawg::from_terms(vec!["cat", "dog", "fish"].iter());
    let dict2: DynamicDawg<()> = DynamicDawg::from_terms(vec!["cat", "fish", "bird"].iter());

    let z1 = DynamicDawgZipper::new_from_dict(&dict1);
    let z2 = DynamicDawgZipper::new_from_dict(&dict2);

    let intersection = z1.intersection_with(z2);
    let results = collect_terms(intersection.iter());

    assert_eq!(results, vec!["cat", "fish"]);
}

#[test]
fn test_difference_dawg() {
    let dict_a: DynamicDawg<()> = DynamicDawg::from_terms(vec!["cat", "dog", "fish"].iter());
    let dict_b: DynamicDawg<()> = DynamicDawg::from_terms(vec!["dog", "bird"].iter());

    let z_a = DynamicDawgZipper::new_from_dict(&dict_a);
    let z_b = DynamicDawgZipper::new_from_dict(&dict_b);

    let difference = z_a.difference_from(z_b);
    let results = collect_terms(difference.iter());

    assert_eq!(results, vec!["cat", "fish"]);
}

#[test]
fn test_symmetric_difference_dawg() {
    let dict_a: DynamicDawg<()> = DynamicDawg::from_terms(vec!["cat", "dog", "fish"].iter());
    let dict_b: DynamicDawg<()> = DynamicDawg::from_terms(vec!["dog", "fish", "bird"].iter());

    let z_a = DynamicDawgZipper::new_from_dict(&dict_a);
    let z_b = DynamicDawgZipper::new_from_dict(&dict_b);

    let sym_diff = z_a.symmetric_difference_with(z_b);
    let results = collect_terms(sym_diff.iter());

    assert_eq!(results, vec!["bird", "cat"]);
}

// ============================================================================
// Unicode (Char) Dictionary Tests
// ============================================================================

#[test]
fn test_intersection_char() {
    let dict1 = DoubleArrayTrieChar::from_terms(vec!["café", "naïve", "résumé"].iter());
    let dict2 = DoubleArrayTrieChar::from_terms(vec!["café", "naïve", "fiancé"].iter());

    let z1 = DoubleArrayTrieCharZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieCharZipper::new_from_dict(&dict2);

    let intersection = z1.intersection_with(z2);
    let mut results: Vec<String> = intersection
        .iter()
        .map(|(path, _)| path.iter().collect())
        .collect();
    results.sort();

    assert_eq!(results, vec!["café", "naïve"]);
}

#[test]
fn test_difference_char() {
    let dict_a = DoubleArrayTrieChar::from_terms(vec!["中国", "日本", "韩国"].iter());
    let dict_b = DoubleArrayTrieChar::from_terms(vec!["日本"].iter());

    let z_a = DoubleArrayTrieCharZipper::new_from_dict(&dict_a);
    let z_b = DoubleArrayTrieCharZipper::new_from_dict(&dict_b);

    let difference = z_a.difference_from(z_b);
    let mut results: Vec<String> = difference
        .iter()
        .map(|(path, _)| path.iter().collect())
        .collect();
    results.sort();

    assert_eq!(results, vec!["中国", "韩国"]);
}

#[test]
fn test_symmetric_difference_char() {
    let dict_a = DoubleArrayTrieChar::from_terms(vec!["α", "β", "γ"].iter());
    let dict_b = DoubleArrayTrieChar::from_terms(vec!["β", "γ", "δ"].iter());

    let z_a = DoubleArrayTrieCharZipper::new_from_dict(&dict_a);
    let z_b = DoubleArrayTrieCharZipper::new_from_dict(&dict_b);

    let sym_diff = z_a.symmetric_difference_with(z_b);
    let mut results: Vec<String> = sym_diff
        .iter()
        .map(|(path, _)| path.iter().collect())
        .collect();
    results.sort();

    assert_eq!(results, vec!["α", "δ"]);
}

// ============================================================================
// Large Dictionary Tests
// ============================================================================

#[test]
fn test_intersection_large() {
    let terms1: Vec<String> = (0..500).map(|i| format!("word{}", i)).collect();
    let terms2: Vec<String> = (250..750).map(|i| format!("word{}", i)).collect();

    let dict1 = DoubleArrayTrie::from_terms(terms1.iter());
    let dict2 = DoubleArrayTrie::from_terms(terms2.iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let intersection = z1.intersection_with(z2);

    // Intersection: 250..500 = 250 terms
    assert_eq!(intersection.iter().count(), 250);
}

#[test]
fn test_difference_large() {
    let terms1: Vec<String> = (0..500).map(|i| format!("word{}", i)).collect();
    let terms2: Vec<String> = (250..750).map(|i| format!("word{}", i)).collect();

    let dict1 = DoubleArrayTrie::from_terms(terms1.iter());
    let dict2 = DoubleArrayTrie::from_terms(terms2.iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let difference = z1.difference_from(z2);

    // A \ B: 0..250 = 250 terms
    assert_eq!(difference.iter().count(), 250);
}

#[test]
fn test_symmetric_difference_large() {
    let terms1: Vec<String> = (0..500).map(|i| format!("word{}", i)).collect();
    let terms2: Vec<String> = (250..750).map(|i| format!("word{}", i)).collect();

    let dict1 = DoubleArrayTrie::from_terms(terms1.iter());
    let dict2 = DoubleArrayTrie::from_terms(terms2.iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let sym_diff = z1.symmetric_difference_with(z2);

    // A Δ B: (0..250) ∪ (500..750) = 500 terms
    assert_eq!(sym_diff.iter().count(), 500);
}

// ============================================================================
// Cross-Backend Consistency Tests
// ============================================================================

#[test]
fn test_consistency_dat_dawg_intersection() {
    let terms1 = vec!["apple", "application"];
    let terms2 = vec!["apple", "banana"];

    // DoubleArrayTrie
    let dat1 = DoubleArrayTrie::from_terms(terms1.iter());
    let dat2 = DoubleArrayTrie::from_terms(terms2.iter());
    let dat_z1 = DoubleArrayTrieZipper::new_from_dict(&dat1);
    let dat_z2 = DoubleArrayTrieZipper::new_from_dict(&dat2);
    let dat_intersection = dat_z1.intersection_with(dat_z2);

    // DynamicDawg
    let dawg1: DynamicDawg<()> = DynamicDawg::from_terms(terms1.iter());
    let dawg2: DynamicDawg<()> = DynamicDawg::from_terms(terms2.iter());
    let dawg_z1 = DynamicDawgZipper::new_from_dict(&dawg1);
    let dawg_z2 = DynamicDawgZipper::new_from_dict(&dawg2);
    let dawg_intersection = dawg_z1.intersection_with(dawg_z2);

    let dat_results = collect_terms(dat_intersection.iter());
    let dawg_results = collect_terms(dawg_intersection.iter());

    assert_eq!(dat_results, dawg_results);
    assert_eq!(dat_results, vec!["apple"]);
}

#[test]
fn test_consistency_dat_dawg_difference() {
    let terms1 = vec!["apple", "application", "banana"];
    let terms2 = vec!["apple", "banana"];

    // DoubleArrayTrie
    let dat1 = DoubleArrayTrie::from_terms(terms1.iter());
    let dat2 = DoubleArrayTrie::from_terms(terms2.iter());
    let dat_z1 = DoubleArrayTrieZipper::new_from_dict(&dat1);
    let dat_z2 = DoubleArrayTrieZipper::new_from_dict(&dat2);
    let dat_difference = dat_z1.difference_from(dat_z2);

    // DynamicDawg
    let dawg1: DynamicDawg<()> = DynamicDawg::from_terms(terms1.iter());
    let dawg2: DynamicDawg<()> = DynamicDawg::from_terms(terms2.iter());
    let dawg_z1 = DynamicDawgZipper::new_from_dict(&dawg1);
    let dawg_z2 = DynamicDawgZipper::new_from_dict(&dawg2);
    let dawg_difference = dawg_z1.difference_from(dawg_z2);

    let dat_results = collect_terms(dat_difference.iter());
    let dawg_results = collect_terms(dawg_difference.iter());

    assert_eq!(dat_results, dawg_results);
    assert_eq!(dat_results, vec!["application"]);
}
