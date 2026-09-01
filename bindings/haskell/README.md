# Vinary Tree libdictenstein for Haskell

A `foreign import ccall` facade over libdictenstein's stable `ldict_*` C ABI,
with a thin `cbits/libdictenstein_hs.c` shim for the aggregate-return calls. The
Hackage package is `libdictenstein`; the module is
`VinaryTree.Libdictenstein`. It exposes DynamicDAWG CRUD and batch insertion,
immutable DoubleArrayTrie construction, SCDAWG substring search, persistent
ARTrie CRUD/checkpoint/reopen, and persistent vocabulary reverse lookup.

## Native library

The facade links the shared library `libdictenstein`. Build it and expose it to
the linker and loader:

```sh
cargo build --release --no-default-features --features ffi
export LIBRARY_PATH="$PWD/target/release:$LIBRARY_PATH"
export LD_LIBRARY_PATH="$PWD/target/release:$LD_LIBRARY_PATH"
cabal build --extra-lib-dirs="$PWD/target/release"
```

## Quickstart

```haskell
{-# LANGUAGE OverloadedStrings #-}

import VinaryTree.Libdictenstein

main :: IO ()
main = do
  d <- dynamicDawg UnicodeScalar
  _ <- putText d "cat" (Just 1)
  _ <- putText d "cot" (Just 2)
  _ <- putText d "cut" Nothing          -- valueless term
  n <- dictionaryLength d
  print n                                -- 3
  hit <- getText d "cot"
  print (found hit, mappedValue hit)     -- (True,Just 2)

  s <- scdawg UnicodeScalar
  _ <- putText s "cat" Nothing
  _ <- putText s "cot" Nothing
  present <- containsSubstring s "ot"    -- ByteString pattern
  freq <- substringFrequency s "t"
  print (present, freq)                  -- (True,2)

  close d
  close s
```

## Backends and capabilities

| Constructor | Kind | Unit domains | Capabilities |
|-------------|------|--------------|--------------|
| `dynamicDawg` | 1 | Byte, UnicodeScalar, U64 | read, insert, remove, clear, compact |
| `doubleArrayTrie` | 2 | Byte, UnicodeScalar | read (immutable) |
| `scdawg` | 3 | Byte, UnicodeScalar | read, insert, substring |
| `createPersistentARTrie` / `openPersistentARTrie` | 4 | Byte, UnicodeScalar, U64 | read, insert, remove, checkpoint |
| `createPersistentVocabulary` / `openPersistentVocabulary` | 5 | UnicodeScalar | read, insert, checkpoint |

`dictionaryKind` and `capabilities` report the runtime backend id and
`LDICT_CAP_*` bitset.

## Values and domains

Both `Text` (`putText`, `getText`, `containsText`, `removeText`) and
`ByteString` (`putBytes`, `getBytes`, `containsBytes`, `removeBytes`,
`putManyBytes`) APIs are provided; the Unicode-scalar backends validate UTF-8.
The u64 API (`putU64`, `containsU64`, `getU64`, `removeU64`) takes `[Word64]`.
Values are `Maybe Word64` over the full `0 .. maxBound` range; `Nothing` and
`Just 0` are distinct. `Lookup` carries `found :: Bool` and
`mappedValue :: Maybe Word64`.

## Error handling

Non-OK statuses throw an `IOError` carrying the thread-local
`ldict_last_error_message()`. Backend-unsupported operations surface the
`UNSUPPORTED` status; wrong-domain terms surface `DOMAIN_MISMATCH` (9).

## Retained resource handoff

`resource` yields the shared retained handle, so a liblevenshtein transducer
retains the dictionary in constant time and keeps its query-start revision valid
after `close`. A `ForeignPtr` finalizer frees any handle a caller forgets.

## Ordered entry collections

`withEntryStream` brackets a one-shot `EntryStream`; `nextEntry` returns owned
`DictionaryEntry` values in native lexicographic order and cleanup runs after
normal completion or an exception. Use `withEntryStreamLimits` with
`EntryBatchLimits` to tune native batches. `materializeEntries` returns a
`Foldable DictionarySnapshot`, while `foldEntries` and
`foldEntriesWithLimits` provide synchronous reduction. `ByteKey`,
`UnicodeKey`, and `U64Key` preserve the domain and `Maybe Word64` preserves the
full value channel.

The Cabal benchmark emits `libdictenstein.host-collection-traversal.v1` JSON
for `materialized`, `stream`, `stream-cancel`, and `reduce`:

```sh
cabal run --project-file=bindings/haskell/cabal.project \
  libdictenstein-collection-profile -- \
  --arm stream-cancel --entries 65536 --batch-size 64 --early-cancel 64
```

<!-- BEGIN GENERATED BINDING OPERATIONS; DO NOT EDIT -->

## Support and package contract

| Property | Contract |
|---|---|
| Binding | Haskell |
| Languages/runtime | GHC/Cabal |
| Support tier | Tier 3 |
| Distribution | Hackage `libdictenstein` |
| Native boundary | Haskell FFI plus retained interop resources |
| Canonical facade source | [`bindings/haskell/src/VinaryTree/Libdictenstein.hs`](../../bindings/haskell/src/VinaryTree/Libdictenstein.hs) |

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

The canonical checked example is [`bindings/haskell/test/Conformance.hs`](../../bindings/haskell/test/Conformance.hs). CI runs
the public package path with:

```sh
cabal test --project-file=bindings/haskell/cabal.project all
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

`materializeEntries` returns a host-owned `Foldable`
snapshot. `withEntryStream` brackets a bounded cursor whose `nextEntry` values
own their `ByteString`, `Text`, or `Vector Word64` keys, while `foldEntries`
uses the native reducer without an intermediate list. Acquire/release is masked
against asynchronous exceptions, and every callback restores the caller's
masking state.


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

```haskell
bracket (algebra Union LatticeJoin left right) close $ \joined ->
  dictionaryLength joined >>= print
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

Use `bracket`/`withDictionary`; mask asynchronous exceptions across acquire/release.

An exported resource is borrowed until its base-vtable `retain` succeeds. A
captured snapshot arrives owning one retain and may outlive the mutable
dictionary. Later inserts, updates, removals, compaction, or checkpoints do not
alter a pinned revision. Every successful retain has exactly one release, and
failed construction transfers no ownership.

## Errors and failure containment

Failures become typed Haskell exceptions/status values. Branch on the typed status or exception, not diagnostic text.
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
