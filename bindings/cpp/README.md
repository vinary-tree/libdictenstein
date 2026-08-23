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
  -I ../vinary-tree-interop/include \
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

## Snapshot entry ranges

`dictionary::entries()` captures one immutable revision immediately and
returns a move-only C++20 input range. Entries borrow the current bounded
native batch, so consume their spans before incrementing the iterator:

```cpp
ld::dynamic_dawg dictionary(ld::unit_domain::unicode_scalar);
(void) dictionary.insert("cat", 0);        // mapped zero
(void) dictionary.insert("cut");           // present without a value

auto entries = dictionary.entries({64, 4096, 64});
for (const ld::entry_view entry : entries) {
    const std::span<const std::uint32_t> key = entry.unicode_scalars();
    const std::optional<std::uint64_t> value = entry.value();
    // key and value remain valid for this iteration step.
}
```

Use `bytes()`, `unicode_scalars()`, or `u64_units()` according to
`entries.domain()`. `exact_size()` is populated only when the captured provider
advertises exact cardinality; the range intentionally does not claim
`sized_range` conditionally at runtime. Destruction after normal exhaustion,
`break`, or an exception cancels the cursor, releases any live batch generation,
and closes it. Moving transfers this entire ownership ledger; copying is
disabled.

## Collection benchmark entrypoint

The public-header benchmark uses the Rust driver's deterministic corpus and
wrapping checksum. Dictionary construction and warmup occur before timing, and
the `materialized`, `stream`, and `stream-cancel` arms emit one JSON record:

```sh
c++ -std=c++20 -O2 -Wall -Wextra -Werror \
  bindings/cpp/examples/collection_traversal_profile.cpp \
  -I include -L target/release -llibdictenstein \
  -o /tmp/libdictenstein-cpp-collection-profile
LD_LIBRARY_PATH=target/release /tmp/libdictenstein-cpp-collection-profile \
  --arm stream --entries 4096 --batch-size 256
```

## Testing

The cross-project snapshot test in
[`tests/cross_project_snapshot.cpp`](tests/cross_project_snapshot.cpp)
composes a dictionary with a liblevenshtein transducer. Compile it against both
libraries and run with the build directory on `LD_LIBRARY_PATH`.

<!-- BEGIN GENERATED BINDING OPERATIONS; DO NOT EDIT -->

## Support and package contract

| Property | Contract |
|---|---|
| Binding | C++ |
| Languages/runtime | C++20/C++23 |
| Support tier | Tier 1 |
| Distribution | Header-only C++ facade in the native package |
| Native boundary | Move-only RAII over `ldict_*` |
| Canonical facade source | [`include/libdictenstein.hpp`](../../include/libdictenstein.hpp) |

All tiers implement the same ownership, snapshot, status, and compatibility
laws. The tier controls release gating rather than semantic quality. Start with
the [producer documentation hub](../../docs/bindings/README.md), then use the
[`ldict_*` C ABI reference](../../docs/bindings/c-abi-reference.md) for exact
preconditions, statuses, thread-safety, and complexity.

![A host facade owns a project handle while exported snapshots cross projects only as retained versioned resources.](../../docs/diagrams/abi-producer-component.svg)

## Installation and native loading

Install the distribution named above and its exact `vinary-tree-interop`
dependency. Published managed packages carry or resolve supported native
artifacts; source builds use the release library from `target/release` or the
installed CMake/pkg-config package. Diagnose loading in this order: toolchain
version, OS/CPU artifact, dependent package pin, loader path, then ABI/API
handshake. Never silently load an arbitrary same-named system library.

## Executable example and verification

The canonical checked example is [`bindings/cpp/tests/conformance.cpp`](../../bindings/cpp/tests/conformance.cpp). CI runs
the public package path with:

```sh
cmake -S bindings/cpp/tests/package -B target/cpp-package && cmake --build target/cpp-package && ctest --test-dir target/cpp-package
```

The example is also conformance evidence: it uses public constructors, checks
membership/value behavior, exports a retained resource, and closes every owned
handle. Cross-project suites pass that resource to liblevenshtein without
serialization.

## Public API, backends, and data domains

| Concept | Semantics |
|---|---|
| Dictionary handle | Owns one mutable or immutable backend instance and exposes kind/capability introspection. |
| CRUD and batch mutation | Text and `u64` operations preserve optional values; empty-batch and partial-failure behavior follows the C reference. |
| Persistent maintenance | `checkpoint`, `compact`, and `clear` are capability-gated and report unsupported operations explicitly. |
| Retained resource | `resource()` lends `vt.dictionary.v1`; a consumer retains and snapshots it independently. |
| Snapshot | Immutable revision with stable node identifiers, exact domains, bounded edge pages, and optional mapped values. |

Dynamic DAWG supports mutable finite-term dictionaries; double-array tries are
read-optimized static structures; SCDAWG indexes substrings; persistent ARTrie
and vocabulary stores provide durable byte/Unicode/`u64` domains. Select by
reported kind and capabilities rather than assuming every operation exists.

Text APIs validate UTF-8 and traverse Unicode scalar values. Byte APIs retain
arbitrary octets. Token APIs preserve the full `u64` range. Optional dictionary
values are represented separately from terminal membership, so `None` is not a
sentinel and empty terms remain valid when supported.

## Native collection surface

The shipped `dictionary::entries()` surface returns a move-only
`entries_view` satisfying the C++20 input-range/view concepts. Each
`entry_view` borrows byte, Unicode-scalar (`uint32_t`), or `u64` units and
returns `std::optional<uint64_t>` for mapped values. The range owns its snapshot
cursor; advancing releases completed batches, while destruction after `break`
or exception cancels, releases the live generation, and closes the cursor.


The pure Rust producer is the semantic and performance baseline: generic
snapshot traversal, borrowed and snapshot-owning `IntoIterator`, optimized bulk
`FromIterator`/`Extend` where infallible, named fallible variants for persistent
stores, deterministic order, and reusable fold/visitor paths. Read the local
[Rust API audit](../../docs/bindings/rust-api-idioms.md) and the family
[collection-protocol design](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/bindings/collection-protocols.md).

Public spelling must remain native to this ecosystem. The shared engine
standardizes laws and batching; it does not expose C handles, vtables, leases,
or status codes to ordinary application code. Documentation may mark a protocol
as shipped only after its language conformance and performance gates pass.

## Ownership, snapshots, and resource handoff

Keep dictionary/resource wrappers in lexical scope; moved-from wrappers are empty and safe to destroy.

An exported resource is borrowed until its base-vtable `retain` succeeds. A
captured snapshot arrives owning one retain and may outlive the mutable
dictionary. Later inserts, updates, removals, compaction, or checkpoints do not
alter a pinned revision. Every successful retain has exactly one release, and
failed construction transfers no ownership.

## Errors and failure containment

Non-OK statuses become `vinary_tree::libdictenstein::error` with status and diagnostic. Branch on the typed status or exception, not diagnostic text.
Invalid UTF-8, domain mismatch, unsupported capability, closed handle, bad
path, allocation failure, provider fault, I/O failure, and contained panic are
distinct cases. Copy thread-local diagnostics before another native call.

## Concurrency and reentrancy

Independent handles and immutable snapshots are reentrant. Mutations follow
the backend's advertised synchronization strategy; one host wrapper must not
invent a stronger promise. Snapshot capture is a linearization point and never
permits a torn root/count pair. Do not race close against another operation on
the same handle, and do not retain callback/paging buffers after return.

## Performance, durability, and marshalling

- Use bulk construction for an initially empty dictionary and presorted input
  when available; unordered construction uses the optimized sort-plus-minimal
  path.
- Batch mutations to amortize foreign-boundary crossings.
- Export retained resources instead of serializing dictionaries between
  Vinary packages.
- Keep byte, Unicode, and `u64` domains explicit to avoid transcoding.
- Treat `checkpoint` as durability, `compact` as representation maintenance,
  and `close` as ownership release; they are not interchangeable.

## Security model

Treat paths, terms, values, page offsets, serialized files, and foreign callers
as untrusted. Validate lengths before allocation, contain panics at the ABI,
bound diagnostics and batches, prevent path traversal, and reject unknown enum
values. See the [FFI boundary analysis](../../docs/security/ffi-boundary.md) and
[family security model](https://github.com/vinary-tree/vinary-tree-interop/blob/master/docs/security-model.md).

## Compatibility and troubleshooting

The project ABI, project API revision, family ABI, interface version, package
version, and persistent format version are separate counters. Negotiate each
at its documented boundary. For unexpected behavior, record dictionary kind,
capabilities, unit domain, persistence path, exact status, and copied diagnostic
before reducing the operation sequence.

## Maintainer checklist

1. Update `bindings/api.json` before changing a public facade or package pin.
2. Regenerate headers/constants and run the binding contract gate.
3. Extend the executable, negative-path, leak, and cross-project tests.
4. Update this guide when ownership, errors, capabilities, or platforms change.
5. Render PlantUML headlessly and run math/link/documentation checks.
6. Verify staged registry artifacts contain the guide and coherent pins.

<!-- END GENERATED BINDING OPERATIONS -->
