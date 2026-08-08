#include <lua.h>
#include <lauxlib.h>

#include <stdint.h>
#include <string.h>

#include "libdictenstein.h"
#include "vinary_tree_lua.h"

typedef struct LuaDictionary {
    VtLuaDictionaryResource resource;
    LdictDictionary* dictionary;
} LuaDictionary;

static int dictionary_error(lua_State* state, LdictStatus status) {
    const char* message = ldict_last_error_message();
    return luaL_error(state, "%s (status %d)", message && *message ? message : "libdictenstein error", (int)status);
}

static LuaDictionary* dictionary(lua_State* state, int index) {
    LuaDictionary* value = (LuaDictionary*)luaL_checkudata(state, index, VT_LUA_DICTIONARY_METATABLE);
    luaL_argcheck(state, value->resource.magic == VT_LUA_DICTIONARY_MAGIC && !value->resource.closed,
                  index, "dictionary is closed");
    return value;
}

static uint32_t domain(lua_State* state, int index) {
    const char* value = luaL_optstring(state, index, "unicode");
    if (strcmp(value, "byte") == 0) return VT_UNIT_DOMAIN_BYTE;
    if (strcmp(value, "unicode") == 0) return VT_UNIT_DOMAIN_UNICODE_SCALAR;
    if (strcmp(value, "u64") == 0) return VT_UNIT_DOMAIN_U64;
    luaL_argerror(state, index, "domain must be byte, unicode, or u64");
    return 0;
}

static LdictOptionalU64 optional_value(lua_State* state, int index) {
    LdictOptionalU64 result = {0, 0, {0}};
    if (!lua_isnoneornil(state, index)) {
        lua_Integer value = luaL_checkinteger(state, index);
        luaL_argcheck(state, value >= 0, index, "value must be a non-negative integer");
        result.value = (uint64_t)value;
        result.has_value = 1;
    }
    return result;
}

static uint64_t nonnegative_integer(lua_State* state, int index, const char* label) {
    lua_Integer value = luaL_checkinteger(state, index);
    luaL_argcheck(state, value >= 0, index, label);
    return (uint64_t)value;
}

static const uint64_t* u64_sequence(lua_State* state, int index, size_t* out_length) {
    luaL_checktype(state, index, LUA_TTABLE);
    lua_Integer length = luaL_len(state, index);
    luaL_argcheck(state, length >= 0, index, "invalid token sequence length");
    uint64_t* data = (uint64_t*)lua_newuserdatauv(
        state, (size_t)length * sizeof(uint64_t), 0);
    for (lua_Integer position = 1; position <= length; ++position) {
        lua_rawgeti(state, index, position);
        data[position - 1] = nonnegative_integer(state, -1, "tokens must be non-negative integers");
        lua_pop(state, 1);
    }
    *out_length = (size_t)length;
    return data;
}

static int push_lookup(lua_State* state, uint8_t found, LdictOptionalU64 result) {
    lua_createtable(state, 0, 2);
    lua_pushboolean(state, found); lua_setfield(state, -2, "found");
    if (result.has_value) {
        lua_pushinteger(state, (lua_Integer)result.value);
        lua_setfield(state, -2, "value");
    }
    return 1;
}

static int push_dictionary(lua_State* state, LdictDictionary* raw, uint32_t selected_domain) {
    LuaDictionary* value = (LuaDictionary*)lua_newuserdatauv(state, sizeof(*value), 0);
    memset(value, 0, sizeof(*value));
    value->resource.magic = VT_LUA_DICTIONARY_MAGIC;
    value->resource.unit_domain = selected_domain;
    value->dictionary = raw;
    LdictStatus status = ldict_dictionary_resource(raw, &value->resource.resource);
    if (status != LDICT_STATUS_OK) {
        ldict_dictionary_free(raw);
        return dictionary_error(state, status);
    }
    luaL_setmetatable(state, VT_LUA_DICTIONARY_METATABLE);
    return 1;
}

static int dynamic_new(lua_State* state) {
    uint32_t selected = domain(state, 1);
    LdictDictionary* output = NULL;
    LdictStatus status = ldict_dynamic_dawg_new(selected, &output);
    return status == LDICT_STATUS_OK ? push_dictionary(state, output, selected) : dictionary_error(state, status);
}

static int scdawg_new(lua_State* state) {
    uint32_t selected = domain(state, 1);
    LdictDictionary* output = NULL;
    LdictStatus status = ldict_scdawg_new(selected, &output);
    return status == LDICT_STATUS_OK ? push_dictionary(state, output, selected) : dictionary_error(state, status);
}

static int double_array_trie_new(lua_State* state) {
    luaL_checktype(state, 1, LUA_TTABLE);
    uint32_t selected = domain(state, 2);
    luaL_argcheck(state, selected != VT_UNIT_DOMAIN_U64, 2,
                  "double-array trie entries use byte or unicode strings");
    lua_Integer count_integer = luaL_len(state, 1);
    luaL_argcheck(state, count_integer >= 0, 1, "invalid entry count");
    size_t count = (size_t)count_integer;
    LdictTextEntry* entries = (LdictTextEntry*)lua_newuserdatauv(
        state, count * sizeof(LdictTextEntry), 0);
    memset(entries, 0, count * sizeof(LdictTextEntry));
    for (lua_Integer index = 1; index <= count_integer; ++index) {
        lua_rawgeti(state, 1, index);
        luaL_checktype(state, -1, LUA_TTABLE);
        lua_rawgeti(state, -1, 1);
        entries[index - 1].data = (const uint8_t*)luaL_checklstring(
            state, -1, &entries[index - 1].len);
        lua_pop(state, 1);
        lua_rawgeti(state, -1, 2);
        entries[index - 1].value = optional_value(state, -1);
        lua_pop(state, 2);
    }
    LdictDictionary* output = NULL;
    LdictStatus status = ldict_double_array_trie_new(selected, entries, count, &output);
    return status == LDICT_STATUS_OK ? push_dictionary(state, output, selected)
                                     : dictionary_error(state, status);
}

static int persistent(lua_State* state, int create) {
    size_t path_length = 0;
    const char* path = luaL_checklstring(state, 1, &path_length);
    uint32_t selected = domain(state, 2);
    LdictDictionary* output = NULL;
    LdictStatus status = create
        ? ldict_persistent_artrie_create(selected, (const uint8_t*)path, path_length, &output)
        : ldict_persistent_artrie_open(selected, (const uint8_t*)path, path_length, &output);
    return status == LDICT_STATUS_OK ? push_dictionary(state, output, selected) : dictionary_error(state, status);
}
static int persistent_create(lua_State* state) { return persistent(state, 1); }
static int persistent_open(lua_State* state) { return persistent(state, 0); }

static int persistent_vocabulary(lua_State* state, int create) {
    size_t path_length = 0;
    const char* path = luaL_checklstring(state, 1, &path_length);
    LdictDictionary* output = NULL;
    LdictStatus status = create
        ? ldict_persistent_vocab_create((const uint8_t*)path, path_length, &output)
        : ldict_persistent_vocab_open((const uint8_t*)path, path_length, &output);
    return status == LDICT_STATUS_OK
        ? push_dictionary(state, output, VT_UNIT_DOMAIN_UNICODE_SCALAR)
        : dictionary_error(state, status);
}
static int persistent_vocabulary_create(lua_State* state) {
    return persistent_vocabulary(state, 1);
}
static int persistent_vocabulary_open(lua_State* state) {
    return persistent_vocabulary(state, 0);
}

static int close_dictionary(lua_State* state) {
    LuaDictionary* value = (LuaDictionary*)luaL_checkudata(state, 1, VT_LUA_DICTIONARY_METATABLE);
    if (!value->resource.closed) {
        ldict_dictionary_free(value->dictionary);
        value->dictionary = NULL;
        value->resource.closed = 1;
    }
    return 0;
}

static int length(lua_State* state) {
    size_t output = 0;
    LdictStatus status = ldict_dictionary_len(dictionary(state, 1)->dictionary, &output);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    lua_pushinteger(state, (lua_Integer)output);
    return 1;
}

static int kind(lua_State* state) {
    uint32_t output = 0;
    LdictStatus status = ldict_dictionary_kind(dictionary(state, 1)->dictionary, &output);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    lua_pushinteger(state, (lua_Integer)output);
    return 1;
}

static int capabilities(lua_State* state) {
    uint64_t output = 0;
    LdictStatus status = ldict_dictionary_capabilities(dictionary(state, 1)->dictionary, &output);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    lua_pushinteger(state, (lua_Integer)output);
    return 1;
}

static int put(lua_State* state) {
    LuaDictionary* value = dictionary(state, 1);
    size_t length = 0;
    const char* term = luaL_checklstring(state, 2, &length);
    uint8_t inserted = 0;
    LdictStatus status = ldict_dictionary_insert_text(
        value->dictionary, (const uint8_t*)term, length, optional_value(state, 3), &inserted);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    lua_pushboolean(state, inserted);
    return 1;
}

static int remove_term(lua_State* state) {
    LuaDictionary* value = dictionary(state, 1);
    size_t length = 0;
    const char* term = luaL_checklstring(state, 2, &length);
    uint8_t removed = 0;
    LdictStatus status = ldict_dictionary_remove_text(
        value->dictionary, (const uint8_t*)term, length, &removed);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    lua_pushboolean(state, removed);
    return 1;
}

static int get(lua_State* state) {
    LuaDictionary* value = dictionary(state, 1);
    size_t length = 0;
    const char* term = luaL_checklstring(state, 2, &length);
    uint8_t found = 0;
    LdictOptionalU64 result = {0, 0, {0}};
    LdictStatus status = ldict_dictionary_get_text(
        value->dictionary, (const uint8_t*)term, length, &found, &result);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    return push_lookup(state, found, result);
}

static int contains(lua_State* state) {
    LuaDictionary* value = dictionary(state, 1);
    size_t length = 0;
    const char* term = luaL_checklstring(state, 2, &length);
    uint8_t output = 0;
    LdictStatus status = ldict_dictionary_contains_text(
        value->dictionary, (const uint8_t*)term, length, &output);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    lua_pushboolean(state, output);
    return 1;
}

static int put_u64(lua_State* state) {
    LuaDictionary* value = dictionary(state, 1);
    LdictOptionalU64 selected_value = optional_value(state, 3);
    size_t length = 0;
    const uint64_t* tokens = u64_sequence(state, 2, &length);
    uint8_t inserted = 0;
    LdictStatus status = ldict_dictionary_insert_u64(
        value->dictionary, tokens, length, selected_value, &inserted);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    lua_pushboolean(state, inserted);
    return 1;
}

static int remove_u64(lua_State* state) {
    LuaDictionary* value = dictionary(state, 1);
    size_t length = 0;
    const uint64_t* tokens = u64_sequence(state, 2, &length);
    uint8_t removed = 0;
    LdictStatus status = ldict_dictionary_remove_u64(
        value->dictionary, tokens, length, &removed);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    lua_pushboolean(state, removed);
    return 1;
}

static int get_u64(lua_State* state) {
    LuaDictionary* value = dictionary(state, 1);
    size_t length = 0;
    const uint64_t* tokens = u64_sequence(state, 2, &length);
    uint8_t found = 0;
    LdictOptionalU64 result = {0, 0, {0}};
    LdictStatus status = ldict_dictionary_get_u64(
        value->dictionary, tokens, length, &found, &result);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    return push_lookup(state, found, result);
}

static int contains_u64(lua_State* state) {
    LuaDictionary* value = dictionary(state, 1);
    size_t length = 0;
    const uint64_t* tokens = u64_sequence(state, 2, &length);
    uint8_t output = 0;
    LdictStatus status = ldict_dictionary_contains_u64(
        value->dictionary, tokens, length, &output);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    lua_pushboolean(state, output);
    return 1;
}

static int vocabulary_term(lua_State* state) {
    LuaDictionary* value = dictionary(state, 1);
    uint64_t index = nonnegative_integer(state, 2, "index must be non-negative");
    size_t length = 0;
    uint8_t found = 0;
    LdictStatus status = ldict_vocab_get_term(
        value->dictionary, index, NULL, 0, &length, &found);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    if (!found) { lua_pushnil(state); return 1; }
    uint8_t* output = (uint8_t*)lua_newuserdatauv(state, length == 0 ? 1 : length, 0);
    status = ldict_vocab_get_term(
        value->dictionary, index, output, length, &length, &found);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    if (!found) { lua_pushnil(state); return 1; }
    lua_pushlstring(state, (const char*)output, length);
    return 1;
}

static int clear(lua_State* state) {
    LdictStatus status = ldict_dictionary_clear(dictionary(state, 1)->dictionary);
    return status == LDICT_STATUS_OK ? 0 : dictionary_error(state, status);
}
static int compact(lua_State* state) {
    size_t reclaimed = 0;
    LdictStatus status = ldict_dictionary_compact(dictionary(state, 1)->dictionary, &reclaimed);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    lua_pushinteger(state, (lua_Integer)reclaimed);
    return 1;
}
static int checkpoint(lua_State* state) {
    LdictStatus status = ldict_dictionary_checkpoint(dictionary(state, 1)->dictionary);
    return status == LDICT_STATUS_OK ? 0 : dictionary_error(state, status);
}
static int contains_substring(lua_State* state) {
    LuaDictionary* value = dictionary(state, 1);
    size_t length = 0;
    const char* term = luaL_checklstring(state, 2, &length);
    uint8_t output = 0;
    LdictStatus status = ldict_scdawg_contains_substring(
        value->dictionary, (const uint8_t*)term, length, &output);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    lua_pushboolean(state, output);
    return 1;
}
static int frequency(lua_State* state) {
    LuaDictionary* value = dictionary(state, 1);
    size_t length = 0;
    const char* term = luaL_checklstring(state, 2, &length);
    size_t output = 0;
    LdictStatus status = ldict_scdawg_substring_frequency(
        value->dictionary, (const uint8_t*)term, length, &output);
    if (status != LDICT_STATUS_OK) return dictionary_error(state, status);
    lua_pushinteger(state, (lua_Integer)output);
    return 1;
}

static const luaL_Reg dictionary_methods[] = {
    {"close", close_dictionary}, {"len", length}, {"kind", kind}, {"capabilities", capabilities},
    {"put", put}, {"remove", remove_term}, {"get", get}, {"contains", contains},
    {"put_u64", put_u64}, {"remove_u64", remove_u64}, {"get_u64", get_u64},
    {"contains_u64", contains_u64}, {"term", vocabulary_term},
    {"clear", clear}, {"compact", compact}, {"checkpoint", checkpoint},
    {"contains_substring", contains_substring}, {"frequency", frequency}, {NULL, NULL}
};

int luaopen_vinary_tree_libdictenstein(lua_State* state) {
    luaL_newmetatable(state, VT_LUA_DICTIONARY_METATABLE);
    lua_pushcfunction(state, close_dictionary); lua_setfield(state, -2, "__gc");
    lua_pushcfunction(state, close_dictionary); lua_setfield(state, -2, "__close");
    lua_newtable(state); luaL_setfuncs(state, dictionary_methods, 0); lua_setfield(state, -2, "__index");
    lua_pop(state, 1);
    luaL_Reg functions[] = {
        {"dynamic_dawg", dynamic_new}, {"double_array_trie", double_array_trie_new},
        {"scdawg", scdawg_new},
        {"create_persistent_artrie", persistent_create}, {"open_persistent_artrie", persistent_open},
        {"create_persistent_vocabulary", persistent_vocabulary_create},
        {"open_persistent_vocabulary", persistent_vocabulary_open},
        {NULL, NULL}
    };
    luaL_newlib(state, functions);
    return 1;
}
