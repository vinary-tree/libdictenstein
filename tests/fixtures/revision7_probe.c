/* Deliberately exports the frozen revision-7 symbol NAME set, not revision-8
 * symbols. Only the version handshake is callable; the remaining inert
 * entries exist to exercise eager Python/Ruby import-time symbol resolution.
 * Do not use this probe for old-API behavior: revision7_consumer.c exercises
 * that against the real revision-8 library. The names below were checked
 * against b6387ba:bindings/api.json, the last revision-7 model. */
#include <stdint.h>

uint32_t ldict_abi_version(void) { return 1; }
uint32_t ldict_api_revision(void) { return 7; }
const char* ldict_last_error_message(void) { return "revision-7 loader probe"; }

#define PROBE_ONLY(name) uint32_t name(void) { return 6; }
PROBE_ONLY(ldict_dynamic_dawg_new_byte_values)
PROBE_ONLY(ldict_dictionary_insert_text_bytes)
PROBE_ONLY(ldict_dictionary_insert_u64_bytes)
PROBE_ONLY(ldict_dictionary_get_text_bytes)
PROBE_ONLY(ldict_dictionary_get_u64_bytes)
PROBE_ONLY(ldict_dictionary_byte_entries_open)
PROBE_ONLY(ldict_byte_entry_cursor_next)
PROBE_ONLY(ldict_byte_entry_cursor_release)
PROBE_ONLY(ldict_byte_entry_cursor_reduce)
PROBE_ONLY(ldict_byte_entry_cursor_cancel)
PROBE_ONLY(ldict_byte_entry_cursor_free)
PROBE_ONLY(ldict_dynamic_dawg_new)
PROBE_ONLY(ldict_double_array_trie_new)
PROBE_ONLY(ldict_scdawg_new)
PROBE_ONLY(ldict_persistent_artrie_create)
PROBE_ONLY(ldict_persistent_artrie_open)
PROBE_ONLY(ldict_persistent_vocab_create)
PROBE_ONLY(ldict_persistent_vocab_open)
PROBE_ONLY(ldict_dictionary_kind)
PROBE_ONLY(ldict_dictionary_capabilities)
PROBE_ONLY(ldict_dictionary_free)
PROBE_ONLY(ldict_dictionary_resource)
PROBE_ONLY(ldict_dictionary_algebra)
PROBE_ONLY(ldict_dictionary_entries_open)
PROBE_ONLY(ldict_entry_cursor_next)
PROBE_ONLY(ldict_entry_cursor_release)
PROBE_ONLY(ldict_entry_cursor_reduce)
PROBE_ONLY(ldict_entry_cursor_cancel)
PROBE_ONLY(ldict_entry_cursor_free)
PROBE_ONLY(ldict_dictionary_len)
PROBE_ONLY(ldict_dictionary_checkpoint)
PROBE_ONLY(ldict_vocab_get_term)
PROBE_ONLY(ldict_dictionary_clear)
PROBE_ONLY(ldict_dictionary_compact)
PROBE_ONLY(ldict_dictionary_insert_text)
PROBE_ONLY(ldict_dictionary_insert_text_value)
PROBE_ONLY(ldict_dictionary_remove_text)
PROBE_ONLY(ldict_dictionary_contains_text)
PROBE_ONLY(ldict_dictionary_get_text)
PROBE_ONLY(ldict_dictionary_get_text_value)
PROBE_ONLY(ldict_dictionary_insert_u64)
PROBE_ONLY(ldict_dictionary_insert_u64_value)
PROBE_ONLY(ldict_dictionary_remove_u64)
PROBE_ONLY(ldict_dictionary_contains_u64)
PROBE_ONLY(ldict_dictionary_get_u64)
PROBE_ONLY(ldict_dictionary_get_u64_value)
PROBE_ONLY(ldict_scdawg_contains_substring)
PROBE_ONLY(ldict_scdawg_substring_frequency)
PROBE_ONLY(ldict_dictionary_insert_text_batch)
PROBE_ONLY(ldict_dictionary_insert_u64_batch)
