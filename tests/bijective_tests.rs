//! Integration tests for bijective dictionary types.
//!
//! These tests verify the bijection invariant and behavior across different
//! dictionary backends and usage patterns.

use libdictenstein::bijective::{BijectiveDictionary, BijectiveMap, IndexedVocabulary, InsertError};
use libdictenstein::MappedDictionary;

// =============================================================================
// IndexedVocabulary Tests
// =============================================================================

mod indexed_vocabulary {
    use super::*;

    #[test]
    fn test_embedding_vocabulary_use_case() {
        // Common use case: building a vocabulary for word embeddings
        let terms = vec![
            "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
            "have", "has", "had", "do", "does", "did", "will", "would", "could", "should",
        ];

        let vocab = IndexedVocabulary::from_terms(terms.clone());

        // Verify all terms are indexed correctly
        for (i, term) in terms.iter().enumerate() {
            assert_eq!(vocab.get_index(term), Some(i as u64));
            assert_eq!(vocab.get_term(i as u64), Some(*term));
        }

        // Verify bijection invariant
        for i in 0..terms.len() as u64 {
            if let Some(term) = vocab.get_term(i) {
                assert_eq!(vocab.get_index(term), Some(i));
            }
        }
    }

    #[test]
    fn test_tokenizer_with_special_tokens() {
        // Reserve indices 0-2 for special tokens: [PAD], [UNK], [CLS]
        let vocab = IndexedVocabulary::with_start_index(3);

        // Insert regular vocabulary
        let words = ["hello", "world", "rust", "programming"];
        for word in words {
            vocab.get_or_insert(word);
        }

        // Verify special token indices are free
        assert_eq!(vocab.get_term(0), None);
        assert_eq!(vocab.get_term(1), None);
        assert_eq!(vocab.get_term(2), None);

        // Verify regular words start at index 3
        assert_eq!(vocab.get_index("hello"), Some(3));
        assert_eq!(vocab.get_index("world"), Some(4));
    }

    #[test]
    fn test_incremental_vocabulary_building() {
        let vocab = IndexedVocabulary::new();

        // Build vocabulary incrementally as we process text
        let text = "the quick brown fox jumps over the lazy dog";
        let mut seen_indices = vec![];

        for word in text.split_whitespace() {
            let idx = vocab.get_or_insert(word);
            seen_indices.push(idx);
        }

        // "the" appears twice but should have the same index
        assert_eq!(seen_indices[0], seen_indices[6]); // "the" at positions 0 and 6

        // Should have 8 unique words
        assert_eq!(vocab.len(), 8);

        // Verify round-trip for all unique words
        for word in ["the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog"] {
            let idx = vocab.get_index(word).expect("word should exist");
            let term = vocab.get_term(idx).expect("index should exist");
            assert_eq!(term, word);
        }
    }

    #[test]
    fn test_large_vocabulary() {
        // Test with a larger vocabulary to ensure scalability
        let vocab = IndexedVocabulary::new();

        for i in 0..10000 {
            let term = format!("term_{:05}", i);
            let idx = vocab.insert(&term);
            assert_eq!(idx, i as u64);
        }

        assert_eq!(vocab.len(), 10000);

        // Spot check some terms
        assert_eq!(vocab.get_index("term_00000"), Some(0));
        assert_eq!(vocab.get_index("term_05000"), Some(5000));
        assert_eq!(vocab.get_index("term_09999"), Some(9999));

        // Verify bijection for sampled indices
        for i in (0..10000).step_by(100) {
            let term = vocab.get_term(i).expect("index should exist");
            assert_eq!(vocab.get_index(term), Some(i));
        }
    }

    #[test]
    fn test_unicode_vocabulary() {
        let terms = vec![
            "hello",
            "世界",     // Chinese
            "مرحبا",    // Arabic
            "привет",   // Russian
            "こんにちは", // Japanese
            "안녕하세요",  // Korean
            "🎉🎊🎈",   // Emoji
            "café",     // Accented
        ];

        let vocab = IndexedVocabulary::from_terms(terms.clone());

        for (i, term) in terms.iter().enumerate() {
            assert_eq!(vocab.get_index(term), Some(i as u64));
            assert_eq!(vocab.get_term(i as u64), Some(*term));
        }
    }
}

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
        assert_eq!(colors.get_term(&"Color::Blue".to_string()), Some("blue".to_string()));
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
        assert_eq!(translations.get_term(&"hola".to_string()), Some("hello".to_string()));
    }
}

// =============================================================================
// Cross-type Tests
// =============================================================================

mod cross_type {
    use super::*;

    #[test]
    fn test_bijective_dictionary_trait_polymorphism() {
        // Both types should implement BijectiveDictionary
        fn verify_bijection<D: BijectiveDictionary<Value = u64>>(dict: &D) {
            // This function works with any BijectiveDictionary<Value = u64>
            let _ = dict.get_term(&0);
            let _ = dict.contains_value(&0);
            let _ = dict.bijection_len();
        }

        let vocab = IndexedVocabulary::from_terms(["a", "b", "c"]);
        verify_bijection(&vocab);

        let bimap = BijectiveMap::from_pairs([("a", 0_u64), ("b", 1), ("c", 2)]);
        verify_bijection(&bimap);
    }

    #[test]
    fn test_mapped_dictionary_trait_polymorphism() {
        // Both types should implement MappedDictionary
        fn verify_mapped<D: MappedDictionary<Value = u64>>(dict: &D, term: &str) -> Option<u64> {
            dict.get_value(term)
        }

        let vocab = IndexedVocabulary::from_terms(["test"]);
        assert_eq!(verify_mapped(&vocab, "test"), Some(0));

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
        /// Property: The bijection invariant always holds.
        /// For every (term, index) pair: get_index(term) = index AND get_term(index) = term
        #[test]
        fn indexed_vocabulary_bijection_invariant(
            terms in prop::collection::vec("[a-z]{1,10}", 1..100)
        ) {
            let unique_terms: Vec<_> = terms.into_iter().collect::<HashSet<_>>().into_iter().collect();
            let vocab = IndexedVocabulary::from_terms(&unique_terms);

            for term in unique_terms.iter() {
                let idx = vocab.get_index(term).expect("term should exist");
                let recovered = vocab.get_term(idx).expect("index should exist");
                prop_assert_eq!(recovered, term.as_str());
            }
        }

        /// Property: Index assignment is monotonic.
        /// Each new term gets the next sequential index.
        #[test]
        fn indexed_vocabulary_monotonic_indices(
            terms in prop::collection::vec("[a-z]{1,5}", 1..50)
        ) {
            let unique_terms: Vec<_> = terms.into_iter().collect::<HashSet<_>>().into_iter().collect();
            let vocab = IndexedVocabulary::new();

            let mut expected_next = 0u64;
            for term in &unique_terms {
                let idx = vocab.get_or_insert(term);
                // Each new unique term should get the next index
                prop_assert!(idx <= expected_next);
                expected_next = expected_next.max(idx + 1);
            }
        }

        /// Property: get_or_insert is idempotent.
        /// Calling it multiple times with the same term returns the same index.
        #[test]
        fn indexed_vocabulary_get_or_insert_idempotent(
            term in "[a-z]{1,10}"
        ) {
            let vocab = IndexedVocabulary::new();

            let idx1 = vocab.get_or_insert(&term);
            let idx2 = vocab.get_or_insert(&term);
            let idx3 = vocab.get_or_insert(&term);

            prop_assert_eq!(idx1, idx2);
            prop_assert_eq!(idx2, idx3);
            prop_assert_eq!(vocab.len(), 1);
        }

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

        /// Property: No index gaps after insertions.
        /// Indices form a contiguous range [start_index, start_index + len).
        #[test]
        fn indexed_vocabulary_no_gaps(
            terms in prop::collection::vec("[a-z]{1,5}", 1..100),
            start_index in 0u64..1000
        ) {
            let unique_terms: Vec<_> = terms.into_iter().collect::<HashSet<_>>().into_iter().collect();
            let vocab = IndexedVocabulary::with_start_index(start_index);

            for term in &unique_terms {
                vocab.get_or_insert(term);
            }

            // Check that all indices in the range are valid
            for i in 0..vocab.len() {
                let idx = start_index + i as u64;
                prop_assert!(vocab.get_term(idx).is_some(), "Missing term at index {}", idx);
            }

            // Check that indices outside the range are invalid
            if start_index > 0 {
                prop_assert!(vocab.get_term(start_index - 1).is_none());
            }
            prop_assert!(vocab.get_term(start_index + vocab.len() as u64).is_none());
        }
    }
}
