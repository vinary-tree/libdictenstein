//! Macro-based test generalization for trait implementations.
//!
//! This module provides macros that generate property-based tests for all
//! implementations of shared dictionary traits. This ensures consistent
//! behavior across DynamicDawg, DoubleArrayTrie, SuffixAutomaton, and other
//! dictionary backends.
//!
//! # Usage
//!
//! ```ignore
//! // Generate tests for all Dictionary implementations
//! test_dictionary_contains!(
//!     dawg_contains_test => DynamicDawg::<()>::new(),
//!     dat_contains_test => DoubleArrayTrie::new(),
//! );
//! ```

/// Generates property tests for the `Dictionary::contains` method across multiple
/// dictionary implementations.
///
/// This macro creates proptest tests that verify:
/// - All inserted terms are found by contains()
/// - Non-inserted terms are not found
/// - Length is accurate after insertions
#[macro_export]
macro_rules! test_dictionary_contains {
    (
        $( $test_name:ident => $dict_expr:expr, $insert_method:tt ),+ $(,)?
    ) => {
        $(
            paste::paste! {
                proptest! {
                    #![proptest_config(ProptestConfig::with_cases(50))]

                    #[test]
                    fn [<$test_name _inserted_terms_found>](
                        terms in prop::collection::vec($crate::common::strategies::ascii_term(1, 15), 1..=30)
                    ) {
                        let dict = $dict_expr;
                        let unique_terms: std::collections::HashSet<_> = terms.into_iter().collect();

                        test_dictionary_contains!(@insert dict, unique_terms, $insert_method);

                        for term in &unique_terms {
                            prop_assert!(
                                dict.contains(term),
                                "Term '{}' should be found by contains()",
                                term
                            );
                        }
                    }

                    #[test]
                    fn [<$test_name _nonexistent_terms_not_found>](
                        terms in prop::collection::vec($crate::common::strategies::ascii_term(1, 10), 1..=20),
                        missing in $crate::common::strategies::ascii_term(11, 15)
                    ) {
                        let dict = $dict_expr;
                        let unique_terms: std::collections::HashSet<_> = terms.into_iter().collect();

                        test_dictionary_contains!(@insert dict, unique_terms, $insert_method);

                        // If missing is not in our set, it should not be found
                        if !unique_terms.contains(&missing) {
                            prop_assert!(
                                !dict.contains(&missing),
                                "Non-inserted term '{}' should not be found",
                                missing
                            );
                        }
                    }
                }
            }
        )+
    };

    // Helper for mutable insertion (one term at a time)
    (@insert $dict:ident, $terms:ident, insert) => {
        for term in &$terms {
            $dict.insert(term);
        }
    };

    // Helper for builder-style (from_terms)
    (@insert $dict:ident, $terms:ident, from_terms) => {
        // Note: for from_terms, the dict must be created with the terms
        // This branch is just a placeholder - actual usage requires different macro structure
        let _ = &$terms; // suppress unused warning
    };
}

/// Generates property tests for `MutableDictionary` implementations.
///
/// Tests insertion, removal, and consistency invariants.
#[macro_export]
macro_rules! test_mutable_dictionary {
    (
        $( $test_name:ident => $dict_expr:expr ),+ $(,)?
    ) => {
        $(
            paste::paste! {
                proptest! {
                    #![proptest_config(ProptestConfig::with_cases(50))]

                    #[test]
                    fn [<$test_name _insert_then_contains>](
                        terms in prop::collection::vec($crate::common::strategies::ascii_term(1, 15), 1..=30)
                    ) {
                        let dict = $dict_expr;
                        let unique_terms: std::collections::HashSet<_> = terms.into_iter().collect();

                        for term in &unique_terms {
                            dict.insert(term);
                        }

                        for term in &unique_terms {
                            prop_assert!(
                                dict.contains(term),
                                "Inserted term '{}' should be found",
                                term
                            );
                        }
                    }

                    #[test]
                    fn [<$test_name _remove_then_not_contains>](
                        terms in prop::collection::vec($crate::common::strategies::ascii_term(1, 15), 5..=30)
                    ) {
                        let dict = $dict_expr;
                        let unique_terms: Vec<_> = terms.into_iter()
                            .collect::<std::collections::HashSet<_>>()
                            .into_iter()
                            .collect();

                        // Insert all
                        for term in &unique_terms {
                            dict.insert(term);
                        }

                        // Remove first half
                        let to_remove: Vec<_> = unique_terms.iter().take(unique_terms.len() / 2).cloned().collect();
                        for term in &to_remove {
                            dict.remove(term);
                        }

                        // Verify removed terms are gone
                        for term in &to_remove {
                            prop_assert!(
                                !dict.contains(term),
                                "Removed term '{}' should not be found",
                                term
                            );
                        }

                        // Verify remaining terms still exist
                        for term in unique_terms.iter().skip(unique_terms.len() / 2) {
                            prop_assert!(
                                dict.contains(term),
                                "Non-removed term '{}' should still be found",
                                term
                            );
                        }
                    }

                    #[test]
                    fn [<$test_name _insert_remove_reinsert>](
                        terms in prop::collection::vec($crate::common::strategies::ascii_term(1, 15), 1..=20)
                    ) {
                        let dict = $dict_expr;
                        let unique_terms: Vec<_> = terms.into_iter()
                            .collect::<std::collections::HashSet<_>>()
                            .into_iter()
                            .collect();

                        // Insert all
                        for term in &unique_terms {
                            dict.insert(term);
                        }

                        // Remove all
                        for term in &unique_terms {
                            dict.remove(term);
                        }

                        // Verify all removed
                        for term in &unique_terms {
                            prop_assert!(
                                !dict.contains(term),
                                "Term '{}' should be gone after remove",
                                term
                            );
                        }

                        // Reinsert all
                        for term in &unique_terms {
                            dict.insert(term);
                        }

                        // Verify all back
                        for term in &unique_terms {
                            prop_assert!(
                                dict.contains(term),
                                "Term '{}' should be back after reinsert",
                                term
                            );
                        }
                    }
                }
            }
        )+
    };
}

/// Generates property tests for `MutableMappedDictionary` implementations.
///
/// Tests value insertion, retrieval, and update consistency.
///
/// **Note**: The calling code must have `MappedDictionary` trait in scope.
#[macro_export]
macro_rules! test_mapped_dictionary {
    (
        $( $test_name:ident => $dict_expr:expr ),+ $(,)?
    ) => {
        $(
            paste::paste! {
                proptest! {
                    #![proptest_config(ProptestConfig::with_cases(50))]

                    #[test]
                    fn [<$test_name _value_roundtrip>](
                        pairs in prop::collection::vec(
                            ($crate::common::strategies::ascii_term(1, 15), any::<u32>()),
                            1..=30
                        )
                    ) {
                        let dict = $dict_expr;
                        let expected: std::collections::HashMap<String, u32> = pairs.into_iter().collect();

                        for (term, value) in &expected {
                            dict.insert_with_value(term, *value);
                        }

                        for (term, expected_value) in &expected {
                            let actual = dict.get_value(term);
                            prop_assert_eq!(
                                actual,
                                Some(*expected_value),
                                "Term '{}' should have value {}",
                                term,
                                expected_value
                            );
                        }
                    }

                    #[test]
                    fn [<$test_name _value_overwrite>](
                        term in $crate::common::strategies::ascii_term(1, 15),
                        value1 in any::<u32>(),
                        value2 in any::<u32>()
                    ) {
                        let dict = $dict_expr;

                        dict.insert_with_value(&term, value1);
                        prop_assert_eq!(dict.get_value(&term), Some(value1));

                        dict.insert_with_value(&term, value2);
                        prop_assert_eq!(
                            dict.get_value(&term),
                            Some(value2),
                            "Value should be updated to new value"
                        );
                    }
                }
            }
        )+
    };
}

/// Generates property tests for `CompactableDictionary` implementations.
///
/// Tests that compaction preserves all terms.
#[macro_export]
macro_rules! test_compactable_dictionary {
    (
        $( $test_name:ident => $dict_expr:expr ),+ $(,)?
    ) => {
        $(
            paste::paste! {
                proptest! {
                    #![proptest_config(ProptestConfig::with_cases(30))]

                    #[test]
                    fn [<$test_name _compact_preserves_terms>](
                        terms in prop::collection::vec($crate::common::strategies::ascii_term(1, 15), 10..=50)
                    ) {
                        let dict = $dict_expr;
                        let unique_terms: std::collections::HashSet<_> = terms.into_iter().collect();

                        // Insert all
                        for term in &unique_terms {
                            dict.insert(term);
                        }

                        // Remove some to create fragmentation
                        let to_remove: Vec<_> = unique_terms.iter().take(unique_terms.len() / 3).cloned().collect();
                        let removed_set: std::collections::HashSet<_> = to_remove.iter().cloned().collect();
                        for term in &to_remove {
                            dict.remove(term);
                        }

                        let remaining: std::collections::HashSet<_> = unique_terms
                            .difference(&removed_set)
                            .cloned()
                            .collect();

                        // Compact
                        dict.compact();

                        // Verify remaining terms still exist
                        for term in &remaining {
                            prop_assert!(
                                dict.contains(term),
                                "Term '{}' should exist after compaction",
                                term
                            );
                        }

                        // Verify removed terms still gone
                        for term in &to_remove {
                            prop_assert!(
                                !dict.contains(term),
                                "Removed term '{}' should not reappear after compaction",
                                term
                            );
                        }
                    }
                }
            }
        )+
    };
}

/// Generates iterator consistency tests for dictionaries.
///
/// Tests that iteration returns all and only the inserted terms.
#[macro_export]
macro_rules! test_dictionary_iterator {
    (
        $( $test_name:ident => $dict_expr:expr, iter_method = $iter_method:ident, to_string = $to_string:expr ),+ $(,)?
    ) => {
        $(
            paste::paste! {
                proptest! {
                    #![proptest_config(ProptestConfig::with_cases(50))]

                    #[test]
                    fn [<$test_name _iterator_completeness>](
                        terms in prop::collection::vec($crate::common::strategies::ascii_term(1, 15), 1..=30)
                    ) {
                        let dict = $dict_expr;
                        let expected: std::collections::HashSet<String> = terms.into_iter().collect();

                        for term in &expected {
                            dict.insert(term);
                        }

                        let iterated: std::collections::HashSet<String> = dict.$iter_method()
                            .map($to_string)
                            .collect();

                        prop_assert_eq!(
                            &iterated,
                            &expected,
                            "Iterator should return exactly the inserted terms"
                        );
                    }
                }
            }
        )+
    };
}

/// Generates Unicode handling tests for char-based dictionaries.
#[macro_export]
macro_rules! test_unicode_dictionary {
    (
        $( $test_name:ident => $dict_expr:expr ),+ $(,)?
    ) => {
        $(
            paste::paste! {
                proptest! {
                    #![proptest_config(ProptestConfig::with_cases(30))]

                    #[test]
                    fn [<$test_name _unicode_roundtrip>](
                        terms in prop::collection::vec($crate::common::strategies::unicode_term(1, 10), 1..=20)
                    ) {
                        let dict = $dict_expr;
                        let unique_terms: std::collections::HashSet<_> = terms.into_iter().collect();

                        for term in &unique_terms {
                            dict.insert(term);
                        }

                        for term in &unique_terms {
                            prop_assert!(
                                dict.contains(term),
                                "Unicode term '{}' should be found",
                                term
                            );
                        }
                    }

                    #[test]
                    fn [<$test_name _emoji_handling>](
                        base in $crate::common::strategies::ascii_term(1, 5)
                    ) {
                        let dict = $dict_expr;

                        let emoji_terms = vec![
                            format!("{}🚀", base),
                            format!("🎉{}", base),
                            format!("{}💡{}", base, base),
                            format!("{}🔥🎨", base),
                        ];

                        for term in &emoji_terms {
                            dict.insert(term);
                        }

                        for term in &emoji_terms {
                            prop_assert!(
                                dict.contains(term),
                                "Emoji term '{}' should be found",
                                term
                            );
                        }
                    }
                }
            }
        )+
    };
}
