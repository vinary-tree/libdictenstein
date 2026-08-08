//! Shared helpers for the `ldict_*` producer C-ABI test suite.
//!
//! Not a test target itself: cargo only builds top-level files in `tests/` as
//! integration-test crates, so this module is included by each `ffi_*` test
//! file via `mod ffi_common;`. Every helper drives the REAL extern "C"
//! surface in `src/ffi.rs` plus the raw `vt.dictionary.v1` vtables emitted by
//! `src/bindings.rs` — no Rust-side shortcuts around the ABI.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::ffi::{c_void, CStr};

use libdictenstein::ffi::{
    ldict_dictionary_contains_text, ldict_dictionary_contains_u64, ldict_dictionary_free,
    ldict_dictionary_get_text, ldict_dictionary_get_u64, ldict_dictionary_insert_text,
    ldict_dictionary_insert_u64, ldict_dictionary_remove_text, ldict_dictionary_remove_u64,
    ldict_dictionary_resource, ldict_dynamic_dawg_new, ldict_last_error_message, ldict_scdawg_new,
    LdictDictionary, LdictOptionalU64, LdictStatus, LdictTextEntry, LdictU64Entry,
};
use vinary_tree_interop::{
    VtDictionaryEdge, VtDictionaryVTable, VtResource, VtStatus, VT_DICTIONARY_INTERFACE_ID,
    VT_DICTIONARY_INTERFACE_VERSION,
};

/// `BindingUnitDomain::Byte` as it crosses the C ABI.
pub const DOMAIN_BYTE: u32 = 1;
/// `BindingUnitDomain::UnicodeScalar` as it crosses the C ABI.
pub const DOMAIN_UNICODE: u32 = 2;
/// `BindingUnitDomain::U64` as it crosses the C ABI.
pub const DOMAIN_U64: u32 = 3;

/// Present optional value.
pub fn some(value: u64) -> LdictOptionalU64 {
    LdictOptionalU64 {
        value,
        has_value: 1,
        reserved: [0; 7],
    }
}

/// Absent optional value.
pub fn none() -> LdictOptionalU64 {
    LdictOptionalU64 {
        value: 0,
        has_value: 0,
        reserved: [0; 7],
    }
}

/// Malformed optional value (`has_value == 2`); must be rejected with
/// `LdictStatus::InvalidArgument`.
pub fn bad_optional() -> LdictOptionalU64 {
    LdictOptionalU64 {
        value: 7,
        has_value: 2,
        reserved: [0; 7],
    }
}

/// Optional value with nonzero reserved bytes; must be rejected with
/// `LdictStatus::InvalidArgument` (`mustBeZero` law from bindings/api.json,
/// the producer-side parallel of interop VT-ABI-5).
pub fn dirty_reserved_optional() -> LdictOptionalU64 {
    LdictOptionalU64 {
        value: 7,
        has_value: 1,
        reserved: [1, 0, 0, 0, 0, 0, 0],
    }
}

/// Read the calling thread's last ABI error message.
pub fn last_error() -> String {
    let pointer = ldict_last_error_message();
    assert!(
        !pointer.is_null(),
        "ldict_last_error_message must never return null"
    );
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .expect("error message must be valid UTF-8")
        .to_owned()
}

/// Owning guard over one `LdictDictionary` handle.
pub struct DictGuard(pub *mut LdictDictionary);

impl DictGuard {
    /// Construct an empty DynamicDAWG for `domain`, asserting success.
    pub fn dynamic(domain: u32) -> Self {
        let mut handle: *mut LdictDictionary = std::ptr::null_mut();
        let status = unsafe { ldict_dynamic_dawg_new(domain, &mut handle) };
        assert_eq!(status, LdictStatus::Ok, "DynamicDAWG constructor failed");
        assert!(!handle.is_null());
        Self(handle)
    }

    /// Construct an empty SCDAWG for `domain`, asserting success.
    pub fn scdawg(domain: u32) -> Self {
        let mut handle: *mut LdictDictionary = std::ptr::null_mut();
        let status = unsafe { ldict_scdawg_new(domain, &mut handle) };
        assert_eq!(status, LdictStatus::Ok, "SCDAWG constructor failed");
        assert!(!handle.is_null());
        Self(handle)
    }

    /// The raw handle pointer.
    pub fn ptr(&self) -> *mut LdictDictionary {
        self.0
    }

    /// Borrow the retained `vt.dictionary.v1` resource words.
    pub fn resource(&self) -> VtResource {
        let mut resource = VtResource::NULL;
        let status = unsafe { ldict_dictionary_resource(self.0, &mut resource) };
        assert_eq!(status, LdictStatus::Ok, "resource borrow failed");
        assert!(!resource.is_null());
        resource
    }
}

impl Drop for DictGuard {
    fn drop(&mut self) {
        unsafe { ldict_dictionary_free(self.0) };
    }
}

/// Insert one text term; returns `(status, inserted-flag)`.
pub fn insert_text(
    dictionary: *mut LdictDictionary,
    term: &[u8],
    value: Option<u64>,
) -> (LdictStatus, bool) {
    let mut inserted: u8 = u8::MAX;
    let status = unsafe {
        ldict_dictionary_insert_text(
            dictionary,
            term.as_ptr(),
            term.len(),
            value.map_or_else(none, some),
            &mut inserted,
        )
    };
    (status, inserted == 1)
}

/// Remove one text term; returns `(status, removed-flag)`.
pub fn remove_text(dictionary: *mut LdictDictionary, term: &[u8]) -> (LdictStatus, bool) {
    let mut removed: u8 = u8::MAX;
    let status = unsafe {
        ldict_dictionary_remove_text(dictionary, term.as_ptr(), term.len(), &mut removed)
    };
    (status, removed == 1)
}

/// Membership for one text term; returns `(status, contains-flag)`.
pub fn contains_text(dictionary: *mut LdictDictionary, term: &[u8]) -> (LdictStatus, bool) {
    let mut contains: u8 = u8::MAX;
    let status = unsafe {
        ldict_dictionary_contains_text(dictionary, term.as_ptr(), term.len(), &mut contains)
    };
    (status, contains == 1)
}

/// Lookup for one text term; `Ok(None)` means absent, `Ok(Some(v))` means
/// present with optional value `v`.
#[allow(clippy::type_complexity)]
pub fn get_text(
    dictionary: *mut LdictDictionary,
    term: &[u8],
) -> (LdictStatus, Option<Option<u64>>) {
    let mut found: u8 = u8::MAX;
    let mut value = LdictOptionalU64::default();
    let status = unsafe {
        ldict_dictionary_get_text(
            dictionary,
            term.as_ptr(),
            term.len(),
            &mut found,
            &mut value,
        )
    };
    let observed = match (status, found) {
        (LdictStatus::Ok, 1) => Some(decode_optional(value)),
        _ => None,
    };
    (status, observed)
}

/// Insert one u64-token term; returns `(status, inserted-flag)`.
pub fn insert_u64(
    dictionary: *mut LdictDictionary,
    term: &[u64],
    value: Option<u64>,
) -> (LdictStatus, bool) {
    let mut inserted: u8 = u8::MAX;
    let status = unsafe {
        ldict_dictionary_insert_u64(
            dictionary,
            term.as_ptr(),
            term.len(),
            value.map_or_else(none, some),
            &mut inserted,
        )
    };
    (status, inserted == 1)
}

/// Remove one u64-token term; returns `(status, removed-flag)`.
pub fn remove_u64(dictionary: *mut LdictDictionary, term: &[u64]) -> (LdictStatus, bool) {
    let mut removed: u8 = u8::MAX;
    let status =
        unsafe { ldict_dictionary_remove_u64(dictionary, term.as_ptr(), term.len(), &mut removed) };
    (status, removed == 1)
}

/// Membership for one u64-token term; returns `(status, contains-flag)`.
pub fn contains_u64(dictionary: *mut LdictDictionary, term: &[u64]) -> (LdictStatus, bool) {
    let mut contains: u8 = u8::MAX;
    let status = unsafe {
        ldict_dictionary_contains_u64(dictionary, term.as_ptr(), term.len(), &mut contains)
    };
    (status, contains == 1)
}

/// Lookup for one u64-token term with the same shape as [`get_text`].
#[allow(clippy::type_complexity)]
pub fn get_u64(
    dictionary: *mut LdictDictionary,
    term: &[u64],
) -> (LdictStatus, Option<Option<u64>>) {
    let mut found: u8 = u8::MAX;
    let mut value = LdictOptionalU64::default();
    let status = unsafe {
        ldict_dictionary_get_u64(
            dictionary,
            term.as_ptr(),
            term.len(),
            &mut found,
            &mut value,
        )
    };
    let observed = match (status, found) {
        (LdictStatus::Ok, 1) => Some(decode_optional(value)),
        _ => None,
    };
    (status, observed)
}

/// Decode an ABI optional, asserting the zero-or-one law.
pub fn decode_optional(value: LdictOptionalU64) -> Option<u64> {
    match value.has_value {
        0 => None,
        1 => Some(value.value),
        other => panic!("producer emitted has_value == {other}"),
    }
}

/// Build a borrowed text-entry descriptor.
pub fn text_entry(term: &[u8], value: Option<u64>) -> LdictTextEntry {
    LdictTextEntry {
        data: term.as_ptr(),
        len: term.len(),
        value: value.map_or_else(none, some),
    }
}

/// Build a borrowed u64-entry descriptor.
pub fn u64_entry(term: &[u64], value: Option<u64>) -> LdictU64Entry {
    LdictU64Entry {
        data: term.as_ptr(),
        len: term.len(),
        value: value.map_or_else(none, some),
    }
}

// ---------------------------------------------------------------------------
// Raw vt.dictionary.v1 consumer layer.
// ---------------------------------------------------------------------------

/// Negotiate the dictionary interface vtable for a resource.
///
/// The returned reference is genuinely `'static`: every producer vtable in
/// `src/bindings.rs` is a `static` item.
pub fn dictionary_interface(resource: VtResource) -> &'static VtDictionaryVTable {
    let mut vtable: *const c_void = std::ptr::null();
    let base = unsafe { &*resource.vtable };
    let status = unsafe {
        (base
            .query_interface
            .expect("producer publishes query_interface"))(
            resource.context,
            &VT_DICTIONARY_INTERFACE_ID,
            VT_DICTIONARY_INTERFACE_VERSION,
            &mut vtable,
        )
    };
    assert_eq!(status, VtStatus::Ok, "interface negotiation failed");
    assert!(!vtable.is_null());
    unsafe { &*vtable.cast::<VtDictionaryVTable>() }
}

/// Add one owned retain through the base vtable.
pub fn vt_retain(resource: VtResource) {
    let base = unsafe { &*resource.vtable };
    unsafe { (base.retain.expect("producer publishes retain"))(resource.context) };
}

/// Release one owned retain through the base vtable.
pub fn vt_release(resource: VtResource) {
    let base = unsafe { &*resource.vtable };
    unsafe { (base.release.expect("producer publishes release"))(resource.context) };
}

/// Owned snapshot resource; releases its single retain on drop.
pub struct SnapshotGuard {
    pub resource: VtResource,
}

impl Drop for SnapshotGuard {
    fn drop(&mut self) {
        vt_release(self.resource);
    }
}

/// Capture an immutable snapshot resource from any dictionary resource.
pub fn capture_snapshot(resource: VtResource) -> SnapshotGuard {
    let vtable = dictionary_interface(resource);
    let mut captured = VtResource::NULL;
    let status = unsafe {
        (vtable.snapshot.expect("producer publishes snapshot"))(resource.context, &mut captured)
    };
    assert_eq!(status, VtStatus::Ok, "snapshot capture failed");
    assert!(!captured.is_null(), "captured snapshot is null");
    SnapshotGuard { resource: captured }
}

/// Read the root node of an immutable snapshot resource.
pub fn snapshot_root(snapshot: VtResource) -> u64 {
    let vtable = dictionary_interface(snapshot);
    let mut root = u64::MAX;
    let status =
        unsafe { (vtable.root.expect("producer publishes root"))(snapshot.context, &mut root) };
    assert_eq!(status, VtStatus::Ok, "root read failed");
    root
}

/// Read `(len, known)` from an immutable snapshot resource.
pub fn snapshot_len(snapshot: VtResource) -> (usize, bool) {
    let vtable = dictionary_interface(snapshot);
    let mut len = usize::MAX;
    let mut known: u8 = u8::MAX;
    let status = unsafe {
        (vtable.len.expect("producer publishes len"))(snapshot.context, &mut len, &mut known)
    };
    assert_eq!(status, VtStatus::Ok, "len read failed");
    (len, known == 1)
}

/// One raw `node_edges` call; returns `(status, page, written, total)`.
pub fn edges_page(
    vtable: &VtDictionaryVTable,
    snapshot: VtResource,
    node: u64,
    start: usize,
    capacity: usize,
) -> (VtStatus, Vec<VtDictionaryEdge>, usize, usize) {
    let mut page = vec![VtDictionaryEdge::default(); capacity];
    let mut written = usize::MAX;
    let mut total = usize::MAX;
    let out_edges = match capacity {
        0 => std::ptr::null_mut(),
        _ => page.as_mut_ptr(),
    };
    let status = unsafe {
        (vtable.node_edges.expect("producer publishes node_edges"))(
            snapshot.context,
            node,
            start,
            out_edges,
            capacity,
            &mut written,
            &mut total,
        )
    };
    match status {
        VtStatus::Ok => {
            assert!(written <= capacity, "written exceeds capacity");
            page.truncate(written);
            (status, page, written, total)
        }
        _ => (status, Vec::new(), written, total),
    }
}

/// Enumerate every edge of `node` by paging with `capacity`.
pub fn all_edges(
    vtable: &VtDictionaryVTable,
    snapshot: VtResource,
    node: u64,
    capacity: usize,
) -> Vec<VtDictionaryEdge> {
    assert!(capacity > 0, "paging capacity must be positive");
    let (status, _, _, total) = edges_page(vtable, snapshot, node, 0, 0);
    assert_eq!(status, VtStatus::Ok);
    let mut edges = Vec::with_capacity(total);
    let mut start = 0usize;
    while start < total {
        let (status, page, written, page_total) =
            edges_page(vtable, snapshot, node, start, capacity);
        assert_eq!(status, VtStatus::Ok);
        assert_eq!(page_total, total, "out_total drifted between pages");
        assert!(written > 0, "paging made no progress before total");
        edges.extend_from_slice(&page);
        start += written;
    }
    edges
}

/// Follow one labelled edge; returns `(status, child-when-found)`.
pub fn transition(
    vtable: &VtDictionaryVTable,
    snapshot: VtResource,
    node: u64,
    label: u64,
) -> (VtStatus, Option<u64>) {
    let mut child = u64::MAX;
    let mut found: u8 = u8::MAX;
    let status = unsafe {
        (vtable
            .node_transition
            .expect("producer publishes node_transition"))(
            snapshot.context,
            node,
            label,
            &mut child,
            &mut found,
        )
    };
    let observed = match (status, found) {
        (VtStatus::Ok, 1) => Some(child),
        _ => None,
    };
    (status, observed)
}

/// Read node finality; returns `(status, is-final)`.
pub fn node_is_final(
    vtable: &VtDictionaryVTable,
    snapshot: VtResource,
    node: u64,
) -> (VtStatus, bool) {
    let mut is_final: u8 = u8::MAX;
    let status = unsafe {
        (vtable
            .node_is_final
            .expect("producer publishes node_is_final"))(
            snapshot.context, node, &mut is_final
        )
    };
    (status, is_final == 1)
}

/// Read a node's optional value; returns `(status, value)`.
pub fn node_value(
    vtable: &VtDictionaryVTable,
    snapshot: VtResource,
    node: u64,
) -> (VtStatus, Option<u64>) {
    let mut value = vinary_tree_interop::VtOptionalU64::default();
    let status = unsafe {
        (vtable
            .node_value_u64
            .expect("producer publishes node_value_u64"))(snapshot.context, node, &mut value)
    };
    let observed = match (status, value.has_value) {
        (VtStatus::Ok, 1) => Some(value.value),
        _ => None,
    };
    (status, observed)
}

/// Depth-first walk of an immutable snapshot: label-path -> final-node value.
///
/// Keys are ABI label paths (`Vec<u64>`), which represent bytes, Unicode
/// scalar values, or u64 tokens depending on the snapshot's unit domain.
pub fn walk_terms(snapshot: VtResource, capacity: usize) -> BTreeMap<Vec<u64>, Option<u64>> {
    let vtable = dictionary_interface(snapshot);
    let root = snapshot_root(snapshot);
    let mut terms = BTreeMap::new();
    let mut stack = vec![(root, Vec::new())];
    while let Some((node, path)) = stack.pop() {
        let (status, is_final) = node_is_final(vtable, snapshot, node);
        assert_eq!(status, VtStatus::Ok, "is_final failed during walk");
        if is_final {
            let (status, value) = node_value(vtable, snapshot, node);
            assert_eq!(status, VtStatus::Ok, "value read failed during walk");
            terms.insert(path.clone(), value);
        }
        for edge in all_edges(vtable, snapshot, node, capacity) {
            let mut child_path = Vec::with_capacity(path.len() + 1);
            child_path.extend_from_slice(&path);
            child_path.push(edge.label);
            stack.push((edge.node, child_path));
        }
    }
    terms
}

/// Encode a UTF-8 string as its Unicode-scalar ABI label path.
pub fn unicode_labels(term: &str) -> Vec<u64> {
    term.chars().map(u64::from).collect()
}

/// Encode a byte string as its byte-domain ABI label path.
pub fn byte_labels(term: &[u8]) -> Vec<u64> {
    term.iter().copied().map(u64::from).collect()
}
