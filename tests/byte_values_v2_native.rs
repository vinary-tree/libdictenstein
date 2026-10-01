#![cfg(feature = "ffi")]
//! End-to-end native producer and revision-7 C facade qualification.

use libdictenstein::ffi::*;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::ptr;
use vinary_tree_interop::{
    VtDictionaryByteBatchLimits, VtDictionaryBytesVTable, VtDictionaryGraphVTable,
    VtDictionaryGraphView, VtDictionaryVTable, VtResource, VtStatus, VtValueDomain,
    VT_DICTIONARY_BYTES_INTERFACE_ID, VT_DICTIONARY_GRAPH_INTERFACE_ID, VT_DICTIONARY_INTERFACE_ID,
};

struct Dict(*mut LdictDictionary);

// The scoped workers borrow a single live C handle; Dict frees it only after
// they join. The byte-valued DynamicDawg binding supports concurrent calls.
struct SharedByteDictionary(*mut LdictDictionary);
// SAFETY: the C handle remains live for the entire scoped borrow, and its
// byte-valued binding synchronizes concurrent reads and writes internally.
unsafe impl Sync for SharedByteDictionary {}

impl SharedByteDictionary {
    fn as_ptr(&self) -> *mut LdictDictionary {
        self.0
    }
}

impl Dict {
    fn new(domain: u32) -> Self {
        let mut raw = ptr::null_mut();
        assert_eq!(
            unsafe { ldict_dynamic_dawg_new_byte_values(domain, &mut raw) },
            LdictStatus::Ok
        );
        assert!(!raw.is_null());
        Self(raw)
    }
}

impl Drop for Dict {
    fn drop(&mut self) {
        unsafe { ldict_dictionary_free(self.0) };
    }
}

fn get_value(
    dictionary: &Dict,
    domain: u32,
    text_key: &[u8],
    token_key: &[u64],
    buffer: *mut u8,
    cap: usize,
) -> (LdictStatus, u8, u8, usize, usize) {
    let mut found = 9u8;
    let mut has = 9u8;
    let mut written = 9usize;
    let mut required = 9usize;
    let status = if domain == 3 {
        unsafe {
            ldict_dictionary_get_u64_bytes(
                dictionary.0,
                token_key.as_ptr(),
                token_key.len(),
                &mut found,
                buffer,
                cap,
                &mut written,
                &mut required,
                &mut has,
            )
        }
    } else {
        unsafe {
            ldict_dictionary_get_text_bytes(
                dictionary.0,
                text_key.as_ptr(),
                text_key.len(),
                &mut found,
                buffer,
                cap,
                &mut written,
                &mut required,
                &mut has,
            )
        }
    };
    (status, found, has, written, required)
}

#[test]
fn c_crud_preserves_three_presence_states_and_unit_domains() {
    let mut invalid = std::ptr::without_provenance_mut::<LdictDictionary>(1);
    assert_eq!(
        unsafe { ldict_dynamic_dawg_new_byte_values(99, &mut invalid) },
        LdictStatus::InvalidArgument
    );
    assert!(invalid.is_null());
    for domain in 1..=3 {
        let dictionary = Dict::new(domain);
        let mut inserted = 9u8;
        let text_key: &[u8] = if domain == 1 {
            &[0xff]
        } else {
            "α".as_bytes()
        };
        let token_key = [0, u64::MAX];
        let (text_ptr, text_len) = (text_key.as_ptr(), text_key.len());
        if domain == 3 {
            assert_eq!(
                unsafe {
                    ldict_dictionary_insert_u64_bytes(
                        dictionary.0,
                        token_key.as_ptr(),
                        token_key.len(),
                        ptr::null(),
                        0,
                        0,
                        &mut inserted,
                    )
                },
                LdictStatus::Ok
            );
        } else {
            assert_eq!(
                unsafe {
                    ldict_dictionary_insert_text_bytes(
                        dictionary.0,
                        text_ptr,
                        text_len,
                        ptr::null(),
                        0,
                        0,
                        &mut inserted,
                    )
                },
                LdictStatus::Ok
            );
        }
        assert_eq!(inserted, 1);
        assert_eq!(
            get_value(
                &dictionary,
                domain,
                text_key,
                &token_key,
                ptr::null_mut(),
                0
            ),
            (LdictStatus::Ok, 1, 0, 0, 0)
        );
        let insert_empty = if domain == 3 {
            unsafe {
                ldict_dictionary_insert_u64_bytes(
                    dictionary.0,
                    token_key.as_ptr(),
                    token_key.len(),
                    ptr::null(),
                    0,
                    1,
                    &mut inserted,
                )
            }
        } else {
            unsafe {
                ldict_dictionary_insert_text_bytes(
                    dictionary.0,
                    text_ptr,
                    text_len,
                    ptr::null(),
                    0,
                    1,
                    &mut inserted,
                )
            }
        };
        assert_eq!(insert_empty, LdictStatus::Ok);
        assert_eq!(
            get_value(
                &dictionary,
                domain,
                text_key,
                &token_key,
                ptr::null_mut(),
                0
            ),
            (LdictStatus::Ok, 1, 1, 0, 0)
        );
        let payload = [0, 0xff, 42];
        let insert_payload = if domain == 3 {
            unsafe {
                ldict_dictionary_insert_u64_bytes(
                    dictionary.0,
                    token_key.as_ptr(),
                    token_key.len(),
                    payload.as_ptr(),
                    payload.len(),
                    1,
                    &mut inserted,
                )
            }
        } else {
            unsafe {
                ldict_dictionary_insert_text_bytes(
                    dictionary.0,
                    text_ptr,
                    text_len,
                    payload.as_ptr(),
                    payload.len(),
                    1,
                    &mut inserted,
                )
            }
        };
        assert_eq!(insert_payload, LdictStatus::Ok);
        assert_eq!(
            get_value(
                &dictionary,
                domain,
                text_key,
                &token_key,
                ptr::null_mut(),
                0
            ),
            (LdictStatus::LimitExceeded, 1, 1, 0, 3)
        );
        let mut output = [0u8; 3];
        assert_eq!(
            get_value(
                &dictionary,
                domain,
                text_key,
                &token_key,
                output.as_mut_ptr(),
                3
            ),
            (LdictStatus::Ok, 1, 1, 3, 3)
        );
        assert_eq!(output, payload);
        // A malformed absent value cannot mutate an existing entry.
        let bad_insert = if domain == 3 {
            unsafe {
                ldict_dictionary_insert_u64_bytes(
                    dictionary.0,
                    token_key.as_ptr(),
                    token_key.len(),
                    payload.as_ptr(),
                    1,
                    0,
                    &mut inserted,
                )
            }
        } else {
            unsafe {
                ldict_dictionary_insert_text_bytes(
                    dictionary.0,
                    text_ptr,
                    text_len,
                    payload.as_ptr(),
                    1,
                    0,
                    &mut inserted,
                )
            }
        };
        assert_eq!(bad_insert, LdictStatus::InvalidArgument);
        assert_eq!(
            get_value(
                &dictionary,
                domain,
                text_key,
                &token_key,
                output.as_mut_ptr(),
                3
            ),
            (LdictStatus::Ok, 1, 1, 3, 3)
        );
        assert_eq!(output, payload);
        assert_eq!(
            unsafe {
                ldict_dictionary_insert_text_bytes(
                    dictionary.0,
                    text_ptr,
                    text_len,
                    ptr::null(),
                    0,
                    2,
                    &mut inserted,
                )
            },
            LdictStatus::InvalidArgument
        );
        assert_eq!(ldict_api_revision(), 7);
    }
}

#[test]
fn old_term_only_paths_work_but_u64_value_paths_reject_bytes() {
    let dictionary = Dict::new(1);
    let mut inserted = 9u8;
    assert_eq!(
        unsafe {
            ldict_dictionary_insert_text(
                dictionary.0,
                b"term".as_ptr(),
                4,
                LdictOptionalU64::default(),
                &mut inserted,
            )
        },
        LdictStatus::Ok
    );
    let mut contains = 9u8;
    assert_eq!(
        unsafe { ldict_dictionary_contains_text(dictionary.0, b"term".as_ptr(), 4, &mut contains) },
        LdictStatus::Ok
    );
    assert_eq!(contains, 1);
    let mut found = 9u8;
    let mut value = 9u64;
    let mut has = 9u8;
    assert_eq!(
        unsafe {
            ldict_dictionary_get_text_value(
                dictionary.0,
                b"term".as_ptr(),
                4,
                &mut found,
                &mut value,
                &mut has,
            )
        },
        LdictStatus::Unsupported
    );
    assert_eq!((found, value, has), (9, 9, 9));
    let mut cursor = ptr::null_mut();
    let mut info = LdictEntriesInfo::default();
    assert_eq!(
        unsafe { ldict_dictionary_entries_open(dictionary.0, &mut cursor, &mut info) },
        LdictStatus::Unsupported
    );
    assert!(cursor.is_null());
}

#[test]
fn byte_entry_leases_are_bounded_atomic_and_snapshot_owned() {
    let dictionary = Dict::new(1);
    let mut inserted = 0u8;
    assert_eq!(
        unsafe {
            ldict_dictionary_insert_text_bytes(
                dictionary.0,
                ptr::null(),
                0,
                ptr::null(),
                0,
                0,
                &mut inserted,
            )
        },
        LdictStatus::Ok
    );
    assert_eq!(
        unsafe {
            ldict_dictionary_insert_text_bytes(
                dictionary.0,
                b"a".as_ptr(),
                1,
                ptr::null(),
                0,
                1,
                &mut inserted,
            )
        },
        LdictStatus::Ok
    );
    assert_eq!(
        unsafe {
            ldict_dictionary_insert_text_bytes(
                dictionary.0,
                b"b".as_ptr(),
                1,
                b"xy".as_ptr(),
                2,
                1,
                &mut inserted,
            )
        },
        LdictStatus::Ok
    );
    let mut cursor = ptr::null_mut();
    let mut info = LdictByteEntriesInfo::default();
    assert_eq!(
        unsafe { ldict_dictionary_byte_entries_open(dictionary.0, &mut cursor, &mut info) },
        LdictStatus::Ok
    );
    assert_eq!(info.value_domain, VtValueDomain::Bytes as u32);
    assert_eq!(info.exact_len, 3);
    unsafe { ldict_dictionary_free(dictionary.0) };
    std::mem::forget(dictionary); // cursor retained its own snapshot
    let mut limits = LdictByteEntryBatchLimits {
        max_entries: 2,
        max_units: 1,
        max_value_bytes: 0,
        reserved: 0,
    };
    let mut batch = LdictByteEntryBatch::default();
    assert_eq!(
        unsafe { ldict_byte_entry_cursor_next(cursor, &limits, &mut batch) },
        LdictStatus::Ok
    );
    assert_eq!(batch.entry_count, 2);
    let entries = unsafe { std::slice::from_raw_parts(batch.entries, batch.entry_count) };
    assert_eq!((entries[0].has_value, entries[0].value_len), (0, 0));
    assert_eq!((entries[1].has_value, entries[1].value_len), (1, 0));
    assert_eq!(
        unsafe { ldict_byte_entry_cursor_next(cursor, &limits, &mut batch) },
        LdictStatus::BatchInUse
    );
    assert_eq!(
        unsafe { ldict_byte_entry_cursor_free(cursor) },
        LdictStatus::BatchInUse
    );
    assert_eq!(
        unsafe { ldict_byte_entry_cursor_release(cursor, batch.generation + 1) },
        LdictStatus::InvalidArgument
    );
    assert_eq!(
        unsafe { ldict_byte_entry_cursor_release(cursor, batch.generation) },
        LdictStatus::Ok
    );
    let mut untouched = LdictByteEntryBatch {
        generation: 777,
        ..Default::default()
    };
    assert_eq!(
        unsafe { ldict_byte_entry_cursor_next(cursor, &limits, &mut untouched) },
        LdictStatus::LimitExceeded
    );
    assert_eq!(untouched.generation, 777);
    limits.max_value_bytes = 2;
    assert_eq!(
        unsafe { ldict_byte_entry_cursor_next(cursor, &limits, &mut batch) },
        LdictStatus::Ok
    );
    assert_eq!(batch.entry_count, 1);
    assert_eq!(batch.value_byte_count, 2);
    assert_eq!(
        unsafe { std::slice::from_raw_parts(batch.value_bytes, 2) },
        b"xy"
    );
    assert_eq!(
        unsafe { ldict_byte_entry_cursor_cancel(cursor) },
        LdictStatus::Ok
    );
    assert_eq!(
        unsafe { ldict_byte_entry_cursor_release(cursor, batch.generation) },
        LdictStatus::Ok
    );
    assert_eq!(
        unsafe { ldict_byte_entry_cursor_next(cursor, &limits, &mut batch) },
        LdictStatus::End
    );
    assert_eq!(
        unsafe { ldict_byte_entry_cursor_free(cursor) },
        LdictStatus::Ok
    );
}

struct Snapshot(VtResource);
impl Drop for Snapshot {
    fn drop(&mut self) {
        unsafe { (*self.0.vtable).release.unwrap()(self.0.context) };
    }
}

unsafe fn query<T>(resource: VtResource, id: &vinary_tree_interop::VtInterfaceId) -> *const T {
    let mut table = ptr::null();
    let status = unsafe {
        (*resource.vtable).query_interface.unwrap()(
            resource.context,
            id,
            if id.bytes == VT_DICTIONARY_BYTES_INTERFACE_ID.bytes {
                2
            } else {
                1
            },
            &mut table,
        )
    };
    assert_eq!(VtStatus::from_raw(status), Some(VtStatus::Ok));
    assert!(!table.is_null());
    table.cast()
}

unsafe fn capture(dictionary: *const LdictDictionary) -> Snapshot {
    let mut resource = VtResource::NULL;
    assert_eq!(
        unsafe { ldict_dictionary_resource(dictionary, &mut resource) },
        LdictStatus::Ok
    );
    let dict = unsafe { &*query::<VtDictionaryVTable>(resource, &VT_DICTIONARY_INTERFACE_ID) };
    let mut snapshot = VtResource::NULL;
    assert_eq!(
        VtStatus::from_raw(unsafe { dict.snapshot.unwrap()(resource.context, &mut snapshot) }),
        Some(VtStatus::Ok)
    );
    Snapshot(snapshot)
}

#[test]
fn graph_tokens_are_snapshot_unique_and_point_copy_is_two_phase() {
    let dictionary = Dict::new(1);
    let mut inserted = 0u8;
    assert_eq!(
        unsafe {
            ldict_dictionary_insert_text_bytes(
                dictionary.0,
                b"x".as_ptr(),
                1,
                b"one".as_ptr(),
                3,
                1,
                &mut inserted,
            )
        },
        LdictStatus::Ok
    );
    let first = unsafe { capture(dictionary.0) };
    let first_graph =
        unsafe { &*query::<VtDictionaryGraphVTable>(first.0, &VT_DICTIONARY_GRAPH_INTERFACE_ID) };
    let first_bytes =
        unsafe { &*query::<VtDictionaryBytesVTable>(first.0, &VT_DICTIONARY_BYTES_INTERFACE_ID) };
    let mut graph = VtDictionaryGraphView::default();
    assert_eq!(
        VtStatus::from_raw(unsafe { first_graph.graph.unwrap()(first.0.context, &mut graph) }),
        Some(VtStatus::Ok)
    );
    let nodes = unsafe { std::slice::from_raw_parts(graph.nodes, graph.node_count) };
    let token = nodes
        .iter()
        .find(|node| node.is_final == 1)
        .unwrap()
        .value_cursor;
    let mut written = 9usize;
    let mut required = 9usize;
    let mut has = 9u8;
    let copy = first_bytes.graph_value_bytes.unwrap();
    assert_eq!(
        VtStatus::from_raw(unsafe {
            copy(
                first.0.context,
                token,
                ptr::null_mut(),
                0,
                &mut written,
                &mut required,
                &mut has,
            )
        }),
        Some(VtStatus::LimitExceeded)
    );
    assert_eq!((written, required, has), (0, 3, 1));
    let mut output = [0u8; 3];
    assert_eq!(
        VtStatus::from_raw(unsafe {
            copy(
                first.0.context,
                token,
                output.as_mut_ptr(),
                3,
                &mut written,
                &mut required,
                &mut has,
            )
        }),
        Some(VtStatus::Ok)
    );
    assert_eq!(&output, b"one");
    assert_eq!(
        unsafe {
            ldict_dictionary_insert_text_bytes(
                dictionary.0,
                b"y".as_ptr(),
                1,
                b"two".as_ptr(),
                3,
                1,
                &mut inserted,
            )
        },
        LdictStatus::Ok
    );
    let second = unsafe { capture(dictionary.0) };
    let second_graph =
        unsafe { &*query::<VtDictionaryGraphVTable>(second.0, &VT_DICTIONARY_GRAPH_INTERFACE_ID) };
    let second_bytes =
        unsafe { &*query::<VtDictionaryBytesVTable>(second.0, &VT_DICTIONARY_BYTES_INTERFACE_ID) };
    let mut graph2 = VtDictionaryGraphView::default();
    assert_eq!(
        VtStatus::from_raw(unsafe { second_graph.graph.unwrap()(second.0.context, &mut graph2) }),
        Some(VtStatus::Ok)
    );
    let nodes2 = unsafe { std::slice::from_raw_parts(graph2.nodes, graph2.node_count) };
    assert!(nodes2.iter().all(|node| node.value_cursor != token));
    assert_eq!(
        VtStatus::from_raw(unsafe {
            second_bytes.graph_value_bytes.unwrap()(
                second.0.context,
                token,
                output.as_mut_ptr(),
                3,
                &mut written,
                &mut required,
                &mut has,
            )
        }),
        Some(VtStatus::InvalidArgument)
    );
    assert_eq!(
        VtStatus::from_raw(unsafe {
            copy(
                first.0.context,
                token,
                output.as_mut_ptr(),
                3,
                &mut written,
                &mut required,
                &mut has,
            )
        }),
        Some(VtStatus::Ok)
    );
    assert_eq!(&output, b"one");
}

#[test]
fn batch_reducer_amortizes_foreign_calls_over_entries() {
    let dictionary = Dict::new(1);
    let mut inserted = 0u8;
    for key in 0u16..130 {
        let units = key.to_be_bytes();
        assert_eq!(
            unsafe {
                ldict_dictionary_insert_text_bytes(
                    dictionary.0,
                    units.as_ptr(),
                    2,
                    b"v".as_ptr(),
                    1,
                    1,
                    &mut inserted,
                )
            },
            LdictStatus::Ok
        );
    }
    let mut cursor = ptr::null_mut();
    let mut info = LdictByteEntriesInfo::default();
    assert_eq!(
        unsafe { ldict_dictionary_byte_entries_open(dictionary.0, &mut cursor, &mut info) },
        LdictStatus::Ok
    );
    unsafe extern "C" fn count_batches(
        context: *mut c_void,
        batch: *const LdictByteEntryBatch,
    ) -> u32 {
        let calls = unsafe { &mut *context.cast::<usize>() };
        *calls += 1;
        assert!(unsafe { (*batch).entry_count } <= 64);
        LdictStatus::Ok as u32
    }
    let limits = VtDictionaryByteBatchLimits {
        max_entries: 64,
        max_units: 128,
        max_value_bytes: 64,
        reserved: 0,
    };
    let mut calls = 0usize;
    let mut count = 0usize;
    assert_eq!(
        unsafe {
            ldict_byte_entry_cursor_reduce(
                cursor,
                &limits,
                Some(count_batches),
                (&mut calls as *mut usize).cast(),
                &mut count,
            )
        },
        LdictStatus::Ok
    );
    assert_eq!(count, 130);
    assert_eq!(calls, 3); // 64 + 64 + 2; no per-key foreign dispatch.
    assert_eq!(
        unsafe { ldict_byte_entry_cursor_free(cursor) },
        LdictStatus::Ok
    );
}

#[test]
fn deterministic_mutation_fuzz_matches_three_state_byte_model() {
    let dictionary = Dict::new(1);
    let mut model = BTreeMap::<Vec<u8>, Option<Vec<u8>>>::new();
    let mut seed = 0x8ca5_1e73_45ab_129du64;
    for _ in 0..192 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let key = [((seed >> 16) & 15) as u8, ((seed >> 24) & 255) as u8];
        let choice = (seed >> 40) & 3;
        if choice <= 1 {
            let value = match (seed >> 44) % 3 {
                0 => None,
                1 => Some(Vec::new()),
                _ => Some(vec![(seed >> 48) as u8, 0xff]),
            };
            let mut inserted = 9u8;
            let bytes = value.as_deref().unwrap_or_default();
            assert_eq!(
                unsafe {
                    ldict_dictionary_insert_text_bytes(
                        dictionary.0,
                        key.as_ptr(),
                        key.len(),
                        bytes.as_ptr(),
                        bytes.len(),
                        u8::from(value.is_some()),
                        &mut inserted,
                    )
                },
                LdictStatus::Ok
            );
            assert_eq!(inserted == 1, model.insert(key.to_vec(), value).is_none());
        } else if choice == 2 {
            let mut removed = 9u8;
            assert_eq!(
                unsafe {
                    ldict_dictionary_remove_text(
                        dictionary.0,
                        key.as_ptr(),
                        key.len(),
                        &mut removed,
                    )
                },
                LdictStatus::Ok
            );
            assert_eq!(removed == 1, model.remove(key.as_slice()).is_some());
        }
        let mut output = [0u8; 2];
        let observed = get_value(&dictionary, 1, &key, &[], output.as_mut_ptr(), output.len());
        match model.get(key.as_slice()) {
            None => assert_eq!(observed, (LdictStatus::Ok, 0, 0, 0, 0)),
            Some(None) => assert_eq!(observed, (LdictStatus::Ok, 1, 0, 0, 0)),
            Some(Some(value)) => {
                assert_eq!(observed, (LdictStatus::Ok, 1, 1, value.len(), value.len()));
                assert_eq!(&output[..value.len()], value);
            }
        }
        let mut len = usize::MAX;
        assert_eq!(
            unsafe { ldict_dictionary_len(dictionary.0, &mut len) },
            LdictStatus::Ok
        );
        assert_eq!(len, model.len());
    }
}

#[test]
fn concurrent_byte_value_reads_and_writes_remain_coherent() {
    let dictionary = Dict::new(1);
    let shared = SharedByteDictionary(dictionary.0);
    std::thread::scope(|scope| {
        let shared = &shared;
        scope.spawn(move || {
            let raw = shared.as_ptr();
            for i in 0u16..128 {
                let key = i.to_be_bytes();
                let mut inserted = 0u8;
                assert_eq!(
                    unsafe {
                        ldict_dictionary_insert_text_bytes(
                            raw,
                            key.as_ptr(),
                            key.len(),
                            key.as_ptr(),
                            key.len(),
                            1,
                            &mut inserted,
                        )
                    },
                    LdictStatus::Ok
                );
                assert_eq!(inserted, 1);
            }
        });
        scope.spawn(move || {
            let raw = shared.as_ptr().cast_const();
            for i in 0u16..128 {
                let key = i.to_be_bytes();
                let mut found = 9u8;
                let mut has = 9u8;
                let mut written = 9usize;
                let mut required = 9usize;
                let mut output = [0u8; 2];
                assert_eq!(
                    unsafe {
                        ldict_dictionary_get_text_bytes(
                            raw,
                            key.as_ptr(),
                            key.len(),
                            &mut found,
                            output.as_mut_ptr(),
                            output.len(),
                            &mut written,
                            &mut required,
                            &mut has,
                        )
                    },
                    LdictStatus::Ok
                );
                assert!(
                    found == 0
                        || (found == 1
                            && has == 1
                            && written == 2
                            && required == 2
                            && output == key)
                );
            }
        });
    });
    let mut len = 0usize;
    assert_eq!(
        unsafe { ldict_dictionary_len(dictionary.0, &mut len) },
        LdictStatus::Ok
    );
    assert_eq!(len, 128);
}
