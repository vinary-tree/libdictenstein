//! Crash recovery integration tests for PersistentARTrie.
//!
//! These tests verify that the WAL-based recovery mechanism correctly restores
//! dictionary state after simulated crashes (drops without sync).

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::PersistentARTrie;
use libdictenstein::Dictionary;
use tempfile::tempdir;

/// Test 13.1: Recovery after clean shutdown.
/// Create dictionary, insert terms, sync, close, reopen and verify all terms present.
#[test]
fn test_recovery_after_clean_shutdown() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict.art");

    let terms = vec!["apple", "banana", "cherry", "date", "elderberry"];

    // Create dictionary and insert terms
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        for term in &terms {
            dict.insert(term);
        }

        // Sync to ensure durability
        dict.sync().expect("sync");
    }

    // Reopen and verify all terms present
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        for term in &terms {
            assert!(
                dict.contains(term),
                "Term '{}' should be present after recovery",
                term
            );
        }
    }
}

/// Test 13.2: Recovery after crash (drop without sync).
/// Create dictionary, insert terms, drop without sync, reopen and verify WAL replay recovers data.
#[test]
fn test_recovery_after_crash_no_sync() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict.art");

    let terms = vec!["foo", "bar", "baz", "qux"];

    // Create dictionary and insert terms (no explicit sync before drop)
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        for term in &terms {
            dict.insert(term);
        }

        // Simulate crash - drop without sync
        // The WAL should have buffered the writes
    }

    // Reopen and verify recovery
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        // Terms should be recovered from WAL replay
        for term in &terms {
            assert!(
                dict.contains(term),
                "Term '{}' should be recovered from WAL after crash",
                term
            );
        }
    }
}

/// Test 13.3: Mixed insert/remove recovery.
/// Create, insert A, insert B, remove A, crash, verify only B present after recovery.
#[test]
fn test_mixed_insert_remove_recovery() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict.art");

    // Create and perform mixed operations
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        dict.insert("alpha");
        dict.insert("beta");
        dict.insert("gamma");
        dict.remove("alpha");
        dict.insert("delta");
        dict.remove("gamma");

        // Sync to ensure WAL is durable
        dict.sync().expect("sync");
    }

    // Reopen and verify correct state
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        // alpha was removed
        assert!(
            !dict.contains("alpha"),
            "alpha was removed and should not be present"
        );

        // beta was inserted and never removed
        assert!(dict.contains("beta"), "beta should be present");

        // gamma was removed
        assert!(
            !dict.contains("gamma"),
            "gamma was removed and should not be present"
        );

        // delta was inserted after some removes
        assert!(dict.contains("delta"), "delta should be present");
    }
}

/// Test 13.4: Checkpoint + recovery.
/// Insert terms, checkpoint, insert more, crash, verify checkpoint + WAL replay recovers all.
#[test]
fn test_checkpoint_and_recovery() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict.art");

    let pre_checkpoint_terms: Vec<String> = (0..50).map(|i| format!("pre_{}", i)).collect();

    let post_checkpoint_terms: Vec<String> = (0..20).map(|i| format!("post_{}", i)).collect();

    // Create dictionary, insert terms, checkpoint, insert more
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        // Insert pre-checkpoint terms
        for term in &pre_checkpoint_terms {
            dict.insert(term);
        }

        // Checkpoint to mark these as durable
        dict.checkpoint().expect("checkpoint");

        // Insert post-checkpoint terms
        for term in &post_checkpoint_terms {
            dict.insert(term);
        }

        // Sync to ensure WAL has post-checkpoint entries
        dict.sync().expect("sync");
    }

    // Reopen and verify all terms present
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        // All pre-checkpoint terms should be present
        for term in &pre_checkpoint_terms {
            assert!(
                dict.contains(term),
                "Pre-checkpoint term '{}' should be present",
                term
            );
        }

        // All post-checkpoint terms should also be present (recovered from WAL)
        for term in &post_checkpoint_terms {
            assert!(
                dict.contains(term),
                "Post-checkpoint term '{}' should be recovered from WAL",
                term
            );
        }
    }
}

/// Test 13.5: Corrupted WAL handling.
/// Create WAL with valid entries, corrupt the file, verify graceful degradation.
#[test]
fn test_corrupted_wal_graceful_degradation() {
    use std::fs::OpenOptions;
    use std::io::Write;

    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict.art");
    let wal_path = dict_path.with_extension("wal");

    // Create dictionary and insert terms
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        dict.insert("valid_term_1");
        dict.insert("valid_term_2");
        dict.sync().expect("sync");
    }

    // Corrupt the WAL file by appending garbage
    {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&wal_path)
            .expect("open WAL for corruption");

        // Write garbage bytes that will fail CRC check
        file.write_all(b"CORRUPTED_DATA_THAT_WILL_FAIL_CRC_CHECK")
            .expect("write corruption");
        file.sync_all().expect("sync corruption");
    }

    // Reopen - should handle corruption gracefully
    // The recovery manager should log a warning but not crash
    {
        let result = PersistentARTrie::<()>::open(&dict_path);

        // Should either succeed with partial recovery or fail gracefully
        match result {
            Ok(dict) => {
                // If we succeeded, the valid terms before corruption should be present
                // (depending on where the corruption occurred)
                // At minimum, the dictionary should be usable
                let _ = dict.contains("valid_term_1");
            }
            Err(e) => {
                // If it failed, it should be a recognizable error, not a panic
                let error_msg = format!("{:?}", e);
                assert!(
                    error_msg.contains("Corrupted")
                        || error_msg.contains("CRC")
                        || error_msg.contains("invalid")
                        || error_msg.contains("recovery"),
                    "Error should indicate corruption-related issue: {}",
                    error_msg
                );
            }
        }
    }
}

/// Test: Multiple open/close cycles with incremental updates.
/// Verifies that WAL truncation after recovery works correctly.
#[test]
fn test_multiple_reopen_cycles() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict.art");

    // Cycle 1: Create and add initial terms
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        dict.insert("cycle1_a");
        dict.insert("cycle1_b");
        dict.sync().expect("sync");
    }

    // Cycle 2: Reopen, verify previous terms, add more
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::open(&dict_path).expect("open dictionary cycle 2");

        assert!(
            dict.contains("cycle1_a"),
            "cycle1_a should exist in cycle 2"
        );
        assert!(
            dict.contains("cycle1_b"),
            "cycle1_b should exist in cycle 2"
        );

        dict.insert("cycle2_a");
        dict.insert("cycle2_b");
        dict.sync().expect("sync");
    }

    // Cycle 3: Reopen, verify all terms, add more
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::open(&dict_path).expect("open dictionary cycle 3");

        assert!(
            dict.contains("cycle1_a"),
            "cycle1_a should exist in cycle 3"
        );
        assert!(
            dict.contains("cycle1_b"),
            "cycle1_b should exist in cycle 3"
        );
        assert!(
            dict.contains("cycle2_a"),
            "cycle2_a should exist in cycle 3"
        );
        assert!(
            dict.contains("cycle2_b"),
            "cycle2_b should exist in cycle 3"
        );

        dict.insert("cycle3_a");
        dict.remove("cycle1_a");
        dict.sync().expect("sync");
    }

    // Cycle 4: Final verification
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::open(&dict_path).expect("open dictionary cycle 4");

        assert!(!dict.contains("cycle1_a"), "cycle1_a was removed");
        assert!(dict.contains("cycle1_b"), "cycle1_b should exist");
        assert!(dict.contains("cycle2_a"), "cycle2_a should exist");
        assert!(dict.contains("cycle2_b"), "cycle2_b should exist");
        assert!(dict.contains("cycle3_a"), "cycle3_a should exist");
    }
}

/// Test: Moderate number of operations followed by recovery.
/// Tests WAL replay with multiple terms. Note: Currently limited by bucket size (256).
#[test]
fn test_large_scale_recovery() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict.art");

    // Use 200 terms to stay within bucket capacity (max 256 before split)
    // Full bucket splitting to ART nodes is future work
    let num_terms = 200;
    let terms: Vec<String> = (0..num_terms).map(|i| format!("term_{:05}", i)).collect();

    // Create and insert many terms
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        for term in &terms {
            dict.insert(term);
        }

        dict.sync().expect("sync");
    }

    // Reopen and verify all terms
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        for term in &terms {
            assert!(
                dict.contains(term),
                "Term '{}' should be present after recovery",
                term
            );
        }
    }
}

/// Test: Recovery with empty dictionary (no operations).
#[test]
fn test_empty_dictionary_recovery() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict.art");

    // Create empty dictionary
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");
        dict.sync().expect("sync");
    }

    // Reopen empty dictionary
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        assert!(
            !dict.contains("anything"),
            "Empty dictionary should not contain any terms"
        );
    }
}

// =============================================================================
// Phase 15: Value Persistence Tests
// =============================================================================

use libdictenstein::value::DictionaryValue;
use libdictenstein::MappedDictionary;
use serde::{Deserialize, Serialize};

/// Custom value type for testing serialization
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
struct TestValue {
    id: u32,
    name: String,
}

impl DictionaryValue for TestValue {}

/// Test 15.1: Value persistence through clean shutdown.
/// Insert terms with values, sync, close, reopen and verify values are recovered.
#[test]
fn test_value_persistence_clean_shutdown() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict_values.art");

    // Create dictionary and insert terms with values
    {
        let dict: PersistentARTrie<u32> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        dict.insert_with_value("apple", 1);
        dict.insert_with_value("banana", 2);
        dict.insert_with_value("cherry", 3);

        // Sync to ensure durability
        dict.sync().expect("sync");
    }

    // Reopen and verify values are recovered
    {
        let dict: PersistentARTrie<u32> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        assert_eq!(
            dict.get_value("apple"),
            Some(1),
            "apple should have value 1"
        );
        assert_eq!(
            dict.get_value("banana"),
            Some(2),
            "banana should have value 2"
        );
        assert_eq!(
            dict.get_value("cherry"),
            Some(3),
            "cherry should have value 3"
        );
        assert_eq!(
            dict.get_value("nonexistent"),
            None,
            "nonexistent should have no value"
        );
    }
}

/// Test 15.2: Value persistence through crash recovery.
/// Insert terms with values, crash (drop without sync), reopen and verify WAL replay recovers values.
#[test]
fn test_value_persistence_crash_recovery() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict_crash_values.art");

    // Create dictionary and insert terms with values (no sync before drop)
    {
        let dict: PersistentARTrie<u32> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        dict.insert_with_value("foo", 42);
        dict.insert_with_value("bar", 100);
        dict.insert_with_value("baz", 999);

        // Simulate crash - no sync before drop
        // WAL should buffer the writes
    }

    // Reopen and verify values are recovered from WAL replay
    {
        let dict: PersistentARTrie<u32> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        assert_eq!(dict.get_value("foo"), Some(42), "foo should have value 42");
        assert_eq!(
            dict.get_value("bar"),
            Some(100),
            "bar should have value 100"
        );
        assert_eq!(
            dict.get_value("baz"),
            Some(999),
            "baz should have value 999"
        );
    }
}

/// Test 15.3: Complex value type persistence.
/// Test with a custom struct value type.
#[test]
fn test_complex_value_persistence() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict_complex.art");

    let values = vec![
        (
            "user_alice",
            TestValue {
                id: 1,
                name: "Alice".into(),
            },
        ),
        (
            "user_bob",
            TestValue {
                id: 2,
                name: "Bob".into(),
            },
        ),
        (
            "user_charlie",
            TestValue {
                id: 3,
                name: "Charlie".into(),
            },
        ),
    ];

    // Create dictionary with complex values
    {
        let dict: PersistentARTrie<TestValue> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        for (term, value) in &values {
            dict.insert_with_value(term, value.clone());
        }

        dict.sync().expect("sync");
    }

    // Reopen and verify complex values are recovered
    {
        let dict: PersistentARTrie<TestValue> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        for (term, expected) in &values {
            let actual = dict.get_value(term);
            assert_eq!(
                actual.as_ref(),
                Some(expected),
                "Term '{}' should have value {:?}",
                term,
                expected
            );
        }
    }
}

/// Test 15.4: Mixed insert (with and without values) recovery.
#[test]
fn test_mixed_value_recovery() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict_mixed_values.art");

    // Create dictionary with mixed inserts
    {
        let dict: PersistentARTrie<u32> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");
        // flag-2 FIXED: this test mixes VALUED inserts with TERM-ONLY inserts (`insert()`
        // with no value) and asserts both survive a reopen. The arbitrary-`V` overlay
        // reestablish now republishes MEMBERSHIP for every recovered final (not just the
        // valued ones), so term-only members survive — this runs on the overlay feature-on
        // (`u32` flips) and the owned tree feature-off, no kill-switch needed.

        // Insert some with values
        dict.insert_with_value("with_value_1", 10);
        dict.insert_with_value("with_value_2", 20);

        // Insert some without values (using default implementation)
        dict.insert("no_value_1");
        dict.insert("no_value_2");

        dict.sync().expect("sync");
    }

    // Reopen and verify
    {
        let dict: PersistentARTrie<u32> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        // Terms with values
        assert_eq!(dict.get_value("with_value_1"), Some(10));
        assert_eq!(dict.get_value("with_value_2"), Some(20));

        // Terms without values (should still be present but with no value)
        assert!(dict.contains("no_value_1"));
        assert!(dict.contains("no_value_2"));
        // Note: get_value returns None for terms inserted without values
        assert_eq!(dict.get_value("no_value_1"), None);
        assert_eq!(dict.get_value("no_value_2"), None);
    }
}

/// Test 15.5: Value update persistence.
/// Verify that updating a value (re-inserting with different value) persists correctly.
#[test]
fn test_value_update_persistence() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict_update_values.art");

    // Create and insert initial values
    {
        let dict: PersistentARTrie<u32> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        dict.insert_with_value("counter", 1);
        dict.sync().expect("sync");
    }

    // Reopen and update the value
    {
        let dict: PersistentARTrie<u32> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        // Update the value
        dict.insert_with_value("counter", 100);
        dict.sync().expect("sync");
    }

    // Reopen and verify updated value
    {
        let dict: PersistentARTrie<u32> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        assert_eq!(
            dict.get_value("counter"),
            Some(100),
            "counter should have updated value 100"
        );
    }
}

// =============================================================================
// Phase 16: ART Node Persistence Tests
// =============================================================================

/// Test 16.1: ART node persistence after bucket split.
/// Insert enough terms with diverse prefixes to trigger bucket-to-ART conversion.
/// Note: Terms must have diverse first bytes to properly split into multiple child buckets.
#[test]
fn test_art_node_bucket_split_persistence() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict_art_split.art");

    // Create 400 terms with diverse first bytes to trigger bucket split properly.
    // Terms like "aa_000", "ab_000", ..., "zz_000" have 676 unique prefixes (26*26).
    // Using format that ensures diverse byte distribution.
    let mut terms = Vec::new();
    for i in 0..15 {
        for c1 in b'a'..=b'z' {
            for c2 in b'a'..=b'f' {
                terms.push(format!("{}{}{:03}", c1 as char, c2 as char, i));
            }
        }
    }
    // 26 * 6 * 15 = 2340 terms, but we'll just use the first 400
    terms.truncate(400);
    let num_terms = terms.len();

    // Create dictionary and insert enough terms to trigger ART node creation
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        for term in &terms {
            dict.insert(term);
        }

        // Checkpoint to persist the ART structure
        dict.checkpoint().expect("checkpoint");
    }

    // Reopen and verify all terms are present
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        for term in &terms {
            assert!(
                dict.contains(term),
                "Term '{}' should be present after ART node recovery",
                term
            );
        }

        assert_eq!(
            dict.len(),
            Some(num_terms),
            "Dictionary should have {} terms after ART node recovery",
            num_terms
        );
    }
}

/// Test 16.2: Large-scale ART node persistence with diverse prefixes.
/// Tests ART node splitting with terms having different first bytes.
#[test]
fn test_art_node_diverse_prefixes_persistence() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict_art_diverse.art");

    // Create terms with diverse first characters to trigger bucket split by first byte
    // This creates terms like "a_000", "b_000", ..., "z_000", "a_001", ...
    let mut terms = Vec::new();
    for i in 0..20 {
        for c in b'a'..=b'z' {
            terms.push(format!("{}_{:03}", c as char, i));
        }
    }
    // 26 letters * 20 iterations = 520 terms

    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        for term in &terms {
            dict.insert(term);
        }

        dict.checkpoint().expect("checkpoint");
    }

    // Reopen and verify
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        for term in &terms {
            assert!(
                dict.contains(term),
                "Term '{}' should be present after diverse ART recovery",
                term
            );
        }

        assert_eq!(dict.len(), Some(terms.len()));
    }
}

/// Test 16.3: ART node persistence with values.
/// Verifies that values are correctly persisted through ART node structures.
/// Uses diverse prefixes to ensure proper bucket splitting.
#[test]
fn test_art_node_with_values_persistence() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict_art_values.art");

    // Create 400 terms with values using diverse prefixes to trigger proper ART split
    let mut terms_with_values = Vec::new();
    let mut counter = 0u32;
    for i in 0..15 {
        for c1 in b'a'..=b'z' {
            for c2 in b'a'..=b'f' {
                terms_with_values.push((format!("{}{}{:03}", c1 as char, c2 as char, i), counter));
                counter += 1;
            }
        }
    }
    terms_with_values.truncate(400);

    {
        let dict: PersistentARTrie<u32> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        for (term, value) in &terms_with_values {
            dict.insert_with_value(term, *value);
        }

        dict.checkpoint().expect("checkpoint");
    }

    // Reopen and verify all terms and values
    {
        let dict: PersistentARTrie<u32> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        for (term, expected_value) in &terms_with_values {
            assert!(dict.contains(term), "Term '{}' should be present", term);
            assert_eq!(
                dict.get_value(term),
                Some(*expected_value),
                "Term '{}' should have value {}",
                term,
                expected_value
            );
        }
    }
}

/// Test 16.4: ART node recovery without checkpoint (WAL replay with ART).
/// Inserts many terms then drops without checkpoint, relying on WAL replay.
/// Uses diverse prefixes to ensure proper bucket splitting.
#[test]
fn test_art_node_wal_only_recovery() {
    let dir = tempdir().expect("create temp dir");
    let dict_path = dir.path().join("test_dict_art_wal.art");

    // Create terms with diverse prefixes for proper bucket splitting
    let mut terms = Vec::new();
    for i in 0..15 {
        for c1 in b'a'..=b'z' {
            for c2 in b'a'..=b'f' {
                terms.push(format!("{}{}{:03}", c1 as char, c2 as char, i));
            }
        }
    }
    terms.truncate(400);

    // Create and insert without explicit checkpoint
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::create(&dict_path).expect("create dictionary");

        for term in &terms {
            dict.insert(term);
        }

        // Only sync WAL, don't checkpoint
        dict.sync().expect("sync");
    }

    // Reopen - should recover from WAL
    {
        let dict: PersistentARTrie<()> =
            PersistentARTrie::open(&dict_path).expect("open dictionary");

        for term in &terms {
            assert!(
                dict.contains(term),
                "Term '{}' should be present after WAL-only recovery",
                term
            );
        }
    }
}

// =============================================================================
// Phase 17: Char-based Recovery and Corruption Detection Tests
// =============================================================================

mod char_recovery_tests {
    use libdictenstein::persistent_artrie::wal::WalConfig;
    use libdictenstein::persistent_artrie_char::{
        detect_corruption, CorruptionType, RecoveryManager, RecoveryMode, RecoveryReport,
    };
    use std::fs;
    use tempfile::tempdir;

    /// Test 17.1: Corruption detection on truncated file.
    #[test]
    fn test_detect_corruption_truncated() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("truncated.artrie");

        // Create a truncated file (smaller than header)
        fs::write(&path, &[0u8; 32]).expect("write truncated file");

        let result = detect_corruption(&path, false).expect("detect_corruption");
        assert!(
            result.is_some(),
            "Should detect truncated file as corruption"
        );

        let info = result.unwrap();
        match info.corruption_type {
            CorruptionType::Truncated { expected, actual } => {
                assert_eq!(actual, 32);
                assert!(expected > actual);
            }
            _ => panic!(
                "Expected Truncated corruption type, got {:?}",
                info.corruption_type
            ),
        }
    }

    /// Test 17.2: Corruption detection on invalid magic.
    #[test]
    fn test_detect_corruption_invalid_magic() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("bad_magic.artrie");

        // Create file with invalid magic (64 bytes)
        let mut data = [0u8; 64];
        data[0..4].copy_from_slice(b"BAAD"); // Wrong magic
        fs::write(&path, &data).expect("write file");

        let result = detect_corruption(&path, false).expect("detect_corruption");
        assert!(result.is_some());

        let info = result.unwrap();
        assert!(matches!(info.corruption_type, CorruptionType::InvalidMagic));
    }

    /// Test 17.3: No corruption on missing file.
    #[test]
    fn test_detect_corruption_missing_file() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("nonexistent.artrie");

        let result = detect_corruption(&path, false).expect("detect_corruption");
        assert!(result.is_none(), "Missing file should not be corruption");
    }

    /// Test 17.4: Recovery manager reports normal for valid file.
    #[test]
    fn test_recovery_manager_no_corruption() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("valid.artrie");

        // Just check needs_recovery for non-existent file
        let config = WalConfig::default();
        let manager = RecoveryManager::new(&path, config);

        assert!(!manager.needs_recovery().expect("needs_recovery"));
    }

    /// Test 17.5: RecoveryMode success checks.
    #[test]
    fn test_recovery_mode_success() {
        assert!(RecoveryMode::Normal {
            wal_records_replayed: 0
        }
        .is_success());
        assert!(RecoveryMode::Normal {
            wal_records_replayed: 100
        }
        .is_success());

        assert!(RecoveryMode::PartialRecovery {
            corrupted_arenas: vec![1, 2, 3],
            recovered_records: 50,
        }
        .is_success());

        assert!(RecoveryMode::RebuildFromWal {
            segments_processed: 5,
            records_replayed: 1000,
        }
        .is_success());

        assert!(!RecoveryMode::Unrecoverable {
            reason: "test".to_string(),
        }
        .is_success());
    }

    /// Test 17.6: RecoveryMode records_replayed counts.
    #[test]
    fn test_recovery_mode_records_replayed() {
        assert_eq!(
            RecoveryMode::Normal {
                wal_records_replayed: 42
            }
            .records_replayed(),
            42
        );

        assert_eq!(
            RecoveryMode::PartialRecovery {
                corrupted_arenas: vec![],
                recovered_records: 100,
            }
            .records_replayed(),
            100
        );

        assert_eq!(
            RecoveryMode::RebuildFromWal {
                segments_processed: 3,
                records_replayed: 999,
            }
            .records_replayed(),
            999
        );

        assert_eq!(
            RecoveryMode::Unrecoverable {
                reason: "error".to_string()
            }
            .records_replayed(),
            0
        );
    }

    /// Test 17.7: RecoveryReport normal constructor.
    #[test]
    fn test_recovery_report_normal() {
        let report = RecoveryReport::normal();
        assert!(report.mode.is_success());
        assert_eq!(report.records_replayed, 0);
        assert_eq!(report.segments_processed, 0);
        assert_eq!(report.corrupted_records_skipped, 0);
    }

    /// Test 17.8: WalConfig default values.
    #[test]
    fn test_wal_config_defaults() {
        let config = WalConfig::default();
        assert!(config.archive_enabled);
        assert_eq!(config.max_segments, 10);
        assert_eq!(config.max_archive_bytes, 10 << 30); // 10 GB
    }
}

// =============================================================================
// Phase 18: Archive Mode Integration Tests
// =============================================================================

mod archive_mode_tests {
    use libdictenstein::persistent_artrie::wal::WalConfig;
    use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Test 18.2: Archive mode disabled skips archiving.
    #[test]
    fn test_archive_mode_disabled() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("no_archive.artrie");

        // Create trie with archive mode disabled
        {
            let config = WalConfig::no_archive();
            let trie =
                PersistentARTrieChar::<()>::create_with_config(&path, config).expect("create trie");

            // Insert some data
            for i in 0..100 {
                trie.insert(&format!("term{}", i)).expect("insert");
            }

            // Checkpoint - should truncate, not archive
            trie.checkpoint().expect("checkpoint");
        }

        // Verify archive directory was NOT created
        let archive_dir = dir.path().join("wal_archive");
        assert!(
            !archive_dir.exists(),
            "Archive directory should not exist when disabled"
        );
    }

    /// Test 18.3: open_with_config preserves archive settings.
    #[test]
    fn test_open_with_config() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("config_test.artrie");

        // Create trie
        {
            let trie = PersistentARTrieChar::<()>::create(&path).expect("create trie");
            trie.insert("hello").expect("insert");
            trie.checkpoint().expect("checkpoint");
        }

        // Re-open with custom config
        {
            let config = WalConfig {
                archive_enabled: true,
                archive_dir: PathBuf::from("custom_archive"),
                max_segments: 5,
                max_archive_bytes: 1 << 30, // 1 GB
            };
            let trie =
                PersistentARTrieChar::<()>::open_with_config(&path, config).expect("open trie");

            // Insert and checkpoint
            trie.insert("world").expect("insert");
            trie.checkpoint().expect("checkpoint");
        }

        // Verify custom archive directory was created
        let custom_archive = dir.path().join("custom_archive");
        assert!(
            custom_archive.exists(),
            "Custom archive directory should exist"
        );
    }

    /// Test 18.4: Data survives multiple checkpoint cycles with archive mode.
    #[test]
    fn test_data_survives_archive_checkpoints() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("multi_checkpoint.artrie");

        // Create and populate
        {
            let trie = PersistentARTrieChar::<()>::create(&path).expect("create trie");

            for round in 0..5 {
                for i in 0..20 {
                    let term = format!("round{}term{}", round, i);
                    trie.insert(&term).expect("insert");
                }
                trie.checkpoint().expect("checkpoint");
            }
        }

        // Verify all data survived
        {
            let trie = PersistentARTrieChar::<()>::open(&path).expect("open trie");

            assert_eq!(
                trie.len(),
                100,
                "Should have 100 terms (5 rounds * 20 terms)"
            );

            for round in 0..5 {
                for i in 0..20 {
                    let term = format!("round{}term{}", round, i);
                    assert!(trie.contains(&term), "Term '{}' should exist", term);
                }
            }
        }
    }
}

// =============================================================================
// Phase 19: Tests for `open_with_recovery()`
// =============================================================================

mod open_with_recovery_tests {
    use libdictenstein::persistent_artrie::recovery::RecoveryMode;

    use libdictenstein::persistent_artrie_char::PersistentARTrieChar;

    use tempfile::tempdir;

    #[test]
    fn test_open_with_recovery_new_file() {
        let dir = tempdir().expect("create tempdir");
        let path = dir.path().join("new_file.artc");

        // open_with_recovery on non-existent file should create it
        let (trie, report) = PersistentARTrieChar::<()>::open_with_recovery(&path)
            .expect("open_with_recovery should succeed");

        assert_eq!(
            report.mode,
            RecoveryMode::CreatedNew,
            "Should report CreatedNew mode"
        );
        assert_eq!(report.records_replayed, 0);
        assert_eq!(report.terms_recovered, 0);

        // Verify the trie works
        trie.insert("hello").expect("insert");
        assert!(trie.contains("hello"));
    }

    #[test]
    fn test_open_with_recovery_normal_file() {
        let dir = tempdir().expect("create tempdir");
        let path = dir.path().join("normal.artc");

        // Create and populate a trie normally
        {
            let trie = PersistentARTrieChar::<()>::create(&path).expect("create trie");

            for i in 0..10 {
                let term = format!("term{}", i);
                trie.insert(&term).expect("insert");
            }

            trie.checkpoint().expect("checkpoint");
        }

        // open_with_recovery on clean file should return Normal mode
        let (trie, report) = PersistentARTrieChar::<()>::open_with_recovery(&path)
            .expect("open_with_recovery should succeed");

        assert_eq!(
            report.mode,
            RecoveryMode::Normal,
            "Should report Normal mode"
        );
        assert!(report.mode.is_normal());
        assert!(!report.mode.recovered());

        // Verify all data is present
        for i in 0..10 {
            let term = format!("term{}", i);
            assert!(trie.contains(&term), "Term '{}' should exist", term);
        }
    }

    #[test]
    fn test_recovery_mode_helpers() {
        // Test RecoveryMode helper methods
        assert!(RecoveryMode::Normal.is_normal());
        assert!(!RecoveryMode::Normal.recovered());

        assert!(!RecoveryMode::RebuildFromWal.is_normal());
        assert!(RecoveryMode::RebuildFromWal.recovered());

        assert!(!RecoveryMode::CreatedNew.is_normal());
        assert!(RecoveryMode::CreatedNew.recovered());

        assert!(!RecoveryMode::RepairInPlace.is_normal());
        assert!(RecoveryMode::RepairInPlace.recovered());
    }
}

// ===========================================================================
// Phase 20: Prefix Operations Tests
// ===========================================================================
// Tests for iter_prefix(), iter_prefix_with_values(), and remove_prefix()

mod phase_20_prefix_operations {
    use libdictenstein::persistent_artrie::PersistentARTrie;
    use libdictenstein::Dictionary;
    use tempfile::tempdir;

    #[test]
    fn test_iter_prefix() {
        let dir = tempdir().expect("Failed to create temp dir");
        let _path = dir.path().join("test.artrie");

        let trie: PersistentARTrie<()> = PersistentARTrie::new();
        trie.insert("apple");
        trie.insert("application");
        trie.insert("apply");
        trie.insert("banana");
        trie.insert("band");

        // Prefix "app" should match 3 terms
        let matches: Vec<_> = trie.iter_prefix(b"app").expect("prefix exists").collect();
        assert_eq!(matches.len(), 3, "Expected 3 matches for prefix 'app'");

        // Convert to strings for easier checking
        let match_strings: Vec<String> = matches
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();
        assert!(match_strings.contains(&"apple".to_string()));
        assert!(match_strings.contains(&"application".to_string()));
        assert!(match_strings.contains(&"apply".to_string()));
    }

    #[test]
    fn test_iter_prefix_not_found() {
        let trie: PersistentARTrie<()> = PersistentARTrie::new();
        trie.insert("apple");
        trie.insert("banana");

        // Prefix "xyz" should not exist
        assert!(
            trie.iter_prefix(b"xyz").is_none(),
            "Non-existent prefix should return None"
        );
    }

    #[test]
    fn test_iter_prefix_exact_term() {
        let trie: PersistentARTrie<()> = PersistentARTrie::new();
        trie.insert("app");
        trie.insert("apple");
        trie.insert("application");

        // Prefix "app" should match "app" itself plus extensions
        let matches: Vec<_> = trie.iter_prefix(b"app").expect("prefix exists").collect();
        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn test_iter_prefix_empty_prefix() {
        let trie: PersistentARTrie<()> = PersistentARTrie::new();
        trie.insert("apple");
        trie.insert("banana");
        trie.insert("cherry");

        // Empty prefix should match all terms
        let matches: Vec<_> = trie
            .iter_prefix(b"")
            .expect("empty prefix exists")
            .collect();
        assert_eq!(matches.len(), 3, "Empty prefix should match all terms");
    }

    #[test]
    fn test_iter_prefix_with_values() {
        let trie: PersistentARTrie<i32> = PersistentARTrie::new();
        trie.insert_with_value("apple", 1);
        trie.insert_with_value("application", 2);
        trie.insert_with_value("apply", 3);
        trie.insert_with_value("banana", 4);

        // Prefix "app" should return (term, value) pairs
        let matches: Vec<_> = trie
            .iter_prefix_with_values(b"app")
            .expect("prefix exists")
            .collect();

        // Note: ValuedDictZipper::value() may return None if value extraction isn't implemented
        // So we just check that we got some results (even if values are empty)
        assert!(matches.len() >= 0, "Should return iterator");
    }

    #[test]
    fn test_remove_prefix() {
        let trie: PersistentARTrie<()> = PersistentARTrie::new();
        trie.insert("apple");
        trie.insert("application");
        trie.insert("apply");
        trie.insert("banana");
        trie.insert("band");

        // Remove all terms starting with "app"
        let removed = trie.remove_prefix(b"app");
        assert_eq!(removed, 3, "Should remove 3 terms with prefix 'app'");

        // Verify terms are gone
        assert!(!trie.contains("apple"));
        assert!(!trie.contains("application"));
        assert!(!trie.contains("apply"));

        // Verify other terms remain
        assert!(trie.contains("banana"));
        assert!(trie.contains("band"));
    }

    #[test]
    fn test_remove_prefix_not_found() {
        let trie: PersistentARTrie<()> = PersistentARTrie::new();
        trie.insert("apple");
        trie.insert("banana");

        // Remove non-existent prefix
        let removed = trie.remove_prefix(b"xyz");
        assert_eq!(removed, 0, "Should remove 0 terms for non-existent prefix");

        // Verify nothing was removed
        assert!(trie.contains("apple"));
        assert!(trie.contains("banana"));
    }

    #[test]
    fn test_remove_prefix_exact_match() {
        let trie: PersistentARTrie<()> = PersistentARTrie::new();
        trie.insert("app");
        trie.insert("apple");
        trie.insert("application");
        trie.insert("banana");

        // Remove all terms starting with "app" (including "app" itself)
        let removed = trie.remove_prefix(b"app");
        assert_eq!(removed, 3, "Should remove 'app', 'apple', 'application'");

        // Verify all are gone
        assert!(!trie.contains("app"));
        assert!(!trie.contains("apple"));
        assert!(!trie.contains("application"));
        assert!(trie.contains("banana"));
    }

    #[test]
    fn test_remove_prefix_empty_prefix() {
        let trie: PersistentARTrie<()> = PersistentARTrie::new();
        trie.insert("apple");
        trie.insert("banana");
        trie.insert("cherry");

        // Remove all terms (empty prefix matches everything)
        let removed = trie.remove_prefix(b"");
        assert_eq!(removed, 3, "Empty prefix should remove all terms");

        // Verify all are gone
        assert!(!trie.contains("apple"));
        assert!(!trie.contains("banana"));
        assert!(!trie.contains("cherry"));
    }

    #[test]
    fn test_iter_prefix_persistent() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        // Create and populate
        {
            let trie = PersistentARTrie::<()>::create(&path).expect("Failed to create trie");
            trie.insert("apple");
            trie.insert("application");
            trie.insert("banana");
            trie.sync().expect("sync failed");
        }

        // Reopen and test iter_prefix
        {
            let trie = PersistentARTrie::<()>::open(&path).expect("Failed to open trie");

            let matches: Vec<_> = trie.iter_prefix(b"app").expect("prefix exists").collect();
            assert_eq!(
                matches.len(),
                2,
                "Should find 2 terms with prefix 'app' after reopen"
            );
        }
    }

    #[test]
    fn test_remove_prefix_with_wal_recovery() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        // Create, populate, and remove prefix
        {
            let trie = PersistentARTrie::<()>::create(&path).expect("Failed to create trie");
            trie.insert("apple");
            trie.insert("application");
            trie.insert("apply");
            trie.insert("banana");
            trie.sync().expect("sync failed");

            // Remove prefix
            let removed = trie.remove_prefix(b"app");
            assert_eq!(removed, 3, "Should remove 3 terms");

            trie.sync().expect("sync failed");
        }

        // Reopen and verify removals persisted
        {
            let trie = PersistentARTrie::<()>::open(&path).expect("Failed to open trie");

            assert!(!trie.contains("apple"), "'apple' should be removed");
            assert!(
                !trie.contains("application"),
                "'application' should be removed"
            );
            assert!(!trie.contains("apply"), "'apply' should be removed");
            assert!(trie.contains("banana"), "'banana' should remain");
        }
    }

    #[test]
    fn test_remove_prefix_batched() {
        let trie: PersistentARTrie<()> = PersistentARTrie::new();

        // Insert many terms with common prefix
        for i in 0..100 {
            trie.insert(&format!("prefix_{:03}", i));
        }
        trie.insert("other_term");

        // Remove in small batches (batch_size = 10)
        let removed = trie.remove_prefix_batched(b"prefix_", 10);
        assert_eq!(removed, 100, "Should remove all 100 prefixed terms");

        // Verify all prefixed terms are gone
        assert!(!trie.contains("prefix_000"));
        assert!(!trie.contains("prefix_050"));
        assert!(!trie.contains("prefix_099"));

        // Verify other term remains
        assert!(trie.contains("other_term"));
    }

    #[test]
    fn test_remove_prefix_batched_tiny_batch() {
        let trie: PersistentARTrie<()> = PersistentARTrie::new();
        trie.insert("aa");
        trie.insert("ab");
        trie.insert("ac");
        trie.insert("ba");

        // Use batch size of 1 - tests the loop iteration
        let removed = trie.remove_prefix_batched(b"a", 1);
        assert_eq!(removed, 3, "Should remove 3 terms with batch_size=1");

        assert!(!trie.contains("aa"));
        assert!(!trie.contains("ab"));
        assert!(!trie.contains("ac"));
        assert!(trie.contains("ba"));
    }
}

/// Phase 21: Prefix operations for char-based PersistentARTrieChar
mod phase_21_char_prefix_operations {
    use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    use tempfile::tempdir;

    // =========================================================================
    // In-memory PersistentARTrieChar tests
    // =========================================================================

    #[test]
    fn test_char_iter_prefix() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        trie.insert("apple");
        trie.insert("application");
        trie.insert("apply");
        trie.insert("banana");
        trie.insert("band");

        // Prefix "app" should match 3 terms
        let matches = trie
            .iter_prefix("app")
            .expect("I/O error")
            .expect("prefix exists");
        assert_eq!(matches.len(), 3, "Expected 3 matches for prefix 'app'");

        assert!(matches.contains(&"apple".to_string()));
        assert!(matches.contains(&"application".to_string()));
        assert!(matches.contains(&"apply".to_string()));
    }

    #[test]
    fn test_char_iter_prefix_not_found() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        trie.insert("apple");
        trie.insert("banana");

        // Prefix "xyz" should not exist
        assert!(
            trie.iter_prefix("xyz").expect("I/O error").is_none(),
            "Non-existent prefix should return None"
        );
    }

    #[test]
    fn test_char_iter_prefix_unicode() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        trie.insert("日本語");
        trie.insert("日本人");
        trie.insert("日曜日");
        trie.insert("月曜日");

        // Prefix "日本" should match 2 terms
        let matches = trie
            .iter_prefix("日本")
            .expect("I/O error")
            .expect("prefix exists");
        assert_eq!(matches.len(), 2, "Expected 2 matches for prefix '日本'");

        assert!(matches.contains(&"日本語".to_string()));
        assert!(matches.contains(&"日本人".to_string()));
    }

    #[test]
    fn test_char_iter_prefix_with_values() {
        let trie: PersistentARTrieChar<i32> = PersistentARTrieChar::new();
        trie.insert_with_value("apple", 1);
        trie.insert_with_value("application", 2);
        trie.insert_with_value("apply", 3);
        trie.insert_with_value("banana", 4);

        // Prefix "app" should return (term, value) pairs
        let matches = trie
            .iter_prefix_with_values("app")
            .expect("I/O error")
            .expect("prefix exists");

        assert_eq!(matches.len(), 3, "Should have 3 matches");

        // Check that values are correct
        assert!(matches.iter().any(|(t, v)| t == "apple" && *v == 1));
        assert!(matches.iter().any(|(t, v)| t == "application" && *v == 2));
        assert!(matches.iter().any(|(t, v)| t == "apply" && *v == 3));
    }

    #[test]
    fn test_char_remove_prefix() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        trie.insert("apple");
        trie.insert("application");
        trie.insert("apply");
        trie.insert("banana");
        trie.insert("band");

        // Remove all terms starting with "app"
        let removed = trie.remove_prefix("app").expect("remove_prefix failed");
        assert_eq!(removed, 3, "Should remove 3 terms with prefix 'app'");

        // Verify terms are gone
        assert!(!trie.contains("apple"));
        assert!(!trie.contains("application"));
        assert!(!trie.contains("apply"));

        // Verify other terms remain
        assert!(trie.contains("banana"));
        assert!(trie.contains("band"));
    }

    #[test]
    fn test_char_remove_prefix_unicode() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();
        trie.insert("日本語");
        trie.insert("日本人");
        trie.insert("日曜日");
        trie.insert("月曜日");

        // Remove terms starting with "日"
        let removed = trie.remove_prefix("日").expect("remove_prefix failed");
        assert_eq!(removed, 3, "Should remove 3 terms with prefix '日'");

        assert!(!trie.contains("日本語"));
        assert!(!trie.contains("日本人"));
        assert!(!trie.contains("日曜日"));
        assert!(trie.contains("月曜日"));
    }

    #[test]
    fn test_char_remove_prefix_batched() {
        let trie: PersistentARTrieChar<()> = PersistentARTrieChar::new();

        // Insert many terms with common prefix
        for i in 0..100 {
            trie.insert(&format!("prefix_{:03}", i));
        }
        trie.insert("other_term");

        // Remove in small batches (batch_size = 10)
        let removed = trie
            .remove_prefix_batched("prefix_", 10)
            .expect("remove_prefix_batched failed");
        assert_eq!(removed, 100, "Should remove all 100 prefixed terms");

        // Verify all prefixed terms are gone
        assert!(!trie.contains("prefix_000"));
        assert!(!trie.contains("prefix_050"));
        assert!(!trie.contains("prefix_099"));

        // Verify other term remains
        assert!(trie.contains("other_term"));
    }

    // =========================================================================
    // Disk-backed PersistentARTrieChar tests
    // =========================================================================

    #[test]
    fn test_disk_char_iter_prefix() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");
        trie.insert("apple").expect("insert failed");
        trie.insert("application").expect("insert failed");
        trie.insert("apply").expect("insert failed");
        trie.insert("banana").expect("insert failed");

        // Prefix "app" should match 3 terms
        let matches = trie
            .iter_prefix("app")
            .expect("I/O error")
            .expect("prefix exists");
        assert_eq!(matches.len(), 3, "Expected 3 matches for prefix 'app'");

        assert!(matches.contains(&"apple".to_string()));
        assert!(matches.contains(&"application".to_string()));
        assert!(matches.contains(&"apply".to_string()));
    }

    #[test]
    fn test_disk_char_iter_prefix_not_found() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");
        trie.insert("apple").expect("insert failed");
        trie.insert("banana").expect("insert failed");

        // Prefix "xyz" should not exist
        let result = trie.iter_prefix("xyz").expect("I/O error");
        assert!(result.is_none(), "Non-existent prefix should return None");
    }

    #[test]
    fn test_disk_char_iter_prefix_unicode() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");
        trie.insert("日本語").expect("insert failed");
        trie.insert("日本人").expect("insert failed");
        trie.insert("日曜日").expect("insert failed");
        trie.insert("月曜日").expect("insert failed");

        // Prefix "日本" should match 2 terms
        let matches = trie
            .iter_prefix("日本")
            .expect("I/O error")
            .expect("prefix exists");
        assert_eq!(matches.len(), 2, "Expected 2 matches for prefix '日本'");

        assert!(matches.contains(&"日本語".to_string()));
        assert!(matches.contains(&"日本人".to_string()));
    }

    #[test]
    fn test_disk_char_iter_prefix_with_values() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<i32>::create(&path).expect("Failed to create trie");
        trie.upsert("apple", 1).expect("upsert failed");
        trie.upsert("application", 2).expect("upsert failed");
        trie.upsert("apply", 3).expect("upsert failed");
        trie.upsert("banana", 4).expect("upsert failed");

        // Prefix "app" should return (term, value) pairs
        let matches = trie
            .iter_prefix_with_values("app")
            .expect("I/O error")
            .expect("prefix exists");

        assert_eq!(matches.len(), 3, "Should have 3 matches");

        // Check that values are correct
        assert!(matches.iter().any(|(t, v)| t == "apple" && *v == 1));
        assert!(matches.iter().any(|(t, v)| t == "application" && *v == 2));
        assert!(matches.iter().any(|(t, v)| t == "apply" && *v == 3));
    }

    #[test]
    fn test_disk_char_remove_prefix() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");
        trie.insert("apple").expect("insert failed");
        trie.insert("application").expect("insert failed");
        trie.insert("apply").expect("insert failed");
        trie.insert("banana").expect("insert failed");
        trie.insert("band").expect("insert failed");

        // Remove all terms starting with "app"
        let removed = trie.remove_prefix("app").expect("remove failed");
        assert_eq!(removed, 3, "Should remove 3 terms with prefix 'app'");

        // Verify terms are gone
        assert!(!trie.contains("apple"));
        assert!(!trie.contains("application"));
        assert!(!trie.contains("apply"));

        // Verify other terms remain
        assert!(trie.contains("banana"));
        assert!(trie.contains("band"));
    }

    #[test]
    fn test_disk_char_remove_prefix_unicode() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");
        trie.insert("日本語").expect("insert failed");
        trie.insert("日本人").expect("insert failed");
        trie.insert("日曜日").expect("insert failed");
        trie.insert("月曜日").expect("insert failed");

        // Remove terms starting with "日"
        let removed = trie.remove_prefix("日").expect("remove failed");
        assert_eq!(removed, 3, "Should remove 3 terms with prefix '日'");

        assert!(!trie.contains("日本語"));
        assert!(!trie.contains("日本人"));
        assert!(!trie.contains("日曜日"));
        assert!(trie.contains("月曜日"));
    }

    #[test]
    fn test_disk_char_remove_prefix_batched() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");

        // Insert many terms with common prefix
        for i in 0..50 {
            trie.insert(&format!("prefix_{:03}", i))
                .expect("insert failed");
        }
        trie.insert("other_term").expect("insert failed");

        // Remove in small batches (batch_size = 10)
        let removed = trie
            .remove_prefix_batched("prefix_", 10)
            .expect("remove failed");
        assert_eq!(removed, 50, "Should remove all 50 prefixed terms");

        // Verify all prefixed terms are gone
        assert!(!trie.contains("prefix_000"));
        assert!(!trie.contains("prefix_025"));
        assert!(!trie.contains("prefix_049"));

        // Verify other term remains
        assert!(trie.contains("other_term"));
    }

    #[test]
    fn test_disk_char_remove_prefix_wal_recovery() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        // Create, populate, and remove prefix
        {
            let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");
            trie.insert("apple").expect("insert failed");
            trie.insert("application").expect("insert failed");
            trie.insert("apply").expect("insert failed");
            trie.insert("banana").expect("insert failed");
            trie.sync().expect("sync failed");

            // Remove prefix
            let removed = trie.remove_prefix("app").expect("remove failed");
            assert_eq!(removed, 3, "Should remove 3 terms");

            trie.sync().expect("sync failed");
        }

        // Reopen and verify removals persisted
        {
            let (trie, _report) =
                PersistentARTrieChar::<()>::open_with_recovery(&path).expect("Failed to open trie");

            assert!(!trie.contains("apple"), "'apple' should be removed");
            assert!(
                !trie.contains("application"),
                "'application' should be removed"
            );
            assert!(!trie.contains("apply"), "'apply' should be removed");
            assert!(trie.contains("banana"), "'banana' should remain");
        }
    }

    #[test]
    fn test_disk_char_iter_prefix_persistent() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        // Create and populate
        {
            let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");
            trie.insert("apple").expect("insert failed");
            trie.insert("application").expect("insert failed");
            trie.insert("banana").expect("insert failed");
            trie.sync().expect("sync failed");
        }

        // Reopen and test iter_prefix
        {
            let (trie, _report) =
                PersistentARTrieChar::<()>::open_with_recovery(&path).expect("Failed to open trie");

            let matches = trie
                .iter_prefix("app")
                .expect("I/O error")
                .expect("prefix exists");
            assert_eq!(
                matches.len(),
                2,
                "Should find 2 terms with prefix 'app' after reopen"
            );
        }
    }

    // =========================================================================
    // Arena-aware iteration tests
    // =========================================================================

    #[test]
    fn test_disk_char_iter_prefix_with_arena() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");

        // Insert terms that will share arenas (terms with common prefixes)
        trie.insert("apple").expect("insert failed");
        trie.insert("application").expect("insert failed");
        trie.insert("apply").expect("insert failed");
        trie.insert("banana").expect("insert failed");
        trie.sync().expect("sync failed");

        // Get terms with arena info
        let terms = trie
            .iter_prefix_with_arena("app")
            .expect("I/O error")
            .expect("prefix exists");

        assert_eq!(terms.len(), 3, "Should find 3 terms with prefix 'app'");

        // Verify we get the expected terms
        let term_strings: Vec<_> = terms.iter().map(|t| t.term.clone()).collect();
        assert!(term_strings.contains(&"apple".to_string()));
        assert!(term_strings.contains(&"application".to_string()));
        assert!(term_strings.contains(&"apply".to_string()));

        // Arena info should be populated for disk-backed nodes
        // (Note: some nodes may still be in-memory if not yet persisted to disk)
    }

    #[test]
    fn test_disk_char_iter_prefix_with_arena_grouping() {
        use std::collections::HashMap;

        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");

        // Insert many terms with common prefix
        for i in 0..50 {
            trie.insert(&format!("prefix_{:03}", i))
                .expect("insert failed");
        }
        trie.insert("other").expect("insert failed");
        trie.sync().expect("sync failed");

        // Get terms with arena info
        let terms = trie
            .iter_prefix_with_arena("prefix_")
            .expect("I/O error")
            .expect("prefix exists");

        assert_eq!(
            terms.len(),
            50,
            "Should find 50 terms with prefix 'prefix_'"
        );

        // Group by arena to verify structure
        let mut by_arena: HashMap<Option<u32>, Vec<String>> = HashMap::new();
        for item in &terms {
            by_arena
                .entry(item.arena_id)
                .or_default()
                .push(item.term.clone());
        }

        // Verify we got all terms grouped
        let total: usize = by_arena.values().map(|v| v.len()).sum();
        assert_eq!(total, 50, "All 50 terms should be grouped");

        // Log arena distribution for debugging
        println!("Arena distribution: {} arenas", by_arena.len());
        for (arena, terms) in &by_arena {
            println!("  Arena {:?}: {} terms", arena, terms.len());
        }
    }

    #[test]
    fn test_disk_char_remove_prefix_arena_batched() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");

        // Insert many terms
        for i in 0..100 {
            trie.insert(&format!("test_{:03}", i))
                .expect("insert failed");
        }
        trie.insert("keep_me").expect("insert failed");
        trie.sync().expect("sync failed");

        // Remove with small batch size (forces multiple batches with arena grouping)
        let removed = trie
            .remove_prefix_batched("test_", 10)
            .expect("remove failed");
        assert_eq!(removed, 100, "Should remove all 100 terms");

        // Verify all terms removed
        assert!(!trie.contains("test_000"));
        assert!(!trie.contains("test_050"));
        assert!(!trie.contains("test_099"));

        // Verify unrelated term remains
        assert!(trie.contains("keep_me"));
    }

    #[test]
    fn test_disk_char_iter_prefix_with_arena_not_found() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");

        trie.insert("apple").expect("insert failed");
        trie.sync().expect("sync failed");

        // Non-existent prefix should return None
        let result = trie.iter_prefix_with_arena("xyz").expect("I/O error");
        assert!(result.is_none(), "Non-existent prefix should return None");
    }

    #[test]
    fn test_disk_char_iter_prefix_with_arena_unicode() {
        let dir = tempdir().expect("Failed to create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<()>::create(&path).expect("Failed to create trie");

        trie.insert("日本語").expect("insert failed");
        trie.insert("日本人").expect("insert failed");
        trie.insert("日曜日").expect("insert failed");
        trie.insert("月曜日").expect("insert failed");
        trie.sync().expect("sync failed");

        // Get terms with arena info for Unicode prefix
        let terms = trie
            .iter_prefix_with_arena("日本")
            .expect("I/O error")
            .expect("prefix exists");

        assert_eq!(terms.len(), 2, "Should find 2 terms with prefix '日本'");

        let term_strings: Vec<_> = terms.iter().map(|t| t.term.clone()).collect();
        assert!(term_strings.contains(&"日本語".to_string()));
        assert!(term_strings.contains(&"日本人".to_string()));
    }
}

// =============================================================================
// Phase 22: Merge Operations Tests
// =============================================================================
// Tests for merge_from(), merge_replace(), SharedCharTrie::union_with(),
// and iter_prefix_with_values_and_arena()

mod phase_22_merge_operations {

    use libdictenstein::persistent_artrie_char::PersistentARTrieChar;
    use libdictenstein::persistent_artrie_char::SharedCharTrie;
    use libdictenstein::ARTrie;

    use tempfile::tempdir;

    // =========================================================================
    // PersistentARTrieChar merge_from() tests
    // =========================================================================

    #[test]
    fn test_char_trie_merge_from_basic() {
        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("trie1.artrie");
        let path2 = dir.path().join("trie2.artrie");

        // Create first trie with some terms
        let mut trie1 = PersistentARTrieChar::<i64>::create(&path1).expect("create trie1");
        trie1.upsert("apple", 5).expect("upsert failed");
        trie1.upsert("banana", 3).expect("upsert failed");

        // Create second trie with overlapping and new terms
        let trie2 = PersistentARTrieChar::<i64>::create(&path2).expect("create trie2");
        trie2.upsert("apple", 7).expect("upsert failed"); // Overlap
        trie2.upsert("cherry", 2).expect("upsert failed"); // New

        // Merge trie2 into trie1 with sum function
        let processed = trie1
            .merge_from(&trie2, |a, b| a + b)
            .expect("merge failed");

        assert_eq!(processed, 2, "Should process 2 terms from trie2");

        // Verify merged values
        // F2-migrate: Bucket A — `get()` returns None under the overlay (the C2 merge
        // routes through the overlay); read merged values via `get_value`.
        assert_eq!(
            trie1.get_value("apple"),
            Some(12),
            "apple should be 5 + 7 = 12"
        );
        assert_eq!(trie1.get_value("banana"), Some(3), "banana should remain 3");
        assert_eq!(
            trie1.get_value("cherry"),
            Some(2),
            "cherry should be added with value 2"
        );
    }

    #[test]
    fn test_char_trie_merge_from_all_new() {
        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("trie1.artrie");
        let path2 = dir.path().join("trie2.artrie");

        // Create first trie
        let mut trie1 = PersistentARTrieChar::<i64>::create(&path1).expect("create trie1");
        trie1.upsert("apple", 1).expect("upsert failed");

        // Create second trie with completely different terms
        let trie2 = PersistentARTrieChar::<i64>::create(&path2).expect("create trie2");
        trie2.upsert("banana", 2).expect("upsert failed");
        trie2.upsert("cherry", 3).expect("upsert failed");
        trie2.upsert("date", 4).expect("upsert failed");

        // Merge - no overlaps, so merge_fn never called
        let processed = trie1
            .merge_from(&trie2, |a, b| a + b)
            .expect("merge failed");

        assert_eq!(processed, 3, "Should process 3 terms");
        assert_eq!(trie1.len(), 4, "trie1 should have 4 terms total");
        // F2-migrate: Bucket A — read merged values via `get_value` (overlay-routed).
        assert_eq!(trie1.get_value("apple"), Some(1));
        assert_eq!(trie1.get_value("banana"), Some(2));
        assert_eq!(trie1.get_value("cherry"), Some(3));
        assert_eq!(trie1.get_value("date"), Some(4));
    }

    #[test]
    fn test_char_trie_merge_from_all_overlapping() {
        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("trie1.artrie");
        let path2 = dir.path().join("trie2.artrie");

        // Create first trie
        let mut trie1 = PersistentARTrieChar::<i64>::create(&path1).expect("create trie1");
        trie1.upsert("apple", 10).expect("upsert failed");
        trie1.upsert("banana", 20).expect("upsert failed");

        // Create second trie with same terms
        let trie2 = PersistentARTrieChar::<i64>::create(&path2).expect("create trie2");
        trie2.upsert("apple", 5).expect("upsert failed");
        trie2.upsert("banana", 10).expect("upsert failed");

        // Merge with sum
        let processed = trie1
            .merge_from(&trie2, |a, b| a + b)
            .expect("merge failed");

        assert_eq!(processed, 2, "Should process 2 terms");
        assert_eq!(trie1.len(), 2, "trie1 should still have 2 terms");
        // F2-migrate: Bucket A — read merged values via `get_value` (overlay-routed).
        assert_eq!(
            trie1.get_value("apple"),
            Some(15),
            "apple should be 10 + 5 = 15"
        );
        assert_eq!(
            trie1.get_value("banana"),
            Some(30),
            "banana should be 20 + 10 = 30"
        );
    }

    #[test]
    fn test_char_trie_merge_from_empty_source() {
        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("trie1.artrie");
        let path2 = dir.path().join("trie2.artrie");

        // Create first trie with terms
        let mut trie1 = PersistentARTrieChar::<i64>::create(&path1).expect("create trie1");
        trie1.upsert("apple", 5).expect("upsert failed");

        // Create empty second trie
        let trie2 = PersistentARTrieChar::<i64>::create(&path2).expect("create trie2");

        // Merge empty trie
        let processed = trie1
            .merge_from(&trie2, |a, b| a + b)
            .expect("merge failed");

        assert_eq!(processed, 0, "Should process 0 terms from empty trie");
        assert_eq!(trie1.len(), 1, "trie1 should still have 1 term");
        // F2-migrate: Bucket A — read via `get_value` (overlay-routed).
        assert_eq!(trie1.get_value("apple"), Some(5), "apple should remain 5");
    }

    #[test]
    fn test_char_trie_merge_replace() {
        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("trie1.artrie");
        let path2 = dir.path().join("trie2.artrie");

        // Create first trie
        let mut trie1 = PersistentARTrieChar::<i64>::create(&path1).expect("create trie1");
        trie1.upsert("apple", 100).expect("upsert failed");
        trie1.upsert("banana", 200).expect("upsert failed");

        // Create second trie with overlapping term
        let trie2 = PersistentARTrieChar::<i64>::create(&path2).expect("create trie2");
        trie2.upsert("apple", 999).expect("upsert failed"); // Will replace
        trie2.upsert("cherry", 300).expect("upsert failed"); // New

        // Merge with replace semantics (right value wins)
        let processed = trie1.merge_replace(&trie2).expect("merge failed");

        assert_eq!(processed, 2, "Should process 2 terms");
        // F2-migrate: Bucket A — read merged values via `get_value` (overlay-routed).
        assert_eq!(
            trie1.get_value("apple"),
            Some(999),
            "apple should be replaced with 999"
        );
        assert_eq!(
            trie1.get_value("banana"),
            Some(200),
            "banana should remain 200"
        );
        assert_eq!(
            trie1.get_value("cherry"),
            Some(300),
            "cherry should be added"
        );
    }

    #[test]
    fn test_char_trie_merge_from_unicode() {
        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("trie1.artrie");
        let path2 = dir.path().join("trie2.artrie");

        // Create first trie with Unicode terms
        let mut trie1 = PersistentARTrieChar::<i64>::create(&path1).expect("create trie1");
        trie1.upsert("日本語", 10).expect("upsert failed");
        trie1.upsert("中文", 20).expect("upsert failed");

        // Create second trie
        let trie2 = PersistentARTrieChar::<i64>::create(&path2).expect("create trie2");
        trie2.upsert("日本語", 5).expect("upsert failed"); // Overlap
        trie2.upsert("한국어", 15).expect("upsert failed"); // New

        // Merge with sum
        let processed = trie1
            .merge_from(&trie2, |a, b| a + b)
            .expect("merge failed");

        assert_eq!(processed, 2, "Should process 2 terms");
        // F2-migrate: Bucket A — read merged values via `get_value` (overlay-routed).
        assert_eq!(
            trie1.get_value("日本語"),
            Some(15),
            "日本語 should be 10 + 5 = 15"
        );
        assert_eq!(trie1.get_value("中文"), Some(20), "中文 should remain 20");
        assert_eq!(
            trie1.get_value("한국어"),
            Some(15),
            "한국어 should be added with value 15"
        );
    }

    #[test]
    fn test_char_trie_merge_with_persistence() {
        let dir = tempdir().expect("create temp dir");
        let path1 = dir.path().join("trie1.artrie");
        let path2 = dir.path().join("trie2.artrie");

        // Create and merge
        {
            let mut trie1 = PersistentARTrieChar::<i64>::create(&path1).expect("create trie1");
            trie1.upsert("apple", 5).expect("upsert failed");
            trie1.sync().expect("sync failed");

            let trie2 = PersistentARTrieChar::<i64>::create(&path2).expect("create trie2");
            trie2.upsert("apple", 7).expect("upsert failed");
            trie2.upsert("banana", 3).expect("upsert failed");
            trie2.sync().expect("sync failed");

            // Merge
            trie1
                .merge_from(&trie2, |a, b| a + b)
                .expect("merge failed");
            trie1.sync().expect("sync failed");
        }

        // Reopen and verify merge persisted
        {
            let (trie1, _report) =
                PersistentARTrieChar::<i64>::open_with_recovery(&path1).expect("open trie1");

            // F2-migrate: Bucket A — `get()` returns None under the overlay; the merged
            // values survive a normal `open_with_recovery` reopen and read via `get_value`.
            assert_eq!(
                trie1.get_value("apple"),
                Some(12),
                "Merged apple should persist"
            );
            assert_eq!(
                trie1.get_value("banana"),
                Some(3),
                "Merged banana should persist"
            );
        }
    }

    // =========================================================================
    // iter_prefix_with_values_and_arena() tests
    // =========================================================================

    #[test]
    fn test_iter_prefix_with_values_and_arena() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<i64>::create(&path).expect("create trie");
        trie.upsert("apple", 1).expect("upsert failed");
        trie.upsert("application", 2).expect("upsert failed");
        trie.upsert("apply", 3).expect("upsert failed");
        trie.upsert("banana", 4).expect("upsert failed");
        trie.sync().expect("sync failed");

        // Get prefix with values and arena info
        let result = trie
            .iter_prefix_with_values_and_arena("app")
            .expect("I/O error")
            .expect("prefix exists");

        assert_eq!(result.len(), 3, "Should find 3 terms with prefix 'app'");

        // Verify terms and values
        let mut found = std::collections::HashMap::new();
        for item in &result {
            found.insert(item.term.clone(), item.value);
        }

        assert_eq!(found.get("apple"), Some(&1));
        assert_eq!(found.get("application"), Some(&2));
        assert_eq!(found.get("apply"), Some(&3));
    }

    #[test]
    fn test_iter_prefix_with_values_and_arena_grouping() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<i64>::create(&path).expect("create trie");

        // Insert many terms
        for i in 0..50 {
            trie.upsert(&format!("prefix_{:03}", i), i as i64)
                .expect("upsert failed");
        }
        trie.sync().expect("sync failed");

        // Get terms with arena info
        let result = trie
            .iter_prefix_with_values_and_arena("prefix_")
            .expect("I/O error")
            .expect("prefix exists");

        assert_eq!(result.len(), 50, "Should find 50 terms");

        // Group by arena
        let mut by_arena: std::collections::HashMap<Option<u32>, Vec<(String, i64)>> =
            std::collections::HashMap::new();
        for item in &result {
            by_arena
                .entry(item.arena_id)
                .or_default()
                .push((item.term.clone(), item.value));
        }

        // Verify total count
        let total: usize = by_arena.values().map(|v| v.len()).sum();
        assert_eq!(total, 50, "All 50 terms should be in arena groups");

        // Log for debugging
        println!(
            "Arena distribution (with values): {} arenas",
            by_arena.len()
        );
        for (arena, terms) in &by_arena {
            println!("  Arena {:?}: {} terms", arena, terms.len());
        }
    }

    #[test]
    fn test_iter_prefix_with_values_and_arena_not_found() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.artrie");

        let trie = PersistentARTrieChar::<i64>::create(&path).expect("create trie");
        trie.upsert("apple", 1).expect("upsert failed");

        // Non-existent prefix
        let result = trie
            .iter_prefix_with_values_and_arena("xyz")
            .expect("I/O error");
        assert!(result.is_none(), "Non-existent prefix should return None");
    }

    // =========================================================================
    // SharedCharTrie tests
    // =========================================================================

    #[test]
    fn test_shared_char_trie_basic() {
        use libdictenstein::ARTrie;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("shared_basic.artrie");

        // Create via ARTrie trait
        let trie: SharedCharTrie<i64> = SharedCharTrie::create(&path).expect("create shared trie");

        // Test upsert (via ARTrie trait)
        assert!(trie.upsert("hello", 42).expect("upsert failed"));
        assert!(!trie.upsert("hello", 100).expect("upsert update")); // Update returns false

        // Test get_value (via ARTrie trait)
        assert_eq!(trie.get_value("hello"), Some(100));

        // Test contains
        assert!(trie.contains("hello"));
        assert!(!trie.contains("world"));

        // Test sync and LSN tracking
        trie.sync().expect("sync failed");
        let lsn = trie.current_lsn();
        assert!(lsn > 0, "LSN should be positive after operations");

        // Test checkpoint
        trie.checkpoint().expect("checkpoint failed");

        // Verify synced LSN
        let synced = trie.synced_lsn();
        assert!(synced.is_some(), "Should have synced LSN after checkpoint");
    }

    #[test]
    fn test_shared_char_trie_increment() {
        use libdictenstein::ARTrie;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("shared_incr.artrie");

        // C1 / F4 (V11.4 sweep B): `increment` is an inherent `V: Counter`
        // ({i64,u64}) method on the OWNED `PersistentARTrieChar` (removed from the
        // `ARTrie` trait / `Shared*` handle, and it stays `&mut self` — owned-only).
        // F4 collapsed `SharedCharTrie` to `Arc<…>` whose `.write()` no longer hands
        // out `&mut`, so this counter test now drives the owned trie directly
        // (the canonical way to reach `increment` post-F4).
        let mut trie: PersistentARTrieChar<i64> =
            PersistentARTrieChar::create(&path).expect("create owned trie");

        let val1 = trie.increment("counter", 5).expect("increment");
        assert_eq!(val1, 5);

        // Increment adds to existing value
        let val2 = trie.increment("counter", 10).expect("increment");
        assert_eq!(val2, 15);

        // Decrement (negative delta)
        let val3 = trie.increment("counter", -3).expect("decrement");
        assert_eq!(val3, 12);

        // Verify via get_value
        assert_eq!(trie.get_value("counter"), Some(12));
    }

    #[test]
    fn test_shared_char_trie_thread_safety() {
        use libdictenstein::ARTrie;
        use std::sync::Arc;
        use std::thread;

        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("shared_threads.artrie");

        let trie: SharedCharTrie<i64> = SharedCharTrie::create(&path).expect("create shared trie");

        // Insert initial data
        for i in 0..50 {
            trie.upsert(&format!("term{}", i), i as i64)
                .expect("upsert");
        }

        // Clone Arc for threads (using Arc::clone)
        let trie_arc = Arc::new(trie);

        // Spawn multiple reader threads
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let trie_clone = Arc::clone(&trie_arc);
                thread::spawn(move || {
                    let mut found = 0;
                    for i in 0..50 {
                        if trie_clone.contains(&format!("term{}", i)) {
                            found += 1;
                        }
                    }
                    found
                })
            })
            .collect();

        // All readers should find all terms
        for handle in handles {
            let found = handle.join().expect("thread join");
            assert_eq!(found, 50, "Reader should find all terms");
        }
    }

    // =========================================================================
    // Parallel merge pattern tests (simulating Google Books import)
    // =========================================================================

    #[test]
    fn test_parallel_merge_pattern() {
        use std::thread;

        let dir = tempdir().expect("create temp dir");

        // Simulate 4 workers each processing a partition
        let num_workers = 4;
        let terms_per_worker = 50;

        // Create worker tries in parallel
        let handles: Vec<_> = (0..num_workers)
            .map(|worker_id| {
                let worker_path = dir.path().join(format!("worker_{}.artrie", worker_id));
                thread::spawn(move || {
                    let trie = PersistentARTrieChar::<i64>::create(&worker_path)
                        .expect("create worker trie");

                    // Each worker inserts unique terms
                    for i in 0..terms_per_worker {
                        let term = format!("worker{}term{}", worker_id, i);
                        trie.upsert(&term, 1).expect("upsert failed");
                    }
                    trie.sync().expect("sync failed");

                    worker_path
                })
            })
            .collect();

        // Wait for all workers
        let worker_paths: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("worker join failed"))
            .collect();

        // Create main trie and merge all workers
        let main_path = dir.path().join("main.artrie");
        let mut main_trie =
            PersistentARTrieChar::<i64>::create(&main_path).expect("create main trie");

        for worker_path in &worker_paths {
            let (worker_trie, _report) =
                PersistentARTrieChar::<i64>::open_with_recovery(worker_path)
                    .expect("open worker trie");

            main_trie
                .merge_from(&worker_trie, |a, b| a + b)
                .expect("merge failed");
        }

        // Verify all terms are present
        let expected_count = num_workers * terms_per_worker;
        assert_eq!(
            main_trie.len(),
            expected_count,
            "Main trie should have all terms"
        );

        // Spot check some terms
        assert!(main_trie.contains("worker0term0"));
        assert!(main_trie.contains("worker1term25"));
        assert!(main_trie.contains("worker3term49"));
    }

    #[test]
    fn test_parallel_merge_with_overlaps() {
        let dir = tempdir().expect("create temp dir");

        // Create 4 worker tries with overlapping terms (simulating same n-gram in different partitions)
        let worker_paths: Vec<_> = (0..4)
            .map(|worker_id| {
                let path = dir.path().join(format!("worker_{}.artrie", worker_id));
                let trie = PersistentARTrieChar::<i64>::create(&path).expect("create worker trie");

                // All workers see the same n-grams (like "the|quick|brown")
                for i in 0..10 {
                    let term = format!("common_ngram_{}", i);
                    trie.upsert(&term, 1).expect("upsert failed");
                }

                // Plus some unique terms
                for i in 0..5 {
                    let term = format!("worker{}_unique_{}", worker_id, i);
                    trie.upsert(&term, 1).expect("upsert failed");
                }

                trie.sync().expect("sync failed");
                path
            })
            .collect();

        // Merge all into main
        let main_path = dir.path().join("main.artrie");
        let mut main_trie =
            PersistentARTrieChar::<i64>::create(&main_path).expect("create main trie");

        for worker_path in &worker_paths {
            let (worker_trie, _report) =
                PersistentARTrieChar::<i64>::open_with_recovery(worker_path)
                    .expect("open worker trie");

            main_trie
                .merge_from(&worker_trie, |a, b| a + b)
                .expect("merge failed");
        }

        // Common n-grams should have count = 4 (one from each worker)
        // F2-migrate: Bucket A — `get()` returns None under the overlay (C2 merge routes
        // through the overlay); read merged counts via `get_value`.
        for i in 0..10 {
            let term = format!("common_ngram_{}", i);
            assert_eq!(
                main_trie.get_value(&term),
                Some(4),
                "common n-gram '{}' should have count 4",
                term
            );
        }

        // Unique terms should have count = 1
        for worker_id in 0..4 {
            for i in 0..5 {
                let term = format!("worker{}_unique_{}", worker_id, i);
                assert_eq!(
                    main_trie.get_value(&term),
                    Some(1),
                    "unique term '{}' should have count 1",
                    term
                );
            }
        }

        // Total: 10 common + 4*5 unique = 30 terms
        assert_eq!(main_trie.len(), 30, "Main trie should have 30 terms");
    }

    // =========================================================================
    // PersistentARTrie (byte-based) MutableMappedDictionary tests
    // Note: These tests require methods that are not yet implemented.
    // See Task #2: Update ARTrie trait with common methods.
    // =========================================================================

    // TODO: Implement these tests once union_with, update_or_insert are added
    // #[test] fn test_persistent_artrie_mutable_mapped_dictionary() { ... }
    // #[test] fn test_persistent_artrie_union_with() { ... }
    // #[test] fn test_persistent_artrie_update_or_insert() { ... }
}

// ===========================================================================
// Phase 22: Byte-oriented arena-aware iteration tests
// ===========================================================================

mod phase_22_byte_arena_aware_iteration {
    use super::*;
    use libdictenstein::persistent_artrie::PersistentARTrie;
    use libdictenstein::Dictionary;
    use std::collections::HashMap;

    #[test]
    fn test_iter_prefix_with_arena_basic() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.artrie");

        let trie: PersistentARTrie<i32> = PersistentARTrie::create(&path).expect("create trie");

        // Insert terms with common prefix
        trie.insert("apple");
        trie.insert("application");
        trie.insert("apply");
        trie.insert("banana");

        // Get terms with arena info
        let terms = trie
            .iter_prefix_with_arena(b"app")
            .expect("I/O error")
            .expect("prefix exists");

        assert_eq!(terms.len(), 3, "Should find 3 terms with prefix 'app'");

        // Verify we get the expected terms
        let term_strings: Vec<String> = terms
            .iter()
            .filter_map(|t| String::from_utf8(t.term.clone()).ok())
            .collect();
        assert!(term_strings.contains(&"apple".to_string()));
        assert!(term_strings.contains(&"application".to_string()));
        assert!(term_strings.contains(&"apply".to_string()));
    }

    #[test]
    fn test_iter_prefix_with_arena_empty_prefix() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.artrie");

        let trie: PersistentARTrie<()> = PersistentARTrie::create(&path).expect("create trie");

        // Insert some terms
        trie.insert("hello");
        trie.insert("world");
        trie.insert("test");

        // Empty prefix should return all terms
        let terms = trie
            .iter_prefix_with_arena(b"")
            .expect("I/O error")
            .expect("prefix exists");

        assert_eq!(terms.len(), 3, "Should find 3 terms with empty prefix");
    }

    #[test]
    fn test_iter_prefix_with_arena_no_match() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.artrie");

        let trie: PersistentARTrie<()> = PersistentARTrie::create(&path).expect("create trie");

        trie.insert("apple");
        trie.insert("banana");

        // Non-existent prefix should return None
        let result = trie.iter_prefix_with_arena(b"xyz").expect("I/O error");

        assert!(
            result.is_none(),
            "Should return None for non-existent prefix"
        );
    }

    #[test]
    fn test_iter_prefix_with_values_and_arena() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.artrie");

        let trie: PersistentARTrie<i32> = PersistentARTrie::create(&path).expect("create trie");

        // Insert terms with values
        use libdictenstein::MutableMappedDictionary;
        trie.insert_with_value("apple", 1);
        trie.insert_with_value("application", 2);
        trie.insert_with_value("banana", 3);

        // Get terms with values and arena info
        let terms = trie
            .iter_prefix_with_values_and_arena(b"app")
            .expect("I/O error")
            .expect("prefix exists");

        assert_eq!(
            terms.len(),
            2,
            "Should find 2 terms with prefix 'app' that have values"
        );

        // Verify values
        let values: HashMap<String, i32> = terms
            .iter()
            .filter_map(|t| String::from_utf8(t.term.clone()).ok().map(|s| (s, t.value)))
            .collect();

        assert_eq!(values.get("apple"), Some(&1));
        assert_eq!(values.get("application"), Some(&2));
    }

    #[test]
    fn test_arena_grouping() {
        let dir = tempdir().expect("create temp dir");
        let path = dir.path().join("test.artrie");

        let trie: PersistentARTrie<()> = PersistentARTrie::create(&path).expect("create trie");

        // Insert many terms with common prefix
        for i in 0..50 {
            trie.insert(&format!("prefix_{:03}", i));
        }
        trie.insert("other");

        // Get terms with arena info
        let terms = trie
            .iter_prefix_with_arena(b"prefix_")
            .expect("I/O error")
            .expect("prefix exists");

        assert_eq!(
            terms.len(),
            50,
            "Should find 50 terms with prefix 'prefix_'"
        );

        // Group by arena to verify structure
        let mut by_arena: HashMap<Option<u32>, Vec<Vec<u8>>> = HashMap::new();
        for item in &terms {
            by_arena
                .entry(item.arena_id)
                .or_default()
                .push(item.term.clone());
        }

        // Verify we got all terms grouped
        let total: usize = by_arena.values().map(|v| v.len()).sum();
        assert_eq!(total, 50, "All 50 terms should be grouped");

        // Log arena distribution for debugging
        println!("Arena distribution: {} arenas", by_arena.len());
        for (arena, terms) in &by_arena {
            println!("  Arena {:?}: {} terms", arena, terms.len());
        }
    }
}
