#include <stdlib.h>
#include "libdictenstein.h"
#include "dictionary_entries.h"

VtResource* ldict_hs_resource(const LdictDictionary* dictionary, LdictStatus* out_status) {
    if (!out_status) return NULL;
    VtResource* resource = (VtResource*)calloc(1, sizeof(VtResource));
    if (!resource) {
        *out_status = LDICT_STATUS_LIMIT_EXCEEDED;
        return NULL;
    }
    *out_status = ldict_dictionary_resource(dictionary, resource);
    if (*out_status != LDICT_STATUS_OK) {
        free(resource);
        return NULL;
    }
    resource->vtable->retain(resource->context);
    return resource;
}

LdictStatus ldict_hs_double_array_trie_new(
    uint32_t domain, const uint8_t* const* data, const size_t* lengths,
    const uint64_t* values, const uint8_t* has_values, size_t count,
    LdictDictionary** output) {
    LdictTextEntry* entries = count ? calloc(count, sizeof(LdictTextEntry)) : NULL;
    if (count && !entries) return LDICT_STATUS_LIMIT_EXCEEDED;
    for (size_t index = 0; index < count; ++index) {
        entries[index].data = data[index];
        entries[index].len = lengths[index];
        entries[index].value.value = values[index];
        entries[index].value.has_value = has_values[index];
    }
    LdictStatus status = ldict_double_array_trie_new(domain, entries, count, output);
    free(entries);
    return status;
}

LdictStatus ldict_hs_insert_text_batch(
    LdictDictionary* dictionary, const uint8_t* const* data,
    const size_t* lengths, const uint64_t* values, const uint8_t* has_values,
    size_t count, size_t* inserted) {
    LdictTextEntry* entries = count ? calloc(count, sizeof(LdictTextEntry)) : NULL;
    if (count && !entries) return LDICT_STATUS_LIMIT_EXCEEDED;
    for (size_t index = 0; index < count; ++index) {
        entries[index].data = data[index];
        entries[index].len = lengths[index];
        entries[index].value.value = values[index];
        entries[index].value.has_value = has_values[index];
    }
    LdictStatus status = ldict_dictionary_insert_text_batch(
        dictionary, entries, count, inserted);
    free(entries);
    return status;
}

VtBindingEntryCursor* ldict_hs_entries_open(
    const LdictDictionary* dictionary, size_t max_entries, size_t max_units,
    size_t max_values, LdictStatus* out_status) {
    if (!out_status) return NULL;
    VtResource resource = {0};
    *out_status = ldict_dictionary_resource(dictionary, &resource);
    if (*out_status != LDICT_STATUS_OK) return NULL;
    VtBindingEntryCursor* cursor =
        (VtBindingEntryCursor*)calloc(1, sizeof(VtBindingEntryCursor));
    if (!cursor) {
        *out_status = LDICT_STATUS_LIMIT_EXCEEDED;
        return NULL;
    }
    *out_status = (LdictStatus)vt_binding_entries_open(
        &resource, max_entries, max_units, max_values, cursor);
    if (*out_status != LDICT_STATUS_OK) {
        free(cursor);
        return NULL;
    }
    return cursor;
}

LdictStatus ldict_hs_entries_next(
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
    return (LdictStatus)status;
}

void ldict_hs_entries_info(
    const VtBindingEntryCursor* cursor, uint32_t* out_unit_domain,
    uint32_t* out_value_domain, uint8_t* out_has_exact, size_t* out_exact,
    uint8_t* out_has_identity, uint64_t* out_producer, uint64_t* out_revision) {
    *out_unit_domain = cursor->info.unit_domain;
    *out_value_domain = cursor->info.value_domain;
    *out_has_exact = (cursor->info.flags
        & VT_DICTIONARY_ENTRIES_INFO_FLAG_EXACT_LEN) != 0;
    *out_exact = cursor->info.exact_len;
    *out_has_identity = (cursor->info.flags
        & VT_DICTIONARY_ENTRIES_INFO_FLAG_SNAPSHOT_IDENTITY) != 0;
    *out_producer = cursor->info.identity.producer;
    *out_revision = cursor->info.identity.revision;
}

const char* ldict_hs_entries_error(const VtBindingEntryCursor* cursor) {
    return cursor && cursor->error ? cursor->error : "";
}

LdictStatus ldict_hs_entries_close(VtBindingEntryCursor* cursor) {
    return (LdictStatus)vt_binding_entries_close(cursor);
}

void ldict_hs_entries_free(VtBindingEntryCursor* cursor) {
    if (!cursor) return;
    (void)vt_binding_entries_close(cursor);
    free(cursor);
}
