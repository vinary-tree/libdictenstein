# Vinary Tree libdictenstein for Ruby

The gem exposes full DynamicDAWG CRUD, immutable DoubleArrayTrie construction,
SCDAWG substring search, persistent ARTrie CRUD/checkpoint/reopen, and persistent
vocabulary reverse lookup. Every object implements `with_resource`, allowing an
independently packaged liblevenshtein transducer to retain it in O(1).

Calls acquire only a short lifetime lease; operations on the same dictionary
are not serialized. The project-owned native resource advertises parallel and
reentrant access automatically.

Every dictionary is an `Enumerable` over immutable-revision `Entry` records:

```ruby
dictionary = VinaryTree::Libdictenstein::DynamicDawg.new
dictionary.put("cat", 0)
dictionary.put("cut", nil)

dictionary.each do |entry|
  p [entry.key, entry.value]
  break if entry.key == "cat" # ensure closes the native cursor
end

keys = dictionary.keys
values = dictionary.values
snapshot = dictionary.entries
```

`each` returns an `Enumerator` without a block. `entry_stream` exposes manual
`next`, `cancel`, and `close` for pull-driven bounded traversal.

<!-- BEGIN GENERATED BINDING OPERATIONS; DO NOT EDIT -->

## Support and package contract

| Property | Contract |
|---|---|
| Binding | Ruby |
| Languages/runtime | Ruby 3.3+ |
| Support tier | Tier 2 |
| Distribution | RubyGems `libdictenstein` |
| Native boundary | Fiddle over the stable C ABI |
| Canonical facade source | [`bindings/ruby/lib/vinary_tree/libdictenstein`](../../bindings/ruby/lib/vinary_tree/libdictenstein) |

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

The canonical checked example is [`bindings/ruby/test/test_conformance.rb`](../../bindings/ruby/test/test_conformance.rb). CI runs
the public package path with:

```sh
ruby -Ibindings/ruby/lib bindings/ruby/test/test_conformance.rb
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

Every dictionary includes `Enumerable`. `each` returns an `Enumerator`
without a block and yields host-owned `Entry` records in lexical order; its
`ensure` path closes the cursor after exhaustion, `break`, or exception.
`entry_stream` also exposes explicit `next`, `cancel`, and `close`, while
`entries`, `keys`, and `values` provide materialized snapshot idioms. Binary
strings, UTF-8 strings, and `Array<Integer>` preserve the three unit domains;
`nil` remains distinct from every mapped integer.

The gem executable keeps construction and warmup outside the timed drain and
prints one JSON record. Run its materialized, streaming, and early-cancel arms
over the shared deterministic corpus with:

```sh
ruby bindings/ruby/bin/libdictenstein-collection-profile --arm materialized --entries 4096
ruby bindings/ruby/bin/libdictenstein-collection-profile --arm stream --entries 65536 --batch-size 256
ruby bindings/ruby/bin/libdictenstein-collection-profile --arm stream-cancel --entries 65536 --batch-size 64 --early-cancel 64
```

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

```ruby
joined = left.union(right, value_merge: LD::ValueMerge::LATTICE_JOIN)
begin
  puts joined.length
ensure
  joined.close
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

Prefer block forms or `ensure { dictionary.close }`; close persistent stores explicitly.

An exported resource is borrowed until its base-vtable `retain` succeeds. A
captured snapshot arrives owning one retain and may outlive the mutable
dictionary. Later inserts, updates, removals, compaction, or checkpoints do not
alter a pinned revision. Every successful retain has exactly one release, and
failed construction transfers no ownership.

## Errors and failure containment

Failures become typed Ruby exceptions with status and diagnostic. Branch on the typed status or exception, not diagnostic text.
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
