//! Correspondence and lifecycle laws for `vt.dict.entry.v1`.

#![cfg(feature = "ffi")]

mod ffi_common;

use ffi_common::{insert_text, insert_u64, vt_status, DictGuard, DOMAIN_BYTE, DOMAIN_U64};
use std::ffi::c_void;
use vinary_tree_interop::{
    dictionary_entries_info_flags, VtDictionaryEntriesCursor, VtDictionaryEntriesInfo,
    VtDictionaryEntriesVTable, VtDictionaryEntryBatchLimits, VtDictionaryEntryBatchView,
    VtDictionaryEntryOrder, VtStatus, VtValueDomain, VT_DICTIONARY_ENTRIES_INTERFACE_ID,
    VT_DICTIONARY_ENTRIES_INTERFACE_VERSION,
};

struct CursorGuard {
    cursor: VtDictionaryEntriesCursor,
    vtable: *const VtDictionaryEntriesVTable,
}

impl Drop for CursorGuard {
    fn drop(&mut self) {
        if !self.cursor.is_null() {
            let status = unsafe { ((*self.vtable).close.unwrap())(&mut self.cursor) };
            assert_eq!(vt_status(status), VtStatus::Ok);
        }
    }
}

fn open(dictionary: &DictGuard) -> (CursorGuard, VtDictionaryEntriesInfo) {
    let resource = dictionary.resource();
    let mut interface: *const c_void = std::ptr::null();
    let status = unsafe {
        ((*resource.vtable).query_interface.unwrap())(
            resource.context,
            &VT_DICTIONARY_ENTRIES_INTERFACE_ID,
            VT_DICTIONARY_ENTRIES_INTERFACE_VERSION,
            &mut interface,
        )
    };
    assert_eq!(vt_status(status), VtStatus::Ok);
    let vtable = interface.cast::<VtDictionaryEntriesVTable>();
    let mut cursor = VtDictionaryEntriesCursor::NULL;
    let mut info = VtDictionaryEntriesInfo::default();
    let status = unsafe { ((*vtable).open.unwrap())(resource.context, &mut cursor, &mut info) };
    assert_eq!(vt_status(status), VtStatus::Ok);
    assert!(!cursor.is_null());
    (CursorGuard { cursor, vtable }, info)
}

fn limits(entries: usize, units: usize, values: usize) -> VtDictionaryEntryBatchLimits {
    VtDictionaryEntryBatchLimits {
        max_entries: entries,
        max_units: units,
        max_values: values,
        reserved: 0,
    }
}

fn next(
    cursor: &mut CursorGuard,
    limits: &VtDictionaryEntryBatchLimits,
) -> (VtStatus, VtDictionaryEntryBatchView) {
    let mut batch = VtDictionaryEntryBatchView::default();
    let status =
        unsafe { ((*cursor.vtable).next_batch.unwrap())(&mut cursor.cursor, limits, &mut batch) };
    (vt_status(status), batch)
}

fn release(cursor: &mut CursorGuard, generation: u64) -> VtStatus {
    vt_status(unsafe { ((*cursor.vtable).release_batch.unwrap())(&mut cursor.cursor, generation) })
}

unsafe fn raw_slice<'a, T>(pointer: *const T, len: usize) -> &'a [T] {
    if len == 0 {
        &[]
    } else {
        assert!(!pointer.is_null());
        // SAFETY: a live batch owns `len` contiguous elements until release.
        unsafe { std::slice::from_raw_parts(pointer, len) }
    }
}

#[test]
// INVARIANT-HOOK: LDICT-ENTRY-1
// INVARIANT-HOOK: LDICT-ENTRY-3..5
fn byte_batches_are_lexicographic_lossless_and_snapshot_owned() {
    let dictionary = DictGuard::dynamic(DOMAIN_BYTE);
    for (key, value) in [
        (&b"beta"[..], None),
        (&b"alpha"[..], Some(0)),
        (&b"alphabet"[..], Some(u64::MAX)),
        (&b"\0\xff"[..], None),
        (&b""[..], Some(9)),
    ] {
        assert_eq!(insert_text(dictionary.ptr(), key, value).0 as u32, 0);
    }

    let (mut cursor, info) = open(&dictionary);
    assert_eq!(info.unit_domain, DOMAIN_BYTE);
    assert_eq!(info.value_domain, VtValueDomain::OptionalU64 as u32);
    assert_eq!(info.order, VtDictionaryEntryOrder::Lexicographic as u32);
    assert_eq!(
        info.flags
            & (dictionary_entries_info_flags::EXACT_LEN
                | dictionary_entries_info_flags::SNAPSHOT_IDENTITY),
        dictionary_entries_info_flags::EXACT_LEN | dictionary_entries_info_flags::SNAPSHOT_IDENTITY
    );
    assert_eq!(info.exact_len, 5);

    // Mutation after open must not enter this captured cursor.
    assert_eq!(insert_text(dictionary.ptr(), b"gamma", Some(3)).0 as u32, 0);

    let mut got = Vec::new();
    loop {
        let (status, batch) = next(&mut cursor, &limits(2, 16, 2));
        if status == VtStatus::End {
            assert_eq!(batch.entry_count, 0);
            assert_eq!(batch.generation, 0);
            break;
        }
        assert_eq!(status, VtStatus::Ok);
        let entries = unsafe { raw_slice(batch.entries, batch.entry_count) };
        let units = unsafe { raw_slice(batch.units.cast::<u8>(), batch.unit_count) };
        let values = unsafe { raw_slice(batch.values, batch.value_count) };
        for entry in entries {
            let key = units[entry.unit_offset..entry.unit_offset + entry.unit_len].to_vec();
            let value = match entry.value_len {
                0 => None,
                1 => Some(values[entry.value_offset]),
                other => panic!("invalid optional-u64 width {other}"),
            };
            got.push((key, value));
        }

        let (blocked, canonical) = next(&mut cursor, &limits(1, 8, 1));
        assert_eq!(blocked, VtStatus::BatchInUse);
        assert_eq!(canonical.entry_count, 0);
        assert_eq!(release(&mut cursor, batch.generation), VtStatus::Ok);
        assert_eq!(
            release(&mut cursor, batch.generation),
            VtStatus::InvalidArgument
        );
    }
    assert_eq!(
        got,
        vec![
            (b"".to_vec(), Some(9)),
            (b"\0\xff".to_vec(), None),
            (b"alpha".to_vec(), Some(0)),
            (b"alphabet".to_vec(), Some(u64::MAX)),
            (b"beta".to_vec(), None),
        ]
    );
    assert_eq!(next(&mut cursor, &limits(1, 1, 1)).0, VtStatus::End);
}

#[test]
// INVARIANT-HOOK: LDICT-ENTRY-2
fn oversize_first_entry_is_retryable_without_advancing() {
    let dictionary = DictGuard::dynamic(DOMAIN_BYTE);
    assert_eq!(insert_text(dictionary.ptr(), b"large", Some(7)).0 as u32, 0);
    let (mut cursor, _) = open(&dictionary);

    let (status, batch) = next(&mut cursor, &limits(1, 4, 1));
    assert_eq!(status, VtStatus::LimitExceeded);
    assert_eq!(batch.entry_count, 0);

    let (status, batch) = next(&mut cursor, &limits(1, 5, 1));
    assert_eq!(status, VtStatus::Ok);
    let units = unsafe { raw_slice(batch.units.cast::<u8>(), batch.unit_count) };
    assert_eq!(units, b"large");
    assert_eq!(release(&mut cursor, batch.generation), VtStatus::Ok);
    assert_eq!(next(&mut cursor, &limits(1, 5, 1)).0, VtStatus::End);
}

#[test]
fn u64_units_keep_numeric_lexicographic_order() {
    let dictionary = DictGuard::dynamic(DOMAIN_U64);
    for (key, value) in [
        (&[u64::MAX][..], None),
        (&[1, 9][..], Some(19)),
        (&[1][..], Some(1)),
        (&[2][..], Some(2)),
    ] {
        assert_eq!(insert_u64(dictionary.ptr(), key, value).0 as u32, 0);
    }
    let (mut cursor, info) = open(&dictionary);
    assert_eq!(info.unit_domain, DOMAIN_U64);
    let mut keys = Vec::new();
    loop {
        let (status, batch) = next(&mut cursor, &limits(8, 32, 8));
        if status == VtStatus::End {
            break;
        }
        assert_eq!(status, VtStatus::Ok);
        let entries = unsafe { raw_slice(batch.entries, batch.entry_count) };
        let units = unsafe { raw_slice(batch.units.cast::<u64>(), batch.unit_count) };
        for entry in entries {
            keys.push(units[entry.unit_offset..entry.unit_offset + entry.unit_len].to_vec());
        }
        assert_eq!(release(&mut cursor, batch.generation), VtStatus::Ok);
    }
    assert_eq!(keys, vec![vec![1], vec![1, 9], vec![2], vec![u64::MAX]]);
}

#[test]
// INVARIANT-HOOK: LDICT-ENTRY-6..7
fn cancel_is_idempotent_and_close_refuses_a_live_lease() {
    let dictionary = DictGuard::dynamic(DOMAIN_BYTE);
    assert_eq!(insert_text(dictionary.ptr(), b"a", None).0 as u32, 0);
    let (mut cursor, _) = open(&dictionary);
    let (status, batch) = next(&mut cursor, &limits(1, 1, 1));
    assert_eq!(status, VtStatus::Ok);
    assert_eq!(
        vt_status(unsafe { ((*cursor.vtable).cancel.unwrap())(&mut cursor.cursor) }),
        VtStatus::Ok
    );
    assert_eq!(
        vt_status(unsafe { ((*cursor.vtable).close.unwrap())(&mut cursor.cursor) }),
        VtStatus::BatchInUse
    );
    assert_eq!(release(&mut cursor, batch.generation), VtStatus::Ok);
    assert_eq!(next(&mut cursor, &limits(1, 1, 1)).0, VtStatus::End);
    assert_eq!(
        vt_status(unsafe { ((*cursor.vtable).cancel.unwrap())(&mut cursor.cursor) }),
        VtStatus::Ok
    );
}

#[test]
fn scdawg_entry_stream_enumerates_stored_terms_not_internal_substrings() {
    let dictionary = DictGuard::scdawg(DOMAIN_BYTE);
    for (key, value) in [
        (&b"banana"[..], Some(1)),
        (&b"band"[..], None),
        (&b"an"[..], Some(2)),
    ] {
        assert_eq!(insert_text(dictionary.ptr(), key, value).0 as u32, 0);
    }
    let (mut cursor, info) = open(&dictionary);
    assert_eq!(info.exact_len, 3);
    let mut keys = Vec::new();
    loop {
        let (status, batch) = next(&mut cursor, &limits(8, 64, 8));
        if status == VtStatus::End {
            break;
        }
        assert_eq!(status, VtStatus::Ok);
        let entries = unsafe { raw_slice(batch.entries, batch.entry_count) };
        let units = unsafe { raw_slice(batch.units.cast::<u8>(), batch.unit_count) };
        for entry in entries {
            keys.push(units[entry.unit_offset..entry.unit_offset + entry.unit_len].to_vec());
        }
        assert_eq!(release(&mut cursor, batch.generation), VtStatus::Ok);
    }
    assert_eq!(
        keys,
        vec![b"an".to_vec(), b"banana".to_vec(), b"band".to_vec()]
    );
}

struct ReducerProbe {
    cursor: *mut VtDictionaryEntriesCursor,
    vtable: *const VtDictionaryEntriesVTable,
    calls: usize,
    reentry_status: Option<VtStatus>,
}

unsafe extern "C" fn stop_after_one_batch(
    context: *mut c_void,
    batch: *const VtDictionaryEntryBatchView,
) -> u32 {
    let probe = unsafe { &mut *context.cast::<ReducerProbe>() };
    let batch = unsafe { &*batch };
    assert!(batch.entry_count > 0);
    probe.calls += 1;
    let mut nested = VtDictionaryEntryBatchView::default();
    let nested_limits = limits(1, 8, 1);
    let raw =
        unsafe { ((*probe.vtable).next_batch.unwrap())(probe.cursor, &nested_limits, &mut nested) };
    probe.reentry_status = Some(vt_status(raw));
    VtStatus::End.to_raw()
}

#[test]
// INVARIANT-HOOK: LDICT-ENTRY-8
fn reducer_settles_its_lease_and_refuses_callback_reentry() {
    let dictionary = DictGuard::dynamic(DOMAIN_BYTE);
    for key in [b"a", b"b", b"c"] {
        assert_eq!(insert_text(dictionary.ptr(), key, Some(1)).0 as u32, 0);
    }
    let (mut cursor, _) = open(&dictionary);
    let mut probe = ReducerProbe {
        cursor: &mut cursor.cursor,
        vtable: cursor.vtable,
        calls: 0,
        reentry_status: None,
    };
    let mut count = usize::MAX;
    let raw = unsafe {
        ((*cursor.vtable).reduce.unwrap())(
            &mut cursor.cursor,
            &limits(2, 8, 2),
            Some(stop_after_one_batch),
            (&mut probe as *mut ReducerProbe).cast(),
            &mut count,
        )
    };
    assert_eq!(vt_status(raw), VtStatus::Ok);
    assert_eq!(probe.calls, 1);
    assert_eq!(probe.reentry_status, Some(VtStatus::BatchInUse));
    assert_eq!(count, 2);

    // The reducer settled its internal lease and left the cursor resumable.
    let (status, batch) = next(&mut cursor, &limits(2, 8, 2));
    assert_eq!(status, VtStatus::Ok);
    assert_eq!(batch.entry_count, 1);
    assert_eq!(release(&mut cursor, batch.generation), VtStatus::Ok);
    assert_eq!(next(&mut cursor, &limits(2, 8, 2)).0, VtStatus::End);
}
