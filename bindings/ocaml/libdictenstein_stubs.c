#include <caml/alloc.h>
#include <caml/custom.h>
#include <caml/fail.h>
#include <caml/memory.h>
#include <caml/mlvalues.h>
#include <stdint.h>
#include <stdlib.h>
#include "libdictenstein.h"
#include "vinary_tree_ocaml.h"

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
