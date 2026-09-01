#ifndef VINARY_TREE_BINDING_DICTIONARY_ENTRIES_H
#define VINARY_TREE_BINDING_DICTIONARY_ENTRIES_H

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "vinary_tree_interop.h"

/* Private, header-only adapter shared by the native language facades. It
 * copies and releases every native batch before host-language code runs. */
typedef struct VtBindingEntryView {
    const void* units;
    size_t unit_len;
    uint64_t value;
    uint8_t has_value;
} VtBindingEntryView;

typedef struct VtBindingEntryCursor {
    VtDictionaryEntriesCursor native;
    VtDictionaryEntriesInfo info;
    VtDictionaryEntryBatchLimits limits;
    VtDictionaryEntry* entries;
    uint8_t* units;
    uint64_t* values;
    size_t entry_count;
    size_t entry_index;
    size_t delivered;
    uint8_t* previous;
    size_t previous_len;
    uint8_t previous_known;
    uint8_t ended;
    uint8_t closed;
    const char* error;
} VtBindingEntryCursor;

static const VtInterfaceId VT_BINDING_ENTRIES_ID = {
    { 'v','t','.','d','i','c','t','.','e','n','t','r','y','.','v','1' }
};

static size_t vt_binding_unit_size(uint32_t domain) {
    switch (domain) {
        case VT_UNIT_DOMAIN_BYTE: return 1;
        case VT_UNIT_DOMAIN_UNICODE_SCALAR: return sizeof(uint32_t);
        case VT_UNIT_DOMAIN_U64: return sizeof(uint64_t);
        default: return 0;
    }
}

static VtStatus vt_binding_entries_error(
    VtBindingEntryCursor* cursor, VtStatus status, const char* message) {
    cursor->error = message;
    return status;
}

static int vt_binding_pointer_count(const void* pointer, size_t count) {
    return (pointer == NULL) == (count == 0);
}

static int vt_binding_mul(size_t left, size_t right, size_t* output) {
    if (left != 0 && right > SIZE_MAX / left) return 0;
    *output = left * right;
    return 1;
}

static void vt_binding_entries_free_batch(VtBindingEntryCursor* cursor) {
    free(cursor->entries);
    free(cursor->units);
    free(cursor->values);
    cursor->entries = NULL;
    cursor->units = NULL;
    cursor->values = NULL;
    cursor->entry_count = 0;
    cursor->entry_index = 0;
}

static int vt_binding_compare_units(
    uint32_t domain,
    const void* left,
    size_t left_len,
    const void* right,
    size_t right_len) {
    size_t common = left_len < right_len ? left_len : right_len;
    for (size_t index = 0; index < common; ++index) {
        uint64_t a = 0;
        uint64_t b = 0;
        if (domain == VT_UNIT_DOMAIN_BYTE) {
            a = ((const uint8_t*)left)[index];
            b = ((const uint8_t*)right)[index];
        } else if (domain == VT_UNIT_DOMAIN_UNICODE_SCALAR) {
            a = ((const uint32_t*)left)[index];
            b = ((const uint32_t*)right)[index];
        } else {
            a = ((const uint64_t*)left)[index];
            b = ((const uint64_t*)right)[index];
        }
        if (a < b) return -1;
        if (a > b) return 1;
    }
    return left_len < right_len ? -1 : left_len > right_len ? 1 : 0;
}

static int vt_binding_scalar_valid(uint32_t value) {
    return value <= UINT32_C(0x10ffff)
        && !(value >= UINT32_C(0xd800) && value <= UINT32_C(0xdfff));
}

static int vt_binding_empty_batch(const VtDictionaryEntryBatchView* batch) {
    return batch->entries == NULL && batch->entry_count == 0
        && batch->units == NULL && batch->unit_count == 0
        && batch->values == NULL && batch->value_count == 0
        && batch->generation == 0 && batch->reserved == 0;
}

static VtStatus vt_binding_entries_close(VtBindingEntryCursor* cursor) {
    if (!cursor) return VT_STATUS_NULL_POINTER;
    vt_binding_entries_free_batch(cursor);
    free(cursor->previous);
    cursor->previous = NULL;
    cursor->previous_len = 0;
    cursor->previous_known = 0;
    if (cursor->closed) return VT_STATUS_OK;
    const VtDictionaryEntriesVTable* table = cursor->native.vtable;
    if (!table || !table->close) {
        cursor->closed = 1;
        return vt_binding_entries_error(
            cursor, VT_STATUS_PROVIDER_ERROR, "entries cursor has no close operation");
    }
    VtStatus cancel_status = VT_STATUS_OK;
    if (!cursor->ended && table->cancel)
        cancel_status = table->cancel(&cursor->native);
    VtStatus close_status = table->close(&cursor->native);
    if (close_status == VT_STATUS_OK) cursor->closed = 1;
    if (cancel_status != VT_STATUS_OK)
        return vt_binding_entries_error(cursor, cancel_status, "entries cancel failed");
    if (close_status != VT_STATUS_OK)
        return vt_binding_entries_error(cursor, close_status, "entries close failed");
    return VT_STATUS_OK;
}

static VtStatus vt_binding_entries_open(
    const VtResource* resource,
    size_t max_entries,
    size_t max_units,
    size_t max_values,
    VtBindingEntryCursor* cursor) {
    if (!cursor) return VT_STATUS_NULL_POINTER;
    memset(cursor, 0, sizeof(*cursor));
    cursor->limits.max_entries = max_entries;
    cursor->limits.max_units = max_units;
    cursor->limits.max_values = max_values;
    if (!resource || !resource->context || !resource->vtable)
        return vt_binding_entries_error(cursor, VT_STATUS_INVALID_ARGUMENT,
                                        "dictionary resource is null or half-null");
    const VtResourceVTable* base = resource->vtable;
    if (base->struct_size < sizeof(VtResourceVTable)
        || base->abi_version != VT_ABI_VERSION || base->reserved != 0
        || !base->retain || !base->release || !base->query_interface)
        return vt_binding_entries_error(cursor, VT_STATUS_PROVIDER_ERROR,
                                        "dictionary resource vtable is incompatible");
    if (max_entries == 0)
        return vt_binding_entries_error(cursor, VT_STATUS_INVALID_ARGUMENT,
                                        "max_entries must be positive");
    const void* discovered = NULL;
    VtStatus status = base->query_interface(
        resource->context, &VT_BINDING_ENTRIES_ID,
        VT_DICTIONARY_ENTRIES_INTERFACE_VERSION, &discovered);
    if (status != VT_STATUS_OK) {
        cursor->error = status == VT_STATUS_UNSUPPORTED
            ? "dictionary entries interface is unsupported"
            : "dictionary entries interface query failed";
        return status;
    }
    const VtDictionaryEntriesVTable* table =
        (const VtDictionaryEntriesVTable*)discovered;
    if (!table || table->struct_size < sizeof(VtDictionaryEntriesVTable)
        || table->interface_version < VT_DICTIONARY_ENTRIES_INTERFACE_VERSION
        || table->reserved != 0 || !table->open || !table->next_batch
        || !table->release_batch || !table->reduce || !table->cancel || !table->close)
        return vt_binding_entries_error(cursor, VT_STATUS_PROVIDER_ERROR,
                                        "dictionary entries vtable is incompatible");
    status = table->open(resource->context, &cursor->native, &cursor->info);
    if (status != VT_STATUS_OK) {
        cursor->error = "opening dictionary entries cursor failed";
        return status;
    }
    const VtDictionaryEntriesVTable* cursor_table = cursor->native.vtable;
    if (!cursor->native.context || !cursor_table
        || cursor_table->struct_size < sizeof(VtDictionaryEntriesVTable)
        || cursor_table->interface_version < VT_DICTIONARY_ENTRIES_INTERFACE_VERSION
        || cursor_table->reserved != 0 || !cursor_table->next_batch
        || !cursor_table->release_batch || !cursor_table->cancel || !cursor_table->close) {
        table->close(&cursor->native);
        return vt_binding_entries_error(cursor, VT_STATUS_PROVIDER_ERROR,
                                        "opened dictionary entries cursor is incompatible");
    }
    uint64_t flags = cursor->info.flags;
    if (vt_binding_unit_size(cursor->info.unit_domain) == 0
        || (cursor->info.value_domain != VT_VALUE_DOMAIN_UNIT
            && cursor->info.value_domain != VT_VALUE_DOMAIN_OPTIONAL_U64)
        || cursor->info.order != VT_DICTIONARY_ENTRY_ORDER_LEXICOGRAPHIC
        || cursor->info.reserved0 != 0 || cursor->info.reserved[0] != 0
        || cursor->info.reserved[1] != 0
        || (!(flags & VT_DICTIONARY_ENTRIES_INFO_FLAG_EXACT_LEN)
            && cursor->info.exact_len != 0)
        || (!(flags & VT_DICTIONARY_ENTRIES_INFO_FLAG_SNAPSHOT_IDENTITY)
            && (cursor->info.identity.producer != 0
                || cursor->info.identity.revision != 0))) {
        cursor->native.vtable->close(&cursor->native);
        cursor->closed = 1;
        return vt_binding_entries_error(cursor, VT_STATUS_PROVIDER_ERROR,
                                        "dictionary entries metadata is malformed");
    }
    return VT_STATUS_OK;
}

static VtStatus vt_binding_entries_copy_batch(
    VtBindingEntryCursor* cursor,
    const VtDictionaryEntryBatchView* batch) {
    size_t unit_size = vt_binding_unit_size(cursor->info.unit_domain);
    size_t entry_bytes = 0;
    size_t unit_bytes = 0;
    size_t value_bytes = 0;
    if (batch->entry_count == 0 || batch->entry_count > cursor->limits.max_entries
        || batch->unit_count > cursor->limits.max_units
        || batch->value_count > cursor->limits.max_values
        || batch->generation == 0 || batch->reserved != 0
        || !vt_binding_pointer_count(batch->entries, batch->entry_count)
        || !vt_binding_pointer_count(batch->units, batch->unit_count)
        || !vt_binding_pointer_count(batch->values, batch->value_count)
        || ((uintptr_t)batch->entries % _Alignof(VtDictionaryEntry)) != 0
        || (batch->unit_count && ((uintptr_t)batch->units % unit_size) != 0)
        || (batch->value_count
            && ((uintptr_t)batch->values % _Alignof(uint64_t)) != 0)
        || !vt_binding_mul(batch->entry_count, sizeof(VtDictionaryEntry), &entry_bytes)
        || !vt_binding_mul(batch->unit_count, unit_size, &unit_bytes)
        || !vt_binding_mul(batch->value_count, sizeof(uint64_t), &value_bytes))
        return vt_binding_entries_error(cursor, VT_STATUS_PROVIDER_ERROR,
                                        "dictionary entries batch is malformed");

    VtDictionaryEntry* entries = (VtDictionaryEntry*)malloc(entry_bytes);
    uint8_t* units = unit_bytes ? (uint8_t*)malloc(unit_bytes) : NULL;
    uint64_t* values = value_bytes ? (uint64_t*)malloc(value_bytes) : NULL;
    if (!entries || (unit_bytes && !units) || (value_bytes && !values)) {
        free(entries); free(units); free(values);
        return vt_binding_entries_error(cursor, VT_STATUS_LIMIT_EXCEEDED,
                                        "allocating an owned entries batch failed");
    }
    memcpy(entries, batch->entries, entry_bytes);
    if (unit_bytes) memcpy(units, batch->units, unit_bytes);
    if (value_bytes) memcpy(values, batch->values, value_bytes);

    size_t next_unit = 0;
    size_t next_value = 0;
    const void* prior = cursor->previous;
    size_t prior_len = cursor->previous_len;
    int prior_known = cursor->previous_known;
    for (size_t index = 0; index < batch->entry_count; ++index) {
        const VtDictionaryEntry* entry = &entries[index];
        if (entry->reserved != 0 || entry->unit_offset != next_unit
            || entry->value_offset != next_value
            || entry->unit_offset > batch->unit_count
            || entry->value_offset > batch->value_count
            || entry->unit_len > batch->unit_count - entry->unit_offset
            || entry->value_len > batch->value_count - entry->value_offset
            || (cursor->info.value_domain == VT_VALUE_DOMAIN_UNIT
                && entry->value_len != 0)
            || (cursor->info.value_domain == VT_VALUE_DOMAIN_OPTIONAL_U64
                && entry->value_len > 1)) {
            free(entries); free(units); free(values);
            return vt_binding_entries_error(cursor, VT_STATUS_PROVIDER_ERROR,
                                            "dictionary entry descriptors are malformed");
        }
        const void* current = units
            ? units + entry->unit_offset * unit_size
            : NULL;
        if (cursor->info.unit_domain == VT_UNIT_DOMAIN_UNICODE_SCALAR) {
            const uint32_t* scalars = (const uint32_t*)current;
            for (size_t scalar = 0; scalar < entry->unit_len; ++scalar) {
                if (!vt_binding_scalar_valid(scalars[scalar])) {
                    free(entries); free(units); free(values);
                    return vt_binding_entries_error(cursor, VT_STATUS_PROVIDER_ERROR,
                                                    "entry contains an invalid Unicode scalar");
                }
            }
        }
        if (prior_known && vt_binding_compare_units(
                cursor->info.unit_domain, prior, prior_len,
                current, entry->unit_len) >= 0) {
            free(entries); free(units); free(values);
            return vt_binding_entries_error(cursor, VT_STATUS_PROVIDER_ERROR,
                                            "entries are not strictly lexicographic");
        }
        prior = current;
        prior_len = entry->unit_len;
        prior_known = 1;
        next_unit = entry->unit_offset + entry->unit_len;
        next_value = entry->value_offset + entry->value_len;
    }
    if (next_unit != batch->unit_count || next_value != batch->value_count
        || batch->entry_count > SIZE_MAX - cursor->delivered) {
        free(entries); free(units); free(values);
        return vt_binding_entries_error(cursor, VT_STATUS_PROVIDER_ERROR,
                                        "dictionary entry arenas are not canonical");
    }
    size_t delivered = cursor->delivered + batch->entry_count;
    if ((cursor->info.flags & VT_DICTIONARY_ENTRIES_INFO_FLAG_EXACT_LEN)
        && delivered > cursor->info.exact_len) {
        free(entries); free(units); free(values);
        return vt_binding_entries_error(cursor, VT_STATUS_PROVIDER_ERROR,
                                        "entries exceed the advertised exact length");
    }

    const VtDictionaryEntry* last = &entries[batch->entry_count - 1];
    size_t previous_bytes = last->unit_len * unit_size;
    uint8_t* previous = previous_bytes ? (uint8_t*)malloc(previous_bytes) : NULL;
    if (previous_bytes && !previous) {
        free(entries); free(units); free(values);
        return vt_binding_entries_error(cursor, VT_STATUS_LIMIT_EXCEEDED,
                                        "allocating the order-validation key failed");
    }
    if (previous_bytes)
        memcpy(previous, units + last->unit_offset * unit_size, previous_bytes);
    free(cursor->previous);
    cursor->previous = previous;
    cursor->previous_len = last->unit_len;
    cursor->previous_known = 1;
    cursor->entries = entries;
    cursor->units = units;
    cursor->values = values;
    cursor->entry_count = batch->entry_count;
    cursor->entry_index = 0;
    cursor->delivered = delivered;
    return VT_STATUS_OK;
}

static VtStatus vt_binding_entries_refill(VtBindingEntryCursor* cursor) {
    vt_binding_entries_free_batch(cursor);
    VtDictionaryEntryBatchView batch;
    memset(&batch, 0, sizeof(batch));
    const VtDictionaryEntriesVTable* table = cursor->native.vtable;
    VtStatus status = table->next_batch(&cursor->native, &cursor->limits, &batch);
    if (status == VT_STATUS_END) {
        if (!vt_binding_empty_batch(&batch)
            || ((cursor->info.flags & VT_DICTIONARY_ENTRIES_INFO_FLAG_EXACT_LEN)
                && cursor->delivered != cursor->info.exact_len))
            return vt_binding_entries_error(cursor, VT_STATUS_PROVIDER_ERROR,
                                            "entries ended with inconsistent metadata");
        cursor->ended = 1;
        status = table->close(&cursor->native);
        if (status == VT_STATUS_OK) cursor->closed = 1;
        else cursor->error = "closing an exhausted entries cursor failed";
        return status == VT_STATUS_OK ? VT_STATUS_END : status;
    }
    if (status != VT_STATUS_OK) {
        cursor->error = "fetching the next entries batch failed";
        return status;
    }
    VtStatus copy_status = vt_binding_entries_copy_batch(cursor, &batch);
    VtStatus release_status = table->release_batch(&cursor->native, batch.generation);
    if (copy_status != VT_STATUS_OK) {
        vt_binding_entries_free_batch(cursor);
        return copy_status;
    }
    if (release_status != VT_STATUS_OK) {
        vt_binding_entries_free_batch(cursor);
        return vt_binding_entries_error(cursor, release_status,
                                        "releasing an entries batch failed");
    }
    return VT_STATUS_OK;
}

static VtStatus vt_binding_entries_next(
    VtBindingEntryCursor* cursor,
    VtBindingEntryView* output,
    uint8_t* has_entry) {
    if (!cursor || !output || !has_entry) return VT_STATUS_NULL_POINTER;
    *has_entry = 0;
    memset(output, 0, sizeof(*output));
    if (cursor->closed) return cursor->ended ? VT_STATUS_END : VT_STATUS_CLOSED;
    if (cursor->entry_index == cursor->entry_count) {
        VtStatus status = vt_binding_entries_refill(cursor);
        if (status != VT_STATUS_OK) return status;
    }
    const VtDictionaryEntry* entry = &cursor->entries[cursor->entry_index++];
    size_t unit_size = vt_binding_unit_size(cursor->info.unit_domain);
    output->units = cursor->units
        ? cursor->units + entry->unit_offset * unit_size
        : NULL;
    output->unit_len = entry->unit_len;
    output->has_value = entry->value_len == 1;
    output->value = output->has_value ? cursor->values[entry->value_offset] : 0;
    *has_entry = 1;
    return VT_STATUS_OK;
}

#endif
