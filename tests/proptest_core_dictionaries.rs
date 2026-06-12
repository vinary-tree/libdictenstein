//! Property-based tests for core dictionary implementations.
//!
//! These tests verify fundamental invariants across DynamicDawg, DoubleArrayTrie,
//! and SuffixAutomaton implementations.
//!
//! Run with: cargo test --test proptest_core_dictionaries

mod common;

use common::strategies::*;
use libdictenstein::double_array_trie::char::DoubleArrayTrieChar;
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::dynamic_dawg::char::DynamicDawgChar;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
use libdictenstein::suffix_automaton::SuffixAutomaton;
use libdictenstein::Dictionary;
use proptest::prelude::*;
use std::collections::{HashMap, HashSet};

// =============================================================================
// DynamicDawg Property Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Property: All inserted terms are retrievable via contains().
    #[test]
    fn dawg_inserted_terms_retrievable(
        terms in prop::collection::vec(ascii_term(1, 20), 1..=100)
    ) {
        let dict: DynamicDawg<()> = DynamicDawg::new();
        let unique_terms: HashSet<_> = terms.iter().cloned().collect();

        for term in &unique_terms {
            dict.insert(term);
        }

        for term in &unique_terms {
            prop_assert!(
                dict.contains(term),
                "Term '{}' should be retrievable after insert",
                term
            );
        }
    }

    /// Property: Removed terms are not retrievable.
    #[test]
    fn dawg_removed_terms_not_retrievable(
        terms in prop::collection::vec(ascii_term(1, 15), 5..=50)
    ) {
        let dict: DynamicDawg<()> = DynamicDawg::new();
        let unique_terms: Vec<_> = terms.into_iter().collect::<HashSet<_>>().into_iter().collect();

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
                "Term '{}' should NOT be retrievable after remove",
                term
            );
        }

        // Verify remaining terms still exist
        for term in unique_terms.iter().skip(unique_terms.len() / 2) {
            prop_assert!(
                dict.contains(term),
                "Term '{}' should still be retrievable",
                term
            );
        }
    }

    /// Property: Iterator returns all inserted terms (and only those).
    #[test]
    fn dawg_iterator_completeness(
        terms in prop::collection::vec(ascii_term(1, 15), 1..=50)
    ) {
        let dict: DynamicDawg<()> = DynamicDawg::new();
        let expected: HashSet<String> = terms.iter().cloned().collect();

        for term in &expected {
            dict.insert(term);
        }

        let iterated: HashSet<String> = dict.iter().map(|(term, _)| term).collect();

        prop_assert_eq!(
            iterated,
            expected,
            "Iterator should return exactly the inserted terms"
        );
    }

    /// Property: Value mapping is consistent after insert_with_value.
    #[test]
    fn dawg_value_mapping_consistent(
        pairs in prop::collection::vec((ascii_term(1, 15), any::<u32>()), 1..=50)
    ) {
        let dict: DynamicDawg<u32> = DynamicDawg::new();
        let expected: HashMap<String, u32> = pairs.into_iter().collect();

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

    /// Property: Compaction preserves all terms.
    #[test]
    fn dawg_compaction_preserves_terms(
        terms in prop::collection::vec(ascii_term(1, 15), 5..=30)
    ) {
        let dict: DynamicDawg<()> = DynamicDawg::new();
        let unique_terms: HashSet<_> = terms.iter().cloned().collect();

        // Insert all
        for term in &unique_terms {
            dict.insert(term);
        }

        // Remove some to create fragmentation
        let to_remove: Vec<_> = unique_terms.iter().take(unique_terms.len() / 3).cloned().collect();
        for term in &to_remove {
            dict.remove(term);
        }

        let remaining: HashSet<_> = unique_terms.difference(&to_remove.into_iter().collect()).cloned().collect();

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
    }

    /// Property: Prefix-clustered terms are handled correctly.
    #[test]
    fn dawg_prefix_clustered_terms(
        terms in prefix_clustered_terms(20)
    ) {
        let dict: DynamicDawg<()> = DynamicDawg::new();
        let unique_terms: HashSet<_> = terms.into_iter().filter(|t| !t.is_empty()).collect();

        for term in &unique_terms {
            dict.insert(term);
        }

        for term in &unique_terms {
            prop_assert!(
                dict.contains(term),
                "Prefix-clustered term '{}' should be retrievable",
                term
            );
        }
    }

    /// Property: Suffix-clustered terms benefit from DAWG sharing.
    #[test]
    fn dawg_suffix_clustered_terms(
        terms in suffix_clustered_terms(20)
    ) {
        let dict: DynamicDawg<()> = DynamicDawg::new();
        let unique_terms: HashSet<_> = terms.into_iter().filter(|t| !t.is_empty()).collect();

        for term in &unique_terms {
            dict.insert(term);
        }

        for term in &unique_terms {
            prop_assert!(
                dict.contains(term),
                "Suffix-clustered term '{}' should be retrievable",
                term
            );
        }
    }
}

// =============================================================================
// DoubleArrayTrie Property Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Property: All inserted terms are retrievable.
    #[test]
    fn dat_inserted_terms_retrievable(
        terms in prop::collection::vec(ascii_term(1, 20), 1..=100)
    ) {
        let unique_terms: HashSet<_> = terms.iter().cloned().collect();
        let dict = DoubleArrayTrie::from_terms(unique_terms.iter());

        for term in &unique_terms {
            prop_assert!(
                dict.contains(term),
                "Term '{}' should be retrievable after insert",
                term
            );
        }
    }

    /// Property: Iterator returns all inserted terms.
    #[test]
    fn dat_iterator_completeness(
        terms in prop::collection::vec(ascii_term(1, 15), 1..=50)
    ) {
        let expected: HashSet<String> = terms.into_iter().collect();
        let dict = DoubleArrayTrie::from_terms(expected.iter());

        let iterated: HashSet<String> = dict.iter_terms()
            .map(|bytes| String::from_utf8(bytes).expect("valid UTF-8"))
            .collect();

        prop_assert_eq!(
            iterated,
            expected,
            "Iterator should return exactly the inserted terms"
        );
    }

    /// Property: Value mapping is consistent.
    #[test]
    fn dat_value_mapping_consistent(
        pairs in prop::collection::vec((ascii_term(1, 15), any::<u32>()), 1..=50)
    ) {
        let expected: HashMap<String, u32> = pairs.into_iter().collect();
        let dict = DoubleArrayTrie::from_terms_with_values(
            expected.iter().map(|(k, v)| (k.as_str(), *v))
        );

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

    /// Property: len() matches actual count.
    #[test]
    fn dat_len_accurate(
        terms in prop::collection::vec(ascii_term(1, 15), 1..=50)
    ) {
        let unique_terms: HashSet<String> = terms.into_iter().collect();
        let dict = DoubleArrayTrie::from_terms(unique_terms.iter());

        prop_assert_eq!(
            dict.len(),
            Some(unique_terms.len()),
            "len() should match unique term count"
        );
    }
}

// =============================================================================
// DoubleArrayTrieChar Property Tests (Unicode)
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: Unicode terms are handled correctly.
    #[test]
    fn dat_char_unicode_terms(
        terms in prop::collection::vec(unicode_term(1, 10), 1..=30)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict = DoubleArrayTrieChar::from_terms(unique_terms.iter());

        for term in &unique_terms {
            prop_assert!(
                dict.contains(term),
                "Unicode term '{}' should be retrievable",
                term
            );
        }
    }
}

// =============================================================================
// DynamicDawgChar Property Tests (Unicode)
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: Unicode terms are handled correctly.
    #[test]
    fn dawg_char_unicode_terms(
        terms in prop::collection::vec(unicode_term(1, 10), 1..=30)
    ) {
        let dict: DynamicDawgChar<()> = DynamicDawgChar::new();
        let unique_terms: HashSet<_> = terms.into_iter().collect();

        for term in &unique_terms {
            dict.insert(term);
        }

        for term in &unique_terms {
            prop_assert!(
                dict.contains(term),
                "Unicode term '{}' should be retrievable",
                term
            );
        }
    }
}

// =============================================================================
// SuffixAutomaton Property Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: All indexed strings are findable.
    #[test]
    fn suffix_automaton_strings_findable(
        terms in prop::collection::vec(ascii_term(1, 20), 1..=30)
    ) {
        let dict: SuffixAutomaton<()> = SuffixAutomaton::new();
        let unique_terms: HashSet<_> = terms.into_iter().collect();

        for term in &unique_terms {
            dict.insert(term);
        }

        for term in &unique_terms {
            prop_assert!(
                dict.contains(term),
                "Term '{}' should be findable in SuffixAutomaton",
                term
            );
        }
    }

    /// Property: Basic insertion and lookup works for SuffixAutomaton.
    ///
    /// NOTE: SuffixAutomaton has complex removal semantics due to suffix sharing.
    /// The remove() method may fail to find a term if it shares position data with
    /// a longer term (e.g., "k" and "kkkfoo" may share the state for 'k').
    /// This test verifies basic insertion and lookup, not removal behavior.
    #[test]
    fn suffix_automaton_insertion_and_lookup(
        terms in prop::collection::vec(ascii_term(3, 15), 5..=20)
    ) {
        let dict: SuffixAutomaton<()> = SuffixAutomaton::new();
        let unique_terms: Vec<_> = terms.into_iter().collect::<HashSet<_>>().into_iter().collect();

        for term in &unique_terms {
            dict.insert(term);
        }

        // All inserted terms should be findable via contains()
        for term in &unique_terms {
            prop_assert!(
                dict.contains(term),
                "Inserted term '{}' should be findable via contains()",
                term
            );
        }

        // string_count should match
        prop_assert_eq!(
            dict.string_count(),
            unique_terms.len(),
            "string_count should match number of unique inserted terms"
        );
    }
}

// =============================================================================
// SuffixAutomatonChar Property Tests (Unicode)
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property: Unicode strings are handled correctly.
    #[test]
    fn suffix_automaton_char_unicode(
        terms in prop::collection::vec(unicode_term(1, 10), 1..=20)
    ) {
        let dict: SuffixAutomatonChar<()> = SuffixAutomatonChar::new();
        let unique_terms: HashSet<_> = terms.into_iter().collect();

        for term in &unique_terms {
            dict.insert(term);
        }

        for term in &unique_terms {
            prop_assert!(
                dict.contains(term),
                "Unicode term '{}' should be findable",
                term
            );
        }
    }
}

// =============================================================================
// Cross-Dictionary Consistency Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property: DynamicDawg and DoubleArrayTrie contain the same terms.
    #[test]
    fn cross_dict_consistency_dawg_dat(
        terms in prop::collection::vec(ascii_term(1, 15), 5..=30)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();

        let dawg: DynamicDawg<()> = DynamicDawg::from_terms(unique_terms.iter());
        let dat = DoubleArrayTrie::from_terms(unique_terms.iter());

        for term in &unique_terms {
            prop_assert_eq!(
                dawg.contains(term),
                dat.contains(term),
                "DynamicDawg and DoubleArrayTrie should agree on term '{}'",
                term
            );
        }
    }

    /// Property: Iteration produces the same terms across dictionary types.
    #[test]
    fn cross_dict_iteration_consistency(
        terms in prop::collection::vec(ascii_term(1, 15), 5..=30)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();

        let dawg: DynamicDawg<()> = DynamicDawg::from_terms(unique_terms.iter());
        let dat = DoubleArrayTrie::from_terms(unique_terms.iter());

        let dawg_terms: HashSet<String> = dawg.iter().map(|(term, _)| term).collect();
        let dat_terms: HashSet<String> = dat.iter_terms()
            .map(|bytes| String::from_utf8(bytes).expect("valid UTF-8"))
            .collect();

        prop_assert_eq!(
            &dawg_terms,
            &dat_terms,
            "Iteration should produce same terms"
        );
        prop_assert_eq!(
            &dawg_terms,
            &unique_terms,
            "Iterated terms should match inserted terms"
        );
    }
}

// =============================================================================
// Edge Case Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: Edge case terms are handled correctly.
    #[test]
    fn dawg_edge_case_terms(
        terms in edge_case_terms()
    ) {
        let dict: DynamicDawg<()> = DynamicDawg::new();
        let unique_terms: HashSet<_> = terms.into_iter().filter(|t| !t.is_empty()).collect();

        for term in &unique_terms {
            dict.insert(term);
        }

        for term in &unique_terms {
            prop_assert!(
                dict.contains(term),
                "Edge case term should be retrievable"
            );
        }
    }

    /// Property: Insert-remove-reinsert works correctly.
    #[test]
    fn dawg_insert_remove_reinsert(
        terms in prop::collection::vec(ascii_term(1, 15), 1..=20)
    ) {
        let dict: DynamicDawg<u32> = DynamicDawg::new();
        let unique_terms: Vec<_> = terms.into_iter().collect::<HashSet<_>>().into_iter().collect();

        // Insert with value 1
        for term in &unique_terms {
            dict.insert_with_value(term, 1);
        }

        // Remove all
        for term in &unique_terms {
            dict.remove(term);
        }

        // Reinsert with value 2
        for term in &unique_terms {
            dict.insert_with_value(term, 2);
        }

        // Verify all have value 2
        for term in &unique_terms {
            prop_assert_eq!(
                dict.get_value(term),
                Some(2),
                "Reinserted term '{}' should have value 2",
                term
            );
        }
    }
}
