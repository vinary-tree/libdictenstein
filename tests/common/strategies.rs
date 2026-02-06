//! Reusable proptest strategies for dictionary testing.
//!
//! This module provides strategies for generating test data that exercises
//! various edge cases and code paths in dictionary implementations.

use proptest::prelude::*;
use std::collections::HashSet;

// =============================================================================
// Basic Term Strategies
// =============================================================================

/// Generate ASCII terms (lowercase letters only).
///
/// # Arguments
/// * `min_len` - Minimum term length
/// * `max_len` - Maximum term length
pub fn ascii_term(min_len: usize, max_len: usize) -> impl Strategy<Value = String> {
    prop::string::string_regex(&format!("[a-z]{{{},{}}}", min_len, max_len))
        .expect("valid regex for ascii_term")
}

/// Generate Unicode terms with diverse characters.
///
/// # Arguments
/// * `min_len` - Minimum term length (in characters)
/// * `max_len` - Maximum term length (in characters)
pub fn unicode_term(min_len: usize, max_len: usize) -> impl Strategy<Value = String> {
    // Mix of ASCII, accented Latin, Greek, CJK
    prop::string::string_regex(&format!(
        "[a-zA-Zàáâãäåæçèéêëìíîïðñòóôõöøùúûüýþÿαβγδεζηθικλμνξοπρστυφχψω一二三四五六七八九十]{{{},{}}}",
        min_len, max_len
    ))
    .expect("valid regex for unicode_term")
}

/// Generate alphanumeric terms.
pub fn alphanumeric_term(min_len: usize, max_len: usize) -> impl Strategy<Value = String> {
    prop::string::string_regex(&format!("[a-zA-Z0-9]{{{},{}}}", min_len, max_len))
        .expect("valid regex for alphanumeric_term")
}

// =============================================================================
// Clustered Term Strategies (for branch coverage)
// =============================================================================

/// Generate terms that share common prefixes.
///
/// This tests prefix compression and path splitting in tries.
///
/// # Arguments
/// * `count` - Number of terms to generate
pub fn prefix_clustered_terms(count: usize) -> impl Strategy<Value = Vec<String>> {
    // Generate a common prefix, then append different suffixes
    (
        prop::string::string_regex("[a-z]{2,5}").expect("valid prefix regex"),
        prop::collection::vec(
            prop::string::string_regex("[a-z]{0,8}").expect("valid suffix regex"),
            count,
        ),
    )
        .prop_map(|(prefix, suffixes)| {
            suffixes
                .into_iter()
                .map(|suffix| format!("{}{}", prefix, suffix))
                .collect::<HashSet<_>>()
                .into_iter()
                .collect()
        })
}

/// Generate terms that share common suffixes.
///
/// This tests suffix sharing in DAWGs.
///
/// # Arguments
/// * `count` - Number of terms to generate
pub fn suffix_clustered_terms(count: usize) -> impl Strategy<Value = Vec<String>> {
    // Generate a common suffix, then prepend different prefixes
    (
        prop::string::string_regex("[a-z]{2,5}").expect("valid suffix regex"),
        prop::collection::vec(
            prop::string::string_regex("[a-z]{0,8}").expect("valid prefix regex"),
            count,
        ),
    )
        .prop_map(|(suffix, prefixes)| {
            prefixes
                .into_iter()
                .map(|prefix| format!("{}{}", prefix, suffix))
                .collect::<HashSet<_>>()
                .into_iter()
                .collect()
        })
}

/// Generate terms that cluster around specific first bytes.
///
/// This tests bucket distribution in ARTrie nodes.
pub fn first_byte_clustered_terms(count: usize) -> impl Strategy<Value = Vec<String>> {
    // Pick a few "hot" starting letters, concentrate terms there
    (
        prop::sample::subsequence(&['a', 'b', 'c', 'd'], 2..=3),
        prop::collection::vec(
            prop::string::string_regex("[a-z]{1,10}").expect("valid term regex"),
            count,
        ),
    )
        .prop_map(|(hot_chars, terms)| {
            terms
                .into_iter()
                .enumerate()
                .map(|(i, term)| {
                    let first_char = hot_chars[i % hot_chars.len()];
                    format!("{}{}", first_char, term)
                })
                .collect::<HashSet<_>>()
                .into_iter()
                .collect()
        })
}

// =============================================================================
// Edge Case Strategies
// =============================================================================

/// Generate edge-case terms for boundary testing.
///
/// Includes: empty strings, single characters, very long strings,
/// strings with special patterns.
pub fn edge_case_terms() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(
        prop_oneof![
            // Empty string (if supported)
            Just("".to_string()),
            // Single character
            prop::string::string_regex("[a-z]")
                .expect("valid single char regex")
                .prop_map(|s| s),
            // Very short
            prop::string::string_regex("[a-z]{2}")
                .expect("valid short regex")
                .prop_map(|s| s),
            // Medium length
            prop::string::string_regex("[a-z]{5,10}")
                .expect("valid medium regex")
                .prop_map(|s| s),
            // Long strings (stress test)
            prop::string::string_regex("[a-z]{50,100}")
                .expect("valid long regex")
                .prop_map(|s| s),
            // Repeated characters (tests compression)
            (1usize..=20, prop::char::range('a', 'z'))
                .prop_map(|(len, c)| std::iter::repeat(c).take(len).collect()),
        ],
        1..=20,
    )
}

/// Generate byte sequences for fuzzing.
pub fn byte_sequence(min_len: usize, max_len: usize) -> impl Strategy<Value = Vec<u8>> {
    // Printable ASCII to avoid null bytes
    prop::collection::vec(32u8..127, min_len..=max_len)
}

// =============================================================================
// Operation Strategies
// =============================================================================

/// Dictionary operations for property-based testing.
#[derive(Debug, Clone)]
pub enum DictOp<V: Clone + std::fmt::Debug> {
    /// Insert a term with a value
    Insert(String, V),
    /// Remove a term
    Remove(String),
    /// Check if term exists
    Contains(String),
}

/// Generate a sequence of dictionary operations.
///
/// # Arguments
/// * `count` - Number of operations
/// * `insert_ratio` - Ratio of inserts (0.0-1.0), remainder split between remove/contains
pub fn dict_ops_strategy<V>(
    count: usize,
    insert_ratio: f64,
    value_strategy: impl Strategy<Value = V> + Clone + 'static,
) -> impl Strategy<Value = Vec<DictOp<V>>>
where
    V: Clone + std::fmt::Debug + 'static,
{
    let insert_weight = (insert_ratio * 100.0) as u32;
    let other_weight = ((1.0 - insert_ratio) * 50.0) as u32;

    prop::collection::vec(
        prop_oneof![
            insert_weight => (ascii_term(1, 15), value_strategy.clone())
                .prop_map(|(t, v)| DictOp::Insert(t, v)),
            other_weight => ascii_term(1, 15).prop_map(DictOp::Remove),
            other_weight => ascii_term(1, 15).prop_map(DictOp::Contains),
        ],
        count,
    )
}

/// Generate mixed insert/remove operations for stress testing.
pub fn insert_remove_ops(count: usize) -> impl Strategy<Value = Vec<DictOp<()>>> {
    dict_ops_strategy(count, 0.7, Just(()))
}

// =============================================================================
// Value Strategies
// =============================================================================

/// Generate u32 values for dictionary testing.
pub fn u32_value() -> impl Strategy<Value = u32> {
    any::<u32>()
}

/// Generate i64 values for dictionary testing.
pub fn i64_value() -> impl Strategy<Value = i64> {
    any::<i64>()
}

/// Generate string values for dictionary testing.
pub fn string_value() -> impl Strategy<Value = String> {
    prop::string::string_regex("[a-zA-Z0-9_]{1,20}").expect("valid string value regex")
}

// =============================================================================
// Set Operation Strategies
// =============================================================================

/// Generate two sets of terms with controlled overlap.
///
/// # Arguments
/// * `size` - Approximate size of each set
/// * `overlap_ratio` - Ratio of terms that should be in both sets (0.0-1.0)
pub fn overlapping_term_sets(
    size: usize,
    overlap_ratio: f64,
) -> impl Strategy<Value = (Vec<String>, Vec<String>)> {
    let overlap_count = ((size as f64) * overlap_ratio) as usize;
    let unique_count = size.saturating_sub(overlap_count);

    (
        prop::collection::vec(ascii_term(1, 10), overlap_count),
        prop::collection::vec(ascii_term(1, 10), unique_count),
        prop::collection::vec(ascii_term(1, 10), unique_count),
    )
        .prop_map(|(shared, unique_a, unique_b)| {
            let set_a: Vec<String> = shared
                .iter()
                .cloned()
                .chain(unique_a)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            let set_b: Vec<String> = shared
                .into_iter()
                .chain(unique_b)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            (set_a, set_b)
        })
}

/// Generate three sets of terms for testing n-ary operations.
pub fn three_term_sets(size: usize) -> impl Strategy<Value = (Vec<String>, Vec<String>, Vec<String>)> {
    (
        prop::collection::vec(ascii_term(1, 10), size),
        prop::collection::vec(ascii_term(1, 10), size),
        prop::collection::vec(ascii_term(1, 10), size),
    )
        .prop_map(|(a, b, c)| {
            (
                a.into_iter().collect::<HashSet<_>>().into_iter().collect(),
                b.into_iter().collect::<HashSet<_>>().into_iter().collect(),
                c.into_iter().collect::<HashSet<_>>().into_iter().collect(),
            )
        })
}

// =============================================================================
// Test Helpers
// =============================================================================

/// Deduplicate a vector of terms.
pub fn dedupe_terms(terms: Vec<String>) -> Vec<String> {
    terms
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

/// Check if two sets are equal (order-independent).
pub fn sets_equal<T: Eq + std::hash::Hash>(a: &[T], b: &[T]) -> bool {
    let set_a: HashSet<_> = a.iter().collect();
    let set_b: HashSet<_> = b.iter().collect();
    set_a == set_b
}
