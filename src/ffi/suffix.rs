//! Typed suffix-source index ABI. This is deliberately not a dictionary handle:
//! source records have multiplicity, and substring membership is not key lookup.

use super::{boundary, domain, slice, LdictOptionalU64, LdictStatus};
use crate::bindings::BindingUnitDomain;
use crate::suffix_automaton::{SuffixAutomaton, SuffixAutomatonChar, SuffixSourceSnapshot};
use crate::MutableMappedDictionary;
use arc_swap::ArcSwapOption;
use std::ptr;
use std::sync::Arc;

enum IndexState {
    Byte(SuffixAutomaton<u64>),
    Unicode(SuffixAutomatonChar<u64>),
}

enum SnapshotState {
    Byte(SuffixSourceSnapshot<u8, u64>),
    Unicode(SuffixSourceSnapshot<char, u64>),
}

/// Opaque suffix-index handle. `close` releases its state without deallocating
/// the handle, permitting a defined CLOSED result; `free` deallocates it.
pub struct LdictSuffixIndex {
    state: ArcSwapOption<IndexState>,
}

/// Opaque retained source revision, independent of later index mutations.
pub struct LdictSuffixSnapshot {
    state: ArcSwapOption<SnapshotState>,
}

/// One active source record in a lexicographic page. Equal texts are ordered
/// by source ID. IDs are stable within a captured revision but may be reused
/// after clear. `data` is borrowed until this snapshot is closed/freed;
/// the caller owns only the descriptor array.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LdictSuffixSourceRecord {
    pub source_id: u64,
    pub data: *const u8,
    pub len: usize,
    pub value: LdictOptionalU64,
}

impl Default for LdictSuffixSourceRecord {
    fn default() -> Self {
        Self {
            source_id: 0,
            data: ptr::null(),
            len: 0,
            value: LdictOptionalU64::default(),
        }
    }
}

type Failure = (LdictStatus, String);

fn closed() -> Failure {
    (LdictStatus::Closed, "suffix resource is closed".into())
}

unsafe fn load_index(handle: *const LdictSuffixIndex) -> Result<Arc<IndexState>, Failure> {
    let handle = handle
        .as_ref()
        .ok_or((LdictStatus::NullPointer, "index is null".into()))?;
    handle.state.load_full().ok_or_else(closed)
}

unsafe fn load_snapshot(handle: *const LdictSuffixSnapshot) -> Result<Arc<SnapshotState>, Failure> {
    let handle = handle
        .as_ref()
        .ok_or((LdictStatus::NullPointer, "snapshot is null".into()))?;
    handle.state.load_full().ok_or_else(closed)
}

unsafe fn text<'a>(data: *const u8, len: usize) -> Result<&'a str, Failure> {
    std::str::from_utf8(slice(data, len, "data")?)
        .map_err(|error| (LdictStatus::InvalidUtf8, error.to_string()))
}

fn size(value: usize) -> Result<u64, Failure> {
    u64::try_from(value).map_err(|_| (LdictStatus::LimitExceeded, "count exceeds u64".into()))
}

/// Construct a byte-transition or Unicode-scalar suffix index. Source text is
/// valid UTF-8 in both modes; u64-token sources are not supported.
///
/// # Safety
/// `out_index` must be a writable pointer.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_new(
    unit_domain: u32,
    out_index: *mut *mut LdictSuffixIndex,
) -> LdictStatus {
    boundary(|| {
        if out_index.is_null() {
            return Err((LdictStatus::NullPointer, "out_index is null".into()));
        }
        out_index.write(ptr::null_mut());
        let state = match domain(unit_domain)? {
            BindingUnitDomain::Byte => IndexState::Byte(SuffixAutomaton::new()),
            BindingUnitDomain::UnicodeScalar => IndexState::Unicode(SuffixAutomatonChar::new()),
            BindingUnitDomain::U64 => {
                return Err((
                    LdictStatus::Unsupported,
                    "u64 suffix sources are unsupported".into(),
                ))
            }
        };
        out_index.write(Box::into_raw(Box::new(LdictSuffixIndex {
            state: ArcSwapOption::new(Some(Arc::new(state))),
        })));
        Ok(LdictStatus::Ok)
    })
}

/// Close index state while keeping the opaque handle allocated. Captured
/// snapshots remain valid.
///
/// # Safety
/// `index` must be null or an allocated suffix-index handle.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_close(index: *mut LdictSuffixIndex) -> LdictStatus {
    boundary(|| {
        let handle = index
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "index is null".into()))?;
        Ok(if handle.state.swap(None).is_some() {
            LdictStatus::Ok
        } else {
            LdictStatus::Closed
        })
    })
}

/// Deallocate an index handle; no operation on its pointer is valid afterward.
///
/// # Safety
/// `index` must be null or a live allocated handle, freed at most once.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_free(index: *mut LdictSuffixIndex) {
    if !index.is_null() {
        drop(Box::from_raw(index));
    }
}

/// Append one source record, even when its text equals an existing record.
///
/// # Safety
/// `index` and `data` must be valid for their declared lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_insert_text(
    index: *mut LdictSuffixIndex,
    data: *const u8,
    len: usize,
    value: LdictOptionalU64,
) -> LdictStatus {
    boundary(|| {
        let state = load_index(index)?;
        let text = text(data, len)?;
        let value = value.decode()?;
        match state.as_ref() {
            IndexState::Byte(index) => match value {
                Some(value) => {
                    index.insert_with_value(text, value);
                }
                None => {
                    index.insert(text);
                }
            },
            IndexState::Unicode(index) => match value {
                Some(value) => {
                    index.insert_with_value(text, value);
                }
                None => {
                    index.insert(text);
                }
            },
        }
        Ok(LdictStatus::Ok)
    })
}

/// Remove the oldest active insertion record with exactly this source text.
///
/// # Safety
/// Input and output pointers must be valid for their lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_remove_text(
    index: *mut LdictSuffixIndex,
    data: *const u8,
    len: usize,
    out_removed: *mut u8,
) -> LdictStatus {
    boundary(|| {
        if out_removed.is_null() {
            return Err((LdictStatus::NullPointer, "out_removed is null".into()));
        }
        out_removed.write(0);
        let state = load_index(index)?;
        let text = text(data, len)?;
        let removed = match state.as_ref() {
            IndexState::Byte(index) => index.remove(text),
            IndexState::Unicode(index) => index.remove(text),
        };
        out_removed.write(u8::from(removed));
        Ok(LdictStatus::Ok)
    })
}

/// Clear all active source records; producer identity is retained.
///
/// # Safety
/// `index` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_clear(index: *mut LdictSuffixIndex) -> LdictStatus {
    boundary(|| {
        match load_index(index)?.as_ref() {
            IndexState::Byte(index) => index.clear(),
            IndexState::Unicode(index) => index.clear(),
        }
        Ok(LdictStatus::Ok)
    })
}

/// Compact the index graph. No reclaimed-count claim is made.
///
/// # Safety
/// `index` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_compact(index: *mut LdictSuffixIndex) -> LdictStatus {
    boundary(|| {
        match load_index(index)?.as_ref() {
            IndexState::Byte(index) => index.compact(),
            IndexState::Unicode(index) => index.compact(),
        }
        Ok(LdictStatus::Ok)
    })
}

/// Capture one immutable active-source revision.
///
/// # Safety
/// `index` and `out_snapshot` must be valid pointers.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_snapshot(
    index: *const LdictSuffixIndex,
    out_snapshot: *mut *mut LdictSuffixSnapshot,
) -> LdictStatus {
    boundary(|| {
        if out_snapshot.is_null() {
            return Err((LdictStatus::NullPointer, "out_snapshot is null".into()));
        }
        out_snapshot.write(ptr::null_mut());
        let state = match load_index(index)?.as_ref() {
            IndexState::Byte(index) => SnapshotState::Byte(index.source_snapshot()),
            IndexState::Unicode(index) => SnapshotState::Unicode(index.source_snapshot()),
        };
        out_snapshot.write(Box::into_raw(Box::new(LdictSuffixSnapshot {
            state: ArcSwapOption::new(Some(Arc::new(state))),
        })));
        Ok(LdictStatus::Ok)
    })
}

/// Close a snapshot but keep the handle allocated for defined CLOSED results.
///
/// # Safety
/// `snapshot` must be null or an allocated suffix-snapshot handle.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_snapshot_close(
    snapshot: *mut LdictSuffixSnapshot,
) -> LdictStatus {
    boundary(|| {
        let handle = snapshot
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "snapshot is null".into()))?;
        Ok(if handle.state.swap(None).is_some() {
            LdictStatus::Ok
        } else {
            LdictStatus::Closed
        })
    })
}

/// Deallocate a snapshot handle; no operation on its pointer is valid afterward.
///
/// # Safety
/// `snapshot` must be null or an allocated handle, freed at most once.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_snapshot_free(snapshot: *mut LdictSuffixSnapshot) {
    if !snapshot.is_null() {
        drop(Box::from_raw(snapshot));
    }
}

/// Return type-scoped producer and revision identity. These numbers must not
/// be compared with dictionary snapshot identities across resource families.
///
/// # Safety
/// All pointers must be valid.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_snapshot_identity(
    snapshot: *const LdictSuffixSnapshot,
    out_producer: *mut u64,
    out_revision: *mut u64,
) -> LdictStatus {
    boundary(|| {
        if !out_producer.is_null() {
            out_producer.write(0);
        }
        if !out_revision.is_null() {
            out_revision.write(0);
        }
        if out_producer.is_null() || out_revision.is_null() {
            return Err((LdictStatus::NullPointer, "identity output is null".into()));
        }
        let state = load_snapshot(snapshot)?;
        let (producer, revision) = match state.as_ref() {
            SnapshotState::Byte(view) => (view.producer_id(), view.revision()),
            SnapshotState::Unicode(view) => (view.producer_id(), view.revision()),
        };
        out_producer.write(producer);
        out_revision.write(revision);
        Ok(LdictStatus::Ok)
    })
}

/// Count active insertion records, including equal-text duplicates.
///
/// # Safety
/// All pointers must be valid.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_snapshot_source_count(
    snapshot: *const LdictSuffixSnapshot,
    out_count: *mut u64,
) -> LdictStatus {
    boundary(|| {
        if out_count.is_null() {
            return Err((LdictStatus::NullPointer, "out_count is null".into()));
        }
        out_count.write(0);
        let state = load_snapshot(snapshot)?;
        let count = match state.as_ref() {
            SnapshotState::Byte(view) => view.source_count(),
            SnapshotState::Unicode(view) => view.source_count(),
        };
        out_count.write(size(count)?);
        Ok(LdictStatus::Ok)
    })
}

unsafe fn query(
    snapshot_handle: *const LdictSuffixSnapshot,
    data: *const u8,
    len: usize,
    out_result: *mut u64,
    op: fn(&SnapshotState, &str) -> Result<u64, Failure>,
) -> LdictStatus {
    boundary(|| {
        if out_result.is_null() {
            return Err((LdictStatus::NullPointer, "out_result is null".into()));
        }
        out_result.write(0);
        let state = load_snapshot(snapshot_handle)?;
        let pattern = text(data, len)?;
        out_result.write(op(state.as_ref(), pattern)?);
        Ok(LdictStatus::Ok)
    })
}

/// Exact active-source membership, not graph-path reachability.
///
/// # Safety
/// Input/output pointers must be valid for their lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_snapshot_contains_source(
    snapshot: *const LdictSuffixSnapshot,
    data: *const u8,
    len: usize,
    out_contains: *mut u64,
) -> LdictStatus {
    query(snapshot, data, len, out_contains, |state, text| {
        Ok(u64::from(match state {
            SnapshotState::Byte(view) => view.contains_source(text),
            SnapshotState::Unicode(view) => view.contains_source(text),
        }))
    })
}

/// Substring membership in currently active source records.
///
/// # Safety
/// Input/output pointers must be valid for their lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_snapshot_contains_substring(
    snapshot: *const LdictSuffixSnapshot,
    data: *const u8,
    len: usize,
    out_contains: *mut u64,
) -> LdictStatus {
    query(snapshot, data, len, out_contains, |state, text| {
        Ok(u64::from(match state {
            SnapshotState::Byte(view) => view.contains_substring(text),
            SnapshotState::Unicode(view) => view.contains_substring(text),
        }))
    })
}

/// Overlapping occurrence count across active records. Empty pattern counts
/// each transition-unit boundary, including one in an empty source.
///
/// # Safety
/// Input/output pointers must be valid for their lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_snapshot_substring_frequency(
    snapshot: *const LdictSuffixSnapshot,
    data: *const u8,
    len: usize,
    out_frequency: *mut u64,
) -> LdictStatus {
    query(snapshot, data, len, out_frequency, |state, text| {
        size(match state {
            SnapshotState::Byte(view) => view.substring_frequency(text),
            SnapshotState::Unicode(view) => view.substring_frequency(text),
        })
    })
}

/// Fill a lexicographic page of active source records. Equal texts retain
/// insertion-ID order. `capacity == 0` permits `out_records == NULL` to query
/// total count. At or beyond the end, returns END with no descriptors.
///
/// # Safety
/// Output descriptors must be writable for `capacity`; borrowed text must not
/// be used after closing/freeing this snapshot.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_snapshot_source_page(
    snapshot: *const LdictSuffixSnapshot,
    offset: u64,
    out_records: *mut LdictSuffixSourceRecord,
    capacity: usize,
    out_written: *mut usize,
    out_total: *mut u64,
) -> LdictStatus {
    boundary(|| {
        if !out_written.is_null() {
            out_written.write(0);
        }
        if !out_total.is_null() {
            out_total.write(0);
        }
        if !out_records.is_null() && capacity != 0 {
            for descriptor in std::slice::from_raw_parts_mut(out_records, capacity) {
                *descriptor = LdictSuffixSourceRecord::default();
            }
        }
        if out_written.is_null() || out_total.is_null() || (capacity != 0 && out_records.is_null())
        {
            return Err((
                LdictStatus::NullPointer,
                "source page output is null".into(),
            ));
        }
        let state = load_snapshot(snapshot)?;
        let total = match state.as_ref() {
            SnapshotState::Byte(view) => view.source_count(),
            SnapshotState::Unicode(view) => view.source_count(),
        };
        out_total.write(size(total)?);
        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        if offset >= total {
            return Ok(LdictStatus::End);
        }
        let written = capacity.min(total - offset);
        let mut descriptors = Vec::with_capacity(written);
        for rank in 0..written {
            let record = match state.as_ref() {
                SnapshotState::Byte(view) => view
                    .source_at(offset + rank)
                    .map(|record| (record.source_id, record.text, record.value)),
                SnapshotState::Unicode(view) => view
                    .source_at(offset + rank)
                    .map(|record| (record.source_id, record.text, record.value)),
            }
            .expect("rank is within source_count");
            descriptors.push(LdictSuffixSourceRecord {
                source_id: size(record.0)?,
                data: record.1.as_ptr(),
                len: record.1.len(),
                value: LdictOptionalU64::encode(record.2.copied()),
            });
        }
        if written != 0 {
            ptr::copy_nonoverlapping(descriptors.as_ptr(), out_records, written);
        }
        out_written.write(written);
        Ok(LdictStatus::Ok)
    })
}
