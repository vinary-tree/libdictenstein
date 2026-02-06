//! Property-based tests using generalized trait macros.
//!
//! This file demonstrates using the test macros to run identical property tests
//! across multiple dictionary implementations.
//!
//! Run with: cargo test --test proptest_trait_macros

#[macro_use]
mod common;

use common::strategies::*;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::dynamic_dawg_char::DynamicDawgChar;
use libdictenstein::MappedDictionary;
use proptest::prelude::*;
use std::collections::{HashMap, HashSet};

// =============================================================================
// MutableDictionary Tests via Macro
// =============================================================================

// Test insert/remove/contains for DAWG implementations
// Note: SuffixAutomaton has different removal semantics (substring-based matching
// means removal doesn't fully eliminate presence detection for substrings)
test_mutable_dictionary!(
    dawg => DynamicDawg::<()>::new(),
    dawg_char => DynamicDawgChar::<()>::new(),
);

// =============================================================================
// MutableMappedDictionary Tests via Macro
// =============================================================================

// Test value mapping for DAWG implementations
test_mapped_dictionary!(
    dawg_mapped => DynamicDawg::<u32>::new(),
    dawg_char_mapped => DynamicDawgChar::<u32>::new(),
);

// =============================================================================
// CompactableDictionary Tests via Macro
// =============================================================================

// Test compaction for DAWG implementations only
// Note: SuffixAutomaton compaction has different semantics
test_compactable_dictionary!(
    dawg_compact => DynamicDawg::<()>::new(),
);

// =============================================================================
// Unicode Dictionary Tests via Macro
// =============================================================================

// Test Unicode handling for char-based implementations
test_unicode_dictionary!(
    dawg_char_unicode => DynamicDawgChar::<()>::new(),
);

// =============================================================================
// Iterator Tests via Macro
// =============================================================================

// Test iterator completeness for DAWG implementations
test_dictionary_iterator!(
    dawg_iter => DynamicDawg::<()>::new(),
        iter_method = iter,
        to_string = |(term, _)| term,
);

// =============================================================================
// Cross-Implementation Consistency Tests (Manual)
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property: DAWG implementations agree on contains() after insertions
    #[test]
    fn cross_impl_contains_consistency(
        terms in prop::collection::vec(ascii_term(1, 15), 5..=30)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();

        let dawg: DynamicDawg<()> = DynamicDawg::new();
        let dawg_char: DynamicDawgChar<()> = DynamicDawgChar::new();

        for term in &unique_terms {
            dawg.insert(term);
            dawg_char.insert(term);
        }

        for term in &unique_terms {
            let dawg_result = dawg.contains(term);
            let dawg_char_result = dawg_char.contains(term);

            prop_assert!(dawg_result, "DynamicDawg should contain '{}'", term);
            prop_assert!(dawg_char_result, "DynamicDawgChar should contain '{}'", term);
        }
    }

    /// Property: DAWG implementations store values consistently
    #[test]
    fn cross_impl_value_consistency(
        pairs in prop::collection::vec((ascii_term(1, 15), any::<u32>()), 5..=30)
    ) {
        let expected: HashMap<String, u32> = pairs.into_iter().collect();

        let dawg: DynamicDawg<u32> = DynamicDawg::new();
        let dawg_char: DynamicDawgChar<u32> = DynamicDawgChar::new();

        for (term, value) in &expected {
            dawg.insert_with_value(term, *value);
            dawg_char.insert_with_value(term, *value);
        }

        for (term, expected_value) in &expected {
            prop_assert_eq!(
                dawg.get_value(term),
                Some(*expected_value),
                "DynamicDawg value mismatch for '{}'",
                term
            );
            prop_assert_eq!(
                dawg_char.get_value(term),
                Some(*expected_value),
                "DynamicDawgChar value mismatch for '{}'",
                term
            );
        }
    }

    /// Property: Removal is consistent across DAWG implementations
    #[test]
    fn cross_impl_removal_consistency(
        terms in prop::collection::vec(ascii_term(1, 15), 10..=30)
    ) {
        let unique_terms: Vec<_> = terms.into_iter().collect::<HashSet<_>>().into_iter().collect();

        let dawg: DynamicDawg<()> = DynamicDawg::new();
        let dawg_char: DynamicDawgChar<()> = DynamicDawgChar::new();

        // Insert all
        for term in &unique_terms {
            dawg.insert(term);
            dawg_char.insert(term);
        }

        // Remove first half
        let to_remove: Vec<_> = unique_terms.iter().take(unique_terms.len() / 2).cloned().collect();
        for term in &to_remove {
            dawg.remove(term);
            dawg_char.remove(term);
        }

        // Check consistency
        for term in &unique_terms {
            let should_exist = !to_remove.contains(term);

            prop_assert_eq!(
                dawg.contains(term),
                should_exist,
                "DynamicDawg consistency for '{}' (expected: {})",
                term,
                should_exist
            );
            prop_assert_eq!(
                dawg_char.contains(term),
                should_exist,
                "DynamicDawgChar consistency for '{}' (expected: {})",
                term,
                should_exist
            );
        }
    }
}
