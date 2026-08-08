//! Status and capability matrix for all 35 `ldict_*` producer C-ABI functions.
//!
//! Spec: the ldict status tables are proved in
//! `formal-verification/rocq/Spec/AbiStatusMappingSpec.v` (plan obligation
//! #13); this file is the ABI-observable correspondence anchor for the
//! LDICT-STAT-* rows of libdictenstein's invariant registry.
//!
//! INVARIANT-HOOK: LDICT-STAT-1 — status totality: every entry point maps each
//! failure class to its documented `LdictStatus` (null pointer, invalid
//! argument, malformed UTF-8, capability, domain, persistence) and success to
//! `Ok`, with no class swallowed into a neighbouring status.
//! INVARIANT-HOOK: LDICT-STAT-2 — argument discipline: validation order is
//! pinned (optional-value decode precedes handle checks; null checks precede
//! capability dispatch), constructor out-params are pre-nulled on failure, and
//! query out-params are left untouched on failure.
//! INVARIANT-HOOK: LDICT-STAT-3 — the error channel is thread-local: failures
//! set a non-empty `ldict_last_error_message` on the calling thread only, and
//! `Ok`/`End` clear it.
//! INVARIANT-HOOK: LDICT-STAT-4 — the `LDICT_KIND_*` and `LDICT_CAP_*`
//! constants observed through the ABI match `bindings/api.json` per backend.

#![cfg(feature = "ffi")]

mod ffi_common;

use ffi_common::{
    bad_optional, contains_text, contains_u64, dirty_reserved_optional, get_text, insert_text,
    insert_u64, last_error, none, remove_text, remove_u64, some, text_entry, DictGuard,
    DOMAIN_BYTE, DOMAIN_U64, DOMAIN_UNICODE,
};
use libdictenstein::ffi::{
    ldict_abi_version, ldict_api_revision, ldict_dictionary_capabilities,
    ldict_dictionary_checkpoint, ldict_dictionary_clear, ldict_dictionary_compact,
    ldict_dictionary_contains_text, ldict_dictionary_contains_u64, ldict_dictionary_free,
    ldict_dictionary_get_text, ldict_dictionary_get_text_value, ldict_dictionary_get_u64,
    ldict_dictionary_get_u64_value, ldict_dictionary_insert_text,
    ldict_dictionary_insert_text_batch, ldict_dictionary_insert_text_value,
    ldict_dictionary_insert_u64, ldict_dictionary_insert_u64_batch,
    ldict_dictionary_insert_u64_value, ldict_dictionary_kind, ldict_dictionary_len,
    ldict_dictionary_remove_text, ldict_dictionary_remove_u64, ldict_dictionary_resource,
    ldict_double_array_trie_new, ldict_dynamic_dawg_new, ldict_last_error_message,
    ldict_persistent_artrie_create, ldict_persistent_artrie_open, ldict_persistent_vocab_create,
    ldict_persistent_vocab_open, ldict_scdawg_contains_substring, ldict_scdawg_new,
    ldict_scdawg_substring_frequency, ldict_vocab_get_term, LdictDictionary, LdictOptionalU64,
    LdictStatus, LdictTextEntry, LDICT_ABI_VERSION, LDICT_API_REVISION, LDICT_CAP_CHECKPOINT,
    LDICT_CAP_CLEAR, LDICT_CAP_COMPACT, LDICT_CAP_INSERT, LDICT_CAP_READ, LDICT_CAP_REMOVE,
    LDICT_CAP_SUBSTRING, LDICT_KIND_DOUBLE_ARRAY_TRIE, LDICT_KIND_DYNAMIC_DAWG,
    LDICT_KIND_PERSISTENT_ARTRIE, LDICT_KIND_PERSISTENT_VOCAB_ARTRIE, LDICT_KIND_SCDAWG,
};
use std::ffi::CStr;
use vinary_tree_interop::VtResource;

/// A valid non-empty UTF-8 term used wherever a matrix row needs one.
const TERM: &[u8] = b"cap";
/// A valid u64-token term used wherever a matrix row needs one.
const TOKENS: &[u64] = &[7, 9];
/// Bytes that are not valid UTF-8 anywhere.
const NOT_UTF8: &[u8] = &[0xFF, 0xFE, 0x01];
/// Raw bytes with an embedded NUL and a 0xFF, legal in the byte domain.
const RAW_BYTES: &[u8] = &[0x00, 0x61, 0xFF];

fn dat(domain: u32, terms: &[&[u8]]) -> DictGuard {
    let entries: Vec<LdictTextEntry> = terms.iter().map(|term| text_entry(term, None)).collect();
    let mut handle: *mut LdictDictionary = std::ptr::null_mut();
    let status = unsafe {
        ldict_double_array_trie_new(domain, entries.as_ptr(), entries.len(), &mut handle)
    };
    assert_eq!(status, LdictStatus::Ok, "DAT constructor failed");
    DictGuard(handle)
}

fn persistent(directory: &std::path::Path, name: &str, domain: u32) -> DictGuard {
    let path = directory.join(name);
    let bytes = path.to_str().expect("tempdir paths are UTF-8").as_bytes();
    let mut handle: *mut LdictDictionary = std::ptr::null_mut();
    let status =
        unsafe { ldict_persistent_artrie_create(domain, bytes.as_ptr(), bytes.len(), &mut handle) };
    assert_eq!(
        status,
        LdictStatus::Ok,
        "persistent create failed: {}",
        last_error()
    );
    DictGuard(handle)
}

fn vocab(directory: &std::path::Path, name: &str) -> DictGuard {
    let path = directory.join(name);
    let bytes = path.to_str().expect("tempdir paths are UTF-8").as_bytes();
    let mut handle: *mut LdictDictionary = std::ptr::null_mut();
    let status = unsafe { ldict_persistent_vocab_create(bytes.as_ptr(), bytes.len(), &mut handle) };
    assert_eq!(
        status,
        LdictStatus::Ok,
        "vocab create failed: {}",
        last_error()
    );
    DictGuard(handle)
}

// ---------------------------------------------------------------------------
// LDICT-STAT-1: null-pointer sweep across every pointer parameter.
// ---------------------------------------------------------------------------

type NullCase<'a> = (&'static str, Box<dyn Fn() -> LdictStatus + 'a>);

#[test]
fn every_null_pointer_parameter_is_rejected_with_null_pointer_status() {
    let dynamic = DictGuard::dynamic(DOMAIN_BYTE);
    let scdawg = DictGuard::scdawg(DOMAIN_BYTE);
    let dict = dynamic.ptr();
    let sub = scdawg.ptr();

    let cases: Vec<NullCase> = vec![
        (
            "dynamic_dawg_new(out=NULL)",
            Box::new(|| unsafe { ldict_dynamic_dawg_new(DOMAIN_BYTE, std::ptr::null_mut()) }),
        ),
        (
            "double_array_trie_new(out=NULL)",
            Box::new(|| unsafe {
                ldict_double_array_trie_new(DOMAIN_BYTE, std::ptr::null(), 0, std::ptr::null_mut())
            }),
        ),
        (
            "double_array_trie_new(entries=NULL,count=1)",
            Box::new(|| unsafe {
                let mut out: *mut LdictDictionary = std::ptr::null_mut();
                ldict_double_array_trie_new(DOMAIN_BYTE, std::ptr::null(), 1, &mut out)
            }),
        ),
        (
            "double_array_trie_new(entry.data=NULL,len=1)",
            Box::new(|| unsafe {
                let broken = [LdictTextEntry {
                    data: std::ptr::null(),
                    len: 1,
                    value: none(),
                }];
                let mut out: *mut LdictDictionary = std::ptr::null_mut();
                ldict_double_array_trie_new(DOMAIN_BYTE, broken.as_ptr(), broken.len(), &mut out)
            }),
        ),
        (
            "scdawg_new(out=NULL)",
            Box::new(|| unsafe { ldict_scdawg_new(DOMAIN_BYTE, std::ptr::null_mut()) }),
        ),
        (
            "persistent_artrie_create(out=NULL)",
            Box::new(|| unsafe {
                ldict_persistent_artrie_create(DOMAIN_BYTE, b"p".as_ptr(), 1, std::ptr::null_mut())
            }),
        ),
        (
            "persistent_artrie_create(path=NULL,len=1)",
            Box::new(|| unsafe {
                let mut out: *mut LdictDictionary = std::ptr::null_mut();
                ldict_persistent_artrie_create(DOMAIN_BYTE, std::ptr::null(), 1, &mut out)
            }),
        ),
        (
            "persistent_artrie_open(out=NULL)",
            Box::new(|| unsafe {
                ldict_persistent_artrie_open(DOMAIN_BYTE, b"p".as_ptr(), 1, std::ptr::null_mut())
            }),
        ),
        (
            "persistent_artrie_open(path=NULL,len=1)",
            Box::new(|| unsafe {
                let mut out: *mut LdictDictionary = std::ptr::null_mut();
                ldict_persistent_artrie_open(DOMAIN_BYTE, std::ptr::null(), 1, &mut out)
            }),
        ),
        (
            "persistent_vocab_create(out=NULL)",
            Box::new(|| unsafe {
                ldict_persistent_vocab_create(b"p".as_ptr(), 1, std::ptr::null_mut())
            }),
        ),
        (
            "persistent_vocab_create(path=NULL,len=1)",
            Box::new(|| unsafe {
                let mut out: *mut LdictDictionary = std::ptr::null_mut();
                ldict_persistent_vocab_create(std::ptr::null(), 1, &mut out)
            }),
        ),
        (
            "persistent_vocab_open(out=NULL)",
            Box::new(|| unsafe {
                ldict_persistent_vocab_open(b"p".as_ptr(), 1, std::ptr::null_mut())
            }),
        ),
        (
            "persistent_vocab_open(path=NULL,len=1)",
            Box::new(|| unsafe {
                let mut out: *mut LdictDictionary = std::ptr::null_mut();
                ldict_persistent_vocab_open(std::ptr::null(), 1, &mut out)
            }),
        ),
        (
            "dictionary_kind(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut kind = 0u32;
                ldict_dictionary_kind(std::ptr::null(), &mut kind)
            }),
        ),
        (
            "dictionary_kind(out_kind=NULL)",
            Box::new(move || unsafe { ldict_dictionary_kind(dict, std::ptr::null_mut()) }),
        ),
        (
            "dictionary_capabilities(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut capabilities = 0u64;
                ldict_dictionary_capabilities(std::ptr::null(), &mut capabilities)
            }),
        ),
        (
            "dictionary_capabilities(out=NULL)",
            Box::new(move || unsafe { ldict_dictionary_capabilities(dict, std::ptr::null_mut()) }),
        ),
        (
            "dictionary_resource(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut resource = VtResource::NULL;
                ldict_dictionary_resource(std::ptr::null(), &mut resource)
            }),
        ),
        (
            "dictionary_resource(out=NULL)",
            Box::new(move || unsafe { ldict_dictionary_resource(dict, std::ptr::null_mut()) }),
        ),
        (
            "dictionary_len(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut len = 0usize;
                ldict_dictionary_len(std::ptr::null(), &mut len)
            }),
        ),
        (
            "dictionary_len(out=NULL)",
            Box::new(move || unsafe { ldict_dictionary_len(dict, std::ptr::null_mut()) }),
        ),
        (
            "dictionary_checkpoint(dictionary=NULL)",
            Box::new(|| unsafe { ldict_dictionary_checkpoint(std::ptr::null_mut()) }),
        ),
        (
            "vocab_get_term(dictionary=NULL)",
            Box::new(|| unsafe {
                let (mut len, mut found) = (0usize, 0u8);
                ldict_vocab_get_term(
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    0,
                    &mut len,
                    &mut found,
                )
            }),
        ),
        (
            "vocab_get_term(out_len=NULL)",
            Box::new(move || unsafe {
                let mut found = 0u8;
                ldict_vocab_get_term(
                    dict,
                    0,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut found,
                )
            }),
        ),
        (
            "vocab_get_term(out_found=NULL)",
            Box::new(move || unsafe {
                let mut len = 0usize;
                ldict_vocab_get_term(
                    dict,
                    0,
                    std::ptr::null_mut(),
                    0,
                    &mut len,
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "vocab_get_term(out_data=NULL,capacity=4)",
            Box::new(move || unsafe {
                let (mut len, mut found) = (0usize, 0u8);
                ldict_vocab_get_term(dict, 0, std::ptr::null_mut(), 4, &mut len, &mut found)
            }),
        ),
        (
            "dictionary_clear(dictionary=NULL)",
            Box::new(|| unsafe { ldict_dictionary_clear(std::ptr::null_mut()) }),
        ),
        (
            "dictionary_compact(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut reclaimed = 0usize;
                ldict_dictionary_compact(std::ptr::null_mut(), &mut reclaimed)
            }),
        ),
        (
            "dictionary_compact(out=NULL)",
            Box::new(move || unsafe { ldict_dictionary_compact(dict, std::ptr::null_mut()) }),
        ),
        (
            "insert_text(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut inserted = 0u8;
                ldict_dictionary_insert_text(
                    std::ptr::null_mut(),
                    TERM.as_ptr(),
                    TERM.len(),
                    none(),
                    &mut inserted,
                )
            }),
        ),
        (
            "insert_text(data=NULL,len=3)",
            Box::new(move || unsafe {
                let mut inserted = 0u8;
                ldict_dictionary_insert_text(dict, std::ptr::null(), 3, none(), &mut inserted)
            }),
        ),
        (
            "insert_text(out=NULL)",
            Box::new(move || unsafe {
                ldict_dictionary_insert_text(
                    dict,
                    TERM.as_ptr(),
                    TERM.len(),
                    none(),
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "insert_text_value(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut inserted = 0u8;
                ldict_dictionary_insert_text_value(
                    std::ptr::null_mut(),
                    TERM.as_ptr(),
                    TERM.len(),
                    1,
                    1,
                    &mut inserted,
                )
            }),
        ),
        (
            "insert_text_value(data=NULL,len=3)",
            Box::new(move || unsafe {
                let mut inserted = 0u8;
                ldict_dictionary_insert_text_value(dict, std::ptr::null(), 3, 1, 1, &mut inserted)
            }),
        ),
        (
            "insert_text_value(out=NULL)",
            Box::new(move || unsafe {
                ldict_dictionary_insert_text_value(
                    dict,
                    TERM.as_ptr(),
                    TERM.len(),
                    1,
                    1,
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "remove_text(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut removed = 0u8;
                ldict_dictionary_remove_text(
                    std::ptr::null_mut(),
                    TERM.as_ptr(),
                    TERM.len(),
                    &mut removed,
                )
            }),
        ),
        (
            "remove_text(data=NULL,len=3)",
            Box::new(move || unsafe {
                let mut removed = 0u8;
                ldict_dictionary_remove_text(dict, std::ptr::null(), 3, &mut removed)
            }),
        ),
        (
            "remove_text(out=NULL)",
            Box::new(move || unsafe {
                ldict_dictionary_remove_text(dict, TERM.as_ptr(), TERM.len(), std::ptr::null_mut())
            }),
        ),
        (
            "contains_text(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut contains = 0u8;
                ldict_dictionary_contains_text(
                    std::ptr::null(),
                    TERM.as_ptr(),
                    TERM.len(),
                    &mut contains,
                )
            }),
        ),
        (
            "contains_text(data=NULL,len=3)",
            Box::new(move || unsafe {
                let mut contains = 0u8;
                ldict_dictionary_contains_text(dict, std::ptr::null(), 3, &mut contains)
            }),
        ),
        (
            "contains_text(out=NULL)",
            Box::new(move || unsafe {
                ldict_dictionary_contains_text(
                    dict,
                    TERM.as_ptr(),
                    TERM.len(),
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "get_text(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut found = 0u8;
                let mut value = LdictOptionalU64::default();
                ldict_dictionary_get_text(
                    std::ptr::null(),
                    TERM.as_ptr(),
                    TERM.len(),
                    &mut found,
                    &mut value,
                )
            }),
        ),
        (
            "get_text(data=NULL,len=3)",
            Box::new(move || unsafe {
                let mut found = 0u8;
                let mut value = LdictOptionalU64::default();
                ldict_dictionary_get_text(dict, std::ptr::null(), 3, &mut found, &mut value)
            }),
        ),
        (
            "get_text(out_found=NULL)",
            Box::new(move || unsafe {
                let mut value = LdictOptionalU64::default();
                ldict_dictionary_get_text(
                    dict,
                    TERM.as_ptr(),
                    TERM.len(),
                    std::ptr::null_mut(),
                    &mut value,
                )
            }),
        ),
        (
            "get_text(out_value=NULL)",
            Box::new(move || unsafe {
                let mut found = 0u8;
                ldict_dictionary_get_text(
                    dict,
                    TERM.as_ptr(),
                    TERM.len(),
                    &mut found,
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "get_text_value(out_found=NULL)",
            Box::new(move || unsafe {
                let (mut value, mut has_value) = (0u64, 0u8);
                ldict_dictionary_get_text_value(
                    dict,
                    TERM.as_ptr(),
                    TERM.len(),
                    std::ptr::null_mut(),
                    &mut value,
                    &mut has_value,
                )
            }),
        ),
        (
            "get_text_value(out_value=NULL)",
            Box::new(move || unsafe {
                let (mut found, mut has_value) = (0u8, 0u8);
                ldict_dictionary_get_text_value(
                    dict,
                    TERM.as_ptr(),
                    TERM.len(),
                    &mut found,
                    std::ptr::null_mut(),
                    &mut has_value,
                )
            }),
        ),
        (
            "get_text_value(out_has_value=NULL)",
            Box::new(move || unsafe {
                let (mut found, mut value) = (0u8, 0u64);
                ldict_dictionary_get_text_value(
                    dict,
                    TERM.as_ptr(),
                    TERM.len(),
                    &mut found,
                    &mut value,
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "get_text_value(dictionary=NULL)",
            Box::new(|| unsafe {
                let (mut found, mut value, mut has_value) = (0u8, 0u64, 0u8);
                ldict_dictionary_get_text_value(
                    std::ptr::null(),
                    TERM.as_ptr(),
                    TERM.len(),
                    &mut found,
                    &mut value,
                    &mut has_value,
                )
            }),
        ),
        (
            "get_text_value(data=NULL,len=3)",
            Box::new(move || unsafe {
                let (mut found, mut value, mut has_value) = (0u8, 0u64, 0u8);
                ldict_dictionary_get_text_value(
                    dict,
                    std::ptr::null(),
                    3,
                    &mut found,
                    &mut value,
                    &mut has_value,
                )
            }),
        ),
        (
            "insert_u64(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut inserted = 0u8;
                ldict_dictionary_insert_u64(
                    std::ptr::null_mut(),
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    none(),
                    &mut inserted,
                )
            }),
        ),
        (
            "insert_u64(data=NULL,len=2)",
            Box::new(move || unsafe {
                let mut inserted = 0u8;
                ldict_dictionary_insert_u64(dict, std::ptr::null(), 2, none(), &mut inserted)
            }),
        ),
        (
            "insert_u64(out=NULL)",
            Box::new(move || unsafe {
                ldict_dictionary_insert_u64(
                    dict,
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    none(),
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "insert_u64_value(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut inserted = 0u8;
                ldict_dictionary_insert_u64_value(
                    std::ptr::null_mut(),
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    1,
                    1,
                    &mut inserted,
                )
            }),
        ),
        (
            "insert_u64_value(data=NULL,len=2)",
            Box::new(move || unsafe {
                let mut inserted = 0u8;
                ldict_dictionary_insert_u64_value(dict, std::ptr::null(), 2, 1, 1, &mut inserted)
            }),
        ),
        (
            "insert_u64_value(out=NULL)",
            Box::new(move || unsafe {
                ldict_dictionary_insert_u64_value(
                    dict,
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    1,
                    1,
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "remove_u64(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut removed = 0u8;
                ldict_dictionary_remove_u64(
                    std::ptr::null_mut(),
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    &mut removed,
                )
            }),
        ),
        (
            "remove_u64(data=NULL,len=2)",
            Box::new(move || unsafe {
                let mut removed = 0u8;
                ldict_dictionary_remove_u64(dict, std::ptr::null(), 2, &mut removed)
            }),
        ),
        (
            "remove_u64(out=NULL)",
            Box::new(move || unsafe {
                ldict_dictionary_remove_u64(
                    dict,
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "contains_u64(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut contains = 0u8;
                ldict_dictionary_contains_u64(
                    std::ptr::null(),
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    &mut contains,
                )
            }),
        ),
        (
            "contains_u64(data=NULL,len=2)",
            Box::new(move || unsafe {
                let mut contains = 0u8;
                ldict_dictionary_contains_u64(dict, std::ptr::null(), 2, &mut contains)
            }),
        ),
        (
            "contains_u64(out=NULL)",
            Box::new(move || unsafe {
                ldict_dictionary_contains_u64(
                    dict,
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "get_u64(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut found = 0u8;
                let mut value = LdictOptionalU64::default();
                ldict_dictionary_get_u64(
                    std::ptr::null(),
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    &mut found,
                    &mut value,
                )
            }),
        ),
        (
            "get_u64(data=NULL,len=2)",
            Box::new(move || unsafe {
                let mut found = 0u8;
                let mut value = LdictOptionalU64::default();
                ldict_dictionary_get_u64(dict, std::ptr::null(), 2, &mut found, &mut value)
            }),
        ),
        (
            "get_u64(out_found=NULL)",
            Box::new(move || unsafe {
                let mut value = LdictOptionalU64::default();
                ldict_dictionary_get_u64(
                    dict,
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    std::ptr::null_mut(),
                    &mut value,
                )
            }),
        ),
        (
            "get_u64(out_value=NULL)",
            Box::new(move || unsafe {
                let mut found = 0u8;
                ldict_dictionary_get_u64(
                    dict,
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    &mut found,
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "get_u64_value(dictionary=NULL)",
            Box::new(|| unsafe {
                let (mut found, mut value, mut has_value) = (0u8, 0u64, 0u8);
                ldict_dictionary_get_u64_value(
                    std::ptr::null(),
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    &mut found,
                    &mut value,
                    &mut has_value,
                )
            }),
        ),
        (
            "get_u64_value(data=NULL,len=2)",
            Box::new(move || unsafe {
                let (mut found, mut value, mut has_value) = (0u8, 0u64, 0u8);
                ldict_dictionary_get_u64_value(
                    dict,
                    std::ptr::null(),
                    2,
                    &mut found,
                    &mut value,
                    &mut has_value,
                )
            }),
        ),
        (
            "get_u64_value(out_found=NULL)",
            Box::new(move || unsafe {
                let (mut value, mut has_value) = (0u64, 0u8);
                ldict_dictionary_get_u64_value(
                    dict,
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    std::ptr::null_mut(),
                    &mut value,
                    &mut has_value,
                )
            }),
        ),
        (
            "get_u64_value(out_value=NULL)",
            Box::new(move || unsafe {
                let (mut found, mut has_value) = (0u8, 0u8);
                ldict_dictionary_get_u64_value(
                    dict,
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    &mut found,
                    std::ptr::null_mut(),
                    &mut has_value,
                )
            }),
        ),
        (
            "get_u64_value(out_has_value=NULL)",
            Box::new(move || unsafe {
                let (mut found, mut value) = (0u8, 0u64);
                ldict_dictionary_get_u64_value(
                    dict,
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    &mut found,
                    &mut value,
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "scdawg_contains_substring(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut contains = 0u8;
                ldict_scdawg_contains_substring(
                    std::ptr::null(),
                    TERM.as_ptr(),
                    TERM.len(),
                    &mut contains,
                )
            }),
        ),
        (
            "scdawg_contains_substring(data=NULL,len=3)",
            Box::new(move || unsafe {
                let mut contains = 0u8;
                ldict_scdawg_contains_substring(sub, std::ptr::null(), 3, &mut contains)
            }),
        ),
        (
            "scdawg_contains_substring(out=NULL)",
            Box::new(move || unsafe {
                ldict_scdawg_contains_substring(
                    sub,
                    TERM.as_ptr(),
                    TERM.len(),
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "scdawg_substring_frequency(dictionary=NULL)",
            Box::new(|| unsafe {
                let mut frequency = 0usize;
                ldict_scdawg_substring_frequency(
                    std::ptr::null(),
                    TERM.as_ptr(),
                    TERM.len(),
                    &mut frequency,
                )
            }),
        ),
        (
            "scdawg_substring_frequency(data=NULL,len=3)",
            Box::new(move || unsafe {
                let mut frequency = 0usize;
                ldict_scdawg_substring_frequency(sub, std::ptr::null(), 3, &mut frequency)
            }),
        ),
        (
            "scdawg_substring_frequency(out=NULL)",
            Box::new(move || unsafe {
                ldict_scdawg_substring_frequency(
                    sub,
                    TERM.as_ptr(),
                    TERM.len(),
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "insert_text_batch(dictionary=NULL)",
            Box::new(|| unsafe {
                let entries = [text_entry(TERM, None)];
                let mut inserted = 0usize;
                ldict_dictionary_insert_text_batch(
                    std::ptr::null_mut(),
                    entries.as_ptr(),
                    entries.len(),
                    &mut inserted,
                )
            }),
        ),
        (
            "insert_text_batch(entries=NULL,count=1)",
            Box::new(move || unsafe {
                let mut inserted = 0usize;
                ldict_dictionary_insert_text_batch(dict, std::ptr::null(), 1, &mut inserted)
            }),
        ),
        (
            "insert_text_batch(out=NULL)",
            Box::new(move || unsafe {
                let entries = [text_entry(TERM, None)];
                ldict_dictionary_insert_text_batch(
                    dict,
                    entries.as_ptr(),
                    entries.len(),
                    std::ptr::null_mut(),
                )
            }),
        ),
        (
            "insert_u64_batch(dictionary=NULL)",
            Box::new(|| unsafe {
                let entries = [ffi_common::u64_entry(TOKENS, None)];
                let mut inserted = 0usize;
                ldict_dictionary_insert_u64_batch(
                    std::ptr::null_mut(),
                    entries.as_ptr(),
                    entries.len(),
                    &mut inserted,
                )
            }),
        ),
        (
            "insert_u64_batch(entries=NULL,count=1)",
            Box::new(move || unsafe {
                let mut inserted = 0usize;
                ldict_dictionary_insert_u64_batch(dict, std::ptr::null(), 1, &mut inserted)
            }),
        ),
        (
            "insert_u64_batch(out=NULL)",
            Box::new(move || unsafe {
                let entries = [ffi_common::u64_entry(TOKENS, None)];
                ldict_dictionary_insert_u64_batch(
                    dict,
                    entries.as_ptr(),
                    entries.len(),
                    std::ptr::null_mut(),
                )
            }),
        ),
    ];

    for (name, case) in &cases {
        assert_eq!(
            case(),
            LdictStatus::NullPointer,
            "{name} must fail with NullPointer"
        );
        assert!(
            !last_error().is_empty(),
            "{name} must leave a non-empty error message"
        );
    }
}

/// `len == 0` legalises a null data pointer: the slice helper produces the
/// empty term/pattern without touching `data`.
#[test]
fn zero_length_null_data_is_the_legal_empty_term() {
    let dynamic = DictGuard::dynamic(DOMAIN_BYTE);
    let (status, inserted) = {
        let mut inserted = u8::MAX;
        let status = unsafe {
            ldict_dictionary_insert_text(dynamic.ptr(), std::ptr::null(), 0, some(4), &mut inserted)
        };
        (status, inserted)
    };
    assert_eq!(status, LdictStatus::Ok);
    assert_eq!(inserted, 1, "empty term inserts like any other");

    let mut contains = u8::MAX;
    let status = unsafe {
        ldict_dictionary_contains_text(dynamic.ptr(), std::ptr::null(), 0, &mut contains)
    };
    assert_eq!((status, contains), (LdictStatus::Ok, 1));

    let mut found = u8::MAX;
    let mut value = LdictOptionalU64::default();
    let status = unsafe {
        ldict_dictionary_get_text(dynamic.ptr(), std::ptr::null(), 0, &mut found, &mut value)
    };
    assert_eq!(
        (status, found, value.has_value, value.value),
        (LdictStatus::Ok, 1, 1, 4)
    );

    let mut removed = u8::MAX;
    let status =
        unsafe { ldict_dictionary_remove_text(dynamic.ptr(), std::ptr::null(), 0, &mut removed) };
    assert_eq!((status, removed), (LdictStatus::Ok, 1));

    // u64 domain mirror.
    let tokens = DictGuard::dynamic(DOMAIN_U64);
    let mut inserted = u8::MAX;
    let status = unsafe {
        ldict_dictionary_insert_u64(tokens.ptr(), std::ptr::null(), 0, none(), &mut inserted)
    };
    assert_eq!((status, inserted), (LdictStatus::Ok, 1));

    // Empty substring pattern against an SCDAWG.
    let scdawg = DictGuard::scdawg(DOMAIN_BYTE);
    assert_eq!(insert_text(scdawg.ptr(), b"ab", None).0, LdictStatus::Ok);
    let mut contains = u8::MAX;
    let status = unsafe {
        ldict_scdawg_contains_substring(scdawg.ptr(), std::ptr::null(), 0, &mut contains)
    };
    assert_eq!(
        (status, contains),
        (LdictStatus::Ok, 1),
        "empty pattern is a substring"
    );

    // Zero-count batches never dereference their entry pointers.
    let mut batch_inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text_batch(dynamic.ptr(), std::ptr::null(), 0, &mut batch_inserted)
    };
    assert_eq!((status, batch_inserted), (LdictStatus::Ok, 0));
    let mut batch_inserted = usize::MAX;
    let status = unsafe {
        ldict_dictionary_insert_u64_batch(tokens.ptr(), std::ptr::null(), 0, &mut batch_inserted)
    };
    assert_eq!((status, batch_inserted), (LdictStatus::Ok, 0));
}

// ---------------------------------------------------------------------------
// LDICT-STAT-1: invalid unit domains and malformed optionals.
// ---------------------------------------------------------------------------

#[test]
fn unknown_unit_domains_are_invalid_arguments() {
    for domain in [0u32, 4, u32::MAX] {
        let mut out: *mut LdictDictionary = std::ptr::null_mut();
        assert_eq!(
            unsafe { ldict_dynamic_dawg_new(domain, &mut out) },
            LdictStatus::InvalidArgument,
            "dynamic domain {domain}"
        );
        assert!(out.is_null(), "failed constructor must null its out-param");

        let mut out: *mut LdictDictionary = std::ptr::null_mut();
        assert_eq!(
            unsafe { ldict_double_array_trie_new(domain, std::ptr::null(), 0, &mut out) },
            LdictStatus::InvalidArgument,
            "DAT domain {domain}"
        );
        assert!(out.is_null());

        let mut out: *mut LdictDictionary = std::ptr::null_mut();
        assert_eq!(
            unsafe { ldict_scdawg_new(domain, &mut out) },
            LdictStatus::InvalidArgument,
            "SCDAWG domain {domain}"
        );
        assert!(out.is_null());
    }
}

/// The U64 unit domain exists but is `Unsupported` (not `InvalidArgument`)
/// for the two byte/Unicode-only constructors.
#[test]
fn u64_domain_is_unsupported_for_dat_and_scdawg_constructors() {
    let mut out: *mut LdictDictionary = std::ptr::null_mut();
    assert_eq!(
        unsafe { ldict_double_array_trie_new(DOMAIN_U64, std::ptr::null(), 0, &mut out) },
        LdictStatus::Unsupported
    );
    assert!(out.is_null());

    let mut out: *mut LdictDictionary = std::ptr::null_mut();
    assert_eq!(
        unsafe { ldict_scdawg_new(DOMAIN_U64, &mut out) },
        LdictStatus::Unsupported
    );
    assert!(out.is_null());
}

#[test]
fn malformed_optionals_are_invalid_arguments() {
    let dynamic = DictGuard::dynamic(DOMAIN_BYTE);
    let mut inserted = u8::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text(
            dynamic.ptr(),
            TERM.as_ptr(),
            TERM.len(),
            bad_optional(),
            &mut inserted,
        )
    };
    assert_eq!(status, LdictStatus::InvalidArgument);
    assert_eq!(
        inserted,
        u8::MAX,
        "failed insert must not write its out-param"
    );

    let mut inserted = u8::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text_value(
            dynamic.ptr(),
            TERM.as_ptr(),
            TERM.len(),
            7,
            2,
            &mut inserted,
        )
    };
    assert_eq!(status, LdictStatus::InvalidArgument);

    let tokens = DictGuard::dynamic(DOMAIN_U64);
    let mut inserted = u8::MAX;
    let status = unsafe {
        ldict_dictionary_insert_u64(
            tokens.ptr(),
            TOKENS.as_ptr(),
            TOKENS.len(),
            bad_optional(),
            &mut inserted,
        )
    };
    assert_eq!(status, LdictStatus::InvalidArgument);

    let mut inserted = u8::MAX;
    let status = unsafe {
        ldict_dictionary_insert_u64_value(
            tokens.ptr(),
            TOKENS.as_ptr(),
            TOKENS.len(),
            7,
            2,
            &mut inserted,
        )
    };
    assert_eq!(status, LdictStatus::InvalidArgument);

    // Constructor entries validate the same law.
    let entries = [LdictTextEntry {
        data: TERM.as_ptr(),
        len: TERM.len(),
        value: bad_optional(),
    }];
    let mut out: *mut LdictDictionary = std::ptr::null_mut();
    let status = unsafe {
        ldict_double_array_trie_new(DOMAIN_BYTE, entries.as_ptr(), entries.len(), &mut out)
    };
    assert_eq!(status, LdictStatus::InvalidArgument);
    assert!(out.is_null());
}

/// LDICT-STAT-2 precedence pin: the optional decodes before the handle null
/// check, so `has_value == 2` wins over `dictionary == NULL`.
#[test]
fn optional_decode_precedes_the_handle_null_check() {
    let mut inserted = u8::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text(
            std::ptr::null_mut(),
            TERM.as_ptr(),
            TERM.len(),
            bad_optional(),
            &mut inserted,
        )
    };
    assert_eq!(status, LdictStatus::InvalidArgument);
    assert_eq!(last_error(), "has_value must be zero or one");
}

/// LDICT-STAT-2: nonzero reserved bytes in `LdictOptionalU64` are rejected
/// on both the insert path and the constructor-entry path (the api.json
/// `mustBeZero` law; regression for ledger finding LDICT-B6), while an
/// all-zero reserved block continues to insert cleanly.
#[test]
fn reserved_bytes_must_be_zero() {
    let dictionary = DictGuard::dynamic(DOMAIN_BYTE);
    let mut inserted = u8::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text(
            dictionary.0,
            TERM.as_ptr(),
            TERM.len(),
            dirty_reserved_optional(),
            &mut inserted,
        )
    };
    assert_eq!(status, LdictStatus::InvalidArgument);
    assert_eq!(last_error(), "reserved bytes must be zero");

    // Control: the same insert with clean reserved bytes succeeds.
    let clean = LdictOptionalU64 {
        value: 7,
        has_value: 1,
        reserved: [0; 7],
    };
    let mut inserted = u8::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text(
            dictionary.0,
            TERM.as_ptr(),
            TERM.len(),
            clean,
            &mut inserted,
        )
    };
    assert_eq!(status, LdictStatus::Ok);
    assert_eq!(inserted, 1);

    // Constructor entries validate the same law.
    let entries = [LdictTextEntry {
        data: TERM.as_ptr(),
        len: TERM.len(),
        value: dirty_reserved_optional(),
    }];
    let mut out: *mut LdictDictionary = std::ptr::null_mut();
    let status = unsafe {
        ldict_double_array_trie_new(DOMAIN_BYTE, entries.as_ptr(), entries.len(), &mut out)
    };
    assert_eq!(status, LdictStatus::InvalidArgument);
    assert!(out.is_null());
}

// ---------------------------------------------------------------------------
// LDICT-STAT-1: capability/domain dispatch matrix per backend.
// ---------------------------------------------------------------------------

/// Operations exercised uniformly across every backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Op {
    InsertText,
    RemoveText,
    ContainsText,
    GetText,
    InsertU64,
    RemoveU64,
    ContainsU64,
    GetU64,
    Clear,
    Compact,
    ContainsSubstring,
    SubstringFrequency,
    Checkpoint,
    VocabGetTerm,
}

const OPS: [Op; 14] = [
    Op::InsertText,
    Op::RemoveText,
    Op::ContainsText,
    Op::GetText,
    Op::InsertU64,
    Op::RemoveU64,
    Op::ContainsU64,
    Op::GetU64,
    Op::Clear,
    Op::Compact,
    Op::ContainsSubstring,
    Op::SubstringFrequency,
    Op::Checkpoint,
    Op::VocabGetTerm,
];

fn run_op(dictionary: *mut LdictDictionary, op: Op) -> LdictStatus {
    match op {
        Op::InsertText => insert_text(dictionary, TERM, None).0,
        Op::RemoveText => remove_text(dictionary, TERM).0,
        Op::ContainsText => contains_text(dictionary, TERM).0,
        Op::GetText => get_text(dictionary, TERM).0,
        Op::InsertU64 => insert_u64(dictionary, TOKENS, None).0,
        Op::RemoveU64 => remove_u64(dictionary, TOKENS).0,
        Op::ContainsU64 => contains_u64(dictionary, TOKENS).0,
        Op::GetU64 => {
            let mut found = u8::MAX;
            let mut value = LdictOptionalU64::default();
            unsafe {
                ldict_dictionary_get_u64(
                    dictionary,
                    TOKENS.as_ptr(),
                    TOKENS.len(),
                    &mut found,
                    &mut value,
                )
            }
        }
        Op::Clear => unsafe { ldict_dictionary_clear(dictionary) },
        Op::Compact => {
            let mut reclaimed = usize::MAX;
            unsafe { ldict_dictionary_compact(dictionary, &mut reclaimed) }
        }
        Op::ContainsSubstring => {
            let mut contains = u8::MAX;
            unsafe {
                ldict_scdawg_contains_substring(
                    dictionary,
                    TERM.as_ptr(),
                    TERM.len(),
                    &mut contains,
                )
            }
        }
        Op::SubstringFrequency => {
            let mut frequency = usize::MAX;
            unsafe {
                ldict_scdawg_substring_frequency(
                    dictionary,
                    TERM.as_ptr(),
                    TERM.len(),
                    &mut frequency,
                )
            }
        }
        Op::Checkpoint => unsafe { ldict_dictionary_checkpoint(dictionary) },
        Op::VocabGetTerm => {
            let (mut len, mut found) = (usize::MAX, u8::MAX);
            unsafe {
                ldict_vocab_get_term(dictionary, 0, std::ptr::null_mut(), 0, &mut len, &mut found)
            }
        }
    }
}

const OK: LdictStatus = LdictStatus::Ok;
const UN: LdictStatus = LdictStatus::Unsupported;
const DM: LdictStatus = LdictStatus::DomainMismatch;

fn assert_capability_row(name: &str, dictionary: &DictGuard, expected: [LdictStatus; 14]) {
    for (op, expected) in OPS.iter().zip(expected) {
        let observed = run_op(dictionary.ptr(), *op);
        assert_eq!(
            observed, expected,
            "{name}: {op:?} returned {observed:?}, expected {expected:?}"
        );
        match expected {
            LdictStatus::Ok => {}
            _ => assert!(
                !last_error().is_empty(),
                "{name}: {op:?} failure must set an error message"
            ),
        }
    }
}

#[test]
fn dynamic_dawg_capability_rows_match_the_binding_dispatch() {
    // Text CRUD + clear/compact succeed; u64 CRUD is a domain mismatch;
    // substring, checkpoint, and vocabulary lookups are unsupported.
    let row_text = [OK, OK, OK, OK, DM, DM, DM, DM, OK, OK, UN, UN, UN, UN];
    assert_capability_row(
        "DynamicDawg/Byte",
        &DictGuard::dynamic(DOMAIN_BYTE),
        row_text,
    );
    assert_capability_row(
        "DynamicDawg/Unicode",
        &DictGuard::dynamic(DOMAIN_UNICODE),
        row_text,
    );
    // The U64 dictionary mirrors the mismatch on the text surface.
    let row_u64 = [DM, DM, DM, DM, OK, OK, OK, OK, OK, OK, UN, UN, UN, UN];
    assert_capability_row("DynamicDawg/U64", &DictGuard::dynamic(DOMAIN_U64), row_u64);
}

#[test]
fn double_array_trie_capability_rows_match_the_binding_dispatch() {
    // Read-only: insert/remove/clear/compact are unsupported, u64 CRUD is a
    // domain mismatch, and reads succeed.
    let row = [UN, UN, OK, OK, DM, DM, DM, DM, UN, UN, UN, UN, UN, UN];
    assert_capability_row("DoubleArrayTrie/Byte", &dat(DOMAIN_BYTE, &[TERM]), row);
    assert_capability_row(
        "DoubleArrayTrie/Unicode",
        &dat(DOMAIN_UNICODE, &[TERM]),
        row,
    );
}

#[test]
fn scdawg_capability_rows_match_the_binding_dispatch() {
    // Insert + reads + both substring operations succeed; removal, clear,
    // compact, checkpoint, and vocabulary lookups are unsupported.
    let row = [OK, UN, OK, OK, DM, DM, DM, DM, UN, UN, OK, OK, UN, UN];
    assert_capability_row("Scdawg/Byte", &DictGuard::scdawg(DOMAIN_BYTE), row);
    assert_capability_row("Scdawg/Unicode", &DictGuard::scdawg(DOMAIN_UNICODE), row);
}

#[cfg(not(miri))]
#[test]
fn persistent_capability_rows_match_the_binding_dispatch() {
    let directory = tempfile::tempdir().expect("tempdir");
    // Text CRUD + checkpoint succeed; clear/compact/substring/vocab are
    // unsupported even though the backend is writable.
    let row_text = [OK, OK, OK, OK, DM, DM, DM, DM, UN, UN, UN, UN, OK, UN];
    assert_capability_row(
        "PersistentARTrie/Byte",
        &persistent(directory.path(), "byte.artrie", DOMAIN_BYTE),
        row_text,
    );
    assert_capability_row(
        "PersistentARTrie/Unicode",
        &persistent(directory.path(), "unicode.artrie", DOMAIN_UNICODE),
        row_text,
    );
    let row_u64 = [DM, DM, DM, DM, OK, OK, OK, OK, UN, UN, UN, UN, OK, UN];
    assert_capability_row(
        "PersistentARTrie/U64",
        &persistent(directory.path(), "tokens.artrie", DOMAIN_U64),
        row_u64,
    );
    // The vocabulary rejects removal (append-only index assignment) but
    // supports text insertion, reads, checkpoint, and index lookups.
    let row_vocab = [OK, UN, OK, OK, DM, DM, DM, DM, UN, UN, UN, UN, OK, OK];
    assert_capability_row(
        "PersistentVocab",
        &vocab(directory.path(), "vocab.vocab"),
        row_vocab,
    );
}

// ---------------------------------------------------------------------------
// LDICT-STAT-1: UTF-8 validation versus raw byte-domain acceptance.
// ---------------------------------------------------------------------------

#[test]
fn utf8_validating_paths_reject_malformed_input() {
    // Unicode DynamicDAWG validates every text argument.
    let unicode = DictGuard::dynamic(DOMAIN_UNICODE);
    assert_eq!(
        insert_text(unicode.ptr(), NOT_UTF8, None).0,
        LdictStatus::InvalidUtf8
    );
    assert_eq!(
        remove_text(unicode.ptr(), NOT_UTF8).0,
        LdictStatus::InvalidUtf8
    );
    assert_eq!(
        contains_text(unicode.ptr(), NOT_UTF8).0,
        LdictStatus::InvalidUtf8
    );
    assert_eq!(
        get_text(unicode.ptr(), NOT_UTF8).0,
        LdictStatus::InvalidUtf8
    );

    // The SCDAWG decodes text in BOTH domains: even the byte-transition
    // variant stores UTF-8 strings, so malformed bytes are rejected rather
    // than accepted raw.
    for domain in [DOMAIN_BYTE, DOMAIN_UNICODE] {
        let scdawg = DictGuard::scdawg(domain);
        assert_eq!(
            insert_text(scdawg.ptr(), NOT_UTF8, None).0,
            LdictStatus::InvalidUtf8
        );
        assert_eq!(
            contains_text(scdawg.ptr(), NOT_UTF8).0,
            LdictStatus::InvalidUtf8
        );
        assert_eq!(get_text(scdawg.ptr(), NOT_UTF8).0, LdictStatus::InvalidUtf8);
        let mut contains = u8::MAX;
        let status = unsafe {
            ldict_scdawg_contains_substring(
                scdawg.ptr(),
                NOT_UTF8.as_ptr(),
                NOT_UTF8.len(),
                &mut contains,
            )
        };
        assert_eq!(status, LdictStatus::InvalidUtf8);
        let mut frequency = usize::MAX;
        let status = unsafe {
            ldict_scdawg_substring_frequency(
                scdawg.ptr(),
                NOT_UTF8.as_ptr(),
                NOT_UTF8.len(),
                &mut frequency,
            )
        };
        assert_eq!(status, LdictStatus::InvalidUtf8);
    }

    // The DoubleArrayTrie decodes text in both domains too: construction and
    // queries reject malformed UTF-8 (the byte domain only selects byte
    // transition semantics, not raw byte storage).
    let mut out: *mut LdictDictionary = std::ptr::null_mut();
    let broken = [text_entry(NOT_UTF8, None)];
    let status = unsafe {
        ldict_double_array_trie_new(DOMAIN_BYTE, broken.as_ptr(), broken.len(), &mut out)
    };
    assert_eq!(status, LdictStatus::InvalidUtf8);
    assert!(out.is_null());
    let trie = dat(DOMAIN_BYTE, &[TERM]);
    assert_eq!(
        contains_text(trie.ptr(), NOT_UTF8).0,
        LdictStatus::InvalidUtf8
    );
    assert_eq!(get_text(trie.ptr(), NOT_UTF8).0, LdictStatus::InvalidUtf8);
}

#[test]
fn byte_domain_dynamic_dawg_accepts_embedded_nul_and_high_bytes() {
    let byte = DictGuard::dynamic(DOMAIN_BYTE);
    let (status, inserted) = insert_text(byte.ptr(), RAW_BYTES, Some(11));
    assert_eq!((status, inserted), (LdictStatus::Ok, true));
    let (status, observed) = get_text(byte.ptr(), RAW_BYTES);
    assert_eq!((status, observed), (LdictStatus::Ok, Some(Some(11))));
    let (status, removed) = remove_text(byte.ptr(), RAW_BYTES);
    assert_eq!((status, removed), (LdictStatus::Ok, true));
}

#[cfg(not(miri))]
#[test]
fn byte_domain_persistent_artrie_accepts_embedded_nul_and_high_bytes() {
    let directory = tempfile::tempdir().expect("tempdir");
    let byte = persistent(directory.path(), "raw.artrie", DOMAIN_BYTE);
    let (status, inserted) = insert_text(byte.ptr(), RAW_BYTES, Some(12));
    assert_eq!((status, inserted), (LdictStatus::Ok, true));
    let (status, observed) = get_text(byte.ptr(), RAW_BYTES);
    assert_eq!((status, observed), (LdictStatus::Ok, Some(Some(12))));

    // The Unicode persistent variant validates UTF-8 like its volatile twin.
    let unicode = persistent(directory.path(), "unicode.artrie", DOMAIN_UNICODE);
    assert_eq!(
        insert_text(unicode.ptr(), NOT_UTF8, None).0,
        LdictStatus::InvalidUtf8
    );
    let vocabulary = vocab(directory.path(), "vocab.vocab");
    assert_eq!(
        insert_text(vocabulary.ptr(), NOT_UTF8, None).0,
        LdictStatus::InvalidUtf8
    );
}

// ---------------------------------------------------------------------------
// LDICT-STAT-2: free(NULL) no-op and out-param discipline.
// ---------------------------------------------------------------------------

#[test]
fn free_of_null_is_a_no_op() {
    unsafe { ldict_dictionary_free(std::ptr::null_mut()) };
}

#[test]
fn failed_constructors_null_their_out_params_and_queries_leave_them_alone() {
    // Constructors write NULL before doing anything fallible.
    let sentinel = 0xDEAD_BEEFusize as *mut LdictDictionary;
    let mut out = sentinel;
    let status = unsafe { ldict_dynamic_dawg_new(0, &mut out) };
    assert_eq!(status, LdictStatus::InvalidArgument);
    assert!(out.is_null(), "constructor failure must null the out-param");

    let mut out = sentinel;
    let status = unsafe { ldict_scdawg_new(u32::MAX, &mut out) };
    assert_eq!(status, LdictStatus::InvalidArgument);
    assert!(out.is_null());

    // Query functions leave their out-params untouched on failure.
    let scdawg = DictGuard::scdawg(DOMAIN_BYTE);
    let mut kind = 0xAAu32;
    assert_eq!(
        unsafe { ldict_dictionary_kind(std::ptr::null(), &mut kind) },
        LdictStatus::NullPointer
    );
    assert_eq!(kind, 0xAA, "failed kind query must not write");

    let mut reclaimed = 0xBBusize;
    assert_eq!(
        unsafe { ldict_dictionary_compact(scdawg.ptr(), &mut reclaimed) },
        LdictStatus::Unsupported
    );
    assert_eq!(reclaimed, 0xBB, "unsupported compact must not write");

    let mut inserted = 0xCCu8;
    let unicode = DictGuard::dynamic(DOMAIN_UNICODE);
    assert_eq!(
        insert_textish_invalid(unicode.ptr(), &mut inserted),
        LdictStatus::InvalidUtf8
    );
    assert_eq!(inserted, 0xCC, "failed insert must not write");

    let mut contains = 0xDDu8;
    assert_eq!(
        unsafe {
            ldict_scdawg_contains_substring(unicode.ptr(), TERM.as_ptr(), TERM.len(), &mut contains)
        },
        LdictStatus::Unsupported
    );
    assert_eq!(contains, 0xDD, "unsupported substring query must not write");
}

fn insert_textish_invalid(dictionary: *mut LdictDictionary, out: &mut u8) -> LdictStatus {
    unsafe {
        ldict_dictionary_insert_text(dictionary, NOT_UTF8.as_ptr(), NOT_UTF8.len(), none(), out)
    }
}

// ---------------------------------------------------------------------------
// LDICT-STAT-3: thread-local error-message discipline.
// ---------------------------------------------------------------------------

#[test]
fn failures_set_and_successes_clear_the_thread_local_message() {
    let dynamic = DictGuard::dynamic(DOMAIN_UNICODE);
    assert_eq!(
        insert_text(dynamic.ptr(), NOT_UTF8, None).0,
        LdictStatus::InvalidUtf8
    );
    let message = last_error();
    assert!(!message.is_empty(), "failure must set a message");

    assert_eq!(insert_text(dynamic.ptr(), b"fine", None).0, LdictStatus::Ok);
    assert_eq!(last_error(), "", "success must clear the message");
}

#[test]
fn error_messages_are_thread_local() {
    let dynamic = DictGuard::dynamic(DOMAIN_UNICODE);
    // Establish a known message on the main thread.
    assert_eq!(
        insert_text(dynamic.ptr(), NOT_UTF8, None).0,
        LdictStatus::InvalidUtf8
    );
    let main_message = last_error();
    assert!(!main_message.is_empty());

    // A fresh thread starts with an empty message, produces its own failure,
    // and never disturbs the main thread's slot.
    let pointer = SendPtr(dynamic.ptr());
    let worker_message = std::thread::spawn(move || {
        let pointer = pointer;
        let initial = unsafe { CStr::from_ptr(ldict_last_error_message()) }
            .to_str()
            .expect("utf8")
            .to_owned();
        assert_eq!(initial, "", "a fresh thread starts with an empty message");
        let mut contains = u8::MAX;
        let status = unsafe {
            ldict_scdawg_contains_substring(pointer.0, TERM.as_ptr(), TERM.len(), &mut contains)
        };
        assert_eq!(status, LdictStatus::Unsupported);
        unsafe { CStr::from_ptr(ldict_last_error_message()) }
            .to_str()
            .expect("utf8")
            .to_owned()
    })
    .join()
    .expect("worker thread");

    assert!(!worker_message.is_empty());
    assert_ne!(
        worker_message, main_message,
        "each thread reports its own failure"
    );
    assert_eq!(
        last_error(),
        main_message,
        "thread B's failure must not touch thread A"
    );
}

struct SendPtr(*mut LdictDictionary);
// SAFETY: the DynamicDAWG binding behind the handle is internally synchronized
// (RwLock over an atomically published revision) and advertises
// PARALLEL_REENTRANT; the test joins the worker before dropping the guard.
unsafe impl Send for SendPtr {}

// ---------------------------------------------------------------------------
// LDICT-STAT-4: kind/capability pins and ABI meta constants.
// ---------------------------------------------------------------------------

fn kind_and_capabilities(dictionary: &DictGuard) -> (u32, u64) {
    let mut kind = u32::MAX;
    let mut capabilities = u64::MAX;
    assert_eq!(
        unsafe { ldict_dictionary_kind(dictionary.ptr(), &mut kind) },
        LdictStatus::Ok
    );
    assert_eq!(
        unsafe { ldict_dictionary_capabilities(dictionary.ptr(), &mut capabilities) },
        LdictStatus::Ok
    );
    (kind, capabilities)
}

#[test]
fn in_memory_backends_pin_their_kind_and_capability_constants() {
    for domain in [DOMAIN_BYTE, DOMAIN_UNICODE, DOMAIN_U64] {
        assert_eq!(
            kind_and_capabilities(&DictGuard::dynamic(domain)),
            (
                LDICT_KIND_DYNAMIC_DAWG,
                LDICT_CAP_READ
                    | LDICT_CAP_INSERT
                    | LDICT_CAP_REMOVE
                    | LDICT_CAP_CLEAR
                    | LDICT_CAP_COMPACT
            ),
            "DynamicDawg domain {domain}"
        );
    }
    for domain in [DOMAIN_BYTE, DOMAIN_UNICODE] {
        assert_eq!(
            kind_and_capabilities(&dat(domain, &[TERM])),
            (LDICT_KIND_DOUBLE_ARRAY_TRIE, LDICT_CAP_READ),
            "DoubleArrayTrie domain {domain}"
        );
        assert_eq!(
            kind_and_capabilities(&DictGuard::scdawg(domain)),
            (
                LDICT_KIND_SCDAWG,
                LDICT_CAP_READ | LDICT_CAP_INSERT | LDICT_CAP_SUBSTRING
            ),
            "Scdawg domain {domain}"
        );
    }
}

#[cfg(not(miri))]
#[test]
fn persistent_backends_pin_their_kind_and_capability_constants() {
    let directory = tempfile::tempdir().expect("tempdir");
    for (name, domain) in [
        ("byte.artrie", DOMAIN_BYTE),
        ("unicode.artrie", DOMAIN_UNICODE),
        ("tokens.artrie", DOMAIN_U64),
    ] {
        assert_eq!(
            kind_and_capabilities(&persistent(directory.path(), name, domain)),
            (
                LDICT_KIND_PERSISTENT_ARTRIE,
                LDICT_CAP_READ | LDICT_CAP_INSERT | LDICT_CAP_REMOVE | LDICT_CAP_CHECKPOINT
            ),
            "PersistentARTrie {name}"
        );
    }
    assert_eq!(
        kind_and_capabilities(&vocab(directory.path(), "vocab.vocab")),
        (
            LDICT_KIND_PERSISTENT_VOCAB_ARTRIE,
            LDICT_CAP_READ | LDICT_CAP_INSERT | LDICT_CAP_CHECKPOINT
        )
    );
}

#[test]
fn abi_meta_constants_match_the_binding_model() {
    assert_eq!(ldict_abi_version(), LDICT_ABI_VERSION);
    assert_eq!(ldict_abi_version(), 1);
    assert_eq!(ldict_api_revision(), LDICT_API_REVISION);
    assert_eq!(ldict_api_revision(), 4);
}

// ---------------------------------------------------------------------------
// Persistent path validation (no filesystem access on the failure paths).
// ---------------------------------------------------------------------------

#[test]
fn persistent_path_validation_is_ordered_before_io() {
    // Empty path (len == 0, data NULL is legal for empty input) is an
    // InvalidArgument, not an I/O attempt.
    let mut out: *mut LdictDictionary = std::ptr::null_mut();
    let status =
        unsafe { ldict_persistent_artrie_create(DOMAIN_BYTE, std::ptr::null(), 0, &mut out) };
    assert_eq!(status, LdictStatus::InvalidArgument);
    assert!(out.is_null());
    assert_eq!(last_error(), "path is empty");

    // Malformed UTF-8 in the path is InvalidUtf8.
    let mut out: *mut LdictDictionary = std::ptr::null_mut();
    let status = unsafe {
        ldict_persistent_artrie_open(DOMAIN_BYTE, NOT_UTF8.as_ptr(), NOT_UTF8.len(), &mut out)
    };
    assert_eq!(status, LdictStatus::InvalidUtf8);
    assert!(out.is_null());

    // The unit domain is validated after the path parses.
    let mut out: *mut LdictDictionary = std::ptr::null_mut();
    let status = unsafe { ldict_persistent_artrie_create(0, b"p".as_ptr(), 1, &mut out) };
    assert_eq!(status, LdictStatus::InvalidArgument);
    assert!(out.is_null());
    assert!(last_error().contains("unit domain"));
}

#[cfg(not(miri))]
#[test]
fn vocab_get_term_protocol_covers_size_query_truncation_and_absence() {
    let directory = tempfile::tempdir().expect("tempdir");
    let vocabulary = vocab(directory.path(), "protocol.vocab");
    assert_eq!(
        insert_text(vocabulary.ptr(), "beta".as_bytes(), Some(7)).0,
        LdictStatus::Ok
    );

    // Full copy.
    let mut buffer = [0u8; 8];
    let (mut out_len, mut out_found) = (usize::MAX, u8::MAX);
    let status = unsafe {
        ldict_vocab_get_term(
            vocabulary.ptr(),
            7,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut out_len,
            &mut out_found,
        )
    };
    assert_eq!((status, out_found, out_len), (LdictStatus::Ok, 1, 4));
    assert_eq!(&buffer[..4], b"beta");

    // Size query: NULL data with zero capacity reports the byte count.
    let (mut out_len, mut out_found) = (usize::MAX, u8::MAX);
    let status = unsafe {
        ldict_vocab_get_term(
            vocabulary.ptr(),
            7,
            std::ptr::null_mut(),
            0,
            &mut out_len,
            &mut out_found,
        )
    };
    assert_eq!((status, out_found, out_len), (LdictStatus::Ok, 1, 4));

    // Truncation: a short buffer receives a `capacity`-byte prefix and the
    // call fails with LimitExceeded while still reporting the full length.
    let mut short = [0xAAu8; 2];
    let (mut out_len, mut out_found) = (usize::MAX, u8::MAX);
    let status = unsafe {
        ldict_vocab_get_term(
            vocabulary.ptr(),
            7,
            short.as_mut_ptr(),
            short.len(),
            &mut out_len,
            &mut out_found,
        )
    };
    assert_eq!(
        (status, out_found, out_len),
        (LdictStatus::LimitExceeded, 1, 4)
    );
    assert_eq!(&short, b"be", "prefix copy up to capacity");
    assert!(last_error().contains("4 bytes"));

    // Non-null data with zero capacity is NOT a size query: it fails with
    // LimitExceeded and copies nothing.
    let mut untouched = [0x55u8; 1];
    let (mut out_len, mut out_found) = (usize::MAX, u8::MAX);
    let status = unsafe {
        ldict_vocab_get_term(
            vocabulary.ptr(),
            7,
            untouched.as_mut_ptr(),
            0,
            &mut out_len,
            &mut out_found,
        )
    };
    assert_eq!(
        (status, out_found, out_len),
        (LdictStatus::LimitExceeded, 1, 4)
    );
    assert_eq!(untouched, [0x55], "zero capacity copies nothing");

    // Absent index: Ok with found == 0 and len == 0.
    let (mut out_len, mut out_found) = (usize::MAX, u8::MAX);
    let status = unsafe {
        ldict_vocab_get_term(
            vocabulary.ptr(),
            999,
            std::ptr::null_mut(),
            0,
            &mut out_len,
            &mut out_found,
        )
    };
    assert_eq!((status, out_found, out_len), (LdictStatus::Ok, 0, 0));
}
