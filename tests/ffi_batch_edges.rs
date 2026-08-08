//! Edge-case matrix for the two batched mutation entry points,
//! `ldict_dictionary_insert_text_batch` and `ldict_dictionary_insert_u64_batch`.
//!
//! Spec: the status/argument-discipline family is proved in
//! `formal-verification/rocq/Spec/AbiStatusMappingSpec.v` (plan obligation
//! #13); this file is the batch-surface correspondence anchor for
//! LDICT-STAT-2.
//!
//! INVARIANT-HOOK: LDICT-STAT-2 — batch argument discipline: a zero count is
//! an `Ok` no-op that never dereferences `entries`; entry validation failures
//! are PREFIX-APPLIED (every entry before the failing one is inserted, the
//! failing and following entries are not, and `out_inserted` is left
//! unwritten); the out-count counts NEWLY INSERTED terms only (value updates
//! of terms that already exist — including duplicates within one batch —
//! contribute zero).
//!
//! Prefix semantics evidence: both batch loops in `src/ffi.rs` iterate the
//! entry slice and `?`-propagate the first per-entry error out of `boundary`
//! *after* earlier iterations already called `insert_text`/`insert_u64` on
//! the shared backend, and `out_inserted.write` only happens after the loop.
//! The rejection is therefore prefix-applied, NOT atomic; these tests pin
//! exactly that behaviour.

#![cfg(feature = "ffi")]

mod ffi_common;

use ffi_common::{
    bad_optional, contains_text, contains_u64, get_text, get_u64, insert_text, last_error, none,
    text_entry, u64_entry, DictGuard, DOMAIN_BYTE, DOMAIN_U64, DOMAIN_UNICODE,
};
use libdictenstein::ffi::{
    ldict_dictionary_insert_text_batch, ldict_dictionary_insert_u64_batch, ldict_dictionary_len,
    LdictStatus, LdictTextEntry, LdictU64Entry,
};
use vinary_tree_interop::VT_RECOMMENDED_EDGE_BATCH;

fn text_batch(dictionary: &DictGuard, entries: &[LdictTextEntry]) -> (LdictStatus, usize) {
    let mut inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text_batch(
            dictionary.ptr(),
            entries.as_ptr(),
            entries.len(),
            &mut inserted,
        )
    };
    (status, inserted)
}

fn u64_batch(dictionary: &DictGuard, entries: &[LdictU64Entry]) -> (LdictStatus, usize) {
    let mut inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_u64_batch(
            dictionary.ptr(),
            entries.as_ptr(),
            entries.len(),
            &mut inserted,
        )
    };
    (status, inserted)
}

fn dictionary_len(dictionary: &DictGuard) -> usize {
    let mut len = usize::MAX;
    assert_eq!(
        unsafe { ldict_dictionary_len(dictionary.ptr(), &mut len) },
        LdictStatus::Ok
    );
    len
}

#[test]
fn zero_count_batches_are_ok_no_ops_for_null_and_valid_arrays() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);

    let mut inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text_batch(dictionary.ptr(), std::ptr::null(), 0, &mut inserted)
    };
    assert_eq!((status, inserted), (LdictStatus::Ok, 0));
    assert_eq!(last_error(), "", "a successful no-op clears the error");

    // A valid entries pointer with count 0 behaves identically.
    let unused = [text_entry(b"never", None)];
    let mut inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text_batch(dictionary.ptr(), unused.as_ptr(), 0, &mut inserted)
    };
    assert_eq!((status, inserted), (LdictStatus::Ok, 0));
    assert_eq!(dictionary_len(&dictionary), 0);

    let tokens = DictGuard::dynamic(DOMAIN_U64);
    let mut inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_u64_batch(tokens.ptr(), std::ptr::null(), 0, &mut inserted)
    };
    assert_eq!((status, inserted), (LdictStatus::Ok, 0));
}

#[test]
fn single_entry_batches_count_only_new_insertions() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    let (status, inserted) = text_batch(&dictionary, &[text_entry(b"one", Some(1))]);
    assert_eq!((status, inserted), (LdictStatus::Ok, 1));

    // Re-inserting the same term with a new value updates it but counts zero.
    let (status, inserted) = text_batch(&dictionary, &[text_entry(b"one", Some(2))]);
    assert_eq!((status, inserted), (LdictStatus::Ok, 0));
    assert_eq!(
        get_text(dictionary.ptr(), b"one"),
        (LdictStatus::Ok, Some(Some(2)))
    );
    assert_eq!(dictionary_len(&dictionary), 1);
}

#[test]
fn large_batches_cross_the_recommended_batch_size_multiple_times() {
    // >= 4x VT_RECOMMENDED_EDGE_BATCH distinct terms in one crossing.
    let count = 4 * VT_RECOMMENDED_EDGE_BATCH + 6;
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    let terms: Vec<String> = (0..count).map(|index| format!("term{index:05}")).collect();
    let entries: Vec<LdictTextEntry> = terms
        .iter()
        .enumerate()
        .map(|(index, term)| text_entry(term.as_bytes(), Some(index as u64)))
        .collect();
    let (status, inserted) = text_batch(&dictionary, &entries);
    assert_eq!((status, inserted), (LdictStatus::Ok, count));
    assert_eq!(dictionary_len(&dictionary), count);
    for (index, term) in terms.iter().enumerate().step_by(101) {
        assert_eq!(
            get_text(dictionary.ptr(), term.as_bytes()),
            (LdictStatus::Ok, Some(Some(index as u64))),
            "term {term} must round-trip"
        );
    }

    // u64 mirror.
    let tokens = DictGuard::dynamic(DOMAIN_U64);
    let sequences: Vec<[u64; 2]> = (0..count as u64).map(|index| [index, index + 1]).collect();
    let entries: Vec<LdictU64Entry> = sequences
        .iter()
        .map(|sequence| u64_entry(sequence, None))
        .collect();
    let (status, inserted) = u64_batch(&tokens, &entries);
    assert_eq!((status, inserted), (LdictStatus::Ok, count));
    assert_eq!(dictionary_len(&tokens), count);
}

#[test]
fn null_entry_arrays_with_positive_counts_are_null_pointer_failures() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    let mut inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text_batch(dictionary.ptr(), std::ptr::null(), 3, &mut inserted)
    };
    assert_eq!(status, LdictStatus::NullPointer);
    assert_eq!(
        inserted,
        usize::MAX,
        "failed batch must not write out_inserted"
    );
    assert!(!last_error().is_empty());

    let tokens = DictGuard::dynamic(DOMAIN_U64);
    let mut inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_u64_batch(tokens.ptr(), std::ptr::null(), 3, &mut inserted)
    };
    assert_eq!(status, LdictStatus::NullPointer);
    assert_eq!(inserted, usize::MAX);
}

#[test]
fn null_entry_data_mid_batch_is_prefix_applied() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    let broken = LdictTextEntry {
        data: std::ptr::null(),
        len: 4,
        value: none(),
    };
    let entries = [
        text_entry(b"kept0", Some(10)),
        text_entry(b"kept1", None),
        broken,
        text_entry(b"lost3", Some(30)),
    ];
    let mut inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text_batch(
            dictionary.ptr(),
            entries.as_ptr(),
            entries.len(),
            &mut inserted,
        )
    };
    assert_eq!(status, LdictStatus::NullPointer);
    assert_eq!(inserted, usize::MAX, "out_inserted is unwritten on failure");
    // Prefix applied: entries before the failing index are visible.
    assert_eq!(
        get_text(dictionary.ptr(), b"kept0"),
        (LdictStatus::Ok, Some(Some(10)))
    );
    assert_eq!(
        get_text(dictionary.ptr(), b"kept1"),
        (LdictStatus::Ok, Some(None))
    );
    assert_eq!(
        contains_text(dictionary.ptr(), b"lost3"),
        (LdictStatus::Ok, false)
    );
    assert_eq!(dictionary_len(&dictionary), 2);
}

#[test]
fn duplicate_terms_within_one_batch_count_once_and_last_value_wins() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    let entries = [
        text_entry(b"dup", Some(1)),
        text_entry(b"other", None),
        text_entry(b"dup", Some(2)),
        text_entry(b"dup", None),
    ];
    let (status, inserted) = text_batch(&dictionary, &entries);
    // "dup" is newly inserted exactly once; the later duplicates are updates.
    assert_eq!((status, inserted), (LdictStatus::Ok, 2));
    assert_eq!(dictionary_len(&dictionary), 2);
    // Last write wins, including the value -> absent transition.
    assert_eq!(
        get_text(dictionary.ptr(), b"dup"),
        (LdictStatus::Ok, Some(None))
    );

    let tokens = DictGuard::dynamic(DOMAIN_U64);
    let sequence = [42u64, 43];
    let entries = [u64_entry(&sequence, Some(1)), u64_entry(&sequence, Some(9))];
    let (status, inserted) = u64_batch(&tokens, &entries);
    assert_eq!((status, inserted), (LdictStatus::Ok, 1));
    assert_eq!(
        get_u64(tokens.ptr(), &sequence),
        (LdictStatus::Ok, Some(Some(9)))
    );
}

#[test]
fn mixed_value_and_valueless_entries_round_trip() {
    let dictionary = DictGuard::dynamic(DOMAIN_BYTE);
    let entries = [
        text_entry(b"valued", Some(u64::MAX)),
        text_entry(b"bare", None),
        text_entry(&[0x00, 0xFF], Some(0)),
    ];
    let (status, inserted) = text_batch(&dictionary, &entries);
    assert_eq!((status, inserted), (LdictStatus::Ok, 3));
    assert_eq!(
        get_text(dictionary.ptr(), b"valued"),
        (LdictStatus::Ok, Some(Some(u64::MAX)))
    );
    assert_eq!(
        get_text(dictionary.ptr(), b"bare"),
        (LdictStatus::Ok, Some(None))
    );
    assert_eq!(
        get_text(dictionary.ptr(), &[0x00, 0xFF]),
        (LdictStatus::Ok, Some(Some(0)))
    );
}

#[test]
fn per_entry_optional_rejection_is_prefix_applied_not_atomic() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    let entries = [
        text_entry(b"applied0", Some(1)),
        text_entry(b"applied1", None),
        LdictTextEntry {
            data: b"rejected".as_ptr(),
            len: 8,
            value: bad_optional(),
        },
        text_entry(b"skipped", Some(4)),
    ];
    let mut inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text_batch(
            dictionary.ptr(),
            entries.as_ptr(),
            entries.len(),
            &mut inserted,
        )
    };
    assert_eq!(status, LdictStatus::InvalidArgument);
    assert_eq!(inserted, usize::MAX, "out_inserted is unwritten on failure");
    // The loop applied entries 0 and 1 before rejecting entry 2: the
    // rejection is prefix-applied, not atomic (see the file header).
    assert_eq!(
        contains_text(dictionary.ptr(), b"applied0"),
        (LdictStatus::Ok, true)
    );
    assert_eq!(
        contains_text(dictionary.ptr(), b"applied1"),
        (LdictStatus::Ok, true)
    );
    assert_eq!(
        contains_text(dictionary.ptr(), b"rejected"),
        (LdictStatus::Ok, false)
    );
    assert_eq!(
        contains_text(dictionary.ptr(), b"skipped"),
        (LdictStatus::Ok, false)
    );
    assert_eq!(dictionary_len(&dictionary), 2);

    // u64 mirror.
    let tokens = DictGuard::dynamic(DOMAIN_U64);
    let kept = [1u64];
    let rejected = [2u64];
    let skipped = [3u64];
    let entries = [
        u64_entry(&kept, None),
        LdictU64Entry {
            data: rejected.as_ptr(),
            len: rejected.len(),
            value: bad_optional(),
        },
        u64_entry(&skipped, None),
    ];
    let mut inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_u64_batch(
            tokens.ptr(),
            entries.as_ptr(),
            entries.len(),
            &mut inserted,
        )
    };
    assert_eq!(status, LdictStatus::InvalidArgument);
    assert_eq!(inserted, usize::MAX);
    assert_eq!(contains_u64(tokens.ptr(), &kept), (LdictStatus::Ok, true));
    assert_eq!(
        contains_u64(tokens.ptr(), &rejected),
        (LdictStatus::Ok, false)
    );
    assert_eq!(
        contains_u64(tokens.ptr(), &skipped),
        (LdictStatus::Ok, false)
    );
}

#[test]
fn invalid_utf8_mid_batch_is_prefix_applied_on_validating_backends() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    let entries = [
        text_entry(b"fine", None),
        text_entry(&[0xFF, 0x01], None),
        text_entry(b"after", None),
    ];
    let mut inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text_batch(
            dictionary.ptr(),
            entries.as_ptr(),
            entries.len(),
            &mut inserted,
        )
    };
    assert_eq!(status, LdictStatus::InvalidUtf8);
    assert_eq!(inserted, usize::MAX);
    assert_eq!(
        contains_text(dictionary.ptr(), b"fine"),
        (LdictStatus::Ok, true)
    );
    assert_eq!(
        contains_text(dictionary.ptr(), b"after"),
        (LdictStatus::Ok, false)
    );

    // The byte-domain DynamicDAWG accepts the same bytes raw.
    let byte = DictGuard::dynamic(DOMAIN_BYTE);
    let (status, inserted) = text_batch(&byte, &entries);
    assert_eq!((status, inserted), (LdictStatus::Ok, 3));
    assert_eq!(
        contains_text(byte.ptr(), &[0xFF, 0x01]),
        (LdictStatus::Ok, true)
    );
}

#[test]
fn capability_failures_surface_identically_through_batches() {
    // A read-only DoubleArrayTrie rejects the first entry with Unsupported;
    // the prefix is empty and out_inserted stays unwritten.
    let entries = [text_entry(b"base", None)];
    let mut handle: *mut libdictenstein::ffi::LdictDictionary = std::ptr::null_mut();
    let status = unsafe {
        libdictenstein::ffi::ldict_double_array_trie_new(
            DOMAIN_UNICODE,
            entries.as_ptr(),
            entries.len(),
            &mut handle,
        )
    };
    assert_eq!(status, LdictStatus::Ok);
    let trie = DictGuard(handle);
    let batch = [text_entry(b"new", None)];
    let (status, inserted) = {
        let mut inserted = usize::MAX;
        let status = unsafe {
            ldict_dictionary_insert_text_batch(
                trie.ptr(),
                batch.as_ptr(),
                batch.len(),
                &mut inserted,
            )
        };
        (status, inserted)
    };
    assert_eq!(status, LdictStatus::Unsupported);
    assert_eq!(inserted, usize::MAX);
    assert_eq!(contains_text(trie.ptr(), b"new"), (LdictStatus::Ok, false));

    // A u64 batch against a text-domain dictionary is a DomainMismatch.
    let unicode = DictGuard::dynamic(DOMAIN_UNICODE);
    let sequence = [5u64];
    let batch = [u64_entry(&sequence, None)];
    let (status, inserted) = u64_batch(&unicode, &batch);
    assert_eq!(status, LdictStatus::DomainMismatch);
    assert_eq!(inserted, usize::MAX);

    // The SCDAWG accepts batched inserts (INSERT capability) and validates
    // UTF-8 per entry even in the byte domain.
    let scdawg = DictGuard::scdawg(DOMAIN_BYTE);
    let good = [text_entry(b"abc", Some(1)), text_entry(b"bcd", None)];
    let (status, inserted) = text_batch(&scdawg, &good);
    assert_eq!((status, inserted), (LdictStatus::Ok, 2));
    let mixed = [text_entry(b"ok", None), text_entry(&[0x80], None)];
    let (status, inserted) = {
        let mut inserted = usize::MAX;
        let status = unsafe {
            ldict_dictionary_insert_text_batch(
                scdawg.ptr(),
                mixed.as_ptr(),
                mixed.len(),
                &mut inserted,
            )
        };
        (status, inserted)
    };
    assert_eq!(status, LdictStatus::InvalidUtf8);
    assert_eq!(inserted, usize::MAX);
    assert_eq!(contains_text(scdawg.ptr(), b"ok"), (LdictStatus::Ok, true));
}

#[test]
fn empty_terms_inside_batches_are_legal() {
    let dictionary = DictGuard::dynamic(DOMAIN_UNICODE);
    let empty = LdictTextEntry {
        data: std::ptr::null(),
        len: 0,
        value: none(),
    };
    let entries = [empty, text_entry(b"tail", Some(2))];
    let (status, inserted) = text_batch(&dictionary, &entries);
    assert_eq!((status, inserted), (LdictStatus::Ok, 2));
    assert_eq!(
        contains_text(dictionary.ptr(), b""),
        (LdictStatus::Ok, true)
    );
    assert_eq!(insert_text(dictionary.ptr(), b"", None).0, LdictStatus::Ok);
}
