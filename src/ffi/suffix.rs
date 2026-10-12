//! Typed suffix-source index ABI. This is deliberately not a dictionary handle:
//! source records have multiplicity, and substring membership is not key lookup.

use super::{boundary, domain, persistent_path, slice, LdictOptionalU64, LdictStatus};
use crate::bindings::BindingUnitDomain;
use crate::persistent_artrie::scdawg::PersistentScdawgSourceView;
use crate::persistent_artrie::suffix_automaton::PersistentSuffixAutomatonSourceView;
use crate::persistent_artrie::suffix_tree::PersistentSuffixTreeSourceView;
use crate::persistent_artrie::{
    PersistentScdawg, PersistentScdawgChar, PersistentSuffixAutomaton,
    PersistentSuffixAutomatonChar, PersistentSuffixTree, PersistentSuffixTreeChar, RecoveryReport,
};
use crate::suffix_automaton::{SuffixAutomaton, SuffixAutomatonChar, SuffixSourceSnapshot};
use crate::MutableMappedDictionary;
use arc_swap::ArcSwapOption;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_PERSISTENT_SUFFIX_PRODUCER: AtomicU64 = AtomicU64::new(1 << 62);

enum IndexState {
    Byte(SuffixAutomaton<u64>),
    Unicode(SuffixAutomatonChar<u64>),
    AutomatonByte(PersistentSuffixAutomaton<u64>),
    AutomatonUnicode(PersistentSuffixAutomatonChar<u64>),
    TreeByte(PersistentSuffixTree<u64>),
    TreeUnicode(PersistentSuffixTreeChar<u64>),
    ScdawgByte(PersistentScdawg<u64>),
    ScdawgUnicode(PersistentScdawgChar<u64>),
}

enum SnapshotState {
    Byte(SuffixSourceSnapshot<u8, u64>),
    Unicode(SuffixSourceSnapshot<char, u64>),
    AutomatonByte(PersistentSuffixAutomatonSourceView<u8, u64>, u64),
    AutomatonUnicode(PersistentSuffixAutomatonSourceView<char, u64>, u64),
    TreeByte(PersistentSuffixTreeSourceView<u8, u64>, u64),
    TreeUnicode(PersistentSuffixTreeSourceView<char, u64>, u64),
    ScdawgByte(PersistentScdawgSourceView<u8, u64>, u64),
    ScdawgUnicode(PersistentScdawgSourceView<char, u64>, u64),
}

macro_rules! snapshot_call {
    ($snapshot:expr, $method:ident $(, $arg:expr)*) => {
        match $snapshot {
            SnapshotState::Byte(view) => view.$method($($arg),*),
            SnapshotState::Unicode(view) => view.$method($($arg),*),
            SnapshotState::AutomatonByte(view, _) => view.$method($($arg),*),
            SnapshotState::AutomatonUnicode(view, _) => view.$method($($arg),*),
            SnapshotState::TreeByte(view, _) => view.$method($($arg),*),
            SnapshotState::TreeUnicode(view, _) => view.$method($($arg),*),
            SnapshotState::ScdawgByte(view, _) => view.$method($($arg),*),
            SnapshotState::ScdawgUnicode(view, _) => view.$method($($arg),*),
        }
    };
}

impl SnapshotState {
    fn identity(&self) -> (u64, u64) {
        match self {
            Self::Byte(view) => (view.producer_id(), view.revision()),
            Self::Unicode(view) => (view.producer_id(), view.revision()),
            Self::AutomatonByte(view, producer) => (*producer, view.revision_token()),
            Self::AutomatonUnicode(view, producer) => (*producer, view.revision_token()),
            Self::TreeByte(view, producer) => (*producer, view.revision_token()),
            Self::TreeUnicode(view, producer) => (*producer, view.revision_token()),
            Self::ScdawgByte(view, producer) => (*producer, view.revision_token()),
            Self::ScdawgUnicode(view, producer) => (*producer, view.revision_token()),
        }
    }

    fn source_count(&self) -> usize {
        snapshot_call!(self, source_count)
    }

    fn source_at(&self, rank: usize) -> Option<(u64, &str, Option<&u64>)> {
        match self {
            Self::Byte(view) => view.source_at(rank).and_then(|record| {
                Some((
                    u64::try_from(record.source_id).ok()?,
                    record.text,
                    record.value,
                ))
            }),
            Self::Unicode(view) => view.source_at(rank).and_then(|record| {
                Some((
                    u64::try_from(record.source_id).ok()?,
                    record.text,
                    record.value,
                ))
            }),
            Self::AutomatonByte(view, _) => view.source_at(rank),
            Self::AutomatonUnicode(view, _) => view.source_at(rank),
            Self::TreeByte(view, _) => view.source_at(rank),
            Self::TreeUnicode(view, _) => view.source_at(rank),
            Self::ScdawgByte(view, _) => view.source_at(rank),
            Self::ScdawgUnicode(view, _) => view.source_at(rank),
        }
    }

    fn contains_source(&self, text: &str) -> bool {
        snapshot_call!(self, contains_source, text)
    }

    fn contains_substring(&self, pattern: &str) -> bool {
        snapshot_call!(self, contains_substring, pattern)
    }

    fn substring_frequency(&self, pattern: &str) -> usize {
        snapshot_call!(self, substring_frequency, pattern)
    }
}

/// Opaque suffix-index handle. `close` releases its state without deallocating
/// the handle, permitting a defined CLOSED result; `free` deallocates it.
pub struct LdictSuffixIndex {
    state: ArcSwapOption<IndexState>,
    producer_id: u64,
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

fn persistent_failure(error: impl std::fmt::Display) -> Failure {
    (LdictStatus::IoError, error.to_string())
}

trait PersistentSuffixOps {
    fn insert_source(&self, text: &str, value: Option<u64>) -> Result<(), Failure>;
    fn remove_source(&self, text: &str) -> Result<bool, Failure>;
    fn clear_sources(&self) -> Result<(), Failure>;
    fn compact_sources(&self) -> Result<(), Failure>;
    fn checkpoint(&self) -> Result<(), Failure>;
    fn needs_compaction(&self) -> bool;
}

macro_rules! impl_persistent_suffix_ops {
    ($($ty:ty),+ $(,)?) => {
        $(impl PersistentSuffixOps for $ty {
            fn insert_source(&self, text: &str, value: Option<u64>) -> Result<(), Failure> {
                match value {
                    Some(value) => self.try_insert_with_value(text, value),
                    None => self.try_insert(text),
                }
                .map(|_| ())
                .map_err(persistent_failure)
            }

            fn remove_source(&self, text: &str) -> Result<bool, Failure> {
                self.try_remove(text).map_err(persistent_failure)
            }

            fn clear_sources(&self) -> Result<(), Failure> {
                self.try_clear().map_err(persistent_failure)
            }

            fn compact_sources(&self) -> Result<(), Failure> {
                self.try_compact().map(|_| ()).map_err(persistent_failure)
            }

            fn checkpoint(&self) -> Result<(), Failure> {
                self.checkpoint().map_err(persistent_failure)
            }

            fn needs_compaction(&self) -> bool {
                self.needs_compaction()
            }
        })+
    };
}

impl_persistent_suffix_ops!(
    PersistentSuffixAutomaton<u64>,
    PersistentSuffixAutomatonChar<u64>,
    PersistentSuffixTree<u64>,
    PersistentSuffixTreeChar<u64>,
    PersistentScdawg<u64>,
    PersistentScdawgChar<u64>,
);

impl IndexState {
    fn persistent(&self) -> Option<&dyn PersistentSuffixOps> {
        match self {
            Self::AutomatonByte(index) => Some(index),
            Self::AutomatonUnicode(index) => Some(index),
            Self::TreeByte(index) => Some(index),
            Self::TreeUnicode(index) => Some(index),
            Self::ScdawgByte(index) => Some(index),
            Self::ScdawgUnicode(index) => Some(index),
            Self::Byte(_) | Self::Unicode(_) => None,
        }
    }
}

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
            producer_id: NEXT_PERSISTENT_SUFFIX_PRODUCER.fetch_add(1, Ordering::Relaxed),
        })));
        Ok(LdictStatus::Ok)
    })
}

fn persistent_index(
    family: u32,
    unit_domain: BindingUnitDomain,
    path: &std::path::Path,
    create: bool,
) -> Result<(IndexState, Option<RecoveryReport>), Failure> {
    if unit_domain == BindingUnitDomain::U64 {
        return Err((
            LdictStatus::Unsupported,
            "u64 suffix sources are unsupported".into(),
        ));
    }
    if create && path.exists() {
        return Err((
            LdictStatus::InvalidArgument,
            "persistent suffix store already exists; open it instead".into(),
        ));
    }
    macro_rules! construct {
        ($ty:ty, $variant:ident) => {{
            if create {
                Ok((
                    IndexState::$variant(<$ty>::create(path).map_err(persistent_failure)?),
                    None,
                ))
            } else {
                let (index, report) =
                    <$ty>::open_with_recovery(path).map_err(persistent_failure)?;
                Ok((IndexState::$variant(index), Some(report)))
            }
        }};
    }
    match (family, unit_domain) {
        (1, BindingUnitDomain::Byte) => construct!(PersistentSuffixAutomaton<u64>, AutomatonByte),
        (1, BindingUnitDomain::UnicodeScalar) => {
            construct!(PersistentSuffixAutomatonChar<u64>, AutomatonUnicode)
        }
        (2, BindingUnitDomain::Byte) => construct!(PersistentSuffixTree<u64>, TreeByte),
        (2, BindingUnitDomain::UnicodeScalar) => {
            construct!(PersistentSuffixTreeChar<u64>, TreeUnicode)
        }
        (3, BindingUnitDomain::Byte) => construct!(PersistentScdawg<u64>, ScdawgByte),
        (3, BindingUnitDomain::UnicodeScalar) => {
            construct!(PersistentScdawgChar<u64>, ScdawgUnicode)
        }
        _ => Err((
            LdictStatus::InvalidArgument,
            "unknown persistent suffix family".into(),
        )),
    }
}

/// Create a typed persistent suffix automaton (1), suffix tree (2), or
/// SCDAWG (3), with byte (1) or Unicode-scalar (2) transitions.
///
/// # Safety
/// The path bytes and output slot must be valid for their declared lengths.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_persistent_create(
    family: u32,
    unit_domain: u32,
    path_data: *const u8,
    path_len: usize,
    out_index: *mut *mut LdictSuffixIndex,
) -> LdictStatus {
    boundary(|| {
        if out_index.is_null() {
            return Err((LdictStatus::NullPointer, "out_index is null".into()));
        }
        out_index.write(ptr::null_mut());
        let path = persistent_path(path_data, path_len)?;
        let (state, _) = persistent_index(family, domain(unit_domain)?, path, true)?;
        out_index.write(Box::into_raw(Box::new(LdictSuffixIndex {
            state: ArcSwapOption::new(Some(Arc::new(state))),
            producer_id: NEXT_PERSISTENT_SUFFIX_PRODUCER.fetch_add(1, Ordering::Relaxed),
        })));
        Ok(LdictStatus::Ok)
    })
}

/// Open or recover a typed persistent suffix family and return the native
/// recovery result. A missing path is created with CREATED_NEW mode.
///
/// # Safety
/// The path bytes and distinct output slots must be valid for their lengths.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_persistent_open_with_recovery(
    family: u32,
    unit_domain: u32,
    path_data: *const u8,
    path_len: usize,
    out_index: *mut *mut LdictSuffixIndex,
    out_report: *mut *mut super::recovery::LdictRecoveryReport,
) -> LdictStatus {
    boundary(|| {
        if out_index.is_null() || out_report.is_null() {
            return Err((
                LdictStatus::NullPointer,
                "persistent suffix output is null".into(),
            ));
        }
        out_index.write(ptr::null_mut());
        out_report.write(ptr::null_mut());
        let path = persistent_path(path_data, path_len)?;
        let (state, report) = persistent_index(family, domain(unit_domain)?, path, false)?;
        let report = report.expect("open always returns a recovery report");
        out_index.write(Box::into_raw(Box::new(LdictSuffixIndex {
            state: ArcSwapOption::new(Some(Arc::new(state))),
            producer_id: NEXT_PERSISTENT_SUFFIX_PRODUCER.fetch_add(1, Ordering::Relaxed),
        })));
        out_report.write(Box::into_raw(Box::new(
            super::recovery::LdictRecoveryReport::new(report),
        )));
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
unsafe fn insert_text(
    index: *mut LdictSuffixIndex,
    data: *const u8,
    len: usize,
    value: Option<u64>,
) -> Result<(), Failure> {
    let state = load_index(index)?;
    let text = text(data, len)?;
    if let Some(index) = state.persistent() {
        return index.insert_source(text, value);
    }
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
        _ => unreachable!("persistent variant handled above"),
    }
    Ok(())
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
        insert_text(index, data, len, value.decode()?)?;
        Ok(LdictStatus::Ok)
    })
}

/// Append one source record through scalar arguments. This additive entry
/// point preserves the exact optional-value semantics of the struct-valued
/// call for foreign runtimes that pass C structs only by reference.
///
/// # Safety
/// `index` and `data` must be valid for their declared lengths.
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_insert_text_value(
    index: *mut LdictSuffixIndex,
    data: *const u8,
    len: usize,
    value: u64,
    has_value: u8,
) -> LdictStatus {
    boundary(|| {
        let value = match has_value {
            0 => None,
            1 => Some(value),
            _ => {
                return Err((
                    LdictStatus::InvalidArgument,
                    "has_value must be zero or one".into(),
                ))
            }
        };
        insert_text(index, data, len, value)?;
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
        let removed = if let Some(index) = state.persistent() {
            index.remove_source(text)?
        } else {
            match state.as_ref() {
                IndexState::Byte(index) => index.remove(text),
                IndexState::Unicode(index) => index.remove(text),
                _ => unreachable!("persistent variant handled above"),
            }
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
        let state = load_index(index)?;
        if let Some(index) = state.persistent() {
            index.clear_sources()?;
            return Ok(LdictStatus::Ok);
        }
        match state.as_ref() {
            IndexState::Byte(index) => index.clear(),
            IndexState::Unicode(index) => index.clear(),
            _ => unreachable!("persistent variant handled above"),
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
        let state = load_index(index)?;
        if let Some(index) = state.persistent() {
            index.compact_sources()?;
            return Ok(LdictStatus::Ok);
        }
        match state.as_ref() {
            IndexState::Byte(index) => index.compact(),
            IndexState::Unicode(index) => index.compact(),
            _ => unreachable!("persistent variant handled above"),
        }
        Ok(LdictStatus::Ok)
    })
}

/// Flush one persistent suffix index to its checkpoint image. In-memory
/// suffix indexes return UNSUPPORTED.
///
/// # Safety
/// `index` must be a valid live suffix-index handle.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_checkpoint(
    index: *const LdictSuffixIndex,
) -> LdictStatus {
    boundary(|| {
        let state = load_index(index)?;
        let index = state.persistent().ok_or((
            LdictStatus::Unsupported,
            "in-memory suffix index has no checkpoint".into(),
        ))?;
        index.checkpoint()?;
        Ok(LdictStatus::Ok)
    })
}

/// Report whether a persistent suffix graph has tombstones to compact.
///
/// # Safety
/// `index` and `out_needed` must be valid, with writable output.
#[cfg(feature = "persistent-artrie")]
#[no_mangle]
pub unsafe extern "C" fn ldict_suffix_index_needs_compaction(
    index: *const LdictSuffixIndex,
    out_needed: *mut u8,
) -> LdictStatus {
    boundary(|| {
        if out_needed.is_null() {
            return Err((LdictStatus::NullPointer, "out_needed is null".into()));
        }
        out_needed.write(0);
        let state = load_index(index)?;
        let index = state.persistent().ok_or((
            LdictStatus::Unsupported,
            "in-memory suffix index has no persistent maintenance state".into(),
        ))?;
        out_needed.write(u8::from(index.needs_compaction()));
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
        let index_state = index
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "index is null".into()))?;
        let state = match load_index(index)?.as_ref() {
            IndexState::Byte(index) => SnapshotState::Byte(index.source_snapshot()),
            IndexState::Unicode(index) => SnapshotState::Unicode(index.source_snapshot()),
            IndexState::AutomatonByte(index) => {
                SnapshotState::AutomatonByte(index.source_view(), index_state.producer_id)
            }
            IndexState::AutomatonUnicode(index) => {
                SnapshotState::AutomatonUnicode(index.source_view(), index_state.producer_id)
            }
            IndexState::TreeByte(index) => {
                SnapshotState::TreeByte(index.source_view(), index_state.producer_id)
            }
            IndexState::TreeUnicode(index) => {
                SnapshotState::TreeUnicode(index.source_view(), index_state.producer_id)
            }
            IndexState::ScdawgByte(index) => {
                SnapshotState::ScdawgByte(index.source_view(), index_state.producer_id)
            }
            IndexState::ScdawgUnicode(index) => {
                SnapshotState::ScdawgUnicode(index.source_view(), index_state.producer_id)
            }
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
        let (producer, revision) = state.identity();
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
        let count = state.source_count();
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
        Ok(u64::from(state.contains_source(text)))
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
        Ok(u64::from(state.contains_substring(text)))
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
        size(state.substring_frequency(text))
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
        let total = state.source_count();
        out_total.write(size(total)?);
        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        if offset >= total {
            return Ok(LdictStatus::End);
        }
        let written = capacity.min(total - offset);
        let mut descriptors = Vec::with_capacity(written);
        for rank in 0..written {
            let record = state
                .source_at(offset + rank)
                .expect("rank is within source_count");
            descriptors.push(LdictSuffixSourceRecord {
                source_id: record.0,
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
