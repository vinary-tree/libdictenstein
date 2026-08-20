# Bindings — the producer contract corpus

**Navigation**: [← Documentation index](../README.md)

libdictenstein is the **producer** half of the family's dictionary ABI: it
owns the concrete dictionaries and their CRUD, exports a 35-function `ldict_*`
C surface, and hands consumers a two-word, retained `vt.dictionary.v1`
resource whose snapshots they walk. The **consumer** half (cursor model, lease
protocol, language-facade query APIs) lives in
[liblevenshtein](https://github.com/vinary-tree/liblevenshtein-rust) — this
corpus documents everything on the producing side of that boundary.

## What lives where

| Artifact | Path | What it is |
|---|---|---|
| **C ABI reference** | [`c-abi-reference.md`](c-abi-reference.md) | The normative reference for all 35 `ldict_*` functions: exact signatures, preconditions, exact status sets, ownership, thread-safety, complexity; the status/kind/capability tables; the per-backend support matrix; persistence caveats; a compile-and-run-verified C example. |
| **Resource-producer architecture** | [`resource-producer.md`](resource-producer.md) | How the producer side works: the four backend bindings, `OwnedDictionaryResource` and the retain ledger, per-backend $`\mathcal{O}(1)`$ snapshot capture, lazy ABI-local node ids, the flag truth table, and the new-backend checklist. |
| **Native Rust idioms** | [`rust-api-idioms.md`](rust-api-idioms.md) | Confirmed iterator/construction gaps in the pure Rust producer and the optimized, generic target for `Iterator`, `IntoIterator`, `FromIterator`, `Extend`, fallible bulk construction, folds, snapshots, and every automaton/unit domain. |
| **FFI boundary analysis** | [`../security/ffi-boundary.md`](../security/ffi-boundary.md) | The producer-side trust analysis: what a misbehaving foreign caller can and cannot cause, and whose duty each defense is. Extends the [threat model](../security/threat-model.md). |
| **Findings ledger** | [`FINDINGS_LEDGER.md`](FINDINGS_LEDGER.md) | The scientific ledger of binding-scrutiny findings: defects, pins, coverage gaps, and version-pin inconsistencies (`LDICT-B<N>` schema). |
| **Machine-readable model** | [`../../bindings/api.json`](../../bindings/api.json) | The source of truth for the binding surface: symbols, enums, kinds, capabilities, marshalling and snapshot laws, facade layout, registry coordinates. |
| **Contract gates** | [`../../scripts/check-bindings.py`](../../scripts/check-bindings.py), [`../../scripts/check-binding-docs.py`](../../scripts/check-binding-docs.py) | Enforce the ABI model and reject a declared facade whose guide, executable evidence, required operational topics, or local links are missing or stale. CI job `binding-contract`. |
| **Diagrams** | [`../diagrams/`](../diagrams/) | `abi-producer-component` (layer map), `snapshot-capture-sequence` (the walk protocol), `owned-resource-lifecycle-state` (the retain ledger); sources under [`../diagrams/src/`](../diagrams/src/). |
| **Language facades** | [`../../bindings/`](../../bindings/) | Fourteen governed guides over the `ldict_*` surface, including the native C contract and grouped JVM/JavaScript language families. |
| **Guide generator** | [`../../scripts/generate-binding-guides.py`](../../scripts/generate-binding-guides.py) | Owns the uniform support, loading, ownership, error, concurrency, performance, security, compatibility, and maintainer sections while preserving each facade's handwritten tutorial. |

Collection interfaces are a planned parity feature, not yet a blanket claim of
the packages in the matrix. The family
[collection-protocol design](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/bindings/collection-protocols.md)
makes the optimized [pure Rust surface](rust-api-idioms.md) the baseline, then
maps it to each ecosystem's familiar collection and lifetime idioms. A shared
engine is required; a lowest-common-denominator public API is not.

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

- [ABI reference — `vinary_tree_interop.h`, annotated](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/abi-reference.md)
- [ABI evolution policy — the four version counters](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/abi-evolution.md)
- [Family security model — trust zones and validation duties](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/vinary-tree-interop/docs/security-model.md)
- [liblevenshtein language-binding architecture (the consumer side)](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/language-bindings.md)
