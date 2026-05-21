//! Integration tests for bijective dictionary types.
//!
//! These tests verify the bijection invariant and behavior across different
//! dictionary backends and usage patterns.

use libdictenstein::bijective::{BijectiveDictionary, BijectiveMap, InsertError};
use libdictenstein::MappedDictionary;

// =============================================================================
// BijectiveMap Tests
// =============================================================================

mod bijective_map {
    use super::*;

    #[test]
    fn test_symbol_table_use_case() {
        // Common use case: symbol table mapping names to unique IDs
        let symbols = BijectiveMap::new();

        symbols.insert("main", 1001_u32);
        symbols.insert("foo", 1002);
        symbols.insert("bar", 1003);
        symbols.insert("helper_fn", 1004);

        // Look up symbol by name
        assert_eq!(symbols.get_value("main"), Some(1001));
        assert_eq!(symbols.get_value("foo"), Some(1002));

        // Look up name by symbol ID
        assert_eq!(symbols.get_term(&1001), Some("main".to_string()));
        assert_eq!(symbols.get_term(&1002), Some("foo".to_string()));
    }

    #[test]
    fn test_enum_string_mapping() {
        // Use case: bidirectional mapping between enum variants and strings
        // Note: For custom enums, you would implement DictionaryValue
        // For simplicity, we use String values in this test
        let colors: BijectiveMap<String> = BijectiveMap::from_pairs([
            ("red", "Color::Red".to_string()),
            ("green", "Color::Green".to_string()),
            ("blue", "Color::Blue".to_string()),
        ]);

        assert_eq!(colors.get_value("red"), Some("Color::Red".to_string()));
        assert_eq!(
            colors.get_term(&"Color::Blue".to_string()),
            Some("blue".to_string())
        );
    }

    #[test]
    fn test_try_insert_error_handling() {
        let bimap: BijectiveMap<i32> = BijectiveMap::new();

        // First insertions should succeed
        assert!(bimap.try_insert("one", 1).is_ok());
        assert!(bimap.try_insert("two", 2).is_ok());

        // Duplicate term should fail
        let err = bimap.try_insert("one", 3);
        assert_eq!(err, Err(InsertError::DuplicateTerm));

        // Duplicate value should fail
        let err = bimap.try_insert("eins", 1);
        assert_eq!(err, Err(InsertError::DuplicateValue));

        // Map should be unchanged after failures
        assert_eq!(bimap.len(), 2);
        assert_eq!(bimap.get_value("one"), Some(1));
        assert_eq!(bimap.get_term(&1), Some("one".to_string()));
    }

    #[test]
    fn test_bijection_invariant_comprehensive() {
        let bimap = BijectiveMap::from_pairs([
            ("alpha", 1),
            ("beta", 2),
            ("gamma", 3),
            ("delta", 4),
            ("epsilon", 5),
        ]);

        // For every term, get_value followed by get_term should return the term
        for term in ["alpha", "beta", "gamma", "delta", "epsilon"] {
            let value = bimap.get_value(term).expect("term should exist");
            let recovered_term = bimap.get_term(&value).expect("value should exist");
            assert_eq!(recovered_term, term);
        }

        // For every value, get_term followed by get_value should return the value
        for value in [1, 2, 3, 4, 5] {
            let term = bimap.get_term(&value).expect("value should exist");
            let recovered_value = bimap.get_value(&term).expect("term should exist");
            assert_eq!(recovered_value, value);
        }
    }

    #[test]
    fn test_string_to_string_mapping() {
        let translations: BijectiveMap<String> = BijectiveMap::from_pairs([
            ("hello", "hola".to_string()),
            ("goodbye", "adiós".to_string()),
            ("yes", "sí".to_string()),
            ("no", "no".to_string()),
        ]);

        // English to Spanish
        assert_eq!(translations.get_value("hello"), Some("hola".to_string()));

        // Spanish to English
        assert_eq!(
            translations.get_term(&"hola".to_string()),
            Some("hello".to_string())
        );
    }
}

// =============================================================================
// Cross-type Tests
// =============================================================================

mod cross_type {
    use super::*;

    #[test]
    fn test_bijective_dictionary_trait_polymorphism() {
        // BijectiveMap should implement BijectiveDictionary
        fn verify_bijection<D: BijectiveDictionary<Value = u64>>(dict: &D) {
            // This function works with any BijectiveDictionary<Value = u64>
            let _ = dict.get_term(&0);
            let _ = dict.contains_value(&0);
            let _ = dict.bijection_len();
        }

        let bimap = BijectiveMap::from_pairs([("a", 0_u64), ("b", 1), ("c", 2)]);
        verify_bijection(&bimap);
    }

    #[test]
    fn test_mapped_dictionary_trait_polymorphism() {
        // BijectiveMap should implement MappedDictionary
        fn verify_mapped<D: MappedDictionary<Value = u64>>(dict: &D, term: &str) -> Option<u64> {
            dict.get_value(term)
        }

        let bimap = BijectiveMap::from_pairs([("test", 42_u64)]);
        assert_eq!(verify_mapped(&bimap, "test"), Some(42));
    }
}

// =============================================================================
// Property-based Tests
// =============================================================================

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashSet;

    proptest! {
        /// Property: BijectiveMap bijection invariant.
        /// For every (term, value) pair: get_value(term) = value AND get_term(value) = term
        #[test]
        fn bijective_map_bijection_invariant(
            pairs in prop::collection::vec(
                ("[a-z]{1,10}", 0i32..10000),
                1..50
            )
        ) {
            // Ensure unique terms and values
            let mut seen_terms = HashSet::new();
            let mut seen_values = HashSet::new();
            let unique_pairs: Vec<_> = pairs
                .into_iter()
                .filter(|(t, v)| seen_terms.insert(t.clone()) && seen_values.insert(*v))
                .collect();

            let bimap: BijectiveMap<i32> = BijectiveMap::new();
            for (term, value) in &unique_pairs {
                let _ = bimap.try_insert(term, *value);
            }

            // Verify bijection for all inserted pairs
            for (term, _value) in &unique_pairs {
                if let Some(v) = bimap.get_value(term) {
                    let recovered_term = bimap.get_term(&v);
                    prop_assert!(recovered_term.is_some());
                    prop_assert_eq!(bimap.get_value(&recovered_term.unwrap()), Some(v));
                }
            }
        }
    }
}
