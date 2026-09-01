#define _POSIX_C_SOURCE 200809L

#include "libdictenstein.h"

#include <errno.h>
#include <inttypes.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

enum {
    DEFAULT_ENTRIES = 65536,
    DEFAULT_BATCH_SIZE = 256,
    DEFAULT_EARLY_CANCEL = 64,
    KEY_UNITS = 38,
};

typedef enum ProfileArm {
    ARM_MATERIALIZED,
    ARM_STREAM,
    ARM_STREAM_CANCEL,
    ARM_REDUCER,
} ProfileArm;

typedef struct ProfileConfig {
    ProfileArm arm;
    const char* arm_name;
    size_t entries;
    size_t passes;
    size_t warmup_passes;
    size_t batch_size;
    size_t early_cancel;
} ProfileConfig;

typedef struct CorpusEntry {
    char key[KEY_UNITS + 1];
    uint64_t value;
} CorpusEntry;

typedef struct OwnedEntry {
    uint8_t* key;
    size_t key_len;
    uint64_t value;
    bool has_value;
} OwnedEntry;

typedef struct DrainResult {
    uint64_t checksum;
    size_t count;
} DrainResult;

typedef struct ReduceContext {
    uint64_t checksum;
    size_t count;
    size_t limit;
} ReduceContext;

static void fail(const char* message) {
    fprintf(stderr, "%s\n", message);
    exit(2);
}

static void require_status(LdictStatus status, const char* operation) {
    if (status == LDICT_STATUS_OK) return;
    fprintf(stderr, "%s failed with status %u: %s\n", operation,
            (unsigned)status, ldict_last_error_message());
    exit(2);
}

static void* allocate(size_t count, size_t width) {
    if (count != 0 && width > SIZE_MAX / count) fail("allocation size overflow");
    void* result = calloc(count == 0 ? 1 : count, width);
    if (result == NULL) fail("allocation failed");
    return result;
}

static size_t parse_size(const char* value, const char* option,
                         bool allow_zero) {
    char* end = NULL;
    errno = 0;
    const uintmax_t parsed = strtoumax(value, &end, 10);
    if (errno != 0 || end == value || *end != '\0' || parsed > SIZE_MAX ||
        (!allow_zero && parsed == 0)) {
        fprintf(stderr, "%s must be a %s integer\n", option,
                allow_zero ? "nonnegative" : "positive");
        exit(2);
    }
    return (size_t)parsed;
}

static ProfileConfig parse_arguments(int argc, char** argv) {
    ProfileConfig config = {ARM_STREAM, NULL, DEFAULT_ENTRIES, 1, 1,
                            DEFAULT_BATCH_SIZE, DEFAULT_EARLY_CANCEL};
    for (int index = 1; index < argc; index += 2) {
        if (index + 1 >= argc) fail("every option requires a value");
        const char* option = argv[index];
        const char* value = argv[index + 1];
        if (strcmp(option, "--arm") == 0) {
            config.arm_name = value;
            if (strcmp(value, "materialized") == 0)
                config.arm = ARM_MATERIALIZED;
            else if (strcmp(value, "stream") == 0)
                config.arm = ARM_STREAM;
            else if (strcmp(value, "stream-cancel") == 0)
                config.arm = ARM_STREAM_CANCEL;
            else if (strcmp(value, "reduce") == 0)
                config.arm = ARM_REDUCER;
            else
                fail("--arm must be materialized, stream, stream-cancel, or reduce");
        } else if (strcmp(option, "--entries") == 0) {
            config.entries = parse_size(value, option, false);
        } else if (strcmp(option, "--passes") == 0) {
            config.passes = parse_size(value, option, false);
        } else if (strcmp(option, "--warmup-passes") == 0) {
            config.warmup_passes = parse_size(value, option, true);
        } else if (strcmp(option, "--batch-size") == 0) {
            config.batch_size = parse_size(value, option, false);
        } else if (strcmp(option, "--early-cancel") == 0) {
            config.early_cancel = parse_size(value, option, false);
        } else {
            fprintf(stderr, "unknown argument: %s\n", option);
            exit(2);
        }
    }
    if (config.arm_name == NULL) fail("--arm is required");
    if (config.batch_size > SIZE_MAX / KEY_UNITS)
        fail("--batch-size is too large");
    return config;
}

static CorpusEntry* make_corpus(size_t size) {
    CorpusEntry* corpus = allocate(size, sizeof(*corpus));
    for (size_t index = 0; index < size; ++index) {
        const int written = snprintf(corpus[index].key,
                                     sizeof(corpus[index].key),
                                     "collection/%04" PRIxMAX "/%08" PRIxMAX
                                     "/shared-suffix",
                                     (uintmax_t)(index & 0x0fff),
                                     (uintmax_t)index);
        if (written != KEY_UNITS) fail("generated key length changed");
        corpus[index].value = (uint64_t)index;
    }
    return corpus;
}

static int compare_corpus(const void* left, const void* right) {
    return memcmp(((const CorpusEntry*)left)->key,
                  ((const CorpusEntry*)right)->key, KEY_UNITS);
}

static uint64_t expected_checksum(const CorpusEntry* corpus, size_t size,
                                  size_t limit) {
    CorpusEntry* ordered = allocate(size, sizeof(*ordered));
    memcpy(ordered, corpus, size * sizeof(*ordered));
    qsort(ordered, size, sizeof(*ordered), compare_corpus);
    if (limit > size) limit = size;
    uint64_t checksum = 0;
    for (size_t index = 0; index < limit; ++index)
        checksum += (uint64_t)KEY_UNITS ^ ordered[index].value;
    free(ordered);
    return checksum;
}

static LdictDictionary* build_dictionary(const CorpusEntry* corpus,
                                         size_t size) {
    LdictDictionary* dictionary = NULL;
    require_status(ldict_dynamic_dawg_new(VT_UNIT_DOMAIN_BYTE, &dictionary),
                   "ldict_dynamic_dawg_new");
    LdictTextEntry* entries = allocate(size, sizeof(*entries));
    for (size_t index = 0; index < size; ++index) {
        entries[index].data = (const uint8_t*)corpus[index].key;
        entries[index].len = KEY_UNITS;
        entries[index].value.value = corpus[index].value;
        entries[index].value.has_value = 1;
        memset(entries[index].value.reserved, 0,
               sizeof(entries[index].value.reserved));
    }
    size_t inserted = 0;
    require_status(ldict_dictionary_insert_text_batch(dictionary, entries,
                                                      size, &inserted),
                   "ldict_dictionary_insert_text_batch");
    free(entries);
    if (inserted != size) fail("generated corpus did not insert completely");
    return dictionary;
}

static LdictEntryBatchLimits limits_for(size_t batch_size) {
    const LdictEntryBatchLimits limits = {
        batch_size, batch_size * KEY_UNITS, batch_size, 0};
    return limits;
}

static uint64_t batch_entry_checksum(const LdictEntryBatch* batch,
                                     const LdictEntry* entry) {
    if (entry->value_len > 1) fail("invalid optional-u64 descriptor");
    const uint64_t value = entry->value_len == 0
                               ? 0
                               : batch->values[entry->value_offset];
    return (uint64_t)entry->unit_len ^ value;
}

static DrainResult drain_stream(LdictDictionary* dictionary,
                                const ProfileConfig* config, size_t limit,
                                bool cancel) {
    LdictEntryCursor* cursor = NULL;
    LdictEntriesInfo info = {0};
    require_status(ldict_dictionary_entries_open(dictionary, &cursor, &info),
                   "ldict_dictionary_entries_open");
    if (info.unit_domain != VT_UNIT_DOMAIN_BYTE)
        fail("benchmark expected a byte-domain cursor");
    const LdictEntryBatchLimits limits = limits_for(config->batch_size);
    DrainResult result = {0, 0};
    bool ended = false;
    while (result.count < limit) {
        LdictEntryBatch batch = {0};
        const LdictStatus status =
            ldict_entry_cursor_next(cursor, &limits, &batch);
        if (status == LDICT_STATUS_END) {
            ended = true;
            break;
        }
        require_status(status, "ldict_entry_cursor_next");
        const size_t remaining = limit - result.count;
        const size_t consume =
            batch.entry_count < remaining ? batch.entry_count : remaining;
        for (size_t index = 0; index < consume; ++index)
            result.checksum +=
                batch_entry_checksum(&batch, &batch.entries[index]);
        result.count += consume;
        require_status(ldict_entry_cursor_release(cursor, batch.generation),
                       "ldict_entry_cursor_release");
    }
    if (cancel) {
        require_status(ldict_entry_cursor_cancel(cursor),
                       "ldict_entry_cursor_cancel");
    } else if (!ended) {
        LdictEntryBatch batch = {0};
        const LdictStatus status =
            ldict_entry_cursor_next(cursor, &limits, &batch);
        if (status != LDICT_STATUS_END)
            fail("stream cardinality exceeds the generated corpus");
    }
    require_status(ldict_entry_cursor_free(cursor),
                   "ldict_entry_cursor_free");
    if (result.count != limit) fail("stream ended before the expected count");
    return result;
}

static DrainResult drain_materialized(LdictDictionary* dictionary,
                                      const ProfileConfig* config) {
    LdictEntryCursor* cursor = NULL;
    LdictEntriesInfo info = {0};
    require_status(ldict_dictionary_entries_open(dictionary, &cursor, &info),
                   "ldict_dictionary_entries_open");
    if (info.unit_domain != VT_UNIT_DOMAIN_BYTE ||
        (info.flags & LDICT_ENTRIES_INFO_FLAG_EXACT_LEN) == 0 ||
        info.exact_len != config->entries)
        fail("materialized snapshot metadata mismatch");
    OwnedEntry* owned = allocate(info.exact_len, sizeof(*owned));
    const LdictEntryBatchLimits limits = limits_for(config->batch_size);
    size_t count = 0;
    for (;;) {
        LdictEntryBatch batch = {0};
        const LdictStatus status =
            ldict_entry_cursor_next(cursor, &limits, &batch);
        if (status == LDICT_STATUS_END) break;
        require_status(status, "ldict_entry_cursor_next");
        for (size_t index = 0; index < batch.entry_count; ++index) {
            const LdictEntry* entry = &batch.entries[index];
            if (count >= info.exact_len) fail("snapshot exceeded exact length");
            owned[count].key_len = entry->unit_len;
            owned[count].key = allocate(entry->unit_len, sizeof(uint8_t));
            if (entry->unit_len != 0)
                memcpy(owned[count].key,
                       (const uint8_t*)batch.units + entry->unit_offset,
                       entry->unit_len);
            if (entry->value_len > 1)
                fail("invalid optional-u64 descriptor");
            owned[count].has_value = entry->value_len == 1;
            owned[count].value = owned[count].has_value
                                     ? batch.values[entry->value_offset]
                                     : 0;
            ++count;
        }
        require_status(ldict_entry_cursor_release(cursor, batch.generation),
                       "ldict_entry_cursor_release");
    }
    require_status(ldict_entry_cursor_free(cursor),
                   "ldict_entry_cursor_free");
    if (count != info.exact_len) fail("snapshot ended before exact length");
    DrainResult result = {0, count};
    for (size_t index = 0; index < count; ++index) {
        result.checksum += (uint64_t)owned[index].key_len ^
                           (owned[index].has_value ? owned[index].value : 0);
        free(owned[index].key);
    }
    free(owned);
    return result;
}

static LdictStatus reduce_batch(void* context,
                                const LdictEntryBatch* batch) {
    ReduceContext* state = (ReduceContext*)context;
    const size_t remaining = state->limit - state->count;
    const size_t consume =
        batch->entry_count < remaining ? batch->entry_count : remaining;
    for (size_t index = 0; index < consume; ++index)
        state->checksum +=
            batch_entry_checksum(batch, &batch->entries[index]);
    state->count += consume;
    return LDICT_STATUS_OK;
}

static DrainResult drain_reducer(LdictDictionary* dictionary,
                                 const ProfileConfig* config) {
    LdictEntryCursor* cursor = NULL;
    LdictEntriesInfo info = {0};
    require_status(ldict_dictionary_entries_open(dictionary, &cursor, &info),
                   "ldict_dictionary_entries_open");
    const LdictEntryBatchLimits limits = limits_for(config->batch_size);
    ReduceContext context = {0, 0, config->entries};
    size_t reduced = 0;
    require_status(ldict_entry_cursor_reduce(cursor, &limits, reduce_batch,
                                             &context, &reduced),
                   "ldict_entry_cursor_reduce");
    if (context.count != config->entries || reduced != config->entries)
        fail("reducer cardinality mismatch");
    require_status(ldict_entry_cursor_free(cursor),
                   "ldict_entry_cursor_free");
    const DrainResult result = {context.checksum, context.count};
    return result;
}

static DrainResult drain(LdictDictionary* dictionary,
                         const ProfileConfig* config) {
    switch (config->arm) {
        case ARM_MATERIALIZED:
            return drain_materialized(dictionary, config);
        case ARM_STREAM:
            return drain_stream(dictionary, config, config->entries, false);
        case ARM_STREAM_CANCEL:
            return drain_stream(dictionary, config,
                                config->early_cancel < config->entries
                                    ? config->early_cancel
                                    : config->entries,
                                true);
        case ARM_REDUCER:
            return drain_reducer(dictionary, config);
    }
    fail("unreachable arm");
    return (DrainResult){0, 0};
}

static uint64_t monotonic_nanoseconds(void) {
    struct timespec value = {0, 0};
    if (clock_gettime(CLOCK_MONOTONIC, &value) != 0)
        fail("clock_gettime failed");
    return (uint64_t)value.tv_sec * UINT64_C(1000000000) +
           (uint64_t)value.tv_nsec;
}

int main(int argc, char** argv) {
    const ProfileConfig config = parse_arguments(argc, argv);
    CorpusEntry* corpus = make_corpus(config.entries);
    LdictDictionary* dictionary = build_dictionary(corpus, config.entries);
    const size_t consumed = config.arm == ARM_STREAM_CANCEL
                                ? (config.early_cancel < config.entries
                                       ? config.early_cancel
                                       : config.entries)
                                : config.entries;
    const uint64_t expected =
        expected_checksum(corpus, config.entries, consumed);
    free(corpus);

    for (size_t pass = 0; pass < config.warmup_passes; ++pass) {
        const DrainResult result = drain(dictionary, &config);
        if (result.count != consumed || result.checksum != expected)
            fail("warmup checksum or cardinality mismatch");
    }

    const uint64_t started = monotonic_nanoseconds();
    uint64_t checksum = 0;
    for (size_t pass = 0; pass < config.passes; ++pass) {
        const DrainResult result = drain(dictionary, &config);
        if (result.count != consumed || result.checksum != expected)
            fail("timed checksum or cardinality mismatch");
        checksum += result.checksum;
    }
    const uint64_t measured_ns = monotonic_nanoseconds() - started;
    const uint64_t elapsed_ns = measured_ns == 0 ? 1 : measured_ns;
    if (checksum != expected * (uint64_t)config.passes)
        fail("aggregate checksum mismatch");

    printf("{\"schema\":\"libdictenstein.host-collection-traversal.v1\","
           "\"runtime\":\"c\",\"arm\":\"%s\","
           "\"dictionary_entries\":%zu,"
           "\"consumed_entries_per_pass\":%zu,\"passes\":%zu,"
           "\"warmup_passes\":%zu,\"batch_size\":%zu,"
           "\"early_cancel\":",
           config.arm_name, config.entries, consumed, config.passes,
           config.warmup_passes, config.batch_size);
    if (config.arm == ARM_STREAM_CANCEL)
        printf("%zu", config.early_cancel);
    else
        printf("null");
    printf(",\"elapsed_ns\":%" PRIu64 ",\"checksum\":%" PRIu64 "}\n",
           elapsed_ns, checksum);
    ldict_dictionary_free(dictionary);
    return 0;
}
