#![cfg(feature = "persistent-artrie")]
//! **Vocab F4 — the lock-collapse concurrent soak (deadlock + no-lost-write under load).**
//!
//! The vocab analogue of `persistent_f4_lock_collapse_soak.rs`: sustained concurrent load on ONE
//! collapsed `Arc<PersistentVocabARTrie>` handle for a fixed wall-clock budget, exercising every
//! path the F4 collapse and the new lossless `clone`/`fork_to` touch:
//!
//!   W writer threads  ‖  R reader threads  ‖  2 checkpointer threads (contend `checkpoint_lock`)
//!   ‖  an eviction enable→force→disable churner  ‖  a snapshot/`fork_to` churner
//!
//! **Pass criteria:**
//! 1. NO DEADLOCK — completes within the caller's `timeout` wrapper
//!    (`timeout 90 cargo test … vocab_shared_lockfree_soak`); a lock cycle or a
//!    join-while-holding-lock hangs and the timeout kills it.
//! 2. NO LOST WRITE — every term a writer's `insert` ACKNOWLEDGED is still readable at the end and
//!    after drop→reopen (the lock-free overlay write is never excluded by a concurrent checkpoint /
//!    eviction / snapshot).
//! 3. SNAPSHOT/FORK CONSISTENCY UNDER MUTATION — a `snapshot()` (materialize-from-frozen-root) and
//!    a `fork_to()` taken while writers hammer the source are each internally self-consistent
//!    (`iter_terms` round-trips through `get_index`/`get_term`; `len` agrees), and a fork reopens.
//!
//! Scratch is REAL disk (`target/test-tmp`), never `/tmp` (tmpfs on this host).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use libdictenstein::artrie_trait::{ARTrie, EvictableARTrie};
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::persistent_artrie::vocab::{PersistentVocabARTrie, SharedVocabARTrie};

const WRITERS: usize = 6;
const READERS: usize = 6;
const BUDGET: Duration = Duration::from_secs(6);
/// `fork_to` is O(n) durable inserts (fsync-bound), so cap it to a few early churns (large-scale
/// fork correctness is covered by `vocab_scale_checkpoint_repro` + `vocab_clone_fork_lossless`).
const MAX_FORKS: u32 = 3;

fn scratch_dir() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from("target/test-tmp");
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn unique(tag: &str) -> std::path::PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    scratch_dir().join(format!("vocab-soak-{tag}-{pid}-{nanos}-{seq}.vocab"))
}

/// Internal-consistency audit of a value (a live trie, a snapshot, or a fork): every term it lists
/// round-trips through `get_index` → `get_term`, and `len` matches the enumeration.
fn assert_self_consistent(t: &PersistentVocabARTrie, label: &str) {
    let terms: Vec<String> = t.iter_terms().collect();
    assert_eq!(
        terms.len(),
        t.len(),
        "{label}: iter_terms count ({}) != len ({})",
        terms.len(),
        t.len()
    );
    // Bounded round-trip sample (the full set can be large under load).
    for term in terms.iter().take(64) {
        let id = t
            .get_index(term)
            .unwrap_or_else(|| panic!("{label}: lists {term:?} but get_index is None"));
        assert_eq!(
            t.get_term(id).as_deref(),
            Some(term.as_str()),
            "{label}: reverse map disagrees for id {id} ({term:?})"
        );
    }
}

#[test]
fn vocab_soak_no_deadlock_no_lost_write_consistent_snapshots() {
    let path = unique("main");
    let trie: SharedVocabARTrie =
        Arc::new(PersistentVocabARTrie::create(&path).expect("create soak vocab"));

    let acked: Arc<Mutex<BTreeSet<String>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let mut handles: Vec<JoinHandle<()>> = Vec::new();

    // ── Writers: lock-free `&self` overlay inserts (disjoint per-writer namespaces; every term is
    //    unique, so the count-based `ARTrie::insert` returns `true` exactly on a successful add). ──
    for w in 0..WRITERS {
        let trie = Arc::clone(&trie);
        let acked = Arc::clone(&acked);
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            let mut i = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let term = format!("w{w}-{i:08}");
                if trie.insert(&term) {
                    acked.lock().expect("acked").insert(term);
                }
                i += 1;
                if i.is_multiple_of(64) {
                    std::thread::yield_now();
                }
            }
        }));
    }

    // ── Readers: lock-free forward + reverse + prefix reads (never blocked by writers). ──
    for _ in 0..READERS {
        let trie = Arc::clone(&trie);
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            let mut reads = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let _ = trie.get_index("w0-00000000");
                let _ = trie.contains("w1-00000001");
                let _ = trie.get_term(0);
                let _ = trie.len();
                if reads.is_multiple_of(97) {
                    let _ = trie.iter_terms_with_prefix("w2-").take(4).count();
                }
                reads += 1;
                if reads.is_multiple_of(128) {
                    std::thread::yield_now();
                }
            }
        }));
    }

    // ── Two checkpointers via the ARTrie trait (contend the new `checkpoint_lock`). ──
    for _ in 0..2 {
        let trie = Arc::clone(&trie);
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                ARTrie::checkpoint(&trie).expect("soak checkpoint must not error");
                std::thread::sleep(Duration::from_millis(5));
            }
        }));
    }

    // ── Eviction churner: enable → force → disable (the deadlock-prone combo). ──
    {
        let trie = Arc::clone(&trie);
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if trie
                    .enable_eviction(EvictionConfig::without_memory_monitor())
                    .is_ok()
                {
                    let _ = trie.force_eviction(1 << 16);
                    trie.disable_eviction()
                        .expect("disable must not error/hang");
                }
                std::thread::sleep(Duration::from_millis(7));
            }
        }));
    }

    // ── Snapshot / fork churner: the new clone + fork_to paths under concurrent mutation. ──
    {
        let trie = Arc::clone(&trie);
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            let mut last_fork = Instant::now();
            let mut forks = 0u32;
            while !stop.load(Ordering::Relaxed) {
                // A materialize-from-frozen-root snapshot taken WHILE writers mutate the source
                // must be internally self-consistent (no skew).
                let snap = trie.snapshot();
                assert_self_consistent(&snap, "snapshot");

                // fork_to is O(n) durable inserts — cap the count + time-gate so it doesn't
                // dominate the soak (it grows fsync-bound as the trie grows).
                if forks < MAX_FORKS && last_fork.elapsed() >= Duration::from_millis(1000) {
                    let fpath = unique("fork");
                    if let Ok(fork) = trie.fork_to(&fpath) {
                        assert_self_consistent(&fork, "fork");
                        drop(fork);
                        // Reopen the fork's own file — its published image round-trips.
                        let (reopened, _r) = PersistentVocabARTrie::open_with_recovery(&fpath)
                            .expect("fork reopens");
                        assert_self_consistent(&reopened, "fork-reopened");
                        drop(reopened);
                        let _ = std::fs::remove_file(&fpath);
                        let _ = std::fs::remove_file(fpath.with_extension("vocab.wal"));
                    }
                    forks += 1;
                    last_fork = Instant::now();
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        }));
    }

    // Run the budget, then stop + join EVERYTHING (a deadlock blocks the join → caller timeout).
    std::thread::sleep(BUDGET);
    stop.store(true, Ordering::Release);
    for h in handles {
        h.join().expect("a soak thread panicked");
    }
    trie.disable_eviction().expect("final disable");

    // ── NO-LOST-WRITE AUDIT (in memory) ──
    let acked = acked.lock().expect("acked").clone();
    assert!(
        !acked.is_empty(),
        "soak made no progress — no writes acknowledged (suspect a stall)"
    );
    let mut missing = Vec::new();
    for term in &acked {
        if !trie.contains(term) {
            missing.push(term.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "LOST WRITE: {} of {} acknowledged terms not readable after soak (first: {:?})",
        missing.len(),
        acked.len(),
        missing.iter().take(10).collect::<Vec<_>>()
    );
    // Reverse map round-trips for a sample of acked terms.
    for term in acked.iter().take(64) {
        let id = trie.get_index(term).expect("acked term present");
        assert_eq!(trie.get_term(id).as_deref(), Some(term.as_str()));
    }

    // ── SURVIVES REOPEN ──
    drop(trie);
    let (reopened, _r) =
        PersistentVocabARTrie::open_with_recovery(&path).expect("reopen soak vocab");
    let mut missing_after = Vec::new();
    for term in &acked {
        if !reopened.contains(term) {
            missing_after.push(term.clone());
        }
    }
    assert!(
        missing_after.is_empty(),
        "LOST WRITE ACROSS REOPEN: {} of {} acknowledged terms missing after restart",
        missing_after.len(),
        acked.len()
    );

    drop(reopened);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("vocab.wal"));
}
