#!/usr/bin/env python3
"""Render the uniform operational contract in every libdictenstein guide."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MARKER = "<!-- BEGIN GENERATED BINDING OPERATIONS; DO NOT EDIT -->"
END = "<!-- END GENERATED BINDING OPERATIONS -->"


@dataclass(frozen=True)
class Guide:
    name: str
    runtimes: str
    tier: str
    package: str
    boundary: str
    cleanup: str
    errors: str
    source: str
    evidence: str
    command: str


GUIDES = {
    "c": Guide("C", "C17/C23", "Tier 1", "CMake/pkg-config native package", "Direct `ldict_*` calls", "Balance every successful constructor or retained resource with exactly one free or release.", "Inspect `LdictStatus` and copy the thread-local diagnostic before another call.", "include/libdictenstein.h", "bindings/c/examples/snapshot_walk.c", "cc -std=c17 -Wall -Wextra -Werror -fsyntax-only -Iinclude bindings/c/examples/snapshot_walk.c"),
    "cpp": Guide("C++", "C++20/C++23", "Tier 1", "Header-only C++ facade in the native package", "Move-only RAII over `ldict_*`", "Keep dictionary/resource wrappers in lexical scope; moved-from wrappers are empty and safe to destroy.", "Non-OK statuses become `vinary_tree::libdictenstein::error` with status and diagnostic.", "include/libdictenstein.hpp", "bindings/cpp/tests/conformance.cpp", "cmake -S bindings/cpp/tests/package -B target/cpp-package && cmake --build target/cpp-package && ctest --test-dir target/cpp-package"),
    "python": Guide("Python", "Python 3.10+", "Tier 1", "PyPI `libdictenstein`", "`ctypes` over the stable C ABI", "Use dictionaries as context managers or call `close()` in `finally`; finalizers are fallback containment.", "Native statuses become typed Python exceptions preserving the diagnostic.", "bindings/python/src/libdictenstein", "bindings/python/tests/test_backends.py", "PYTHONPATH=bindings/python/src pytest -q bindings/python/tests"),
    "jvm": Guide("JVM", "Java 22+, Kotlin, and Scala", "Tier 1", "Maven `io.vinarytree:libdictenstein`", "Java FFM over the stable C ABI", "Use try-with-resources, Kotlin `use`, or Scala `Using`; `Cleaner` is not deterministic cleanup.", "Native statuses become typed JVM exceptions with symbolic status and diagnostic.", "bindings/jvm/src/main/java/io/vinarytree/libdictenstein", "bindings/jvm/src/test/java/io/vinarytree/libdictenstein/BackendIntegrationTest.java", "./gradlew -p bindings/jvm test"),
    "clojure": Guide("Clojure", "Clojure 1.12+ on Java 22+", "Tier 1", "Clojars `io.vinarytree/libdictenstein-clojure`", "Idiomatic namespace delegating to the JVM facade", "Use `with-open`; close persistent stores before process teardown so checkpoints and descriptors are deterministic.", "JVM failures become `ExceptionInfo` with structured status data.", "bindings/clojure/src/vinary_tree/libdictenstein.clj", "bindings/clojure/test/vinary_tree/libdictenstein_test.clj", "clojure -M:test -m vinary-tree.libdictenstein-test-runner"),
    "javascript": Guide("JavaScript family", "JavaScript, TypeScript, and ClojureScript", "Tier 1", "npm `@vinary-tree/libdictenstein`", "Facade on the singleton `@vinary-tree/javascript-runtime`", "Use `using` or `close()`/`close!` in `finally`; never rely on GC to flush persistent state.", "Failures become structured `VinaryTreeError` instances.", "bindings/javascript", "bindings/javascript/test/facades.test.mjs", "npm test --prefix bindings/javascript"),
    "dotnet": Guide(".NET", ".NET 8+ / C#", "Tier 2", "NuGet `Libdictenstein`", "P/Invoke and `VinaryTree.Interop` retained resources", "Use `using`; `SafeHandle` covers exceptional paths but not prompt checkpoint/close policy.", "Failures become typed .NET exceptions containing status and diagnostic.", "bindings/dotnet/src/VinaryTree.Libdictenstein", "bindings/dotnet/tests/VinaryTree.Libdictenstein.Tests/Program.cs", "dotnet run --project bindings/dotnet/tests/VinaryTree.Libdictenstein.Tests"),
    "go": Guide("Go", "Go 1.25+ with cgo", "Tier 2", "Go module `github.com/vinary-tree/libdictenstein/bindings/go/v4`", "cgo over `ldict_*`", "Call `Close` with `defer` immediately after construction; finalizers only report abandoned handles.", "Operations return inspectable Go errors with native status and diagnostic.", "bindings/go/libdictenstein.go", "bindings/go/libdictenstein_test.go", "go test ./bindings/go/..."),
    "swift": Guide("Swift", "Swift 6+", "Tier 2", "SwiftPM `Libdictenstein`", "Swift system-library target over the C ABI", "Use lexical `defer` and explicit `close`; `deinit` timing is not a persistence guarantee.", "C statuses become throwing Swift errors preserving diagnostics.", "bindings/swift/libdictenstein/Sources/Libdictenstein", "bindings/swift/libdictenstein/Tests/LibdictensteinTests/ConformanceTests.swift", "swift test --package-path bindings/swift/libdictenstein"),
    "ruby": Guide("Ruby", "Ruby 3.3+", "Tier 2", "RubyGems `libdictenstein`", "Fiddle over the stable C ABI", "Prefer block forms or `ensure { dictionary.close }`; close persistent stores explicitly.", "Failures become typed Ruby exceptions with status and diagnostic.", "bindings/ruby/lib/vinary_tree/libdictenstein", "bindings/ruby/test/test_conformance.rb", "ruby -Ibindings/ruby/lib bindings/ruby/test/test_conformance.rb"),
    "fortran": Guide("Fortran", "Fortran 2018", "Tier 2", "fpm `libdictenstein`", "`iso_c_binding` over `ldict_*`", "Call the derived handle's `close`; final procedures are last-resort cleanup.", "Procedures preserve both the project status and native diagnostic.", "bindings/fortran/src/vinary_tree_libdictenstein.f90", "bindings/fortran/test/conformance.f90", "fpm test --profile release --directory bindings/fortran"),
    "ocaml": Guide("OCaml", "OCaml 5", "Tier 3", "opam `libdictenstein`", "C stubs over the stable ABI", "Use explicit close functions or `Fun.protect`; finalizers only protect abandoned values.", "Statuses become typed OCaml exceptions with copied diagnostics.", "bindings/ocaml/vinary_tree_libdictenstein.mli", "bindings/ocaml/test/conformance.ml", "opam exec -- dune runtest --root bindings/ocaml"),
    "haskell": Guide("Haskell", "GHC/Cabal", "Tier 3", "Hackage `libdictenstein`", "Haskell FFI plus retained interop resources", "Use `bracket`/`withDictionary`; mask asynchronous exceptions across acquire/release.", "Failures become typed Haskell exceptions/status values.", "bindings/haskell/src/VinaryTree/Libdictenstein.hs", "bindings/haskell/test/Conformance.hs", "cabal test --project-file=bindings/haskell/cabal.project all"),
    "lua": Guide("Lua", "Lua 5.4+", "Tier 3", "LuaRocks `libdictenstein`", "C userdata module over the ABI", "Use to-be-closed variables or `:close()`; `__gc` is fallback cleanup.", "Failures become Lua errors carrying the symbolic status and diagnostic.", "bindings/lua/src/libdictenstein_lua.c", "bindings/lua/test/conformance.lua", "lua bindings/lua/test/conformance.lua"),
    "julia": Guide("Julia", "Julia 1.10+", "Tier 2", "Julia General `Libdictenstein`", "`ccall` over the stable C ABI plus `VinaryTreeInterop` snapshots", "Call `close` in `finally`; finalizers contain abandoned handles but do not define resource lifetime.", "Native statuses become `NativeError` values containing the exact operation and copied diagnostic.", "bindings/julia/Libdictenstein/src/Libdictenstein.jl", "bindings/julia/Libdictenstein/test/runtests.jl", "julia --project=bindings/julia/Libdictenstein -e 'using Pkg; Pkg.test()'"),
    "raku": Guide("Raku", "Rakudo 2025.01+", "Tier 3", "Zef `Libdictenstein`", "NativeCall over the stable C ABI plus `Vinary-Tree-Interop` snapshots", "Call `close` in `LEAVE`/`CATCH` paths; `DESTROY` is fallback containment, and explicitly close iterators after early termination.", "Native statuses become `X::Libdictenstein` exceptions containing the exact operation and copied diagnostic.", "bindings/raku/lib/Libdictenstein.rakumod", "bindings/raku/t/01-conformance.rakutest", "raku -Ibindings/raku/lib -I../vinary-tree-interop/bindings/raku/lib bindings/raku/t/01-conformance.rakutest"),
}


ALGEBRA_EXAMPLES = {
    "C": ("c", "LdictDictionary *joined = NULL;\nLdictStatus status = ldict_dictionary_algebra(\n    left, right, LDICT_ALGEBRA_UNION, LDICT_VALUE_LATTICE_JOIN, &joined);\nif (status == LDICT_OK) ldict_dictionary_free(joined);"),
    "C++": ("cpp", "auto joined = left.set_union(right, value_merge::lattice_join);"),
    "Python": ("python", "with left.union(right, ValueMerge.LATTICE_JOIN) as joined:\n    print(len(joined))"),
    "JVM": ("java", "try (var joined = left.union(right, ValueMerge.LATTICE_JOIN)) {\n    System.out.println(joined.size());\n}"),
    "Clojure": ("clojure", "(with-open [joined (d/union left right {:value-merge :lattice-join})]\n  (println (d/size joined)))"),
    "JavaScript family": ("javascript", "using joined = left.union(right, \"lattice-join\");\nusing common = left.intersection(right);"),
    ".NET": ("csharp", "using var joined = left.Union(right, ValueMerge.LatticeJoin);\nusing var common = left & right;"),
    "Go": ("go", "joined, err := left.UnionWith(right, libdictenstein.LatticeJoinValue)\nif err != nil { return err }\ndefer joined.Close()\ncount, err := joined.Len()\nif err != nil { return err }\nfmt.Println(count)"),
    "Swift": ("swift", "let joined = try left.union(right, valueMerge: .latticeJoin)\ndefer { joined.close() }"),
    "Ruby": ("ruby", "joined = left.union(right, value_merge: LD::ValueMerge::LATTICE_JOIN)\nbegin\n  puts joined.length\nensure\n  joined.close\nend"),
    "Fortran": ("fortran", "call left%set_union(right, joined, value_merge_lattice_join, status)\nif (status /= ldict_ok) error stop \"union failed\"\ncall joined%close()"),
    "OCaml": ("ocaml", "let joined = union ~value_merge:Lattice_join left right in\nFun.protect ~finally:(fun () -> close joined)\n  (fun () -> Printf.printf \"%d\\n\" (length joined))"),
    "Haskell": ("haskell", "bracket (algebra Union LatticeJoin left right) close $ \\joined ->\n  dictionaryLength joined >>= print"),
    "Lua": ("lua", "local joined <close> = left:union(right, \"lattice_join\")\nlocal common <close> = left & right"),
    "Julia": ("julia", "joined = algebra(left, right, ALGEBRA_UNION, VALUE_MERGE_LATTICE_JOIN)\ntry\n    println(length(joined))\nfinally\n    close(joined)\nend"),
    "Raku": ("raku", "my $joined = $left.union($right, merge => VALUE-MERGE-LATTICE-JOIN);\nLEAVE $joined.close;"),
}


def algebra_section(g: Guide) -> str:
    language, example = ALGEBRA_EXAMPLES[g.name]
    return f"""## Snapshot-consistent dictionary algebra

Every facade exposes native union, intersection, left difference, and
symmetric difference. The operation captures one immutable revision from each
input; those two captures are independent, and later mutations cannot alter
the result. Inputs must use the same byte, Unicode-scalar, or `u64` term
domain.

The producer merges the two lexicographically ordered entry streams once and
feeds the sorted, duplicate-free output directly to the DynamicDAWG
freeze-once builder. For input cardinalities $`|A|`$ and $`|B|`$, this is
$`\\Theta(|A|+|B|)`$ work plus $`\\Theta(|R|)`$ result storage. It avoids a
host-language hash table, per-entry foreign calls, and repeated mutable graph
publication. The returned DynamicDAWG is independently mutable.

Keys present in both inputs use an explicit optional-`u64` value policy:
left/first, right/last, lattice join (optional maximum), or lattice meet
(shared optional minimum). Valueless membership remains distinct from absence
and from the value zero. Union defaults to right/last and intersection defaults
to lattice meet; difference operations have no overlapping output key, so a
value policy cannot affect them.

```{language}
{example}
```
"""

def block(g: Guide) -> str:
    benchmark_section = ""
    if g.name == "C":
        collection_section = """The shipped low-level collection surface is an opaque `LdictEntryCursor`
over one immutable revision. Callers select hard descriptor/unit/value bounds,
lease one `LdictEntryBatch` at a time, release its exact generation before the
next call, and use `ldict_entry_cursor_reduce` when a synchronous callback fold
is more natural. `cancel` is sticky and idempotent; `free` requires no live
lease. Batch descriptors preserve byte, Unicode-scalar (`uint32_t`), and `u64`
unit arenas plus valueless versus present-`u64` members without sentinels."""
    elif g.name == "C++":
        collection_section = """The shipped `dictionary::entries()` surface returns a move-only
`entries_view` satisfying the C++20 input-range/view concepts. Each
`entry_view` borrows byte, Unicode-scalar (`uint32_t`), or `u64` units and
returns `std::optional<uint64_t>` for mapped values. The range owns its snapshot
cursor; advancing releases completed batches, while destruction after `break`
or exception cancels, releases the live generation, and closes the cursor."""
    elif g.name == "Python":
        collection_section = """`Dictionary.snapshot()` returns an immutable, repeatable
`collections.abc.Mapping` copied from one native revision. `keys()`, `items()`,
and `values()` are ordinary Python views; membership remains a direct native
lookup. `stream_entries()` returns a context-managed iterator with exact length
and snapshot identity metadata when advertised. Each yielded `bytes`, `str`, or
`tuple[int, ...]` key owns its Python storage, and `with` closes promptly after
exhaustion, `break`, or exception."""
    elif g.name == "JVM":
        collection_section = """`Dictionary.snapshot()` returns an immutable ordered
`DictionarySnapshot` with `Collection`, `List`, `Set`, and `Map` views over
value-semantic `DictionaryKey`/`DictionaryEntry` objects. The dictionary is
repeatably `Iterable`; `openEntryStream` is a closeable `Iterator`/`Spliterator`,
and `streamEntries` is a sequential Java `Stream`. Java uses
try-with-resources, Kotlin uses collections/`asSequence()`/`use`, and Scala uses
collection converters/`Using`; all share the same FFM cursor and never depend
on garbage collection for native cleanup."""
    elif g.name == "Clojure":
        collection_section = """The shipped facade provides immutable `entries`, `keys`,
`values`, and `entry-seq` snapshots plus reducible `entry-eduction` adapters.
`with-entry-stream` scopes the native cursor for `reduce-entry-stream` and
`transduce-entry-stream`, so reduced values and exceptions close promptly.
Byte arrays and `long` arrays are copied losslessly, while ordinary Unicode
terms remain strings. No lazy sequence is allowed to outlive its resource
scope."""
    elif g.name == "JavaScript family":
        collection_section = """Dictionaries expose familiar `size`, `has`, `get`, `set`,
`delete`, `entries`, `keys`, `values`, `forEach`, and `[Symbol.iterator]`
operations. Ordinary iteration is backed by one host-owned immutable snapshot;
`streamEntries()` is the explicit bounded native cursor and implements
`return`, `close`, and `Symbol.dispose` for prompt cleanup after early exit.
Native Node, browser-WASM, and WASI runtimes preserve the same synchronous
contract; no fake async iterator or promise hop is introduced."""
    elif g.name == ".NET":
        collection_section = """`SnapshotEntries()` returns a repeatable immutable
`IReadOnlyCollection<DictionaryEntry>` with ordered `IReadOnlySet<DictionaryKey>`
and `IReadOnlyDictionary<DictionaryKey, ulong?>` views. `StreamEntries()` is an
`IEnumerable<DictionaryEntry>`/`IEnumerator<DictionaryEntry>` whose native
cursor implements `IDisposable`; `foreach` and LINQ work naturally, while
early-stop code uses `using`. `DictionaryKey` has value equality across text,
bytes, and unsigned `ulong` token sequences."""
    elif g.name == "Go":
        collection_section = """`SnapshotEntries` and `Entries` materialize host-owned keys from one
immutable revision. `OpenEntryStream` exposes bounded `Next`, `Cancel`, and
`Close` operations plus Go 1.23 range-compatible `Seq` and `Seq2` helpers.
Range exit closes automatically, including early `break` and panic; direct
`Next` callers close explicitly. `SnapshotEntry` keeps byte keys as `[]byte`,
Unicode-scalar keys as `string`, `u64` keys as `[]uint64`, and mapped values as
`*uint64`, so nil, zero, and max remain distinct."""
        benchmark_section = """
The public-package benchmark keeps construction and warmup outside the timed
drain and prints one JSON record. Run the 4,096-entry latency cell, the
65,536-entry streaming cell, or the 64-entry cancellation cell with:

```sh
go run ./bindings/go/cmd/collection-traversal-profile --arm materialized --entries 4096
go run ./bindings/go/cmd/collection-traversal-profile --arm stream --entries 65536 --batch-size 256
go run ./bindings/go/cmd/collection-traversal-profile --arm stream-cancel --entries 65536 --batch-size 64 --early-cancel 64
```"""
    elif g.name == "Swift":
        collection_section = """`Dictionary.entries()` returns a host-owned `EntrySnapshot` that safely
conforms to `RandomAccessCollection`. For bounded traversal, `entryStream()`
returns a throwing `EntryStream` with explicit `next`, `cancel`, and `close`;
native calls and arena copies use `withExtendedLifetime`, and `deinit` is a
cleanup safety net. Domain-tagged keys preserve raw bytes, Unicode scalar
values, and `UInt64` units while `UInt64?` preserves term-only membership."""
        benchmark_section = """
The SwiftPM executable keeps construction and warmup outside the timed drain
and prints one JSON record. Its materialized, streaming, and early-cancel arms
use the same 4,096/65,536-entry corpus as the Rust driver:

```sh
swift run --package-path bindings/swift/libdictenstein -c release libdictenstein-collection-profile --arm materialized --entries 4096
swift run --package-path bindings/swift/libdictenstein -c release libdictenstein-collection-profile --arm stream --entries 65536 --batch-size 256
swift run --package-path bindings/swift/libdictenstein -c release libdictenstein-collection-profile --arm stream-cancel --entries 65536 --batch-size 64 --early-cancel 64
```"""
    elif g.name == "Ruby":
        collection_section = """Every dictionary includes `Enumerable`. `each` returns an `Enumerator`
without a block and yields host-owned `Entry` records in lexical order; its
`ensure` path closes the cursor after exhaustion, `break`, or exception.
`entry_stream` also exposes explicit `next`, `cancel`, and `close`, while
`entries`, `keys`, and `values` provide materialized snapshot idioms. Binary
strings, UTF-8 strings, and `Array<Integer>` preserve the three unit domains;
`nil` remains distinct from every mapped integer."""
        benchmark_section = """
The gem executable keeps construction and warmup outside the timed drain and
prints one JSON record. Run its materialized, streaming, and early-cancel arms
over the shared deterministic corpus with:

```sh
ruby bindings/ruby/bin/libdictenstein-collection-profile --arm materialized --entries 4096
ruby bindings/ruby/bin/libdictenstein-collection-profile --arm stream --entries 65536 --batch-size 256
ruby bindings/ruby/bin/libdictenstein-collection-profile --arm stream-cancel --entries 65536 --batch-size 64 --early-cancel 64
```"""
    elif g.name == "Fortran":
        collection_section = """`dictionary%open_entries` returns a derived
`dictionary_entry_cursor` with bounded `next_batch`, cancellation, and explicit
`close`; copied batches preserve byte, Unicode-scalar, and full-range `int64`
bit patterns plus optional values. `dictionary%fold_entries` is the natural
synchronous reducer and settles every native lease before invoking subsequent
Fortran code. Status values remain explicit, and finalization is only a
last-resort cleanup path."""
    elif g.name == "OCaml":
        collection_section = """`with_entries_seq` supplies a scoped, repeatable `Seq.t`
whose keys are `Bytes`, UTF-8 `Unicode`, or raw-bit-pattern `U64` arrays and
whose values remain `int64 option`. `fold_entries` uses the native synchronous
reducer. `Fun.protect` closes after full drain, early return, or exception, and
the sequence cannot escape its owning callback; a finalizer only contains an
abandoned custom cursor."""
    elif g.name == "Haskell":
        collection_section = """`materializeEntries` returns a host-owned `Foldable`
snapshot. `withEntryStream` brackets a bounded cursor whose `nextEntry` values
own their `ByteString`, `Text`, or `Vector Word64` keys, while `foldEntries`
uses the native reducer without an intermediate list. Acquire/release is masked
against asynchronous exceptions, and every callback restores the caller's
masking state."""
    elif g.name == "Lua":
        collection_section = """`dictionary:entries()` materializes one immutable
revision for ordinary table iteration, while `dictionary:entries_iter()`
provides the idiomatic generic-for triple. `dictionary:entry_cursor(limits)`
exposes explicit `:next()`, metadata, and idempotent `:close()` for bounded
streaming. Lua 5.4 to-be-closed variables provide lexical cleanup and `__gc`
only contains abandoned userdata."""
    elif g.name == "Julia":
        collection_section = """Every dictionary is an `AbstractDict` whose key type follows its
byte, Unicode-scalar, or `UInt64`-token domain. Ordinary `iterate`, `keys`,
`values`, `haskey`, indexing, mutation, `merge`, `intersect`, and `setdiff`
therefore compose with Julia's standard collection algorithms. Iteration pins
one immutable retained snapshot and closes it at exhaustion or exception;
callers close a dictionary explicitly when its native lifetime ends."""
    elif g.name == "Raku":
        collection_section = """Every dictionary implements `Associative` and `Iterable`, so
postcircumfix lookup, `:exists`, assignment, deletion, `elems`, `Seq`, and
ordinary `for` loops use familiar Raku protocols. Iteration owns one immutable
retained snapshot and closes it after full drain. For an early stop, obtain
`iterator`, scope it with `LEAVE`, and call its idempotent `close`; `DESTROY`
only contains an abandoned iterator."""
    else:
        raise AssertionError(f"collection documentation is missing for {g.name}")
    return f"""{MARKER}

## Support and package contract

| Property | Contract |
|---|---|
| Binding | {g.name} |
| Languages/runtime | {g.runtimes} |
| Support tier | {g.tier} |
| Distribution | {g.package} |
| Native boundary | {g.boundary} |
| Canonical facade source | [`{g.source}`](../../{g.source}) |

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

The canonical checked example is [`{g.evidence}`](../../{g.evidence}). CI runs
the public package path with:

```sh
{g.command}
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

{collection_section}
{benchmark_section}

{algebra_section(g)}

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

{g.cleanup}

An exported resource is borrowed until its base-vtable `retain` succeeds. A
captured snapshot arrives owning one retain and may outlive the mutable
dictionary. Later inserts, updates, removals, compaction, or checkpoints do not
alter a pinned revision. Every successful retain has exactly one release, and
failed construction transfers no ownership.

## Errors and failure containment

{g.errors} Branch on the typed status or exception, not diagnostic text.
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

{END}
"""


def render(path: Path, guide: Guide) -> str:
    if path.exists():
        prefix = path.read_text(encoding="utf-8").split(MARKER, 1)[0].rstrip()
    else:
        prefix = f"# Vinary Tree libdictenstein for {guide.name}\n\nThis guide documents the supported {guide.name} facade over libdictenstein's stable dictionary ABI."
    return f"{prefix}\n\n{block(guide)}"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    stale: list[Path] = []
    for key, guide in GUIDES.items():
        path = ROOT / f"bindings/{key}/README.md"
        output = render(path, guide)
        if args.check:
            if not path.exists() or path.read_text(encoding="utf-8") != output:
                stale.append(path.relative_to(ROOT))
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(output, encoding="utf-8")
    if stale:
        raise SystemExit("stale binding guides:\n" + "\n".join(f"  - {p}" for p in stale))


if __name__ == "__main__":
    main()
