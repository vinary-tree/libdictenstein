#include <lua.h>
#include <lauxlib.h>

#include <stdint.h>
#include <inttypes.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "libdictenstein.h"
#include "vinary_tree_lua.h"
#include "dictionary_entries.h"

#define VT_LUA_ENTRY_CURSOR_METATABLE "vinary-tree.dictionary.entries.cursor.v1"
#define VT_LUA_ENTRIES_METATABLE "vinary-tree.dictionary.entries.snapshot.v1"

typedef struct LuaDictionary {
    VtLuaDictionaryResource resource;
    LdictDictionary* dictionary;
} LuaDictionary;

typedef struct LuaEntryCursor {
    VtBindingEntryCursor cursor;
    uint8_t active;
} LuaEntryCursor;

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

static uint64_t nonnegative_integer(lua_State* state, int index, const char* label);
static void push_unsigned(lua_State* state, uint64_t value);

static LdictOptionalU64 optional_value(lua_State* state, int index) {
    LdictOptionalU64 result = {0, 0, {0}};
    if (!lua_isnoneornil(state, index)) {
        result.value = nonnegative_integer(
            state, index, "value must be an unsigned integer or decimal string");
        result.has_value = 1;
    }
    return result;
}

static uint64_t nonnegative_integer(lua_State* state, int index, const char* label) {
    if (lua_isinteger(state, index)) {
        lua_Integer value = lua_tointeger(state, index);
        luaL_argcheck(state, value >= 0, index, label);
        return (uint64_t)value;
    }
    size_t length = 0;
    const char* decimal = luaL_checklstring(state, index, &length);
    luaL_argcheck(state, length != 0, index, label);
    for (size_t position = 0; position < length; ++position)
        luaL_argcheck(state, decimal[position] >= '0' && decimal[position] <= '9',
                      index, label);
    errno = 0;
    char* end = NULL;
    uint64_t value = strtoull(decimal, &end, 10);
    luaL_argcheck(state, errno != ERANGE && end == decimal + length, index, label);
    return value;
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
        push_unsigned(state, result.value);
        lua_setfield(state, -2, "value");
    }
    return 1;
}

static void push_unsigned(lua_State* state, uint64_t value) {
    if (value <= (uint64_t)LUA_MAXINTEGER) {
        lua_pushinteger(state, (lua_Integer)value);
        return;
    }
    char decimal[32];
    int length = snprintf(decimal, sizeof(decimal), "%" PRIu64, value);
    lua_pushlstring(state, decimal, (size_t)length);
}

static size_t utf8_length(const uint32_t* scalars, size_t count) {
    size_t length = 0;
    for (size_t index = 0; index < count; ++index) {
        uint32_t scalar = scalars[index];
        length += scalar <= 0x7f ? 1 : scalar <= 0x7ff ? 2 : scalar <= 0xffff ? 3 : 4;
    }
    return length;
}

static void utf8_copy(char* output, const uint32_t* scalars, size_t count) {
    size_t position = 0;
    for (size_t index = 0; index < count; ++index) {
        uint32_t scalar = scalars[index];
        if (scalar <= 0x7f) output[position++] = (char)scalar;
        else if (scalar <= 0x7ff) {
            output[position++] = (char)(0xc0 | (scalar >> 6));
            output[position++] = (char)(0x80 | (scalar & 0x3f));
        } else if (scalar <= 0xffff) {
            output[position++] = (char)(0xe0 | (scalar >> 12));
            output[position++] = (char)(0x80 | ((scalar >> 6) & 0x3f));
            output[position++] = (char)(0x80 | (scalar & 0x3f));
        } else {
            output[position++] = (char)(0xf0 | (scalar >> 18));
            output[position++] = (char)(0x80 | ((scalar >> 12) & 0x3f));
            output[position++] = (char)(0x80 | ((scalar >> 6) & 0x3f));
            output[position++] = (char)(0x80 | (scalar & 0x3f));
        }
    }
}

static void push_entry_key(
    lua_State* state, uint32_t domain, const VtBindingEntryView* entry) {
    if (domain == VT_UNIT_DOMAIN_BYTE) {
        lua_pushlstring(state,
            entry->units ? (const char*)entry->units : "", entry->unit_len);
    } else if (domain == VT_UNIT_DOMAIN_UNICODE_SCALAR) {
        size_t length = utf8_length((const uint32_t*)entry->units, entry->unit_len);
        luaL_Buffer buffer;
        char* output = luaL_buffinitsize(state, &buffer, length);
        utf8_copy(output, (const uint32_t*)entry->units, entry->unit_len);
        luaL_pushresultsize(&buffer, length);
    } else {
        const uint64_t* tokens = (const uint64_t*)entry->units;
        lua_createtable(state, (int)entry->unit_len, 0);
        for (size_t index = 0; index < entry->unit_len; ++index) {
            push_unsigned(state, tokens[index]);
            lua_rawseti(state, -2, (lua_Integer)index + 1);
        }
    }
}

static int entry_cursor_error(
    lua_State* state, LuaEntryCursor* cursor, VtStatus status) {
    const char* message = cursor && cursor->cursor.error
        ? cursor->cursor.error : ldict_last_error_message();
    return luaL_error(state, "%s (status %d)",
        message && *message ? message : "dictionary entries failed", (int)status);
}

static LuaEntryCursor* entry_cursor(lua_State* state, int index) {
    return (LuaEntryCursor*)luaL_checkudata(
        state, index, VT_LUA_ENTRY_CURSOR_METATABLE);
}

static size_t entry_limit_field(
    lua_State* state, int index, const char* field, size_t fallback) {
    lua_getfield(state, index, field);
    lua_Integer value = luaL_optinteger(state, -1, (lua_Integer)fallback);
    lua_pop(state, 1);
    luaL_argcheck(state, value > 0, index, "entry batch limits must be positive");
    return (size_t)value;
}

static LuaEntryCursor* push_entry_cursor(
    lua_State* state, LuaDictionary* source, int limits_index) {
    size_t max_entries = 256;
    size_t max_units = 65536;
    size_t max_values = 256;
    if (!lua_isnoneornil(state, limits_index)) {
        luaL_checktype(state, limits_index, LUA_TTABLE);
        max_entries = entry_limit_field(
            state, limits_index, "max_entries", max_entries);
        max_units = entry_limit_field(state, limits_index, "max_units", max_units);
        max_values = entry_limit_field(
            state, limits_index, "max_values", max_values);
    }
    LuaEntryCursor* cursor =
        (LuaEntryCursor*)lua_newuserdatauv(state, sizeof(*cursor), 0);
    memset(cursor, 0, sizeof(*cursor));
    VtStatus status = vt_binding_entries_open(
        &source->resource.resource, max_entries, max_units, max_values,
        &cursor->cursor);
    if (status != VT_STATUS_OK) entry_cursor_error(state, cursor, status);
    cursor->active = 1;
    luaL_setmetatable(state, VT_LUA_ENTRY_CURSOR_METATABLE);
    return cursor;
}

static int close_entry_cursor(lua_State* state) {
    LuaEntryCursor* cursor = entry_cursor(state, 1);
    if (cursor->active) {
        VtStatus status = vt_binding_entries_close(&cursor->cursor);
        cursor->active = 0;
        if (status != VT_STATUS_OK) return entry_cursor_error(state, cursor, status);
    }
    return 0;
}

static int next_entry_cursor(lua_State* state) {
    LuaEntryCursor* cursor = entry_cursor(state, 1);
    if (!cursor->active) return 0;
    VtBindingEntryView entry = {0};
    uint8_t present = 0;
    VtStatus status = vt_binding_entries_next(&cursor->cursor, &entry, &present);
    if (status == VT_STATUS_END) {
        cursor->active = 0;
        return 0;
    }
    if (status != VT_STATUS_OK) return entry_cursor_error(state, cursor, status);
    if (!present) return 0;
    push_entry_key(state, cursor->cursor.info.unit_domain, &entry);
    if (entry.has_value) push_unsigned(state, entry.value);
    else lua_pushnil(state);
    lua_pushboolean(state, entry.has_value);
    return 3;
}

static int entry_cursor_metadata(lua_State* state) {
    LuaEntryCursor* cursor = entry_cursor(state, 1);
    VtDictionaryEntriesInfo* info = &cursor->cursor.info;
    lua_createtable(state, 0, 5);
    lua_pushstring(state, info->unit_domain == VT_UNIT_DOMAIN_BYTE ? "byte"
        : info->unit_domain == VT_UNIT_DOMAIN_UNICODE_SCALAR ? "unicode" : "u64");
    lua_setfield(state, -2, "unit_domain");
    lua_pushstring(state, info->value_domain == VT_VALUE_DOMAIN_UNIT
        ? "unit" : "optional_u64");
    lua_setfield(state, -2, "value_domain");
    if (info->flags & VT_DICTIONARY_ENTRIES_INFO_FLAG_EXACT_LEN) {
        push_unsigned(state, info->exact_len);
        lua_setfield(state, -2, "exact_length");
    }
    if (info->flags & VT_DICTIONARY_ENTRIES_INFO_FLAG_SNAPSHOT_IDENTITY) {
        lua_createtable(state, 0, 2);
        push_unsigned(state, info->identity.producer);
        lua_setfield(state, -2, "producer");
        push_unsigned(state, info->identity.revision);
        lua_setfield(state, -2, "revision");
        lua_setfield(state, -2, "snapshot_identity");
    }
    return 1;
}

static int open_entry_cursor(lua_State* state) {
    push_entry_cursor(state, dictionary(state, 1), 2);
    return 1;
}

static int stream_entries(lua_State* state) {
    push_entry_cursor(state, dictionary(state, 1), 2);
    lua_pushcfunction(state, next_entry_cursor);
    lua_insert(state, -2);
    lua_pushvalue(state, -1);
    lua_pushnil(state);
    lua_insert(state, -2);
    return 4;
}

static int snapshot_next(lua_State* state) {
    lua_Integer index = lua_tointeger(state, lua_upvalueindex(2)) + 1;
    lua_pushinteger(state, index);
    lua_replace(state, lua_upvalueindex(2));
    lua_rawgeti(state, lua_upvalueindex(1), index);
    if (lua_isnil(state, -1)) return 0;
    lua_rawgeti(state, -1, 1);
    lua_rawgeti(state, -2, 2);
    lua_rawgeti(state, -3, 3);
    return 3;
}

static int snapshot_pairs(lua_State* state) {
    lua_pushvalue(state, 1);
    lua_pushinteger(state, 0);
    lua_pushcclosure(state, snapshot_next, 2);
    lua_pushnil(state);
    lua_pushnil(state);
    return 3;
}

static int materialize_entries(lua_State* state) {
    LuaDictionary* source = dictionary(state, 1);
    LuaEntryCursor* cursor = push_entry_cursor(state, source, 2);
    int cursor_index = lua_gettop(state);
    lua_toclose(state, cursor_index);
    lua_createtable(state, 0, 1);
    int snapshot_index = lua_gettop(state);
    lua_pushcfunction(state, entry_cursor_metadata);
    lua_pushvalue(state, cursor_index);
    lua_call(state, 1, 1);
    lua_setfield(state, snapshot_index, "metadata");
    lua_Integer count = 0;
    for (;;) {
        VtBindingEntryView entry = {0};
        uint8_t present = 0;
        VtStatus status = vt_binding_entries_next(&cursor->cursor, &entry, &present);
        if (status == VT_STATUS_END) {
            cursor->active = 0;
            break;
        }
        if (status != VT_STATUS_OK) return entry_cursor_error(state, cursor, status);
        if (!present) break;
        lua_createtable(state, 3, 0);
        push_entry_key(state, cursor->cursor.info.unit_domain, &entry);
        lua_rawseti(state, -2, 1);
        if (entry.has_value) push_unsigned(state, entry.value);
        else lua_pushnil(state);
        lua_rawseti(state, -2, 2);
        lua_pushboolean(state, entry.has_value);
        lua_rawseti(state, -2, 3);
        lua_rawseti(state, snapshot_index, ++count);
    }
    luaL_setmetatable(state, VT_LUA_ENTRIES_METATABLE);
    lua_closeslot(state, cursor_index);
    lua_remove(state, cursor_index);
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

static uint32_t algebra_operation(lua_State* state, int index) {
    const char* value = luaL_checkstring(state, index);
    if (strcmp(value, "union") == 0) return LDICT_ALGEBRA_UNION;
    if (strcmp(value, "intersection") == 0) return LDICT_ALGEBRA_INTERSECTION;
    if (strcmp(value, "difference") == 0) return LDICT_ALGEBRA_DIFFERENCE;
    if (strcmp(value, "symmetric_difference") == 0
        || strcmp(value, "symmetric-difference") == 0)
        return LDICT_ALGEBRA_SYMMETRIC_DIFFERENCE;
    luaL_argerror(state, index,
        "operation must be union, intersection, difference, or symmetric_difference");
    return 0;
}

static uint32_t value_merge(lua_State* state, int index, uint32_t fallback) {
    if (lua_isnoneornil(state, index)) return fallback;
    const char* value = luaL_checkstring(state, index);
    if (strcmp(value, "first") == 0) return LDICT_VALUE_MERGE_FIRST;
    if (strcmp(value, "last") == 0) return LDICT_VALUE_MERGE_LAST;
    if (strcmp(value, "lattice_join") == 0 || strcmp(value, "lattice-join") == 0)
        return LDICT_VALUE_MERGE_LATTICE_JOIN;
    if (strcmp(value, "lattice_meet") == 0 || strcmp(value, "lattice-meet") == 0)
        return LDICT_VALUE_MERGE_LATTICE_MEET;
    luaL_argerror(state, index,
        "value merge must be first, last, lattice_join, or lattice_meet");
    return 0;
}

static int perform_algebra(
    lua_State* state, uint32_t operation, uint32_t merge) {
    LuaDictionary* left = dictionary(state, 1);
    LuaDictionary* right = dictionary(state, 2);
    LdictDictionary* output = NULL;
    LdictStatus status = ldict_dictionary_algebra(
        left->dictionary, right->dictionary, operation, merge, &output);
    return status == LDICT_STATUS_OK
        ? push_dictionary(state, output, left->resource.unit_domain)
        : dictionary_error(state, status);
}

static int dictionary_algebra(lua_State* state) {
    return perform_algebra(state, algebra_operation(state, 3),
        value_merge(state, 4, LDICT_VALUE_MERGE_LAST));
}

static int dictionary_union(lua_State* state) {
    return perform_algebra(state, LDICT_ALGEBRA_UNION,
        value_merge(state, 3, LDICT_VALUE_MERGE_LAST));
}

static int dictionary_intersection(lua_State* state) {
    return perform_algebra(state, LDICT_ALGEBRA_INTERSECTION,
        value_merge(state, 3, LDICT_VALUE_MERGE_LATTICE_MEET));
}

static int dictionary_difference(lua_State* state) {
    return perform_algebra(
        state, LDICT_ALGEBRA_DIFFERENCE, LDICT_VALUE_MERGE_FIRST);
}

static int dictionary_symmetric_difference(lua_State* state) {
    return perform_algebra(state, LDICT_ALGEBRA_SYMMETRIC_DIFFERENCE,
        LDICT_VALUE_MERGE_FIRST);
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

static int abi_version(lua_State* state) {
    lua_pushinteger(state, (lua_Integer)ldict_abi_version());
    return 1;
}
static int api_revision(lua_State* state) {
    lua_pushinteger(state, (lua_Integer)ldict_api_revision());
    return 1;
}

static const luaL_Reg dictionary_methods[] = {
    {"close", close_dictionary}, {"len", length}, {"kind", kind}, {"capabilities", capabilities},
    {"put", put}, {"remove", remove_term}, {"get", get}, {"contains", contains},
    {"put_u64", put_u64}, {"remove_u64", remove_u64}, {"get_u64", get_u64},
    {"contains_u64", contains_u64}, {"term", vocabulary_term},
    {"entries", materialize_entries}, {"entry_cursor", open_entry_cursor},
    {"entries_iter", stream_entries},
    {"algebra", dictionary_algebra}, {"union", dictionary_union},
    {"intersection", dictionary_intersection},
    {"difference", dictionary_difference},
    {"symmetric_difference", dictionary_symmetric_difference},
    {"clear", clear}, {"compact", compact}, {"checkpoint", checkpoint},
    {"contains_substring", contains_substring}, {"frequency", frequency}, {NULL, NULL}
};

int luaopen_vinary_tree_libdictenstein(lua_State* state) {
    luaL_newmetatable(state, VT_LUA_ENTRY_CURSOR_METATABLE);
    lua_pushcfunction(state, close_entry_cursor); lua_setfield(state, -2, "__gc");
    lua_pushcfunction(state, close_entry_cursor); lua_setfield(state, -2, "__close");
    lua_pushcfunction(state, next_entry_cursor); lua_setfield(state, -2, "__call");
    lua_newtable(state);
    lua_pushcfunction(state, close_entry_cursor); lua_setfield(state, -2, "close");
    lua_pushcfunction(state, next_entry_cursor); lua_setfield(state, -2, "next");
    lua_pushcfunction(state, entry_cursor_metadata); lua_setfield(state, -2, "metadata");
    lua_setfield(state, -2, "__index");
    lua_pop(state, 1);
    luaL_newmetatable(state, VT_LUA_ENTRIES_METATABLE);
    lua_pushcfunction(state, snapshot_pairs); lua_setfield(state, -2, "__pairs");
    lua_pop(state, 1);
    luaL_newmetatable(state, VT_LUA_DICTIONARY_METATABLE);
    lua_pushcfunction(state, close_dictionary); lua_setfield(state, -2, "__gc");
    lua_pushcfunction(state, close_dictionary); lua_setfield(state, -2, "__close");
    lua_pushcfunction(state, dictionary_union); lua_setfield(state, -2, "__bor");
    lua_pushcfunction(state, dictionary_intersection); lua_setfield(state, -2, "__band");
    lua_pushcfunction(state, dictionary_difference); lua_setfield(state, -2, "__sub");
    lua_pushcfunction(state, dictionary_symmetric_difference); lua_setfield(state, -2, "__bxor");
    lua_newtable(state); luaL_setfuncs(state, dictionary_methods, 0); lua_setfield(state, -2, "__index");
    lua_pop(state, 1);
    luaL_Reg functions[] = {
        {"abi_version", abi_version}, {"api_revision", api_revision},
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
