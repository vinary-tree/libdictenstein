//! C-ABI correspondence tests for snapshot-based dictionary algebra.
//!
//! The operation captures one immutable lexicographic revision per input,
//! performs a linear two-way merge, and feeds the sorted result directly to
//! the DynamicDAWG freeze-once builder.
//!
//! INVARIANT-HOOK: LDICT-ALG-1 — union, intersection, left difference, and
//! symmetric difference equal their mathematical finite-map models.
//! INVARIANT-HOOK: LDICT-ALG-2 — duplicate values obey first, last,
//! `Option<u64>` lattice join, or `Option<u64>` lattice meet exactly.
//! INVARIANT-HOOK: LDICT-ALG-3 — byte, Unicode-scalar, and u64-token domains
//! preserve key units and absent-versus-valueless distinctions.
//! INVARIANT-HOOK: LDICT-ALG-4 — results own an independent mutable revision;
//! later input mutation cannot change them.

#![cfg(feature = "ffi")]

mod ffi_common;

use std::collections::{BTreeMap, BTreeSet};

use ffi_common::{
    algebra, capture_snapshot, insert_text, insert_u64, walk_terms, DictGuard, DOMAIN_BYTE,
    DOMAIN_U64, DOMAIN_UNICODE,
};
use libdictenstein::ffi::{
    ldict_dictionary_algebra, LdictAlgebraOperation, LdictDictionary, LdictStatus, LdictValueMerge,
};
use proptest::prelude::*;

type FiniteMap = BTreeMap<Vec<u64>, Option<u64>>;
type FixturePair = (FiniteMap, FiniteMap);

fn merge_values(left: Option<u64>, right: Option<u64>, policy: LdictValueMerge) -> Option<u64> {
    match policy {
        LdictValueMerge::First => left,
        LdictValueMerge::Last => right,
        LdictValueMerge::LatticeJoin => match (left, right) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        },
        LdictValueMerge::LatticeMeet => match (left, right) {
            (Some(left), Some(right)) => Some(left.min(right)),
            _ => None,
        },
    }
}

fn model(
    left: &FiniteMap,
    right: &FiniteMap,
    operation: LdictAlgebraOperation,
    policy: LdictValueMerge,
) -> FiniteMap {
    let keys: BTreeSet<_> = left.keys().chain(right.keys()).cloned().collect();
    let mut result = BTreeMap::new();
    for key in keys {
        let value = match (left.get(&key), right.get(&key)) {
            (Some(left), Some(right))
                if matches!(
                    operation,
                    LdictAlgebraOperation::Union | LdictAlgebraOperation::Intersection
                ) =>
            {
                Some(merge_values(*left, *right, policy))
            }
            (Some(left), None)
                if matches!(
                    operation,
                    LdictAlgebraOperation::Union
                        | LdictAlgebraOperation::Difference
                        | LdictAlgebraOperation::SymmetricDifference
                ) =>
            {
                Some(*left)
            }
            (None, Some(right))
                if matches!(
                    operation,
                    LdictAlgebraOperation::Union | LdictAlgebraOperation::SymmetricDifference
                ) =>
            {
                Some(*right)
            }
            _ => None,
        };
        if let Some(value) = value {
            result.insert(key, value);
        }
    }
    result
}

fn text_key(domain: u32, units: &[u64]) -> Vec<u8> {
    match domain {
        DOMAIN_BYTE => units
            .iter()
            .map(|unit| u8::try_from(*unit).expect("byte fixture unit"))
            .collect(),
        DOMAIN_UNICODE => units
            .iter()
            .map(|unit| char::from_u32(*unit as u32).expect("Unicode fixture scalar"))
            .collect::<String>()
            .into_bytes(),
        _ => unreachable!("text_key only accepts text domains"),
    }
}

fn dictionary(domain: u32, entries: &FiniteMap) -> DictGuard {
    let dictionary = DictGuard::dynamic(domain);
    for (term, value) in entries {
        let status = if domain == DOMAIN_U64 {
            insert_u64(dictionary.ptr(), term, *value).0
        } else {
            let text = text_key(domain, term);
            insert_text(dictionary.ptr(), &text, *value).0
        };
        assert_eq!(status, LdictStatus::Ok);
    }
    dictionary
}

fn fixtures(domain: u32) -> FixturePair {
    let base = match domain {
        DOMAIN_BYTE => u64::from(b'a'),
        DOMAIN_UNICODE => 0x03B1,
        DOMAIN_U64 => 10_000,
        _ => unreachable!("unknown fixture domain"),
    };
    let key = |offset| vec![base + offset];
    (
        BTreeMap::from([
            (key(0), None),
            (key(1), Some(0)),
            (key(2), Some(9)),
            (key(4), None),
        ]),
        BTreeMap::from([
            (key(1), Some(7)),
            (key(2), None),
            (key(3), Some(4)),
            (key(4), None),
        ]),
    )
}

const OPERATIONS: [LdictAlgebraOperation; 4] = [
    LdictAlgebraOperation::Union,
    LdictAlgebraOperation::Intersection,
    LdictAlgebraOperation::Difference,
    LdictAlgebraOperation::SymmetricDifference,
];
const POLICIES: [LdictValueMerge; 4] = [
    LdictValueMerge::First,
    LdictValueMerge::Last,
    LdictValueMerge::LatticeJoin,
    LdictValueMerge::LatticeMeet,
];

#[test]
fn every_operation_and_value_policy_matches_all_unit_domains() {
    for domain in [DOMAIN_BYTE, DOMAIN_UNICODE, DOMAIN_U64] {
        let (left_model, right_model) = fixtures(domain);
        let left = dictionary(domain, &left_model);
        let right = dictionary(domain, &right_model);

        for operation in OPERATIONS {
            for policy in POLICIES {
                let result = algebra(left.ptr(), right.ptr(), operation, policy);
                let snapshot = capture_snapshot(result.resource());
                assert_eq!(
                    walk_terms(snapshot.resource, 2),
                    model(&left_model, &right_model, operation, policy),
                    "domain={domain}, operation={operation:?}, policy={policy:?}"
                );
            }
        }
    }
}

#[test]
fn result_revision_is_independent_and_mutable() {
    let (left_model, right_model) = fixtures(DOMAIN_UNICODE);
    let left = dictionary(DOMAIN_UNICODE, &left_model);
    let right = dictionary(DOMAIN_UNICODE, &right_model);
    let result = algebra(
        left.ptr(),
        right.ptr(),
        LdictAlgebraOperation::Union,
        LdictValueMerge::LatticeJoin,
    );
    let expected = model(
        &left_model,
        &right_model,
        LdictAlgebraOperation::Union,
        LdictValueMerge::LatticeJoin,
    );

    assert_eq!(
        insert_text(left.ptr(), "ω".as_bytes(), Some(99)).0,
        LdictStatus::Ok
    );
    assert_eq!(
        insert_text(right.ptr(), "ψ".as_bytes(), None).0,
        LdictStatus::Ok
    );
    assert_eq!(
        walk_terms(capture_snapshot(result.resource()).resource, 3),
        expected
    );

    assert_eq!(
        insert_text(result.ptr(), "χ".as_bytes(), Some(42)).0,
        LdictStatus::Ok
    );
    let mut expected_after_result_mutation = expected;
    expected_after_result_mutation.insert(vec![u64::from('χ')], Some(42));
    assert_eq!(
        walk_terms(capture_snapshot(result.resource()).resource, 3),
        expected_after_result_mutation
    );
}

#[test]
fn invalid_inputs_fail_without_publishing_a_handle() {
    let left = DictGuard::dynamic(DOMAIN_BYTE);
    let right = DictGuard::dynamic(DOMAIN_U64);
    let sentinel = std::ptr::dangling_mut::<LdictDictionary>();

    let mut out = sentinel;
    assert_eq!(
        unsafe {
            ldict_dictionary_algebra(
                left.ptr(),
                right.ptr(),
                LdictAlgebraOperation::Union as u32,
                LdictValueMerge::First as u32,
                &mut out,
            )
        },
        LdictStatus::DomainMismatch
    );
    assert!(out.is_null());

    for (operation, policy) in [(0, 1), (1, 0), (u32::MAX, 1), (1, u32::MAX)] {
        out = sentinel;
        assert_eq!(
            unsafe {
                ldict_dictionary_algebra(left.ptr(), left.ptr(), operation, policy, &mut out)
            },
            LdictStatus::InvalidArgument
        );
        assert!(out.is_null());
    }

    out = sentinel;
    assert_eq!(
        unsafe { ldict_dictionary_algebra(std::ptr::null(), left.ptr(), 1, 1, &mut out,) },
        LdictStatus::NullPointer
    );
    assert_eq!(out, sentinel, "input pointers are validated before output");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn randomized_unicode_maps_match_the_finite_map_model(
        left in prop::collection::btree_map("[a-f]{1,5}", prop::option::of(any::<u64>()), 0..32),
        right in prop::collection::btree_map("[a-f]{1,5}", prop::option::of(any::<u64>()), 0..32),
        operation_index in 0usize..OPERATIONS.len(),
        policy_index in 0usize..POLICIES.len(),
    ) {
        let to_units = |map: BTreeMap<String, Option<u64>>| {
            map.into_iter()
                .map(|(key, value)| (key.chars().map(|unit| u64::from(unit as u32)).collect(), value))
                .collect::<BTreeMap<Vec<u64>, Option<u64>>>()
        };
        let left_model = to_units(left);
        let right_model = to_units(right);
        let left = dictionary(DOMAIN_UNICODE, &left_model);
        let right = dictionary(DOMAIN_UNICODE, &right_model);
        let operation = OPERATIONS[operation_index];
        let policy = POLICIES[policy_index];
        let result = algebra(left.ptr(), right.ptr(), operation, policy);

        prop_assert_eq!(
            walk_terms(capture_snapshot(result.resource()).resource, 7),
            model(&left_model, &right_model, operation, policy),
        );
    }
}
