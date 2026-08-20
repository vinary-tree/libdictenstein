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
    "python": Guide("Python", "Python 3.10+", "Tier 1", "PyPI `vinary-tree-libdictenstein`", "`ctypes` over the stable C ABI", "Use dictionaries as context managers or call `close()` in `finally`; finalizers are fallback containment.", "Native statuses become typed Python exceptions preserving the diagnostic.", "bindings/python/src/libdictenstein", "bindings/python/tests/test_backends.py", "PYTHONPATH=bindings/python/src pytest -q bindings/python/tests"),
    "jvm": Guide("JVM", "Java 22+, Kotlin, and Scala", "Tier 1", "Maven `io.vinarytree:libdictenstein`", "Java FFM over the stable C ABI", "Use try-with-resources, Kotlin `use`, or Scala `Using`; `Cleaner` is not deterministic cleanup.", "Native statuses become typed JVM exceptions with symbolic status and diagnostic.", "bindings/jvm/src/main/java/io/vinarytree/libdictenstein", "bindings/jvm/src/test/java/io/vinarytree/libdictenstein/BackendIntegrationTest.java", "./gradlew -p bindings/jvm test"),
    "clojure": Guide("Clojure", "Clojure 1.12+ on Java 22+", "Tier 1", "Clojars `io.vinarytree/libdictenstein-clojure`", "Idiomatic namespace delegating to the JVM facade", "Use `with-open`; close persistent stores before process teardown so checkpoints and descriptors are deterministic.", "JVM failures become `ExceptionInfo` with structured status data.", "bindings/clojure/src/vinary_tree/libdictenstein.clj", "bindings/clojure/test/vinary_tree/libdictenstein_test.clj", "clojure -M:test -m vinary-tree.libdictenstein-test-runner"),
    "javascript": Guide("JavaScript family", "JavaScript, TypeScript, and ClojureScript", "Tier 1", "npm `@vinary-tree/libdictenstein`", "Facade on the singleton `@vinary-tree/vinary-tree` runtime", "Use `using` or `close()`/`close!` in `finally`; never rely on GC to flush persistent state.", "Failures become structured `VinaryTreeError` instances.", "bindings/javascript", "bindings/javascript/test/facades.test.mjs", "npm test --prefix bindings/javascript"),
    "dotnet": Guide(".NET", ".NET 8+ / C#", "Tier 2", "NuGet `VinaryTree.Libdictenstein`", "P/Invoke and `VinaryTree.Interop` retained resources", "Use `using`; `SafeHandle` covers exceptional paths but not prompt checkpoint/close policy.", "Failures become typed .NET exceptions containing status and diagnostic.", "bindings/dotnet/src/VinaryTree.Libdictenstein", "bindings/dotnet/tests/VinaryTree.Libdictenstein.Tests/Program.cs", "dotnet run --project bindings/dotnet/tests/VinaryTree.Libdictenstein.Tests"),
    "go": Guide("Go", "Go 1.25+ with cgo", "Tier 2", "Go module `github.com/vinary-tree/libdictenstein/bindings/go`", "cgo over `ldict_*`", "Call `Close` with `defer` immediately after construction; finalizers only report abandoned handles.", "Operations return inspectable Go errors with native status and diagnostic.", "bindings/go/libdictenstein.go", "bindings/go/libdictenstein_test.go", "go test ./bindings/go/..."),
    "swift": Guide("Swift", "Swift 6+", "Tier 2", "SwiftPM `VinaryTreeLibdictenstein`", "Swift system-library target over the C ABI", "Use lexical `defer` and explicit `close`; `deinit` timing is not a persistence guarantee.", "C statuses become throwing Swift errors preserving diagnostics.", "bindings/swift/libdictenstein/Sources/Libdictenstein", "bindings/swift/libdictenstein/Tests/LibdictensteinTests/ConformanceTests.swift", "swift test --package-path bindings/swift/libdictenstein"),
    "ruby": Guide("Ruby", "Ruby 3.3+", "Tier 2", "RubyGems `vinary-tree-libdictenstein`", "Fiddle over the stable C ABI", "Prefer block forms or `ensure { dictionary.close }`; close persistent stores explicitly.", "Failures become typed Ruby exceptions with status and diagnostic.", "bindings/ruby/lib/vinary_tree/libdictenstein", "bindings/ruby/test/test_conformance.rb", "ruby -Ibindings/ruby/lib bindings/ruby/test/test_conformance.rb"),
    "fortran": Guide("Fortran", "Fortran 2018", "Tier 2", "fpm `vinary-tree-libdictenstein`", "`iso_c_binding` over `ldict_*`", "Call the derived handle's `close`; final procedures are last-resort cleanup.", "Procedures preserve both the project status and native diagnostic.", "bindings/fortran/src/vinary_tree_libdictenstein.f90", "bindings/fortran/test/conformance.f90", "fpm test --profile release --directory bindings/fortran"),
    "ocaml": Guide("OCaml", "OCaml 5", "Tier 3", "opam `vinary-tree-libdictenstein`", "C stubs over the stable ABI", "Use explicit close functions or `Fun.protect`; finalizers only protect abandoned values.", "Statuses become typed OCaml exceptions with copied diagnostics.", "bindings/ocaml/vinary_tree_libdictenstein.mli", "bindings/ocaml/test/conformance.ml", "opam exec -- dune runtest --root bindings/ocaml"),
    "haskell": Guide("Haskell", "GHC/Cabal", "Tier 3", "Hackage `vinary-tree-libdictenstein`", "Haskell FFI plus retained interop resources", "Use `bracket`/`withDictionary`; mask asynchronous exceptions across acquire/release.", "Failures become typed Haskell exceptions/status values.", "bindings/haskell/src/VinaryTree/Libdictenstein.hs", "bindings/haskell/test/Conformance.hs", "cabal test --project-file=bindings/haskell/cabal.project all"),
    "lua": Guide("Lua", "Lua 5.4+", "Tier 3", "LuaRocks `vinary-tree-libdictenstein`", "C userdata module over the ABI", "Use to-be-closed variables or `:close()`; `__gc` is fallback cleanup.", "Failures become Lua errors carrying the symbolic status and diagnostic.", "bindings/lua/src/libdictenstein_lua.c", "bindings/lua/test/conformance.lua", "lua bindings/lua/test/conformance.lua"),
}

COLLECTION_IDIOMS = {
    "C": "a bounded entry-batch cursor plus callback reducer; C has no standard collection protocol",
    "C++": "RAII snapshot ranges, input iterators, and sized ranges only when cardinality is exact",
    "Python": "read-only collections.abc Set/Mapping views plus a context-managed streaming iterator",
    "JVM": "Java Set/Map/Iterable/Spliterator/Stream views, Kotlin collections/Sequence, and Scala collections/Iterator",
    "Clojure": "reducible and sequence views whose resource-scoped forms use with-open",
    "JavaScript family": "iterable immutable views and an async iterator only for genuinely asynchronous runtimes",
    ".NET": "IReadOnlySet/IReadOnlyDictionary/IEnumerable views and a using-scoped streaming enumerator",
    "Go": "range-compatible iterator functions and an explicit cancellable streaming form",
    "Swift": "Sequence views and a closeable streaming iterator",
    "Ruby": "Enumerable with each, including deterministic cleanup after break",
    "Fortran": "counted batches and callback/fold procedures with explicit status results",
    "OCaml": "Seq/fold adapters and a resource-scoped streaming fold",
    "Haskell": "Foldable-style materialized views and bracketed streaming folds",
    "Lua": "pairs-style materialized iteration and an explicitly closeable stream",
}


def block(g: Guide) -> str:
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

## Native collection idioms and planned parity

The current facade exposes lookup, length, mutation, and deterministic resource
ownership, but does **not** yet claim the complete host collection protocol.
The planned native shape for this runtime is {COLLECTION_IDIOMS[g.name]}. The
ordinary collection view will own host data from one immutable revision, while
the large-dictionary stream will retain one bounded native snapshot and require
lexical cleanup. Membership remains a direct lookup, never an iteration scan.

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
[family security model](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/security-model.md).

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
