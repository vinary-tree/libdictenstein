//! Concurrency stress over one DynamicDAWG resource: snapshot capture, node
//! walks, CRUD mutation, and retain/release cycling from 4-8 threads, with
//! correctness assertions only (throughput is a benchmark concern).
//!
//! Spec: the capture-vs-CRUD protocol is modelled in
//! `formal-verification/tla+/AbiProducerSnapshot.tla` (plan obligation #10;
//! TLC-checked with the `_Unsafe.cfg` negative control) and the refcount
//! realization contract lives in the unsafe inventory rows plus this file
//! (obligation #14).
//!
//! INVARIANT-HOOK: LDICT-LIFE-1 — retain/release balance: interleaved
//! vtable retains and releases from many threads leave the resource with
//! exactly its owned retains; after every extra retain is released, the
//! final single-owner drop tears the context down without a crash, and a
//! handle freed while consumers hold retains does not invalidate them.
//! (The producer realizes the counter with `Arc::increment_strong_count` /
//! `Arc::decrement_strong_count`; no `Weak` observer crosses the ABI, so
//! balance is asserted behaviourally here and numerically in the
//! `src/bindings.rs` unit tests, which can reach the internal `Arc`.)
//! INVARIANT-HOOK: LDICT-LIFE-2 — snapshots captured concurrently with CRUD
//! are internally consistent (stable totals, agreeing transitions, walkable
//! term sets) and keep working after the producing handle is freed.

#![cfg(feature = "ffi")]

mod ffi_common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use ffi_common::{
    all_edges, capture_snapshot, dictionary_interface, insert_text, remove_text, snapshot_len,
    snapshot_root, transition, unicode_labels, vt_release, vt_retain, walk_terms, DictGuard,
    DOMAIN_UNICODE,
};
use libdictenstein::ffi::{LdictDictionary, LdictStatus};
use vinary_tree_interop::{VtResource, VtStatus};

/// Threads per role (8 threads total, within the mandated 4-8 band).
const THREADS_PER_ROLE: usize = 2;
/// Bounded iterations per role thread.
const MUTATION_ITERATIONS: usize = 200;
const SNAPSHOT_ITERATIONS: usize = 60;
const WALK_ITERATIONS: usize = 40;
const RETAIN_ITERATIONS: usize = 400;

/// Raw handle wrapper for crossing threads.
///
/// SAFETY: the DynamicDAWG binding is internally synchronized (RwLock over
/// atomically published revisions), the producer vtables advertise
/// PARALLEL_REENTRANT, and every test joins its threads before dropping the
/// guard that owns the handle.
#[derive(Clone, Copy)]
struct SharedDict(*mut LdictDictionary);
unsafe impl Send for SharedDict {}
unsafe impl Sync for SharedDict {}

/// Two-word resource wrapper for crossing threads (copying the words is not
/// a retain; threads that store the resource retain it first).
#[derive(Clone, Copy)]
struct SharedResource(VtResource);
unsafe impl Send for SharedResource {}
unsafe impl Sync for SharedResource {}

/// Walk a snapshot and assert internal consistency; returns the term count.
fn assert_snapshot_consistent(snapshot: VtResource) -> usize {
    let vtable = dictionary_interface(snapshot);
    let root = snapshot_root(snapshot);
    let (len, known) = snapshot_len(snapshot);
    assert!(known, "DynamicDAWG snapshots know their length");

    // Every listed edge must transition to the same child, and the edge
    // total must be stable between a probe and an enumeration.
    let edges = all_edges(vtable, snapshot, root, 7);
    for edge in &edges {
        let (status, child) = transition(vtable, snapshot, root, edge.label);
        assert_eq!(status, VtStatus::Ok);
        assert_eq!(child, Some(edge.node), "transition/edges disagreement");
    }

    let walked = walk_terms(snapshot, 7);
    assert_eq!(
        walked.len(),
        len,
        "walked term count must equal the snapshot len"
    );
    len
}

#[test]
fn concurrent_snapshot_walk_mutate_and_retain_release_stay_consistent() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    // Seed a stable core that mutators never touch.
    let mut seed = BTreeMap::new();
    for index in 0..32u64 {
        let term = format!("seed{index:03}");
        assert_eq!(
            insert_text(dictionary.ptr(), term.as_bytes(), Some(index)).0,
            LdictStatus::Ok
        );
        seed.insert(unicode_labels(&term), Some(index));
    }

    let resource = dictionary.resource();
    // A snapshot captured before any concurrent mutation: its view must
    // never change, no matter what the other threads do.
    let frozen = capture_snapshot(resource);
    let frozen_view = walk_terms(frozen.resource, 7);
    assert_eq!(frozen_view, seed);

    let shared_dict = SharedDict(dictionary.ptr());
    let shared_resource = SharedResource(resource);
    let completed_mutations = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        // Mutators: churn a disjoint namespace of terms through the C ABI.
        for worker in 0..THREADS_PER_ROLE {
            let dict = shared_dict;
            scope.spawn(move || {
                let dict = dict;
                for iteration in 0..MUTATION_ITERATIONS {
                    let term = format!("mut{worker}-{:03}", iteration % 64);
                    let (status, _) = insert_text(dict.0, term.as_bytes(), Some(iteration as u64));
                    assert_eq!(status, LdictStatus::Ok);
                    if iteration % 3 == 0 {
                        let victim = format!("mut{worker}-{:03}", (iteration / 2) % 64);
                        let (status, _) = remove_text(dict.0, victim.as_bytes());
                        assert_eq!(status, LdictStatus::Ok);
                    }
                }
            });
        }
        // Snapshotters: capture, verify internal consistency, release.
        for _ in 0..THREADS_PER_ROLE {
            let resource = shared_resource;
            let counter = &completed_mutations;
            scope.spawn(move || {
                let resource = resource;
                for _ in 0..SNAPSHOT_ITERATIONS {
                    let snapshot = capture_snapshot(resource.0);
                    let len = assert_snapshot_consistent(snapshot.resource);
                    assert!(len >= 32, "the seed core is never removed");
                    counter.fetch_add(1, Ordering::Relaxed);
                    // SnapshotGuard releases the capture's retain here.
                }
            });
        }
        // Walkers: re-walk the pre-mutation snapshot; it must never move.
        for _ in 0..THREADS_PER_ROLE {
            let resource = SharedResource(frozen.resource);
            let expected = &frozen_view;
            scope.spawn(move || {
                let resource = resource;
                // A stored copy of the two words is retained before use and
                // released after (copy-not-retain discipline).
                vt_retain(resource.0);
                for _ in 0..WALK_ITERATIONS {
                    assert_eq!(&walk_terms(resource.0, 5), expected);
                }
                vt_release(resource.0);
            });
        }
        // Retain/release cyclers: balanced churn on the live resource.
        for _ in 0..THREADS_PER_ROLE {
            let resource = shared_resource;
            scope.spawn(move || {
                let resource = resource;
                for _ in 0..RETAIN_ITERATIONS {
                    vt_retain(resource.0);
                    vt_retain(resource.0);
                    vt_release(resource.0);
                    vt_release(resource.0);
                }
            });
        }
    });

    assert_eq!(
        completed_mutations.load(Ordering::Relaxed),
        THREADS_PER_ROLE * SNAPSHOT_ITERATIONS
    );

    // The frozen snapshot still equals its capture-time view.
    assert_eq!(walk_terms(frozen.resource, 7), frozen_view);

    // The seed core survived all mutations.
    let final_snapshot = capture_snapshot(resource);
    let final_view = walk_terms(final_snapshot.resource, 7);
    for (term, value) in &seed {
        assert_eq!(final_view.get(term), Some(value), "seed term lost");
    }
}

/// LDICT-LIFE-1 free-order adversary: the handle is freed FIRST, while
/// snapshots and retained resource copies are still alive on other threads.
#[test]
fn retained_resources_survive_handle_free_across_threads() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    let mut expected = BTreeMap::new();
    for index in 0..24u64 {
        let term = format!("term{index:02}");
        assert_eq!(
            insert_text(dictionary.ptr(), term.as_bytes(), Some(index)).0,
            LdictStatus::Ok
        );
        expected.insert(unicode_labels(&term), Some(index));
    }

    let resource = dictionary.resource();
    // Store one extra owned retain of the live resource (copy + retain).
    vt_retain(resource);
    let stored = SharedResource(resource);
    let snapshot = capture_snapshot(resource);

    // Free the handle while the retains are outstanding.
    drop(dictionary);

    let snapshot_view = walk_terms(snapshot.resource, 7);
    assert_eq!(snapshot_view, expected, "snapshot survives handle free");

    std::thread::scope(|scope| {
        for _ in 0..4 {
            let snapshot = SharedResource(snapshot.resource);
            let expected = &snapshot_view;
            scope.spawn(move || {
                let stored = stored;
                let snapshot = snapshot;
                // The stored LIVE resource can still mint fresh snapshots
                // after the handle is gone...
                for _ in 0..20 {
                    let fresh = capture_snapshot(stored.0);
                    assert_eq!(&walk_terms(fresh.resource, 5), expected);
                }
                // ...and the pre-free snapshot stays valid throughout.
                for _ in 0..20 {
                    assert_eq!(&walk_terms(snapshot.0, 5), expected);
                }
            });
        }
    });

    // Balance: release the stored retain last; the SnapshotGuard drop then
    // performs the final release of the snapshot context. No further use of
    // either resource is legal past this point — reaching the end of the
    // test without a crash (and leak-free under the sanitizer CI leg) is the
    // observable for the final single-owner teardown.
    vt_release(stored.0);
    drop(snapshot);
}

/// Regression for finding LDICT-B4 (torn snapshot capture): the captured
/// root and the advertised snapshot `len` must come from ONE published
/// revision. Before the fix (`root_with_term_count`, one lock-free version
/// load), a writer racing the two separate loads in
/// `DynamicBackend::snapshot` tore ~2% of captures under this exact churn
/// (2163/100000 torn, 0/10000 quiescent, release mode, 2026-08-08); after
/// the fix the count is 0. INVARIANT-HOOK: LDICT-SNAP-1.
#[test]
fn snapshot_len_is_never_torn_from_its_root_under_write_churn() {
    use std::sync::atomic::AtomicBool;

    let dictionary = DictGuard::dynamic(ffi_common::DOMAIN_U64);
    let raw = SharedDict(dictionary.ptr());
    let resource = SharedResource(dictionary.resource());
    let stop = AtomicBool::new(false);

    std::thread::scope(|scope| {
        let stop = &stop;
        scope.spawn(move || {
            let raw = raw;
            // Bounded namespace: high version-CAS churn, small root degree.
            let mut label = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let (status, _) = ffi_common::insert_u64(raw.0, &[label % 64], None);
                assert_eq!(status, LdictStatus::Ok);
                let (status, _) = ffi_common::remove_u64(raw.0, &[(label + 32) % 64]);
                assert_eq!(status, LdictStatus::Ok);
                label += 1;
            }
        });
        scope.spawn(move || {
            let resource = resource;
            for _ in 0..12_000 {
                let snapshot = capture_snapshot(resource.0);
                let (len, known) = snapshot_len(snapshot.resource);
                assert!(known);
                // Count FINAL nodes: removals legitimately leave non-final
                // ghost edges until compaction, so root degree may exceed
                // len, but the walked FINAL count must equal the captured
                // len on every single capture.
                let walked = walk_terms(snapshot.resource, 128).len();
                assert_eq!(
                    walked, len,
                    "torn snapshot capture: walked {walked} final nodes but len says {len}"
                );
            }
            stop.store(true, Ordering::Relaxed);
        });
    });
}

/// Concurrent capture across threads mints independent snapshot contexts
/// (no accidental sharing between concurrently created captures of a LIVE
/// resource).
#[test]
fn concurrent_captures_are_independent_resources() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    for index in 0..8u64 {
        let term = format!("base{index}");
        assert_eq!(
            insert_text(dictionary.ptr(), term.as_bytes(), None).0,
            LdictStatus::Ok
        );
    }
    let resource = dictionary.resource();
    let shared = SharedResource(resource);

    let contexts: Vec<usize> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(move || {
                    let shared = shared;
                    let snapshot = capture_snapshot(shared.0);
                    let context = snapshot.resource.context as usize;
                    assert_snapshot_consistent(snapshot.resource);
                    context
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("capture thread"))
            .collect()
    });

    let mut unique = contexts.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        contexts.len(),
        "live captures must mint distinct snapshot contexts"
    );
}
