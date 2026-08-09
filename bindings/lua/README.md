# Vinary Tree libdictenstein for Lua

A C extension module over libdictenstein's stable `ldict_*` C ABI. The LuaRocks
package is `vinary-tree-libdictenstein`; the module loads as
`vinary_tree_libdictenstein`. It exposes DynamicDAWG CRUD, immutable
DoubleArrayTrie construction, SCDAWG substring search, persistent ARTrie
CRUD/checkpoint/reopen, and persistent vocabulary reverse lookup.

## Building

The rockspec compiles `bindings/lua/src/libdictenstein_lua.c` and links the
shared library `libdictenstein`. Build the native library first:

```sh
cargo build --release --no-default-features --features ffi
luarocks make bindings/lua/vinary-tree-libdictenstein-0.2.1-1.rockspec
export LD_LIBRARY_PATH="$PWD/target/release:$LD_LIBRARY_PATH"
```

## Quickstart

```lua
local ld = require("vinary_tree_libdictenstein")

local dictionary = ld.dynamic_dawg()          -- "unicode" domain by default
dictionary:put("cat", 1)
dictionary:put("cot", 2)
dictionary:put("cut")                          -- valueless term
assert(dictionary:len() == 3)

local hit = dictionary:get("cot")
assert(hit.found and hit.value == 2)

local suffixes = ld.scdawg()
suffixes:put("cat")
suffixes:put("cot")
assert(suffixes:contains_substring("ot"))
assert(suffixes:frequency("t") == 2)
dictionary:close()
suffixes:close()
```

## Constructors and methods

Module constructors: `dynamic_dawg([domain])`, `scdawg([domain])`,
`double_array_trie(entries[, domain])`,
`create_persistent_artrie(path[, domain])`, `open_persistent_artrie(path[, domain])`,
`create_persistent_vocabulary(path)`, `open_persistent_vocabulary(path)`.

Dictionary methods: `put`, `remove`, `get`, `contains`, `contains_u64`, `term`,
`clear`, `compact`, `checkpoint`, `contains_substring`, `frequency`, `len`,
`kind`, `capabilities`, `close`.

The `domain` argument is the string `"byte"`, `"unicode"`, or `"u64"` (default
`"unicode"`). `get` returns a table `{found = <bool>[, value = <int>]}`; the
`value` field is absent for a valueless term.

## Backends and capabilities

| Constructor | Kind | Unit domains | Capabilities |
|-------------|------|--------------|--------------|
| `dynamic_dawg` | 1 | byte, unicode, u64 | read, insert, remove, clear, compact |
| `double_array_trie` | 2 | byte, unicode | read (immutable) |
| `scdawg` | 3 | byte, unicode | read, insert, substring |
| `create/open_persistent_artrie` | 4 | byte, unicode, u64 | read, insert, remove, checkpoint |
| `create/open_persistent_vocabulary` | 5 | unicode | read, insert, checkpoint |

## Values and domains

The Unicode-scalar backends validate UTF-8 and reject invalid input. Mapped
values are non-negative Lua integers; because `lua_Integer` is a signed 64-bit
type, representable values run `0 .. 2^63 - 1` (u64 values above that are not
expressible from Lua). A `nil` value and a value of `0` are distinct.

## Error handling

Failing calls raise a Lua error whose message is the thread-local
`ldict_last_error_message()`. Backend-unsupported operations surface the
`UNSUPPORTED` status; wrong-domain terms surface `DOMAIN_MISMATCH` (9).

## Retained resource handoff

Each dictionary userdata carries the shared `VtResource`, so an independently
packaged liblevenshtein transducer retains it in constant time and keeps its
query-start revision valid after `close`.
