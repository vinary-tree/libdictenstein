//! Property-based tests for BijectiveMap.
//!
//! These tests verify the fundamental bijection invariant and related properties.
//!
//! Run with: cargo test --test proptest_bijective

mod common;

use common::strategies::*;
use libdictenstein::bijective::{BijectiveDictionary, BijectiveMap, InsertError};
use proptest::prelude::*;
use std::collections::{HashMap, HashSet};

// =============================================================================
// Helper Functions
// =============================================================================

/// Generate unique term-value pairs for bijective map testing.
fn unique_pairs(pairs: Vec<(String, i32)>) -> Vec<(String, i32)> {
    let mut seen_terms = HashSet::new();
    let mut seen_values = HashSet::new();
    pairs
        .into_iter()
        .filter(|(t, v)| seen_terms.insert(t.clone()) && seen_values.insert(*v))
        .collect()
}

// =============================================================================
// Bijection Invariant Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Property: Forward-reverse roundtrip: get_term(get_value(term)) == term
    #[test]
    fn bijection_forward_reverse_roundtrip(
        pairs in prop::collection::vec((ascii_term(1, 15), any::<i32>()), 1..=50)
    ) {
        let unique_pairs = unique_pairs(pairs);
        let bimap: BijectiveMap<i32> = BijectiveMap::new();

        for (term, value) in &unique_pairs {
            let _ = bimap.try_insert(term, *value);
        }

        for (term, _) in &unique_pairs {
            if let Some(value) = bimap.get_value(term) {
                let recovered_term = bimap.get_term(&value);
                prop_assert!(
                    recovered_term.is_some(),
                    "get_term should return Some for valid value"
                );
                prop_assert_eq!(
                    &recovered_term.unwrap(),
                    term,
                    "Forward-reverse roundtrip should return original term"
                );
            }
        }
    }

    /// Property: Reverse-forward roundtrip: get_value(get_term(value)) == value
    #[test]
    fn bijection_reverse_forward_roundtrip(
        pairs in prop::collection::vec((ascii_term(1, 15), any::<i32>()), 1..=50)
    ) {
        let unique_pairs = unique_pairs(pairs);
        let bimap: BijectiveMap<i32> = BijectiveMap::new();

        for (term, value) in &unique_pairs {
            let _ = bimap.try_insert(term, *value);
        }

        for (_, value) in &unique_pairs {
            if let Some(term) = bimap.get_term(value) {
                let recovered_value = bimap.get_value(&term);
                prop_assert!(
                    recovered_value.is_some(),
                    "get_value should return Some for valid term"
                );
                prop_assert_eq!(
                    recovered_value.unwrap(),
                    *value,
                    "Reverse-forward roundtrip should return original value"
                );
            }
        }
    }

    /// Property: No duplicate terms allowed
    #[test]
    fn no_duplicate_terms(
        term in ascii_term(1, 15),
        value1 in any::<i32>(),
        value2 in any::<i32>()
    ) {
        prop_assume!(value1 != value2);

        let bimap: BijectiveMap<i32> = BijectiveMap::new();

        let first_insert = bimap.try_insert(&term, value1);
        prop_assert!(first_insert.is_ok(), "First insert should succeed");

        let second_insert = bimap.try_insert(&term, value2);
        prop_assert_eq!(
            second_insert,
            Err(InsertError::DuplicateTerm),
            "Second insert with same term should fail"
        );

        // Original mapping should be preserved
        prop_assert_eq!(bimap.get_value(&term), Some(value1));
    }

    /// Property: No duplicate values allowed
    #[test]
    fn no_duplicate_values(
        term1 in ascii_term(1, 15),
        term2 in ascii_term(1, 15),
        value in any::<i32>()
    ) {
        prop_assume!(term1 != term2);

        let bimap: BijectiveMap<i32> = BijectiveMap::new();

        let first_insert = bimap.try_insert(&term1, value);
        prop_assert!(first_insert.is_ok(), "First insert should succeed");

        let second_insert = bimap.try_insert(&term2, value);
        prop_assert_eq!(
            second_insert,
            Err(InsertError::DuplicateValue),
            "Second insert with same value should fail"
        );

        // Original mapping should be preserved
        prop_assert_eq!(bimap.get_term(&value), Some(term1.clone()));
    }

    /// Property: Length consistency after operations
    #[test]
    fn length_consistency(
        pairs in prop::collection::vec((ascii_term(1, 15), any::<i32>()), 1..=50)
    ) {
        let unique_pairs = unique_pairs(pairs);
        let bimap: BijectiveMap<i32> = BijectiveMap::new();

        let mut expected_len = 0;
        for (term, value) in &unique_pairs {
            if bimap.try_insert(term, *value).is_ok() {
                expected_len += 1;
            }
        }

        prop_assert_eq!(
            bimap.len(),
            expected_len,
            "len() should match number of successful insertions"
        );

        // bijection_len should match len
        prop_assert_eq!(
            bimap.bijection_len(),
            expected_len,
            "bijection_len() should match len()"
        );
    }
}

// =============================================================================
// Consistency Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: contains_term() and get_value() are consistent
    #[test]
    fn contains_get_value_consistency(
        pairs in prop::collection::vec((ascii_term(1, 15), any::<i32>()), 1..=30),
        missing_term in ascii_term(1, 15)
    ) {
        let unique_pairs = unique_pairs(pairs);
        let bimap: BijectiveMap<i32> = BijectiveMap::new();

        for (term, value) in &unique_pairs {
            let _ = bimap.try_insert(term, *value);
        }

        // All inserted terms should have consistent contains_term/get_value
        for (term, _) in &unique_pairs {
            let contains = bimap.contains_term(term);
            let get_value = bimap.get_value(term);
            prop_assert_eq!(
                contains,
                get_value.is_some(),
                "contains_term() and get_value().is_some() should be consistent"
            );
        }

        // Missing term (if not inserted) should be consistent
        if !unique_pairs.iter().any(|(t, _)| t == &missing_term) {
            prop_assert!(!bimap.contains_term(&missing_term));
            prop_assert!(bimap.get_value(&missing_term).is_none());
        }
    }

    /// Property: contains_value() and get_term() are consistent
    #[test]
    fn contains_value_get_term_consistency(
        pairs in prop::collection::vec((ascii_term(1, 15), any::<i32>()), 1..=30),
        missing_value in any::<i32>()
    ) {
        let unique_pairs = unique_pairs(pairs);
        let bimap: BijectiveMap<i32> = BijectiveMap::new();

        for (term, value) in &unique_pairs {
            let _ = bimap.try_insert(term, *value);
        }

        // All inserted values should have consistent contains_value/get_term
        for (_, value) in &unique_pairs {
            let contains = bimap.contains_value(value);
            let get_term = bimap.get_term(value);
            prop_assert_eq!(
                contains,
                get_term.is_some(),
                "contains_value() and get_term().is_some() should be consistent"
            );
        }

        // Missing value (if not inserted) should be consistent
        if !unique_pairs.iter().any(|(_, v)| v == &missing_value) {
            prop_assert!(!bimap.contains_value(&missing_value));
            prop_assert!(bimap.get_term(&missing_value).is_none());
        }
    }
}

// =============================================================================
// Iterator Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: Iterator returns all inserted pairs
    #[test]
    fn iterator_completeness(
        pairs in prop::collection::vec((ascii_term(1, 15), any::<i32>()), 1..=30)
    ) {
        let unique_pairs = unique_pairs(pairs);
        let bimap: BijectiveMap<i32> = BijectiveMap::new();

        let mut expected: HashMap<String, i32> = HashMap::new();
        for (term, value) in &unique_pairs {
            if bimap.try_insert(term, *value).is_ok() {
                expected.insert(term.clone(), *value);
            }
        }

        let iterated: HashMap<String, i32> = bimap.iter().collect();

        prop_assert_eq!(
            iterated,
            expected,
            "Iterator should return exactly the inserted pairs"
        );
    }
}

// =============================================================================
// from_pairs Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: from_pairs creates correct mapping
    #[test]
    fn from_pairs_correctness(
        pairs in prop::collection::vec((ascii_term(1, 15), any::<i32>()), 1..=30)
    ) {
        let unique_pairs = unique_pairs(pairs);
        let pairs_vec: Vec<(&str, i32)> = unique_pairs
            .iter()
            .map(|(t, v)| (t.as_str(), *v))
            .collect();

        let bimap: BijectiveMap<i32> = BijectiveMap::from_pairs(pairs_vec.clone());

        // Note: from_pairs uses insert() which overwrites, so only final values matter
        // For unique pairs, all should be present
        for (term, value) in &unique_pairs {
            if bimap.contains_term(term) {
                prop_assert_eq!(
                    bimap.get_value(term),
                    Some(*value),
                    "from_pairs should preserve term-value mapping"
                );
            }
        }
    }
}

// =============================================================================
// Edge Case Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property: Empty string as term works correctly
    #[test]
    fn empty_string_term(value in any::<i32>()) {
        let bimap: BijectiveMap<i32> = BijectiveMap::new();

        let result = bimap.try_insert("", value);
        prop_assert!(result.is_ok(), "Empty string should be insertable");

        prop_assert!(bimap.contains_term(""));
        prop_assert_eq!(bimap.get_value(""), Some(value));
        prop_assert_eq!(bimap.get_term(&value), Some("".to_string()));
    }

    /// Property: Unicode terms work correctly
    #[test]
    fn unicode_terms(
        term in unicode_term(1, 10),
        value in any::<i32>()
    ) {
        let bimap: BijectiveMap<i32> = BijectiveMap::new();

        let result = bimap.try_insert(&term, value);
        prop_assert!(result.is_ok(), "Unicode term should be insertable");

        prop_assert!(bimap.contains_term(&term));
        prop_assert_eq!(bimap.get_value(&term), Some(value));
        prop_assert_eq!(bimap.get_term(&value), Some(term.clone()));
    }

    /// Property: String values work correctly
    #[test]
    fn string_values(
        term in ascii_term(1, 15),
        value in string_value()
    ) {
        let bimap: BijectiveMap<String> = BijectiveMap::new();

        let result = bimap.try_insert(&term, value.clone());
        prop_assert!(result.is_ok(), "String value should be insertable");

        prop_assert!(bimap.contains_term(&term));
        prop_assert_eq!(bimap.get_value(&term), Some(value.clone()));
        prop_assert_eq!(bimap.get_term(&value), Some(term.clone()));
    }
}
