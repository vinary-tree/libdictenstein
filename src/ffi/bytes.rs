//! Additive revision-7 C facade for byte-valued DynamicDAWGs.

use super::*;
use vinary_tree_interop::{
    VtDictionaryByteBatchLimits, VtDictionaryByteBatchView, VtDictionaryByteEntriesCursor,
    VtDictionaryByteEntriesVTable, VT_DICTIONARY_BYTE_ENTRIES_INTERFACE_ID,
    VT_DICTIONARY_BYTE_ENTRIES_INTERFACE_VERSION,
};

/// One v2 byte-valued entry descriptor.
pub type LdictByteEntry = vinary_tree_interop::VtDictionaryByteEntry;
/// Hard descriptor, unit-element, and value-byte limits for one batch.
pub type LdictByteEntryBatchLimits = VtDictionaryByteBatchLimits;
/// One borrowed, cursor-owned batch.
pub type LdictByteEntryBatch = VtDictionaryByteBatchView;
/// Captured immutable cursor metadata.
pub type LdictByteEntriesInfo = VtDictionaryEntriesInfo;
/// Callback for [`ldict_byte_entry_cursor_reduce`].
pub type LdictByteEntryReducer = unsafe extern "C" fn(
    reducer_context: *mut std::ffi::c_void,
    batch: *const LdictByteEntryBatch,
) -> u32;

/// Unique owned handle for a finite byte-valued entry stream.
pub struct LdictByteEntryCursor {
    raw: VtDictionaryByteEntriesCursor,
    vtable: *const VtDictionaryByteEntriesVTable,
    lease: Option<u64>,
    in_callback: bool,
    unit_domain: u32,
    last_key: Option<Vec<u64>>,
}

fn byte_binding(
    dictionary: &LdictDictionary,
) -> Result<&ByteValueDawgBinding, (LdictStatus, String)> {
    match &dictionary.binding {
        LdictBinding::DynamicBytes(binding) => Ok(binding),
        _ => Err((
            LdictStatus::Unsupported,
            "dictionary does not carry byte values".into(),
        )),
    }
}

fn decode_value<'a>(
    data: *const u8,
    len: usize,
    has_value: u8,
) -> Result<Option<&'a [u8]>, (LdictStatus, String)> {
    if has_value > 1 || (has_value == 0 && len != 0) {
        return Err((
            LdictStatus::InvalidArgument,
            "has_value must be 0 or 1 and absent values have zero length".into(),
        ));
    }
    if has_value == 0 {
        return Ok(None);
    }
    // SAFETY: C caller promises a readable buffer; zero length accepts NULL.
    Ok(Some(unsafe { slice(data, len, "value bytes")? }))
}

/// Construct a DynamicDAWG with optional opaque byte values and fixed units.
///
/// # Safety
/// `out_dictionary` must be writable.
#[no_mangle]
pub unsafe extern "C" fn ldict_dynamic_dawg_new_byte_values(
    unit_domain: u32,
    out_dictionary: *mut *mut LdictDictionary,
) -> LdictStatus {
    boundary(|| {
        if out_dictionary.is_null() {
            return Err((LdictStatus::NullPointer, "out_dictionary is null".into()));
        }
        unsafe { out_dictionary.write(ptr::null_mut()) };
        let domain = domain(unit_domain)?;
        unsafe {
            out_dictionary.write(Box::into_raw(Box::new(LdictDictionary::new(
                LdictBinding::DynamicBytes(ByteValueDawgBinding::new(domain)),
            ))))
        };
        Ok(LdictStatus::Ok)
    })
}

/// Insert or update a byte/Unicode key, preserving absent versus empty value.
///
/// # Safety
/// Nonempty input buffers and output pointer must be valid.
#[allow(clippy::too_many_arguments)]
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_insert_text_bytes(
    dictionary: *mut LdictDictionary,
    key: *const u8,
    key_len: usize,
    value: *const u8,
    value_len: usize,
    has_value: u8,
    out_inserted: *mut u8,
) -> LdictStatus {
    boundary(|| {
        let dictionary = unsafe { dictionary.as_ref() }
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_inserted.is_null() {
            return Err((LdictStatus::NullPointer, "out_inserted is null".into()));
        }
        let byte_dawg = byte_binding(dictionary)?;
        let value = decode_value(value, value_len, has_value)?;
        let key = unsafe { slice(key, key_len, "key")? };
        let inserted = binding(byte_dawg.insert_text(key, value))?;
        unsafe { out_inserted.write(u8::from(inserted)) };
        Ok(LdictStatus::Ok)
    })
}

/// Insert or update a u64-token key with optional byte value.
///
/// # Safety
/// Nonempty input buffers and output pointer must be valid.
#[allow(clippy::too_many_arguments)]
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_insert_u64_bytes(
    dictionary: *mut LdictDictionary,
    key: *const u64,
    key_len: usize,
    value: *const u8,
    value_len: usize,
    has_value: u8,
    out_inserted: *mut u8,
) -> LdictStatus {
    boundary(|| {
        let dictionary = unsafe { dictionary.as_ref() }
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_inserted.is_null() {
            return Err((LdictStatus::NullPointer, "out_inserted is null".into()));
        }
        let byte_dawg = byte_binding(dictionary)?;
        let value = decode_value(value, value_len, has_value)?;
        let key = unsafe { slice(key, key_len, "key")? };
        let inserted = binding(byte_dawg.insert_u64(key, value))?;
        unsafe { out_inserted.write(u8::from(inserted)) };
        Ok(LdictStatus::Ok)
    })
}

#[allow(clippy::too_many_arguments)]
unsafe fn copy_result(
    result: Option<Option<Vec<u8>>>,
    out_found: *mut u8,
    out_bytes: *mut u8,
    capacity: usize,
    out_written: *mut usize,
    out_required: *mut usize,
    out_has_value: *mut u8,
) -> Result<LdictStatus, (LdictStatus, String)> {
    let found = result.is_some();
    let value = result.flatten();
    let bytes = value.as_deref().unwrap_or_default();
    // SAFETY: output slots were checked before the dictionary lookup.
    unsafe {
        out_found.write(u8::from(found));
        out_required.write(bytes.len());
        out_has_value.write(u8::from(value.is_some()));
        if capacity < bytes.len() {
            out_written.write(0);
            return Err((
                LdictStatus::LimitExceeded,
                "byte value exceeds output capacity".into(),
            ));
        }
        if !bytes.is_empty() {
            ptr::copy_nonoverlapping(bytes.as_ptr(), out_bytes, bytes.len());
        }
        out_written.write(bytes.len());
    }
    Ok(LdictStatus::Ok)
}

#[allow(clippy::too_many_arguments)]
unsafe fn get_bytes(
    dictionary: *const LdictDictionary,
    text_key: Option<(*const u8, usize)>,
    token_key: Option<(*const u64, usize)>,
    out_found: *mut u8,
    out_bytes: *mut u8,
    capacity: usize,
    out_written: *mut usize,
    out_required: *mut usize,
    out_has_value: *mut u8,
) -> LdictStatus {
    boundary(|| {
        let dictionary = unsafe { dictionary.as_ref() }
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_found.is_null()
            || out_written.is_null()
            || out_required.is_null()
            || out_has_value.is_null()
            || (capacity != 0 && out_bytes.is_null())
        {
            return Err((
                LdictStatus::NullPointer,
                "byte-value output pointer is null".into(),
            ));
        }
        let byte_dawg = byte_binding(dictionary)?;
        let result = if let Some((key, len)) = text_key {
            binding(byte_dawg.get_text(unsafe { slice(key, len, "key")? }))?
        } else if let Some((key, len)) = token_key {
            binding(byte_dawg.get_u64(unsafe { slice(key, len, "key")? }))?
        } else {
            return Err((LdictStatus::InvalidArgument, "no key supplied".into()));
        };
        unsafe {
            copy_result(
                result,
                out_found,
                out_bytes,
                capacity,
                out_written,
                out_required,
                out_has_value,
            )
        }
    })
}

/// Bounded two-phase read of a byte/Unicode key's optional byte value.
///
/// # Safety
/// Required output slots and a positive-capacity buffer must be writable.
#[allow(clippy::too_many_arguments)]
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_get_text_bytes(
    dictionary: *const LdictDictionary,
    key: *const u8,
    key_len: usize,
    out_found: *mut u8,
    out_bytes: *mut u8,
    capacity: usize,
    out_written: *mut usize,
    out_required: *mut usize,
    out_has_value: *mut u8,
) -> LdictStatus {
    unsafe {
        get_bytes(
            dictionary,
            Some((key, key_len)),
            None,
            out_found,
            out_bytes,
            capacity,
            out_written,
            out_required,
            out_has_value,
        )
    }
}

/// Bounded two-phase read of a u64-token key's optional byte value.
///
/// # Safety
/// Required output slots and a positive-capacity buffer must be writable.
#[allow(clippy::too_many_arguments)]
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_get_u64_bytes(
    dictionary: *const LdictDictionary,
    key: *const u64,
    key_len: usize,
    out_found: *mut u8,
    out_bytes: *mut u8,
    capacity: usize,
    out_written: *mut usize,
    out_required: *mut usize,
    out_has_value: *mut u8,
) -> LdictStatus {
    unsafe {
        get_bytes(
            dictionary,
            None,
            Some((key, key_len)),
            out_found,
            out_bytes,
            capacity,
            out_written,
            out_required,
            out_has_value,
        )
    }
}

struct OwnedSnapshotResource(VtResource);

impl Drop for OwnedSnapshotResource {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: snapshot callback returned one owned retain.
            unsafe {
                ((*self.0.vtable)
                    .release
                    .expect("snapshot release is required"))(self.0.context)
            };
        }
    }
}

fn validate_byte_info(
    info: &LdictByteEntriesInfo,
    domain: BindingUnitDomain,
) -> Result<(), (LdictStatus, String)> {
    let expected = VtUnitDomain::from(domain) as u32;
    let known_flags =
        dictionary_entries_info_flags::EXACT_LEN | dictionary_entries_info_flags::SNAPSHOT_IDENTITY;
    if info.unit_domain != expected
        || info.value_domain != VtValueDomain::Bytes as u32
        || info.order != VtDictionaryEntryOrder::Lexicographic as u32
        || info.reserved0 != 0
        || info.reserved != [0; 2]
        || info.flags & !known_flags != 0
        || (info.flags & dictionary_entries_info_flags::EXACT_LEN == 0 && info.exact_len != 0)
        || (info.flags & dictionary_entries_info_flags::SNAPSHOT_IDENTITY == 0
            && info.identity != vinary_tree_interop::VtSnapshotIdentity::default())
    {
        return Err((
            LdictStatus::ProviderError,
            "invalid byte-entry metadata".into(),
        ));
    }
    Ok(())
}

/// Open a finite byte-valued entry stream over one captured immutable revision.
///
/// # Safety
/// `dictionary`, `out_cursor`, and `out_info` must be valid pointers. On
/// failure the output slots are untouched.
#[no_mangle]
pub unsafe extern "C" fn ldict_dictionary_byte_entries_open(
    dictionary: *const LdictDictionary,
    out_cursor: *mut *mut LdictByteEntryCursor,
    out_info: *mut LdictByteEntriesInfo,
) -> LdictStatus {
    boundary(|| {
        let dictionary = unsafe { dictionary.as_ref() }
            .ok_or((LdictStatus::NullPointer, "dictionary is null".into()))?;
        if out_cursor.is_null() || out_info.is_null() {
            return Err((
                LdictStatus::NullPointer,
                "byte-entry output pointer is null".into(),
            ));
        }
        let binding = byte_binding(dictionary)?;
        let live = dictionary.resource.as_raw();
        let live_table = unsafe { live.vtable.as_ref() }
            .ok_or((LdictStatus::ProviderError, "resource vtable is null".into()))?;
        let mut base_ptr: *const std::ffi::c_void = ptr::null();
        let query = live_table.query_interface.ok_or((
            LdictStatus::Unsupported,
            "resource interface discovery is unavailable".into(),
        ))?;
        let status = provider_status(
            unsafe {
                query(
                    live.context,
                    &vinary_tree_interop::VT_DICTIONARY_INTERFACE_ID,
                    vinary_tree_interop::VT_DICTIONARY_INTERFACE_VERSION,
                    &mut base_ptr,
                )
            },
            "dictionary discovery",
        )?;
        if status != VtStatus::Ok || base_ptr.is_null() {
            return Err((
                LdictStatus::ProviderError,
                "dictionary interface missing".into(),
            ));
        }
        let base = unsafe { &*base_ptr.cast::<vinary_tree_interop::VtDictionaryVTable>() };
        let snapshot_fn = base.snapshot.ok_or((
            LdictStatus::ProviderError,
            "dictionary snapshot callback missing".into(),
        ))?;
        let mut captured = VtResource::NULL;
        let status = provider_status(
            unsafe { snapshot_fn(live.context, &mut captured) },
            "dictionary snapshot",
        )?;
        if status != VtStatus::Ok || captured.is_null() {
            return Err((
                LdictStatus::ProviderError,
                "snapshot returned no resource".into(),
            ));
        }
        let captured = OwnedSnapshotResource(captured);
        let captured_table = unsafe { &*captured.0.vtable };
        let query = captured_table.query_interface.ok_or((
            LdictStatus::ProviderError,
            "snapshot discovery callback missing".into(),
        ))?;
        let mut interface: *const std::ffi::c_void = ptr::null();
        let status = provider_status(
            unsafe {
                query(
                    captured.0.context,
                    &VT_DICTIONARY_BYTE_ENTRIES_INTERFACE_ID,
                    VT_DICTIONARY_BYTE_ENTRIES_INTERFACE_VERSION,
                    &mut interface,
                )
            },
            "byte-entry interface discovery",
        )?;
        if status != VtStatus::Ok || interface.is_null() {
            return Err((
                LdictStatus::ProviderError,
                "byte-entry interface missing".into(),
            ));
        }
        let table = interface.cast::<VtDictionaryByteEntriesVTable>();
        let vtable = unsafe { &*table };
        if vtable.struct_size < std::mem::size_of::<VtDictionaryByteEntriesVTable>()
            || vtable.interface_version < VT_DICTIONARY_BYTE_ENTRIES_INTERFACE_VERSION
            || vtable.reserved != 0
            || vtable.open.is_none()
            || vtable.next_batch.is_none()
            || vtable.release_batch.is_none()
            || vtable.reduce.is_none()
            || vtable.cancel.is_none()
            || vtable.close.is_none()
        {
            return Err((
                LdictStatus::ProviderError,
                "incomplete byte-entry vtable".into(),
            ));
        }
        let mut raw = VtDictionaryByteEntriesCursor::NULL;
        let mut info = LdictByteEntriesInfo::default();
        let status = provider_status(
            unsafe { vtable.open.expect("checked")(captured.0.context, &mut raw, &mut info) },
            "byte-entry open",
        )?;
        if status != VtStatus::Ok || raw.is_null() {
            return Err((
                LdictStatus::ProviderError,
                "byte-entry open returned no cursor".into(),
            ));
        }
        if let Err(error) = validate_byte_info(&info, binding.domain()) {
            let _ = unsafe { vtable.close.expect("checked")(&mut raw) };
            return Err(error);
        }
        let cursor = Box::new(LdictByteEntryCursor {
            raw,
            vtable: table,
            lease: None,
            in_callback: false,
            unit_domain: info.unit_domain,
            last_key: None,
        });
        unsafe {
            out_info.write(info);
            out_cursor.write(Box::into_raw(cursor));
        }
        Ok(LdictStatus::Ok)
    })
}

fn cursor_mut<'a>(
    cursor: *mut LdictByteEntryCursor,
) -> Result<&'a mut LdictByteEntryCursor, (LdictStatus, String)> {
    if cursor.is_null() {
        return Err((LdictStatus::NullPointer, "byte-entry cursor is null".into()));
    }
    if unsafe { (*cursor).in_callback } {
        return Err((
            LdictStatus::BatchInUse,
            "byte-entry cursor is inside reducer".into(),
        ));
    }
    Ok(unsafe { &mut *cursor })
}

fn validate_batch(
    batch: &LdictByteEntryBatch,
    limits: &LdictByteEntryBatchLimits,
    unit_domain: u32,
    previous: Option<&[u64]>,
) -> Result<Vec<u64>, (LdictStatus, String)> {
    let width = match unit_domain {
        1 => 1usize,
        2 => 4,
        3 => 8,
        _ => return Err((LdictStatus::ProviderError, "invalid unit domain".into())),
    };
    if batch.entry_count == 0
        || batch.entry_count > limits.max_entries
        || batch.unit_count > limits.max_units
        || batch.value_byte_count > limits.max_value_bytes
        || batch.generation == 0
        || batch.reserved != 0
        || batch.entries.is_null()
        || batch.units.is_null() != (batch.unit_count == 0)
        || batch.value_bytes.is_null() != (batch.value_byte_count == 0)
        || (batch.units as usize) % width != 0
        || batch
            .entry_count
            .checked_mul(std::mem::size_of::<LdictByteEntry>())
            .is_none_or(|bytes| bytes > isize::MAX as usize)
        || batch
            .unit_count
            .checked_mul(width)
            .is_none_or(|bytes| bytes > isize::MAX as usize)
        || batch.value_byte_count > isize::MAX as usize
    {
        return Err((
            LdictStatus::ProviderError,
            "invalid byte-entry arena shape".into(),
        ));
    }
    // An in-process ABI cannot validate arbitrary forged mapped addresses.
    // The native provider owns this descriptor slice; validate all metadata
    // before returning its borrowed pointer to a C caller.
    let entries = unsafe { std::slice::from_raw_parts(batch.entries, batch.entry_count) };
    let mut unit_end = 0usize;
    let mut byte_end = 0usize;
    for entry in entries {
        if entry.has_value > 1
            || entry.reserved != [0; 7]
            || entry.unit_offset > batch.unit_count
            || entry.unit_len > batch.unit_count - entry.unit_offset
            || entry.value_offset > batch.value_byte_count
            || entry.value_len > batch.value_byte_count - entry.value_offset
            || (entry.has_value == 0 && entry.value_len != 0)
        {
            return Err((
                LdictStatus::ProviderError,
                "invalid byte-entry descriptor".into(),
            ));
        }
        if entry.unit_len == 0 {
            if entry.unit_offset != 0 {
                return Err((
                    LdictStatus::ProviderError,
                    "noncanonical empty key offset".into(),
                ));
            }
        } else if entry.unit_offset != unit_end {
            return Err((LdictStatus::ProviderError, "unpacked key arena".into()));
        }
        if entry.value_len == 0 {
            if entry.value_offset != 0 {
                return Err((
                    LdictStatus::ProviderError,
                    "noncanonical empty value offset".into(),
                ));
            }
        } else if entry.has_value != 1 || entry.value_offset != byte_end {
            return Err((
                LdictStatus::ProviderError,
                "unpacked byte-value arena".into(),
            ));
        }
        if entry.unit_len > 0 {
            unit_end = entry.unit_offset + entry.unit_len;
        }
        if entry.value_len > 0 {
            byte_end = entry.value_offset + entry.value_len;
        }
    }
    if unit_end != batch.unit_count || byte_end != batch.value_byte_count {
        return Err((
            LdictStatus::ProviderError,
            "byte-entry arena has trailing data".into(),
        ));
    }
    let mut last = previous.map_or_else(Vec::new, <[u64]>::to_vec);
    for (position, entry) in entries.iter().enumerate() {
        let key = (entry.unit_offset..entry.unit_offset + entry.unit_len).map(|index| {
            // SAFETY: alignment, byte count, and descriptor range were checked.
            unsafe {
                match unit_domain {
                    1 => *batch.units.cast::<u8>().add(index) as u64,
                    2 => *batch.units.cast::<u32>().add(index) as u64,
                    _ => *batch.units.cast::<u64>().add(index),
                }
            }
        });
        if unit_domain == 2
            && key
                .clone()
                .any(|unit| char::from_u32(unit as u32).is_none())
        {
            return Err((
                LdictStatus::ProviderError,
                "invalid Unicode scalar key".into(),
            ));
        }
        if (position != 0 || previous.is_some())
            && last.iter().copied().cmp(key.clone()) != std::cmp::Ordering::Less
        {
            return Err((
                LdictStatus::ProviderError,
                "byte-entry keys are not strictly ordered".into(),
            ));
        }
        last.clear();
        last.extend(key);
    }
    Ok(last)
}

/// Borrow the next complete bounded byte-valued batch.
///
/// # Safety
/// Inputs must be valid; returned arenas expire on exact-generation release.
#[no_mangle]
pub unsafe extern "C" fn ldict_byte_entry_cursor_next(
    cursor: *mut LdictByteEntryCursor,
    limits: *const LdictByteEntryBatchLimits,
    out_batch: *mut LdictByteEntryBatch,
) -> LdictStatus {
    boundary(|| {
        if limits.is_null() || out_batch.is_null() {
            return Err((
                LdictStatus::NullPointer,
                "byte-entry limits or output is null".into(),
            ));
        }
        let limits_value = unsafe { *limits };
        if limits_value.max_entries == 0 || limits_value.reserved != 0 {
            return Err((
                LdictStatus::InvalidArgument,
                "invalid byte-entry limits".into(),
            ));
        }
        let cursor = cursor_mut(cursor)?;
        if cursor.lease.is_some() {
            return Err((LdictStatus::BatchInUse, "byte-entry lease is live".into()));
        }
        let mut batch = LdictByteEntryBatch::default();
        let next = unsafe { (*cursor.vtable).next_batch }
            .ok_or((LdictStatus::ProviderError, "byte-entry next is null".into()))?;
        let status = provider_status(
            unsafe { next(&mut cursor.raw, limits, &mut batch) },
            "byte-entry next",
        )?;
        match status {
            VtStatus::Ok => {
                let last = match validate_batch(
                    &batch,
                    &limits_value,
                    cursor.unit_domain,
                    cursor.last_key.as_deref(),
                ) {
                    Ok(last) => last,
                    Err(error) => {
                        if batch.generation != 0 {
                            let release =
                                unsafe { (*cursor.vtable).release_batch.expect("validated") };
                            let _ = unsafe { release(&mut cursor.raw, batch.generation) };
                        }
                        return Err(error);
                    }
                };
                cursor.lease = Some(batch.generation);
                cursor.last_key = Some(last);
                unsafe { out_batch.write(batch) };
                Ok(LdictStatus::Ok)
            }
            VtStatus::End => {
                if batch.entry_count != 0
                    || batch.unit_count != 0
                    || batch.value_byte_count != 0
                    || !batch.entries.is_null()
                    || !batch.units.is_null()
                    || !batch.value_bytes.is_null()
                    || batch.generation != 0
                    || batch.reserved != 0
                {
                    return Err((LdictStatus::ProviderError, "noncanonical end batch".into()));
                }
                unsafe { out_batch.write(batch) };
                Ok(LdictStatus::End)
            }
            _ => unreachable!("provider_status maps errors to Err"),
        }
    })
}

/// Release the one live batch with its exact generation.
///
/// # Safety
/// `cursor` must be the unique live handle.
#[no_mangle]
pub unsafe extern "C" fn ldict_byte_entry_cursor_release(
    cursor: *mut LdictByteEntryCursor,
    generation: u64,
) -> LdictStatus {
    boundary(|| {
        let cursor = cursor_mut(cursor)?;
        if generation == 0 || cursor.lease != Some(generation) {
            return Err((
                LdictStatus::InvalidArgument,
                "byte-entry generation is not leased".into(),
            ));
        }
        let release = unsafe { (*cursor.vtable).release_batch }.ok_or((
            LdictStatus::ProviderError,
            "byte-entry release is null".into(),
        ))?;
        let status = provider_status(
            unsafe { release(&mut cursor.raw, generation) },
            "byte-entry release",
        )?;
        if status != VtStatus::Ok {
            return Err((
                LdictStatus::ProviderError,
                "byte-entry release returned end".into(),
            ));
        }
        cursor.lease = None;
        Ok(LdictStatus::Ok)
    })
}

struct ReducerContext {
    callback: LdictByteEntryReducer,
    callback_context: *mut std::ffi::c_void,
    error: Option<LdictStatus>,
    unit_domain: u32,
    limits: LdictByteEntryBatchLimits,
    last_key: Option<Vec<u64>>,
}

unsafe extern "C" fn reducer_trampoline(
    context: *mut std::ffi::c_void,
    batch: *const LdictByteEntryBatch,
) -> u32 {
    if context.is_null() {
        return VtStatus::NullPointer.to_raw();
    }
    let context = unsafe { &mut *context.cast::<ReducerContext>() };
    let Some(batch) = (unsafe { batch.as_ref() }) else {
        context.error = Some(LdictStatus::ProviderError);
        return VtStatus::ProviderError.to_raw();
    };
    match validate_batch(
        batch,
        &context.limits,
        context.unit_domain,
        context.last_key.as_deref(),
    ) {
        Ok(last) => context.last_key = Some(last),
        Err(_) => {
            context.error = Some(LdictStatus::ProviderError);
            return VtStatus::ProviderError.to_raw();
        }
    }
    let raw = unsafe { (context.callback)(context.callback_context, batch) };
    match ldict_status_from_raw(raw) {
        Some(LdictStatus::Ok) => VtStatus::Ok.to_raw(),
        Some(LdictStatus::End) => VtStatus::End.to_raw(),
        Some(status) => {
            context.error = Some(status);
            VtStatus::ProviderError.to_raw()
        }
        None => {
            context.error = Some(LdictStatus::InvalidArgument);
            VtStatus::ProviderError.to_raw()
        }
    }
}

/// Reduce bounded batches with one foreign call per batch, never per entry.
///
/// # Safety
/// Inputs and callback must remain valid throughout this synchronous call.
#[no_mangle]
pub unsafe extern "C" fn ldict_byte_entry_cursor_reduce(
    cursor: *mut LdictByteEntryCursor,
    limits: *const LdictByteEntryBatchLimits,
    reducer: Option<LdictByteEntryReducer>,
    reducer_context: *mut std::ffi::c_void,
    out_count: *mut usize,
) -> LdictStatus {
    boundary(|| {
        if limits.is_null() || out_count.is_null() || reducer.is_none() {
            return Err((
                LdictStatus::NullPointer,
                "byte-entry reduce input is null".into(),
            ));
        }
        let limits_value = unsafe { *limits };
        if limits_value.max_entries == 0 || limits_value.reserved != 0 {
            return Err((
                LdictStatus::InvalidArgument,
                "invalid byte-entry limits".into(),
            ));
        }
        let cursor_ptr = cursor_mut(cursor)? as *mut LdictByteEntryCursor;
        if unsafe { (*cursor_ptr).lease.is_some() } {
            return Err((LdictStatus::BatchInUse, "byte-entry lease is live".into()));
        }
        let reduce = unsafe { (*(*cursor_ptr).vtable).reduce }.ok_or((
            LdictStatus::ProviderError,
            "byte-entry reduce is null".into(),
        ))?;
        let mut context = ReducerContext {
            callback: reducer.expect("checked"),
            callback_context: reducer_context,
            error: None,
            unit_domain: unsafe { (*cursor_ptr).unit_domain },
            limits: limits_value,
            last_key: unsafe { (*cursor_ptr).last_key.clone() },
        };
        let mut count = 0usize;
        unsafe { (*cursor_ptr).in_callback = true };
        let raw = unsafe {
            reduce(
                &mut (*cursor_ptr).raw,
                limits,
                Some(reducer_trampoline),
                (&mut context as *mut ReducerContext).cast(),
                &mut count,
            )
        };
        unsafe {
            (*cursor_ptr).in_callback = false;
            (*cursor_ptr).last_key = context.last_key;
        }
        if let Some(status) = context.error {
            return Err((status, format!("byte-entry reducer returned {status:?}")));
        }
        let status = provider_status(raw, "byte-entry reduce")?;
        if status != VtStatus::Ok {
            return Err((
                LdictStatus::ProviderError,
                "byte-entry reduce returned end".into(),
            ));
        }
        unsafe { out_count.write(count) };
        Ok(LdictStatus::Ok)
    })
}

/// Request sticky exhaustion, without invalidating an existing lease.
///
/// # Safety
/// `cursor` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn ldict_byte_entry_cursor_cancel(
    cursor: *mut LdictByteEntryCursor,
) -> LdictStatus {
    boundary(|| {
        let cursor = cursor_mut(cursor)?;
        let cancel = unsafe { (*cursor.vtable).cancel }.ok_or((
            LdictStatus::ProviderError,
            "byte-entry cancel is null".into(),
        ))?;
        let status = provider_status(unsafe { cancel(&mut cursor.raw) }, "byte-entry cancel")?;
        if status != VtStatus::Ok {
            return Err((
                LdictStatus::ProviderError,
                "byte-entry cancel returned end".into(),
            ));
        }
        Ok(LdictStatus::Ok)
    })
}

/// Close a lease-free cursor. NULL is an idempotent no-op.
///
/// # Safety
/// On successful close the unique cursor pointer is consumed.
#[no_mangle]
pub unsafe extern "C" fn ldict_byte_entry_cursor_free(
    cursor: *mut LdictByteEntryCursor,
) -> LdictStatus {
    boundary(|| {
        if cursor.is_null() {
            return Ok(LdictStatus::Ok);
        }
        let cursor = unsafe { &mut *cursor };
        if cursor.in_callback || cursor.lease.is_some() {
            return Err((
                LdictStatus::BatchInUse,
                "byte-entry lease or reducer is live".into(),
            ));
        }
        let close = unsafe { (*cursor.vtable).close }.ok_or((
            LdictStatus::ProviderError,
            "byte-entry close is null".into(),
        ))?;
        let status = provider_status(unsafe { close(&mut cursor.raw) }, "byte-entry close")?;
        if status != VtStatus::Ok {
            return Err((
                LdictStatus::ProviderError,
                "byte-entry close returned end".into(),
            ));
        }
        unsafe { drop(Box::from_raw(cursor as *mut LdictByteEntryCursor)) };
        Ok(LdictStatus::Ok)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_provider_batches_are_rejected_before_publication() {
        let units = [b'a'];
        let values = [0xff];
        let mut entries = [LdictByteEntry {
            unit_offset: 0,
            unit_len: 1,
            value_offset: 0,
            value_len: 1,
            has_value: 1,
            reserved: [0; 7],
        }];
        let mut batch = LdictByteEntryBatch {
            entries: entries.as_ptr(),
            entry_count: 1,
            units: units.as_ptr().cast(),
            unit_count: 1,
            value_bytes: values.as_ptr(),
            value_byte_count: 1,
            generation: 1,
            reserved: 0,
        };
        let limits = LdictByteEntryBatchLimits {
            max_entries: 1,
            max_units: 1,
            max_value_bytes: 1,
            reserved: 0,
        };
        assert_eq!(
            validate_batch(&batch, &limits, 1, None).unwrap(),
            vec![b'a' as u64]
        );
        entries[0].has_value = 2;
        assert_eq!(
            validate_batch(&batch, &limits, 1, None).unwrap_err().0,
            LdictStatus::ProviderError
        );
        entries[0].has_value = 0;
        assert_eq!(
            validate_batch(&batch, &limits, 1, None).unwrap_err().0,
            LdictStatus::ProviderError
        );
        entries[0].has_value = 1;
        entries[0].value_offset = 1;
        assert_eq!(
            validate_batch(&batch, &limits, 1, None).unwrap_err().0,
            LdictStatus::ProviderError
        );
        entries[0].value_offset = 0;
        assert_eq!(entries[0].value_offset, 0);
        batch.value_byte_count = 2;
        assert_eq!(
            validate_batch(&batch, &limits, 1, None).unwrap_err().0,
            LdictStatus::ProviderError
        );
        batch.value_byte_count = 1;
        assert_eq!(
            validate_batch(&batch, &limits, 1, Some(&[b'a' as u64]))
                .unwrap_err()
                .0,
            LdictStatus::ProviderError
        );
        batch.generation = 0;
        assert_eq!(
            validate_batch(&batch, &limits, 1, None).unwrap_err().0,
            LdictStatus::ProviderError
        );
    }
}
