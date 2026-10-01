# Revision-8 language capability matrix

**Navigation**: [bindings corpus](README.md) ·
[revision-8 contract](backend-api-revision8.md) ·
[canonical API model](../../bindings/api.json)

This matrix tracks *idiomatic host capabilities*, not merely generated C
declarations. `PathMap` means a revision-8 exact-key dictionary with arbitrary
byte keys in byte mode, UTF-8/scalar keys in Unicode mode, optional `u64`
values, ordinary dictionary mutation, and retained snapshots. A typed suffix
index means distinct insertion records (including duplicate text), optional
values, exact-source and substring queries, byte/scalar occurrence counts,
ordered source pages, and retained source snapshots. It is **not** an
ordinary dictionary or an alias for SCDAWG.

`Supported` requires executable semantic tests. `Explicitly unsupported`
requires an executable failure test that cannot silently select an unrelated
backend. `Unqualified` means that neither outcome has yet been demonstrated;
it is **not** a promise of support. Direct C clients linked to new symbols
need a revision-8 library at process load. Optional dynamic loaders must
check `ldict_api_revision() >= 8` before resolving them. A mediated facade may
use its existing host/runtime bridge instead of importing C symbols itself.

| Host surface | PathMap dictionary | Typed suffix index | Qualification boundary |
| --- | --- | --- | --- |
| C ABI | Supported; feature-off constructor returns `UNSUPPORTED` | Supported as separate index/snapshot handles | [Public FFI controls](../../tests/ffi_backend_revision8.rs), [header layout](../../tests/fixtures/revision8_header_layout.c), and [revision-7 consumer](../../tests/fixtures/revision7_consumer.c) |
| Julia | Supported; constructor gates revision 8 | Supported as `SuffixIndex`/`SuffixSnapshot`, not `AbstractDict` | [Package tests](../../bindings/julia/Libdictenstein/test/runtests.jl) and [revision-7 probe](../../bindings/julia/Libdictenstein/test/revision7_gate.jl) |
| Raku | Supported as `Dictionary`; constructor gates revision 8 | Explicitly unsupported by `suffix-index`; raw C declarations are not a facade | [Conformance](../../bindings/raku/t/01-conformance.rakutest) and [revision-7 probe](../../bindings/raku/t/02-revision7-gate.rakutest) |
| C++ | Unqualified | Unqualified | Native header integration under review |
| Python | Unqualified | Unqualified | Dynamic `ctypes` loader under review |
| Ruby | Unqualified | Unqualified | Dynamic Fiddle loader under review |
| Lua | Unqualified | Unqualified | Native module under review |
| Go | Unqualified | Unqualified | cgo facade under review |
| OCaml | Unqualified | Unqualified | Copied C header is exact; high-level facade under review |
| JVM/Java | Unqualified | Unqualified | Java foreign-function facade under review |
| Clojure | Unqualified | Unqualified | Mediated through JVM facade; under review |
| JavaScript, TypeScript, ClojureScript | Unqualified | Unqualified | Mediated host runtime; under review |
| .NET/C# | Unqualified | Unqualified | P/Invoke facade under review |
| Swift | Unqualified | Unqualified | Clang-imported native facade under review |
| Fortran | Unqualified | Unqualified | ISO C binding facade under review |
| Haskell | Unqualified | Unqualified | FFI facade under review |

No RC.6 artifact should advertise an `Unqualified` capability as supported.
This matrix must be updated from tested evidence as the mirror lanes complete;
the source of truth for numeric symbols and layouts remains the
[canonical model](../../bindings/api.json), not this prose table.
