/* Stable project-owned C API for libdictenstein dictionaries. */
#ifndef LIBDICTENSTEIN_H
#define LIBDICTENSTEIN_H

#include <stddef.h>
#include <stdint.h>
#ifndef VT_INTEROP_HEADER
#define VT_INTEROP_HEADER "vinary_tree_interop.h"
#endif
#include VT_INTEROP_HEADER

#if defined(_WIN32) || defined(__CYGWIN__)
#  if defined(LIBDICTENSTEIN_BUILDING_DLL)
#    define LDICT_API __declspec(dllexport)
#  elif defined(LIBDICTENSTEIN_USING_DLL)
#    define LDICT_API __declspec(dllimport)
#  else
#    define LDICT_API
#  endif
#elif defined(__GNUC__) || defined(__clang__)
#  define LDICT_API __attribute__((visibility("default")))
#else
#  define LDICT_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

#define LDICT_ABI_VERSION 1u
#define LDICT_API_REVISION 5u

#define LDICT_KIND_DYNAMIC_DAWG 1u
#define LDICT_KIND_DOUBLE_ARRAY_TRIE 2u
#define LDICT_KIND_SCDAWG 3u
#define LDICT_KIND_PERSISTENT_ARTRIE 4u
#define LDICT_KIND_PERSISTENT_VOCAB_ARTRIE 5u

#define LDICT_CAP_READ (UINT64_C(1) << 0)
#define LDICT_CAP_INSERT (UINT64_C(1) << 1)
#define LDICT_CAP_REMOVE (UINT64_C(1) << 2)
#define LDICT_CAP_CLEAR (UINT64_C(1) << 3)
#define LDICT_CAP_COMPACT (UINT64_C(1) << 4)
#define LDICT_CAP_SUBSTRING (UINT64_C(1) << 5)
#define LDICT_CAP_CHECKPOINT (UINT64_C(1) << 6)

typedef enum LdictStatus {
    LDICT_STATUS_OK = 0,
    LDICT_STATUS_END = 1,
    LDICT_STATUS_INVALID_ARGUMENT = 2,
    LDICT_STATUS_INVALID_UTF8 = 3,
    LDICT_STATUS_NULL_POINTER = 4,
    LDICT_STATUS_PANIC = 5,
    LDICT_STATUS_UNSUPPORTED = 6,
    LDICT_STATUS_IO_ERROR = 7,
    LDICT_STATUS_CLOSED = 8,
    LDICT_STATUS_DOMAIN_MISMATCH = 9,
    LDICT_STATUS_LIMIT_EXCEEDED = 10,
    LDICT_STATUS_PROVIDER_ERROR = 11,
    LDICT_STATUS_BATCH_IN_USE = 12
} LdictStatus;

typedef struct LdictOptionalU64 {
    uint64_t value;
    uint8_t has_value;
    uint8_t reserved[7];
} LdictOptionalU64;

typedef struct LdictTextEntry {
    const uint8_t* data;
    size_t len;
    LdictOptionalU64 value;
} LdictTextEntry;

typedef struct LdictU64Entry {
    const uint64_t* data;
    size_t len;
    LdictOptionalU64 value;
} LdictU64Entry;

/* Natural project aliases for the vt.dict.entry.v1 descriptor arenas. Keys are
 * always present; value_len == 0 means a valueless member and value_len == 1
 * means values[value_offset] is present, including the value zero. */
typedef VtDictionaryEntry LdictEntry;
typedef VtDictionaryEntryBatchLimits LdictEntryBatchLimits;
typedef VtDictionaryEntryBatchView LdictEntryBatch;
typedef VtDictionaryEntriesInfo LdictEntriesInfo;

#define LDICT_ENTRY_ORDER_LEXICOGRAPHIC VT_DICTIONARY_ENTRY_ORDER_LEXICOGRAPHIC
#define LDICT_ENTRIES_INFO_FLAG_EXACT_LEN VT_DICTIONARY_ENTRIES_INFO_FLAG_EXACT_LEN
#define LDICT_ENTRIES_INFO_FLAG_SNAPSHOT_IDENTITY \
    VT_DICTIONARY_ENTRIES_INFO_FLAG_SNAPSHOT_IDENTITY

typedef struct LdictEntryCursor LdictEntryCursor;
typedef LdictStatus (*LdictEntryReducer)(
    void* reducer_context,
    const LdictEntryBatch* batch);

typedef struct LdictDictionary LdictDictionary;

LDICT_API uint32_t ldict_abi_version(void);
LDICT_API uint32_t ldict_api_revision(void);
LDICT_API const char* ldict_last_error_message(void);

LDICT_API LdictStatus ldict_dynamic_dawg_new(
    uint32_t unit_domain,
    LdictDictionary** out_dictionary);
LDICT_API LdictStatus ldict_double_array_trie_new(
    uint32_t unit_domain,
    const LdictTextEntry* entries,
    size_t entry_count,
    LdictDictionary** out_dictionary);
LDICT_API LdictStatus ldict_scdawg_new(
    uint32_t unit_domain,
    LdictDictionary** out_dictionary);
LDICT_API LdictStatus ldict_persistent_artrie_create(
    uint32_t unit_domain,
    const uint8_t* path_data,
    size_t path_len,
    LdictDictionary** out_dictionary);
LDICT_API LdictStatus ldict_persistent_artrie_open(
    uint32_t unit_domain,
    const uint8_t* path_data,
    size_t path_len,
    LdictDictionary** out_dictionary);
LDICT_API LdictStatus ldict_persistent_vocab_create(
    const uint8_t* path_data,
    size_t path_len,
    LdictDictionary** out_dictionary);
LDICT_API LdictStatus ldict_persistent_vocab_open(
    const uint8_t* path_data,
    size_t path_len,
    LdictDictionary** out_dictionary);
LDICT_API void ldict_dictionary_free(LdictDictionary* dictionary);
LDICT_API LdictStatus ldict_dictionary_kind(
    const LdictDictionary* dictionary,
    uint32_t* out_kind);
LDICT_API LdictStatus ldict_dictionary_capabilities(
    const LdictDictionary* dictionary,
    uint64_t* out_capabilities);

/* Borrowed resource; retaining consumers may outlive the dictionary handle. */
LDICT_API LdictStatus ldict_dictionary_resource(
    const LdictDictionary* dictionary,
    VtResource* out_resource);

/* Open one immutable lexicographic revision. The opaque cursor owns that
 * snapshot and may outlive the source dictionary. */
LDICT_API LdictStatus ldict_dictionary_entries_open(
    const LdictDictionary* dictionary,
    LdictEntryCursor** out_cursor,
    LdictEntriesInfo* out_info);
/* On OK, the returned arenas are borrowed until the exact generation is
 * released. A second next/reduce while leased returns BATCH_IN_USE. */
LDICT_API LdictStatus ldict_entry_cursor_next(
    LdictEntryCursor* cursor,
    const LdictEntryBatchLimits* limits,
    LdictEntryBatch* out_batch);
LDICT_API LdictStatus ldict_entry_cursor_release(
    LdictEntryCursor* cursor,
    uint64_t generation);
/* The callback returns OK to continue, END to stop successfully, or another
 * LdictStatus to abort. out_count counts entries in completed callbacks. */
LDICT_API LdictStatus ldict_entry_cursor_reduce(
    LdictEntryCursor* cursor,
    const LdictEntryBatchLimits* limits,
    LdictEntryReducer reducer,
    void* reducer_context,
    size_t* out_count);
LDICT_API LdictStatus ldict_entry_cursor_cancel(LdictEntryCursor* cursor);
/* Null is a no-op. A live lease returns BATCH_IN_USE without consuming the
 * cursor; release the lease and retry. */
LDICT_API LdictStatus ldict_entry_cursor_free(LdictEntryCursor* cursor);

LDICT_API LdictStatus ldict_dictionary_len(
    const LdictDictionary* dictionary,
    size_t* out_len);
LDICT_API LdictStatus ldict_dictionary_clear(LdictDictionary* dictionary);
LDICT_API LdictStatus ldict_dictionary_compact(
    LdictDictionary* dictionary,
    size_t* out_reclaimed);
LDICT_API LdictStatus ldict_dictionary_checkpoint(LdictDictionary* dictionary);
LDICT_API LdictStatus ldict_vocab_get_term(
    const LdictDictionary* dictionary,
    uint64_t index,
    uint8_t* out_data,
    size_t capacity,
    size_t* out_len,
    uint8_t* out_found);

LDICT_API LdictStatus ldict_dictionary_insert_text(
    LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    LdictOptionalU64 value,
    uint8_t* out_inserted);
LDICT_API LdictStatus ldict_dictionary_insert_text_value(
    LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    uint64_t value,
    uint8_t has_value,
    uint8_t* out_inserted);
LDICT_API LdictStatus ldict_dictionary_remove_text(
    LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    uint8_t* out_removed);
LDICT_API LdictStatus ldict_dictionary_contains_text(
    const LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    uint8_t* out_contains);
LDICT_API LdictStatus ldict_dictionary_get_text(
    const LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    uint8_t* out_found,
    LdictOptionalU64* out_value);
LDICT_API LdictStatus ldict_dictionary_get_text_value(
    const LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    uint8_t* out_found,
    uint64_t* out_value,
    uint8_t* out_has_value);

LDICT_API LdictStatus ldict_dictionary_insert_u64(
    LdictDictionary* dictionary,
    const uint64_t* data,
    size_t len,
    LdictOptionalU64 value,
    uint8_t* out_inserted);
LDICT_API LdictStatus ldict_dictionary_insert_u64_value(
    LdictDictionary* dictionary,
    const uint64_t* data,
    size_t len,
    uint64_t value,
    uint8_t has_value,
    uint8_t* out_inserted);
LDICT_API LdictStatus ldict_dictionary_remove_u64(
    LdictDictionary* dictionary,
    const uint64_t* data,
    size_t len,
    uint8_t* out_removed);
LDICT_API LdictStatus ldict_dictionary_contains_u64(
    const LdictDictionary* dictionary,
    const uint64_t* data,
    size_t len,
    uint8_t* out_contains);
LDICT_API LdictStatus ldict_dictionary_get_u64(
    const LdictDictionary* dictionary,
    const uint64_t* data,
    size_t len,
    uint8_t* out_found,
    LdictOptionalU64* out_value);
LDICT_API LdictStatus ldict_dictionary_get_u64_value(
    const LdictDictionary* dictionary,
    const uint64_t* data,
    size_t len,
    uint8_t* out_found,
    uint64_t* out_value,
    uint8_t* out_has_value);

LDICT_API LdictStatus ldict_dictionary_insert_text_batch(
    LdictDictionary* dictionary,
    const LdictTextEntry* entries,
    size_t entry_count,
    size_t* out_inserted);
LDICT_API LdictStatus ldict_dictionary_insert_u64_batch(
    LdictDictionary* dictionary,
    const LdictU64Entry* entries,
    size_t entry_count,
    size_t* out_inserted);

LDICT_API LdictStatus ldict_scdawg_contains_substring(
    const LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    uint8_t* out_contains);
LDICT_API LdictStatus ldict_scdawg_substring_frequency(
    const LdictDictionary* dictionary,
    const uint8_t* data,
    size_t len,
    size_t* out_frequency);

#ifdef __cplusplus
}
#endif

#endif /* LIBDICTENSTEIN_H */
