//! Paging laws for `node_edges`/`node_transition` through the real
//! `vt.dictionary.v1` resource ABI (`ldict_dictionary_resource` ->
//! `query_interface` -> `snapshot` -> node walks).
//!
//! Spec: the producer paging refinement is proved in
//! `formal-verification/rocq/Spec/AbiPagingProducerSpec.v` (plan obligation
//! #12); this file is the correspondence anchor for the LDICT-PAGE-* rows
//! of libdictenstein's invariant registry.
//!
//! INVARIANT-HOOK: LDICT-PAGE-1 — paging laws: `out_total` is the exact edge
//! count and is stable across pages; a `capacity == 0` probe writes zero
//! edges (a null `out_edges` is legal there); pages of any capacity
//! concatenate losslessly, in order, to the single full listing; `start`
//! beyond the total yields an empty page with the same total.
//! INVARIANT-HOOK: LDICT-PAGE-2 — traversal agreement: every edge listed by
//! `node_edges` is confirmed by `node_transition` with the same child id;
//! absent labels (including labels above `char::MAX` in the Unicode-scalar
//! domain) report `Ok` with `found == 0`; node finality and values agree
//! with a `BTreeMap` oracle over the inserted terms.
//!
//! Pinned deviation from the planning summary: `node_transition` never
//! returns `InvalidArgument` for out-of-domain labels. The producer's arena
//! matches labels structurally (`edge.label == label`), so an oversized
//! Unicode label is simply "not found" (`Ok`, `found == 0`). Only invalid
//! NODE identifiers produce `InvalidArgument`.

#![cfg(feature = "ffi")]

mod ffi_common;

use std::collections::BTreeMap;

use ffi_common::{
    all_edges, capture_snapshot, dictionary_interface, edges_page, insert_text, insert_u64,
    node_is_final, node_value, snapshot_len, snapshot_root, transition, unicode_labels, vt_status,
    DictGuard, DOMAIN_U64, DOMAIN_UNICODE,
};
use libdictenstein::ffi::LdictStatus;
use proptest::prelude::*;
use vinary_tree_interop::{VtStatus, VT_RECOMMENDED_EDGE_BATCH};

/// Root degrees that straddle the recommended edge batch, plus a uniform
/// sweep of 0..600.
fn degree_strategy() -> impl Strategy<Value = usize> {
    prop_oneof![
        4 => 0usize..600,
        1 => Just(VT_RECOMMENDED_EDGE_BATCH - 1),
        1 => Just(VT_RECOMMENDED_EDGE_BATCH),
        1 => Just(VT_RECOMMENDED_EDGE_BATCH + 1),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// LDICT-PAGE-1 over a root node whose degree is drawn across the
    /// recommended batch size (u64 domain: one single-token term per label).
    #[test]
    fn root_paging_laws_hold_for_degrees_across_the_recommended_batch(
        degree in degree_strategy(),
        capacity in 1usize..64,
    ) {
        let dictionary = DictGuard::dynamic(DOMAIN_U64);
        for label in 0..degree as u64 {
            let (status, inserted) = insert_u64(dictionary.ptr(), &[label], Some(label * 3));
            prop_assert_eq!(status, LdictStatus::Ok);
            prop_assert!(inserted);
        }
        let snapshot = capture_snapshot(dictionary.resource());
        let vtable = dictionary_interface(snapshot.resource);
        let root = snapshot_root(snapshot.resource);
        prop_assert_eq!(root, 0, "producer snapshots root at ABI-local id 0");

        // Capacity-0 probe: written == 0, total == degree, null out_edges legal.
        let (status, page, written, total) = edges_page(vtable, snapshot.resource, root, 0, 0);
        prop_assert_eq!(status, VtStatus::Ok);
        prop_assert_eq!((page.len(), written, total), (0, 0, degree));

        // Full listing in one call with capacity == total (or 1 when empty).
        let full_capacity = degree.max(1);
        let (status, full, written, total) =
            edges_page(vtable, snapshot.resource, root, 0, full_capacity);
        prop_assert_eq!(status, VtStatus::Ok);
        prop_assert_eq!(written, degree);
        prop_assert_eq!(total, degree);

        // Overshoot capacity writes exactly the total.
        let (status, overshoot, written, _) =
            edges_page(vtable, snapshot.resource, root, 0, degree + 7);
        prop_assert_eq!(status, VtStatus::Ok);
        prop_assert_eq!(written, degree);
        prop_assert_eq!(&overshoot, &full, "overshoot listing must equal the exact listing");

        // start > total: empty page, stable total.
        for start in [degree + 1, degree + 500, usize::MAX] {
            let (status, page, written, total) =
                edges_page(vtable, snapshot.resource, root, start, capacity);
            prop_assert_eq!(status, VtStatus::Ok);
            prop_assert_eq!((page.len(), written, total), (0, 0, degree));
        }

        // Lossless in-order concatenation at the drawn capacity, with
        // out_total stability asserted on every page inside all_edges.
        let paged = all_edges(vtable, snapshot.resource, root, capacity);
        prop_assert_eq!(&paged, &full, "paged concatenation must equal the full listing");

        // Capacity-1 pages: the strictest concatenation.
        let single_stepped = all_edges(vtable, snapshot.resource, root, 1);
        prop_assert_eq!(&single_stepped, &full);

        // The listing is exactly the inserted label set.
        let labels: Vec<u64> = {
            let mut labels: Vec<u64> = full.iter().map(|edge| edge.label).collect();
            labels.sort_unstable();
            labels
        };
        let expected: Vec<u64> = (0..degree as u64).collect();
        prop_assert_eq!(labels, expected);

        // LDICT-PAGE-2: every listed edge transitions to the same child; the
        // first absent labels report found == 0 without an error status.
        for edge in &full {
            let (status, child) = transition(vtable, snapshot.resource, root, edge.label);
            prop_assert_eq!(status, VtStatus::Ok);
            prop_assert_eq!(child, Some(edge.node), "transition disagrees with node_edges");
        }
        for absent in degree as u64..degree as u64 + 3 {
            let (status, child) = transition(vtable, snapshot.resource, root, absent);
            prop_assert_eq!(status, VtStatus::Ok);
            prop_assert_eq!(child, None);
        }
    }

    /// LDICT-PAGE-2 finality/value agreement against a BTreeMap oracle over
    /// multi-token terms (values on some, valueless on others).
    #[test]
    fn node_finality_and_values_agree_with_the_oracle(
        entries in prop::collection::btree_map(
            prop::collection::vec(any::<u64>(), 1..4),
            prop::option::of(any::<u64>()),
            1..40,
        ),
        capacity in 1usize..8,
    ) {
        let dictionary = DictGuard::dynamic(DOMAIN_U64);
        for (term, value) in &entries {
            let (status, inserted) = insert_u64(dictionary.ptr(), term, *value);
            prop_assert_eq!(status, LdictStatus::Ok);
            prop_assert!(inserted);
        }
        let snapshot = capture_snapshot(dictionary.resource());
        let vtable = dictionary_interface(snapshot.resource);
        let (len, known) = snapshot_len(snapshot.resource);
        prop_assert!(known, "DynamicDAWG snapshots know their length");
        prop_assert_eq!(len, entries.len());

        // Walk every node by its label path; compare finality and value.
        let mut stack = vec![(snapshot_root(snapshot.resource), Vec::<u64>::new())];
        let mut observed = BTreeMap::new();
        while let Some((node, path)) = stack.pop() {
            let (status, is_final) = node_is_final(vtable, snapshot.resource, node);
            prop_assert_eq!(status, VtStatus::Ok);
            prop_assert_eq!(
                is_final,
                entries.contains_key(&path),
                "finality mismatch at {:?}", path
            );
            let (status, value) = node_value(vtable, snapshot.resource, node);
            prop_assert_eq!(status, VtStatus::Ok);
            match entries.get(&path) {
                Some(expected) => {
                    if is_final {
                        prop_assert_eq!(value, *expected, "value mismatch at {:?}", path);
                        observed.insert(path.clone(), value);
                    }
                }
                None => prop_assert_eq!(value, None, "non-final node carries a value at {:?}", path),
            }
            for edge in all_edges(vtable, snapshot.resource, node, capacity) {
                let mut child_path = Vec::with_capacity(path.len() + 1);
                child_path.extend_from_slice(&path);
                child_path.push(edge.label);
                stack.push((edge.node, child_path));
            }
        }
        prop_assert_eq!(observed, entries, "walked term set must equal the oracle");
    }
}

/// Boundary example: degrees exactly at 255/256/257 page correctly at the
/// recommended capacity.
#[test]
fn recommended_batch_boundary_degrees_page_losslessly() {
    for degree in [
        VT_RECOMMENDED_EDGE_BATCH - 1,
        VT_RECOMMENDED_EDGE_BATCH,
        VT_RECOMMENDED_EDGE_BATCH + 1,
    ] {
        let dictionary = DictGuard::dynamic(DOMAIN_U64);
        for label in 0..degree as u64 {
            assert_eq!(
                insert_u64(dictionary.ptr(), &[label], None).0,
                LdictStatus::Ok
            );
        }
        let snapshot = capture_snapshot(dictionary.resource());
        let vtable = dictionary_interface(snapshot.resource);
        let root = snapshot_root(snapshot.resource);

        let (status, first_page, written, total) = edges_page(
            vtable,
            snapshot.resource,
            root,
            0,
            VT_RECOMMENDED_EDGE_BATCH,
        );
        assert_eq!(status, VtStatus::Ok);
        assert_eq!(total, degree);
        assert_eq!(written, degree.min(VT_RECOMMENDED_EDGE_BATCH));

        let paged = all_edges(vtable, snapshot.resource, root, VT_RECOMMENDED_EDGE_BATCH);
        assert_eq!(paged.len(), degree);
        assert_eq!(&paged[..first_page.len()], &first_page[..]);
    }
}

/// LDICT-PAGE-2 in the Unicode-scalar domain, including the oversized-label
/// pin (see the file header for the deviation note).
#[test]
fn unicode_domain_labels_transition_and_oversized_labels_are_not_found() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    for (term, value) in [("aé", 1u64), ("a🦀", 2), ("z", 3)] {
        let (status, inserted) = insert_text(dictionary.ptr(), term.as_bytes(), Some(value));
        assert_eq!(status, LdictStatus::Ok);
        assert!(inserted);
    }
    let snapshot = capture_snapshot(dictionary.resource());
    let vtable = dictionary_interface(snapshot.resource);
    let root = snapshot_root(snapshot.resource);

    // Labels are Unicode scalar values widened to u64.
    let root_edges = all_edges(vtable, snapshot.resource, root, 4);
    let mut labels: Vec<u64> = root_edges.iter().map(|edge| edge.label).collect();
    labels.sort_unstable();
    assert_eq!(
        labels,
        vec![u64::from(u32::from('a')), u64::from(u32::from('z'))]
    );

    // Walk "a" -> 'é' and "a" -> '🦀'.
    let (status, a_node) = transition(vtable, snapshot.resource, root, u64::from(u32::from('a')));
    assert_eq!(status, VtStatus::Ok);
    let a_node = a_node.expect("edge 'a' exists");
    for (label, expected_value) in [('é', 1u64), ('🦀', 2)] {
        let (status, child) = transition(
            vtable,
            snapshot.resource,
            a_node,
            u64::from(u32::from(label)),
        );
        assert_eq!(status, VtStatus::Ok);
        let child = child.expect("child edge exists");
        let (status, is_final) = node_is_final(vtable, snapshot.resource, child);
        assert_eq!((status, is_final), (VtStatus::Ok, true));
        let (status, value) = node_value(vtable, snapshot.resource, child);
        assert_eq!((status, value), (VtStatus::Ok, Some(expected_value)));
    }

    // Oversized labels: above char::MAX and above u32: Ok + found == 0,
    // never InvalidArgument (structural label matching).
    for oversized in [
        u64::from(u32::from(char::MAX)) + 1,
        u64::from(u32::MAX),
        u64::MAX,
    ] {
        let (status, child) = transition(vtable, snapshot.resource, root, oversized);
        assert_eq!(status, VtStatus::Ok, "oversized label {oversized}");
        assert_eq!(child, None, "oversized label {oversized} must not resolve");
    }

    // The full ABI walk equals the inserted term set.
    let walked = ffi_common::walk_terms(snapshot.resource, 3);
    let expected: BTreeMap<Vec<u64>, Option<u64>> = [
        (unicode_labels("aé"), Some(1)),
        (unicode_labels("a🦀"), Some(2)),
        (unicode_labels("z"), Some(3)),
    ]
    .into_iter()
    .collect();
    assert_eq!(walked, expected);
}

/// Invalid NODE identifiers (as opposed to labels) are `InvalidArgument`,
/// and failed traversal calls leave their out-params untouched.
#[test]
fn invalid_node_identifiers_are_invalid_arguments_with_untouched_outputs() {
    let dictionary = DictGuard::dynamic(DOMAIN_U64);
    assert_eq!(insert_u64(dictionary.ptr(), &[1], None).0, LdictStatus::Ok);
    let snapshot = capture_snapshot(dictionary.resource());
    let vtable = dictionary_interface(snapshot.resource);

    let bogus_node = 999_999u64;
    let (status, _, written, total) = edges_page(vtable, snapshot.resource, bogus_node, 0, 4);
    assert_eq!(status, VtStatus::InvalidArgument);
    assert_eq!(
        (written, total),
        (usize::MAX, usize::MAX),
        "failed node_edges must not write its out-params"
    );

    let (status, child) = transition(vtable, snapshot.resource, bogus_node, 1);
    assert_eq!((status, child), (VtStatus::InvalidArgument, None));
    let (status, _) = node_is_final(vtable, snapshot.resource, bogus_node);
    assert_eq!(status, VtStatus::InvalidArgument);
    let (status, value) = node_value(vtable, snapshot.resource, bogus_node);
    assert_eq!((status, value), (VtStatus::InvalidArgument, None));
}

/// Traversal entry points demand an immutable snapshot: they reject the live
/// resource itself with `InvalidArgument`.
#[test]
fn live_resources_reject_traversal_until_a_snapshot_is_captured() {
    let dictionary = DictGuard::dynamic(DOMAIN_U64);
    assert_eq!(insert_u64(dictionary.ptr(), &[1], None).0, LdictStatus::Ok);
    let live = dictionary.resource();
    let vtable = dictionary_interface(live);

    let mut root = u64::MAX;
    let status =
        vt_status(unsafe { (vtable.root.expect("root published"))(live.context, &mut root) });
    assert_eq!(status, VtStatus::InvalidArgument);
    assert_eq!(root, u64::MAX, "failed root must not write");

    let (mut len, mut known) = (usize::MAX, u8::MAX);
    let status = vt_status(unsafe {
        (vtable.len.expect("len published"))(live.context, &mut len, &mut known)
    });
    assert_eq!(status, VtStatus::InvalidArgument);

    let (status, _, written, total) = edges_page(vtable, live, 0, 0, 4);
    assert_eq!(status, VtStatus::InvalidArgument);
    assert_eq!((written, total), (usize::MAX, usize::MAX));

    // The snapshot captured FROM the live resource traverses normally.
    let snapshot = capture_snapshot(live);
    assert_eq!(snapshot_root(snapshot.resource), 0);
}
