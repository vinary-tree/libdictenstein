//! Lock-free finite entry streaming for `vt.dict.entry.v1`.

use super::{ResourceContext, SnapshotOps};
use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use vinary_tree_interop::{
    dictionary_entries_info_flags, VtDictionaryEdge, VtDictionaryEntriesCursor,
    VtDictionaryEntriesInfo, VtDictionaryEntriesVTable, VtDictionaryEntry,
    VtDictionaryEntryBatchLimits, VtDictionaryEntryBatchView, VtDictionaryEntryOrder,
    VtDictionaryEntryReducer, VtSnapshotIdentity, VtStatus, VtUnitDomain, VtValueDomain,
    VT_DICTIONARY_ENTRIES_INTERFACE_VERSION,
};

struct TraversalFrame {
    node: u64,
    edges: Vec<VtDictionaryEdge>,
    next_edge: usize,
    is_final: bool,
    entered: bool,
    loaded: bool,
    restore_path_len: usize,
}

impl TraversalFrame {
    fn lazy(node: u64, restore_path_len: usize) -> Self {
        Self {
            node,
            edges: Vec::new(),
            next_edge: 0,
            is_final: false,
            entered: false,
            loaded: false,
            restore_path_len,
        }
    }
}

pub(super) struct PendingEntry {
    pub(super) units: Vec<u64>,
    pub(super) value: Option<u64>,
}

enum UnitArena {
    Byte(Vec<u8>),
    Unicode(Vec<u32>),
    U64(Vec<u64>),
}

impl UnitArena {
    fn new(domain: VtUnitDomain) -> Self {
        match domain {
            VtUnitDomain::Byte => Self::Byte(Vec::new()),
            VtUnitDomain::UnicodeScalar => Self::Unicode(Vec::new()),
            VtUnitDomain::U64 => Self::U64(Vec::new()),
        }
    }

    fn clear(&mut self) {
        match self {
            Self::Byte(units) => units.clear(),
            Self::Unicode(units) => units.clear(),
            Self::U64(units) => units.clear(),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Byte(units) => units.len(),
            Self::Unicode(units) => units.len(),
            Self::U64(units) => units.len(),
        }
    }

    fn extend(&mut self, units: &[u64]) -> Result<(), VtStatus> {
        match self {
            Self::Byte(output) => {
                output.reserve(units.len());
                for &unit in units {
                    output.push(u8::try_from(unit).map_err(|_| VtStatus::ProviderError)?);
                }
            }
            Self::Unicode(output) => {
                output.reserve(units.len());
                for &unit in units {
                    let scalar = u32::try_from(unit).map_err(|_| VtStatus::ProviderError)?;
                    if char::from_u32(scalar).is_none() {
                        return Err(VtStatus::ProviderError);
                    }
                    output.push(scalar);
                }
            }
            Self::U64(output) => output.extend_from_slice(units),
        }
        Ok(())
    }

    fn as_void_ptr(&self) -> *const c_void {
        match self {
            Self::Byte(units) => slice_ptr(units).cast(),
            Self::Unicode(units) => slice_ptr(units).cast(),
            Self::U64(units) => slice_ptr(units).cast(),
        }
    }
}

fn slice_ptr<T>(slice: &[T]) -> *const T {
    if slice.is_empty() {
        ptr::null()
    } else {
        slice.as_ptr()
    }
}

pub(super) struct EntryCursorState {
    snapshot: Arc<dyn SnapshotOps>,
    records: Option<super::SnapshotEntryStream>,
    stack: Vec<TraversalFrame>,
    path: Vec<u64>,
    pending: Option<PendingEntry>,
    descriptors: Vec<VtDictionaryEntry>,
    units: UnitArena,
    values: Vec<u64>,
    generation: u64,
    leased_generation: Option<u64>,
    cancelled: bool,
    ended: bool,
}

impl EntryCursorState {
    pub(super) fn new(snapshot: Arc<dyn SnapshotOps>) -> Self {
        let root = snapshot.root();
        let records = snapshot.entries();
        Self {
            units: UnitArena::new(snapshot.domain()),
            snapshot,
            records,
            stack: vec![TraversalFrame::lazy(root, 0)],
            path: Vec::with_capacity(16),
            pending: None,
            descriptors: Vec::new(),
            values: Vec::new(),
            generation: 0,
            leased_generation: None,
            cancelled: false,
            ended: false,
        }
    }

    fn load_top(&mut self) -> Result<(), VtStatus> {
        let frame = self.stack.last_mut().ok_or(VtStatus::ProviderError)?;
        if frame.loaded {
            return Ok(());
        }
        let (is_final, _, total) = self.snapshot.copy_node(frame.node, 0, &mut [])?;
        if total > isize::MAX as usize / std::mem::size_of::<VtDictionaryEdge>() {
            return Err(VtStatus::LimitExceeded);
        }
        frame.edges.resize(total, VtDictionaryEdge::default());
        let (confirmed_final, written, confirmed_total) =
            self.snapshot.copy_node(frame.node, 0, &mut frame.edges)?;
        if confirmed_final != is_final || confirmed_total != total || written != total {
            return Err(VtStatus::ProviderError);
        }
        frame.edges.sort_unstable_by_key(|edge| edge.label);
        if frame
            .edges
            .windows(2)
            .any(|pair| pair[0].label >= pair[1].label)
        {
            return Err(VtStatus::ProviderError);
        }
        frame.is_final = is_final;
        frame.loaded = true;
        Ok(())
    }

    pub(super) fn next_entry(&mut self) -> Result<Option<PendingEntry>, VtStatus> {
        if let Some(records) = &mut self.records {
            return Ok(records
                .next()
                .map(|(units, value)| PendingEntry { units, value }));
        }
        loop {
            if self.stack.is_empty() {
                return Ok(None);
            }
            self.load_top()?;
            let frame = self.stack.last_mut().expect("the top frame was loaded");
            if !frame.entered {
                frame.entered = true;
                if frame.is_final {
                    return Ok(Some(PendingEntry {
                        units: self.path.clone(),
                        value: self.snapshot.value(frame.node)?,
                    }));
                }
            } else if let Some(edge) = frame.edges.get(frame.next_edge).copied() {
                frame.next_edge += 1;
                let restore_path_len = self.path.len();
                self.path.push(edge.label);
                self.stack
                    .push(TraversalFrame::lazy(edge.node, restore_path_len));
            } else {
                let restore_path_len = frame.restore_path_len;
                self.stack.pop();
                self.path.truncate(restore_path_len);
            }
        }
    }

    fn fill(&mut self, limits: VtDictionaryEntryBatchLimits) -> Result<(), VtStatus> {
        if limits.reserved != 0 || limits.max_entries == 0 {
            return Err(VtStatus::InvalidArgument);
        }
        self.descriptors.clear();
        self.units.clear();
        self.values.clear();

        while self.descriptors.len() < limits.max_entries {
            let entry = match self.pending.take() {
                Some(entry) => entry,
                None => match self.next_entry()? {
                    Some(entry) => entry,
                    None => break,
                },
            };
            let unit_end = self
                .units
                .len()
                .checked_add(entry.units.len())
                .ok_or(VtStatus::LimitExceeded)?;
            let value_len = usize::from(entry.value.is_some());
            let value_end = self
                .values
                .len()
                .checked_add(value_len)
                .ok_or(VtStatus::LimitExceeded)?;
            if unit_end > limits.max_units || value_end > limits.max_values {
                self.pending = Some(entry);
                if self.descriptors.is_empty() {
                    return Err(VtStatus::LimitExceeded);
                }
                break;
            }

            let unit_offset = self.units.len();
            let value_offset = self.values.len();
            self.units.extend(&entry.units)?;
            if let Some(value) = entry.value {
                self.values.push(value);
            }
            self.descriptors.push(VtDictionaryEntry {
                unit_offset,
                unit_len: entry.units.len(),
                value_offset,
                value_len,
                reserved: 0,
            });
        }
        Ok(())
    }

    fn view(&self, generation: u64) -> VtDictionaryEntryBatchView {
        VtDictionaryEntryBatchView {
            entries: slice_ptr(&self.descriptors),
            entry_count: self.descriptors.len(),
            units: self.units.as_void_ptr(),
            unit_count: self.units.len(),
            values: slice_ptr(&self.values),
            value_count: self.values.len(),
            generation,
            reserved: 0,
        }
    }
}

unsafe fn state_mut<'a>(
    cursor: *mut VtDictionaryEntriesCursor,
) -> Result<&'a mut EntryCursorState, VtStatus> {
    if cursor.is_null() {
        return Err(VtStatus::NullPointer);
    }
    // SAFETY: checked above; the ABI grants exclusive cursor access to each call.
    let cursor = unsafe { &mut *cursor };
    if cursor.context.is_null() || !ptr::eq(cursor.vtable, &DICTIONARY_ENTRIES_VTABLE) {
        return Err(VtStatus::Closed);
    }
    // SAFETY: `open` installs exactly this boxed state until successful close.
    Ok(unsafe { &mut *cursor.context.cast::<EntryCursorState>() })
}

pub(super) unsafe extern "C" fn open(
    resource_context: *mut c_void,
    out_cursor: *mut VtDictionaryEntriesCursor,
    out_info: *mut VtDictionaryEntriesInfo,
) -> u32 {
    if resource_context.is_null() || out_cursor.is_null() || out_info.is_null() {
        return VtStatus::NullPointer.to_raw();
    }
    // SAFETY: the resource interface supplies its live context to this callback.
    let resource = unsafe { &*resource_context.cast::<ResourceContext>() };
    let snapshot = resource.snapshot();
    let mut flags = dictionary_entries_info_flags::SNAPSHOT_IDENTITY;
    let exact_len = match snapshot.len() {
        Some(len) => {
            flags |= dictionary_entries_info_flags::EXACT_LEN;
            len
        }
        None => 0,
    };
    let identity = snapshot.identity();
    let info = VtDictionaryEntriesInfo {
        unit_domain: snapshot.domain() as u32,
        value_domain: VtValueDomain::OptionalU64 as u32,
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
    let state = Box::new(EntryCursorState::new(snapshot));
    // SAFETY: all outputs were validated and are written only on success.
    unsafe {
        out_cursor.write(VtDictionaryEntriesCursor {
            context: Box::into_raw(state).cast(),
            vtable: &DICTIONARY_ENTRIES_VTABLE,
        });
        out_info.write(info);
    }
    VtStatus::Ok.to_raw()
}

unsafe fn next_batch_status(
    cursor: *mut VtDictionaryEntriesCursor,
    limits: *const VtDictionaryEntryBatchLimits,
    out_batch: *mut VtDictionaryEntryBatchView,
) -> VtStatus {
    if limits.is_null() || out_batch.is_null() {
        return VtStatus::NullPointer;
    }
    // SAFETY: validated above; canonicalize every non-success result.
    unsafe { out_batch.write(VtDictionaryEntryBatchView::default()) };
    // SAFETY: the cursor is exclusively borrowed for this call.
    let state = match unsafe { state_mut(cursor) } {
        Ok(state) => state,
        Err(status) => return status,
    };
    if state.leased_generation.is_some() {
        return VtStatus::BatchInUse;
    }
    if state.ended || state.cancelled {
        state.ended = true;
        return VtStatus::End;
    }
    // SAFETY: validated above and copied before any callback can run.
    let limits = unsafe { *limits };
    if let Err(status) = state.fill(limits) {
        return status;
    }
    if state.descriptors.is_empty() {
        state.ended = true;
        return VtStatus::End;
    }
    let Some(generation) = state.generation.checked_add(1) else {
        return VtStatus::LimitExceeded;
    };
    state.generation = generation;
    state.leased_generation = Some(generation);
    // SAFETY: output was validated and arenas remain stable during the lease.
    unsafe { out_batch.write(state.view(generation)) };
    VtStatus::Ok
}

pub(super) unsafe extern "C" fn next_batch(
    cursor: *mut VtDictionaryEntriesCursor,
    limits: *const VtDictionaryEntryBatchLimits,
    out_batch: *mut VtDictionaryEntryBatchView,
) -> u32 {
    // SAFETY: forwarded under the exact ABI contracts.
    unsafe { next_batch_status(cursor, limits, out_batch) }.to_raw()
}

unsafe fn release_batch_status(
    cursor: *mut VtDictionaryEntriesCursor,
    generation: u64,
) -> VtStatus {
    // SAFETY: the cursor is exclusively borrowed for this call.
    let state = match unsafe { state_mut(cursor) } {
        Ok(state) => state,
        Err(status) => return status,
    };
    if generation == 0 || state.leased_generation != Some(generation) {
        return VtStatus::InvalidArgument;
    }
    state.leased_generation = None;
    VtStatus::Ok
}

pub(super) unsafe extern "C" fn release_batch(
    cursor: *mut VtDictionaryEntriesCursor,
    generation: u64,
) -> u32 {
    // SAFETY: forwarded under the exact ABI contracts.
    unsafe { release_batch_status(cursor, generation) }.to_raw()
}

pub(super) unsafe extern "C" fn reduce(
    cursor: *mut VtDictionaryEntriesCursor,
    limits: *const VtDictionaryEntryBatchLimits,
    reducer: Option<VtDictionaryEntryReducer>,
    reducer_context: *mut c_void,
    out_count: *mut usize,
) -> u32 {
    if limits.is_null() || reducer.is_none() || out_count.is_null() {
        return VtStatus::NullPointer.to_raw();
    }
    // SAFETY: validated above.
    unsafe { out_count.write(0) };
    let reducer = reducer.expect("validated reducer");
    let mut count = 0usize;
    loop {
        let mut batch = VtDictionaryEntryBatchView::default();
        // SAFETY: local output and caller-supplied cursor/limits obey the ABI.
        match unsafe { next_batch_status(cursor, limits, &mut batch) } {
            VtStatus::Ok => {}
            VtStatus::End => {
                // SAFETY: validated output pointer.
                unsafe { out_count.write(count) };
                return VtStatus::Ok.to_raw();
            }
            status => return status.to_raw(),
        }
        // No Rust borrow of cursor state crosses the foreign callback.
        let callback_raw = unsafe { reducer(reducer_context, &batch) };
        // Settle the internal lease before interpreting callback control flow.
        let release = unsafe { release_batch_status(cursor, batch.generation) };
        if release != VtStatus::Ok {
            return release.to_raw();
        }
        count = match count.checked_add(batch.entry_count) {
            Some(count) => count,
            None => return VtStatus::LimitExceeded.to_raw(),
        };
        match VtStatus::from_raw(callback_raw) {
            Some(VtStatus::Ok) => {}
            Some(VtStatus::End) => {
                // SAFETY: validated output pointer.
                unsafe { out_count.write(count) };
                return VtStatus::Ok.to_raw();
            }
            Some(status) => return status.to_raw(),
            None => return VtStatus::InvalidArgument.to_raw(),
        }
    }
}

pub(super) unsafe extern "C" fn cancel(cursor: *mut VtDictionaryEntriesCursor) -> u32 {
    // SAFETY: the cursor is exclusively borrowed for this call.
    match unsafe { state_mut(cursor) } {
        Ok(state) => {
            state.cancelled = true;
            VtStatus::Ok.to_raw()
        }
        Err(status) => status.to_raw(),
    }
}

pub(super) unsafe extern "C" fn close(cursor: *mut VtDictionaryEntriesCursor) -> u32 {
    if cursor.is_null() {
        return VtStatus::Ok.to_raw();
    }
    // SAFETY: checked above.
    let handle = unsafe { &mut *cursor };
    if handle.context.is_null() && handle.vtable.is_null() {
        return VtStatus::Ok.to_raw();
    }
    if handle.context.is_null() || !ptr::eq(handle.vtable, &DICTIONARY_ENTRIES_VTABLE) {
        return VtStatus::Closed.to_raw();
    }
    // SAFETY: this context was installed by `open` and remains owned here.
    let state = unsafe { &mut *handle.context.cast::<EntryCursorState>() };
    if state.leased_generation.is_some() {
        return VtStatus::BatchInUse.to_raw();
    }
    let context = handle.context;
    handle.context = ptr::null_mut();
    handle.vtable = ptr::null();
    // SAFETY: successful close consumes the unique box exactly once.
    unsafe { drop(Box::from_raw(context.cast::<EntryCursorState>())) };
    VtStatus::Ok.to_raw()
}

pub(super) static DICTIONARY_ENTRIES_VTABLE: VtDictionaryEntriesVTable =
    VtDictionaryEntriesVTable {
        struct_size: std::mem::size_of::<VtDictionaryEntriesVTable>(),
        interface_version: VT_DICTIONARY_ENTRIES_INTERFACE_VERSION,
        reserved: 0,
        open: Some(open),
        next_batch: Some(next_batch),
        release_batch: Some(release_batch),
        reduce: Some(reduce),
        cancel: Some(cancel),
        close: Some(close),
    };
