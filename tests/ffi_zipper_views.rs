//! Direct ABI laws for native prefix pruning and changed optional values.
#![cfg(feature = "ffi")]

mod ffi_common;

use ffi_common::{insert_text, DictGuard, DOMAIN_BYTE, DOMAIN_UNICODE};
use libdictenstein::ffi::{
    ldict_dictionary_prefix_entries_open, ldict_dictionary_value_diff_open,
    ldict_entry_cursor_free, ldict_entry_cursor_next, ldict_entry_cursor_release,
    ldict_value_diff_cursor_cancel, ldict_value_diff_cursor_free, ldict_value_diff_cursor_next,
    ldict_value_diff_cursor_release, LdictEntriesInfo, LdictEntryBatch, LdictEntryBatchLimits,
    LdictEntryCursor, LdictStatus, LdictUnitKey, LdictValueDiffBatch, LdictValueDiffCursor,
};
use std::ptr;

#[test]
fn prefix_cursor_seeks_and_prunes_binary_and_unicode_paths() {
    let dictionary = DictGuard::dynamic(DOMAIN_BYTE);
    for key in [
        b"ap\xff".as_slice(),
        b"app".as_slice(),
        b"ape".as_slice(),
        b"bee".as_slice(),
    ] {
        assert_eq!(insert_text(dictionary.ptr(), key, None).0, LdictStatus::Ok);
    }
    let include = b"ap";
    let exclude = b"app";
    let include = LdictUnitKey {
        data: include.as_ptr().cast(),
        len: include.len(),
    };
    let exclude = LdictUnitKey {
        data: exclude.as_ptr().cast(),
        len: exclude.len(),
    };
    let mut cursor: *mut LdictEntryCursor = ptr::null_mut();
    let mut info = LdictEntriesInfo::default();
    assert_eq!(
        unsafe {
            ldict_dictionary_prefix_entries_open(
                dictionary.ptr(),
                &include,
                &exclude,
                1,
                &mut cursor,
                &mut info,
            )
        },
        LdictStatus::Ok
    );
    assert_eq!(info.unit_domain, DOMAIN_BYTE);
    let limits = LdictEntryBatchLimits {
        max_entries: 1,
        max_units: 8,
        max_values: 1,
        reserved: 0,
    };
    let mut keys = Vec::new();
    loop {
        let mut batch = LdictEntryBatch::default();
        match unsafe { ldict_entry_cursor_next(cursor, &limits, &mut batch) } {
            LdictStatus::End => break,
            LdictStatus::Ok => {}
            status => panic!("prefix next returned {status:?}"),
        }
        let descriptor = unsafe { &*batch.entries };
        keys.push(
            unsafe {
                std::slice::from_raw_parts(
                    batch.units.cast::<u8>().add(descriptor.unit_offset),
                    descriptor.unit_len,
                )
            }
            .to_vec(),
        );
        assert_eq!(
            unsafe { ldict_entry_cursor_release(cursor, batch.generation) },
            LdictStatus::Ok
        );
    }
    assert_eq!(keys, [b"ape".to_vec(), b"ap\xff".to_vec()]);
    assert_eq!(unsafe { ldict_entry_cursor_free(cursor) }, LdictStatus::Ok);

    let unicode = DictGuard::dynamic(DOMAIN_UNICODE);
    assert_eq!(
        insert_text(unicode.ptr(), "école".as_bytes(), None).0,
        LdictStatus::Ok
    );
    let invalid = [0xffu8];
    let bad = LdictUnitKey {
        data: invalid.as_ptr().cast(),
        len: 1,
    };
    let mut cursor = ptr::null_mut();
    assert_eq!(
        unsafe {
            ldict_dictionary_prefix_entries_open(
                unicode.ptr(),
                &bad,
                ptr::null(),
                0,
                &mut cursor,
                &mut info,
            )
        },
        LdictStatus::InvalidUtf8
    );
    assert!(cursor.is_null());
}

#[test]
fn value_diff_pages_preserve_optional_values_and_lease_rules() {
    let left = DictGuard::dynamic(DOMAIN_BYTE);
    let right = DictGuard::dynamic(DOMAIN_BYTE);
    for (key, value) in [
        (b"changed".as_slice(), None),
        (b"same".as_slice(), Some(3)),
        (b"zero".as_slice(), Some(0)),
    ] {
        assert_eq!(insert_text(left.ptr(), key, value).0, LdictStatus::Ok);
    }
    for (key, value) in [
        (b"changed".as_slice(), Some(7)),
        (b"same".as_slice(), Some(3)),
        (b"zero".as_slice(), None),
    ] {
        assert_eq!(insert_text(right.ptr(), key, value).0, LdictStatus::Ok);
    }
    let mut cursor: *mut LdictValueDiffCursor = ptr::null_mut();
    let mut domain = 0;
    assert_eq!(
        unsafe {
            ldict_dictionary_value_diff_open(left.ptr(), right.ptr(), &mut cursor, &mut domain)
        },
        LdictStatus::Ok
    );
    assert_eq!(domain, DOMAIN_BYTE);
    drop(left);
    drop(right);
    let too_small = LdictEntryBatchLimits {
        max_entries: 1,
        max_units: 1,
        max_values: 2,
        reserved: 0,
    };
    let mut batch = LdictValueDiffBatch::default();
    assert_eq!(
        unsafe { ldict_value_diff_cursor_next(cursor, &too_small, &mut batch) },
        LdictStatus::LimitExceeded
    );
    assert!(batch.entries.is_null());
    let limits = LdictEntryBatchLimits {
        max_entries: 1,
        max_units: 16,
        max_values: 2,
        reserved: 0,
    };
    assert_eq!(
        unsafe { ldict_value_diff_cursor_next(cursor, &limits, &mut batch) },
        LdictStatus::Ok
    );
    let entry = unsafe { &*batch.entries };
    assert_eq!(
        unsafe { std::slice::from_raw_parts(batch.units.cast::<u8>(), entry.unit_len) },
        b"changed"
    );
    assert_eq!(entry.left.has_value, 0);
    assert_eq!(entry.right.has_value, 1);
    assert_eq!(entry.right.value, 7);
    let generation = batch.generation;
    assert_eq!(
        unsafe { ldict_value_diff_cursor_next(cursor, &limits, &mut batch) },
        LdictStatus::BatchInUse
    );
    assert_eq!(
        unsafe { ldict_value_diff_cursor_free(cursor) },
        LdictStatus::BatchInUse
    );
    assert_eq!(
        unsafe { ldict_value_diff_cursor_release(cursor, generation + 1) },
        LdictStatus::InvalidArgument
    );
    assert_eq!(
        unsafe { ldict_value_diff_cursor_release(cursor, generation) },
        LdictStatus::Ok
    );
    assert_eq!(
        unsafe { ldict_value_diff_cursor_next(cursor, &limits, &mut batch) },
        LdictStatus::Ok
    );
    let entry = unsafe { &*batch.entries };
    assert_eq!(
        unsafe { std::slice::from_raw_parts(batch.units.cast::<u8>(), entry.unit_len) },
        b"zero"
    );
    assert_eq!(entry.left.has_value, 1);
    assert_eq!(entry.left.value, 0);
    assert_eq!(entry.right.has_value, 0);
    assert_eq!(
        unsafe { ldict_value_diff_cursor_cancel(cursor) },
        LdictStatus::Ok
    );
    assert_eq!(
        unsafe { ldict_value_diff_cursor_release(cursor, batch.generation) },
        LdictStatus::Ok
    );
    assert_eq!(
        unsafe { ldict_value_diff_cursor_next(cursor, &limits, &mut batch) },
        LdictStatus::End
    );
    assert_eq!(
        unsafe { ldict_value_diff_cursor_free(cursor) },
        LdictStatus::Ok
    );
}
