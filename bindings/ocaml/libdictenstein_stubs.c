#include <caml/alloc.h>
#include <caml/custom.h>
#include <caml/fail.h>
#include <caml/memory.h>
#include <caml/mlvalues.h>
#include <stdint.h>
#include <stdlib.h>
#include "libdictenstein.h"
#include "vinary_tree_ocaml.h"
#include "dictionary_entries.h"

typedef struct { LdictDictionary* value; } OcamlDictionary;

static void dictionary_finalize(value block) {
    OcamlDictionary* dictionary = (OcamlDictionary*)Data_custom_val(block);
    if (dictionary->value) {
        ldict_dictionary_free(dictionary->value);
        dictionary->value = NULL;
    }
}

static struct custom_operations dictionary_operations = {
    "io.vinarytree.libdictenstein.dictionary.v1", dictionary_finalize,
    custom_compare_default, custom_hash_default, custom_serialize_default,
    custom_deserialize_default, custom_compare_ext_default,
    custom_fixed_length_default
};

static void check_status(LdictStatus status) {
    if (status == LDICT_STATUS_OK) return;
    const char* message = ldict_last_error_message();
    caml_failwith(message && *message ? message : "libdictenstein error");
}

static LdictDictionary* dictionary_val(value block) {
    OcamlDictionary* dictionary = (OcamlDictionary*)Data_custom_val(block);
    if (!dictionary->value) caml_invalid_argument("dictionary is closed");
    return dictionary->value;
}

static value copy_dictionary(LdictDictionary* raw) {
    value block = caml_alloc_custom(&dictionary_operations,
                                    sizeof(OcamlDictionary), 0, 1);
    ((OcamlDictionary*)Data_custom_val(block))->value = raw;
    return block;
}

static uint32_t domain_val(value domain) { return (uint32_t)Int_val(domain) + 1u; }

static LdictOptionalU64 optional_u64(value option) {
    LdictOptionalU64 result = {0, 0, {0}};
    if (Is_block(option)) {
        result.value = (uint64_t)Int64_val(Field(option, 0));
        result.has_value = 1;
    }
    return result;
}

static value copy_optional_u64(LdictOptionalU64 input) {
    CAMLparam0();
    CAMLlocal2(result, payload);
    if (!input.has_value) CAMLreturn(Val_int(0));
    payload = caml_copy_int64((int64_t)input.value);
    result = caml_alloc(1, 0);
    Store_field(result, 0, payload);
    CAMLreturn(result);
}

static uint64_t* copy_u64_array(value input, size_t* out_length) {
    size_t length = Wosize_val(input);
    uint64_t* output = length ? malloc(length * sizeof(uint64_t)) : NULL;
    if (length && !output) caml_raise_out_of_memory();
    for (size_t index = 0; index < length; ++index)
        output[index] = (uint64_t)Int64_val(Field(input, index));
    *out_length = length;
    return output;
}

CAMLprim value ocaml_ldict_abi_version(value unit) {
    CAMLparam1(unit);
    CAMLreturn(Val_int((int)ldict_abi_version()));
}

CAMLprim value ocaml_ldict_api_revision(value unit) {
    CAMLparam1(unit);
    CAMLreturn(Val_int((int)ldict_api_revision()));
}

CAMLprim value ocaml_ldict_dynamic_dawg(value domain) {
    CAMLparam1(domain);
    LdictDictionary* output = NULL;
    check_status(ldict_dynamic_dawg_new(domain_val(domain), &output));
    CAMLreturn(copy_dictionary(output));
}

CAMLprim value ocaml_ldict_scdawg(value domain) {
    CAMLparam1(domain);
    LdictDictionary* output = NULL;
    check_status(ldict_scdawg_new(domain_val(domain), &output));
    CAMLreturn(copy_dictionary(output));
}

CAMLprim value ocaml_ldict_double_array_trie(value domain, value input) {
    CAMLparam2(domain, input);
    size_t count = Wosize_val(input);
    LdictTextEntry* entries = count ? calloc(count, sizeof(*entries)) : NULL;
    if (count && !entries) caml_raise_out_of_memory();
    for (size_t index = 0; index < count; ++index) {
        value pair = Field(input, index);
        value text = Field(pair, 0);
        entries[index].data = (const uint8_t*)String_val(text);
        entries[index].len = caml_string_length(text);
        entries[index].value = optional_u64(Field(pair, 1));
    }
    LdictDictionary* output = NULL;
    LdictStatus status = ldict_double_array_trie_new(
        domain_val(domain), entries, count, &output);
    free(entries);
    check_status(status);
    CAMLreturn(copy_dictionary(output));
}

static value persistent_artrie(value domain, value path, int create) {
    CAMLparam2(domain, path);
    LdictDictionary* output = NULL;
    LdictStatus status = create
        ? ldict_persistent_artrie_create(domain_val(domain),
            (const uint8_t*)String_val(path), caml_string_length(path), &output)
        : ldict_persistent_artrie_open(domain_val(domain),
            (const uint8_t*)String_val(path), caml_string_length(path), &output);
    check_status(status);
    CAMLreturn(copy_dictionary(output));
}
CAMLprim value ocaml_ldict_create_persistent_artrie(value domain, value path) {
    return persistent_artrie(domain, path, 1);
}
CAMLprim value ocaml_ldict_open_persistent_artrie(value domain, value path) {
    return persistent_artrie(domain, path, 0);
}

static value persistent_vocabulary(value path, int create) {
    CAMLparam1(path);
    LdictDictionary* output = NULL;
    LdictStatus status = create
        ? ldict_persistent_vocab_create((const uint8_t*)String_val(path),
            caml_string_length(path), &output)
        : ldict_persistent_vocab_open((const uint8_t*)String_val(path),
            caml_string_length(path), &output);
    check_status(status);
    CAMLreturn(copy_dictionary(output));
}
CAMLprim value ocaml_ldict_create_persistent_vocabulary(value path) {
    return persistent_vocabulary(path, 1);
}
CAMLprim value ocaml_ldict_open_persistent_vocabulary(value path) {
    return persistent_vocabulary(path, 0);
}

CAMLprim value ocaml_ldict_close(value block) {
    CAMLparam1(block); dictionary_finalize(block); CAMLreturn(Val_unit);
}

CAMLprim value ocaml_ldict_resource(value block) {
    CAMLparam1(block);
    VtResource resource = {0};
    check_status(ldict_dictionary_resource(dictionary_val(block), &resource));
    CAMLreturn(vt_ocaml_copy_resource(&resource));
}

CAMLprim value ocaml_ldict_length(value block) {
    size_t output = 0;
    check_status(ldict_dictionary_len(dictionary_val(block), &output));
    return Val_long(output);
}
CAMLprim value ocaml_ldict_kind(value block) {
    uint32_t output = 0;
    check_status(ldict_dictionary_kind(dictionary_val(block), &output));
    return Val_int(output);
}
CAMLprim value ocaml_ldict_capabilities(value block) {
    CAMLparam1(block);
    uint64_t output = 0;
    check_status(ldict_dictionary_capabilities(dictionary_val(block), &output));
    CAMLreturn(caml_copy_int64((int64_t)output));
}

CAMLprim value ocaml_ldict_put(value block, value text, value mapped) {
    uint8_t inserted = 0;
    check_status(ldict_dictionary_insert_text(dictionary_val(block),
        (const uint8_t*)String_val(text), caml_string_length(text),
        optional_u64(mapped), &inserted));
    return Val_bool(inserted);
}

CAMLprim value ocaml_ldict_put_many(value block, value input) {
    size_t count = Wosize_val(input);
    LdictTextEntry* entries = count ? calloc(count, sizeof(*entries)) : NULL;
    if (count && !entries) caml_raise_out_of_memory();
    for (size_t index = 0; index < count; ++index) {
        value pair = Field(input, index);
        value text = Field(pair, 0);
        entries[index].data = (const uint8_t*)String_val(text);
        entries[index].len = caml_string_length(text);
        entries[index].value = optional_u64(Field(pair, 1));
    }
    size_t inserted = 0;
    LdictStatus status = ldict_dictionary_insert_text_batch(
        dictionary_val(block), entries, count, &inserted);
    free(entries);
    check_status(status);
    return Val_long(inserted);
}

CAMLprim value ocaml_ldict_remove(value block, value text) {
    uint8_t output = 0;
    check_status(ldict_dictionary_remove_text(dictionary_val(block),
        (const uint8_t*)String_val(text), caml_string_length(text), &output));
    return Val_bool(output);
}
CAMLprim value ocaml_ldict_contains(value block, value text) {
    uint8_t output = 0;
    check_status(ldict_dictionary_contains_text(dictionary_val(block),
        (const uint8_t*)String_val(text), caml_string_length(text), &output));
    return Val_bool(output);
}

static value copy_lookup(uint8_t found, LdictOptionalU64 mapped) {
    CAMLparam0(); CAMLlocal3(result, option, found_value);
    option = copy_optional_u64(mapped);
    found_value = Val_bool(found);
    result = caml_alloc_tuple(2);
    Store_field(result, 0, found_value);
    Store_field(result, 1, option);
    CAMLreturn(result);
}

CAMLprim value ocaml_ldict_get(value block, value text) {
    CAMLparam2(block, text);
    uint8_t found = 0; LdictOptionalU64 mapped = {0, 0, {0}};
    check_status(ldict_dictionary_get_text(dictionary_val(block),
        (const uint8_t*)String_val(text), caml_string_length(text), &found, &mapped));
    CAMLreturn(copy_lookup(found, mapped));
}

static value u64_mutation(value block, value input, value mapped, int remove) {
    size_t length = 0; uint64_t* tokens = copy_u64_array(input, &length);
    uint8_t output = 0;
    LdictStatus status = remove
        ? ldict_dictionary_remove_u64(dictionary_val(block), tokens, length, &output)
        : ldict_dictionary_insert_u64(dictionary_val(block), tokens, length,
            optional_u64(mapped), &output);
    free(tokens); check_status(status); return Val_bool(output);
}
CAMLprim value ocaml_ldict_put_u64(value block, value input, value mapped) {
    return u64_mutation(block, input, mapped, 0);
}
CAMLprim value ocaml_ldict_remove_u64(value block, value input) {
    return u64_mutation(block, input, Val_int(0), 1);
}
CAMLprim value ocaml_ldict_contains_u64(value block, value input) {
    size_t length = 0; uint64_t* tokens = copy_u64_array(input, &length);
    uint8_t output = 0;
    LdictStatus status = ldict_dictionary_contains_u64(
        dictionary_val(block), tokens, length, &output);
    free(tokens); check_status(status); return Val_bool(output);
}
CAMLprim value ocaml_ldict_get_u64(value block, value input) {
    CAMLparam2(block, input);
    size_t length = 0; uint64_t* tokens = copy_u64_array(input, &length);
    uint8_t found = 0; LdictOptionalU64 mapped = {0, 0, {0}};
    LdictStatus status = ldict_dictionary_get_u64(
        dictionary_val(block), tokens, length, &found, &mapped);
    free(tokens); check_status(status);
    CAMLreturn(copy_lookup(found, mapped));
}

CAMLprim value ocaml_ldict_clear(value block) {
    check_status(ldict_dictionary_clear(dictionary_val(block))); return Val_unit;
}
CAMLprim value ocaml_ldict_compact(value block) {
    size_t output = 0;
    check_status(ldict_dictionary_compact(dictionary_val(block), &output));
    return Val_long(output);
}
CAMLprim value ocaml_ldict_algebra(
    value left, value right, value operation, value value_merge) {
    CAMLparam4(left, right, operation, value_merge);
    LdictDictionary* output = NULL;
    check_status(ldict_dictionary_algebra(
        dictionary_val(left), dictionary_val(right),
        (uint32_t)Int_val(operation), (uint32_t)Int_val(value_merge), &output));
    CAMLreturn(copy_dictionary(output));
}
CAMLprim value ocaml_ldict_checkpoint(value block) {
    check_status(ldict_dictionary_checkpoint(dictionary_val(block))); return Val_unit;
}
CAMLprim value ocaml_ldict_contains_substring(value block, value text) {
    uint8_t output = 0;
    check_status(ldict_scdawg_contains_substring(dictionary_val(block),
        (const uint8_t*)String_val(text), caml_string_length(text), &output));
    return Val_bool(output);
}
CAMLprim value ocaml_ldict_substring_frequency(value block, value text) {
    size_t output = 0;
    check_status(ldict_scdawg_substring_frequency(dictionary_val(block),
        (const uint8_t*)String_val(text), caml_string_length(text), &output));
    return Val_long(output);
}

CAMLprim value ocaml_ldict_term(value block, value index) {
    CAMLparam2(block, index); CAMLlocal2(result, text);
    size_t length = 0; uint8_t found = 0;
    check_status(ldict_vocab_get_term(dictionary_val(block),
        (uint64_t)Int64_val(index), NULL, 0, &length, &found));
    if (!found) CAMLreturn(Val_int(0));
    text = caml_alloc_string(length);
    check_status(ldict_vocab_get_term(dictionary_val(block),
        (uint64_t)Int64_val(index), (uint8_t*)Bytes_val(text), length, &length, &found));
    if (!found) CAMLreturn(Val_int(0));
    result = caml_alloc(1, 0); Store_field(result, 0, text); CAMLreturn(result);
}

typedef struct {
    VtBindingEntryCursor cursor;
    int active;
} OcamlEntryCursor;

static void entry_cursor_finalize(value block) {
    OcamlEntryCursor* cursor = (OcamlEntryCursor*)Data_custom_val(block);
    if (cursor->active) {
        (void)vt_binding_entries_close(&cursor->cursor);
        cursor->active = 0;
    }
}

static struct custom_operations entry_cursor_operations = {
    "io.vinarytree.libdictenstein.entries.v1", entry_cursor_finalize,
    custom_compare_default, custom_hash_default, custom_serialize_default,
    custom_deserialize_default, custom_compare_ext_default,
    custom_fixed_length_default
};

static OcamlEntryCursor* entry_cursor_val(value block) {
    OcamlEntryCursor* cursor = (OcamlEntryCursor*)Data_custom_val(block);
    if (!cursor->active) caml_invalid_argument("dictionary entry cursor is closed");
    return cursor;
}

static void check_entry_status(VtBindingEntryCursor* cursor, VtStatus status) {
    if (status == VT_STATUS_OK) return;
    const char* message = cursor && cursor->error ? cursor->error : NULL;
    if (!message || !*message) message = ldict_last_error_message();
    caml_failwith(message && *message ? message : "dictionary entries failed");
}

static size_t unicode_utf8_length(const uint32_t* scalars, size_t count) {
    size_t length = 0;
    for (size_t index = 0; index < count; ++index) {
        uint32_t scalar = scalars[index];
        length += scalar <= 0x7f ? 1 : scalar <= 0x7ff ? 2 : scalar <= 0xffff ? 3 : 4;
    }
    return length;
}

static void unicode_utf8_copy(
    uint8_t* output, const uint32_t* scalars, size_t count) {
    size_t position = 0;
    for (size_t index = 0; index < count; ++index) {
        uint32_t scalar = scalars[index];
        if (scalar <= 0x7f) output[position++] = (uint8_t)scalar;
        else if (scalar <= 0x7ff) {
            output[position++] = (uint8_t)(0xc0 | (scalar >> 6));
            output[position++] = (uint8_t)(0x80 | (scalar & 0x3f));
        } else if (scalar <= 0xffff) {
            output[position++] = (uint8_t)(0xe0 | (scalar >> 12));
            output[position++] = (uint8_t)(0x80 | ((scalar >> 6) & 0x3f));
            output[position++] = (uint8_t)(0x80 | (scalar & 0x3f));
        } else {
            output[position++] = (uint8_t)(0xf0 | (scalar >> 18));
            output[position++] = (uint8_t)(0x80 | ((scalar >> 12) & 0x3f));
            output[position++] = (uint8_t)(0x80 | ((scalar >> 6) & 0x3f));
            output[position++] = (uint8_t)(0x80 | (scalar & 0x3f));
        }
    }
}

static value copy_entry_key(uint32_t domain, const VtBindingEntryView* view) {
    CAMLparam0();
    CAMLlocal3(key, payload, boxed);
    if (domain == VT_UNIT_DOMAIN_BYTE) {
        payload = caml_alloc_string(view->unit_len);
        if (view->unit_len) memcpy(Bytes_val(payload), view->units, view->unit_len);
        key = caml_alloc(1, 0);
        Store_field(key, 0, payload);
    } else if (domain == VT_UNIT_DOMAIN_UNICODE_SCALAR) {
        size_t length = unicode_utf8_length(
            (const uint32_t*)view->units, view->unit_len);
        payload = caml_alloc_string(length);
        unicode_utf8_copy((uint8_t*)Bytes_val(payload),
                          (const uint32_t*)view->units, view->unit_len);
        key = caml_alloc(1, 1);
        Store_field(key, 0, payload);
    } else {
        payload = view->unit_len ? caml_alloc(view->unit_len, 0) : Atom(0);
        const uint64_t* values = (const uint64_t*)view->units;
        for (size_t index = 0; index < view->unit_len; ++index) {
            boxed = caml_copy_int64((int64_t)values[index]);
            Store_field(payload, index, boxed);
        }
        key = caml_alloc(1, 2);
        Store_field(key, 0, payload);
    }
    CAMLreturn(key);
}

CAMLprim value ocaml_ldict_entries_open(
    value block, value max_entries, value max_units, value max_values) {
    CAMLparam4(block, max_entries, max_units, max_values);
    CAMLlocal1(result);
    VtResource resource = {0};
    check_status(ldict_dictionary_resource(dictionary_val(block), &resource));
    result = caml_alloc_custom(&entry_cursor_operations,
                               sizeof(OcamlEntryCursor), 0, 1);
    OcamlEntryCursor* cursor = (OcamlEntryCursor*)Data_custom_val(result);
    memset(cursor, 0, sizeof(*cursor));
    VtStatus status = vt_binding_entries_open(
        &resource, Long_val(max_entries), Long_val(max_units),
        Long_val(max_values), &cursor->cursor);
    if (status != VT_STATUS_OK) check_entry_status(&cursor->cursor, status);
    cursor->active = 1;
    CAMLreturn(result);
}

CAMLprim value ocaml_ldict_entries_metadata(value block) {
    CAMLparam1(block);
    CAMLlocal5(result, exact, exact_box, identity, pair);
    CAMLlocal1(producer);
    OcamlEntryCursor* wrapper = entry_cursor_val(block);
    VtDictionaryEntriesInfo* info = &wrapper->cursor.info;
    exact = Val_int(0);
    if (info->flags & VT_DICTIONARY_ENTRIES_INFO_FLAG_EXACT_LEN) {
        exact_box = caml_copy_int64((int64_t)info->exact_len);
        exact = caml_alloc(1, 0);
        Store_field(exact, 0, exact_box);
    }
    identity = Val_int(0);
    if (info->flags & VT_DICTIONARY_ENTRIES_INFO_FLAG_SNAPSHOT_IDENTITY) {
        pair = caml_alloc_tuple(2);
        producer = caml_copy_int64((int64_t)info->identity.producer);
        Store_field(pair, 0, producer);
        producer = caml_copy_int64((int64_t)info->identity.revision);
        Store_field(pair, 1, producer);
        identity = caml_alloc(1, 0);
        Store_field(identity, 0, pair);
    }
    result = caml_alloc(4, 0);
    Store_field(result, 0, Val_int((int)info->unit_domain - 1));
    Store_field(result, 1, Val_int((int)info->value_domain));
    Store_field(result, 2, exact);
    Store_field(result, 3, identity);
    CAMLreturn(result);
}

CAMLprim value ocaml_ldict_entries_next(value block) {
    CAMLparam1(block);
    CAMLlocal5(result, entry, key, mapped, boxed);
    OcamlEntryCursor* wrapper = (OcamlEntryCursor*)Data_custom_val(block);
    if (!wrapper->active) {
        if (wrapper->cursor.ended) CAMLreturn(Val_int(0));
        caml_invalid_argument("dictionary entry cursor is closed");
    }
    VtBindingEntryView view = {0};
    uint8_t has_entry = 0;
    VtStatus status = vt_binding_entries_next(
        &wrapper->cursor, &view, &has_entry);
    if (status == VT_STATUS_END) {
        wrapper->active = 0;
        CAMLreturn(Val_int(0));
    }
    check_entry_status(&wrapper->cursor, status);
    if (!has_entry) CAMLreturn(Val_int(0));
    key = copy_entry_key(wrapper->cursor.info.unit_domain, &view);
    mapped = Val_int(0);
    if (view.has_value) {
        boxed = caml_copy_int64((int64_t)view.value);
        mapped = caml_alloc(1, 0);
        Store_field(mapped, 0, boxed);
    }
    entry = caml_alloc(2, 0);
    Store_field(entry, 0, key);
    Store_field(entry, 1, mapped);
    result = caml_alloc(1, 0);
    Store_field(result, 0, entry);
    CAMLreturn(result);
}

CAMLprim value ocaml_ldict_entries_close(value block) {
    CAMLparam1(block);
    OcamlEntryCursor* cursor = (OcamlEntryCursor*)Data_custom_val(block);
    if (cursor->active) {
        VtStatus status = vt_binding_entries_close(&cursor->cursor);
        if (status != VT_STATUS_OK) check_entry_status(&cursor->cursor, status);
        cursor->active = 0;
    }
    CAMLreturn(Val_unit);
}
