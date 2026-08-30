# Bindings — the producer contract corpus

**Navigation**: [← Documentation index](../README.md)

libdictenstein is the **producer** half of the family's dictionary ABI: it
owns the concrete dictionaries and their CRUD, exports a 42-function `ldict_*`
C surface, and hands consumers a two-word, retained `vt.dictionary.v1`
resource whose snapshots they walk. The **consumer** half (cursor model, lease
protocol, language-facade query APIs) lives in
[liblevenshtein](https://github.com/vinary-tree/liblevenshtein-rust) — this
corpus documents everything on the producing side of that boundary.

## What lives where

| Artifact | Path | What it is |
|---|---|---|
| **C ABI reference** | [`c-abi-reference.md`](c-abi-reference.md) | The normative reference for all 42 `ldict_*` functions: exact signatures, preconditions, exact status sets, ownership, thread-safety, complexity; the status/kind/capability tables; the per-backend support matrix; persistence caveats; a compile-and-run-verified C example. |
| **Resource-producer architecture** | [`resource-producer.md`](resource-producer.md) | How the producer side works: the four backend bindings, `OwnedDictionaryResource` and the retain ledger, per-backend $`\mathcal{O}(1)`$ snapshot capture, lazy ABI-local node ids, the flag truth table, and the new-backend checklist. |
| **Native Rust idioms** | [`rust-api-idioms.md`](rust-api-idioms.md) | Confirmed iterator/construction gaps in the pure Rust producer and the optimized, generic target for `Iterator`, `IntoIterator`, `FromIterator`, `Extend`, fallible bulk construction, folds, snapshots, and every automaton/unit domain. |
| **FFI boundary analysis** | [`../security/ffi-boundary.md`](../security/ffi-boundary.md) | The producer-side trust analysis: what a misbehaving foreign caller can and cannot cause, and whose duty each defense is. Extends the [threat model](../security/threat-model.md). |
| **Findings ledger** | [`FINDINGS_LEDGER.md`](FINDINGS_LEDGER.md) | The scientific ledger of binding-scrutiny findings: defects, pins, coverage gaps, and version-pin inconsistencies (`LDICT-B<N>` schema). |
| **Machine-readable model** | [`../../bindings/api.json`](../../bindings/api.json) | The source of truth for the binding surface: symbols, enums, kinds, capabilities, marshalling and snapshot laws, facade layout, registry coordinates. |
| **Contract gates** | [`../../scripts/check-bindings.py`](../../scripts/check-bindings.py), [`../../scripts/check-binding-docs.py`](../../scripts/check-binding-docs.py) | Enforce the ABI model and reject a declared facade whose guide, executable evidence, required operational topics, or local links are missing or stale. CI job `binding-contract`. |
| **Diagrams** | [`../diagrams/`](../diagrams/) | `abi-producer-component` (layer map), `snapshot-capture-sequence` (the walk protocol), `owned-resource-lifecycle-state` (the retain ledger); sources under [`../diagrams/src/`](../diagrams/src/). |
| **Language facades** | [`../../bindings/`](../../bindings/) | Sixteen governed guides over the `ldict_*` surface, including the native C contract and grouped JVM/JavaScript language families. |
| **Guide generator** | [`../../scripts/generate-binding-guides.py`](../../scripts/generate-binding-guides.py) | Owns the uniform support, loading, ownership, error, concurrency, performance, security, compatibility, and maintainer sections while preserving each facade's handwritten tutorial. |

Collection interfaces are shipped across the packages in the matrix. Six of
six of the 42 C functions form the shared bounded-entry cursor and reducer substrate;
the optimized [pure Rust surface](rust-api-idioms.md) bypasses that ABI, and
each foreign facade maps the same snapshot, ordering, value, and cancellation
laws to its own familiar protocols. The family
[collection-protocol contract](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/bindings/collection-protocols.md)
is the normative semantic reference. The public APIs deliberately are not a
lowest-common-denominator transliteration of C.

Ordinary collection views own their host data and therefore remain usable
after the dictionary changes or closes. Streaming views instead retain one
immutable native revision and lease bounded batches; callers must use the
language's deterministic lexical cleanup form when they may stop early.
Membership operations remain direct dictionary lookups, never traversal scans.

## Native collection quick reference

| Language | Familiar materialized or standard surface | Bounded stream / reduction | Deterministic lifetime |
|---|---|---|---|
| Rust | `DictionaryEntries`/`Terms`/`Keys`/`Values`, `IntoIterator`, `FromIterator`, `Extend`, lazy zipper collections | `fold_entries` / `try_fold_entries`; iterators pin one revision | Ownership and `Drop`; fallible persistent builders are explicit `try_*` APIs |
| C17/C23 | Caller-owned arrays copied from `LdictEntryBatch` | `ldict_entry_cursor_*`; `ldict_entry_cursor_reduce` callback fold | Exact-generation `release`, then `cancel` on early exit and `free` |
| C++20/C++23 | `dictionary::entries()` is a standard move-only input range; collect into an ordinary container when retention is needed | Range algorithms over borrowed `entry_view` values | RAII closes after exhaustion, `break`, or exception |
| Python | `DictionarySnapshot` implements `Mapping`; dictionaries implement mapping protocols and expose `snapshot`, `keys`, `items`, and `values` | `stream_entries(...)` returns an `Iterator` context manager | `with` for streams and dictionaries; `close` remains explicit and idempotent |
| Java | `Dictionary` is `Iterable<DictionaryEntry>`; `DictionarySnapshot` is an immutable `Collection` with `List`, `Set`, `Collection`, and `Map` views | `EntryStream` is `Iterator` + `Spliterator`; `streamEntries` returns a sequential `Stream` | try-with-resources for dictionaries and any stream that may stop early |
| Kotlin | Java collection views support ordinary iteration and `snapshot().asSequence()` | `openEntryStream(n).use { it.asSequence() }` | `use` scopes the `AutoCloseable` stream and dictionary |
| Scala | Java collection views adapt with `snapshot().asScala` | `Using.resource(openEntryStream(n))(_.asScala)` | `Using` scopes the `AutoCloseable` resource |
| Clojure | Persistent-vector snapshots support seq/reduce/transduce; `entry-eduction` composes transducers | `with-entry-stream`, `stream-seq`, `reduce-entries`, `transduce-entries` | `with-open` through `with-entry-stream` |
| JavaScript | Iterable `DictionarySnapshot`, `entries`, `keys`, `values`, `forEach`, and `toMap` | `streamEntries()` is a closeable iterator with `nextBatch` and `reduceBatches` | iterator `return`, `close`, and `Symbol.dispose` where supported |
| TypeScript | The JavaScript collection surface with declared key/value/snapshot types | Typed `streamEntries()` closeable iterable cursor | `using`/`Symbol.dispose` where available, otherwise `try`/`finally` + `close` |
| ClojureScript | Persistent-vector `snapshot`, seq `entries`, `keys`, and `values` | `with-entry-stream` and `reduce-entries` over `streamEntries()` | Facade helpers close in `finally` |
| C# / .NET | `Dictionary` is `IEnumerable<KeyValuePair<…>>`; `DictionarySnapshot` is `IReadOnlyDictionary` + `IReadOnlyCollection`; LINQ composes naturally | `OpenEntryStream` returns `IEnumerator` + `IDisposable` | `using`; `foreach` disposes its enumerator |
| Go | `SnapshotEntries`/`Entries` return host-owned `EntrySnapshot` values | `OpenEntryStream`, `Next`, and Go 1.23 `iter.Seq`/`Seq2` | range helpers close on exhaustion, `break`, or panic; direct pulls use `defer Close` |
| Swift | `entries()` returns an `EntrySnapshot : RandomAccessCollection` | `entryStream`, `next`, and `cancel` | `defer { try? stream.close() }`; `deinit` is fallback containment |
| Ruby | Every dictionary includes `Enumerable`; `entries`, `keys`, and `values` materialize snapshots | `each`/`Enumerator` or manual `entry_stream` | `ensure` closes iteration; manual streams expose `cancel` and `close` |
| Fortran | Owned `dictionary_entry_batch` values are normally assignable | `open_entries`/`next_batch`; `fold_entries` callback procedure | Explicit `%close`; finalization is fallback containment |
| OCaml | `with_entries_seq` supplies a native-ordered `Seq.t` | `fold_entries` provides synchronous reduction | `Fun.protect` scopes and closes the cursor |
| Haskell | `materializeEntries` returns a `Foldable DictionarySnapshot` | `withEntryStream`/`nextEntry`; `foldEntries` | Bracketed `with*` APIs mask exceptions across release; `ForeignPtr` finalizer is fallback |
| Lua 5.4 | `dictionary:entries()` snapshot works with `pairs` | `entries_iter` generic-for form or explicit `entry_cursor` | to-be-closed values or explicit `:close`; `__gc` is fallback containment |
| Julia | `Dictionary <: AbstractDict`; standard keys, values, iteration, mutation, and set-like algebra | Iteration pins one retained native snapshot | `close` in `finally`; finalizers contain abandoned handles |
| Raku | `Dictionary does Associative does Iterable`; postcircumfix lookup and ordinary `Seq`/`for` traversal | Explicit closeable iterator over one retained snapshot | `LEAVE $iterator.close` after early termination; `DESTROY` is fallback |

## Collection benchmark entrypoints

All profiles keep construction and warmup outside the timed drain and emit one
[`libdictenstein.host-collection-traversal.v1`](../benchmarks/collection-traversal-and-bindings.md#foreign-facade-measurements)
JSON object. Commands below select the representative streaming arm; replace
it with `materialized`, `stream-cancel`, or `reduce` only when the linked guide
lists that arm. C and C++ first compile the public example exactly as shown in
their guides.

| Runtime / package | Public-package command |
|---|---|
| Rust | `cargo run --release --features bindings-core --example collection_traversal_profile -- --arm direct-owned --entries 4096` |
| [C](../../bindings/c/README.md#collection-benchmark-entrypoint) | `LD_LIBRARY_PATH=target/release /tmp/libdictenstein-c-collection-profile --arm stream --entries 4096 --batch-size 256` |
| [C++](../../bindings/cpp/README.md#collection-benchmark-entrypoint) | `LD_LIBRARY_PATH=target/release /tmp/libdictenstein-cpp-collection-profile --arm stream --entries 4096 --batch-size 256` |
| [Python](../../bindings/python/README.md) | `PYTHONPATH=bindings/python/src python -m libdictenstein._collection_profile --arm stream --entries 4096 --batch-size 256` |
| [JVM: Java, Kotlin, Scala](../../bindings/jvm/README.md) | `./gradlew -p bindings/jvm collectionTraversalProfile -PjavaToolchain=22 -PvinaryTree.nativeDir=../../target/debug -PprofileArgs='--arm stream --entries 4096 --batch-size 256'` |
| [Clojure](../../bindings/clojure/README.md#collection-benchmark-entrypoint) | `cd bindings/clojure && clojure -J-Djava.library.path=../../target/release -M:profile --arm stream --entries 4096 --batch-size 256` |
| [JavaScript, TypeScript, ClojureScript](../../bindings/javascript/README.md) | `node bindings/javascript/bin/libdictenstein-collection-profile.mjs --runtime native --arm stream --entries 4096 --batch-size 256` |
| [.NET / C#](../../bindings/dotnet/README.md) | `dotnet run --project bindings/dotnet/benchmarks/VinaryTree.Libdictenstein.CollectionTraversalProfile/VinaryTree.Libdictenstein.CollectionTraversalProfile.csproj -c Release -f net10.0 -- --arm stream --entries 4096 --batch-size 256` |
| [Go](../../bindings/go/README.md) | `go run ./bindings/go/cmd/collection-traversal-profile --arm stream --entries 4096 --batch-size 256` |
| [Swift](../../bindings/swift/README.md) | `swift run --package-path bindings/swift/libdictenstein -c release libdictenstein-collection-profile --arm stream --entries 4096 --batch-size 256` |
| [Ruby](../../bindings/ruby/README.md) | `ruby bindings/ruby/bin/libdictenstein-collection-profile --arm stream --entries 4096 --batch-size 256` |
| [Fortran](../../bindings/fortran/README.md) | `fpm run --directory bindings/fortran --profile release --example collection_traversal_profile -- --arm stream --entries 4096 --batch-size 256` |
| [OCaml](../../bindings/ocaml/README.md) | `dune exec --root bindings/ocaml bin/collection_traversal_profile.exe -- --arm stream --entries 4096 --batch-size 256` |
| [Haskell](../../bindings/haskell/README.md) | `cabal run --project-file=bindings/haskell/cabal.project libdictenstein-collection-profile -- --arm stream --entries 4096 --batch-size 256` |
| [Lua](../../bindings/lua/README.md) | `lua bindings/lua/examples/collection_traversal_profile.lua --arm stream --entries 4096 --batch-size 256` |

## Language guide matrix

The guide is part of the package contract, not release-adjacent prose. Every
row names a checked example that exercises the public facade and deterministic
resource cleanup. “Tier” controls how frequently a package is release-gated;
it does not weaken ownership, snapshot, or error semantics.

| Guide | Represented languages | Tier | Boundary |
|---|---|---:|---|
| [C](../../bindings/c/README.md) | C17/C23 | 1 | Direct `ldict_*` ABI |
| [C++](../../bindings/cpp/README.md) | C++20/C++23 | 1 | Move-only RAII over C |
| [Python](../../bindings/python/README.md) | Python | 1 | `ctypes` |
| [JVM](../../bindings/jvm/README.md) | Java, Kotlin, Scala | 1 | Java Foreign Function & Memory API |
| [Clojure](../../bindings/clojure/README.md) | Clojure | 1 | JVM facade |
| [JavaScript family](../../bindings/javascript/README.md) | JavaScript, TypeScript, ClojureScript | 1 | Singleton N-API/WebAssembly/WASI runtime |
| [.NET](../../bindings/dotnet/README.md) | C# | 2 | P/Invoke |
| [Go](../../bindings/go/README.md) | Go | 2 | cgo |
| [Swift](../../bindings/swift/README.md) | Swift | 2 | Swift system-library target |
| [Ruby](../../bindings/ruby/README.md) | Ruby | 2 | Fiddle |
| [Fortran](../../bindings/fortran/README.md) | Fortran | 2 | `iso_c_binding` |
| [OCaml](../../bindings/ocaml/README.md) | OCaml | 3 | C stubs |
| [Haskell](../../bindings/haskell/README.md) | Haskell | 3 | Haskell FFI |
| [Lua](../../bindings/lua/README.md) | Lua | 3 | C userdata module |
| [Julia](../../bindings/julia/README.md) | Julia | 2 | `ccall` plus `VinaryTreeInterop` retained snapshots |
| [Raku](../../bindings/raku/README.md) | Raku | 3 | NativeCall plus `Vinary-Tree-Interop` retained snapshots |

Regenerate the governed sections after changing
[`bindings/api.json`](../../bindings/api.json), package metadata, or a public
facade:

```sh
python3 scripts/generate-binding-guides.py
python3 scripts/check-binding-docs.py
```

## Reading order

1. **Orient** — the component diagram in
   [`resource-producer.md § 2`](resource-producer.md#2-architecture) shows
   the whole producer stack on one page.
2. **Call it** — [`c-abi-reference.md`](c-abi-reference.md), front to back:
   versioning → status discipline → backend matrix → the function groups →
   the verified example.
3. **Understand what you were handed** — the rest of
   [`resource-producer.md`](resource-producer.md): snapshots, node-id leasing,
   flags, and the refcount ledger.
4. **Trust it** — [`../security/ffi-boundary.md`](../security/ffi-boundary.md)
   for the adversarial reading, then the family canon below for the laws this
   repo instantiates.
5. **Audit it** — [`FINDINGS_LEDGER.md`](FINDINGS_LEDGER.md) plus a local run
   of `python3 scripts/check-bindings.py`.

## Family documents

Canonical family-level specifications live with the interop crate in
liblevenshtein-rust (linked absolutely — cross-repo relative paths do not
survive packaging):

- [ABI reference — `vinary_tree_interop.h`, annotated](https://github.com/vinary-tree/vinary-tree-interop/blob/master/docs/abi-reference.md)
- [ABI evolution policy — the four version counters](https://github.com/vinary-tree/vinary-tree-interop/blob/master/docs/abi-evolution.md)
- [Family security model — trust zones and validation duties](https://github.com/vinary-tree/vinary-tree-interop/blob/master/docs/security-model.md)
- [liblevenshtein language-binding architecture (the consumer side)](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/language-bindings.md)
