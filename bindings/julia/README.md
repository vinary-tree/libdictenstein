# Libdictenstein for Julia

High-performance dictionaries and trie-maps for approximate string matching,
presented as Julia `AbstractDict` implementations rather than C-shaped handles.
The `Libdictenstein` package covers DynamicDAWG, DoubleArrayTrie, SCDAWG,
persistent ARTrie, and persistent vocabulary backends across byte, Unicode
scalar, and `UInt64` token domains.

## Quick start

Install `Libdictenstein` and its `VinaryTreeInterop` dependency through Julia's
package manager. A source checkout can select a locally built native library:

```sh
export LIBDICTENSTEIN_LIBRARY="$PWD/target/release/liblibdictenstein.so"
julia --project=bindings/julia/Libdictenstein -e 'using Pkg; Pkg.test()'
```

```julia
using Libdictenstein

words = DynamicDawg()
try
    words["colour"] = UInt64(17)
    words["color"] = nothing
    @assert haskey(words, "color")
    @assert words["colour"] == 17
    @assert collect(keys(words)) == ["color", "colour"]
finally
    close(words)
end
```

The complete executable contract is
[`bindings/julia/Libdictenstein/test/runtests.jl`](Libdictenstein/test/runtests.jl).
Package-specific API pages live under
[`bindings/julia/Libdictenstein/docs/src/`](Libdictenstein/docs/src/).

## Generated ABI boundary

Julia's low-level constants, enums, layouts, and 42 typed native calls are
generated from the signature and lifetime records in
[`bindings/api.json`](../api.json). The public C header remains an independent
parity oracle: generation fails if a return type, parameter type, parameter
name, symbol, ABI version, or API revision differs between the model and
[`include/libdictenstein.h`](../../include/libdictenstein.h). The high-level
`AbstractDict` facade calls only these generated wrappers; handwritten `ccall`
sites outside the delimited generated region are rejected.

The reviewable
[`julia-abi-capabilities.tsv`](../generated/julia-abi-capabilities.tsv)
records every symbol's group, feature gate, C signature, parameter direction,
ownership rule, Julia wrapper, Julia types, ABI version, and API revision. Run
the freshness and mutation-based negative controls with:

```sh
python3 scripts/generate-julia-abi.py --check
python3 scripts/generate-julia-abi.py --self-test
```

After an intentional model change, regenerate with `--write`, inspect both the
Julia diff and inventory diff, and then run the two commands above. The
self-test proves that duplicate symbols, invalid flow directions, header
signature drift, and an injected handwritten `ccall` are detected.

## Snapshot algebra

`union`, `intersection`, `difference`, and `symmetric_difference` capture one
immutable revision of each input. If `A` and `B` are their lexicographically
ordered entry streams, the native engine performs a linear merge:

```math
T_{\mathrm{algebra}}(|A|,|B|)=\Theta(|A|+|B|).
```

Mapped values use explicit first, last, lattice-join, or lattice-meet policies;
valueless membership is distinct from absence and from the value zero. The
result is an independently mutable DynamicDAWG frozen directly from the sorted
stream, without a Julia hash-table round trip.

```julia
left = DynamicDawg()
right = DynamicDawg()
left["shared"] = 4
right["shared"] = 9
joined = algebra(left, right, ALGEBRA_UNION, VALUE_MERGE_LATTICE_JOIN)
try
    @assert joined["shared"] == 9
finally
    close(joined); close(left); close(right)
end
```

<!-- BEGIN GENERATED BINDING OPERATIONS; DO NOT EDIT -->

## Support and package contract

| Property | Contract |
|---|---|
| Binding | Julia |
| Languages/runtime | Julia 1.10+ |
| Support tier | Tier 2 |
| Distribution | Julia General `Libdictenstein` |
| Native boundary | `ccall` over the stable C ABI plus `VinaryTreeInterop` snapshots |
| Canonical facade source | [`bindings/julia/Libdictenstein/src/Libdictenstein.jl`](../../bindings/julia/Libdictenstein/src/Libdictenstein.jl) |

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

The canonical checked example is [`bindings/julia/Libdictenstein/test/runtests.jl`](../../bindings/julia/Libdictenstein/test/runtests.jl). CI runs
the public package path with:

```sh
julia --project=bindings/julia/Libdictenstein -e 'using Pkg; Pkg.test()'
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

Every dictionary is an `AbstractDict` whose key type follows its
byte, Unicode-scalar, or `UInt64`-token domain. Ordinary `iterate`, `keys`,
`values`, `haskey`, indexing, mutation, `merge`, `intersect`, and `setdiff`
therefore compose with Julia's standard collection algorithms. Iteration pins
one immutable retained snapshot and closes it at exhaustion or exception;
callers close a dictionary explicitly when its native lifetime ends.


## Snapshot-consistent dictionary algebra

Every facade exposes native union, intersection, left difference, and
symmetric difference. The operation captures one immutable revision from each
input; those two captures are independent, and later mutations cannot alter
the result. Inputs must use the same byte, Unicode-scalar, or `u64` term
domain.

The producer merges the two lexicographically ordered entry streams once and
feeds the sorted, duplicate-free output directly to the DynamicDAWG
freeze-once builder. For input cardinalities $`|A|`$ and $`|B|`$, this is
$`\Theta(|A|+|B|)`$ work plus $`\Theta(|R|)`$ result storage. It avoids a
host-language hash table, per-entry foreign calls, and repeated mutable graph
publication. The returned DynamicDAWG is independently mutable.

Keys present in both inputs use an explicit optional-`u64` value policy:
left/first, right/last, lattice join (optional maximum), or lattice meet
(shared optional minimum). Valueless membership remains distinct from absence
and from the value zero. Union defaults to right/last and intersection defaults
to lattice meet; difference operations have no overlapping output key, so a
value policy cannot affect them.

```julia
joined = algebra(left, right, ALGEBRA_UNION, VALUE_MERGE_LATTICE_JOIN)
try
    println(length(joined))
finally
    close(joined)
end
```


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

Call `close` in `finally`; finalizers contain abandoned handles but do not define resource lifetime.

An exported resource is borrowed until its base-vtable `retain` succeeds. A
captured snapshot arrives owning one retain and may outlive the mutable
dictionary. Later inserts, updates, removals, compaction, or checkpoints do not
alter a pinned revision. Every successful retain has exactly one release, and
failed construction transfers no ownership.

## Errors and failure containment

Native statuses become `NativeError` values containing the exact operation and copied diagnostic. Branch on the typed status or exception, not diagnostic text.
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
