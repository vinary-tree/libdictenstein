# Vinary Tree libdictenstein for C

This guide documents the supported C facade over libdictenstein's stable dictionary ABI.

## Bounded entry traversal

`ldict_dictionary_entries_open` captures one immutable revision and returns an
opaque cursor plus truthful domain, ordering, length, and identity metadata.
Call `ldict_entry_cursor_next` with explicit descriptor/unit/value bounds,
consume the borrowed batch, and release its exact generation before advancing.
Early exits call `ldict_entry_cursor_cancel`, settle any lease, and then call
`ldict_entry_cursor_free`. `ldict_entry_cursor_reduce` provides the natural C
callback-fold form and performs each batch release before the next callback.
The lifecycle and retry rules are executable in
[`tests/entries.c`](tests/entries.c).

## Collection benchmark entrypoint

The public C example uses the same deterministic corpus and wrapping checksum
as the Rust traversal driver. Construction and warmup precede the timed drain;
the `materialized`, `stream`, `stream-cancel`, and `reduce` arms print one JSON
record:

```sh
cc -std=c17 -O2 -Wall -Wextra -Werror \
  bindings/c/examples/collection_traversal_profile.c \
  -I include -L target/release -llibdictenstein \
  -o /tmp/libdictenstein-c-collection-profile
LD_LIBRARY_PATH=target/release /tmp/libdictenstein-c-collection-profile \
  --arm stream --entries 4096 --batch-size 256
```

<!-- BEGIN GENERATED BINDING OPERATIONS; DO NOT EDIT -->

## Support and package contract

| Property | Contract |
|---|---|
| Binding | C |
| Languages/runtime | C17/C23 |
| Support tier | Tier 1 |
| Distribution | CMake/pkg-config native package |
| Native boundary | Direct `ldict_*` calls |
| Canonical facade source | [`include/libdictenstein.h`](../../include/libdictenstein.h) |

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

The canonical checked example is [`bindings/c/examples/snapshot_walk.c`](../../bindings/c/examples/snapshot_walk.c). CI runs
the public package path with:

```sh
cc -std=c17 -Wall -Wextra -Werror -fsyntax-only -Iinclude bindings/c/examples/snapshot_walk.c
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

The shipped low-level collection surface is an opaque `LdictEntryCursor`
over one immutable revision. Callers select hard descriptor/unit/value bounds,
lease one `LdictEntryBatch` at a time, release its exact generation before the
next call, and use `ldict_entry_cursor_reduce` when a synchronous callback fold
is more natural. `cancel` is sticky and idempotent; `free` requires no live
lease. Batch descriptors preserve byte, Unicode-scalar (`uint32_t`), and `u64`
unit arenas plus valueless versus present-`u64` members without sentinels.


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

Balance every successful constructor or retained resource with exactly one free or release.

An exported resource is borrowed until its base-vtable `retain` succeeds. A
captured snapshot arrives owning one retain and may outlive the mutable
dictionary. Later inserts, updates, removals, compaction, or checkpoints do not
alter a pinned revision. Every successful retain has exactly one release, and
failed construction transfers no ownership.

## Errors and failure containment

Inspect `LdictStatus` and copy the thread-local diagnostic before another call. Branch on the typed status or exception, not diagnostic text.
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

<!-- END GENERATED BINDING OPERATIONS -->
