//! Comprehensive tests for ExcludingPrefixZipper functionality across all dictionary backends.
//!
//! This test suite validates:
//! - Single and multiple prefix exclusion
//! - Empty exclusion list behavior
//! - Combined prefix navigation with exclusion
//! - Valued dictionary support
//! - Unicode support (character-level variants)
//! - Edge cases (empty prefix, exact matches, all excluded)
//! - Consistency across backends (DAT, DAWG)

use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::double_array_trie_char::DoubleArrayTrieChar;
use libdictenstein::double_array_trie_char_zipper::DoubleArrayTrieCharZipper;
use libdictenstein::double_array_trie_zipper::DoubleArrayTrieZipper;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::dynamic_dawg_char::DynamicDawgChar;
use libdictenstein::dynamic_dawg_char_zipper::DynamicDawgCharZipper;
use libdictenstein::dynamic_dawg_zipper::DynamicDawgZipper;
use libdictenstein::excluding_prefix_zipper::{
    ExcludingPrefixZipper, ValuedExcludingPrefixZipper,
};

// ============================================================================
// Helper Functions
// ============================================================================

/// Sort results for consistent comparison
fn sorted_results<T: Ord>(mut results: Vec<T>) -> Vec<T> {
    results.sort();
    results
}

// ============================================================================
// Basic Exclusion Tests - DoubleArrayTrie
// ============================================================================

#[test]
fn test_dat_single_exclusion_null_prefix() {
    let terms = vec!["\x00meta", "\x00index", "hello", "world"];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"\x00"];
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["hello", "world"]);
}

#[test]
fn test_dat_single_exclusion_underscore_prefix() {
    let terms = vec!["_private", "_internal", "public", "visible"];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"_"];
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["public", "visible"]);
}

#[test]
fn test_dat_multiple_exclusions() {
    let terms = vec![
        "_private",
        "_internal",
        ".hidden",
        ".dotfile",
        "public",
        "visible",
    ];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"_", b"."];
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["public", "visible"]);
}

#[test]
fn test_dat_empty_exclusion_returns_all() {
    let terms = vec!["cat", "dog", "fish"];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[];
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["cat", "dog", "fish"]);
}

#[test]
fn test_dat_all_excluded_returns_empty() {
    let terms = vec!["\x00meta", "\x00index", "\x00data"];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"\x00"];
    let results: Vec<String> = zipper
        .iter_excluding(excluded)
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    assert!(results.is_empty());
}

#[test]
fn test_dat_exclusion_with_partial_overlap() {
    // Test where excluded prefix is a prefix of some terms but not others
    let terms = vec!["api", "api_v1", "api_internal", "application", "web"];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"api_i"]; // Only exclude "api_internal"
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["api", "api_v1", "application", "web"]);
}

// ============================================================================
// Combined Prefix + Exclusion Tests - DoubleArrayTrie
// ============================================================================

#[test]
fn test_dat_with_prefix_excluding() {
    let terms = vec!["api_v1", "api_v2", "api__internal", "api__debug", "web_v1"];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"api__"];
    let results: Vec<String> = sorted_results(
        zipper
            .with_prefix_excluding(b"api_", excluded)
            .unwrap()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["api_v1", "api_v2"]);
}

#[test]
fn test_dat_with_prefix_excluding_nonexistent_prefix() {
    let terms = vec!["hello", "world"];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"\x00"];
    let result = zipper.with_prefix_excluding(b"xyz", excluded);

    assert!(result.is_none());
}

#[test]
fn test_dat_with_prefix_excluding_all_under_prefix() {
    let terms = vec!["api__a", "api__b", "web_v1"];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"api__"];
    let results: Vec<String> = zipper
        .with_prefix_excluding(b"api__", excluded)
        .unwrap()
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    // All terms under "api__" are excluded
    assert!(results.is_empty());
}

// ============================================================================
// Valued Dictionary Tests - DoubleArrayTrie
// ============================================================================

#[test]
fn test_dat_valued_excluding_iterator() {
    let terms_with_values = vec![
        ("cat", 1usize),
        ("cats", 2),
        ("\x00meta", 99),
        ("\x00index", 100),
        ("dog", 3),
    ];
    let dict = DoubleArrayTrie::from_terms_with_values(terms_with_values.into_iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"\x00"];
    let results: Vec<(String, usize)> = sorted_results(
        zipper
            .iter_values_excluding(excluded)
            .map(|(path, val)| (String::from_utf8(path).unwrap(), val))
            .collect(),
    );

    assert_eq!(
        results,
        vec![
            ("cat".to_string(), 1),
            ("cats".to_string(), 2),
            ("dog".to_string(), 3),
        ]
    );
}

#[test]
fn test_dat_valued_with_prefix_excluding() {
    let terms_with_values = vec![
        ("api_v1", 1usize),
        ("api_v2", 2),
        ("api__internal", 99),
        ("web_v1", 3),
    ];
    let dict = DoubleArrayTrie::from_terms_with_values(terms_with_values.into_iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"api__"];
    let results: Vec<(String, usize)> = sorted_results(
        zipper
            .with_prefix_values_excluding(b"api_", excluded)
            .unwrap()
            .map(|(path, val)| (String::from_utf8(path).unwrap(), val))
            .collect(),
    );

    assert_eq!(
        results,
        vec![("api_v1".to_string(), 1), ("api_v2".to_string(), 2),]
    );
}

// ============================================================================
// Basic Exclusion Tests - DynamicDawg
// ============================================================================

#[test]
fn test_dawg_single_exclusion() {
    let terms = vec!["\x00meta", "\x00index", "hello", "world"];
    let dict: DynamicDawg<()> = DynamicDawg::from_terms(terms.iter());

    let zipper = DynamicDawgZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"\x00"];
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["hello", "world"]);
}

#[test]
fn test_dawg_multiple_exclusions() {
    let terms = vec!["_private", ".hidden", "public"];
    let dict: DynamicDawg<()> = DynamicDawg::from_terms(terms.iter());

    let zipper = DynamicDawgZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"_", b"."];
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["public"]);
}

#[test]
fn test_dawg_empty_exclusion_returns_all() {
    let terms = vec!["a", "b", "c"];
    let dict: DynamicDawg<()> = DynamicDawg::from_terms(terms.iter());

    let zipper = DynamicDawgZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[];
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["a", "b", "c"]);
}

#[test]
fn test_dawg_with_prefix_excluding() {
    let terms = vec!["api_v1", "api_v2", "api__internal", "web_v1"];
    let dict: DynamicDawg<()> = DynamicDawg::from_terms(terms.iter());

    let zipper = DynamicDawgZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"api__"];
    let results: Vec<String> = sorted_results(
        zipper
            .with_prefix_excluding(b"api_", excluded)
            .unwrap()
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["api_v1", "api_v2"]);
}

// ============================================================================
// Character-Level Tests - DoubleArrayTrieChar
// ============================================================================

#[test]
fn test_dat_char_exclusion_unicode() {
    let terms = vec!["_private", "_internal", "café", "naïve"];
    let dict = DoubleArrayTrieChar::from_terms(terms.iter());

    let zipper = DoubleArrayTrieCharZipper::new_from_dict(&dict);
    let excluded: &[Vec<char>] = &[vec!['_']];
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| path.iter().collect())
            .collect(),
    );

    assert_eq!(results, vec!["café", "naïve"]);
}

#[test]
fn test_dat_char_exclusion_emoji() {
    let terms = vec!["🔒private", "🔒hidden", "🌍public", "visible"];
    let dict = DoubleArrayTrieChar::from_terms(terms.iter());

    let zipper = DoubleArrayTrieCharZipper::new_from_dict(&dict);
    let excluded: &[Vec<char>] = &[vec!['🔒']];
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| path.iter().collect())
            .collect(),
    );

    assert_eq!(results, vec!["visible", "🌍public"]);
}

#[test]
fn test_dat_char_with_prefix_excluding() {
    let terms = vec!["api_v1", "api_v2", "api__内部", "web_v1"];
    let dict = DoubleArrayTrieChar::from_terms(terms.iter());

    let zipper = DoubleArrayTrieCharZipper::new_from_dict(&dict);
    let prefix: Vec<char> = "api_".chars().collect();
    let excluded: &[Vec<char>] = &["api__".chars().collect()];
    let results: Vec<String> = sorted_results(
        zipper
            .with_prefix_excluding(&prefix, excluded)
            .unwrap()
            .map(|(path, _)| path.iter().collect())
            .collect(),
    );

    assert_eq!(results, vec!["api_v1", "api_v2"]);
}

// ============================================================================
// Character-Level Tests - DynamicDawgChar
// ============================================================================

#[test]
fn test_dawg_char_exclusion() {
    let terms = vec!["_private", "公開", "表示"];
    let dict: DynamicDawgChar<()> = DynamicDawgChar::from_terms(terms.iter());

    let zipper = DynamicDawgCharZipper::new_from_dict(&dict);
    let excluded: &[Vec<char>] = &[vec!['_']];
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| path.iter().collect())
            .collect(),
    );

    assert_eq!(results, vec!["公開", "表示"]);
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn test_empty_dictionary() {
    let dict: DoubleArrayTrie = DoubleArrayTrie::from_terms(Vec::<&str>::new().iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"\x00"];
    let results: Vec<String> = zipper
        .iter_excluding(excluded)
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    assert!(results.is_empty());
}

#[test]
fn test_single_character_terms() {
    let terms = vec!["a", "b", "\x00"];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"\x00"];
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(results, vec!["a", "b"]);
}

#[test]
fn test_long_excluded_prefix() {
    let prefix = "a".repeat(50);
    let term1 = format!("{}hidden", prefix);
    let term2 = "visible".to_string();

    let terms: Vec<&str> = vec![term1.as_str(), term2.as_str()];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: Vec<Vec<u8>> = vec![prefix.as_bytes().to_vec()];
    let excluded_refs: Vec<&[u8]> = excluded.iter().map(|v| v.as_slice()).collect();
    let results: Vec<String> = zipper
        .iter_excluding(&excluded_refs)
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    assert_eq!(results, vec!["visible"]);
}

#[test]
fn test_exclusion_prefix_not_in_dictionary() {
    let terms = vec!["hello", "world"];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"\x00", b"_", b"."]; // None of these exist
    let results: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    // All terms should still be returned
    assert_eq!(results, vec!["hello", "world"]);
}

#[test]
fn test_overlapping_exclusions() {
    // Test with overlapping exclusion patterns
    let terms = vec!["api", "api_v1", "api__internal"];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    // Both prefixes overlap: "api_" and "api__"
    let excluded: &[&[u8]] = &[b"api_", b"api__"];
    let results: Vec<String> = zipper
        .iter_excluding(excluded)
        .map(|(path, _)| String::from_utf8(path).unwrap())
        .collect();

    // Only "api" should remain (both "api_v1" and "api__internal" start with "api_")
    assert_eq!(results, vec!["api"]);
}

// ============================================================================
// Consistency Tests - Same Results Across Backends
// ============================================================================

#[test]
fn test_consistency_dat_vs_dawg() {
    let terms = vec!["\x00meta", "_private", "apple", "banana", "cherry"];

    let dat = DoubleArrayTrie::from_terms(terms.iter());
    let dawg: DynamicDawg<()> = DynamicDawg::from_terms(terms.iter());

    let excluded: &[&[u8]] = &[b"\x00", b"_"];

    let dat_results: Vec<String> = sorted_results(
        DoubleArrayTrieZipper::new_from_dict(&dat)
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    let dawg_results: Vec<String> = sorted_results(
        DynamicDawgZipper::new_from_dict(&dawg)
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(dat_results, dawg_results);
    assert_eq!(dat_results, vec!["apple", "banana", "cherry"]);
}

#[test]
fn test_consistency_dat_char_vs_dawg_char() {
    let terms = vec!["_private", "café", "naïve", "日本語"];

    let dat_char = DoubleArrayTrieChar::from_terms(terms.iter());
    let dawg_char: DynamicDawgChar<()> = DynamicDawgChar::from_terms(terms.iter());

    let excluded: &[Vec<char>] = &[vec!['_']];

    let dat_results: Vec<String> = sorted_results(
        DoubleArrayTrieCharZipper::new_from_dict(&dat_char)
            .iter_excluding(excluded)
            .map(|(path, _)| path.iter().collect())
            .collect(),
    );

    let dawg_results: Vec<String> = sorted_results(
        DynamicDawgCharZipper::new_from_dict(&dawg_char)
            .iter_excluding(excluded)
            .map(|(path, _)| path.iter().collect())
            .collect(),
    );

    assert_eq!(dat_results, dawg_results);
    assert_eq!(dat_results, vec!["café", "naïve", "日本語"]);
}

// ============================================================================
// Practical Use Case Tests
// ============================================================================

#[test]
fn test_metadata_exclusion_use_case() {
    // Simulate a dictionary with metadata entries prefixed by \x00
    let terms = vec![
        "\x00__schema_version",
        "\x00__created_at",
        "\x00__node_count",
        "user:alice",
        "user:bob",
        "item:apple",
        "item:banana",
    ];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"\x00"];

    // Get all user-visible entries
    let visible: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(
        visible,
        vec!["item:apple", "item:banana", "user:alice", "user:bob"]
    );
}

#[test]
fn test_internal_api_exclusion() {
    // Simulate an API where internal endpoints are prefixed with __
    let terms = vec![
        "api/v1/users",
        "api/v1/items",
        "api/__internal/debug",
        "api/__internal/metrics",
        "api/__internal/health",
    ];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"api/__internal"];

    // Get only public API endpoints
    let public_endpoints: Vec<String> = sorted_results(
        zipper
            .iter_excluding(excluded)
            .map(|(path, _)| String::from_utf8(path).unwrap())
            .collect(),
    );

    assert_eq!(public_endpoints, vec!["api/v1/items", "api/v1/users"]);
}

#[test]
fn test_count_excluding() {
    let terms = vec!["\x00a", "\x00b", "\x00c", "x", "y", "z"];
    let dict = DoubleArrayTrie::from_terms(terms.iter());

    let zipper = DoubleArrayTrieZipper::new_from_dict(&dict);
    let excluded: &[&[u8]] = &[b"\x00"];

    let count = zipper.iter_excluding(excluded).count();
    assert_eq!(count, 3); // Only x, y, z
}
