# Vinary Tree libdictenstein for Swift

A Swift Package Manager facade over libdictenstein's stable `ldict_*` C ABI,
importing the C header through the `CLibdictenstein` system-library target. The
development package lives here; the root [`Package.swift`](../../Package.swift)
declares the distributable `Libdictenstein` package and product.

## Native library

The facade links the shared library `libdictenstein`. Build it and expose it to
the linker and loader:

```sh
cargo build --release --no-default-features --features ffi
export LIBRARY_PATH="$PWD/target/release:$LIBRARY_PATH"
export LD_LIBRARY_PATH="$PWD/target/release:$LD_LIBRARY_PATH"
```

## Quickstart

```swift
import Libdictenstein

let dictionary = try DynamicDAWG()               // .unicodeScalar by default
try dictionary.put("cat", value: 1)
try dictionary.put("cot", value: 2)
try dictionary.put("cut")                         // valueless term
let hit = try dictionary.get("cot")
assert(hit.found && hit.value == 2)

let suffixes = try SCDAWG()
try suffixes.put("cat")
try suffixes.put("cot")
assert(try suffixes.containsSubstring("ot"))
assert(try suffixes.substringFrequency("t") == 2)
```

Each dictionary is a class; `deinit` frees the native handle, and `close()` is
an idempotent early release.

`entries()` materializes one immutable revision as a `RandomAccessCollection`:

```swift
let snapshot = try dictionary.entries()
for entry in snapshot {
    print(entry.key, entry.value as Any)
}

let stream = try dictionary.entryStream(
    limits: EntryBatchLimits(maxEntries: 64, maxUnits: 4096, maxValues: 64)
)
defer { try? stream.close() }
while let entry = try stream.next() {
    if entry.key.string == "cot" { try stream.cancel(); break }
}
```

Both forms copy keys before releasing a native batch. Use `put(bytes:)` and
`get(bytes:)` for arbitrary byte-domain terms.

## Backends

| Class | Unit domains | Notes |
|-------|--------------|-------|
| `DynamicDAWG` | `.byte`, `.unicodeScalar`, `.u64` | full CRUD (`put`/`remove`/`clear`/`compact`) |
| `DoubleArrayTrie` | `.byte`, `.unicodeScalar` | immutable, built from one entry batch |
| `SCDAWG` | `.byte`, `.unicodeScalar` | `put`, `containsSubstring`, `substringFrequency` |
| `PersistentARTrie` | `.byte`, `.unicodeScalar`, `.u64` | `create`/`open`, CRUD, `checkpoint` |

Text and u64 terms are both accepted (`String` and `[UInt64]`); `put`, `get`,
and `remove` are overloaded on the term type. `count` is a throwing property.

## Values and domains

`String` terms are passed as UTF-8; the Unicode-scalar backends validate it.
Values are `UInt64?` over the full `0 ... UInt64.max` range; `nil` and `0` are
distinct. `Lookup` carries `found: Bool` and `value: UInt64?`.

## Error handling

Fallible calls `throw` `LibdictensteinError`, whose `description` is the
thread-local `ldict_last_error_message()`. Backend-unsupported operations
surface the `UNSUPPORTED` status; wrong-domain terms surface `DOMAIN_MISMATCH`
(9).

## Retained resource handoff

`withVtResource { pointer in ... }` borrows the shared `VtResource` for one
synchronous retaining call, so a liblevenshtein transducer retains the
dictionary in constant time and keeps its query-start revision valid after
`close`.

## Coverage note

This facade currently binds the constructor, CRUD, maintenance, and substring
surface. The `kind`/`capabilities` accessors, the contiguous batch inserts, and
the persistent vocabulary backend are not yet wrapped; use another facade (or
the C ABI directly) when those are required.

<!-- BEGIN GENERATED BINDING OPERATIONS; DO NOT EDIT -->

## Support and package contract

| Property | Contract |
|---|---|
| Binding | Swift |
| Languages/runtime | Swift 6+ |
| Support tier | Tier 2 |
| Distribution | SwiftPM `Libdictenstein` |
| Native boundary | Swift system-library target over the C ABI |
| Canonical facade source | [`bindings/swift/libdictenstein/Sources/Libdictenstein`](../../bindings/swift/libdictenstein/Sources/Libdictenstein) |

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

The canonical checked example is [`bindings/swift/libdictenstein/Tests/LibdictensteinTests/ConformanceTests.swift`](../../bindings/swift/libdictenstein/Tests/LibdictensteinTests/ConformanceTests.swift). CI runs
the public package path with:

```sh
swift test --package-path bindings/swift/libdictenstein
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

`Dictionary.entries()` returns a host-owned `EntrySnapshot` that safely
conforms to `RandomAccessCollection`. For bounded traversal, `entryStream()`
returns a throwing `EntryStream` with explicit `next`, `cancel`, and `close`;
native calls and arena copies use `withExtendedLifetime`, and `deinit` is a
cleanup safety net. Domain-tagged keys preserve raw bytes, Unicode scalar
values, and `UInt64` units while `UInt64?` preserves term-only membership.

The SwiftPM executable keeps construction and warmup outside the timed drain
and prints one JSON record. Its materialized, streaming, and early-cancel arms
use the same 4,096/65,536-entry corpus as the Rust driver:

```sh
swift run --package-path bindings/swift/libdictenstein -c release libdictenstein-collection-profile --arm materialized --entries 4096
swift run --package-path bindings/swift/libdictenstein -c release libdictenstein-collection-profile --arm stream --entries 65536 --batch-size 256
swift run --package-path bindings/swift/libdictenstein -c release libdictenstein-collection-profile --arm stream-cancel --entries 65536 --batch-size 64 --early-cancel 64
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

```swift
let joined = try left.union(right, valueMerge: .latticeJoin)
defer { joined.close() }
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

Use lexical `defer` and explicit `close`; `deinit` timing is not a persistence guarantee.

An exported resource is borrowed until its base-vtable `retain` succeeds. A
captured snapshot arrives owning one retain and may outlive the mutable
dictionary. Later inserts, updates, removals, compaction, or checkpoints do not
alter a pinned revision. Every successful retain has exactly one release, and
failed construction transfers no ownership.

## Errors and failure containment

C statuses become throwing Swift errors preserving diagnostics. Branch on the typed status or exception, not diagnostic text.
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
