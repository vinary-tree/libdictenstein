# Vinary Tree libdictenstein for C++

A header-only, RAII facade over libdictenstein's stable `ldict_*` C ABI. It is
distributed with the native package through the CMake config in
[`cmake/`](../../cmake) and the pkg-config file in
[`pkgconfig/`](../../pkgconfig); there is no separate registry coordinate.

The single header [`include/libdictenstein.hpp`](../../include/libdictenstein.hpp)
lives beside the C header [`include/libdictenstein.h`](../../include/libdictenstein.h)
and pulls in the shared interop header `vinary_tree_interop.h`. Everything is in
namespace `vinary_tree::libdictenstein`.

## Building the native library

The facade links the shared library `libdictenstein` (file name
`liblibdictenstein.so` / `.dylib` / `.dll`):

```sh
cargo build --release --no-default-features --features ffi
```

## Compiling against it

With pkg-config (installs the `.pc` and headers):

```sh
c++ -std=c++20 app.cpp $(pkg-config --cflags --libs libdictenstein) -o app
```

From a source checkout, point the compiler at both header roots and the built
library:

```sh
c++ -std=c++20 app.cpp \
  -I include \
  -I ../liblevenshtein-rust/vinary-tree-interop/include \
  -L target/release -llibdictenstein -o app
LD_LIBRARY_PATH=target/release ./app
```

In CMake, `find_package(libdictenstein CONFIG REQUIRED)` exposes the imported
target; link it into your executable or library.

## Quickstart

```cpp
#include <libdictenstein.hpp>
#include <cassert>

namespace ld = vinary_tree::libdictenstein;

int main() {
    ld::dynamic_dawg dictionary;                 // Unicode-scalar by default
    (void) dictionary.insert("cat", 1);
    (void) dictionary.insert("cot", 2);
    (void) dictionary.insert("cut");             // valueless term
    assert(dictionary.size() == 3);
    assert(dictionary.contains("cot"));

    const ld::lookup hit = dictionary.get("cot");
    assert(hit.found && hit.value == 2);

    ld::scdawg suffixes;
    (void) suffixes.insert("cat");
    (void) suffixes.insert("cot");
    assert(suffixes.contains_substring("ot"));
    assert(suffixes.substring_frequency("t") == 2);
}
```

Each `dictionary` owns its native handle; the destructor calls
`ldict_dictionary_free`. Instances are move-only. Move-assignment frees the
previous handle, so ownership transfer never double-frees.

## Backends and capabilities

| Class | Kind | Unit domains | Capabilities |
|-------|------|--------------|--------------|
| `dynamic_dawg` | 1 | byte, unicode_scalar, u64 | read, insert, remove, clear, compact |
| `double_array_trie` | 2 | byte, unicode_scalar | read (immutable) |
| `scdawg` | 3 | byte, unicode_scalar | read, insert, substring |
| `persistent_artrie` | 4 | byte, unicode_scalar, u64 | read, insert, remove, checkpoint |
| `persistent_vocabulary` | 5 | unicode_scalar | read, insert, checkpoint |

Query `capabilities()` for the runtime bitset (`LDICT_CAP_*`), or `kind()` for
the backend identifier.

## Text domains and values

`std::string_view` terms are passed verbatim as bytes; the Unicode-scalar
backends validate UTF-8 and reject invalid input with status
`LDICT_STATUS_INVALID_UTF8`. The byte domain accepts any bytes, including
embedded NUL. The u64 domain takes `std::span<const std::uint64_t>` terms.
Values are `std::optional<std::uint64_t>` over the full `0 .. UINT64_MAX`
range; absence and a mapped value of `0` are distinct.

## Error handling

Non-OK statuses throw `vinary_tree::libdictenstein::error`, a
`std::runtime_error` whose message is the thread-local
`ldict_last_error_message()` captured at the failure site and whose `status()`
returns the `LdictStatus`. Unsupported operations for a backend surface as
`LDICT_STATUS_UNSUPPORTED`; a term submitted to the wrong unit domain surfaces
as `LDICT_STATUS_DOMAIN_MISMATCH` (9).

## Retained resource handoff

`resource()` returns the shared `VtResource` two-word handle. An independently
packaged liblevenshtein transducer retains it in constant time, and a query it
started keeps the exact immutable revision it saw at its start even after this
dictionary is destroyed.

## Testing

The cross-project snapshot test in
[`tests/cross_project_snapshot.cpp`](tests/cross_project_snapshot.cpp)
composes a dictionary with a liblevenshtein transducer. Compile it against both
libraries and run with the build directory on `LD_LIBRARY_PATH`.
