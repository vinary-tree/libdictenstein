#![cfg(all(feature = "persistent-artrie", target_os = "linux"))]
//! Property-based regression test for the background-thread-leak fix.
//!
//! Complements the example-based `persistent_char_thread_lifecycle.rs`: instead
//! of one fixed create→drop loop, this drives *random sequences* of trie
//! lifecycles — plain, eviction-enabled, and eviction-enabled-then-disabled —
//! and asserts the process's daemon-thread count always returns to baseline.
//! Pre-fix, any eviction-enabled iteration leaked ~3 OS threads; post-fix the
//! workers hold a `Weak` and `Drop`/`disable_eviction` join them.
//!
//! Linux-only: thread accounting reads `/proc/self/task`.

use libdictenstein::artrie_trait::{ARTrie, EvictableARTrie};
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::persistent_artrie_char::SharedCharARTrie;
use libdictenstein::MutableMappedDictionary;
use proptest::prelude::*;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// Serialize against any other thread-lifecycle test in this binary (they count
/// process-global daemon threads by name).
fn serialize_thread_tests() -> std::sync::MutexGuard<'static, ()> {
    static THREAD_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    THREAD_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Count this process's live trie background daemon threads (prefix-matched to
/// survive the kernel's 15-char `comm` truncation).
fn trie_thread_count() -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir("/proc/self/task") {
        for entry in entries.flatten() {
            if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) {
                let name = comm.trim();
                if name == "wal-sync" || name.starts_with("artrie-") {
                    count += 1;
                }
            }
        }
    }
    count
}

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

/// One trie lifecycle to exercise.
#[derive(Debug, Clone)]
enum Lifecycle {
    /// Create + insert, then drop (only the `wal-sync` daemon).
    Plain,
    /// Create + insert + `enable_eviction`, then drop (all three daemons).
    Eviction,
    /// Create + insert + `enable_eviction` + `disable_eviction`, then drop.
    EvictionThenDisable,
}

fn lifecycle_strategy() -> impl Strategy<Value = Lifecycle> {
    prop_oneof![
        Just(Lifecycle::Plain),
        Just(Lifecycle::Eviction),
        Just(Lifecycle::EvictionThenDisable),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, .. ProptestConfig::default() })]

    /// Property: for any sequence of trie lifecycles, the daemon-thread count
    /// returns to the pre-sequence baseline once every trie is torn down.
    #[test]
    fn daemon_threads_return_to_baseline(
        ops in prop::collection::vec(lifecycle_strategy(), 1..12)
    ) {
        let _serial = serialize_thread_tests();
        let baseline = trie_thread_count();

        for op in &ops {
            let dir = tempdir().expect("tempdir");
            let path = dir.path().join("t.artrie");
            let shared: SharedCharARTrie<i32> = ARTrie::create(&path).expect("create");
            let _ = MutableMappedDictionary::insert_with_value(&shared, "alpha", 1);
            match op {
                Lifecycle::Plain => {}
                Lifecycle::Eviction => {
                    let _ = shared.enable_eviction(EvictionConfig::default());
                }
                Lifecycle::EvictionThenDisable => {
                    let _ = shared.enable_eviction(EvictionConfig::default());
                    let _ = shared.disable_eviction();
                }
            }
            drop(shared);
        }

        let after = wait_until_threads_at_most(baseline, Duration::from_secs(10));
        prop_assert!(
            after <= baseline,
            "daemon threads leaked across {} lifecycles: baseline={}, after={}, ops={:?}",
            ops.len(), baseline, after, ops
        );
    }
}
