//! Raw-ABI snapshot laws for producer resources: query-start capture,
//! immutability under concurrent-looking CRUD, and the snapshot-of-snapshot
//! contract. Extends the PATTERN of
//! `tests/query_start_snapshot_correspondence.rs` (which pins the Rust
//! iterator surface) onto the `vt.dictionary.v1` C ABI without duplicating
//! its cases.
//!
//! Spec: `formal-verification/tla+/AbiProducerSnapshot.tla` (plan
//! obligation #10; TLC-checked with the `_Unsafe.cfg` negative control) and
//! the arena functional spec `formal-verification/rocq/Spec/
//! AbiTraversalSnapshotSpec.v` (obligation #11); this file is the
//! snapshot-law correspondence anchor for those IDs.
//!
//! INVARIANT-HOOK: LDICT-SNAP-1 (`CapturedRevisionImmutable`) — a snapshot
//! resource captured before a mutation batch replays byte-identically
//! afterwards (same node ids, edges, finality, and values under a full
//! structural walk), even across `clear`.
//! INVARIANT-HOOK: LDICT-SNAP-2 (`FreshCaptureSeesHead`) — a snapshot
//! captured after the batch observes exactly the mutated contents.
//! INVARIANT-HOOK: LDICT-SNAP-3 (`ContentPreservingPublishes`) — `compact`
//! publishes preserve captured AND fresh contents; likewise capturing FROM
//! an IMMUTABLE snapshot yields a NEW resource context (not a self-retain
//! of the same words) that shares the same traversal arena — node
//! identifiers coincide between parent and child snapshots — and
//! advertises IMMUTABLE.
//!
//! Pinned realization (from `src/bindings.rs`): `ResourceContext::snapshot`
//! on a `Snapshot` payload is `Arc::clone` of the same `TraversalSnapshot`,
//! wrapped in a fresh `ResourceContext`; hence "new context words, shared
//! node-id space".

#![cfg(feature = "ffi")]

mod ffi_common;

use std::collections::BTreeMap;

use ffi_common::{
    all_edges, capture_snapshot, dictionary_interface, insert_text, node_is_final, node_value,
    remove_text, snapshot_identity, snapshot_len, snapshot_root, unicode_labels, walk_terms,
    DictGuard, DOMAIN_BYTE, DOMAIN_U64, DOMAIN_UNICODE,
};
use libdictenstein::ffi::{ldict_dictionary_clear, LdictStatus};
use proptest::prelude::*;
use vinary_tree_interop::{dictionary_flags, VtResource};

/// Full structural walk: DFS-ordered `(node, is_final, value, edges)` rows.
/// Two walks of the same immutable snapshot must be identical, row for row.
type Structure = Vec<(u64, bool, Option<u64>, Vec<(u64, u64)>)>;

fn walk_structure(snapshot: VtResource, capacity: usize) -> Structure {
    let vtable = dictionary_interface(snapshot);
    let root = snapshot_root(snapshot);
    let mut rows = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let (status, is_final) = node_is_final(vtable, snapshot, node);
        assert_eq!(status, vinary_tree_interop::VtStatus::Ok);
        let (status, value) = node_value(vtable, snapshot, node);
        assert_eq!(status, vinary_tree_interop::VtStatus::Ok);
        let edges = all_edges(vtable, snapshot, node, capacity);
        let pairs: Vec<(u64, u64)> = edges.iter().map(|edge| (edge.label, edge.node)).collect();
        for edge in edges.iter().rev() {
            stack.push(edge.node);
        }
        rows.push((node, is_final, value, pairs));
    }
    rows
}

#[test]
fn snapshot_identity_reuses_a_revision_and_advances_after_mutation() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    assert_eq!(
        insert_text(dictionary.ptr(), b"alpha", Some(1)).0,
        LdictStatus::Ok
    );

    let first = capture_snapshot(dictionary.resource());
    let second = capture_snapshot(dictionary.resource());
    let first_identity = snapshot_identity(first.resource);
    assert_eq!(snapshot_identity(second.resource), first_identity);
    assert_ne!(first.resource.context, second.resource.context);

    let child = capture_snapshot(first.resource);
    assert_eq!(snapshot_identity(child.resource), first_identity);

    assert_eq!(
        insert_text(dictionary.ptr(), b"beta", Some(2)).0,
        LdictStatus::Ok
    );
    let next = capture_snapshot(dictionary.resource());
    let next_identity = snapshot_identity(next.resource);
    assert_eq!(next_identity.producer, first_identity.producer);
    assert!(next_identity.revision > first_identity.revision);
    assert_eq!(snapshot_identity(first.resource), first_identity);
}

/// One mutation in the post-capture batch. `Compact` is the
/// content-preserving publish from LDICT-SNAP-3: it swaps the published
/// revision without changing the term set.
#[derive(Clone, Debug)]
enum Mutation {
    Insert(String, Option<u64>),
    Remove(String),
    Clear,
    Compact,
}

fn mutation_strategy() -> impl Strategy<Value = Mutation> {
    prop_oneof![
        4 => ("[a-d]{1,6}", prop::option::of(any::<u64>()))
            .prop_map(|(term, value)| Mutation::Insert(term, value)),
        3 => "[a-d]{1,6}".prop_map(Mutation::Remove),
        1 => Just(Mutation::Clear),
        1 => Just(Mutation::Compact),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// LDICT-SNAP-1 + LDICT-SNAP-2 under arbitrary mutation batches.
    #[test]
    fn captured_revisions_are_immune_to_mutation_batches(
        seed in prop::collection::btree_map("[a-d]{1,6}", prop::option::of(any::<u64>()), 1..24),
        mutations in prop::collection::vec(mutation_strategy(), 1..32),
    ) {
        let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
        for (term, value) in &seed {
            let (status, inserted) = insert_text(dictionary.ptr(), term.as_bytes(), *value);
            prop_assert_eq!(status, LdictStatus::Ok);
            prop_assert!(inserted);
        }

        // Capture BEFORE the batch; record the full structural walk.
        let captured = capture_snapshot(dictionary.resource());
        let before = walk_structure(captured.resource, 8);
        let (len_before, known) = snapshot_len(captured.resource);
        prop_assert!(known);
        prop_assert_eq!(len_before, seed.len());

        // Apply the mutation batch through the C ABI.
        let mut model: BTreeMap<String, Option<u64>> = seed.clone();
        for mutation in mutations {
            match mutation {
                Mutation::Insert(term, value) => {
                    let (status, _) = insert_text(dictionary.ptr(), term.as_bytes(), value);
                    prop_assert_eq!(status, LdictStatus::Ok);
                    model.insert(term, value);
                }
                Mutation::Remove(term) => {
                    let (status, _) = remove_text(dictionary.ptr(), term.as_bytes());
                    prop_assert_eq!(status, LdictStatus::Ok);
                    model.remove(&term);
                }
                Mutation::Clear => {
                    let status = unsafe { ldict_dictionary_clear(dictionary.ptr()) };
                    prop_assert_eq!(status, LdictStatus::Ok);
                    model.clear();
                }
                Mutation::Compact => {
                    // LDICT-SNAP-3: a content-preserving publish.
                    let mut reclaimed = usize::MAX;
                    let status = unsafe {
                        libdictenstein::ffi::ldict_dictionary_compact(
                            dictionary.ptr(),
                            &mut reclaimed,
                        )
                    };
                    prop_assert_eq!(status, LdictStatus::Ok);
                }
            }
        }

        // LDICT-SNAP-1: the captured revision is byte-identical — same node
        // ids, same edges in the same order, same finality and values.
        let after = walk_structure(captured.resource, 8);
        prop_assert_eq!(&after, &before, "captured snapshot changed under mutation");
        let (len_after, _) = snapshot_len(captured.resource);
        prop_assert_eq!(len_after, len_before);
        let expected_seed: BTreeMap<Vec<u64>, Option<u64>> = seed
            .iter()
            .map(|(term, value)| (unicode_labels(term), *value))
            .collect();
        prop_assert_eq!(walk_terms(captured.resource, 8), expected_seed);

        // LDICT-SNAP-2: a fresh snapshot observes exactly the mutated model.
        let fresh = capture_snapshot(dictionary.resource());
        let expected_model: BTreeMap<Vec<u64>, Option<u64>> = model
            .iter()
            .map(|(term, value)| (unicode_labels(term), *value))
            .collect();
        prop_assert_eq!(walk_terms(fresh.resource, 8), expected_model);
        let (fresh_len, fresh_known) = snapshot_len(fresh.resource);
        prop_assert!(fresh_known);
        prop_assert_eq!(fresh_len, model.len());
    }

    /// LDICT-SNAP-3 under arbitrary contents: snapshot-of-snapshot is a new
    /// context sharing the parent's arena and node-id space.
    #[test]
    fn snapshot_of_snapshot_shares_the_arena_under_a_new_context(
        seed in prop::collection::btree_map("[a-d]{1,5}", prop::option::of(any::<u64>()), 1..16),
    ) {
        let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
        for (term, value) in &seed {
            prop_assert_eq!(insert_text(dictionary.ptr(), term.as_bytes(), *value).0, LdictStatus::Ok);
        }
        let parent = capture_snapshot(dictionary.resource());
        let child = capture_snapshot(parent.resource);

        // New context words, not a self-retain.
        prop_assert!(
            !std::ptr::eq(parent.resource.context, child.resource.context),
            "snapshot-of-snapshot must mint a fresh resource context"
        );

        // Both advertise IMMUTABLE (and stay PARALLEL_REENTRANT).
        for resource in [parent.resource, child.resource] {
            let vtable = dictionary_interface(resource);
            prop_assert_ne!(vtable.flags & dictionary_flags::IMMUTABLE, 0);
            prop_assert_ne!(vtable.flags & dictionary_flags::PARALLEL_REENTRANT, 0);
        }

        // Shared arena: materialize ids through the CHILD, observe through
        // the PARENT, and vice versa — identical (label, id) rows.
        let child_structure = walk_structure(child.resource, 4);
        let parent_structure = walk_structure(parent.resource, 4);
        prop_assert_eq!(&child_structure, &parent_structure, "parent and child disagree");

        // The parent's contents survive dropping the child (independent
        // retains on the shared arena).
        drop(child);
        let expected: BTreeMap<Vec<u64>, Option<u64>> = seed
            .iter()
            .map(|(term, value)| (unicode_labels(term), *value))
            .collect();
        prop_assert_eq!(walk_terms(parent.resource, 4), expected);
    }
}

/// Deterministic LDICT-SNAP-1 example crossing `clear`: the strongest
/// revision swap the producer can publish.
#[test]
fn a_captured_snapshot_survives_clear() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    for (term, value) in [("cat", Some(1)), ("cot", None), ("cut", Some(3))] {
        assert_eq!(
            insert_text(dictionary.ptr(), term.as_bytes(), value).0,
            LdictStatus::Ok
        );
    }
    let captured = capture_snapshot(dictionary.resource());
    let before = walk_structure(captured.resource, 8);

    assert_eq!(
        unsafe { ldict_dictionary_clear(dictionary.ptr()) },
        LdictStatus::Ok
    );

    assert_eq!(walk_structure(captured.resource, 8), before);
    let (len, known) = snapshot_len(captured.resource);
    assert_eq!((len, known), (3, true));

    let fresh = capture_snapshot(dictionary.resource());
    assert_eq!(walk_terms(fresh.resource, 8), BTreeMap::new());
    let (len, known) = snapshot_len(fresh.resource);
    assert_eq!((len, known), (0, true));
}

/// Snapshot resources outlive the `LdictDictionary` handle that produced
/// them (the handle's drop releases only the handle's own retain).
#[test]
fn a_captured_snapshot_outlives_the_freed_handle() {
    let dictionary = DictGuard::dynamic(DOMAIN_BYTE);
    for term in [&[0x00u8, 0x61][..], &[0xFF][..]] {
        assert_eq!(
            insert_text(dictionary.ptr(), term, Some(9)).0,
            LdictStatus::Ok
        );
    }
    let captured = capture_snapshot(dictionary.resource());
    let before = walk_structure(captured.resource, 8);
    drop(dictionary); // ldict_dictionary_free
    assert_eq!(walk_structure(captured.resource, 8), before);
}

/// Flag truth table across live and snapshot vtables for every in-memory
/// backend family (IMMUTABLE appears exactly on snapshots; SUFFIX_BASED
/// exactly on SCDAWGs; PARALLEL_REENTRANT everywhere).
#[test]
fn immutable_and_suffix_flags_are_advertised_exactly_where_they_belong() {
    let cases: [(&str, DictGuard, bool); 6] = [
        ("dynamic/byte", DictGuard::dynamic(DOMAIN_BYTE), false),
        ("dynamic/unicode", DictGuard::dynamic(DOMAIN_UNICODE), false),
        ("dynamic/u64", DictGuard::dynamic(DOMAIN_U64), false),
        ("scdawg/byte", DictGuard::scdawg(DOMAIN_BYTE), true),
        ("scdawg/unicode", DictGuard::scdawg(DOMAIN_UNICODE), true),
        ("dynamic/byte-2", DictGuard::dynamic(DOMAIN_BYTE), false),
    ];
    for (name, dictionary, suffix) in &cases {
        let live = dictionary_interface(dictionary.resource());
        assert_eq!(
            live.flags & dictionary_flags::IMMUTABLE,
            0,
            "{name}: live resources are not IMMUTABLE"
        );
        assert_ne!(
            live.flags & dictionary_flags::PARALLEL_REENTRANT,
            0,
            "{name}"
        );
        assert_eq!(
            live.flags & dictionary_flags::SUFFIX_BASED != 0,
            *suffix,
            "{name}: SUFFIX_BASED truth"
        );

        let snapshot = capture_snapshot(dictionary.resource());
        let vtable = dictionary_interface(snapshot.resource);
        assert_ne!(
            vtable.flags & dictionary_flags::IMMUTABLE,
            0,
            "{name}: snapshot IMMUTABLE"
        );
        assert_ne!(
            vtable.flags & dictionary_flags::PARALLEL_REENTRANT,
            0,
            "{name}"
        );
        assert_eq!(
            vtable.flags & dictionary_flags::SUFFIX_BASED != 0,
            *suffix,
            "{name}: snapshot SUFFIX_BASED truth"
        );
    }
}
