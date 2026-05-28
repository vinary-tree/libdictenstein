#![cfg(all(feature = "persistent-artrie", target_os = "linux"))]
//! Regression test for the background-thread leak fix — byte (`u8`) variant.
//!
//! The companion `persistent_char_thread_lifecycle.rs` covers the UTF-8 trie;
//! this asserts the same guarantees for the byte trie. Each disk-backed
//! `PersistentARTrie` spawns up to three background daemon threads — `wal-sync`
//! (on create), and `artrie-eviction` + `artrie-memory-monitor` (on
//! `enable_eviction`). Historically the worker closures captured a strong `Arc`
//! to their manager, so the manager's `Drop` never ran and the OS threads leaked
//! once per trie instance (the production symptom: ~14k stuck threads, ~38 GB
//! RSS). The fix makes the workers hold a `Weak` and adds
//! `PersistentARTrie::close`/`Drop`, so the threads are joined when the trie is
//! dropped.
//!
//! These tests assert the threads return to baseline after create→drop, and
//! that explicit `disable_eviction` is deadlock-free + idempotent (it joins the
//! eviction thread, which itself takes the trie write lock — joining while
//! holding that lock would deadlock).
//!
//! Linux-only: thread accounting reads `/proc/self/task`.

use libdictenstein::artrie_trait::{ARTrie, EvictableARTrie};
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::persistent_artrie::SharedARTrie;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// Serialize the thread-lifecycle tests *within this binary*.
///
/// Both tests count process-global daemon threads by name, so running them
/// concurrently (the `cargo test` default of one thread per `#[test]` in a
/// shared process) would let one test's daemons pollute another's baseline.
/// Holding this guard for the whole test body makes the counts reliable.
/// (`cargo nextest` additionally runs each test in its own process.)
fn serialize_thread_tests() -> std::sync::MutexGuard<'static, ()> {
    static THREAD_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    THREAD_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Count this process's live trie background daemon threads by name.
fn trie_thread_count() -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir("/proc/self/task") {
        for entry in entries.flatten() {
            if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) {
                // The kernel truncates a thread's `comm` to 15 chars
                // (TASK_COMM_LEN = 16, incl. NUL), so the longer daemon names
                // arrive truncated: "artrie-memory-monitor" -> "artrie-memory-m".
                // Match by prefix so every trie daemon (incl. the monitor) is
                // counted.
                let name = comm.trim();
                if name == "wal-sync" || name.starts_with("artrie-") {
                    count += 1;
                }
            }
        }
    }
    count
}

/// Poll until the trie-thread count drops to `target` or the timeout elapses.
/// (The join completes synchronously on drop, but the kernel reaping the task
/// from `/proc` can lag by a hair, so we allow a brief settle window.)
fn wait_until_threads_at_most(target: usize, timeout: Duration) -> usize {
    let start = Instant::now();
    loop {
        let now = trie_thread_count();
        if now <= target || start.elapsed() > timeout {
            return now;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Build a disk-backed shared byte trie with all three daemon threads running:
/// `wal-sync` (from create) plus `artrie-eviction` + `artrie-memory-monitor`
/// (from `enable_eviction` with the default, monitor-enabled config).
fn build_trie_with_eviction(path: &std::path::Path) -> SharedARTrie<()> {
    let shared: SharedARTrie<()> = ARTrie::create(path).expect("create byte trie");
    // The byte trie is a plain (value-less) dictionary — it inserts terms via
    // the `ARTrie` trait (`SharedARTrie` implements neither
    // `MutableMappedDictionary` nor `MutableDictionary`, which the char/vocab
    // tries use for valued inserts).
    assert!(ARTrie::insert(&shared, "alpha"));
    assert!(ARTrie::insert(&shared, "alphabet"));
    shared
        .enable_eviction(EvictionConfig::default())
        .expect("enable eviction");
    shared
}

#[test]
fn background_threads_reclaimed_on_drop() {
    let _serial = serialize_thread_tests();
    let baseline = trie_thread_count();

    // Repeatedly create a fully-threaded trie and drop it. Pre-fix, each
    // iteration leaked ~3 OS threads; post-fix, drop joins them all.
    for _ in 0..16 {
        let dir = tempdir().expect("tempdir");
        let shared = build_trie_with_eviction(&dir.path().join("t.artrie"));
        drop(shared); // PersistentARTrie::Drop -> close() -> joins all daemons
    }

    let after = wait_until_threads_at_most(baseline, Duration::from_secs(10));
    assert!(
        after <= baseline,
        "trie background threads leaked across 16 create/drop cycles: \
         baseline={baseline}, after={after}"
    );
}

#[test]
fn disable_eviction_is_deadlock_free_and_idempotent() {
    let _serial = serialize_thread_tests();
    let baseline = trie_thread_count();
    let dir = tempdir().expect("tempdir");
    let shared = build_trie_with_eviction(&dir.path().join("t.artrie"));

    // Explicit eviction teardown must not deadlock: `shutdown()` joins the
    // eviction thread, whose callback takes the trie write lock. The fix takes
    // the coordinator out under a short guard, then joins with NO guard held.
    shared.disable_eviction().expect("disable eviction");
    // Idempotent: the coordinator is already gone, so this is a no-op.
    shared
        .disable_eviction()
        .expect("disable eviction (idempotent)");

    drop(shared); // joins the remaining wal-sync thread

    let after = wait_until_threads_at_most(baseline, Duration::from_secs(10));
    assert!(
        after <= baseline,
        "threads leaked after disable_eviction + drop: baseline={baseline}, after={after}"
    );
}
