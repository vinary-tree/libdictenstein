//! Bounded, snapshot-owning value-difference cursor for the project C ABI.

use super::*;
use crate::bindings::{BindingTerm, BindingValueDiffEntries, BindingValueDifference};

/// One shared key whose optional values differ between captured revisions.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LdictValueDiffEntry {
    pub unit_offset: usize,
    pub unit_len: usize,
    pub left: LdictOptionalU64,
    pub right: LdictOptionalU64,
}

/// One borrowed page, valid until its generation is released.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LdictValueDiffBatch {
    pub entries: *const LdictValueDiffEntry,
    pub entry_count: usize,
    pub units: *const c_void,
    pub unit_count: usize,
    pub generation: u64,
}

impl Default for LdictValueDiffBatch {
    fn default() -> Self {
        Self {
            entries: ptr::null(),
            entry_count: 0,
            units: ptr::null(),
            unit_count: 0,
            generation: 0,
        }
    }
}

enum UnitArena {
    Byte(Vec<u8>),
    Unicode(Vec<u32>),
    U64(Vec<u64>),
}

impl UnitArena {
    fn new(domain: BindingUnitDomain) -> Self {
        match domain {
            BindingUnitDomain::Byte => Self::Byte(Vec::new()),
            BindingUnitDomain::UnicodeScalar => Self::Unicode(Vec::new()),
            BindingUnitDomain::U64 => Self::U64(Vec::new()),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Byte(units) => units.len(),
            Self::Unicode(units) => units.len(),
            Self::U64(units) => units.len(),
        }
    }

    fn clear(&mut self) {
        match self {
            Self::Byte(units) => units.clear(),
            Self::Unicode(units) => units.clear(),
            Self::U64(units) => units.clear(),
        }
    }

    fn append(&mut self, term: BindingTerm) -> Result<usize, (LdictStatus, String)> {
        match (self, term) {
            (Self::Byte(units), BindingTerm::Bytes(term)) => {
                let len = term.len();
                units.extend(term);
                Ok(len)
            }
            (Self::Unicode(units), BindingTerm::Unicode(term)) => {
                let before = units.len();
                units.extend(term.chars().map(u32::from));
                Ok(units.len() - before)
            }
            (Self::U64(units), BindingTerm::U64(term)) => {
                let len = term.len();
                units.extend(term);
                Ok(len)
            }
            _ => Err((
                LdictStatus::ProviderError,
                "value diff domain changed".into(),
            )),
        }
    }

    fn ptr(&self) -> *const c_void {
        match self {
            Self::Byte(units) => units.as_ptr().cast(),
            Self::Unicode(units) => units.as_ptr().cast(),
            Self::U64(units) => units.as_ptr().cast(),
        }
    }
}

/// Opaque cursor retaining both immutable revisions and one native merge.
pub struct LdictValueDiffCursor {
    source: BindingValueDiffEntries,
    pending: Option<BindingValueDifference>,
    descriptors: Vec<LdictValueDiffEntry>,
    units: UnitArena,
    generation: u64,
    leased_generation: Option<u64>,
    cancelled: bool,
    ended: bool,
}

unsafe fn cursor_mut<'a>(
    cursor: *mut LdictValueDiffCursor,
) -> Result<&'a mut LdictValueDiffCursor, (LdictStatus, String)> {
    cursor
        .as_mut()
        .ok_or((LdictStatus::NullPointer, "value diff cursor is null".into()))
}

/// Capture two matching-domain revisions without enumerating them.
///
/// # Safety
/// Input handles and distinct output slots must be valid and writable.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_value_diff_open(
    left: *const LdictDictionary,
    right: *const LdictDictionary,
    out_cursor: *mut *mut LdictValueDiffCursor,
    out_unit_domain: *mut u32,
) -> LdictStatus {
    boundary(|| {
        if out_cursor.is_null() || out_unit_domain.is_null() {
            return Err((LdictStatus::NullPointer, "value diff output is null".into()));
        }
        out_cursor.write(ptr::null_mut());
        out_unit_domain.write(0);
        let left = left
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "left dictionary is null".into()))?;
        let right = right
            .as_ref()
            .ok_or((LdictStatus::NullPointer, "right dictionary is null".into()))?;
        let source = BindingValueDiffEntries::new(&left.resource, &right.resource)
            .map_err(|error| algebra_error(error, "value diff open"))?;
        let domain = source.domain();
        let cursor = Box::new(LdictValueDiffCursor {
            source,
            pending: None,
            descriptors: Vec::new(),
            units: UnitArena::new(domain),
            generation: 0,
            leased_generation: None,
            cancelled: false,
            ended: false,
        });
        out_unit_domain.write(domain as u32);
        out_cursor.write(Box::into_raw(cursor));
        Ok(LdictStatus::Ok)
    })
}

/// Lease one page of changed values, bounded by descriptors, units and present
/// mapped values in `limits`. An oversized first record remains pending.
///
/// # Safety
/// All pointers must be valid. Returned arenas are borrowed until release.
#[no_mangle]
pub unsafe extern "C" fn ldict_value_diff_cursor_next(
    cursor: *mut LdictValueDiffCursor,
    limits: *const LdictEntryBatchLimits,
    out_batch: *mut LdictValueDiffBatch,
) -> LdictStatus {
    boundary(|| {
        if limits.is_null() || out_batch.is_null() {
            return Err((
                LdictStatus::NullPointer,
                "value diff page argument is null".into(),
            ));
        }
        out_batch.write(LdictValueDiffBatch::default());
        let limits = *limits;
        if limits.max_entries == 0 || limits.reserved != 0 {
            return Err((
                LdictStatus::InvalidArgument,
                "invalid value diff limits".into(),
            ));
        }
        let cursor = cursor_mut(cursor)?;
        if cursor.leased_generation.is_some() {
            return Err((LdictStatus::BatchInUse, "value diff page is leased".into()));
        }
        if cursor.cancelled || cursor.ended {
            return Ok(LdictStatus::End);
        }
        cursor.descriptors.clear();
        cursor.units.clear();
        let mut value_count = 0usize;
        while cursor.descriptors.len() < limits.max_entries {
            let next = if let Some(pending) = cursor.pending.take() {
                Some(pending)
            } else {
                cursor
                    .source
                    .next()
                    .transpose()
                    .map_err(|error| algebra_error(error, "value diff next"))?
            };
            let Some(next) = next else {
                cursor.ended = true;
                break;
            };
            let unit_len = match &next.term {
                BindingTerm::Bytes(units) => units.len(),
                BindingTerm::Unicode(text) => text.chars().count(),
                BindingTerm::U64(units) => units.len(),
            };
            let values = usize::from(next.left.is_some()) + usize::from(next.right.is_some());
            if cursor.units.len().saturating_add(unit_len) > limits.max_units
                || value_count.saturating_add(values) > limits.max_values
            {
                cursor.pending = Some(next);
                if cursor.descriptors.is_empty() {
                    return Err((
                        LdictStatus::LimitExceeded,
                        "value diff record exceeds page limits".into(),
                    ));
                }
                break;
            }
            let unit_offset = cursor.units.len();
            let left = LdictOptionalU64::encode(next.left);
            let right = LdictOptionalU64::encode(next.right);
            cursor.units.append(next.term)?;
            cursor.descriptors.push(LdictValueDiffEntry {
                unit_offset,
                unit_len,
                left,
                right,
            });
            value_count += values;
        }
        if cursor.descriptors.is_empty() {
            return Ok(LdictStatus::End);
        }
        cursor.generation = cursor.generation.checked_add(1).ok_or((
            LdictStatus::LimitExceeded,
            "value diff generation exhausted".into(),
        ))?;
        cursor.leased_generation = Some(cursor.generation);
        out_batch.write(LdictValueDiffBatch {
            entries: cursor.descriptors.as_ptr(),
            entry_count: cursor.descriptors.len(),
            units: if cursor.units.len() == 0 {
                ptr::null()
            } else {
                cursor.units.ptr()
            },
            unit_count: cursor.units.len(),
            generation: cursor.generation,
        });
        Ok(LdictStatus::Ok)
    })
}

/// Release exactly the active page generation.
///
/// # Safety
/// `cursor` must point to a live cursor.
#[no_mangle]
pub unsafe extern "C" fn ldict_value_diff_cursor_release(
    cursor: *mut LdictValueDiffCursor,
    generation: u64,
) -> LdictStatus {
    boundary(|| {
        let cursor = cursor_mut(cursor)?;
        if cursor.leased_generation != Some(generation) {
            return Err((
                LdictStatus::InvalidArgument,
                "value diff generation does not match".into(),
            ));
        }
        cursor.leased_generation = None;
        cursor.descriptors.clear();
        cursor.units.clear();
        Ok(LdictStatus::Ok)
    })
}

/// Stop future traversal; an existing page lease still needs release.
///
/// # Safety
/// `cursor` must point to a live cursor.
#[no_mangle]
pub unsafe extern "C" fn ldict_value_diff_cursor_cancel(
    cursor: *mut LdictValueDiffCursor,
) -> LdictStatus {
    boundary(|| {
        cursor_mut(cursor)?.cancelled = true;
        Ok(LdictStatus::Ok)
    })
}

/// Consume a cursor with no live page lease. Null is a no-op.
///
/// # Safety
/// `cursor` must be null or point to a live cursor owned by the caller.
#[no_mangle]
pub unsafe extern "C" fn ldict_value_diff_cursor_free(
    cursor: *mut LdictValueDiffCursor,
) -> LdictStatus {
    boundary(|| {
        if cursor.is_null() {
            return Ok(LdictStatus::Ok);
        }
        if (*cursor).leased_generation.is_some() {
            return Err((LdictStatus::BatchInUse, "value diff page is leased".into()));
        }
        drop(Box::from_raw(cursor));
        Ok(LdictStatus::Ok)
    })
}
