#include <stdlib.h>
#include "libdictenstein.h"

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
