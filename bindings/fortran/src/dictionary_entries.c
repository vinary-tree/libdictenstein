#include <stdint.h>
#include <stdlib.h>

#include "../../../include/libdictenstein.h"
#include "dictionary_entries.h"

VtBindingEntryCursor* vt_fortran_entries_open(
    const LdictDictionary* dictionary, size_t max_entries, size_t max_units,
    size_t max_values, uint32_t* out_unit_domain,
    uint32_t* out_value_domain, uint8_t* out_has_exact,
    size_t* out_exact, uint8_t* out_has_identity,
    uint64_t* out_producer, uint64_t* out_revision, int32_t* out_status) {
    if (!out_status) return NULL;
    VtResource resource = {0};
    *out_status = (int32_t)ldict_dictionary_resource(dictionary, &resource);
    if (*out_status != LDICT_STATUS_OK) return NULL;
    VtBindingEntryCursor* cursor =
        (VtBindingEntryCursor*)calloc(1, sizeof(VtBindingEntryCursor));
    if (!cursor) {
        *out_status = LDICT_STATUS_LIMIT_EXCEEDED;
        return NULL;
    }
    *out_status = (int32_t)vt_binding_entries_open(
        &resource, max_entries, max_units, max_values, cursor);
    if (*out_status != LDICT_STATUS_OK) {
        free(cursor);
        return NULL;
    }
    *out_unit_domain = cursor->info.unit_domain;
    *out_value_domain = cursor->info.value_domain;
    *out_has_exact = (cursor->info.flags
        & VT_DICTIONARY_ENTRIES_INFO_FLAG_EXACT_LEN) != 0;
    *out_exact = cursor->info.exact_len;
    *out_has_identity = (cursor->info.flags
        & VT_DICTIONARY_ENTRIES_INFO_FLAG_SNAPSHOT_IDENTITY) != 0;
    *out_producer = cursor->info.identity.producer;
    *out_revision = cursor->info.identity.revision;
    return cursor;
}

int32_t vt_fortran_entries_next(
    VtBindingEntryCursor* cursor, const void** out_units, size_t* out_length,
    uint64_t* out_value, uint8_t* out_has_value, uint8_t* out_has_entry) {
    if (!cursor || !out_units || !out_length || !out_value
        || !out_has_value || !out_has_entry)
        return LDICT_STATUS_NULL_POINTER;
    VtBindingEntryView view = {0};
    VtStatus status = vt_binding_entries_next(cursor, &view, out_has_entry);
    *out_units = view.units;
    *out_length = view.unit_len;
    *out_value = view.value;
    *out_has_value = view.has_value;
    return (int32_t)status;
}

int32_t vt_fortran_entries_close(VtBindingEntryCursor* cursor) {
    return (int32_t)vt_binding_entries_close(cursor);
}

void vt_fortran_entries_free(VtBindingEntryCursor* cursor) {
    if (!cursor) return;
    (void)vt_binding_entries_close(cursor);
    free(cursor);
}
