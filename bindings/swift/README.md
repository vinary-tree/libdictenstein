# Vinary Tree libdictenstein for Swift

A Swift Package Manager facade over libdictenstein's stable `ldict_*` C ABI,
importing the C header through the `CLibdictenstein` system-library target. The
development package lives here; the root [`Package.swift`](../../Package.swift)
declares the distributable `VinaryTreeLibdictenstein` product.

## Native library

The facade links the shared library `libdictenstein`. Build it and expose it to
the linker and loader:

```sh
cargo build --release --no-default-features --features ffi
export LIBRARY_PATH="$PWD/target/release:$LIBRARY_PATH"
export LD_LIBRARY_PATH="$PWD/target/release:$LD_LIBRARY_PATH"
```

## Quickstart

```swift
import Libdictenstein

let dictionary = try DynamicDAWG()               // .unicodeScalar by default
try dictionary.put("cat", value: 1)
try dictionary.put("cot", value: 2)
try dictionary.put("cut")                         // valueless term
let hit = try dictionary.get("cot")
assert(hit.found && hit.value == 2)

let suffixes = try SCDAWG()
try suffixes.put("cat")
try suffixes.put("cot")
assert(try suffixes.containsSubstring("ot"))
assert(try suffixes.substringFrequency("t") == 2)
```

Each dictionary is a class; `deinit` frees the native handle, and `close()` is
an idempotent early release.

## Backends

| Class | Unit domains | Notes |
|-------|--------------|-------|
| `DynamicDAWG` | `.byte`, `.unicodeScalar`, `.u64` | full CRUD (`put`/`remove`/`clear`/`compact`) |
| `DoubleArrayTrie` | `.byte`, `.unicodeScalar` | immutable, built from one entry batch |
| `SCDAWG` | `.byte`, `.unicodeScalar` | `put`, `containsSubstring`, `substringFrequency` |
| `PersistentARTrie` | `.byte`, `.unicodeScalar`, `.u64` | `create`/`open`, CRUD, `checkpoint` |

Text and u64 terms are both accepted (`String` and `[UInt64]`); `put`, `get`,
and `remove` are overloaded on the term type. `count` is a throwing property.

## Values and domains

`String` terms are passed as UTF-8; the Unicode-scalar backends validate it.
Values are `UInt64?` over the full `0 ... UInt64.max` range; `nil` and `0` are
distinct. `Lookup` carries `found: Bool` and `value: UInt64?`.

## Error handling

Fallible calls `throw` `LibdictensteinError`, whose `description` is the
thread-local `ldict_last_error_message()`. Backend-unsupported operations
surface the `UNSUPPORTED` status; wrong-domain terms surface `DOMAIN_MISMATCH`
(9).

## Retained resource handoff

`withVtResource { pointer in ... }` borrows the shared `VtResource` for one
synchronous retaining call, so a liblevenshtein transducer retains the
dictionary in constant time and keeps its query-start revision valid after
`close`.

## Coverage note

This facade currently binds the constructor, CRUD, maintenance, and substring
surface. The `kind`/`capabilities` accessors, the contiguous batch inserts, and
the persistent vocabulary backend are not yet wrapped; use another facade (or
the C ABI directly) when those are required.
