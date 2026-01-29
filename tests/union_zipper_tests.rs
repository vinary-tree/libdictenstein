//! Comprehensive tests for UnionZipper functionality across all dictionary backends.
//!
//! This test suite validates:
//! - Basic union of two dictionaries
//! - Union with N dictionaries
//! - Duplicate term handling (FirstWins, LastWins)
//! - Valued dictionary union
//! - Unicode (char) dictionary union
//! - Empty dictionary handling
//! - Composability with PrefixZipper
//! - Cross-backend consistency (DAT, DAWG)

use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
use libdictenstein::double_array_trie_char_zipper::DoubleArrayTrieCharZipper;
use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::dynamic_dawg_char::DynamicDawgChar;
use libdictenstein::dynamic_dawg_char_zipper::DynamicDawgCharZipper;
use libdictenstein::dynamic_dawg_zipper::DynamicDawgZipper;
use libdictenstein::prefix_zipper::PrefixZipper;
use libdictenstein::union_zipper::{
    LastWins, LatticeJoin, LatticeMeet, UnionZipper, UnionZipperExt, ValueMergeStrategy,
    ValuedUnionIterator,
};
use std::collections::HashSet;
use libdictenstein::zipper::{DictZipper, ValuedDictZipper};

// ============================================================================
// Helper Functions
// ============================================================================

fn sorted_results<T: Ord>(mut results: Vec<T>) -> Vec<T> {
    results.sort();
    results
}

// ============================================================================
// Basic Union Tests - DoubleArrayTrie
// ============================================================================

#[test]
fn test_basic_union_two_dictionaries() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["fish", "bird"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["bird", "cat", "dog", "fish"]);
}

#[test]
fn test_union_with_overlapping_terms() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "bird", "fish"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    // Each term should appear only once
    assert_eq!(results, vec!["bird", "cat", "dog", "fish"]);
}

#[test]
fn test_union_n_dictionaries() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["alpha"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["beta"].iter());
    let dict3 = DoubleArrayTrie::from_terms(vec!["gamma"].iter());
    let dict4 = DoubleArrayTrie::from_terms(vec!["delta"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);
    let z4 = DoubleArrayTrieZipper::new_from_dict(&dict4);

    let union = z1.union_all(vec![z2, z3, z4]);

    assert_eq!(union.dictionary_count(), 4);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["alpha", "beta", "delta", "gamma"]);
}

#[test]
fn test_union_identical_dictionaries() {
    let dict = DoubleArrayTrie::from_terms(vec!["cat", "dog", "fish"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    // Should still only yield each term once
    assert_eq!(results, vec!["cat", "dog", "fish"]);
}

// ============================================================================
// Empty Dictionary Tests
// ============================================================================

#[test]
fn test_union_both_empty() {
    let dict1: DoubleArrayTrie = DoubleArrayTrie::new();
    let dict2: DoubleArrayTrie = DoubleArrayTrie::new();

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    assert_eq!(union.iter().count(), 0);
}

#[test]
fn test_union_first_empty() {
    let dict1: DoubleArrayTrie = DoubleArrayTrie::new();
    let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["cat", "dog"]);
}

#[test]
fn test_union_second_empty() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    let dict2: DoubleArrayTrie = DoubleArrayTrie::new();

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["cat", "dog"]);
}

// ============================================================================
// Navigation Tests
// ============================================================================

#[test]
fn test_union_descend_exists_in_both() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "car"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "cab"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    // Descend to 'c' -> 'a'
    let ca = union
        .descend(b'c')
        .and_then(|z| z.descend(b'a'))
        .expect("Should find 'ca'");

    assert_eq!(ca.active_dictionary_count(), 2);

    let mut children: Vec<u8> = ca.children().map(|(label, _)| label).collect();
    children.sort();

    // Should see children from both dicts: t (cat), r (car), b (cab)
    assert_eq!(children, vec![b'b', b'r', b't']);
}

#[test]
fn test_union_descend_exists_in_one() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["apple"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["banana"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    // Descend to 'a' - only in dict1
    let a = union.descend(b'a').expect("Should find 'a'");
    assert_eq!(a.active_dictionary_count(), 1);

    // Descend to 'b' - only in dict2
    let b = union.descend(b'b').expect("Should find 'b'");
    assert_eq!(b.active_dictionary_count(), 1);
}

#[test]
fn test_union_descend_nonexistent() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["cat"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["dog"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    // 'x' doesn't exist in either
    assert!(union.descend(b'x').is_none());
}

#[test]
fn test_union_is_final() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["cat"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["cat", "cats"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    // Navigate to "cat"
    let cat = union
        .descend(b'c')
        .and_then(|z| z.descend(b'a'))
        .and_then(|z| z.descend(b't'))
        .expect("Should find 'cat'");

    assert!(cat.is_final()); // "cat" is final in both dicts
    assert_eq!(cat.path(), b"cat".to_vec());

    // Continue to "cats"
    let cats = cat.descend(b's').expect("Should find 'cats'");
    assert!(cats.is_final()); // "cats" is final (only in dict2, but that's enough)
}

#[test]
fn test_union_path_tracking() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["hello"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["world"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    // Root has empty path
    assert_eq!(union.path(), Vec::<u8>::new());

    // Navigate to "hel"
    let hel = union
        .descend(b'h')
        .and_then(|z| z.descend(b'e'))
        .and_then(|z| z.descend(b'l'))
        .expect("Should find 'hel'");

    assert_eq!(hel.path(), b"hel".to_vec());
}

// ============================================================================
// Valued Dictionary Tests
// ============================================================================

#[test]
fn test_valued_union_first_wins() {
    let dict1 =
        DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize), ("dog", 2)].into_iter());
    let dict2 =
        DoubleArrayTrie::from_terms_with_values(vec![("cat", 10usize), ("fish", 3)].into_iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = UnionZipper::new(vec![z1, z2]);

    // "cat" exists in both - FirstWins should return 1
    let cat = union
        .descend(b'c')
        .and_then(|z| z.descend(b'a'))
        .and_then(|z| z.descend(b't'))
        .expect("Should find 'cat'");

    assert_eq!(cat.value(), Some(1));

    // "fish" exists only in dict2 - should return 3
    let fish = union
        .descend(b'f')
        .and_then(|z| z.descend(b'i'))
        .and_then(|z| z.descend(b's'))
        .and_then(|z| z.descend(b'h'))
        .expect("Should find 'fish'");

    assert_eq!(fish.value(), Some(3));
}

#[test]
fn test_valued_union_last_wins() {
    let dict1 =
        DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize), ("dog", 2)].into_iter());
    let dict2 =
        DoubleArrayTrie::from_terms_with_values(vec![("cat", 10usize), ("fish", 3)].into_iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = UnionZipper::with_strategy(vec![z1, z2], LastWins);

    // "cat" exists in both - LastWins should return 10
    let cat = union
        .descend(b'c')
        .and_then(|z| z.descend(b'a'))
        .and_then(|z| z.descend(b't'))
        .expect("Should find 'cat'");

    assert_eq!(cat.value(), Some(10));
}

#[test]
fn test_valued_union_custom_strategy() {
    #[derive(Clone)]
    struct Sum;

    impl ValueMergeStrategy<usize> for Sum {
        fn merge(&self, existing: usize, new: usize) -> usize {
            existing + new
        }
    }

    let dict1 = DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize)].into_iter());
    let dict2 = DoubleArrayTrie::from_terms_with_values(vec![("cat", 10usize)].into_iter());
    let dict3 = DoubleArrayTrie::from_terms_with_values(vec![("cat", 100usize)].into_iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

    let union = UnionZipper::with_strategy(vec![z1, z2, z3], Sum);

    let cat = union
        .descend(b'c')
        .and_then(|z| z.descend(b'a'))
        .and_then(|z| z.descend(b't'))
        .expect("Should find 'cat'");

    // Sum: 1 + 10 + 100 = 111
    assert_eq!(cat.value(), Some(111));
}

#[test]
fn test_valued_union_iterator() {
    let dict1 =
        DoubleArrayTrie::from_terms_with_values(vec![("cat", 1usize), ("dog", 2)].into_iter());
    let dict2 =
        DoubleArrayTrie::from_terms_with_values(vec![("cat", 10usize), ("fish", 3)].into_iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = UnionZipper::new(vec![z1, z2]);
    let valued_iter = ValuedUnionIterator::new(union);

    let mut results: Vec<(String, usize)> = valued_iter
        .map(|(path, val)| (String::from_utf8(path).unwrap(), val))
        .collect();

    results.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(
        results,
        vec![
            ("cat".to_string(), 1), // FirstWins
            ("dog".to_string(), 2),
            ("fish".to_string(), 3),
        ]
    );
}

// ============================================================================
// Unicode (Char) Dictionary Tests
// ============================================================================

#[test]
fn test_union_char_dictionaries() {
    let dict1 = DoubleArrayTrieChar::from_terms(vec!["café", "naïve"].iter());
    let dict2 = DoubleArrayTrieChar::from_terms(vec!["résumé", "naïveté"].iter());

    let z1 = DoubleArrayTrieCharZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieCharZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| path.iter().collect())
            .collect(),
    );

    assert_eq!(results, vec!["café", "naïve", "naïveté", "résumé"]);
}

#[test]
fn test_union_char_overlap() {
    let dict1 = DoubleArrayTrieChar::from_terms(vec!["café", "naïve"].iter());
    let dict2 = DoubleArrayTrieChar::from_terms(vec!["café", "naïveté"].iter());

    let z1 = DoubleArrayTrieCharZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieCharZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| path.iter().collect())
            .collect(),
    );

    // "café" appears in both but should only appear once
    assert_eq!(results, vec!["café", "naïve", "naïveté"]);
}

#[test]
fn test_union_char_navigation() {
    let dict1 = DoubleArrayTrieChar::from_terms(vec!["中国"].iter());
    let dict2 = DoubleArrayTrieChar::from_terms(vec!["中文"].iter());

    let z1 = DoubleArrayTrieCharZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieCharZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    // Navigate to '中'
    let zhong = union.descend('中').expect("Should find '中'");

    let mut children: Vec<char> = zhong.children().map(|(label, _)| label).collect();
    children.sort();

    // Should have '国' from dict1 and '文' from dict2
    assert_eq!(children, vec!['国', '文']);
}

// ============================================================================
// DynamicDawg Backend Tests
// ============================================================================

#[test]
fn test_union_dawg_basic() {
    let dict1: DynamicDawg<()> = DynamicDawg::from_terms(vec!["cat", "dog"].iter());
    let dict2: DynamicDawg<()> = DynamicDawg::from_terms(vec!["fish", "bird"].iter());

    let z1 = DynamicDawgZipper::new_from_dict(&dict1);
    let z2 = DynamicDawgZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["bird", "cat", "dog", "fish"]);
}

#[test]
fn test_union_dawg_with_overlap() {
    let dict1: DynamicDawg<()> = DynamicDawg::from_terms(vec!["cat", "dog"].iter());
    let dict2: DynamicDawg<()> = DynamicDawg::from_terms(vec!["cat", "fish"].iter());

    let z1 = DynamicDawgZipper::new_from_dict(&dict1);
    let z2 = DynamicDawgZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    // "cat" only once
    assert_eq!(results, vec!["cat", "dog", "fish"]);
}

#[test]
fn test_union_dawg_char() {
    let dict1: DynamicDawgChar<()> = DynamicDawgChar::from_terms(vec!["café", "naïve"].iter());
    let dict2: DynamicDawgChar<()> = DynamicDawgChar::from_terms(vec!["résumé"].iter());

    let z1 = DynamicDawgCharZipper::new_from_dict(&dict1);
    let z2 = DynamicDawgCharZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| path.iter().collect())
            .collect(),
    );

    assert_eq!(results, vec!["café", "naïve", "résumé"]);
}

// ============================================================================
// Cross-Backend Consistency Tests
// ============================================================================

#[test]
fn test_consistency_dat_dawg() {
    let terms1 = vec!["apple", "application"];
    let terms2 = vec!["apple", "banana"];

    // DoubleArrayTrie
    let dat1 = DoubleArrayTrie::from_terms(terms1.iter());
    let dat2 = DoubleArrayTrie::from_terms(terms2.iter());
    let dat_z1 = DoubleArrayTrieZipper::new_from_dict(&dat1);
    let dat_z2 = DoubleArrayTrieZipper::new_from_dict(&dat2);
    let dat_union = dat_z1.union_with(dat_z2);

    // DynamicDawg
    let dawg1: DynamicDawg<()> = DynamicDawg::from_terms(terms1.iter());
    let dawg2: DynamicDawg<()> = DynamicDawg::from_terms(terms2.iter());
    let dawg_z1 = DynamicDawgZipper::new_from_dict(&dawg1);
    let dawg_z2 = DynamicDawgZipper::new_from_dict(&dawg2);
    let dawg_union = dawg_z1.union_with(dawg_z2);

    // Results should be the same
    let dat_results: Vec<String> = sorted_results(
        dat_union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    let dawg_results: Vec<String> = sorted_results(
        dawg_union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(dat_results, dawg_results);
    assert_eq!(dat_results, vec!["apple", "application", "banana"]);
}

// ============================================================================
// Composability with PrefixZipper Tests
// ============================================================================

#[test]
fn test_union_with_prefix_zipper() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["process", "produce"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["product", "program"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    // Use PrefixZipper on the union
    let results: Vec<String> = sorted_results(
        union
            .with_prefix(b"pro")
            .unwrap()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["process", "produce", "product", "program"]);
}

#[test]
fn test_union_with_prefix_zipper_partial_match() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["apple", "application"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["banana", "band"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    // Prefix "app" only matches terms in dict1
    let results: Vec<String> = sorted_results(
        union
            .with_prefix(b"app")
            .unwrap()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["apple", "application"]);

    // Prefix "ban" only matches terms in dict2
    let results: Vec<String> = sorted_results(
        union
            .with_prefix(b"ban")
            .unwrap()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["banana", "band"]);
}

#[test]
fn test_union_with_prefix_zipper_nonexistent() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["fish", "bird"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    // Prefix "xyz" doesn't exist in either
    assert!(union.with_prefix(b"xyz").is_none());
}

#[test]
fn test_union_char_with_prefix_zipper() {
    let dict1 = DoubleArrayTrieChar::from_terms(vec!["café", "cafétéria"].iter());
    let dict2 = DoubleArrayTrieChar::from_terms(vec!["cafard"].iter());

    let z1 = DoubleArrayTrieCharZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieCharZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let prefix: Vec<char> = "caf".chars().collect();
    let results: Vec<String> = sorted_results(
        union
            .with_prefix(&prefix)
            .unwrap()
            .map(|(path, _)| path.iter().collect())
            .collect(),
    );

    assert_eq!(results, vec!["cafard", "café", "cafétéria"]);
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn test_union_single_dictionary() {
    let dict = DoubleArrayTrie::from_terms(vec!["cat", "dog"].iter());
    let z = DoubleArrayTrieZipper::new_from_dict(&dict);

    let union = UnionZipper::new(vec![z]);

    assert_eq!(union.dictionary_count(), 1);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["cat", "dog"]);
}

#[test]
fn test_union_very_long_terms() {
    let long_term1 = "a".repeat(100);
    let long_term2 = "b".repeat(100);

    let dict1 = DoubleArrayTrie::from_terms(vec![long_term1.as_str()].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec![long_term2.as_str()].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec![long_term1, long_term2]);
}

#[test]
fn test_union_single_character_terms() {
    let dict1 = DoubleArrayTrie::from_terms(vec!["a", "b"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["b", "c"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["a", "b", "c"]);
}

#[test]
fn test_union_prefix_overlap() {
    // Terms where one is a prefix of another, across dictionaries
    let dict1 = DoubleArrayTrie::from_terms(vec!["cat"].iter());
    let dict2 = DoubleArrayTrie::from_terms(vec!["cats", "catsup"].iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    let results: Vec<String> = sorted_results(
        union
            .iter()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["cat", "cats", "catsup"]);
}

// ============================================================================
// Large Dictionary Tests
// ============================================================================

#[test]
fn test_union_large_dictionaries() {
    // Create dictionaries with many terms
    let terms1: Vec<String> = (0..500).map(|i| format!("word{}", i)).collect();
    let terms2: Vec<String> = (250..750).map(|i| format!("word{}", i)).collect();

    let dict1 = DoubleArrayTrie::from_terms(terms1.iter());
    let dict2 = DoubleArrayTrie::from_terms(terms2.iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = z1.union_with(z2);

    // Should have 750 unique terms (0..500 + 500..750)
    let count = union.iter().count();
    assert_eq!(count, 750);
}

#[test]
fn test_union_count_with_prefix() {
    let terms1: Vec<String> = (0..100).map(|i| format!("prefix{}", i)).collect();
    let terms2: Vec<String> = (0..100).map(|i| format!("other{}", i)).collect();
    let terms3: Vec<String> = (50..150).map(|i| format!("prefix{}", i)).collect();

    let dict1 = DoubleArrayTrie::from_terms(terms1.iter());
    let dict2 = DoubleArrayTrie::from_terms(terms2.iter());
    let dict3 = DoubleArrayTrie::from_terms(terms3.iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

    let union = z1.union_all(vec![z2, z3]);

    // Prefix "prefix" should match 150 unique terms (0..150)
    let prefix_count = union.with_prefix(b"prefix").unwrap().count();
    assert_eq!(prefix_count, 150);

    // Prefix "other" should match 100 unique terms
    let other_count = union.with_prefix(b"other").unwrap().count();
    assert_eq!(other_count, 100);
}

// ============================================================================
// Lattice Strategy Integration Tests
// ============================================================================

#[test]
fn test_lattice_join_hashset_integration() {
    // Test HashSet union semantics with real dictionaries
    let dict1 = DoubleArrayTrie::from_terms_with_values(
        vec![
            ("println", HashSet::from([1, 2])),
            ("eprintln", HashSet::from([1])),
        ]
        .into_iter(),
    );
    let dict2 = DoubleArrayTrie::from_terms_with_values(
        vec![
            ("println", HashSet::from([2, 3])),
            ("format", HashSet::from([1, 2, 3])),
        ]
        .into_iter(),
    );

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = UnionZipper::with_strategy(vec![z1, z2], LatticeJoin);

    // Check "println" - should be union of {1,2} and {2,3}
    let println = union
        .descend(b'p')
        .and_then(|z| z.descend(b'r'))
        .and_then(|z| z.descend(b'i'))
        .and_then(|z| z.descend(b'n'))
        .and_then(|z| z.descend(b't'))
        .and_then(|z| z.descend(b'l'))
        .and_then(|z| z.descend(b'n'))
        .expect("Should find 'println'");

    assert_eq!(println.value(), Some(HashSet::from([1, 2, 3])));

    // Check "eprintln" - only in dict1
    let eprintln = union
        .descend(b'e')
        .and_then(|z| z.descend(b'p'))
        .and_then(|z| z.descend(b'r'))
        .and_then(|z| z.descend(b'i'))
        .and_then(|z| z.descend(b'n'))
        .and_then(|z| z.descend(b't'))
        .and_then(|z| z.descend(b'l'))
        .and_then(|z| z.descend(b'n'))
        .expect("Should find 'eprintln'");

    assert_eq!(eprintln.value(), Some(HashSet::from([1])));
}

#[test]
fn test_lattice_meet_hashset_integration() {
    // Test HashSet intersection semantics with real dictionaries
    let dict1 = DoubleArrayTrie::from_terms_with_values(
        vec![("shared", HashSet::from([1, 2, 3, 4]))].into_iter(),
    );
    let dict2 = DoubleArrayTrie::from_terms_with_values(
        vec![("shared", HashSet::from([2, 3, 4, 5]))].into_iter(),
    );
    let dict3 = DoubleArrayTrie::from_terms_with_values(
        vec![("shared", HashSet::from([3, 4, 5, 6]))].into_iter(),
    );

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);
    let z3 = DoubleArrayTrieZipper::new_from_dict(&dict3);

    let union = UnionZipper::with_strategy(vec![z1, z2, z3], LatticeMeet);

    let shared = union
        .descend(b's')
        .and_then(|z| z.descend(b'h'))
        .and_then(|z| z.descend(b'a'))
        .and_then(|z| z.descend(b'r'))
        .and_then(|z| z.descend(b'e'))
        .and_then(|z| z.descend(b'd'))
        .expect("Should find 'shared'");

    // Intersection: {1,2,3,4} ∩ {2,3,4,5} ∩ {3,4,5,6} = {3, 4}
    assert_eq!(shared.value(), Some(HashSet::from([3, 4])));
}

#[test]
fn test_lattice_join_numeric_max() {
    // Test numeric max semantics (priority/score scenarios)
    let dict1 = DoubleArrayTrie::from_terms_with_values(
        vec![("priority", 100u32), ("score", 85u32)].into_iter(),
    );
    let dict2 = DoubleArrayTrie::from_terms_with_values(
        vec![("priority", 50u32), ("score", 92u32)].into_iter(),
    );

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = UnionZipper::with_strategy(vec![z1, z2], LatticeJoin);

    // Iterate and collect values
    let mut results: Vec<(String, u32)> = union
        .iter()
        .map(|(path, z)| (String::from_utf8(path).unwrap(), z.value().unwrap()))
        .collect();
    results.sort_by(|a, b| a.0.cmp(&b.0));

    // LatticeJoin = max for numeric
    assert_eq!(
        results,
        vec![
            ("priority".to_string(), 100), // max(100, 50)
            ("score".to_string(), 92),     // max(85, 92)
        ]
    );
}

#[test]
fn test_lattice_meet_numeric_min() {
    // Test numeric min semantics
    let dict1 = DoubleArrayTrie::from_terms_with_values(
        vec![("min_price", 100u32), ("min_qty", 5u32)].into_iter(),
    );
    let dict2 = DoubleArrayTrie::from_terms_with_values(
        vec![("min_price", 80u32), ("min_qty", 10u32)].into_iter(),
    );

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = UnionZipper::with_strategy(vec![z1, z2], LatticeMeet);

    // Check specific values
    let min_price = union
        .descend(b'm')
        .and_then(|z| z.descend(b'i'))
        .and_then(|z| z.descend(b'n'))
        .and_then(|z| z.descend(b'_'))
        .and_then(|z| z.descend(b'p'))
        .and_then(|z| z.descend(b'r'))
        .and_then(|z| z.descend(b'i'))
        .and_then(|z| z.descend(b'c'))
        .and_then(|z| z.descend(b'e'))
        .expect("Should find 'min_price'");

    // LatticeMeet = min for numeric
    assert_eq!(min_price.value(), Some(80)); // min(100, 80)
}

#[test]
fn test_lattice_join_bool_or() {
    // Test boolean OR semantics (feature flags)
    let dict1 = DoubleArrayTrie::from_terms_with_values(
        vec![("feature_a", true), ("feature_b", false)].into_iter(),
    );
    let dict2 = DoubleArrayTrie::from_terms_with_values(
        vec![("feature_a", false), ("feature_b", true)].into_iter(),
    );

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = UnionZipper::with_strategy(vec![z1, z2], LatticeJoin);

    // Iterate and collect values
    let mut results: Vec<(String, bool)> = union
        .iter()
        .map(|(path, z)| (String::from_utf8(path).unwrap(), z.value().unwrap()))
        .collect();
    results.sort_by(|a, b| a.0.cmp(&b.0));

    // LatticeJoin = OR for bool
    assert_eq!(
        results,
        vec![
            ("feature_a".to_string(), true), // true OR false
            ("feature_b".to_string(), true), // false OR true
        ]
    );
}

#[test]
fn test_lattice_meet_bool_and() {
    // Test boolean AND semantics (required permissions)
    let dict1 = DoubleArrayTrie::from_terms_with_values(
        vec![("read", true), ("write", true), ("admin", false)].into_iter(),
    );
    let dict2 = DoubleArrayTrie::from_terms_with_values(
        vec![("read", true), ("write", false), ("admin", false)].into_iter(),
    );

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = UnionZipper::with_strategy(vec![z1, z2], LatticeMeet);

    // Iterate and collect values
    let mut results: Vec<(String, bool)> = union
        .iter()
        .map(|(path, z)| (String::from_utf8(path).unwrap(), z.value().unwrap()))
        .collect();
    results.sort_by(|a, b| a.0.cmp(&b.0));

    // LatticeMeet = AND for bool
    assert_eq!(
        results,
        vec![
            ("admin".to_string(), false), // false AND false
            ("read".to_string(), true),   // true AND true
            ("write".to_string(), false), // true AND false
        ]
    );
}

#[test]
fn test_lattice_join_with_prefix_zipper() {
    // Test composition with PrefixZipper
    let dict1 = DoubleArrayTrie::from_terms_with_values(
        vec![
            ("println", HashSet::from([1, 2])),
            ("print", HashSet::from([1])),
            ("printf", HashSet::from([3])),
        ]
        .into_iter(),
    );
    let dict2 = DoubleArrayTrie::from_terms_with_values(
        vec![
            ("println", HashSet::from([2, 3])),
            ("printk", HashSet::from([4])),
        ]
        .into_iter(),
    );

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = UnionZipper::with_strategy(vec![z1, z2], LatticeJoin);

    // Use PrefixZipper to get all terms starting with "print"
    let prefix_iter = union.with_prefix(b"print").expect("Prefix should exist");

    let mut results: Vec<(String, HashSet<i32>)> = prefix_iter
        .map(|(path, z)| (String::from_utf8(path).unwrap(), z.value().unwrap()))
        .collect();
    results.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(results.len(), 4);

    // Check specific merged values
    let println_result = results.iter().find(|(k, _)| k == "println").unwrap();
    assert_eq!(println_result.1, HashSet::from([1, 2, 3]));

    let print_result = results.iter().find(|(k, _)| k == "print").unwrap();
    assert_eq!(print_result.1, HashSet::from([1]));
}

#[test]
fn test_lattice_valued_iterator() {
    // Test ValuedUnionIterator with LatticeJoin
    let dict1 =
        DoubleArrayTrie::from_terms_with_values(vec![("a", 10u32), ("b", 20u32)].into_iter());
    let dict2 =
        DoubleArrayTrie::from_terms_with_values(vec![("a", 15u32), ("c", 30u32)].into_iter());

    let z1 = DoubleArrayTrieZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieZipper::new_from_dict(&dict2);

    let union = UnionZipper::with_strategy(vec![z1, z2], LatticeJoin);
    let valued_iter = ValuedUnionIterator::new(union);

    let mut results: Vec<(String, u32)> = valued_iter
        .map(|(path, val)| (String::from_utf8(path).unwrap(), val))
        .collect();
    results.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(
        results,
        vec![
            ("a".to_string(), 15), // max(10, 15)
            ("b".to_string(), 20),
            ("c".to_string(), 30),
        ]
    );
}

#[test]
fn test_lattice_char_dictionary() {
    // Test Lattice with char dictionaries (unicode)
    // Note: We use String instead of &str because when persistent-artrie is enabled,
    // DictionaryValue requires Serialize + DeserializeOwned, which &str doesn't implement.
    let dict1 = DoubleArrayTrieChar::from_terms_with_values(
        vec![("日本", HashSet::from(["ja".to_string()]))].into_iter(),
    );
    let dict2 = DoubleArrayTrieChar::from_terms_with_values(
        vec![("日本", HashSet::from(["jp".to_string(), "jpn".to_string()]))].into_iter(),
    );

    let z1 = DoubleArrayTrieCharZipper::new_from_dict(&dict1);
    let z2 = DoubleArrayTrieCharZipper::new_from_dict(&dict2);

    let union = UnionZipper::with_strategy(vec![z1, z2], LatticeJoin);

    let japan = union
        .descend('日')
        .and_then(|z| z.descend('本'))
        .expect("Should find '日本'");

    // LatticeJoin: {"ja"} ∪ {"jp", "jpn"} = {"ja", "jp", "jpn"}
    assert_eq!(japan.value(), Some(HashSet::from(["ja".to_string(), "jp".to_string(), "jpn".to_string()])));
}

#[test]
fn test_lattice_dawg_backend() {
    // Test Lattice with DAWG backend
    let dict1: DynamicDawg<HashSet<i32>> = DynamicDawg::new();
    dict1.insert_with_value("merge", HashSet::from([1, 2]));

    let dict2: DynamicDawg<HashSet<i32>> = DynamicDawg::new();
    dict2.insert_with_value("merge", HashSet::from([2, 3]));

    let z1 = DynamicDawgZipper::new_from_dict(&dict1);
    let z2 = DynamicDawgZipper::new_from_dict(&dict2);

    let union = UnionZipper::with_strategy(vec![z1, z2], LatticeJoin);

    let merge = union
        .descend(b'm')
        .and_then(|z| z.descend(b'e'))
        .and_then(|z| z.descend(b'r'))
        .and_then(|z| z.descend(b'g'))
        .and_then(|z| z.descend(b'e'))
        .expect("Should find 'merge'");

    assert_eq!(merge.value(), Some(HashSet::from([1, 2, 3])));
}
