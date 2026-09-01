# Libdictenstein for Raku

High-performance dictionaries and trie-maps for approximate string matching,
with Raku-native `Associative` and `Iterable` behavior. `Libdictenstein`
exposes DynamicDAWG, DoubleArrayTrie, SCDAWG, persistent ARTrie, and persistent
vocabulary backends through NativeCall while `Vinary-Tree-Interop` owns the
retained snapshot boundary.

## Quick start

Install the `Libdictenstein` distribution with Zef. From a source checkout,
build the native library and run the public conformance program with:

```sh
cargo build --release --no-default-features --features ffi
LIBDICTENSTEIN_LIBRARY="$PWD/target/release/liblibdictenstein.so" \
  raku -Ibindings/raku/lib -I../vinary-tree-interop/bindings/raku/lib \
  bindings/raku/t/01-conformance.rakutest
```

```raku
use Libdictenstein;

my $words = dynamic-dawg;
LEAVE $words.close;
$words{'colour'} = 17;
$words{'color'} = Nil;
die 'missing member' unless $words{'color'}:exists;
die 'wrong value' unless $words{'colour'} == 17;
say $words.list;
```

The complete executable contract is
[`bindings/raku/t/01-conformance.rakutest`](t/01-conformance.rakutest).

## Snapshots, iteration, and algebra

Ordinary full traversal creates one immutable revision and closes its bounded
entry cursor at exhaustion. Code that may stop early obtains an iterator
explicitly and scopes its idempotent `close` with `LEAVE`:

```raku
my $iterator = $words.iterator;
LEAVE $iterator.close;
say $iterator.pull-one;
```

Union, intersection, difference, and symmetric difference execute natively.
For ordered entry streams `A` and `B`, their work is linear in the combined
number of members:

```math
T_{\mathrm{algebra}}(|A|,|B|)=\Theta(|A|+|B|).
```

```raku
my $left = dynamic-dawg;
my $right = dynamic-dawg;
LEAVE { $left.close; $right.close }
$left{'shared'} = 4;
$right{'shared'} = 9;
my $joined = $left.union($right, merge => VALUE-MERGE-LATTICE-JOIN);
LEAVE $joined.close;
die 'join failed' unless $joined{'shared'} == 9;
```

The native boundary copies diagnostics before the next call, rejects invalid
UTF-8 and out-of-range `UInt` values, contains Rust panics as status values,
and packs batch descriptors into one inline C arena to avoid per-key foreign
calls.

<!-- BEGIN GENERATED BINDING OPERATIONS; DO NOT EDIT -->

## Support and package contract

| Property | Contract |
|---|---|
| Binding | Raku |
| Languages/runtime | Rakudo 2025.01+ |
| Support tier | Tier 3 |
| Distribution | Zef `Libdictenstein` |
| Native boundary | NativeCall over the stable C ABI plus `Vinary-Tree-Interop` snapshots |
| Canonical facade source | [`bindings/raku/lib/Libdictenstein.rakumod`](../../bindings/raku/lib/Libdictenstein.rakumod) |

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

The canonical checked example is [`bindings/raku/t/01-conformance.rakutest`](../../bindings/raku/t/01-conformance.rakutest). CI runs
the public package path with:

```sh
raku -Ibindings/raku/lib -I../vinary-tree-interop/bindings/raku/lib bindings/raku/t/01-conformance.rakutest
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

Every dictionary implements `Associative` and `Iterable`, so
postcircumfix lookup, `:exists`, assignment, deletion, `elems`, `Seq`, and
ordinary `for` loops use familiar Raku protocols. Iteration owns one immutable
retained snapshot and closes it after full drain. For an early stop, obtain
`iterator`, scope it with `LEAVE`, and call its idempotent `close`; `DESTROY`
only contains an abandoned iterator.


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

```raku
my $joined = $left.union($right, merge => VALUE-MERGE-LATTICE-JOIN);
LEAVE $joined.close;
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

Call `close` in `LEAVE`/`CATCH` paths; `DESTROY` is fallback containment, and explicitly close iterators after early termination.

An exported resource is borrowed until its base-vtable `retain` succeeds. A
captured snapshot arrives owning one retain and may outlive the mutable
dictionary. Later inserts, updates, removals, compaction, or checkpoints do not
alter a pinned revision. Every successful retain has exactly one release, and
failed construction transfers no ownership.

## Errors and failure containment

Native statuses become `X::Libdictenstein` exceptions containing the exact operation and copied diagnostic. Branch on the typed status or exception, not diagnostic text.
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
