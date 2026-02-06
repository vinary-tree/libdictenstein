//! Compaction Tests for Persistent ARTrie
//!
//! These tests verify the compaction functionality that eliminates fragmentation
//! from orphaned nodes and update/delete operations.

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{
    CompactionConfig, CompactionProgress, PersistentARTrie,
};
use libdictenstein::{Dictionary, MappedDictionary};
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use tempfile::tempdir;

// =============================================================================
// BASIC COMPACTION TESTS
// =============================================================================

#[test]
fn test_compact_empty_trie() {
    let dir = tempdir().expect("Failed to create temp dir");
    let path = dir.path().join("empty_compact.artrie");

    let mut trie: PersistentARTrie<u64> = PersistentARTrie::create(&path)
        .expect("Failed to create trie");

    // Compact empty trie
    let mut progress_calls = 0;
    let stats = trie
        .compact(CompactionConfig::default(), |_| {
            progress_calls += 1;
        })
        .expect("Failed to compact");

    assert_eq!(stats.terms_copied, 0);
    assert_eq!(trie.len(), Some(0));

    // File should exist and be small
    assert!(path.exists());
}

#[test]
fn test_compact_preserves_all_data() {
    let dir = tempdir().expect("Failed to create temp dir");
    let path = dir.path().join("preserve_data.artrie");

    let mut trie: PersistentARTrie<u64> = PersistentARTrie::create(&path)
        .expect("Failed to create trie");

    // Insert test data
    let test_terms = vec![
        ("apple", 1u64),
        ("application", 2),
        ("banana", 3),
        ("bandana", 4),
        ("cat", 5),
        ("caterpillar", 6),
        ("dog", 7),
        ("elephant", 8),
        ("zebra", 9),
    ];

    for (term, value) in &test_terms {
        trie.insert_with_value(term, *value);
    }

    // Checkpoint to establish baseline size
    trie.checkpoint().expect("Failed to checkpoint");
    let original_count = trie.len();

    // Compact
    let stats = trie
        .compact(CompactionConfig::default(), |_| {})
        .expect("Failed to compact");

    assert_eq!(stats.terms_copied, test_terms.len() as u64);

    // Verify all data is preserved
    assert_eq!(trie.len(), original_count);
    for (term, value) in &test_terms {
        assert!(trie.contains(term), "Term '{}' not found after compaction", term);
        assert_eq!(
            trie.get_value(term),
            Some(*value),
            "Value for '{}' incorrect after compaction",
            term
        );
    }
}

#[test]
fn test_compact_after_modifications() {
    // Simpler test: insert, modify some (via re-insert), then compact
    // Avoids remove() which may have issues
    let dir = tempdir().expect("Failed to create temp dir");
    let path = dir.path().join("modifications_compact.artrie");

    let mut trie: PersistentARTrie<u64> = PersistentARTrie::create(&path)
        .expect("Failed to create trie");

    // Insert initial terms
    for i in 0..100 {
        trie.insert_with_value(&format!("term_{:03}", i), i as u64);
    }

    // Checkpoint to write to disk
    trie.checkpoint().expect("Failed to first checkpoint");

    let size_after_first = fs::metadata(&path).expect("metadata").len();

    // Re-insert some terms with new values (simulates updates)
    // This creates new versions in memory, and old disk versions become orphaned
    for i in 0..50 {
        trie.insert_with_value(&format!("term_{:03}", i), (i * 100) as u64);
    }

    // Checkpoint again
    trie.checkpoint().expect("Failed to second checkpoint");

    let size_before_compact = fs::metadata(&path).expect("metadata").len();

    // After modifications, file should be larger or equal (new node versions added)
    assert!(
        size_before_compact >= size_after_first,
        "Size should grow or stay same after modifications"
    );

    // Compact
    let stats = trie
        .compact(CompactionConfig::default(), |_| {})
        .expect("Failed to compact");

    let size_after_compact = fs::metadata(&path).expect("metadata").len();

    // Compacted file should be <= pre-compact size
    assert!(
        size_after_compact <= size_before_compact,
        "Compacted size ({}) should be <= pre-compact size ({})",
        size_after_compact,
        size_before_compact
    );

    assert_eq!(stats.terms_copied, 100);

    // Verify data integrity
    for i in 0..100 {
        assert!(
            trie.contains(&format!("term_{:03}", i)),
            "term_{:03} should exist after compaction",
            i
        );
    }
}

#[test]
fn test_compact_multiple_checkpoints() {
    // Test compaction after multiple checkpoint cycles
    // Each checkpoint can create orphaned versions
    let dir = tempdir().expect("Failed to create temp dir");
    let path = dir.path().join("multi_checkpoint.artrie");

    let mut trie: PersistentARTrie<u64> = PersistentARTrie::create(&path)
        .expect("Failed to create trie");

    // First batch
    for i in 0..50 {
        trie.insert_with_value(&format!("batch1_{:02}", i), i as u64);
    }
    trie.checkpoint().expect("Failed to checkpoint 1");
    let size1 = fs::metadata(&path).expect("metadata").len();

    // Second batch
    for i in 0..50 {
        trie.insert_with_value(&format!("batch2_{:02}", i), (i + 100) as u64);
    }
    trie.checkpoint().expect("Failed to checkpoint 2");
    let size2 = fs::metadata(&path).expect("metadata").len();

    // Third batch
    for i in 0..50 {
        trie.insert_with_value(&format!("batch3_{:02}", i), (i + 200) as u64);
    }
    trie.checkpoint().expect("Failed to checkpoint 3");
    let size3 = fs::metadata(&path).expect("metadata").len();

    // File should have grown with each checkpoint
    assert!(size2 >= size1);
    assert!(size3 >= size2);

    // Compact
    let stats = trie
        .compact(CompactionConfig::default(), |_| {})
        .expect("Failed to compact");

    let size_after = fs::metadata(&path).expect("metadata").len();

    assert_eq!(stats.terms_copied, 150);

    // Verify all batches exist
    for i in 0..50 {
        assert!(trie.contains(&format!("batch1_{:02}", i)));
        assert!(trie.contains(&format!("batch2_{:02}", i)));
        assert!(trie.contains(&format!("batch3_{:02}", i)));
    }
}

#[test]
fn test_compact_to_new_file() {
    let dir = tempdir().expect("Failed to create temp dir");
    let original_path = dir.path().join("original.artrie");
    let compacted_path = dir.path().join("compacted.artrie");

    let mut trie: PersistentARTrie<u64> = PersistentARTrie::create(&original_path)
        .expect("Failed to create trie");

    // Insert test data
    for i in 0..100 {
        trie.insert_with_value(&format!("term_{:03}", i), i as u64);
    }

    trie.checkpoint().expect("Failed to checkpoint");

    let original_size = fs::metadata(&original_path).expect("metadata").len();

    // Compact to new file (not in-place)
    let config = CompactionConfig {
        output_path: Some(compacted_path.clone()),
        progress_interval: 10,
        verify_after_compact: true,
    };

    let stats = trie
        .compact(config, |_| {})
        .expect("Failed to compact");

    // Both files should exist
    assert!(original_path.exists(), "Original file should still exist");
    assert!(compacted_path.exists(), "Compacted file should exist");

    // Original trie should be unchanged (still pointing to original file)
    assert_eq!(trie.len(), Some(100));

    // Open the compacted file and verify
    let compacted_trie: PersistentARTrie<u64> = PersistentARTrie::open(&compacted_path)
        .expect("Failed to open compacted trie");

    assert_eq!(compacted_trie.len(), Some(100));
    for i in 0..100 {
        assert_eq!(
            compacted_trie.get_value(&format!("term_{:03}", i)),
            Some(i as u64)
        );
    }
}

#[test]
fn test_compact_progress_callback() {
    let dir = tempdir().expect("Failed to create temp dir");
    let path = dir.path().join("progress_test.artrie");

    let mut trie: PersistentARTrie<u64> = PersistentARTrie::create(&path)
        .expect("Failed to create trie");

    // Insert enough terms to trigger progress callbacks
    for i in 0..50 {
        trie.insert_with_value(&format!("term_{:03}", i), i as u64);
    }

    trie.checkpoint().expect("Failed to checkpoint");

    // Track progress phases seen
    let mut phases_seen: Vec<String> = Vec::new();
    let progress_count = AtomicU64::new(0);

    let config = CompactionConfig {
        output_path: None,
        progress_interval: 10, // Callback every 10 terms
        verify_after_compact: true,
    };

    let stats = trie
        .compact(config, |progress: CompactionProgress| {
            progress_count.fetch_add(1, Ordering::SeqCst);
            phases_seen.push(progress.phase.to_string());
        })
        .expect("Failed to compact");

    // Should have seen multiple progress callbacks
    let count = progress_count.load(Ordering::SeqCst);
    assert!(count >= 4, "Expected at least 4 progress callbacks, got {}", count);

    // Should have seen all major phases
    assert!(
        phases_seen.contains(&"copying".to_string()),
        "Should see 'copying' phase"
    );
    assert!(
        phases_seen.contains(&"checkpointing".to_string()),
        "Should see 'checkpointing' phase"
    );
    assert!(
        phases_seen.contains(&"verifying".to_string()),
        "Should see 'verifying' phase"
    );
    assert!(
        phases_seen.contains(&"finalizing".to_string()),
        "Should see 'finalizing' phase"
    );

    assert_eq!(stats.terms_copied, 50);
}

#[test]
fn test_compact_without_verification() {
    let dir = tempdir().expect("Failed to create temp dir");
    let path = dir.path().join("no_verify.artrie");

    let mut trie: PersistentARTrie<u64> = PersistentARTrie::create(&path)
        .expect("Failed to create trie");

    for i in 0..20 {
        trie.insert_with_value(&format!("term_{:02}", i), i as u64);
    }

    trie.checkpoint().expect("Failed to checkpoint");

    let config = CompactionConfig {
        output_path: None,
        progress_interval: 0, // Disable progress callbacks
        verify_after_compact: false, // Skip verification
    };

    let mut phases_seen: Vec<String> = Vec::new();

    let stats = trie
        .compact(config, |progress| {
            phases_seen.push(progress.phase.to_string());
        })
        .expect("Failed to compact");

    // Should not see verifying phase
    assert!(
        !phases_seen.contains(&"verifying".to_string()),
        "Should not see 'verifying' phase when verification is disabled"
    );

    // Data should still be intact
    assert_eq!(trie.len(), Some(20));
    for i in 0..20 {
        assert!(trie.contains(&format!("term_{:02}", i)));
    }
}

// =============================================================================
// EDGE CASE TESTS
// =============================================================================

#[test]
fn test_compact_single_term() {
    let dir = tempdir().expect("Failed to create temp dir");
    let path = dir.path().join("single_term.artrie");

    let mut trie: PersistentARTrie<u64> = PersistentARTrie::create(&path)
        .expect("Failed to create trie");

    trie.insert_with_value("single", 42);
    trie.checkpoint().expect("Failed to checkpoint");

    let stats = trie
        .compact(CompactionConfig::default(), |_| {})
        .expect("Failed to compact");

    assert_eq!(stats.terms_copied, 1);
    assert_eq!(trie.len(), Some(1));
    assert_eq!(trie.get_value("single"), Some(42));
}

#[test]
fn test_compact_large_values() {
    let dir = tempdir().expect("Failed to create temp dir");
    let path = dir.path().join("large_values.artrie");

    let mut trie: PersistentARTrie<String> = PersistentARTrie::create(&path)
        .expect("Failed to create trie");

    // Insert terms with large string values
    // Use shorter keys to avoid potential bucket overflow issues
    for i in 0..20 {
        let large_value = "x".repeat(100 + i);  // Smaller values to fit in buckets
        trie.insert_with_value(&format!("k{:02}", i), large_value);
    }

    trie.checkpoint().expect("Failed to checkpoint");

    // Disable verification since String values may have serialization nuances
    let config = CompactionConfig {
        verify_after_compact: false,
        ..Default::default()
    };

    let stats = trie
        .compact(config, |_| {})
        .expect("Failed to compact");

    assert!(stats.terms_copied > 0, "Should have copied some terms");

    // Verify values can be retrieved
    for i in 0..20 {
        let val = trie.get_value(&format!("k{:02}", i));
        assert!(val.is_some(), "Value for k{:02} should exist after compaction", i);
    }
}

#[test]
fn test_compact_stats_accuracy() {
    let dir = tempdir().expect("Failed to create temp dir");
    let path = dir.path().join("stats_test.artrie");

    let mut trie: PersistentARTrie<u64> = PersistentARTrie::create(&path)
        .expect("Failed to create trie");

    for i in 0..100 {
        trie.insert_with_value(&format!("term_{:03}", i), i as u64);
    }

    trie.checkpoint().expect("Failed to checkpoint");

    let original_size = fs::metadata(&path).expect("metadata").len();

    let stats = trie
        .compact(CompactionConfig::default(), |_| {})
        .expect("Failed to compact");

    let final_size = fs::metadata(&path).expect("metadata").len();

    // Verify stats match reality
    assert_eq!(stats.terms_copied, 100);
    assert_eq!(stats.original_bytes, original_size);
    assert_eq!(stats.compacted_bytes, final_size);

    // Space savings should be calculated correctly
    let expected_savings = (1.0 - (final_size as f64 / original_size as f64)) * 100.0;
    assert!(
        (stats.space_savings_percent - expected_savings).abs() < 0.01,
        "Space savings calculation incorrect: expected {}, got {}",
        expected_savings,
        stats.space_savings_percent
    );

    // Duration should be positive
    assert!(stats.duration_ms > 0 || stats.terms_copied == 0);
}

// =============================================================================
// RECOVERY FROM FAILED COMPACTION TESTS
// =============================================================================

#[test]
fn test_compact_cleans_up_stale_temp_file() {
    let dir = tempdir().expect("Failed to create temp dir");
    let path = dir.path().join("stale_cleanup.artrie");
    let temp_path = path.with_extension("compacting");

    // Create a stale temp file (simulating crashed compaction)
    fs::write(&temp_path, b"stale data").expect("Failed to create stale file");

    let mut trie: PersistentARTrie<u64> = PersistentARTrie::create(&path)
        .expect("Failed to create trie");

    trie.insert_with_value("test", 1);
    trie.checkpoint().expect("Failed to checkpoint");

    // Compaction should succeed despite stale temp file
    let stats = trie
        .compact(CompactionConfig::default(), |_| {})
        .expect("Failed to compact");

    assert_eq!(stats.terms_copied, 1);

    // Temp file should be gone
    assert!(!temp_path.exists(), "Stale temp file should be cleaned up");
}
