//! Concurrent access tests for PersistentARTrie.
//!
//! These tests verify thread-safety of the PersistentARTrie implementation:
//! - Multiple concurrent readers
//! - Single writer with multiple readers
//! - Concurrent transducer queries
//! - Reader during checkpoint operations
//!
//! # Architecture Notes
//!
//! PersistentARTrie uses `SharedARTrie` (Arc<RwLock<...>>) for thread-safety:
//! - Arc::clone creates a shared reference to the same underlying data
//! - Multiple clones can be passed to different threads
//! - RwLock ensures read/write safety
//!
//! # Known Limitations
//!
//! - Bucket capacity is 256 entries per bucket
//! - Tests stay within safe capacity limits
//! - Write operations are serialized by RwLock

#![cfg(feature = "persistent-artrie")]

use libdictenstein::persistent_artrie::{PersistentARTrie, SharedARTrie};
use libdictenstein::persistent_artrie_core::shared_access::SharedTrieAccess;
use libdictenstein::Dictionary;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

/// Number of reader threads for concurrent tests.
const NUM_READERS: usize = 8;

/// Number of operations per thread.
const OPS_PER_THREAD: usize = 100;

/// Generate diverse terms for concurrent tests.
fn generate_terms(count: usize, prefix: &str) -> Vec<String> {
    (0..count).map(|i| format!("{}{:05}", prefix, i)).collect()
}

/// Helper to create a SharedARTrie from a PersistentARTrie
fn make_shared<V: libdictenstein::DictionaryValue>(trie: PersistentARTrie<V>) -> SharedARTrie<V> {
    Arc::new(trie)
}

// =============================================================================
// Test: Multiple Concurrent Readers
// =============================================================================

#[test]
fn test_concurrent_readers() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("concurrent_readers.part");

    // Create and populate dictionary
    let dict: PersistentARTrie<i32> = PersistentARTrie::create(&dict_path).expect("create dict");

    // Insert test terms with values
    let terms: Vec<String> = generate_terms(100, "term");
    for (i, term) in terms.iter().enumerate() {
        let _ = dict.insert_with_value(term, i as i32);
    }
    dict.sync().expect("sync");

    // Wrap in SharedARTrie for thread-safe access
    let shared_dict = make_shared(dict);

    // Use a barrier to synchronize thread starts
    let barrier = Arc::new(Barrier::new(NUM_READERS));
    let terms_arc = Arc::new(terms);
    let success_count = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..NUM_READERS)
        .map(|_| {
            let dict_clone = Arc::clone(&shared_dict);
            let barrier_clone = barrier.clone();
            let terms_clone = terms_arc.clone();
            let success = success_count.clone();

            thread::spawn(move || {
                // Wait for all threads to be ready
                barrier_clone.wait();

                // Perform concurrent reads
                let mut local_success = 0;
                let dict_guard = dict_clone.read();
                for term in terms_clone.iter() {
                    if dict_guard.contains(term) {
                        local_success += 1;
                    }
                }

                success.fetch_add(local_success, Ordering::SeqCst);
            })
        })
        .collect();

    // Wait for all threads to complete
    for handle in handles {
        handle.join().expect("thread join");
    }

    // Each reader should find all 100 terms
    let total = success_count.load(Ordering::SeqCst);
    assert_eq!(
        total,
        NUM_READERS * 100,
        "All readers should find all terms"
    );
}

// =============================================================================
// Test: Single Writer with Multiple Readers
// =============================================================================

#[test]
fn test_single_writer_multiple_readers() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("writer_readers.part");

    // Create dictionary with initial terms
    let dict: PersistentARTrie<i32> = PersistentARTrie::create(&dict_path).expect("create dict");

    // Insert some initial terms
    let initial_terms: Vec<String> = generate_terms(50, "init");
    for (i, term) in initial_terms.iter().enumerate() {
        let _ = dict.insert_with_value(term, i as i32);
    }
    dict.sync().expect("sync");

    // Wrap in SharedARTrie for thread-safe access
    let shared_dict = make_shared(dict);

    let stop_flag = Arc::new(AtomicBool::new(false));
    let read_count = Arc::new(AtomicUsize::new(0));
    let terms_arc = Arc::new(initial_terms.clone());

    // Spawn reader threads
    let reader_handles: Vec<_> = (0..NUM_READERS)
        .map(|_| {
            let dict_clone = Arc::clone(&shared_dict);
            let stop = stop_flag.clone();
            let count = read_count.clone();
            let terms = terms_arc.clone();

            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let dict_guard = dict_clone.read();
                    for term in terms.iter() {
                        // Read operations should succeed even during writes
                        let _ = dict_guard.contains(term);
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                    drop(dict_guard);
                    thread::yield_now();
                }
            })
        })
        .collect();

    // Writer thread: insert new terms
    let new_terms: Vec<String> = generate_terms(50, "new_");
    let writer_dict = Arc::clone(&shared_dict);
    let writer_handle = thread::spawn(move || {
        for (i, term) in new_terms.iter().enumerate() {
            let dict_guard = writer_dict.write();
            let _ = dict_guard.insert_with_value(term, (i + 1000) as i32);
            drop(dict_guard);
            thread::sleep(Duration::from_micros(100));
        }
    });

    // Let it run for a short time
    thread::sleep(Duration::from_millis(100));

    // Stop readers and wait for writer
    stop_flag.store(true, Ordering::SeqCst);
    writer_handle.join().expect("writer join");

    for handle in reader_handles {
        handle.join().expect("reader join");
    }

    // Verify reads occurred
    let reads = read_count.load(Ordering::SeqCst);
    assert!(reads > 0, "Readers should have performed reads");

    // Verify all terms are present after writes complete
    let dict_guard = shared_dict.read();
    for term in initial_terms.iter() {
        assert!(
            dict_guard.contains(term),
            "Initial term should exist: {}",
            term
        );
    }
}

// =============================================================================
// Test: Concurrent Reads During Checkpoint
// =============================================================================

#[test]
fn test_reader_during_checkpoint() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("checkpoint_readers.part");

    // Create and populate dictionary
    let dict: PersistentARTrie<i32> = PersistentARTrie::create(&dict_path).expect("create dict");

    let terms: Vec<String> = generate_terms(100, "chkp");
    for (i, term) in terms.iter().enumerate() {
        let _ = dict.insert_with_value(term, i as i32);
    }
    dict.sync().expect("sync");

    // Wrap in SharedARTrie for thread-safe access
    let shared_dict = make_shared(dict);

    let stop_flag = Arc::new(AtomicBool::new(false));
    let read_errors = Arc::new(AtomicUsize::new(0));
    let terms_arc = Arc::new(terms.clone());

    // Spawn reader threads that continuously read during checkpoint
    let reader_handles: Vec<_> = (0..NUM_READERS)
        .map(|_| {
            let dict_clone = Arc::clone(&shared_dict);
            let stop = stop_flag.clone();
            let errors = read_errors.clone();
            let terms = terms_arc.clone();

            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let dict_guard = dict_clone.read();
                    for term in terms.iter() {
                        if !dict_guard.contains(term) {
                            // Term should always be found (snapshot isolation)
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            })
        })
        .collect();

    // Perform checkpoints while readers are active
    for _ in 0..3 {
        let dict_guard = shared_dict.write();
        dict_guard.checkpoint().expect("checkpoint");
        drop(dict_guard);
        thread::sleep(Duration::from_millis(10));
    }

    // Stop readers
    stop_flag.store(true, Ordering::SeqCst);

    for handle in reader_handles {
        handle.join().expect("reader join");
    }

    // No read errors should occur
    let errors = read_errors.load(Ordering::SeqCst);
    assert_eq!(errors, 0, "No read errors should occur during checkpoints");
}

// =============================================================================
// Test: Concurrent Value Lookups
// =============================================================================

#[test]
fn test_concurrent_value_lookups() {
    use libdictenstein::MappedDictionary;

    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("value_lookups.part");

    // Create and populate dictionary with values
    let dict: PersistentARTrie<i32> = PersistentARTrie::create(&dict_path).expect("create dict");

    let terms: Vec<(String, i32)> = (0..100)
        .map(|i| (format!("value{:05}", i), i * 10))
        .collect();

    for (term, value) in &terms {
        let _ = dict.insert_with_value(term, *value);
    }
    dict.sync().expect("sync");

    // Wrap in SharedARTrie for thread-safe access
    let shared_dict = make_shared(dict);

    let barrier = Arc::new(Barrier::new(NUM_READERS));
    let terms_arc = Arc::new(terms);
    let value_mismatches = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..NUM_READERS)
        .map(|_| {
            let dict_clone = Arc::clone(&shared_dict);
            let barrier_clone = barrier.clone();
            let terms_clone = terms_arc.clone();
            let mismatches = value_mismatches.clone();

            thread::spawn(move || {
                barrier_clone.wait();

                let dict_guard = dict_clone.read();
                for (term, expected) in terms_clone.iter() {
                    if let Some(actual) = dict_guard.get_value(term) {
                        if actual != *expected {
                            mismatches.fetch_add(1, Ordering::Relaxed);
                        }
                    } else {
                        mismatches.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread join");
    }

    let mismatches = value_mismatches.load(Ordering::SeqCst);
    assert_eq!(mismatches, 0, "All values should match expected");
}

// =============================================================================
// Test: Writer Contention
// =============================================================================

#[test]
fn test_writer_contention() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("writer_contention.part");

    let dict: PersistentARTrie<i32> = PersistentARTrie::create(&dict_path).expect("create dict");

    // Wrap in SharedARTrie for thread-safe access
    let shared_dict = make_shared(dict);

    let barrier = Arc::new(Barrier::new(4));
    let successful_inserts = Arc::new(AtomicUsize::new(0));

    // Spawn 4 writer threads, each trying to insert different terms
    let handles: Vec<_> = (0..4)
        .map(|thread_id| {
            let dict_clone = Arc::clone(&shared_dict);
            let barrier_clone = barrier.clone();
            let inserts = successful_inserts.clone();

            thread::spawn(move || {
                barrier_clone.wait();

                // Each thread inserts terms with unique prefix
                let prefix = format!("t{}_", thread_id);
                for i in 0..25 {
                    let term = format!("{}{:03}", prefix, i);
                    let dict_guard = dict_clone.write();
                    if dict_guard.insert_with_value(&term, (thread_id * 100 + i) as i32) {
                        inserts.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread join");
    }

    // All 100 inserts should succeed (4 threads × 25 terms)
    let total = successful_inserts.load(Ordering::SeqCst);
    assert_eq!(total, 100, "All inserts should succeed");

    // Verify all terms are present
    let dict_guard = shared_dict.read();
    for thread_id in 0..4 {
        for i in 0..25 {
            let term = format!("t{}_{:03}", thread_id, i);
            assert!(dict_guard.contains(&term), "Term should exist: {}", term);
        }
    }
}

// =============================================================================
// Test: Read-Write Interleaving
// =============================================================================

#[test]
fn test_read_write_interleaving() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("interleaving.part");

    // Pre-populate with some terms
    let dict: PersistentARTrie<i32> = PersistentARTrie::create(&dict_path).expect("create dict");

    let initial_terms: Vec<String> = generate_terms(50, "pre_");
    for (i, term) in initial_terms.iter().enumerate() {
        let _ = dict.insert_with_value(term, i as i32);
    }

    // Wrap in SharedARTrie for thread-safe access
    let shared_dict = make_shared(dict);

    let operations = Arc::new(AtomicUsize::new(0));
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Spawn interleaved reader/writer threads
    let handles: Vec<_> = (0..4)
        .map(|thread_id| {
            let dict_clone = Arc::clone(&shared_dict);
            let ops = operations.clone();
            let stop = stop_flag.clone();
            let terms = initial_terms.clone();

            thread::spawn(move || {
                let mut local_ops = 0;

                while !stop.load(Ordering::Relaxed) && local_ops < OPS_PER_THREAD {
                    // Alternate between reads and writes
                    if local_ops % 2 == 0 {
                        // Read operation
                        let dict_guard = dict_clone.read();
                        let term = &terms[local_ops % terms.len()];
                        let _ = dict_guard.contains(term);
                    } else {
                        // Write operation
                        let dict_guard = dict_clone.write();
                        let term = format!("new_t{}_{:04}", thread_id, local_ops);
                        let _ = dict_guard.insert_with_value(&term, local_ops as i32);
                    }

                    local_ops += 1;
                    ops.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();

    // Let threads run
    thread::sleep(Duration::from_millis(50));
    stop_flag.store(true, Ordering::SeqCst);

    for handle in handles {
        handle.join().expect("thread join");
    }

    // Should have completed many operations
    let total_ops = operations.load(Ordering::SeqCst);
    assert!(total_ops > 0, "Operations should have been performed");

    // Original terms should still exist
    let dict_guard = shared_dict.read();
    for term in &initial_terms {
        assert!(
            dict_guard.contains(term),
            "Original term should exist: {}",
            term
        );
    }
}

// =============================================================================
// Test: Stress - Many Short-Lived Threads
// =============================================================================

#[test]
fn test_many_short_lived_threads() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("short_lived.part");

    let dict: PersistentARTrie<i32> = PersistentARTrie::create(&dict_path).expect("create dict");

    // Pre-populate
    for i in 0..50 {
        let term = format!("base{:03}", i);
        let _ = dict.insert_with_value(&term, i);
    }
    dict.sync().expect("sync");

    // Wrap in SharedARTrie for thread-safe access
    let shared_dict = make_shared(dict);

    let success_count = Arc::new(AtomicUsize::new(0));

    // Spawn many short-lived threads
    let handles: Vec<_> = (0..50)
        .map(|i| {
            let dict_clone = Arc::clone(&shared_dict);
            let success = success_count.clone();

            thread::spawn(move || {
                // Each thread does a few operations then exits
                let dict_guard = dict_clone.read();
                let term = format!("base{:03}", i % 50);
                if dict_guard.contains(&term) {
                    success.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread join");
    }

    let successes = success_count.load(Ordering::SeqCst);
    assert_eq!(successes, 50, "All lookups should succeed");
}

// =============================================================================
// Test: Concurrent Opens of Same Dictionary (should fail)
// =============================================================================

#[test]
fn test_concurrent_opens_same_path() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("same_path.part");

    // Create the dictionary
    let _dict: PersistentARTrie<()> = PersistentARTrie::create(&dict_path).expect("create dict");

    // Try to create another dictionary at the same path - should fail
    let result: Result<PersistentARTrie<()>, _> = PersistentARTrie::create(&dict_path);
    assert!(
        result.is_err(),
        "Creating another dictionary at same path should fail"
    );
}

// =============================================================================
// Test: SharedARTrie Shares State
// =============================================================================

#[test]
fn test_shared_artrie_shares_state() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("shared_state.part");

    let dict: PersistentARTrie<i32> = PersistentARTrie::create(&dict_path).expect("create dict");

    // Insert via original handle
    let _ = dict.insert_with_value("hello", 42);

    // Wrap in SharedARTrie
    let shared_dict = make_shared(dict);

    // Clone should see the insert
    let dict_clone = Arc::clone(&shared_dict);
    {
        let dict_guard = dict_clone.read();
        assert!(
            dict_guard.contains("hello"),
            "Clone should see original insert"
        );
    }

    // Insert via clone
    {
        let dict_guard = dict_clone.write();
        let _ = dict_guard.insert_with_value("world", 100);
    }

    // Original should see clone's insert
    {
        let dict_guard = shared_dict.read();
        assert!(
            dict_guard.contains("world"),
            "Original should see clone's insert"
        );
    }
}

// =============================================================================
// Test: Sync From Multiple Threads
// =============================================================================

#[test]
fn test_sync_from_multiple_threads() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("multi_sync.part");

    let dict: PersistentARTrie<i32> = PersistentARTrie::create(&dict_path).expect("create dict");

    // Insert some data
    for i in 0..50 {
        let _ = dict.insert_with_value(&format!("sync{:03}", i), i);
    }

    // Wrap in SharedARTrie for thread-safe access
    let shared_dict = make_shared(dict);

    let barrier = Arc::new(Barrier::new(4));
    let sync_errors = Arc::new(AtomicUsize::new(0));

    // Multiple threads calling sync concurrently
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let dict_clone = Arc::clone(&shared_dict);
            let barrier_clone = barrier.clone();
            let errors = sync_errors.clone();

            thread::spawn(move || {
                barrier_clone.wait();

                // Multiple sync calls should be safe
                for _ in 0..5 {
                    let dict_guard = dict_clone.read();
                    if dict_guard.sync().is_err() {
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread join");
    }

    let errors = sync_errors.load(Ordering::SeqCst);
    assert_eq!(errors, 0, "All sync calls should succeed");
}

// =============================================================================
// Test: Transducer Queries (if available)
// =============================================================================

// Note: Concurrent transducer tests require the transducer module.
// This is a placeholder for when that integration is needed.
#[test]
#[ignore = "Transducer concurrent tests require transducer module integration"]
fn test_concurrent_transducer_queries() {
    // TODO: Add transducer concurrent query tests when needed
    // This would test multiple threads querying with Levenshtein automata
}

// =============================================================================
// Test: Lock-Free CAS Insert
// =============================================================================

#[test]
fn test_lockfree_insert_cas_basic() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("lockfree_basic.part");

    let mut dict: PersistentARTrie<()> = PersistentARTrie::create(&dict_path).expect("create dict");

    // Enable lock-free mode
    dict.enable_lockfree();

    // Insert some terms using CAS
    assert!(dict.insert_cas(b"hello"), "First insert should succeed");
    assert!(
        !dict.insert_cas(b"hello"),
        "Duplicate insert should return false"
    );

    assert!(dict.insert_cas(b"world"), "Second term should succeed");
    assert!(dict.insert_cas(b"foo"), "Third term should succeed");

    // Verify using lock-free contains
    assert!(dict.contains_lockfree(b"hello"), "Should find hello");
    assert!(dict.contains_lockfree(b"world"), "Should find world");
    assert!(dict.contains_lockfree(b"foo"), "Should find foo");
    assert!(!dict.contains_lockfree(b"bar"), "Should not find bar");
}

#[test]
fn test_lockfree_insert_cas_concurrent() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("lockfree_concurrent.part");

    let mut dict: PersistentARTrie<()> = PersistentARTrie::create(&dict_path).expect("create dict");

    // Enable lock-free mode
    dict.enable_lockfree();

    // Wrap in Arc for sharing (no RwLock needed for lock-free ops!)
    let dict = Arc::new(dict);

    let barrier = Arc::new(Barrier::new(NUM_READERS));
    let insert_count = Arc::new(AtomicUsize::new(0));

    // Spawn multiple threads doing concurrent inserts
    let handles: Vec<_> = (0..NUM_READERS)
        .map(|thread_id| {
            let dict_clone = Arc::clone(&dict);
            let barrier_clone = barrier.clone();
            let count = insert_count.clone();

            thread::spawn(move || {
                barrier_clone.wait();

                // Each thread inserts unique terms
                for i in 0..OPS_PER_THREAD {
                    let term = format!("t{}_{:05}", thread_id, i);
                    if dict_clone.insert_cas(term.as_bytes()) {
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread join");
    }

    // All inserts should succeed since each thread uses unique terms
    let total = insert_count.load(Ordering::SeqCst);
    assert_eq!(
        total,
        NUM_READERS * OPS_PER_THREAD,
        "All unique inserts should succeed"
    );

    // Verify all terms are findable
    for thread_id in 0..NUM_READERS {
        for i in 0..OPS_PER_THREAD {
            let term = format!("t{}_{:05}", thread_id, i);
            assert!(
                dict.contains_lockfree(term.as_bytes()),
                "Term should be found: {}",
                term
            );
        }
    }

    // Check CAS retry count (should be low for unique terms)
    let retries = dict.cas_retry_count();
    println!(
        "CAS retries for {} inserts: {} ({:.2}%)",
        NUM_READERS * OPS_PER_THREAD,
        retries,
        100.0 * retries as f64 / (NUM_READERS * OPS_PER_THREAD) as f64
    );
}

#[test]
fn test_lockfree_insert_cas_same_terms() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("lockfree_same.part");

    let mut dict: PersistentARTrie<()> = PersistentARTrie::create(&dict_path).expect("create dict");

    // Enable lock-free mode
    dict.enable_lockfree();

    // Wrap in Arc for sharing
    let dict = Arc::new(dict);

    let barrier = Arc::new(Barrier::new(NUM_READERS));
    let insert_success = Arc::new(AtomicUsize::new(0));

    // Generate a shared list of terms
    let terms: Vec<String> = (0..50).map(|i| format!("shared_{:03}", i)).collect();
    let terms_arc = Arc::new(terms);

    // Spawn multiple threads trying to insert the SAME terms
    let handles: Vec<_> = (0..NUM_READERS)
        .map(|_| {
            let dict_clone = Arc::clone(&dict);
            let barrier_clone = barrier.clone();
            let count = insert_success.clone();
            let terms = terms_arc.clone();

            thread::spawn(move || {
                barrier_clone.wait();

                // Each thread tries to insert the same 50 terms
                for term in terms.iter() {
                    if dict_clone.insert_cas(term.as_bytes()) {
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread join");
    }

    // Only 50 inserts should succeed (one per unique term)
    let total = insert_success.load(Ordering::SeqCst);
    assert_eq!(
        total, 50,
        "Only 50 unique terms should be inserted, got {}",
        total
    );

    // Verify all terms exist
    for term in terms_arc.iter() {
        assert!(
            dict.contains_lockfree(term.as_bytes()),
            "Term should be found: {}",
            term
        );
    }
}

#[test]
fn test_lockfree_merge_to_persistent() {
    let temp_dir = TempDir::new().expect("create temp dir");
    let dict_path = temp_dir.path().join("lockfree_merge.part");

    let mut dict: PersistentARTrie<()> = PersistentARTrie::create(&dict_path).expect("create dict");
    // **M4b REFRAME.** A fresh `create::<()>()` now create-flips to the overlay, but
    // this test drains the overlay into the owned tree via `merge_lockfree_to_persistent`,
    // which the flip REJECTS (the overlay IS the durable production state). Force the
    // owned regime — then the explicit `enable_lockfree` + `insert_cas` + merge exercises
    // the pre-flip lockfree→persistent merge path this test was written for.
    dict.kill_switch_to_owned();

    // Enable lock-free mode
    dict.enable_lockfree();

    // Insert terms using lock-free CAS
    let terms: Vec<String> = (0..100).map(|i| format!("merge_{:03}", i)).collect();
    for term in &terms {
        dict.insert_cas(term.as_bytes());
    }

    // Verify terms exist in lock-free layer
    for term in &terms {
        assert!(
            dict.contains_lockfree(term.as_bytes()),
            "Term should be in lockfree layer: {}",
            term
        );
    }

    // Merge to persistent storage
    let merged = dict.merge_lockfree_to_persistent().expect("merge");
    assert_eq!(merged, 100, "Should have merged 100 terms");

    // Verify terms exist in persistent storage (using regular contains)
    for term in &terms {
        assert!(
            dict.contains(term),
            "Term should be in persistent storage: {}",
            term
        );
    }
}

/// Byte-overlay regression for the prefix-insert data-loss fix (mirror of the
/// char `prefix_insert_survives_merge_into_persistent_trie`).
///
/// Inserting a term that is a proper prefix of an existing term ("a" after "ab")
/// must report newness `true` and survive the cache-only
/// `merge_lockfree_to_persistent`. Pre-fix, `insert_cas(b"a")` returned `false`
/// and skipped the lock-free cache, so the merge silently dropped "a" — visible
/// only by reading the persistent trie (`contains`), which is what this asserts.
#[test]
fn test_lockfree_prefix_insert_survives_merge() {
    std::fs::create_dir_all("target/test-tmp").ok();
    let temp_dir = tempfile::Builder::new()
        .prefix("byte-prefix-merge")
        .tempdir_in("target/test-tmp")
        .expect("scratch tempdir under target/test-tmp (never /tmp — it is tmpfs)");
    let dict_path = temp_dir.path().join("byte_prefix_merge.part");
    let mut dict: PersistentARTrie<()> = PersistentARTrie::create(&dict_path).expect("create dict");
    // **M4b REFRAME.** A fresh `create::<()>()` now create-flips; this prefix-insert
    // regression drains the overlay via `merge_lockfree_to_persistent`, rejected under
    // the flip. Force the owned regime so the explicit-overlay merge path runs.
    dict.kill_switch_to_owned();
    dict.enable_lockfree();

    assert!(dict.insert_cas(b"ab"), "\"ab\" is a new term");
    assert!(
        dict.insert_cas(b"a"),
        "\"a\" (a proper prefix of \"ab\") is a new term — insert_cas must report true"
    );
    assert!(dict.contains_lockfree(b"a"));
    assert!(dict.contains_lockfree(b"ab"));

    let merged = dict.merge_lockfree_to_persistent().expect("merge");
    assert_eq!(
        merged, 2,
        "both \"ab\" and \"a\" must be merged (none dropped)"
    );

    // Read the persistent trie — the layer the cache-only merge writes to.
    assert!(
        dict.contains("a"),
        "prefix term \"a\" was lost during merge into the persistent trie (data loss)"
    );
    assert!(dict.contains("ab"));
}
