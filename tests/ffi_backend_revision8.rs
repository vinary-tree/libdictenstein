//! Public revision-8 ABI controls: typed suffix sources and PathMap dictionary.

#![cfg(feature = "ffi")]

mod ffi_common;

#[cfg(feature = "pathmap-backend")]
use ffi_common::{capture_snapshot, snapshot_len, walk_terms, DictGuard};
use libdictenstein::ffi::*;
use std::ptr;

fn optional(value: Option<u64>) -> LdictOptionalU64 {
    LdictOptionalU64 {
        value: value.unwrap_or(0),
        has_value: u8::from(value.is_some()),
        reserved: [0; 7],
    }
}

unsafe fn open_suffix(domain: u32) -> *mut LdictSuffixIndex {
    let mut index = ptr::null_mut();
    assert_eq!(ldict_suffix_index_new(domain, &mut index), LdictStatus::Ok);
    assert!(!index.is_null());
    index
}

unsafe fn capture(index: *mut LdictSuffixIndex) -> *mut LdictSuffixSnapshot {
    let mut view = ptr::null_mut();
    assert_eq!(
        ldict_suffix_index_snapshot(index, &mut view),
        LdictStatus::Ok
    );
    assert!(!view.is_null());
    view
}

unsafe fn insert(index: *mut LdictSuffixIndex, text: &str, value: Option<u64>) {
    assert_eq!(
        ldict_suffix_index_insert_text(index, text.as_ptr(), text.len(), optional(value)),
        LdictStatus::Ok
    );
}

unsafe fn frequency(view: *mut LdictSuffixSnapshot, pattern: &str) -> u64 {
    let mut count = u64::MAX;
    assert_eq!(
        ldict_suffix_snapshot_substring_frequency(
            view,
            pattern.as_ptr(),
            pattern.len(),
            &mut count
        ),
        LdictStatus::Ok
    );
    count
}

#[test]
fn suffix_records_are_ordered_retained_and_not_dictionary_keys() {
    unsafe {
        let index = open_suffix(1);
        let empty = capture(index);
        let mut producer = 0;
        let mut revision = u64::MAX;
        assert_eq!(
            ldict_suffix_snapshot_identity(empty, &mut producer, &mut revision),
            LdictStatus::Ok
        );
        assert_ne!(producer, 0);
        assert_eq!(revision, 0);
        for (text, value) in [
            ("aba", Some(0)),
            ("aba", None),
            ("ababa", Some(7)),
            ("", None),
        ] {
            insert(index, text, value);
        }
        let view = capture(index);
        assert_eq!(frequency(view, "aba"), 4);
        assert_eq!(frequency(view, ""), 15);
        let mut exact = 99;
        let mut substring = 99;
        assert_eq!(
            ldict_suffix_snapshot_contains_source(view, b"ba".as_ptr(), 2, &mut exact),
            LdictStatus::Ok
        );
        assert_eq!(
            ldict_suffix_snapshot_contains_substring(view, b"ba".as_ptr(), 2, &mut substring),
            LdictStatus::Ok
        );
        assert_eq!((exact, substring), (0, 1));
        let mut count = 0;
        assert_eq!(
            ldict_suffix_snapshot_source_count(view, &mut count),
            LdictStatus::Ok
        );
        assert_eq!(count, 4);
        let mut rows = [LdictSuffixSourceRecord::default(); 4];
        let mut written = 0;
        let mut total = 0;
        assert_eq!(
            ldict_suffix_snapshot_source_page(
                view,
                0,
                rows.as_mut_ptr(),
                rows.len(),
                &mut written,
                &mut total
            ),
            LdictStatus::Ok
        );
        assert_eq!((written, total), (4, 4));
        assert_eq!(
            rows.iter().map(|r| r.source_id).collect::<Vec<_>>(),
            [3, 0, 1, 2]
        );
        assert_eq!(
            std::str::from_utf8(std::slice::from_raw_parts(rows[1].data, rows[1].len)).unwrap(),
            "aba"
        );
        assert_eq!((rows[1].value.has_value, rows[1].value.value), (1, 0));
        assert_eq!(rows[2].value.has_value, 0);
        let mut removed = 0;
        assert_eq!(
            ldict_suffix_index_remove_text(index, b"aba".as_ptr(), 3, &mut removed),
            LdictStatus::Ok
        );
        assert_eq!(removed, 1);
        let later = capture(index);
        let mut later_rows = [LdictSuffixSourceRecord::default(); 3];
        assert_eq!(
            ldict_suffix_snapshot_source_page(
                later,
                0,
                later_rows.as_mut_ptr(),
                3,
                &mut written,
                &mut total
            ),
            LdictStatus::Ok
        );
        assert_eq!(
            later_rows.iter().map(|r| r.source_id).collect::<Vec<_>>(),
            [3, 1, 2]
        );
        assert_eq!(frequency(later, "aba"), 3);
        assert_eq!(frequency(view, "aba"), 4);
        assert_eq!(ldict_suffix_index_clear(index), LdictStatus::Ok);
        insert(index, "new", None);
        let after_clear = capture(index);
        let mut reused = [LdictSuffixSourceRecord::default(); 1];
        assert_eq!(
            ldict_suffix_snapshot_source_page(
                after_clear,
                0,
                reused.as_mut_ptr(),
                1,
                &mut written,
                &mut total
            ),
            LdictStatus::Ok
        );
        assert_eq!(reused[0].source_id, 0, "source IDs may restart after clear");
        assert_eq!(frequency(view, "aba"), 4);
        assert_eq!(ldict_suffix_index_close(index), LdictStatus::Ok);
        assert_eq!(ldict_suffix_index_close(index), LdictStatus::Closed);
        let mut failed = 123;
        assert_eq!(
            ldict_suffix_index_remove_text(index, b"x".as_ptr(), 1, &mut failed),
            LdictStatus::Closed
        );
        assert_eq!(failed, 0);
        assert_eq!(frequency(view, "aba"), 4);
        assert_eq!(ldict_suffix_snapshot_close(view), LdictStatus::Ok);
        assert_eq!(ldict_suffix_snapshot_close(view), LdictStatus::Closed);
        let mut count = 123;
        assert_eq!(
            ldict_suffix_snapshot_source_count(view, &mut count),
            LdictStatus::Closed
        );
        assert_eq!(count, 0);
        ldict_suffix_snapshot_free(view);
        ldict_suffix_snapshot_free(later);
        ldict_suffix_snapshot_free(after_clear);
        ldict_suffix_snapshot_free(empty);
        ldict_suffix_index_free(index);
    }
}

#[test]
fn suffix_source_record_layout_and_output_boundaries_are_exact() {
    use std::mem::{offset_of, size_of};

    let pointer_size = size_of::<*const u8>();
    assert_eq!(
        size_of::<LdictSuffixSourceRecord>(),
        8 + pointer_size + size_of::<usize>() + size_of::<LdictOptionalU64>()
    );
    assert_eq!(offset_of!(LdictSuffixSourceRecord, source_id), 0);
    assert_eq!(offset_of!(LdictSuffixSourceRecord, data), 8);
    assert_eq!(offset_of!(LdictSuffixSourceRecord, len), 8 + pointer_size);
    assert_eq!(
        offset_of!(LdictSuffixSourceRecord, value),
        8 + pointer_size + size_of::<usize>()
    );
    unsafe {
        let index = open_suffix(1);
        insert(index, "aba", Some(0));
        let view = capture(index);
        let mut producer = 99;
        assert_eq!(
            ldict_suffix_snapshot_identity(view, &mut producer, ptr::null_mut()),
            LdictStatus::NullPointer
        );
        assert_eq!(producer, 0);
        let mut revision = 99;
        assert_eq!(
            ldict_suffix_snapshot_identity(ptr::null(), &mut producer, &mut revision),
            LdictStatus::NullPointer
        );
        assert_eq!((producer, revision), (0, 0));

        let mut row = [LdictSuffixSourceRecord::default(); 1];
        row[0].source_id = 99;
        let mut written = 99;
        let mut total = 99;
        assert_eq!(
            ldict_suffix_snapshot_source_page(
                ptr::null(),
                0,
                row.as_mut_ptr(),
                1,
                &mut written,
                &mut total
            ),
            LdictStatus::NullPointer
        );
        assert_eq!((row[0].source_id, written, total), (0, 0, 0));
        row[0].source_id = 99;
        total = 99;
        assert_eq!(
            ldict_suffix_snapshot_source_page(
                view,
                0,
                row.as_mut_ptr(),
                1,
                ptr::null_mut(),
                &mut total
            ),
            LdictStatus::NullPointer
        );
        assert_eq!((row[0].source_id, total), (0, 0));
        written = 99;
        total = 99;
        assert_eq!(
            ldict_suffix_snapshot_source_page(
                view,
                0,
                ptr::null_mut(),
                1,
                &mut written,
                &mut total
            ),
            LdictStatus::NullPointer
        );
        assert_eq!((written, total), (0, 0));
        ldict_suffix_snapshot_free(view);
        ldict_suffix_index_free(index);
    }
}

#[test]
fn suffix_domains_empty_pattern_and_fail_closed_outputs() {
    unsafe {
        for (domain, expected) in [(1, 10), (2, 5)] {
            let index = open_suffix(domain);
            insert(index, "é🙂é", None);
            insert(index, "", None);
            let view = capture(index);
            assert_eq!(frequency(view, ""), expected);
            assert_eq!(frequency(view, "é"), 2);
            let mut result = 99;
            assert_eq!(
                ldict_suffix_snapshot_contains_source(view, b"\xc3".as_ptr(), 1, &mut result),
                LdictStatus::InvalidUtf8
            );
            assert_eq!(result, 0);
            assert_eq!(
                ldict_suffix_snapshot_contains_source(view, ptr::null(), 0, &mut result),
                LdictStatus::Ok
            );
            assert_eq!(result, 1);
            assert_eq!(
                ldict_suffix_index_insert_text(index, b"\xff".as_ptr(), 1, optional(None)),
                LdictStatus::InvalidUtf8
            );
            let mut invalid = optional(Some(0));
            invalid.has_value = 2;
            assert_eq!(
                ldict_suffix_index_insert_text(index, b"x".as_ptr(), 1, invalid),
                LdictStatus::InvalidArgument
            );
            let mut rows = [LdictSuffixSourceRecord::default(); 1];
            let mut written = 77;
            let mut total = 77;
            assert_eq!(
                ldict_suffix_snapshot_source_page(
                    view,
                    u64::MAX,
                    rows.as_mut_ptr(),
                    1,
                    &mut written,
                    &mut total
                ),
                LdictStatus::End
            );
            assert_eq!((written, total), (0, 2));
            ldict_suffix_snapshot_free(view);
            ldict_suffix_index_free(index);
        }
        let mut index = ptr::dangling_mut::<LdictSuffixIndex>();
        assert_eq!(
            ldict_suffix_index_new(3, &mut index),
            LdictStatus::Unsupported
        );
        assert!(index.is_null());
        assert_eq!(
            ldict_suffix_index_new(1, ptr::null_mut()),
            LdictStatus::NullPointer
        );
        let mut out = 123;
        assert_eq!(
            ldict_suffix_snapshot_source_count(ptr::null(), &mut out),
            LdictStatus::NullPointer
        );
        assert_eq!(out, 0);
    }
}

#[test]
fn pathmap_constructor_is_feature_gated_and_byte_exact() {
    unsafe {
        let mut dictionary = ptr::dangling_mut::<LdictDictionary>();
        let status = ldict_pathmap_new(1, &mut dictionary);
        #[cfg(not(feature = "pathmap-backend"))]
        {
            assert_eq!(status, LdictStatus::Unsupported);
            assert!(dictionary.is_null());
        }
        #[cfg(feature = "pathmap-backend")]
        {
            assert_eq!(status, LdictStatus::Ok);
            let key = [0, 0xff, 0x80];
            let mut inserted = 0;
            assert_eq!(
                ldict_dictionary_insert_text(
                    dictionary,
                    key.as_ptr(),
                    key.len(),
                    optional(None),
                    &mut inserted
                ),
                LdictStatus::Ok
            );
            assert_eq!(inserted, 1);
            let mut contains = 0;
            assert_eq!(
                ldict_dictionary_contains_text(dictionary, key.as_ptr(), key.len(), &mut contains),
                LdictStatus::Ok
            );
            assert_eq!(contains, 1);
            let mut found = 0;
            let mut value = optional(Some(99));
            assert_eq!(
                ldict_dictionary_get_text(
                    dictionary,
                    key.as_ptr(),
                    key.len(),
                    &mut found,
                    &mut value
                ),
                LdictStatus::Ok
            );
            assert_eq!((found, value.has_value), (1, 0));
            assert_eq!(
                ldict_dictionary_insert_text(
                    dictionary,
                    key.as_ptr(),
                    key.len(),
                    optional(Some(0)),
                    &mut inserted
                ),
                LdictStatus::Ok
            );
            assert_eq!(inserted, 0);
            assert_eq!(
                ldict_dictionary_get_text(
                    dictionary,
                    key.as_ptr(),
                    key.len(),
                    &mut found,
                    &mut value
                ),
                LdictStatus::Ok
            );
            assert_eq!((found, value.has_value, value.value), (1, 1, 0));
            let mut removed = 0;
            assert_eq!(
                ldict_dictionary_remove_text(dictionary, key.as_ptr(), key.len(), &mut removed),
                LdictStatus::Ok
            );
            assert_eq!(removed, 1);
            assert_eq!(
                ldict_dictionary_contains_text(dictionary, key.as_ptr(), key.len(), &mut contains),
                LdictStatus::Ok
            );
            assert_eq!(contains, 0);
            ldict_dictionary_free(dictionary);
        }
    }
}

#[cfg(feature = "pathmap-backend")]
#[test]
fn pathmap_batch_is_atomic_and_retained_snapshot_is_immutable() {
    unsafe {
        let mut pointer = ptr::null_mut();
        assert_eq!(ldict_pathmap_new(1, &mut pointer), LdictStatus::Ok);
        let dictionary = DictGuard(pointer);
        let mut kind = 0;
        let mut capabilities = 0;
        assert_eq!(ldict_dictionary_kind(pointer, &mut kind), LdictStatus::Ok);
        assert_eq!(kind, LDICT_KIND_PATHMAP);
        assert_eq!(
            ldict_dictionary_capabilities(pointer, &mut capabilities),
            LdictStatus::Ok
        );
        assert_eq!(
            capabilities,
            LDICT_CAP_READ | LDICT_CAP_INSERT | LDICT_CAP_REMOVE | LDICT_CAP_CLEAR
        );
        let keys: [&[u8]; 3] = [b"", &[0xff, 0x00], &[0x80, 0xc0]];
        let entries = keys.map(|key| LdictTextEntry {
            data: key.as_ptr(),
            len: key.len(),
            value: optional(Some(0)),
        });
        let mut inserted = usize::MAX;
        assert_eq!(
            ldict_dictionary_insert_text_batch(
                pointer,
                entries.as_ptr(),
                entries.len(),
                &mut inserted
            ),
            LdictStatus::Ok
        );
        assert_eq!(inserted, 3);
        let view = capture_snapshot(dictionary.resource());
        assert_eq!(snapshot_len(view.resource), (3, true));
        let original = walk_terms(view.resource, 8);
        assert_eq!(original.len(), 3);
        assert_eq!(original.get(&vec![0xff, 0x00]), Some(&Some(0)));
        let bad = [
            LdictTextEntry {
                data: b"new".as_ptr(),
                len: 3,
                value: optional(None),
            },
            LdictTextEntry {
                data: b"bad".as_ptr(),
                len: 3,
                value: LdictOptionalU64 {
                    value: 0,
                    has_value: 2,
                    reserved: [0; 7],
                },
            },
        ];
        inserted = usize::MAX;
        assert_eq!(
            ldict_dictionary_insert_text_batch(pointer, bad.as_ptr(), 2, &mut inserted),
            LdictStatus::InvalidArgument
        );
        assert_eq!(
            inserted,
            usize::MAX,
            "existing batch ABI leaves failure output untouched"
        );
        let mut contains = 1;
        assert_eq!(
            ldict_dictionary_contains_text(pointer, b"new".as_ptr(), 3, &mut contains),
            LdictStatus::Ok
        );
        assert_eq!(contains, 0, "invalid batch must not partially publish");
        assert_eq!(ldict_dictionary_clear(pointer), LdictStatus::Ok);
        drop(dictionary);
        assert_eq!(
            walk_terms(view.resource, 8),
            original,
            "retained view outlives source handle"
        );
    }
}

#[cfg(feature = "pathmap-backend")]
#[test]
fn pathmap_rejects_u64_and_invalid_unicode_preserving_generic_failure_output() {
    unsafe {
        let mut pointer = ptr::dangling_mut::<LdictDictionary>();
        assert_eq!(ldict_pathmap_new(3, &mut pointer), LdictStatus::Unsupported);
        assert!(pointer.is_null());
        assert_eq!(ldict_pathmap_new(2, &mut pointer), LdictStatus::Ok);
        let dictionary = DictGuard(pointer);
        let mut inserted = 1;
        assert_eq!(
            ldict_dictionary_insert_text(
                pointer,
                b"\xff".as_ptr(),
                1,
                optional(None),
                &mut inserted
            ),
            LdictStatus::InvalidUtf8
        );
        assert_eq!(
            inserted, 1,
            "old generic insert ABI preserves failure output"
        );
        let good = "é".as_bytes();
        assert_eq!(
            ldict_dictionary_insert_text(
                pointer,
                good.as_ptr(),
                good.len(),
                optional(None),
                &mut inserted
            ),
            LdictStatus::Ok
        );
        assert_eq!(inserted, 1);
        drop(dictionary);
    }
}

#[cfg(feature = "pathmap-backend")]
#[test]
fn pathmap_ffi_snapshots_survive_concurrent_public_mutation() {
    unsafe {
        let mut pointer = ptr::null_mut();
        assert_eq!(ldict_pathmap_new(1, &mut pointer), LdictStatus::Ok);
        let dictionary = DictGuard(pointer);
        let base = [0xff, 0x00];
        let mut inserted = 0;
        assert_eq!(
            ldict_dictionary_insert_text(
                pointer,
                base.as_ptr(),
                base.len(),
                optional(Some(0)),
                &mut inserted
            ),
            LdictStatus::Ok
        );
        let address = pointer as usize;
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let pointer = address as *mut LdictDictionary;
                for index in 0..64u8 {
                    let key = [0x80, index];
                    let mut inserted = 0;
                    assert_eq!(
                        ldict_dictionary_insert_text(
                            pointer,
                            key.as_ptr(),
                            key.len(),
                            optional(None),
                            &mut inserted,
                        ),
                        LdictStatus::Ok
                    );
                }
            });
            for _ in 0..4 {
                scope.spawn(move || {
                    let pointer = address as *mut LdictDictionary;
                    for _ in 0..32 {
                        let mut resource = vinary_tree_interop::VtResource::NULL;
                        assert_eq!(
                            ldict_dictionary_resource(pointer, &mut resource),
                            LdictStatus::Ok
                        );
                        let view = capture_snapshot(resource);
                        let terms = walk_terms(view.resource, 8);
                        assert_eq!(terms.get(&vec![0xff, 0x00]), Some(&Some(0)));
                    }
                });
            }
        });
        let final_view = capture_snapshot(dictionary.resource());
        assert_eq!(snapshot_len(final_view.resource), (65, true));
    }
}
