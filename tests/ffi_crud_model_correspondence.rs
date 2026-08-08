//! Model-based CRUD correspondence: operation scripts through the `ldict_*`
//! C ABI against a `BTreeMap` oracle, for DynamicDAWG in all three unit
//! domains and for the SCDAWG (insert-only plus substring queries against a
//! naive occurrence-count oracle).
//!
//! Spec: the status tables are proved in
//! `formal-verification/rocq/Spec/AbiStatusMappingSpec.v` (plan obligation
//! #13) and the traversal-snapshot functional spec in
//! `formal-verification/rocq/Spec/AbiTraversalSnapshotSpec.v` (obligation
//! #11); this file is the CRUD-surface correspondence anchor.
//!
//! INVARIANT-HOOK: LDICT-STAT-1 — success statuses and boolean out-params of
//! insert/remove/contains/get track the map oracle exactly: `inserted` is
//! true iff the term was new, `removed` iff it existed, values follow
//! last-write-wins including the value -> absent transition.
//! INVARIANT-HOOK: LDICT-SNAP-2 — after any script, a snapshot captured
//! through the resource ABI walks to exactly the oracle's final contents.
//!
//! Oracle notes (pinned empirically before writing, then asserted):
//! * SCDAWG re-insertion of an existing term returns `inserted == 0` while
//!   updating the value (same law as DynamicDAWG).
//! * `ldict_scdawg_substring_frequency` counts occurrence POSITIONS over the
//!   set of DISTINCT indexed terms (Blumer et al. 1987 `freq`): the empty
//!   pattern therefore counts `len + 1` positions per term.

#![cfg(feature = "ffi")]

mod ffi_common;

use std::collections::BTreeMap;

use ffi_common::{
    byte_labels, capture_snapshot, contains_text, contains_u64, get_text, get_u64, insert_text,
    insert_u64, remove_text, remove_u64, unicode_labels, walk_terms, DictGuard, DOMAIN_BYTE,
    DOMAIN_U64, DOMAIN_UNICODE,
};
use libdictenstein::ffi::{
    ldict_dictionary_clear, ldict_dictionary_compact, ldict_dictionary_len,
    ldict_scdawg_contains_substring, ldict_scdawg_substring_frequency, LdictStatus,
};
use proptest::prelude::*;

/// One scripted operation over a text-keyed dictionary.
#[derive(Clone, Debug)]
enum TextOp {
    /// Insert without a value (`InsertText`).
    InsertText(Vec<u8>),
    /// Insert with a value (`InsertValue`).
    InsertValue(Vec<u8>, u64),
    Remove(Vec<u8>),
    Clear,
    Compact,
    ContainsQuery(Vec<u8>),
    GetValue(Vec<u8>),
}

fn text_op_strategy(term: impl Strategy<Value = Vec<u8>> + Clone) -> impl Strategy<Value = TextOp> {
    prop_oneof![
        3 => term.clone().prop_map(TextOp::InsertText),
        3 => (term.clone(), any::<u64>()).prop_map(|(t, v)| TextOp::InsertValue(t, v)),
        2 => term.clone().prop_map(TextOp::Remove),
        1 => Just(TextOp::Clear),
        1 => Just(TextOp::Compact),
        2 => term.clone().prop_map(TextOp::ContainsQuery),
        2 => term.prop_map(TextOp::GetValue),
    ]
}

fn dictionary_len(dictionary: &DictGuard) -> usize {
    let mut len = usize::MAX;
    assert_eq!(
        unsafe { ldict_dictionary_len(dictionary.ptr(), &mut len) },
        LdictStatus::Ok
    );
    len
}

/// Drive one text script and verify every observable against the oracle.
fn run_text_script(
    dictionary: &DictGuard,
    script: Vec<TextOp>,
) -> Result<BTreeMap<Vec<u8>, Option<u64>>, TestCaseError> {
    let mut model: BTreeMap<Vec<u8>, Option<u64>> = BTreeMap::new();
    for op in script {
        match op {
            TextOp::InsertText(term) => {
                let (status, inserted) = insert_text(dictionary.ptr(), &term, None);
                prop_assert_eq!(status, LdictStatus::Ok);
                prop_assert_eq!(inserted, !model.contains_key(&term), "insert newness");
                model.insert(term, None);
            }
            TextOp::InsertValue(term, value) => {
                let (status, inserted) = insert_text(dictionary.ptr(), &term, Some(value));
                prop_assert_eq!(status, LdictStatus::Ok);
                prop_assert_eq!(inserted, !model.contains_key(&term), "insert newness");
                model.insert(term, Some(value));
            }
            TextOp::Remove(term) => {
                let (status, removed) = remove_text(dictionary.ptr(), &term);
                prop_assert_eq!(status, LdictStatus::Ok);
                prop_assert_eq!(removed, model.remove(&term).is_some(), "remove existence");
            }
            TextOp::Clear => {
                let status = unsafe { ldict_dictionary_clear(dictionary.ptr()) };
                prop_assert_eq!(status, LdictStatus::Ok);
                model.clear();
            }
            TextOp::Compact => {
                let mut reclaimed = usize::MAX;
                let status = unsafe { ldict_dictionary_compact(dictionary.ptr(), &mut reclaimed) };
                prop_assert_eq!(status, LdictStatus::Ok);
                prop_assert!(reclaimed != usize::MAX, "compact must report a count");
            }
            TextOp::ContainsQuery(term) => {
                let (status, contains) = contains_text(dictionary.ptr(), &term);
                prop_assert_eq!(status, LdictStatus::Ok);
                prop_assert_eq!(contains, model.contains_key(&term));
            }
            TextOp::GetValue(term) => {
                let (status, observed) = get_text(dictionary.ptr(), &term);
                prop_assert_eq!(status, LdictStatus::Ok);
                prop_assert_eq!(observed, model.get(&term).cloned());
            }
        }
        prop_assert_eq!(
            dictionary_len(dictionary),
            model.len(),
            "len tracks the oracle"
        );
    }
    // Final sweep: every oracle entry is observable; a disjoint probe is not.
    for (term, value) in &model {
        prop_assert_eq!(
            contains_text(dictionary.ptr(), term),
            (LdictStatus::Ok, true)
        );
        prop_assert_eq!(
            get_text(dictionary.ptr(), term),
            (LdictStatus::Ok, Some(*value))
        );
    }
    Ok(model)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// DynamicDAWG / Byte domain: arbitrary raw byte terms (including empty,
    /// NUL, and 0xFF) against the map oracle, then LDICT-SNAP-2 via a full
    /// ABI snapshot walk.
    #[test]
    fn dynamic_byte_scripts_correspond_to_the_map_oracle(
        script in prop::collection::vec(
            text_op_strategy(prop::collection::vec(any::<u8>(), 0..10)),
            0..48,
        ),
    ) {
        let dictionary = DictGuard::dynamic(DOMAIN_BYTE);
        let model = run_text_script(&dictionary, script)?;
        let snapshot = capture_snapshot(dictionary.resource());
        let expected: BTreeMap<Vec<u64>, Option<u64>> = model
            .iter()
            .map(|(term, value)| (byte_labels(term), *value))
            .collect();
        prop_assert_eq!(walk_terms(snapshot.resource, 16), expected);
    }

    /// DynamicDAWG / Unicode-scalar domain: UTF-8 terms against the map
    /// oracle, then LDICT-SNAP-2 via a full ABI snapshot walk.
    #[test]
    fn dynamic_unicode_scripts_correspond_to_the_map_oracle(
        script in prop::collection::vec(
            text_op_strategy("[a-c]{1,6}".prop_map(String::into_bytes)),
            0..48,
        ),
    ) {
        let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
        let model = run_text_script(&dictionary, script)?;
        let snapshot = capture_snapshot(dictionary.resource());
        let expected: BTreeMap<Vec<u64>, Option<u64>> = model
            .iter()
            .map(|(term, value)| {
                let text = std::str::from_utf8(term).expect("strategy emits UTF-8");
                (unicode_labels(text), *value)
            })
            .collect();
        prop_assert_eq!(walk_terms(snapshot.resource, 16), expected);
    }

    /// DynamicDAWG / U64 domain: the same op algebra adapted to u64 arrays.
    #[test]
    fn dynamic_u64_scripts_correspond_to_the_map_oracle(
        script in prop::collection::vec(
            (0u8..7, prop::collection::vec(any::<u64>(), 0..8), any::<u64>()),
            0..48,
        ),
    ) {
        let dictionary = DictGuard::dynamic(DOMAIN_U64);
        let mut model: BTreeMap<Vec<u64>, Option<u64>> = BTreeMap::new();
        for (opcode, term, value) in script {
            match opcode {
                0 | 1 => {
                    let payload = match opcode {
                        0 => None,
                        _ => Some(value),
                    };
                    let (status, inserted) = insert_u64(dictionary.ptr(), &term, payload);
                    prop_assert_eq!(status, LdictStatus::Ok);
                    prop_assert_eq!(inserted, !model.contains_key(&term));
                    model.insert(term, payload);
                }
                2 => {
                    let (status, removed) = remove_u64(dictionary.ptr(), &term);
                    prop_assert_eq!(status, LdictStatus::Ok);
                    prop_assert_eq!(removed, model.remove(&term).is_some());
                }
                3 => {
                    let status = unsafe { ldict_dictionary_clear(dictionary.ptr()) };
                    prop_assert_eq!(status, LdictStatus::Ok);
                    model.clear();
                }
                4 => {
                    let mut reclaimed = usize::MAX;
                    let status =
                        unsafe { ldict_dictionary_compact(dictionary.ptr(), &mut reclaimed) };
                    prop_assert_eq!(status, LdictStatus::Ok);
                }
                5 => {
                    let (status, contains) = contains_u64(dictionary.ptr(), &term);
                    prop_assert_eq!(status, LdictStatus::Ok);
                    prop_assert_eq!(contains, model.contains_key(&term));
                }
                _ => {
                    let (status, observed) = get_u64(dictionary.ptr(), &term);
                    prop_assert_eq!(status, LdictStatus::Ok);
                    prop_assert_eq!(observed, model.get(&term).cloned());
                }
            }
            prop_assert_eq!(dictionary_len(&dictionary), model.len());
        }
        for (term, value) in &model {
            prop_assert_eq!(contains_u64(dictionary.ptr(), term), (LdictStatus::Ok, true));
            prop_assert_eq!(get_u64(dictionary.ptr(), term), (LdictStatus::Ok, Some(*value)));
        }
        // LDICT-SNAP-2: the snapshot walk equals the final oracle.
        let snapshot = capture_snapshot(dictionary.resource());
        prop_assert_eq!(walk_terms(snapshot.resource, 16), model);
    }

    /// SCDAWG: insert-only scripts, exact-term oracle, and substring queries
    /// against naive contains/occurrence-count oracles.
    #[test]
    fn scdawg_scripts_correspond_to_the_substring_oracles(
        inserts in prop::collection::vec(
            ("[ab]{1,6}", prop::option::of(any::<u64>())),
            1..24,
        ),
        patterns in prop::collection::vec("[ab]{1,3}", 1..12),
    ) {
        let dictionary = DictGuard::scdawg(DOMAIN_BYTE);
        let mut model: BTreeMap<String, Option<u64>> = BTreeMap::new();
        for (term, value) in inserts {
            let (status, inserted) = insert_text(dictionary.ptr(), term.as_bytes(), value);
            prop_assert_eq!(status, LdictStatus::Ok);
            prop_assert_eq!(inserted, !model.contains_key(&term), "SCDAWG insert newness");
            model.insert(term, value);
        }
        prop_assert_eq!(dictionary_len(&dictionary), model.len());

        // Exact-term correspondence (status + value law).
        for (term, value) in &model {
            prop_assert_eq!(
                contains_text(dictionary.ptr(), term.as_bytes()),
                (LdictStatus::Ok, true)
            );
            prop_assert_eq!(
                get_text(dictionary.ptr(), term.as_bytes()),
                (LdictStatus::Ok, Some(*value))
            );
        }

        // Substring correspondence against the naive oracles.
        for pattern in patterns {
            let expected_contains = model.keys().any(|term| term.contains(&pattern));
            let expected_frequency: usize = model
                .keys()
                .map(|term| occurrence_positions(term, &pattern))
                .sum();

            let mut contains = u8::MAX;
            let status = unsafe {
                ldict_scdawg_contains_substring(
                    dictionary.ptr(),
                    pattern.as_ptr(),
                    pattern.len(),
                    &mut contains,
                )
            };
            prop_assert_eq!(status, LdictStatus::Ok);
            prop_assert_eq!(contains == 1, expected_contains, "contains_substring({})", pattern);

            let mut frequency = usize::MAX;
            let status = unsafe {
                ldict_scdawg_substring_frequency(
                    dictionary.ptr(),
                    pattern.as_ptr(),
                    pattern.len(),
                    &mut frequency,
                )
            };
            prop_assert_eq!(status, LdictStatus::Ok);
            prop_assert_eq!(frequency, expected_frequency, "frequency({})", pattern);
        }
    }
}

/// Number of (possibly overlapping) occurrence positions of `pattern` in
/// `term` — the naive counterpart of SCDAWG `freq` for one term. ASCII-only
/// inputs keep byte and character positions identical.
fn occurrence_positions(term: &str, pattern: &str) -> usize {
    let term = term.as_bytes();
    let pattern = pattern.as_bytes();
    match pattern.len() > term.len() {
        true => 0,
        false => (0..=term.len() - pattern.len())
            .filter(|&start| &term[start..start + pattern.len()] == pattern)
            .count(),
    }
}

/// The empty pattern occurs at `len + 1` positions in every distinct term.
#[test]
fn scdawg_empty_pattern_counts_every_position() {
    let dictionary = DictGuard::scdawg(DOMAIN_BYTE);
    for term in ["aba", "ab", "aba"] {
        assert_eq!(
            insert_text(dictionary.ptr(), term.as_bytes(), None).0,
            LdictStatus::Ok
        );
    }
    // Distinct terms: {"aba", "ab"} -> (3 + 1) + (2 + 1) = 7 positions.
    let mut frequency = usize::MAX;
    let status = unsafe {
        ldict_scdawg_substring_frequency(dictionary.ptr(), std::ptr::null(), 0, &mut frequency)
    };
    assert_eq!((status, frequency), (LdictStatus::Ok, 7));
    let mut contains = u8::MAX;
    let status = unsafe {
        ldict_scdawg_contains_substring(dictionary.ptr(), std::ptr::null(), 0, &mut contains)
    };
    assert_eq!((status, contains), (LdictStatus::Ok, 1));
}

/// SCDAWG value law example: re-insertion updates last-write-wins while
/// reporting `inserted == 0`, and duplicate inserts do not inflate `freq`.
#[test]
fn scdawg_reinsertion_updates_values_without_double_counting() {
    let dictionary = DictGuard::scdawg(DOMAIN_UNICODE);
    let (status, inserted) = insert_text(dictionary.ptr(), b"ab", Some(1));
    assert_eq!((status, inserted), (LdictStatus::Ok, true));
    let (status, inserted) = insert_text(dictionary.ptr(), b"ab", Some(2));
    assert_eq!((status, inserted), (LdictStatus::Ok, false));
    assert_eq!(
        get_text(dictionary.ptr(), b"ab"),
        (LdictStatus::Ok, Some(Some(2)))
    );
    let (status, inserted) = insert_text(dictionary.ptr(), b"ab", None);
    assert_eq!((status, inserted), (LdictStatus::Ok, false));
    assert_eq!(
        get_text(dictionary.ptr(), b"ab"),
        (LdictStatus::Ok, Some(None))
    );
    assert_eq!(dictionary_len(&dictionary), 1);

    let mut frequency = usize::MAX;
    let status = unsafe {
        ldict_scdawg_substring_frequency(dictionary.ptr(), b"a".as_ptr(), 1, &mut frequency)
    };
    assert_eq!(
        (status, frequency),
        (LdictStatus::Ok, 1),
        "freq is term-set based"
    );
}
