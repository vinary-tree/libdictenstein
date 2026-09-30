//! Bounded, leased finite streaming for `vt.dict.entry.v2`.

use super::entries::{slice_ptr, UnitArena};
use super::{ResourceContext, SnapshotByteEntryStream, SnapshotOps};
use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use vinary_tree_interop::{
    dictionary_entries_info_flags, VtDictionaryByteBatchLimits, VtDictionaryByteBatchView,
    VtDictionaryByteEntriesCursor, VtDictionaryByteEntriesVTable, VtDictionaryByteEntry,
    VtDictionaryByteEntryReducer, VtDictionaryEntriesInfo, VtDictionaryEntryOrder,
    VtSnapshotIdentity, VtStatus, VtValueDomain, VT_DICTIONARY_BYTE_ENTRIES_INTERFACE_VERSION,
};

struct PendingEntry {
    units: Vec<u64>,
    value: Option<Vec<u8>>,
}

struct State {
    // Retained independently of the discovery resource and cursor handle.
    _snapshot: Arc<dyn SnapshotOps>,
    entries: SnapshotByteEntryStream,
    pending: Option<PendingEntry>,
    descriptors: Vec<VtDictionaryByteEntry>,
    units: UnitArena,
    values: Vec<u8>,
    generation: u64,
    lease: Option<u64>,
    cancelled: bool,
    ended: bool,
    in_callback: bool,
}

impl State {
    fn new(snapshot: Arc<dyn SnapshotOps>, entries: SnapshotByteEntryStream) -> Self {
        Self {
            units: UnitArena::new(snapshot.domain()),
            _snapshot: snapshot,
            entries,
            pending: None,
            descriptors: Vec::new(),
            values: Vec::new(),
            generation: 0,
            lease: None,
            cancelled: false,
            ended: false,
            in_callback: false,
        }
    }

    fn fill(&mut self, limits: VtDictionaryByteBatchLimits) -> Result<(), VtStatus> {
        if limits.max_entries == 0 || limits.reserved != 0 {
            return Err(VtStatus::InvalidArgument);
        }
        self.descriptors.clear();
        self.units.clear();
        self.values.clear();
        while self.descriptors.len() < limits.max_entries {
            let entry = match self.pending.take() {
                Some(entry) => entry,
                None => match self.entries.next() {
                    Some((units, value)) => PendingEntry { units, value },
                    None => break,
                },
            };
            let value_len = entry.value.as_ref().map_or(0, Vec::len);
            // Subtraction is safe because each published prefix already fits.
            // A too-large next entry remains pending; an earlier complete
            // prefix can still be published instead of being lost on overflow.
            if entry.units.len() > limits.max_units - self.units.len()
                || value_len > limits.max_value_bytes - self.values.len()
            {
                self.pending = Some(entry);
                if self.descriptors.is_empty() {
                    return Err(VtStatus::LimitExceeded);
                }
                break;
            }
            let unit_offset = if entry.units.is_empty() {
                0
            } else {
                self.units.len()
            };
            let value_offset = if value_len == 0 { 0 } else { self.values.len() };
            self.units.extend(&entry.units)?;
            let has_value = u8::from(entry.value.is_some());
            if let Some(value) = entry.value {
                self.values.extend_from_slice(&value);
            }
            self.descriptors.push(VtDictionaryByteEntry {
                unit_offset,
                unit_len: entry.units.len(),
                value_offset,
                value_len,
                has_value,
                reserved: [0; 7],
            });
        }
        Ok(())
    }

    fn view(&self, generation: u64) -> VtDictionaryByteBatchView {
        VtDictionaryByteBatchView {
            entries: slice_ptr(&self.descriptors),
            entry_count: self.descriptors.len(),
            units: self.units.as_void_ptr(),
            unit_count: self.units.len(),
            value_bytes: slice_ptr(&self.values),
            value_byte_count: self.values.len(),
            generation,
            reserved: 0,
        }
    }
}

unsafe fn state_mut<'a>(
    cursor: *mut VtDictionaryByteEntriesCursor,
) -> Result<&'a mut State, VtStatus> {
    if cursor.is_null() {
        return Err(VtStatus::NullPointer);
    }
    // SAFETY: the caller serializes unique cursor ownership.
    let handle = unsafe { &mut *cursor };
    if handle.context.is_null() || !ptr::eq(handle.vtable, &DICTIONARY_BYTE_ENTRIES_VTABLE) {
        return Err(VtStatus::Closed);
    }
    // SAFETY: open installs exactly one Box<State>, consumed by close.
    let state = unsafe { &mut *handle.context.cast::<State>() };
    if state.in_callback {
        return Err(VtStatus::BatchInUse);
    }
    Ok(state)
}

unsafe extern "C" fn open(
    resource_context: *mut c_void,
    out_cursor: *mut VtDictionaryByteEntriesCursor,
    out_info: *mut VtDictionaryEntriesInfo,
) -> u32 {
    if resource_context.is_null() || out_cursor.is_null() || out_info.is_null() {
        return VtStatus::NullPointer.to_raw();
    }
    // SAFETY: the retained resource owns this context during the call.
    let resource = unsafe { &*resource_context.cast::<ResourceContext>() };
    let Ok(immutable) = resource.immutable() else {
        return VtStatus::InvalidArgument.to_raw();
    };
    if immutable.value_domain() != VtValueDomain::Bytes {
        return VtStatus::Unsupported.to_raw();
    }
    let snapshot = resource.snapshot();
    let Some(entries) = snapshot.byte_entries() else {
        return VtStatus::Unsupported.to_raw();
    };
    let mut flags = dictionary_entries_info_flags::SNAPSHOT_IDENTITY;
    let exact_len = snapshot.len().map_or(0, |len| {
        flags |= dictionary_entries_info_flags::EXACT_LEN;
        len
    });
    let identity = snapshot.identity();
    let info = VtDictionaryEntriesInfo {
        unit_domain: snapshot.domain() as u32,
        value_domain: VtValueDomain::Bytes as u32,
        order: VtDictionaryEntryOrder::Lexicographic as u32,
        reserved0: 0,
        flags,
        exact_len,
        identity: VtSnapshotIdentity {
            producer: identity.producer,
            revision: identity.revision,
        },
        reserved: [0; 2],
    };
    let state = Box::new(State::new(snapshot, entries));
    // SAFETY: outputs are required writable slots and are published together.
    unsafe {
        out_cursor.write(VtDictionaryByteEntriesCursor {
            context: Box::into_raw(state).cast(),
            vtable: &DICTIONARY_BYTE_ENTRIES_VTABLE,
        });
        out_info.write(info);
    }
    VtStatus::Ok.to_raw()
}

unsafe fn next_status(
    cursor: *mut VtDictionaryByteEntriesCursor,
    limits: *const VtDictionaryByteBatchLimits,
    out_batch: *mut VtDictionaryByteBatchView,
) -> VtStatus {
    if limits.is_null() || out_batch.is_null() {
        return VtStatus::NullPointer;
    }
    // SAFETY: the cursor is uniquely borrowed for this call.
    let state = match unsafe { state_mut(cursor) } {
        Ok(state) => state,
        Err(status) => return status,
    };
    if state.lease.is_some() {
        return VtStatus::BatchInUse;
    }
    if state.ended || state.cancelled {
        state.ended = true;
        // SAFETY: End's canonical empty output is part of the v2 contract.
        unsafe { out_batch.write(VtDictionaryByteBatchView::default()) };
        return VtStatus::End;
    }
    if state.generation == u64::MAX {
        return VtStatus::LimitExceeded;
    }
    // SAFETY: input limit pointer is required and read once before work.
    let limits = unsafe { *limits };
    if let Err(status) = state.fill(limits) {
        return status;
    }
    if state.descriptors.is_empty() {
        state.ended = true;
        unsafe { out_batch.write(VtDictionaryByteBatchView::default()) };
        return VtStatus::End;
    }
    let generation = state.generation + 1;
    state.generation = generation;
    state.lease = Some(generation);
    unsafe { out_batch.write(state.view(generation)) };
    VtStatus::Ok
}

unsafe extern "C" fn next_batch(
    cursor: *mut VtDictionaryByteEntriesCursor,
    limits: *const VtDictionaryByteBatchLimits,
    out_batch: *mut VtDictionaryByteBatchView,
) -> u32 {
    unsafe { next_status(cursor, limits, out_batch) }.to_raw()
}

unsafe fn release_status(cursor: *mut VtDictionaryByteEntriesCursor, generation: u64) -> VtStatus {
    let state = match unsafe { state_mut(cursor) } {
        Ok(state) => state,
        Err(status) => return status,
    };
    if generation == 0 || state.lease != Some(generation) {
        return VtStatus::InvalidArgument;
    }
    state.lease = None;
    VtStatus::Ok
}

unsafe extern "C" fn release_batch(
    cursor: *mut VtDictionaryByteEntriesCursor,
    generation: u64,
) -> u32 {
    unsafe { release_status(cursor, generation) }.to_raw()
}

unsafe extern "C" fn reduce(
    cursor: *mut VtDictionaryByteEntriesCursor,
    limits: *const VtDictionaryByteBatchLimits,
    reducer: Option<VtDictionaryByteEntryReducer>,
    reducer_context: *mut c_void,
    out_count: *mut usize,
) -> u32 {
    if limits.is_null() || reducer.is_none() || out_count.is_null() {
        return VtStatus::NullPointer.to_raw();
    }
    let reducer = reducer.expect("validated reducer");
    let mut count = 0usize;
    loop {
        let mut batch = VtDictionaryByteBatchView::default();
        match unsafe { next_status(cursor, limits, &mut batch) } {
            VtStatus::Ok => {}
            VtStatus::End => {
                unsafe { out_count.write(count) };
                return VtStatus::Ok.to_raw();
            }
            status => return status.to_raw(),
        }
        // Mark the cursor inaccessible to every same-cursor callback until
        // this reducer returns; no Rust borrow crosses the foreign call.
        let state = match unsafe { state_mut(cursor) } {
            Ok(state) => state,
            Err(status) => return status.to_raw(),
        };
        state.in_callback = true;
        let raw = unsafe { reducer(reducer_context, &batch) };
        // SAFETY: no concurrent/same-cursor use is permitted by the ABI.
        let state = unsafe { &mut *(*cursor).context.cast::<State>() };
        state.in_callback = false;
        let release = unsafe { release_status(cursor, batch.generation) };
        if release != VtStatus::Ok {
            return release.to_raw();
        }
        count = match count.checked_add(batch.entry_count) {
            Some(next) => next,
            None => return VtStatus::LimitExceeded.to_raw(),
        };
        match VtStatus::from_raw(raw) {
            Some(VtStatus::Ok) => {}
            Some(VtStatus::End) => {
                unsafe { out_count.write(count) };
                return VtStatus::Ok.to_raw();
            }
            Some(status) => return status.to_raw(),
            None => return VtStatus::InvalidArgument.to_raw(),
        }
    }
}

unsafe extern "C" fn cancel(cursor: *mut VtDictionaryByteEntriesCursor) -> u32 {
    match unsafe { state_mut(cursor) } {
        Ok(state) => {
            state.cancelled = true;
            VtStatus::Ok.to_raw()
        }
        Err(status) => status.to_raw(),
    }
}

unsafe extern "C" fn close(cursor: *mut VtDictionaryByteEntriesCursor) -> u32 {
    if cursor.is_null() {
        return VtStatus::Ok.to_raw();
    }
    // SAFETY: caller provides a writable cursor handle.
    let handle = unsafe { &mut *cursor };
    if handle.context.is_null() && handle.vtable.is_null() {
        return VtStatus::Ok.to_raw();
    }
    if handle.context.is_null() || !ptr::eq(handle.vtable, &DICTIONARY_BYTE_ENTRIES_VTABLE) {
        return VtStatus::InvalidArgument.to_raw();
    }
    let state = unsafe { &mut *handle.context.cast::<State>() };
    if state.in_callback || state.lease.is_some() {
        return VtStatus::BatchInUse.to_raw();
    }
    let context = handle.context;
    handle.context = ptr::null_mut();
    handle.vtable = ptr::null();
    unsafe { drop(Box::from_raw(context.cast::<State>())) };
    VtStatus::Ok.to_raw()
}

pub(super) static DICTIONARY_BYTE_ENTRIES_VTABLE: VtDictionaryByteEntriesVTable =
    VtDictionaryByteEntriesVTable {
        struct_size: std::mem::size_of::<VtDictionaryByteEntriesVTable>(),
        interface_version: VT_DICTIONARY_BYTE_ENTRIES_INTERFACE_VERSION,
        reserved: 0,
        open: Some(open),
        next_batch: Some(next_batch),
        release_batch: Some(release_batch),
        reduce: Some(reduce),
        cancel: Some(cancel),
        close: Some(close),
    };
