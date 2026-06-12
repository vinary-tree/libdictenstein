#![cfg(feature = "persistent-artrie")]
//! **F4 — the lock-collapse concurrent soak (deadlock + no-lost-write under load).**
//!
//! Runs the EXACT scenario the F4 deadlock-freedom loom models, but against the
//! REAL collapsed `Arc<PersistentARTrieChar>` handle under sustained concurrent
//! load for a fixed wall-clock budget:
//!
//!   N writer threads  ‖  a checkpointer thread  ‖  an eviction enable/force/disable
//!   churner  ‖  reader threads
//!
//! all sharing one `SharedCharARTrie` (now a bare `Arc` — overlay reads AND writes
//! are lock-free; only checkpoint / the dormant owned path / eviction take their
//! dedicated inner locks under the `CK > merge_lock > OR > EC` hierarchy).
//!
//! **Pass criteria:**
//! 1. NO DEADLOCK — the whole soak completes well within the harness `timeout`
//!    wrapper (the caller runs `timeout 40 cargo test … f4_lock_collapse_soak`); a
//!    lock-ordering cycle or a join-while-holding-lock would hang and the timeout
//!    would kill it (non-zero exit).
//! 2. NO LOST WRITE — every term a writer's `insert_with_value` ACKNOWLEDGED is
//!    still readable at the end (the lock-free overlay write is never excluded by a
//!    concurrent checkpoint / eviction / disable; checkpoint captures by the
//!    committed watermark, never tearing an acknowledged write).
//!
//! Scratch is real disk (`target/test-tmp`), never `/tmp` (tmpfs on this host —
//! disk-backed durability semantics need a real filesystem).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use libdictenstein::artrie_trait::{ARTrie, EvictableARTrie};
use libdictenstein::persistent_artrie::char::SharedCharARTrie;
use libdictenstein::persistent_artrie::core::shared_access::SharedTrieAccess;
use libdictenstein::persistent_artrie::eviction::EvictionConfig;
use libdictenstein::MappedDictionary;

fn scratch() -> std::path::PathBuf {
    std::fs::create_dir_all("target/test-tmp").ok();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::path::PathBuf::from(format!("target/test-tmp/f4-soak-{pid}-{nanos}.artc"))
}

/// The headline soak: writers ‖ checkpointer ‖ eviction-churner ‖ readers for a
/// fixed budget. Completes (no deadlock) and loses no acknowledged write.
#[test]
fn f4_lock_collapse_soak_no_deadlock_no_lost_write() {
    // Budget kept modest so the default `cargo test` run is bounded; the caller's
    // `timeout` wrapper is the deadlock tripwire. ~12s of active churn.
    let budget = Duration::from_secs(12);
    let n_writers = 4usize;
    let n_readers = 2usize;

    let path = scratch();
    let trie: SharedCharARTrie<u64> = ARTrie::create(&path).expect("create soak trie");

    // The set of ACKNOWLEDGED terms (insert_with_value returned without error),
    // gathered across all writers — the no-lost-write oracle.
    let acked: Arc<Mutex<BTreeSet<String>>> = Arc::new(Mutex::new(BTreeSet::new()));
    let stop = Arc::new(AtomicBool::new(false));

    let mut handles = Vec::new();

    // ── Writer threads (lock-free overlay inserts — take NO lock) ──
    for w in 0..n_writers {
        let trie = Arc::clone(&trie);
        let acked = Arc::clone(&acked);
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            let mut i = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let term = format!("w{w}-{i:08}");
                // `insert_with_value` is now `&self` (collapsed handle) — overlay CAS.
                let ok = MappedDictionary::get_value(&trie, &term).is_some() || {
                    use libdictenstein::MutableMappedDictionary;
                    MutableMappedDictionary::insert_with_value(&trie, &term, i)
                };
                if ok {
                    acked.lock().expect("acked").insert(term);
                }
                i += 1;
                // A light yield so the scheduler interleaves checkpoint/eviction.
                if i % 64 == 0 {
                    std::thread::yield_now();
                }
            }
            i
        }));
    }

    // ── Reader threads (lock-free overlay reads — take NO lock) ──
    for _ in 0..n_readers {
        let trie = Arc::clone(&trie);
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            let mut reads = 0u64;
            while !stop.load(Ordering::Relaxed) {
                // A handful of point reads + a contains; never excluded by writers.
                let _ = MappedDictionary::get_value(&trie, "w0-00000000");
                let _ = trie.read().contains("w1-00000001");
                reads += 1;
                if reads % 128 == 0 {
                    std::thread::yield_now();
                }
            }
            reads
        }));
    }

    // ── Checkpointer thread (takes CK; owned-arm OR-read capture — but we are
    //    overlay-routed, so it captures the immutable overlay by the watermark) ──
    {
        let trie = Arc::clone(&trie);
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            let mut ckpts = 0u64;
            while !stop.load(Ordering::Relaxed) {
                trie.checkpoint().expect("soak checkpoint must not error");
                ckpts += 1;
                std::thread::sleep(Duration::from_millis(5));
            }
            ckpts
        }));
    }

    // ── Eviction churner: enable → force → disable, repeatedly. This is the
    //    deadlock-prone combo (EC ‖ OR ‖ CK ‖ the worker join) the loom proves
    //    acyclic; here we hammer it for real, concurrently with all of the above. ──
    {
        let trie = Arc::clone(&trie);
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            let mut cycles = 0u64;
            while !stop.load(Ordering::Relaxed) {
                // enable (installs the coordinator + worker)
                if trie
                    .enable_eviction(EvictionConfig::without_memory_monitor())
                    .is_ok()
                {
                    // force one synchronous reclaim pass (callback takes OR>EC)
                    let _ = trie.force_eviction(1 << 16);
                    // disable: the drop-before-join discipline (EC released before join)
                    trie.disable_eviction()
                        .expect("disable must not error/hang");
                    cycles += 1;
                }
                std::thread::sleep(Duration::from_millis(7));
            }
            cycles
        }));
    }

    // Run the budget, then signal stop and join EVERYTHING. If any thread is
    // deadlocked, this join blocks forever and the caller's `timeout` kills us.
    std::thread::sleep(budget);
    stop.store(true, Ordering::Release);
    for h in handles {
        let _ = h.join().expect("a soak thread panicked");
    }

    // Ensure eviction is fully off before the final audit (idempotent).
    trie.disable_eviction().expect("final disable");

    // ── NO-LOST-WRITE AUDIT ──
    // Every acknowledged term must still be readable (acknowledged overlay writes
    // are durable-by-watermark and never excluded by checkpoint/eviction/disable).
    let acked = acked.lock().expect("acked").clone();
    assert!(
        !acked.is_empty(),
        "soak made no progress — no writes were acknowledged (suspect a stall)"
    );
    let mut missing = Vec::new();
    for term in &acked {
        if MappedDictionary::get_value(&trie, term).is_none() {
            missing.push(term.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "LOST WRITE: {} of {} acknowledged terms not readable after the soak \
         (first few: {:?})",
        missing.len(),
        acked.len(),
        missing.iter().take(10).collect::<Vec<_>>()
    );

    // ── SURVIVES REOPEN (durability across restart) ──
    // Drop the handle (joins background threads via Drop), reopen, re-audit.
    drop(trie);
    let reopened: SharedCharARTrie<u64> = ARTrie::open(&path).expect("reopen soak trie");
    let mut missing_after_reopen = Vec::new();
    for term in &acked {
        if MappedDictionary::get_value(&reopened, term).is_none() {
            missing_after_reopen.push(term.clone());
        }
    }
    assert!(
        missing_after_reopen.is_empty(),
        "LOST WRITE ACROSS REOPEN: {} of {} acknowledged terms missing after restart",
        missing_after_reopen.len(),
        acked.len()
    );

    // (Wall-clock deadlock-freedom is enforced by the external `timeout` wrapper.)

    // Best-effort cleanup.
    let _ = std::fs::remove_file(&path);
}
