//! Persistent ARTrie + vocabulary durability through the C ABI: create,
//! CRUD, `ldict_dictionary_checkpoint`, free, reopen via the open
//! constructors, and verify against the oracle. Uses `tempfile::tempdir` for
//! isolation; guarded from Miri because the persistent engine uses mmap and
//! real file I/O.
//!
//! Spec: the capture protocol lives in
//! `formal-verification/tla+/AbiProducerSnapshot.tla` (plan obligation #10;
//! durability is realized by the persistent core's checkpoint/WAL suites);
//! this file is the reopen correspondence anchor for the LDICT-SNAP-4/5
//! rows of libdictenstein's invariant registry.
//!
//! INVARIANT-HOOK: LDICT-SNAP-4 — checkpoint/reopen fidelity: a checkpointed
//! revision reopened through `ldict_persistent_artrie_open` /
//! `ldict_persistent_vocab_open` observes exactly the pre-free oracle
//! contents (terms, optional values, and lengths) in every unit domain.
//! INVARIANT-HOOK: LDICT-SNAP-5 — vocabulary index round-trip: term -> index
//! (via `ldict_dictionary_get_text`) and index -> term (via
//! `ldict_vocab_get_term`) form inverse maps across checkpoint/reopen, and
//! index reassignment is refused as an `IoError` before and after reopen.

#![cfg(feature = "ffi")]
#![cfg(not(miri))]

mod ffi_common;

use std::collections::BTreeMap;

use ffi_common::{
    byte_labels, capture_snapshot, contains_text, contains_u64, get_text, get_u64, insert_text,
    insert_u64, last_error, remove_text, remove_u64, snapshot_len, unicode_labels, walk_terms,
    DictGuard, DOMAIN_BYTE, DOMAIN_U64, DOMAIN_UNICODE,
};
use libdictenstein::ffi::{
    ldict_dictionary_checkpoint, ldict_dictionary_len, ldict_persistent_artrie_create,
    ldict_persistent_artrie_open, ldict_persistent_vocab_create, ldict_persistent_vocab_open,
    ldict_vocab_get_term, LdictDictionary, LdictStatus,
};
use proptest::prelude::*;

fn path_bytes(path: &std::path::Path) -> &[u8] {
    path.to_str().expect("tempdir paths are UTF-8").as_bytes()
}

fn create_artrie(path: &std::path::Path, domain: u32) -> DictGuard {
    let bytes = path_bytes(path);
    let mut handle: *mut LdictDictionary = std::ptr::null_mut();
    let status =
        unsafe { ldict_persistent_artrie_create(domain, bytes.as_ptr(), bytes.len(), &mut handle) };
    assert_eq!(status, LdictStatus::Ok, "create failed: {}", last_error());
    DictGuard(handle)
}

fn open_artrie(path: &std::path::Path, domain: u32) -> DictGuard {
    let bytes = path_bytes(path);
    let mut handle: *mut LdictDictionary = std::ptr::null_mut();
    let status =
        unsafe { ldict_persistent_artrie_open(domain, bytes.as_ptr(), bytes.len(), &mut handle) };
    assert_eq!(status, LdictStatus::Ok, "open failed: {}", last_error());
    DictGuard(handle)
}

fn checkpoint(dictionary: &DictGuard) {
    let status = unsafe { ldict_dictionary_checkpoint(dictionary.ptr()) };
    assert_eq!(
        status,
        LdictStatus::Ok,
        "checkpoint failed: {}",
        last_error()
    );
}

fn dictionary_len(dictionary: &DictGuard) -> usize {
    let mut len = usize::MAX;
    assert_eq!(
        unsafe { ldict_dictionary_len(dictionary.ptr(), &mut len) },
        LdictStatus::Ok
    );
    len
}

/// LDICT-SNAP-4 in the byte domain, raw bytes included.
/// INVARIANT-HOOK: LDICT-SNAP-7 — captured root membership and exact length
/// stay coherent through checkpoint and reopen.
#[test]
fn byte_artrie_checkpoint_reopen_preserves_the_oracle() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("byte.artrie");
    let mut model: BTreeMap<Vec<u8>, Option<u64>> = BTreeMap::new();
    {
        let dictionary = create_artrie(&path, DOMAIN_BYTE);
        let script: [(&[u8], Option<u64>); 6] = [
            (&[0x00], Some(0)),
            (&[0x00, 0xFF], None),
            (b"plain", Some(u64::MAX)),
            (&[0xFE, 0x00, 0x01], Some(7)),
            (b"gone", Some(9)),
            (b"plain", Some(42)), // update
        ];
        for (term, value) in script {
            let (status, inserted) = insert_text(dictionary.ptr(), term, value);
            assert_eq!(status, LdictStatus::Ok, "{}", last_error());
            assert_eq!(inserted, !model.contains_key(term), "insert newness");
            model.insert(term.to_vec(), value);
        }
        let (status, removed) = remove_text(dictionary.ptr(), b"gone");
        assert_eq!((status, removed), (LdictStatus::Ok, true));
        model.remove(&b"gone"[..]);
        assert_eq!(dictionary_len(&dictionary), model.len());
        checkpoint(&dictionary);
        // DictGuard drop => ldict_dictionary_free.
    }
    let reopened = open_artrie(&path, DOMAIN_BYTE);
    assert_eq!(dictionary_len(&reopened), model.len());
    for (term, value) in &model {
        assert_eq!(contains_text(reopened.ptr(), term), (LdictStatus::Ok, true));
        assert_eq!(
            get_text(reopened.ptr(), term),
            (LdictStatus::Ok, Some(*value))
        );
    }
    assert_eq!(
        contains_text(reopened.ptr(), b"gone"),
        (LdictStatus::Ok, false)
    );
    // The reopened revision also walks correctly through the resource ABI.
    let snapshot = capture_snapshot(reopened.resource());
    assert_eq!(snapshot_len(snapshot.resource), (model.len(), true));
    let expected: BTreeMap<Vec<u64>, Option<u64>> = model
        .iter()
        .map(|(term, value)| (byte_labels(term), *value))
        .collect();
    assert_eq!(walk_terms(snapshot.resource, 16), expected);
}

/// LDICT-SNAP-4 in the Unicode domain, non-ASCII included.
#[test]
fn unicode_artrie_checkpoint_reopen_preserves_the_oracle() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("unicode.artrie");
    let mut model: BTreeMap<String, Option<u64>> = BTreeMap::new();
    {
        let dictionary = create_artrie(&path, DOMAIN_UNICODE);
        for (term, value) in [
            ("café", Some(1)),
            ("🦀crab", None),
            ("plain", Some(2)),
            ("removed", Some(3)),
        ] {
            let (status, inserted) = insert_text(dictionary.ptr(), term.as_bytes(), value);
            assert_eq!(status, LdictStatus::Ok, "{}", last_error());
            assert!(inserted);
            model.insert(term.to_owned(), value);
        }
        let (status, removed) = remove_text(dictionary.ptr(), "removed".as_bytes());
        assert_eq!((status, removed), (LdictStatus::Ok, true));
        model.remove("removed");
        checkpoint(&dictionary);
    }
    let reopened = open_artrie(&path, DOMAIN_UNICODE);
    assert_eq!(dictionary_len(&reopened), model.len());
    for (term, value) in &model {
        assert_eq!(
            get_text(reopened.ptr(), term.as_bytes()),
            (LdictStatus::Ok, Some(*value))
        );
    }
    let snapshot = capture_snapshot(reopened.resource());
    assert_eq!(snapshot_len(snapshot.resource), (model.len(), true));
    let expected: BTreeMap<Vec<u64>, Option<u64>> = model
        .iter()
        .map(|(term, value)| (unicode_labels(term), *value))
        .collect();
    assert_eq!(walk_terms(snapshot.resource, 16), expected);
}

/// LDICT-SNAP-4 in the u64 domain, extreme tokens included.
#[test]
fn u64_artrie_checkpoint_reopen_preserves_the_oracle() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("tokens.artrie");
    let mut model: BTreeMap<Vec<u64>, Option<u64>> = BTreeMap::new();
    {
        let dictionary = create_artrie(&path, DOMAIN_U64);
        for (term, value) in [
            (vec![0u64], Some(1u64)),
            (vec![u64::MAX], Some(2)),
            (vec![u64::MAX, 0, 7], None),
            (vec![1, 2, 3], Some(3)),
            (vec![9, 9], Some(4)),
        ] {
            let (status, inserted) = insert_u64(dictionary.ptr(), &term, value);
            assert_eq!(status, LdictStatus::Ok, "{}", last_error());
            assert!(inserted);
            model.insert(term, value);
        }
        let (status, removed) = remove_u64(dictionary.ptr(), &[9, 9]);
        assert_eq!((status, removed), (LdictStatus::Ok, true));
        model.remove(&vec![9u64, 9]);
        checkpoint(&dictionary);
    }
    let reopened = open_artrie(&path, DOMAIN_U64);
    assert_eq!(dictionary_len(&reopened), model.len());
    for (term, value) in &model {
        assert_eq!(contains_u64(reopened.ptr(), term), (LdictStatus::Ok, true));
        assert_eq!(
            get_u64(reopened.ptr(), term),
            (LdictStatus::Ok, Some(*value))
        );
    }
    assert_eq!(
        contains_u64(reopened.ptr(), &[9, 9]),
        (LdictStatus::Ok, false)
    );
    let snapshot = capture_snapshot(reopened.resource());
    assert_eq!(snapshot_len(snapshot.resource), (model.len(), true));
}

/// LDICT-SNAP-5: the vocabulary's bidirectional index survives reopen.
#[test]
fn vocab_checkpoint_reopen_round_trips_terms_and_indices() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("vocab.vocab");
    // term -> index oracle, read back through the ABI at insert time.
    let mut model: BTreeMap<String, u64> = BTreeMap::new();
    {
        let vocabulary = {
            let bytes = path_bytes(&path);
            let mut handle: *mut LdictDictionary = std::ptr::null_mut();
            let status =
                unsafe { ldict_persistent_vocab_create(bytes.as_ptr(), bytes.len(), &mut handle) };
            assert_eq!(status, LdictStatus::Ok, "{}", last_error());
            DictGuard(handle)
        };
        // Mix auto-assigned and explicit indices.
        for (term, explicit) in [
            ("alpha", None),
            ("beta", Some(7u64)),
            ("gamma", None),
            ("delta", Some(100)),
            ("epsilon", None),
        ] {
            let (status, inserted) = insert_text(vocabulary.ptr(), term.as_bytes(), explicit);
            assert_eq!(status, LdictStatus::Ok, "{}", last_error());
            assert!(inserted);
            let (status, observed) = get_text(vocabulary.ptr(), term.as_bytes());
            assert_eq!(status, LdictStatus::Ok);
            let index = observed
                .expect("term just inserted")
                .expect("vocabulary terms always map to an index");
            if let Some(expected) = explicit {
                assert_eq!(index, expected, "explicit index must be honoured");
            }
            model.insert(term.to_owned(), index);
        }
        // Auto-assigned indices are unique.
        let mut indices: Vec<u64> = model.values().copied().collect();
        indices.sort_unstable();
        indices.dedup();
        assert_eq!(indices.len(), model.len(), "indices must be unique");

        // Reassignment is refused with IoError while the handle is live.
        let (status, inserted) = insert_text(vocabulary.ptr(), "alpha".as_bytes(), Some(999));
        assert_eq!((status, inserted), (LdictStatus::IoError, false));
        assert!(last_error().contains("already assigned"));

        checkpoint(&vocabulary);
    }

    let reopened = {
        let bytes = path_bytes(&path);
        let mut handle: *mut LdictDictionary = std::ptr::null_mut();
        let status =
            unsafe { ldict_persistent_vocab_open(bytes.as_ptr(), bytes.len(), &mut handle) };
        assert_eq!(status, LdictStatus::Ok, "{}", last_error());
        DictGuard(handle)
    };
    assert_eq!(dictionary_len(&reopened), model.len());
    let snapshot = capture_snapshot(reopened.resource());
    assert_eq!(snapshot_len(snapshot.resource), (model.len(), true));
    for (term, index) in &model {
        // term -> index.
        assert_eq!(
            get_text(reopened.ptr(), term.as_bytes()),
            (LdictStatus::Ok, Some(Some(*index)))
        );
        // index -> term, size query first, then the exact copy.
        let (mut out_len, mut out_found) = (usize::MAX, u8::MAX);
        let status = unsafe {
            ldict_vocab_get_term(
                reopened.ptr(),
                *index,
                std::ptr::null_mut(),
                0,
                &mut out_len,
                &mut out_found,
            )
        };
        assert_eq!(
            (status, out_found),
            (LdictStatus::Ok, 1),
            "size query for {term}"
        );
        assert_eq!(out_len, term.len());
        let mut buffer = vec![0u8; out_len];
        let (mut copy_len, mut copy_found) = (usize::MAX, u8::MAX);
        let status = unsafe {
            ldict_vocab_get_term(
                reopened.ptr(),
                *index,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut copy_len,
                &mut copy_found,
            )
        };
        assert_eq!(
            (status, copy_found, copy_len),
            (LdictStatus::Ok, 1, term.len())
        );
        assert_eq!(
            buffer,
            term.as_bytes(),
            "index {index} must round-trip to {term}"
        );
    }
    // Absent indices stay absent after reopen.
    let absent = model.values().max().expect("non-empty") + 1;
    let (mut out_len, mut out_found) = (usize::MAX, u8::MAX);
    let status = unsafe {
        ldict_vocab_get_term(
            reopened.ptr(),
            absent,
            std::ptr::null_mut(),
            0,
            &mut out_len,
            &mut out_found,
        )
    };
    assert_eq!((status, out_found, out_len), (LdictStatus::Ok, 0, 0));
    // Reassignment refusal survives reopen.
    let (status, _) = insert_text(reopened.ptr(), "beta".as_bytes(), Some(3));
    assert_eq!(status, LdictStatus::IoError);
    // Idempotent re-insert at the SAME index is accepted (not new).
    let (status, inserted) = insert_text(reopened.ptr(), "beta".as_bytes(), Some(7));
    assert_eq!((status, inserted), (LdictStatus::Ok, false));
}

/// Opening a nonexistent path is an `IoError` with a nulled out-param and a
/// non-empty thread-local message; creating over an existing store is
/// likewise refused.
#[test]
fn missing_paths_and_existing_stores_are_io_errors() {
    let directory = tempfile::tempdir().expect("tempdir");
    let missing = directory.path().join("missing.artrie");
    let bytes = path_bytes(&missing);
    let sentinel = 0xDEAD_BEEFusize as *mut LdictDictionary;
    let mut handle = sentinel;
    let status = unsafe {
        ldict_persistent_artrie_open(DOMAIN_BYTE, bytes.as_ptr(), bytes.len(), &mut handle)
    };
    assert_eq!(status, LdictStatus::IoError);
    assert!(handle.is_null(), "failed open must null its out-param");
    assert!(!last_error().is_empty());

    let missing_vocab = directory.path().join("missing.vocab");
    let bytes = path_bytes(&missing_vocab);
    let mut handle = sentinel;
    let status = unsafe { ldict_persistent_vocab_open(bytes.as_ptr(), bytes.len(), &mut handle) };
    assert_eq!(status, LdictStatus::IoError);
    assert!(handle.is_null());

    // create-on-existing is refused rather than truncating the store.
    let existing = directory.path().join("existing.artrie");
    drop(create_artrie(&existing, DOMAIN_BYTE));
    let bytes = path_bytes(&existing);
    let mut handle = sentinel;
    let status = unsafe {
        ldict_persistent_artrie_create(DOMAIN_BYTE, bytes.as_ptr(), bytes.len(), &mut handle)
    };
    assert_eq!(status, LdictStatus::IoError);
    assert!(handle.is_null());
    assert!(last_error().contains("already exists"));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]

    /// Randomized LDICT-SNAP-4 scripts over the byte-domain ARTrie: inserts,
    /// value updates, and removals all survive checkpoint/free/reopen.
    #[test]
    fn randomized_byte_scripts_survive_checkpoint_and_reopen(
        operations in prop::collection::vec(
            (0u8..3, prop::collection::vec(any::<u8>(), 1..8), prop::option::of(any::<u64>())),
            1..24,
        ),
    ) {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("script.artrie");
        let mut model: BTreeMap<Vec<u8>, Option<u64>> = BTreeMap::new();
        {
            let dictionary = create_artrie(&path, DOMAIN_BYTE);
            for (opcode, term, value) in operations {
                match opcode {
                    0 | 1 => {
                        let (status, inserted) = insert_text(dictionary.ptr(), &term, value);
                        prop_assert_eq!(status, LdictStatus::Ok, "{}", last_error());
                        prop_assert_eq!(inserted, !model.contains_key(&term));
                        model.insert(term, value);
                    }
                    _ => {
                        let (status, removed) = remove_text(dictionary.ptr(), &term);
                        prop_assert_eq!(status, LdictStatus::Ok, "{}", last_error());
                        prop_assert_eq!(removed, model.remove(&term).is_some());
                    }
                }
            }
            prop_assert_eq!(dictionary_len(&dictionary), model.len());
            checkpoint(&dictionary);
        }
        let reopened = open_artrie(&path, DOMAIN_BYTE);
        prop_assert_eq!(dictionary_len(&reopened), model.len());
        for (term, value) in &model {
            prop_assert_eq!(get_text(reopened.ptr(), term), (LdictStatus::Ok, Some(*value)));
        }
    }
}
