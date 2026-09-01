# Vinary Tree libdictenstein for Go

A cgo facade over libdictenstein's stable `ldict_*` C ABI. The module is
`github.com/vinary-tree/libdictenstein/bindings/go/v4`. It exposes DynamicDAWG
CRUD and batch insertion, immutable DoubleArrayTrie construction, SCDAWG
substring search, persistent ARTrie CRUD/checkpoint/reopen, and persistent
vocabulary reverse lookup, and lends each dictionary's retained resource to an
independent liblevenshtein transducer without serialization.

## Building the native library

cgo links `libdictenstein` (file name `liblibdictenstein.so`):

```sh
cargo build --release --no-default-features --features ffi
```

The `#cgo` directives in `libdictenstein.go` already add the in-repo header
roots. Point the linker and loader at the built library:

```sh
export CGO_LDFLAGS="-L$PWD/target/release"
export LD_LIBRARY_PATH="$PWD/target/release:$LD_LIBRARY_PATH"
```

The workspace file [`go.work`](../../go.work) wires this module to the sibling
interop and liblevenshtein Go modules for local development.

## Quickstart

```go
package main

import (
	"fmt"

	ld "github.com/vinary-tree/libdictenstein/bindings/go/v4"
)

// A mapped u64 value is a *uint64, so nil and 0 stay distinct.
func value(v uint64) *uint64 { return &v }

func main() {
	dictionary, err := ld.NewDynamicDawg(ld.UnicodeScalarDomain)
	if err != nil {
		panic(err)
	}
	defer dictionary.Close()

	_, _ = dictionary.PutAll([]ld.Entry{
		{Term: "cat", Value: value(1)},
		{Term: "cot", Value: value(2)},
		{Term: "cut"}, // valueless
	})

	hit, _ := dictionary.Get("cot")
	fmt.Println(hit.Found, *hit.Value) // true 2

	suffixes, _ := ld.NewScdawg(ld.UnicodeScalarDomain)
	defer suffixes.Close()
	_, _ = suffixes.Put("cat", nil)
	_, _ = suffixes.Put("cot", nil)
	present, _ := suffixes.ContainsSubstring("ot")
	frequency, _ := suffixes.SubstringFrequency("t")
	fmt.Println(present, frequency) // true 2
}
```

`Close` is idempotent and a finalizer frees any handle a caller forgets, so a
double `Close` is safe.

Materialize a stable revision with `SnapshotEntries`, or range over bounded
copied batches with the Go 1.23 iterator helper:

```go
stream, err := dictionary.OpenEntryStream(ld.DefaultEntryBatchLimits)
if err != nil { panic(err) }
for entry, err := range stream.Seq2() {
	if err != nil { panic(err) }
	fmt.Println(entry.Text, entry.Value)
	if entry.Text == "cot" { break } // releases and closes through defer
}
```

Call `Next` with `defer stream.Close()` when manual pull iteration is more
convenient. `Cancel` is the explicit early-exit spelling.

## Backends and capabilities

| Constructor | Kind constant | Unit domains | Capabilities |
|-------------|---------------|--------------|--------------|
| `NewDynamicDawg` | `DynamicDawgKind` | byte, unicode-scalar, u64 | read, insert, remove, clear, compact |
| `NewDoubleArrayTrie` | `DoubleArrayTrieKind` | byte, unicode-scalar | read (immutable) |
| `NewScdawg` | `ScdawgKind` | byte, unicode-scalar | read, insert, substring |
| `CreatePersistentArtrie` / `OpenPersistentArtrie` | `PersistentArtrieKind` | byte, unicode-scalar, u64 | read, insert, remove, checkpoint |
| `CreatePersistentVocabulary` / `OpenPersistentVocabulary` | `PersistentVocabularyKind` | unicode-scalar | read, insert, checkpoint |

`Capabilities()` returns the `Can*` bitset; `Kind()` returns the backend
constant.

## Text domains and values

`string` terms are passed as UTF-8 bytes. The Unicode-scalar backends validate
UTF-8 and return an error with status `INVALID_UTF8` on invalid input; the byte
domain accepts arbitrary bytes. The u64 API (`PutU64`, `ContainsU64`, `GetU64`,
`RemoveU64`) takes `[]uint64` terms. Values are `*uint64` over the full
`0 .. math.MaxUint64` range; a `nil` value and a value of `0` are distinct.
`Lookup` reports `Found` and an optional `Value`.

## Error handling

Every fallible call returns an `error`. A native failure is an `*Error` whose
message is the thread-local `ldict_last_error_message()` and whose `Status`
field is the numeric `LdictStatus`. Operations a backend does not support return
`UNSUPPORTED`; a term submitted to the wrong unit domain returns
`DOMAIN_MISMATCH` (9).

## Retained resource handoff

`WithResource(func(context, vtable uintptr) error)` borrows the shared resource
for one synchronous retaining call. A liblevenshtein transducer built over the
dictionary retains it in constant time; a query it started keeps the immutable
revision visible at its start even after `Close`.

## Testing

```sh
go test ./bindings/go/...
```

with `CGO_LDFLAGS` and `LD_LIBRARY_PATH` set as above. See
[`libdictenstein_test.go`](libdictenstein_test.go).

<!-- BEGIN GENERATED BINDING OPERATIONS; DO NOT EDIT -->

## Support and package contract

| Property | Contract |
|---|---|
| Binding | Go |
| Languages/runtime | Go 1.25+ with cgo |
| Support tier | Tier 2 |
| Distribution | Go module `github.com/vinary-tree/libdictenstein/bindings/go/v4` |
| Native boundary | cgo over `ldict_*` |
| Canonical facade source | [`bindings/go/libdictenstein.go`](../../bindings/go/libdictenstein.go) |

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

The canonical checked example is [`bindings/go/libdictenstein_test.go`](../../bindings/go/libdictenstein_test.go). CI runs
the public package path with:

```sh
go test ./bindings/go/...
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

`SnapshotEntries` and `Entries` materialize host-owned keys from one
immutable revision. `OpenEntryStream` exposes bounded `Next`, `Cancel`, and
`Close` operations plus Go 1.23 range-compatible `Seq` and `Seq2` helpers.
Range exit closes automatically, including early `break` and panic; direct
`Next` callers close explicitly. `SnapshotEntry` keeps byte keys as `[]byte`,
Unicode-scalar keys as `string`, `u64` keys as `[]uint64`, and mapped values as
`*uint64`, so nil, zero, and max remain distinct.

The public-package benchmark keeps construction and warmup outside the timed
drain and prints one JSON record. Run the 4,096-entry latency cell, the
65,536-entry streaming cell, or the 64-entry cancellation cell with:

```sh
go run ./bindings/go/cmd/collection-traversal-profile --arm materialized --entries 4096
go run ./bindings/go/cmd/collection-traversal-profile --arm stream --entries 65536 --batch-size 256
go run ./bindings/go/cmd/collection-traversal-profile --arm stream-cancel --entries 65536 --batch-size 64 --early-cancel 64
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

```go
joined, err := left.UnionWith(right, libdictenstein.LatticeJoinValue)
if err != nil { return err }
defer joined.Close()
count, err := joined.Len()
if err != nil { return err }
fmt.Println(count)
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

Call `Close` with `defer` immediately after construction; finalizers only report abandoned handles.

An exported resource is borrowed until its base-vtable `retain` succeeds. A
captured snapshot arrives owning one retain and may outlive the mutable
dictionary. Later inserts, updates, removals, compaction, or checkpoints do not
alter a pinned revision. Every successful retain has exactly one release, and
failed construction transfers no ownership.

## Errors and failure containment

Operations return inspectable Go errors with native status and diagnostic. Branch on the typed status or exception, not diagnostic text.
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
