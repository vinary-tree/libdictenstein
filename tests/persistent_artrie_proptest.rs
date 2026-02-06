//! Property-based tests for PersistentARTrie using proptest
//!
//! These tests verify invariants and discover edge cases for the
//! persistent ART implementation.
//!
//! Run with: cargo test --features persistent-artrie --test persistent_artrie_proptest

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::{Dictionary, MappedDictionary};
use proptest::prelude::*;
use std::collections::{HashMap, HashSet};
use tempfile::TempDir;

// Strategy for generating simple ASCII terms
fn ascii_term_strategy() -> impl Strategy<Value = String> {
    // Lowercase ASCII letters, 1-20 characters
    "[a-z]{1,20}"
}

// Strategy for generating terms with diverse first bytes (for bucket splitting)
fn diverse_term_strategy() -> impl Strategy<Value = String> {
    // First char from a-z, rest from a-z0-9, 2-15 chars total
    prop::string::string_regex("[a-z][a-z0-9]{1,14}")
        .expect("valid regex")
}

// Strategy for generating byte sequences (for fuzzing)
fn byte_term_strategy() -> impl Strategy<Value = Vec<u8>> {
    // Any printable ASCII bytes to avoid null bytes
    prop::collection::vec(32u8..127, 1..=30)
}

// Strategy for generating small dictionaries with values
fn small_dict_with_values_strategy() -> impl Strategy<Value = Vec<(String, i32)>> {
    prop::collection::vec((ascii_term_strategy(), any::<i32>()), 1..=50)
}

// Strategy for generating medium-sized dictionaries
fn medium_dict_strategy() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(diverse_term_strategy(), 10..=200)
}

// Strategy for operations (insert or remove)
#[derive(Debug, Clone)]
enum Operation {
    Insert(String, i32),
    Remove(String),
}

fn operation_strategy() -> impl Strategy<Value = Operation> {
    prop_oneof![
        // 70% inserts
        (ascii_term_strategy(), any::<i32>()).prop_map(|(t, v)| Operation::Insert(t, v)),
        // 30% removes
        ascii_term_strategy().prop_map(Operation::Remove),
    ]
}

fn operations_strategy() -> impl Strategy<Value = Vec<Operation>> {
    prop::collection::vec(operation_strategy(), 10..=100)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Property: All inserted items are retrievable
    #[test]
    fn prop_inserted_items_retrievable(
        terms in prop::collection::vec(ascii_term_strategy(), 1..=50)
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            // Deduplicate terms for testing
            let unique_terms: HashSet<_> = terms.iter().cloned().collect();

            // Insert all terms with values
            for (i, term) in unique_terms.iter().enumerate() {
                let _ = dict.insert_with_value(term, i as i32);
            }

            // Verify all terms are retrievable
            for (i, term) in unique_terms.iter().enumerate() {
                prop_assert!(
                    dict.contains(term),
                    "Term '{}' should be in dictionary after insert",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(term),
                    Some(i as i32),
                    "Term '{}' should have value {}",
                    term,
                    i
                );
            }
        }
    }

    /// Property: Removed items are not retrievable
    #[test]
    fn prop_removed_items_not_retrievable(
        terms in prop::collection::vec(ascii_term_strategy(), 1..=30)
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            // Deduplicate terms
            let unique_terms: Vec<_> = terms.iter().cloned().collect::<HashSet<_>>().into_iter().collect();

            // Insert all terms
            for (i, term) in unique_terms.iter().enumerate() {
                let _ = dict.insert_with_value(term, i as i32);
            }

            // Remove half the terms
            let to_remove: Vec<_> = unique_terms.iter().take(unique_terms.len() / 2).cloned().collect();
            for term in &to_remove {
                let _ = dict.remove(term);
            }

            // Verify removed terms are not retrievable
            for term in &to_remove {
                prop_assert!(
                    !dict.contains(term),
                    "Term '{}' should NOT be in dictionary after remove",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(term),
                    None,
                    "Term '{}' should NOT have a value after remove",
                    term
                );
            }

            // Verify remaining terms are still retrievable
            for (i, term) in unique_terms.iter().enumerate().skip(unique_terms.len() / 2) {
                prop_assert!(
                    dict.contains(term),
                    "Term '{}' should still be in dictionary after other removes",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(term),
                    Some(i as i32),
                    "Term '{}' should still have value {}",
                    term,
                    i
                );
            }
        }
    }

    /// Property: len() matches actual count of items
    #[test]
    fn prop_len_matches_count(
        operations in operations_strategy()
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            // Track expected state manually
            let mut expected: HashMap<String, i32> = HashMap::new();

            for op in operations {
                match op {
                    Operation::Insert(term, value) => {
                        expected.insert(term.clone(), value);
                        let _ = dict.insert_with_value(&term, value);
                    }
                    Operation::Remove(term) => {
                        expected.remove(&term);
                        let _ = dict.remove(&term);
                    }
                }
            }

            // Verify length matches expected
            prop_assert_eq!(
                dict.len(),
                Some(expected.len()),
                "len() should match expected count"
            );

            // Double-check by iterating
            for (term, value) in &expected {
                prop_assert!(
                    dict.contains(term),
                    "Term '{}' should be in dictionary",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(term),
                    Some(*value),
                    "Term '{}' should have value {}",
                    term,
                    value
                );
            }
        }
    }

    /// Property: Round-trip through checkpoint preserves data
    #[test]
    fn prop_checkpoint_roundtrip(
        terms in small_dict_with_values_strategy()
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        // Deduplicate and track expected state
        let expected: HashMap<String, i32> = terms.into_iter().collect();

        // Phase 1: Insert and checkpoint
        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            for (term, value) in &expected {
                let _ = dict.insert_with_value(term, *value);
            }

            dict.checkpoint().expect("checkpoint should succeed");
            dict.sync().expect("sync should succeed");
        }

        // Phase 2: Reopen and verify
        {
            let dict = PersistentARTrie::<i32>::open(&path)
                .expect("reopen dict");

            prop_assert_eq!(
                dict.len(),
                Some(expected.len()),
                "len() should match after reopen"
            );

            for (term, value) in &expected {
                prop_assert!(
                    dict.contains(term),
                    "Term '{}' should exist after checkpoint+reopen",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(term),
                    Some(*value),
                    "Term '{}' should have value {} after checkpoint+reopen",
                    term,
                    value
                );
            }
        }
    }

    /// Property: WAL recovery preserves data without checkpoint
    #[test]
    fn prop_wal_recovery_roundtrip(
        terms in small_dict_with_values_strategy()
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        // Deduplicate and track expected state
        let expected: HashMap<String, i32> = terms.into_iter().collect();

        // Phase 1: Insert and sync (no checkpoint)
        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            for (term, value) in &expected {
                let _ = dict.insert_with_value(term, *value);
            }

            dict.sync().expect("sync should succeed");
        }

        // Phase 2: Reopen and verify (recovery from WAL)
        {
            let dict = PersistentARTrie::<i32>::open(&path)
                .expect("reopen dict");

            prop_assert_eq!(
                dict.len(),
                Some(expected.len()),
                "len() should match after WAL recovery"
            );

            for (term, value) in &expected {
                prop_assert!(
                    dict.contains(term),
                    "Term '{}' should exist after WAL recovery",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(term),
                    Some(*value),
                    "Term '{}' should have value {} after WAL recovery",
                    term,
                    value
                );
            }
        }
    }

    /// Property: Byte sequence fuzzing - arbitrary bytes are handled correctly
    #[test]
    fn prop_byte_sequence_fuzzing(
        byte_terms in prop::collection::vec(byte_term_strategy(), 1..=30)
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        // Deduplicate byte sequences
        let unique_terms: Vec<_> = byte_terms.into_iter().collect::<HashSet<_>>().into_iter().collect();

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            // Insert all byte sequences as UTF-8 strings (lossy conversion)
            let mut expected: HashMap<String, i32> = HashMap::new();
            for (i, bytes) in unique_terms.iter().enumerate() {
                let term = String::from_utf8_lossy(bytes).to_string();
                expected.insert(term.clone(), i as i32);
                let _ = dict.insert_with_value(&term, i as i32);
            }

            // Verify all terms are retrievable
            for (term, value) in &expected {
                prop_assert!(
                    dict.contains(term),
                    "Byte-derived term should be in dictionary"
                );
                prop_assert_eq!(
                    dict.get_value(term),
                    Some(*value),
                    "Byte-derived term should have correct value"
                );
            }
        }
    }

    /// Property: Large-scale operations with diverse prefixes (triggers ART splitting)
    #[test]
    fn prop_large_scale_diverse_prefixes(
        terms in medium_dict_strategy()
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        // Deduplicate
        let unique_terms: HashSet<_> = terms.iter().cloned().collect();

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            // Insert all terms
            for (i, term) in unique_terms.iter().enumerate() {
                let _ = dict.insert_with_value(term, i as i32);
            }

            prop_assert_eq!(
                dict.len(),
                Some(unique_terms.len()),
                "len() should match unique term count"
            );

            // Verify all terms are retrievable
            for (i, term) in unique_terms.iter().enumerate() {
                prop_assert!(
                    dict.contains(term),
                    "Term '{}' should be in dictionary",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(term),
                    Some(i as i32),
                    "Term '{}' should have correct value",
                    term
                );
            }
        }
    }

    /// Property: Insert-remove-reinsert preserves final state
    #[test]
    fn prop_insert_remove_reinsert(
        terms in prop::collection::vec(ascii_term_strategy(), 1..=20)
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        let unique_terms: Vec<_> = terms.into_iter().collect::<HashSet<_>>().into_iter().collect();

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            // Phase 1: Insert with value 1
            for term in &unique_terms {
                let _ = dict.insert_with_value(term, 1);
            }

            // Phase 2: Remove all
            for term in &unique_terms {
                let _ = dict.remove(term);
            }

            // Should be empty
            prop_assert_eq!(dict.len(), Some(0), "Dictionary should be empty after removes");

            // Phase 3: Reinsert with value 2
            for term in &unique_terms {
                let _ = dict.insert_with_value(term, 2);
            }

            // All terms should have value 2
            for term in &unique_terms {
                prop_assert!(
                    dict.contains(term),
                    "Term '{}' should exist after reinsert",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(term),
                    Some(2),
                    "Term '{}' should have value 2 after reinsert",
                    term
                );
            }
        }
    }

    /// Property: Multiple checkpoints maintain consistency
    #[test]
    fn prop_multiple_checkpoints(
        batches in prop::collection::vec(
            prop::collection::vec(ascii_term_strategy(), 1..=10),
            1..=5
        )
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        let mut all_terms: HashSet<String> = HashSet::new();

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            for (batch_idx, batch) in batches.iter().enumerate() {
                // Insert batch
                for term in batch {
                    all_terms.insert(term.clone());
                    let _ = dict.insert_with_value(term, batch_idx as i32);
                }

                // Checkpoint after each batch
                dict.checkpoint().expect("checkpoint should succeed");

                // Verify all terms so far
                prop_assert_eq!(
                    dict.len(),
                    Some(all_terms.len()),
                    "len() should match after batch {} checkpoint",
                    batch_idx
                );
            }
        }

        // Reopen and verify final state
        {
            let dict = PersistentARTrie::<i32>::open(&path)
                .expect("reopen dict");

            prop_assert_eq!(
                dict.len(),
                Some(all_terms.len()),
                "len() should match after reopen"
            );

            for term in &all_terms {
                prop_assert!(
                    dict.contains(term),
                    "Term '{}' should exist after multiple checkpoints",
                    term
                );
            }
        }
    }
}

// =============================================================================
// Node Type Transition Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    /// Property: Inserting many terms with distinct first bytes triggers Node4 -> Node16 -> Node48 -> Node256
    #[test]
    fn prop_node_type_transitions(
        prefixes in prop::collection::vec(prop::char::range('a', 'z'), 50..=100)
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            // Insert terms with diverse prefixes to trigger node growth
            let unique_prefixes: HashSet<char> = prefixes.into_iter().collect();
            for (i, prefix) in unique_prefixes.iter().enumerate() {
                let term = format!("{}suffix{}", prefix, i);
                let _ = dict.insert_with_value(&term, i as i32);
            }

            // All terms should be retrievable regardless of internal node type
            for (i, prefix) in unique_prefixes.iter().enumerate() {
                let term = format!("{}suffix{}", prefix, i);
                prop_assert!(
                    dict.contains(&term),
                    "Term '{}' should exist after node transitions",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(&term),
                    Some(i as i32),
                    "Term '{}' should have correct value",
                    term
                );
            }
        }
    }

    /// Property: Dense first-byte distribution (Node256 scenario)
    #[test]
    fn prop_dense_first_byte_distribution(
        _dummy in 0..1i32
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            // Insert terms starting with every lowercase letter
            for (i, c) in ('a'..='z').enumerate() {
                let term = format!("{}term", c);
                let _ = dict.insert_with_value(&term, i as i32);
            }

            // All should be retrievable
            for (i, c) in ('a'..='z').enumerate() {
                let term = format!("{}term", c);
                prop_assert!(dict.contains(&term), "Term '{}' should exist", term);
                prop_assert_eq!(dict.get_value(&term), Some(i as i32));
            }

            prop_assert_eq!(dict.len(), Some(26));
        }
    }
}

// =============================================================================
// Bucket Split/Merge Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    /// Property: Bucket splits maintain data integrity
    #[test]
    fn prop_bucket_split_integrity(
        terms in prop::collection::vec(
            prop::string::string_regex("[a-z]{1,3}[0-9]{1,5}").expect("valid regex"),
            100..=200
        )
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        let unique_terms: HashSet<_> = terms.iter().cloned().collect();

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            // Insert many terms to trigger bucket splits
            for (i, term) in unique_terms.iter().enumerate() {
                let _ = dict.insert_with_value(term, i as i32);
            }

            // Verify all terms survived splits
            for (i, term) in unique_terms.iter().enumerate() {
                prop_assert!(
                    dict.contains(term),
                    "Term '{}' should exist after bucket splits",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(term),
                    Some(i as i32),
                    "Term '{}' should have correct value",
                    term
                );
            }
        }
    }

    /// Property: Removing terms maintains bucket integrity
    #[test]
    fn prop_bucket_removal_integrity(
        terms in prop::collection::vec(
            prop::string::string_regex("[a-z]{2,5}").expect("valid regex"),
            50..=100
        )
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        let unique_terms: Vec<_> = terms.into_iter().collect::<HashSet<_>>().into_iter().collect();

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            // Insert all
            for (i, term) in unique_terms.iter().enumerate() {
                let _ = dict.insert_with_value(term, i as i32);
            }

            // Remove half
            let to_remove: Vec<_> = unique_terms.iter().take(unique_terms.len() / 2).cloned().collect();
            for term in &to_remove {
                let _ = dict.remove(term);
            }

            // Verify correct state
            for term in &to_remove {
                prop_assert!(!dict.contains(term), "Removed term should not exist");
            }

            for (i, term) in unique_terms.iter().enumerate().skip(unique_terms.len() / 2) {
                prop_assert!(
                    dict.contains(term),
                    "Remaining term '{}' should exist",
                    term
                );
                prop_assert_eq!(
                    dict.get_value(term),
                    Some(i as i32),
                    "Remaining term '{}' should have correct value",
                    term
                );
            }
        }
    }
}

// =============================================================================
// Iteration Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property: Iterator returns all inserted terms
    #[test]
    fn prop_iterator_completeness(
        terms in prop::collection::vec(ascii_term_strategy(), 1..=50)
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        let expected: HashSet<String> = terms.iter().cloned().collect();

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            for (i, term) in expected.iter().enumerate() {
                let _ = dict.insert_with_value(term, i as i32);
            }

            // Collect all terms via iteration
            let iterated: HashSet<String> = dict.iter_strings().collect();

            prop_assert_eq!(
                iterated,
                expected,
                "Iterator should return all inserted terms"
            );
        }
    }

    /// Property: Iterator with values returns correct mappings
    #[test]
    fn prop_iterator_with_values(
        pairs in prop::collection::vec((ascii_term_strategy(), any::<i32>()), 1..=30)
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        let expected: std::collections::HashMap<String, i32> = pairs.into_iter().collect();

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            for (term, value) in &expected {
                let _ = dict.insert_with_value(term, *value);
            }

            // Verify via iteration
            for (term, value) in dict.iter_with_values() {
                let term_str = String::from_utf8(term).expect("valid utf8");
                prop_assert!(
                    expected.contains_key(&term_str),
                    "Iterated term should be in expected set"
                );
                prop_assert_eq!(
                    value,
                    expected.get(&term_str).copied(),
                    "Iterated value should match"
                );
            }
        }
    }
}

// =============================================================================
// Prefix Search Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Property: Prefix search returns all matching terms
    #[test]
    fn prop_prefix_search_completeness(
        prefix in prop::string::string_regex("[b-y]{1,3}").expect("valid prefix regex"),
        suffixes in prop::collection::vec(prop::string::string_regex("[a-z]{1,5}").expect("valid suffix regex"), 5..=20)
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        let matching_terms: HashSet<String> = suffixes
            .iter()
            .map(|s| format!("{}{}", prefix, s))
            .collect();

        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            // Insert matching terms
            for (i, term) in matching_terms.iter().enumerate() {
                let _ = dict.insert_with_value(term, i as i32);
            }

            // Insert some non-matching terms
            let _ = dict.insert_with_value("zzz_nonmatch", 999);
            let _ = dict.insert_with_value("aaa_nonmatch", 998);

            // Prefix search should find all matching terms
            let found: HashSet<String> = dict.iter_prefix(prefix.as_bytes())
                .map(|iter| iter
                    .map(|bytes| String::from_utf8(bytes).expect("valid utf8"))
                    .collect())
                .unwrap_or_default();

            for term in &matching_terms {
                prop_assert!(
                    found.contains(term),
                    "Prefix search should find '{}'",
                    term
                );
            }

            // Should not find non-matching terms
            prop_assert!(!found.contains("zzz_nonmatch"));
            prop_assert!(!found.contains("aaa_nonmatch"));
        }
    }
}

// =============================================================================
// Crash Recovery Simulation Tests
// =============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10))]

    /// Property: WAL replay recovers uncommitted changes
    #[test]
    fn prop_wal_replay_recovery(
        initial_terms in prop::collection::vec(ascii_term_strategy(), 5..=20),
        additional_terms in prop::collection::vec(ascii_term_strategy(), 5..=20)
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        let initial_set: HashSet<String> = initial_terms.into_iter().collect();
        let additional_set: HashSet<String> = additional_terms.into_iter().collect();

        // Phase 1: Insert initial terms and checkpoint
        {
            let mut dict = PersistentARTrie::<i32>::create(&path)
                .expect("create dict");

            for (i, term) in initial_set.iter().enumerate() {
                let _ = dict.insert_with_value(term, i as i32);
            }

            dict.checkpoint().expect("checkpoint");
        }

        // Phase 2: Insert additional terms WITHOUT checkpoint (simulate crash before checkpoint)
        {
            let mut dict = PersistentARTrie::<i32>::open(&path)
                .expect("open dict");

            for (i, term) in additional_set.iter().enumerate() {
                let _ = dict.insert_with_value(term, (initial_set.len() + i) as i32);
            }

            dict.sync().expect("sync WAL");
            // Note: No checkpoint - data is only in WAL
        }

        // Phase 3: Reopen and verify WAL replay recovered additional terms
        {
            let dict = PersistentARTrie::<i32>::open(&path)
                .expect("reopen dict after simulated crash");

            // Initial terms should be present (from checkpoint)
            for term in &initial_set {
                prop_assert!(
                    dict.contains(term),
                    "Initial term '{}' should be recovered",
                    term
                );
            }

            // Additional terms should also be present (from WAL replay)
            for term in &additional_set {
                prop_assert!(
                    dict.contains(term),
                    "WAL term '{}' should be recovered",
                    term
                );
            }
        }
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    /// Regression test: Empty dictionary should have len() = 0
    #[test]
    fn regression_empty_dict_len() {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        let dict = PersistentARTrie::<i32>::create(&path).expect("create dict");
        assert_eq!(dict.len(), Some(0));
    }

    /// Regression test: Single term insert/contains/remove cycle
    #[test]
    fn regression_single_term_lifecycle() {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        let mut dict = PersistentARTrie::<i32>::create(&path).expect("create dict");

        assert!(!dict.contains("test"));
        let _ = dict.insert_with_value("test", 42);
        assert!(dict.contains("test"));
        assert_eq!(dict.get_value("test"), Some(42));
        let _ = dict.remove("test");
        assert!(!dict.contains("test"));
        assert_eq!(dict.get_value("test"), None);
    }

    /// Regression test: Terms with common prefix
    #[test]
    fn regression_common_prefix_terms() {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        let mut dict = PersistentARTrie::<i32>::create(&path).expect("create dict");

        let _ = dict.insert_with_value("test", 1);
        let _ = dict.insert_with_value("testing", 2);
        let _ = dict.insert_with_value("tester", 3);
        let _ = dict.insert_with_value("tested", 4);

        assert_eq!(dict.len(), Some(4));
        assert_eq!(dict.get_value("test"), Some(1));
        assert_eq!(dict.get_value("testing"), Some(2));
        assert_eq!(dict.get_value("tester"), Some(3));
        assert_eq!(dict.get_value("tested"), Some(4));
    }

    /// Regression test: Value update (insert same key twice)
    #[test]
    fn regression_value_update() {
        let temp_dir = TempDir::new().expect("temp dir");
        let path = temp_dir.path().join("test_dict");

        let mut dict = PersistentARTrie::<i32>::create(&path).expect("create dict");

        let _ = dict.insert_with_value("key", 1);
        assert_eq!(dict.get_value("key"), Some(1));

        let _ = dict.insert_with_value("key", 2);
        assert_eq!(dict.get_value("key"), Some(2));

        // Length should still be 1 (same key, updated value)
        assert_eq!(dict.len(), Some(1));
    }
}
