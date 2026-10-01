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
| C++ | Explicitly unsupported by the static RAII facade | Explicitly unsupported; never coerced to `dictionary` | [Linked conformance](../../bindings/cpp/tests/conformance.cpp) requires both requests to throw status 6 |
| Python | Supported; `ctypes` resolves constructor after revision gate | Explicitly unsupported by `SuffixIndex` | [Backend tests](../../bindings/python/tests/test_backends.py) and [real revision-7 symbol-set probe](../../bindings/python/tests/test_revision7_gate.py) |
| Ruby | Supported; Fiddle binds constructor after revision gate | Explicitly unsupported by `SuffixIndex` | [Conformance](../../bindings/ruby/test/test_conformance.rb) and [real revision-7 symbol-set probe](../../bindings/ruby/test/test_revision7_gate.rb) |
| Lua | Explicitly unsupported by the static C module | Explicitly unsupported; not SCDAWG | [Linked conformance](../../bindings/lua/test/conformance.lua) requires both constructors to raise status 6 |
| Go | Explicitly unsupported by the static cgo facade | Explicitly unsupported; never coerced to `Dictionary` | [Linked conformance](../../bindings/go/conformance/conformance_test.go) requires typed status 6 for both |
| OCaml | Explicitly unsupported by the static stubs | Explicitly unsupported as a distinct abstract type | [Linked conformance](../../bindings/ocaml/test/conformance.ml) requires both constructors to fail with status 6; copied C header remains exact |
| JVM/Java | Explicitly unsupported by the Java FFM facade | Explicitly unsupported; never mapped to `Dictionary` or SCDAWG | [Focused linked JUnit gate](../../bindings/jvm/src/test/java/io/vinarytree/libdictenstein/ConformanceTest.java) requires status 6 and revision-aware explanations |
| Clojure | Explicitly unsupported through the JVM facade | Explicitly unsupported; never mapped to a dictionary | [Producer-only](../../bindings/clojure/test/vinary_tree/libdictenstein_conformance.clj) and [idiomatic](../../bindings/clojure/test/vinary_tree/libdictenstein_test.clj) linked gates each passed 1 test/6 assertions with status-6 checks |
| JavaScript, TypeScript, ClojureScript | Unqualified as a combined family | Unqualified as a combined family | JavaScript ESM/CJS tests passed 10/10, but TypeScript declaration and ClojureScript executable gates remain pending; no combined-family support claim yet |
| .NET/C# | Explicitly unsupported by the static P/Invoke facade | Explicitly unsupported; not SCDAWG | [Linked conformance](../../bindings/dotnet/tests/VinaryTree.Libdictenstein.Conformance/Program.cs) requires `LibdictensteinException` status 6 for both |
| Swift | Explicitly unsupported by the static facade | Explicitly unsupported; never coerced to `Dictionary` | [Linked conformance](../../bindings/swift/libdictenstein/Tests/LibdictensteinTests/ConformanceTests.swift): 28/28 tests, including status-6 probes |
| Fortran | Explicitly unsupported by the static ISO C facade | Explicitly unsupported as a separate `suffix_index` type | [Linked conformance](../../bindings/fortran/test/conformance.f90) requires `ldict_unsupported` status 6 and null handles |
| Haskell | Explicitly unsupported by the static FFI facade | Explicitly unsupported; not SCDAWG | [Linked conformance](../../bindings/haskell/test/Conformance.hs) requires both requests to raise status 6 |

No RC.6 artifact should advertise an `Unqualified` capability as supported.
This matrix must be updated from tested evidence as the mirror lanes complete;
the source of truth for numeric symbols and layouts remains the
[canonical model](../../bindings/api.json), not this prose table.
