#include "libdictenstein.h"

#include <assert.h>
#include <limits.h>
#include <stddef.h>
#include <stdint.h>

static LdictOptionalU64 value(uint64_t number) {
    LdictOptionalU64 result = {number, 1, {0}};
    return result;
}

static LdictOptionalU64 no_value(void) {
    LdictOptionalU64 result = {0, 0, {0}};
    return result;
}

static void insert_text(LdictDictionary* dictionary, const uint8_t* data,
                        size_t len, LdictOptionalU64 mapped) {
    uint8_t inserted = 0;
    assert(ldict_dictionary_insert_text(dictionary, data, len, mapped,
                                        &inserted) == LDICT_STATUS_OK);
    assert(inserted == 1);
}

static void insert_u64(LdictDictionary* dictionary, const uint64_t* data,
                       size_t len, LdictOptionalU64 mapped) {
    uint8_t inserted = 0;
    assert(ldict_dictionary_insert_u64(dictionary, data, len, mapped,
                                       &inserted) == LDICT_STATUS_OK);
    assert(inserted == 1);
}

typedef struct ReduceState {
    size_t calls;
    size_t observed;
} ReduceState;

static LdictStatus stop_after_one_batch(void* context,
                                        const LdictEntryBatch* batch) {
    ReduceState* state = (ReduceState*)context;
    assert(batch != NULL);
    assert(batch->entry_count > 0);
    state->calls += 1;
    state->observed += batch->entry_count;
    return LDICT_STATUS_END;
}

static void check_unicode_and_lifecycle(void) {
    LdictDictionary* dictionary = NULL;
    assert(ldict_dynamic_dawg_new(VT_UNIT_DOMAIN_UNICODE_SCALAR,
                                  &dictionary) == LDICT_STATUS_OK);
    insert_text(dictionary, NULL, 0, no_value());
    insert_text(dictionary, (const uint8_t*)"a", 1, value(0));
    insert_text(dictionary, (const uint8_t*)"\xC3\xA9", 2,
                value(UINT64_MAX));

    LdictEntryCursor* cursor = NULL;
    LdictEntriesInfo info = {0};
    assert(ldict_dictionary_entries_open(dictionary, &cursor, &info) ==
           LDICT_STATUS_OK);
    assert(cursor != NULL);
    assert(info.unit_domain == VT_UNIT_DOMAIN_UNICODE_SCALAR);
    assert(info.value_domain == VT_VALUE_DOMAIN_OPTIONAL_U64);
    assert(info.order == LDICT_ENTRY_ORDER_LEXICOGRAPHIC);
    assert((info.flags & LDICT_ENTRIES_INFO_FLAG_EXACT_LEN) != 0);
    assert(info.exact_len == 3);

    /* The opened cursor owns its revision. */
    insert_text(dictionary, (const uint8_t*)"later", 5, value(7));

    const LdictEntryBatchLimits limits = {1, 8, 1, 0};
    size_t seen = 0;
    for (;;) {
        LdictEntryBatch batch = {0};
        const LdictStatus status =
            ldict_entry_cursor_next(cursor, &limits, &batch);
        if (status == LDICT_STATUS_END) break;
        assert(status == LDICT_STATUS_OK);
        assert(batch.entry_count == 1);
        assert(batch.unit_count <= limits.max_units);
        assert(batch.value_count <= limits.max_values);
        const LdictEntry* entry = &batch.entries[0];
        assert(entry->value_len <= 1);
        if (seen == 0) {
            assert(entry->unit_len == 0);
            assert(entry->value_len == 0);
        } else if (seen == 1) {
            const uint32_t* units = (const uint32_t*)batch.units;
            assert(entry->unit_len == 1);
            assert(units[entry->unit_offset] == (uint32_t)'a');
            assert(entry->value_len == 1);
            assert(batch.values[entry->value_offset] == 0);
        } else {
            const uint32_t* units = (const uint32_t*)batch.units;
            assert(entry->unit_len == 1);
            assert(units[entry->unit_offset] == UINT32_C(0xE9));
            assert(entry->value_len == 1);
            assert(batch.values[entry->value_offset] == UINT64_MAX);
        }

        LdictEntryBatch blocked = {0};
        assert(ldict_entry_cursor_next(cursor, &limits, &blocked) ==
               LDICT_STATUS_BATCH_IN_USE);
        assert(blocked.entry_count == 0);
        assert(ldict_entry_cursor_free(cursor) == LDICT_STATUS_BATCH_IN_USE);
        assert(ldict_entry_cursor_release(cursor, batch.generation) ==
               LDICT_STATUS_OK);
        ++seen;
    }
    assert(seen == 3);
    assert(ldict_entry_cursor_free(cursor) == LDICT_STATUS_OK);

    cursor = NULL;
    assert(ldict_dictionary_entries_open(dictionary, &cursor, &info) ==
           LDICT_STATUS_OK);
    /* The cursor's retained snapshot remains valid after its source handle. */
    ldict_dictionary_free(dictionary);
    dictionary = NULL;
    ReduceState state = {0, 0};
    size_t reduced = 0;
    assert(ldict_entry_cursor_reduce(cursor, &limits, stop_after_one_batch,
                                     &state, &reduced) == LDICT_STATUS_OK);
    assert(state.calls == 1);
    assert(state.observed == 1);
    assert(reduced == 1);
    assert(ldict_entry_cursor_cancel(cursor) == LDICT_STATUS_OK);
    assert(ldict_entry_cursor_cancel(cursor) == LDICT_STATUS_OK);
    assert(ldict_entry_cursor_next(cursor, &limits, &(LdictEntryBatch){0}) ==
           LDICT_STATUS_END);
    assert(ldict_entry_cursor_free(cursor) == LDICT_STATUS_OK);
}

static void check_byte_and_u64_domains(void) {
    LdictDictionary* bytes = NULL;
    assert(ldict_dynamic_dawg_new(VT_UNIT_DOMAIN_BYTE, &bytes) ==
           LDICT_STATUS_OK);
    const uint8_t raw[] = {0, UINT8_MAX};
    insert_text(bytes, raw, 2, no_value());
    LdictEntryCursor* cursor = NULL;
    LdictEntriesInfo info = {0};
    assert(ldict_dictionary_entries_open(bytes, &cursor, &info) ==
           LDICT_STATUS_OK);
    assert(info.unit_domain == VT_UNIT_DOMAIN_BYTE);
    const LdictEntryBatchLimits byte_limits = {1, 2, 1, 0};
    LdictEntryBatch batch = {0};
    assert(ldict_entry_cursor_next(cursor, &byte_limits, &batch) ==
           LDICT_STATUS_OK);
    assert(batch.unit_count == 2);
    assert(((const uint8_t*)batch.units)[0] == 0);
    assert(((const uint8_t*)batch.units)[1] == UINT8_MAX);
    assert(ldict_entry_cursor_release(cursor, batch.generation) ==
           LDICT_STATUS_OK);
    assert(ldict_entry_cursor_free(cursor) == LDICT_STATUS_OK);
    ldict_dictionary_free(bytes);

    LdictDictionary* tokens = NULL;
    assert(ldict_dynamic_dawg_new(VT_UNIT_DOMAIN_U64, &tokens) ==
           LDICT_STATUS_OK);
    const uint64_t key[] = {1, UINT64_MAX};
    insert_u64(tokens, key, 2, value(0));
    cursor = NULL;
    assert(ldict_dictionary_entries_open(tokens, &cursor, &info) ==
           LDICT_STATUS_OK);
    assert(info.unit_domain == VT_UNIT_DOMAIN_U64);
    const LdictEntryBatchLimits token_limits = {1, 2, 1, 0};
    batch = (LdictEntryBatch){0};
    assert(ldict_entry_cursor_next(cursor, &token_limits, &batch) ==
           LDICT_STATUS_OK);
    assert(((const uint64_t*)batch.units)[0] == 1);
    assert(((const uint64_t*)batch.units)[1] == UINT64_MAX);
    assert(batch.entries[0].value_len == 1);
    assert(batch.values[batch.entries[0].value_offset] == 0);
    assert(ldict_entry_cursor_release(cursor, batch.generation) ==
           LDICT_STATUS_OK);
    assert(ldict_entry_cursor_free(cursor) == LDICT_STATUS_OK);
    ldict_dictionary_free(tokens);
}

static void check_retryable_limits(void) {
    LdictDictionary* dictionary = NULL;
    assert(ldict_dynamic_dawg_new(VT_UNIT_DOMAIN_BYTE, &dictionary) ==
           LDICT_STATUS_OK);
    const uint8_t key[] = {'a', 'b'};
    insert_text(dictionary, key, 2, value(9));

    LdictEntryCursor* cursor = NULL;
    LdictEntriesInfo info = {0};
    assert(ldict_dictionary_entries_open(dictionary, &cursor, &info) ==
           LDICT_STATUS_OK);
    const LdictEntryBatchLimits too_small = {1, 1, 1, 0};
    LdictEntryBatch batch = {0};
    assert(ldict_entry_cursor_next(cursor, &too_small, &batch) ==
           LDICT_STATUS_LIMIT_EXCEEDED);
    assert(batch.entry_count == 0);

    const LdictEntryBatchLimits fits = {1, 2, 1, 0};
    assert(ldict_entry_cursor_next(cursor, &fits, &batch) == LDICT_STATUS_OK);
    assert(batch.entry_count == 1);
    assert(ldict_entry_cursor_release(cursor, batch.generation + 1) ==
           LDICT_STATUS_INVALID_ARGUMENT);
    assert(ldict_entry_cursor_release(cursor, batch.generation) ==
           LDICT_STATUS_OK);
    assert(ldict_entry_cursor_free(cursor) == LDICT_STATUS_OK);
    ldict_dictionary_free(dictionary);
}

int main(void) {
    check_unicode_and_lifecycle();
    check_byte_and_u64_domains();
    check_retryable_limits();
    return 0;
}
