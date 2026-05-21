//! Property-based tests for dictionary serialization.
//!
//! These tests verify roundtrip serialization correctness for all dictionary types.
//!
//! Run with: cargo test --features serialization --test proptest_serialization

#![cfg(feature = "serialization")]

mod common;

use common::strategies::*;
use libdictenstein::double_array_trie::DoubleArrayTrie;
use libdictenstein::dynamic_dawg::DynamicDawg;
use libdictenstein::serialization::{BincodeSerializer, DictionarySerializer, JsonSerializer};
use libdictenstein::Dictionary;
use proptest::prelude::*;
use std::collections::HashSet;

// =============================================================================
// DynamicDawg Serialization Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: DynamicDawg roundtrip preserves all terms
    #[test]
    fn dawg_serialization_roundtrip(
        terms in prop::collection::vec(ascii_term(1, 15), 1..=50)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict: DynamicDawg<()> = DynamicDawg::from_terms(unique_terms.iter());

        // Serialize
        let mut buffer = Vec::new();
        BincodeSerializer::serialize(&dict, &mut buffer).expect("serialization should succeed");

        // Deserialize
        let restored: DynamicDawg<()> = BincodeSerializer::deserialize(&buffer[..])
            .expect("deserialization should succeed");

        // Verify all terms present
        for term in &unique_terms {
            prop_assert!(
                restored.contains(term),
                "Term '{}' should be present after roundtrip",
                term
            );
        }

        // Verify length matches
        prop_assert_eq!(
            dict.len(),
            restored.len(),
            "Length should match after roundtrip"
        );
    }
}

// =============================================================================
// DoubleArrayTrie Serialization Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// Property: DoubleArrayTrie roundtrip preserves all terms
    #[test]
    fn dat_serialization_roundtrip(
        terms in prop::collection::vec(ascii_term(1, 15), 1..=50)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict = DoubleArrayTrie::from_terms(unique_terms.iter());

        // Serialize
        let mut buffer = Vec::new();
        BincodeSerializer::serialize(&dict, &mut buffer).expect("serialization should succeed");

        // Deserialize
        let restored: DoubleArrayTrie = BincodeSerializer::deserialize(&buffer[..])
            .expect("deserialization should succeed");

        // Verify all terms present
        for term in &unique_terms {
            prop_assert!(
                restored.contains(term),
                "Term '{}' should be present after roundtrip",
                term
            );
        }

        // Verify length matches
        prop_assert_eq!(
            dict.len(),
            restored.len(),
            "Length should match after roundtrip"
        );
    }
}

// =============================================================================
// JSON Serialization Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property: JSON serialization roundtrip works
    #[test]
    fn json_serialization_roundtrip(
        terms in prop::collection::vec(ascii_term(1, 15), 1..=30)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict = DoubleArrayTrie::from_terms(unique_terms.iter());

        // Serialize to JSON
        let mut buffer = Vec::new();
        JsonSerializer::serialize(&dict, &mut buffer).expect("JSON serialization should succeed");

        // Deserialize from JSON
        let restored: DoubleArrayTrie = JsonSerializer::deserialize(&buffer[..])
            .expect("JSON deserialization should succeed");

        // Verify all terms present
        for term in &unique_terms {
            prop_assert!(
                restored.contains(term),
                "Term '{}' should be present after JSON roundtrip",
                term
            );
        }
    }
}

// =============================================================================
// Large Dictionary Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10))]

    /// Property: Large dictionary serialization works correctly
    #[test]
    fn large_dict_serialization(
        terms in prop::collection::vec(ascii_term(1, 20), 500..=1000)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();
        let dict: DynamicDawg<()> = DynamicDawg::from_terms(unique_terms.iter());

        // Serialize
        let mut buffer = Vec::new();
        BincodeSerializer::serialize(&dict, &mut buffer).expect("serialization should succeed");

        // Buffer should be non-empty
        prop_assert!(!buffer.is_empty(), "Serialized buffer should not be empty");

        // Deserialize
        let restored: DynamicDawg<()> = BincodeSerializer::deserialize(&buffer[..])
            .expect("deserialization should succeed");

        // Verify length matches
        prop_assert_eq!(
            dict.len(),
            restored.len(),
            "Length should match after roundtrip"
        );

        // Sample check (checking all would be too slow)
        for term in unique_terms.iter().take(100) {
            prop_assert!(
                restored.contains(term),
                "Sampled term '{}' should be present",
                term
            );
        }
    }
}

// =============================================================================
// Cross-Backend Serialization Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    /// Property: Different backends serialize/deserialize to same logical content
    #[test]
    fn cross_backend_serialization_consistency(
        terms in prop::collection::vec(ascii_term(1, 15), 5..=30)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().collect();

        // Create both backends with same data
        let dawg: DynamicDawg<()> = DynamicDawg::from_terms(unique_terms.iter());
        let dat = DoubleArrayTrie::from_terms(unique_terms.iter());

        // Serialize and deserialize each
        let mut dawg_buffer = Vec::new();
        BincodeSerializer::serialize(&dawg, &mut dawg_buffer).expect("dawg serialize");
        let dawg_restored: DynamicDawg<()> = BincodeSerializer::deserialize(&dawg_buffer[..]).expect("dawg deserialize");

        let mut dat_buffer = Vec::new();
        BincodeSerializer::serialize(&dat, &mut dat_buffer).expect("dat serialize");
        let dat_restored: DoubleArrayTrie = BincodeSerializer::deserialize(&dat_buffer[..]).expect("dat deserialize");

        // Both should contain the same terms
        for term in &unique_terms {
            prop_assert!(
                dawg_restored.contains(term),
                "DynamicDawg should contain '{}'",
                term
            );
            prop_assert!(
                dat_restored.contains(term),
                "DoubleArrayTrie should contain '{}'",
                term
            );
        }
    }
}

// =============================================================================
// Edge Case Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    /// Property: Empty dictionary serialization roundtrip
    #[test]
    fn empty_dict_serialization(_dummy in 0..1i32) {
        let dict: DynamicDawg<()> = DynamicDawg::new();

        let mut buffer = Vec::new();
        BincodeSerializer::serialize(&dict, &mut buffer).expect("serialization should succeed");

        let restored: DynamicDawg<()> = BincodeSerializer::deserialize(&buffer[..])
            .expect("deserialization should succeed");

        prop_assert_eq!(restored.len(), Some(0), "Empty dict should remain empty");
    }

    /// Property: Single term dictionary serialization roundtrip
    #[test]
    fn single_term_serialization(term in ascii_term(1, 15)) {
        let dict: DynamicDawg<()> = DynamicDawg::from_terms(std::iter::once(&term));

        let mut buffer = Vec::new();
        BincodeSerializer::serialize(&dict, &mut buffer).expect("serialization should succeed");

        let restored: DynamicDawg<()> = BincodeSerializer::deserialize(&buffer[..])
            .expect("deserialization should succeed");

        prop_assert!(restored.contains(&term), "Single term should survive roundtrip");
        prop_assert_eq!(restored.len(), Some(1), "Length should be 1");
    }

    /// Property: Terms with common prefixes serialize correctly
    #[test]
    fn prefix_sharing_serialization(
        terms in prefix_clustered_terms(20)
    ) {
        let unique_terms: HashSet<_> = terms.into_iter().filter(|t| !t.is_empty()).collect();
        let dict: DynamicDawg<()> = DynamicDawg::from_terms(unique_terms.iter());

        let mut buffer = Vec::new();
        BincodeSerializer::serialize(&dict, &mut buffer).expect("serialization should succeed");

        let restored: DynamicDawg<()> = BincodeSerializer::deserialize(&buffer[..])
            .expect("deserialization should succeed");

        for term in &unique_terms {
            prop_assert!(
                restored.contains(term),
                "Prefix-clustered term '{}' should survive roundtrip",
                term
            );
        }
    }
}
