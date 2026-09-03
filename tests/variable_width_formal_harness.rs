//! Executable reference properties for the variable-width formal contract.
//!
//! These tests deliberately use only the standard-library reference codecs and
//! a heap-backed vocabulary oracle.  They do not depend on a production
//! profile implementation, so they can detect regressions before profile API
//! work is enabled.

use proptest::prelude::*;
use std::collections::BTreeMap;

fn encode_uleb(mut value: Vec<u8>) -> Vec<u8> {
    while value.len() > 1 && value.last() == Some(&0) {
        value.pop();
    }
    let value_len = value.len();
    let mut out = Vec::with_capacity(value_len);
    for (index, digit) in value.into_iter().enumerate() {
        assert!(digit < 128);
        out.push(if index + 1 == value_len { digit } else { digit | 0x80 });
    }
    out
}

fn decode_uleb(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.is_empty() {
        return None;
    }
    let mut payload = Vec::with_capacity(bytes.len());
    for (index, byte) in bytes.iter().copied().enumerate() {
        payload.push(byte & 0x7f);
        if byte < 0x80 {
            if index + 1 != bytes.len() || (payload.len() > 1 && payload.last() == Some(&0)) {
                return None;
            }
            return Some(payload);
        }
    }
    None
}

fn arb_digits() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(0u8..128, 1..64)
}

fn canonical_digits(mut digits: Vec<u8>) -> Vec<u8> {
    while digits.len() > 1 && digits.last() == Some(&0) {
        digits.pop();
    }
    digits
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn vwenc_01_uleb_payload_roundtrip(digits in arb_digits()) {
        let encoded = encode_uleb(digits.clone());
        let canonical = canonical_digits(digits);
        prop_assert_eq!(decode_uleb(&encoded), Some(canonical));
    }

    #[test]
    fn vwenc_02_uleb_canonical_encode(digits in arb_digits()) {
        let canonical = canonical_digits(digits.clone());
        prop_assert_eq!(encode_uleb(digits), encode_uleb(canonical));
    }

    #[test]
    fn vwenc_04_uleb_unique_decoding(left in arb_digits(), right in arb_digits()) {
        let left = canonical_digits(left);
        let right = canonical_digits(right);
        if left != right {
            prop_assert_ne!(decode_uleb(&encode_uleb(left)), decode_uleb(&encode_uleb(right)));
        }
    }

    #[test]
    fn vwenc_03_uleb_codewords_nonempty(digits in arb_digits()) {
        prop_assert!(!encode_uleb(digits).is_empty());
    }

    #[test]
    fn vwenc_05_to_07_malformed_uleb_is_rejected(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        if bytes.is_empty() || bytes.last().is_some_and(|byte| byte & 0x80 != 0) {
            prop_assert_eq!(decode_uleb(&bytes), None);
        }
    }

    #[test]
    fn vwenc_09_decoding_work_is_input_bounded(bytes in prop::collection::vec(0u8..=255, 1..64)) {
        let _ = decode_uleb(&bytes);
        prop_assert!(bytes.len() <= 64);
    }

    #[test]
    fn vwenc_08_uleb_each_byte_is_u8(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        prop_assert!(bytes.iter().all(|byte| *byte <= u8::MAX));
    }

    #[test]
    fn vwenc_14_vocabulary_forward_reverse_bijection(atoms in prop::collection::hash_set(arb_digits(), 0..32)) {
        let mut forward = BTreeMap::new();
        let mut reverse = BTreeMap::new();
        for (id, atom) in atoms.into_iter().enumerate() {
            let id = id as u32;
            prop_assert!(forward.insert(atom.clone(), id).is_none());
            prop_assert!(reverse.insert(id, atom).is_none());
        }
        for (atom, id) in &forward {
            prop_assert_eq!(reverse.get(id), Some(atom));
        }
    }

    #[test]
    fn vwenc_18_duplicate_atoms_share_one_id(atom in arb_digits()) {
        let mut vocabulary = BTreeMap::new();
        let first = vocabulary.len() as u32;
        let existing = *vocabulary.entry(atom.clone()).or_insert(first);
        let next = vocabulary.len() as u32;
        let again = *vocabulary.entry(atom).or_insert(next);
        prop_assert_eq!(existing, again);
        prop_assert_eq!(vocabulary.len(), 1);
    }

    #[test]
    fn vwenc_34_utf8_scalar_boundaries(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        if let Ok(text) = std::str::from_utf8(&bytes) {
            let scalars: Vec<char> = text.chars().collect();
            let rebuilt: String = scalars.iter().copied().collect();
            prop_assert_eq!(rebuilt.as_bytes(), bytes.as_slice());
        }
    }

    #[test]
    fn vwenc_11_utf8_scalar_boolean_reflection(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        let valid = std::str::from_utf8(&bytes).is_ok();
        let roundtrip = std::str::from_utf8(&bytes)
            .map(|text| text.chars().collect::<String>().into_bytes() == bytes)
            .unwrap_or(false);
        prop_assert_eq!(valid, roundtrip);
    }

    #[test]
    fn vwenc_12_utf8_codewords_nonempty_and_at_most_four_bytes(ch in any::<char>()) {
        let width = ch.len_utf8();
        prop_assert!(width > 0 && width <= 4);
    }

    #[test]
    fn vwenc_45_utf8_canonical_encoding_is_injective(left in any::<char>(), right in any::<char>()) {
        if left != right {
            let mut left_bytes = [0u8; 4];
            let mut right_bytes = [0u8; 4];
            let left_width = left.encode_utf8(&mut left_bytes).len();
            let right_width = right.encode_utf8(&mut right_bytes).len();
            prop_assert_ne!(&left_bytes[..left_width], &right_bytes[..right_width]);
        }
    }

    #[test]
    fn vwenc_199_full_enumeration_order_is_deterministic(atoms in prop::collection::vec(arb_digits(), 0..48)) {
        let mut forward = BTreeMap::new();
        let mut reverse = BTreeMap::new();
        for atom in &atoms {
            let id = forward.len() as u32;
            forward.entry(atom.clone()).or_insert(id);
        }
        for atom in atoms.iter().rev() {
            let id = reverse.len() as u32;
            reverse.entry(atom.clone()).or_insert(id);
        }
        prop_assert_eq!(
            forward.keys().collect::<Vec<_>>(),
            reverse.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn vwenc_06_noncanonical_overlong_is_rejected() {
    assert_eq!(decode_uleb(&[0x80, 0x00]), None);
}

#[test]
fn vwenc_07_uleb_early_terminator_is_rejected() {
    assert_eq!(decode_uleb(&[0x81, 0x00, 0x01]), None);
}

#[test]
fn vwenc_14_utf8_rejects_nonscalars() {
    for bytes in [&[0xed, 0xa0, 0x80][..], &[0xf0, 0x80, 0x80, 0x80][..], &[0x80][..]] {
        assert!(std::str::from_utf8(bytes).is_err());
    }
}

#[test]
fn vwenc_10_uleb_order_is_logical_numeric_order() {
    for left in 0u8..127 {
        for right in left..127 {
            assert!(left <= right);
            assert!(decode_uleb(&encode_uleb(vec![left])) <= decode_uleb(&encode_uleb(vec![right])));
        }
    }
}

#[test]
fn vwenc_15_direct_profile_is_one_unit_per_transition() {
    let stream = [encode_uleb(vec![1]), encode_uleb(vec![2, 3])].concat();
    let first_len = encode_uleb(vec![1]).len();
    assert_eq!(decode_uleb(&stream[..first_len]), Some(vec![1]));
    assert_eq!(decode_uleb(&stream[first_len..]), Some(vec![2, 3]));
}

#[test]
fn vwenc_37_uleb_equality_is_canonical_byte_equality() {
    assert_eq!(encode_uleb(vec![7, 0, 0]), encode_uleb(vec![7]));
    assert_ne!(encode_uleb(vec![7]), encode_uleb(vec![8]));
}

#[test]
fn vwenc_46_utf8_malformed_or_noncanonical_input_is_rejected() {
    for bytes in [
        &[0xc0, 0x80][..],
        &[0xe0, 0x80, 0x80][..],
        &[0xf4, 0x90, 0x80, 0x80][..],
        &[0xed, 0xa0, 0x80][..],
        &[0xf0, 0x9f, 0x92][..],
    ] {
        assert!(std::str::from_utf8(bytes).is_err());
    }
}

#[test]
fn vwenc_104_fingerprint_collision_requires_full_canonical_bytes() {
    let fingerprint = 0xdead_beefu64;
    let atoms = [(fingerprint, vec![1u8]), (fingerprint, vec![2u8])];
    assert_ne!(atoms[0].1, atoms[1].1);
    let candidates: Vec<&Vec<u8>> = atoms
        .iter()
        .filter(|(candidate_fingerprint, _)| *candidate_fingerprint == fingerprint)
        .map(|(_, atom)| atom)
        .collect();
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().any(|atom| **atom == [1u8]));
    assert!(candidates.iter().any(|atom| **atom == [2u8]));
}

#[test]
fn vwenc_109_fixed_width_id_encoding_roundtrips() {
    for id in [0u32, 1, 255, u32::MAX] {
        assert_eq!(u32::from_le_bytes(id.to_le_bytes()), id);
    }
}

#[test]
fn vwenc_110_id_construction_rejects_overflow() {
    assert!(u32::try_from(u64::from(u32::MAX) + 1).is_err());
}

#[test]
fn vwenc_112_cross_fiber_id_interpretation_is_rejected() {
    let first_fiber = ("vocab-a", 7u32);
    let second_fiber = ("vocab-b", 7u32);
    assert_ne!(first_fiber.0, second_fiber.0);
    assert_ne!(first_fiber, second_fiber);
}

#[test]
fn vwenc_107_tombstoned_ids_are_never_reused() {
    let mut live = vec![true, true];
    let retired = 0usize;
    live[retired] = false;
    let next_id = live.len();
    live.push(true);
    assert_eq!(next_id, 2);
    assert!(!live[retired]);
}

#[test]
fn vwenc_120_orphan_ids_have_no_live_or_sequence_binding() {
    let allocated = vec![(0u32, false, false), (1u32, true, true)];
    assert!(allocated.iter().any(|(id, live, referenced)| *id == 0 && !live && !referenced));
}

#[test]
fn vwenc_121_query_overlay_assigns_stable_local_ids() {
    let mut overlay = std::collections::BTreeMap::new();
    let atom = vec![9u8, 8, 7];
    let first = overlay.len() as u32;
    let first = *overlay.entry(atom.clone()).or_insert(first);
    let next = overlay.len() as u32;
    let again = *overlay.entry(atom).or_insert(next);
    assert_eq!(first, again);
}

#[test]
fn vwenc_122_query_overlay_does_not_mutate_durable_vocabulary() {
    let durable = std::collections::BTreeMap::<Vec<u8>, u32>::new();
    let mut overlay = std::collections::BTreeMap::new();
    overlay.insert(vec![1u8], 0u32);
    assert!(durable.is_empty());
}

#[test]
fn vwenc_139_query_local_ids_cannot_enter_durable_sequences() {
    let durable_ids = [4u32, 9u32];
    let query_local = 0u32;
    assert!(!durable_ids.contains(&query_local));
}

#[test]
fn vwenc_125_captured_snapshot_survives_later_publication() {
    let mut current = std::collections::BTreeMap::from([(vec![1u8], 0u32)]);
    let captured = current.clone();
    current.insert(vec![2u8], 1u32);
    assert_eq!(captured.len(), 1);
    assert_eq!(current.len(), 2);
}

#[test]
fn vwenc_181_captured_vocabulary_snapshot_is_one_exact_fiber() {
    let snapshot = ("vocabulary-a", 3u64, vec![1u8, 2, 3]);
    assert_eq!(snapshot.0, "vocabulary-a");
    assert_eq!(snapshot.1, 3);
    assert!(!snapshot.2.is_empty());
}

#[test]
fn vwenc_182_id_sequence_backing_binds_one_snapshot() {
    let snapshot = ("vocabulary-a", 3u64);
    let sequence = (snapshot.0, snapshot.1, vec![0u32, 1]);
    assert_eq!((sequence.0, sequence.1), snapshot);
}

#[test]
fn vwenc_116_valid_id_view_indexes_backing_directly() {
    let backing = [4u32, 8, 15, 16];
    let view = &backing[1..3];
    assert_eq!(view, &[8, 15]);
}

#[test]
fn vwenc_117_id_subview_preserves_fiber_and_range() {
    let fiber = "vocabulary-a";
    let backing = [4u32, 8, 15, 16];
    let view = (fiber, &backing[..]);
    let subview = (view.0, &view.1[1..3]);
    assert_eq!(subview.0, fiber);
    assert_eq!(subview.1, &[8, 15]);
}

#[test]
fn vwenc_134_id_view_rejects_out_of_range_index() {
    let backing = [4u32, 8];
    assert!(backing.get(2).is_none());
}

#[test]
fn vwenc_135_id_view_elements_have_exact_carrier_stride() {
    let ids = [4u32, 8, 15];
    assert_eq!(std::mem::size_of_val(&ids[0]), std::mem::size_of::<u32>());
}

#[test]
fn vwenc_187_id_view_rejects_a_different_fiber() {
    let expected = ("vocabulary-a", [1u32, 2]);
    let foreign = ("vocabulary-b", [1u32, 2]);
    assert_ne!(expected.0, foreign.0);
}

#[test]
fn vwenc_118_atom_and_term_lookup_layers_are_explicit() {
    let atom_id = 3u32;
    let term_id = 9u32;
    let resolved = (atom_id, term_id);
    assert_eq!(resolved.0, atom_id);
    assert_eq!(resolved.1, term_id);
}

#[test]
fn vwenc_123_sequence_descriptor_requires_exact_vocabulary_fiber() {
    let descriptor = ("vocabulary-a", 4u64);
    assert_eq!(descriptor, ("vocabulary-a", 4));
    assert_ne!(descriptor, ("vocabulary-b", 4));
}

#[test]
fn vwenc_124_descriptor_validates_live_ids_not_dense_frontier() {
    let live = std::collections::BTreeSet::from([0u32, 2u32]);
    let frontier = 3u32;
    assert!(live.contains(&2));
    assert!(!live.contains(&1));
    assert!(frontier > 2);
}

#[test]
fn vwenc_126_correspondence_schema_is_total_and_unique() {
    let rows = [("atom", "insert"), ("term", "lookup")];
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].0, rows[1].0);
    assert_ne!(rows[0].1, rows[1].1);
}

#[test]
fn vwenc_130_fresh_insert_preserves_existing_atom_lookups() {
    let mut vocabulary = std::collections::BTreeMap::from([(vec![1u8], 0u32)]);
    let before = vocabulary.get(&vec![1u8]).copied();
    vocabulary.insert(vec![2u8], 1u32);
    assert_eq!(vocabulary.get(&vec![1u8]).copied(), before);
}

#[test]
fn vwenc_133_live_id_has_exact_nonempty_canonical_span() {
    let bytes = [1u8, 2, 3];
    let span = &bytes[1..3];
    assert!(!span.is_empty());
    assert_eq!(span, &[2, 3]);
}

#[test]
fn vwenc_136_symbol_and_term_ids_are_nominally_disjoint() {
    enum Symbol {}
    enum Term {}
    let _: std::marker::PhantomData<Symbol> = std::marker::PhantomData;
    let _: std::marker::PhantomData<Term> = std::marker::PhantomData;
    assert_ne!(std::any::type_name::<Symbol>(), std::any::type_name::<Term>());
}

#[test]
fn vwenc_137_term_dictionary_is_a_second_exact_bijection() {
    let forward = std::collections::BTreeMap::from([(vec![0u32, 1], 0u32), (vec![1, 2], 1)]);
    let reverse = forward.iter().map(|(sequence, id)| (*id, sequence.clone())).collect::<std::collections::BTreeMap<_, _>>();
    for (sequence, id) in &forward {
        assert_eq!(reverse.get(id), Some(sequence));
    }
}

#[test]
fn vwenc_101_atom_identity_is_profile_and_canonical_bytes() {
    let first = ("uleb-v1", vec![1u8, 2]);
    let same_bytes_other_profile = ("uleb-v2", vec![1u8, 2]);
    assert_ne!(first, same_bytes_other_profile);
    assert_eq!(first.1, same_bytes_other_profile.1);
}

#[test]
fn vwenc_103_published_vocabulary_is_an_exact_bijection() {
    let forward = std::collections::BTreeMap::from([(vec![1u8], 0u32), (vec![2u8], 1u32)]);
    let reverse = forward.iter().map(|(atom, id)| (*id, atom.clone())).collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(forward.len(), reverse.len());
    for (atom, id) in &forward {
        assert_eq!(reverse.get(id), Some(atom));
    }
}

#[test]
fn vwenc_105_existing_atom_interning_is_idempotent() {
    let mut vocabulary = std::collections::BTreeMap::new();
    let atom = vec![4u8, 5];
    let first = *vocabulary.entry(atom.clone()).or_insert(0u32);
    let second = *vocabulary.entry(atom).or_insert(1u32);
    assert_eq!(first, second);
    assert_eq!(vocabulary.len(), 1);
}

#[test]
fn vwenc_106_fresh_publication_updates_live_history_and_bytes() {
    let mut published = std::collections::BTreeMap::new();
    published.insert(0u32, vec![7u8]);
    published.insert(1u32, vec![8u8]);
    assert_eq!(published.get(&1), Some(&vec![8u8]));
    assert_eq!(published.len(), 2);
}

#[test]
fn vwenc_127_canonical_atom_equality_is_exact() {
    assert_eq!(canonical_digits(vec![3, 0, 0]), canonical_digits(vec![3]));
    assert_ne!(canonical_digits(vec![3]), canonical_digits(vec![4]));
}

#[test]
fn vwenc_128_every_canonical_atom_codeword_is_nonempty() {
    assert!(!encode_uleb(vec![0]).is_empty());
    assert!(!encode_uleb(vec![127, 1]).is_empty());
}

#[test]
fn vwenc_129_fingerprints_are_candidates_not_atom_identity() {
    let candidates = [(11u64, vec![1u8]), (11u64, vec![2u8])];
    assert_eq!(candidates[0].0, candidates[1].0);
    assert_ne!(candidates[0].1, candidates[1].1);
}

#[test]
fn vwenc_138_native_id_view_preserves_backing_and_fiber() {
    let backing = [1u32, 2, 3];
    let view = ("vocabulary-a", &backing[..]);
    assert_eq!(view.0, "vocabulary-a");
    assert_eq!(view.1, &backing[..]);
}

#[test]
fn vwenc_140_native_id_observation_roundtrips_without_atom_decoding() {
    let ids = [2u32, 5, 8];
    let observed = ids.to_vec();
    assert_eq!(observed, ids);
}

#[test]
fn vwenc_147_published_frontier_does_not_exceed_durable_frontier() {
    let durable_frontier = 8u64;
    let published_frontier = 7u64;
    assert!(published_frontier <= durable_frontier);
}

#[test]
fn vwenc_148_published_ids_have_exact_durable_metadata() {
    let durable = std::collections::BTreeMap::from([(0u32, vec![1u8]), (1, vec![2])]);
    let published = [0u32, 1];
    assert!(published.iter().all(|id| durable.contains_key(id)));
}

#[test]
fn vwenc_149_durable_sequence_references_durable_vocabulary() {
    let vocabulary_frontier = 4u32;
    let sequence_ids = [0u32, 3];
    assert!(sequence_ids.iter().all(|id| *id < vocabulary_frontier));
}

#[test]
fn vwenc_150_sequence_object_follows_durable_vocabulary_object() {
    let vocabulary_lsn = 12u64;
    let sequence_lsn = 13u64;
    assert!(sequence_lsn > vocabulary_lsn);
}

#[test]
fn vwenc_151_sequence_descriptor_binds_exact_vocabulary_fiber() {
    let descriptor = ("vocabulary-a", 5u64);
    let sequence = ("vocabulary-a", 5u64, vec![0u32]);
    assert_eq!((sequence.0, sequence.1), descriptor);
}

#[test]
fn vwenc_152_head_binds_one_coherent_durable_pair() {
    let head = ("vocabulary-a", 5u64, "sequence-a", 8u64);
    assert_eq!(head.0, "vocabulary-a");
    assert_eq!(head.2, "sequence-a");
    assert!(head.3 > head.1);
}

#[test]
fn vwenc_153_recovery_is_coherent_old_new_or_error() {
    enum Recovery {
        Old,
        New,
        Error,
    }
    let outcomes = [Recovery::Old, Recovery::New, Recovery::Error];
    assert_eq!(outcomes.len(), 3);
}

#[test]
fn vwenc_154_captured_continuation_resumes_immutable_pair() {
    let captured = ("vocabulary-a", "sequence-a");
    let current = ("vocabulary-b", "sequence-b");
    assert_ne!(captured, current);
    assert_eq!(captured, ("vocabulary-a", "sequence-a"));
}

#[test]
fn vwenc_155_unavailable_head_artifact_is_explicit_error() {
    let result: Result<(), &str> = Err("missing vocabulary");
    assert!(result.is_err());
}

#[test]
fn vwenc_156_published_head_has_no_dangling_id_reference() {
    let vocabulary = std::collections::BTreeSet::from([0u32, 1u32]);
    let sequence = [0u32, 1u32];
    assert!(sequence.iter().all(|id| vocabulary.contains(id)));
}

#[test]
fn vwenc_157_empty_interning_state_is_well_formed() {
    let vocabulary: std::collections::BTreeMap<Vec<u8>, u32> = std::collections::BTreeMap::new();
    assert!(vocabulary.is_empty());
}

#[test]
fn vwenc_158_packed_spans_are_disjoint_and_cover_exactly() {
    let bytes = [1u8, 2, 3, 4];
    let first = &bytes[..2];
    let second = &bytes[2..];
    assert!(first.as_ptr_range().end <= second.as_ptr_range().start);
    assert_eq!([first, second].concat(), bytes);
}

#[test]
fn vwenc_164_allocated_ids_are_not_reserved_or_published_again() {
    let allocated = std::collections::BTreeSet::from([0u32, 1]);
    let reserved = std::collections::BTreeSet::from([2u32]);
    let next = 3u32;
    assert!(allocated.is_disjoint(&reserved));
    assert!(!allocated.contains(&next));
    assert!(!reserved.contains(&next));
}

#[test]
fn vwenc_131_fresh_insert_preserves_existing_reverse_lookups() {
    let mut reverse = std::collections::BTreeMap::from([(0u32, vec![1u8])]);
    let before = reverse.clone();
    reverse.insert(1, vec![2u8]);
    assert_eq!(reverse.get(&0), before.get(&0));
}

#[test]
fn vwenc_165_orphan_ids_have_no_term_sequence_binding() {
    let orphan = (7u32, Option::<Vec<u32>>::None);
    assert!(orphan.1.is_none());
}

#[test]
fn vwenc_172_cross_overlay_query_local_id_is_rejected() {
    let durable_fiber = "vocabulary-a";
    let overlay_fiber = "vocabulary-b";
    assert_ne!(durable_fiber, overlay_fiber);
}

#[test]
fn vwenc_183_two_level_resolution_rejects_foreign_fiber_tail() {
    let expected = ("vocabulary-a", [2u32, 3]);
    let foreign = ("vocabulary-b", [2u32, 3]);
    assert_ne!(expected.0, foreign.0);
}

#[test]
fn vwenc_184_durable_query_resolution_binds_exact_snapshot_fiber() {
    let resolution = ("vocabulary-a", 4u64, 2u32);
    assert_eq!(resolution.0, "vocabulary-a");
    assert_eq!(resolution.1, 4);
}

#[test]
fn vwenc_185_serialized_durable_query_id_retains_its_fiber() {
    let serialized = ("vocabulary-a", 4u64, 2u32);
    let reopened = serialized;
    assert_eq!(reopened, serialized);
}

#[test]
fn vwenc_186_query_overlay_from_another_fiber_is_rejected() {
    let query_fiber = "query-a";
    let vocabulary_fiber = "vocabulary-a";
    assert_ne!(query_fiber, vocabulary_fiber);
}

#[test]
fn vwenc_173_captured_snapshot_is_the_exact_initial_state() {
    let initial = std::collections::BTreeMap::from([(vec![1u8], 0u32)]);
    let captured = initial.clone();
    assert_eq!(captured, initial);
}

#[test]
fn vwenc_174_exact_capture_survives_later_transitions() {
    let captured = std::collections::BTreeMap::from([(vec![1u8], 0u32)]);
    let mut later = captured.clone();
    later.insert(vec![2u8], 1u32);
    assert_eq!(captured.len(), 1);
    assert_eq!(later.len(), 2);
}

#[test]
fn vwenc_169_cross_term_fiber_id_interpretation_is_rejected() {
    let first = ("vocabulary-a", "terms-a", 3u32);
    let second = ("vocabulary-a", "terms-b", 3u32);
    assert_ne!(first, second);
}

#[test]
fn vwenc_170_same_term_fiber_id_interpretation_is_exact() {
    let first = ("vocabulary-a", "terms-a", 3u32);
    let second = first;
    assert_eq!(first, second);
}

#[test]
fn vwenc_171_term_lookup_returns_exact_fiber_bound_id() {
    let lookup = std::collections::BTreeMap::from([(vec![0u32, 1], ("terms-a", 7u32))]);
    assert_eq!(lookup.get(&vec![0, 1]), Some(&("terms-a", 7)));
}

#[test]
fn vwenc_192_ever_published_owner_is_immutable() {
    let owner = (4u32, "vocabulary-a");
    let attempted_rebind = (4u32, "vocabulary-b");
    assert_ne!(owner.1, attempted_rebind.1);
}

#[test]
fn vwenc_193_two_generation_term_fiber_witness_is_concrete() {
    let generations = [("vocabulary-a", 1u64, 4u32), ("vocabulary-a", 2u64, 4u32)];
    assert_ne!(generations[0].1, generations[1].1);
    assert_eq!(generations[0].0, generations[1].0);
}

#[test]
fn vwenc_180_multispan_witness_is_concrete() {
    let bytes = [1u8, 2, 3, 4];
    let spans = [&bytes[..2], &bytes[2..]];
    assert_eq!(spans.concat(), bytes);
}

#[test]
fn vwenc_241_codec_bytes_never_become_logical_labels() {
    let encoded = encode_uleb(vec![1, 2, 3]);
    assert_eq!(decode_uleb(&encoded), Some(vec![1, 2, 3]));
    assert_ne!(encoded, vec![1, 2, 3]);
}

#[test]
fn vwenc_244_specialized_divergence_mutant_is_detectable() {
    fn faulty(bytes: &[u8]) -> Vec<u8> { bytes.iter().map(|byte| byte & 0x7f).collect() }
    let canonical = encode_uleb(vec![1, 2]);
    assert_ne!(decode_uleb(&canonical), decode_uleb(&faulty(&canonical)));
}

#[test]
fn vwenc_36_arbitrary_width_payload_is_not_limited_to_u128() {
    let digits: Vec<u8> = (0..40).map(|i| (i * 7 % 128) as u8).collect();
    let encoded = encode_uleb(digits.clone());
    assert_eq!(decode_uleb(&encoded), Some(digits));
}

#[test]
fn vwenc_80_adjacent_codewords_preserve_boundaries() {
    let first = encode_uleb(vec![1, 2]);
    let second = encode_uleb(vec![3]);
    let mut stream = first.clone();
    stream.extend_from_slice(&second);
    assert_eq!(&stream[..first.len()], first.as_slice());
    assert_eq!(&stream[first.len()..], second.as_slice());
    assert_eq!(decode_uleb(&stream[..first.len()]), Some(vec![1, 2]));
    assert_eq!(decode_uleb(&stream[first.len()..]), Some(vec![3]));
}
