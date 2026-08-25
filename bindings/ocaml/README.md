# Vinary Tree libdictenstein for OCaml

C stubs over libdictenstein's stable `ldict_*` C ABI, built with Dune. The opam
package is `libdictenstein`; the module is
`Vinary_tree_libdictenstein`. It exposes DynamicDAWG CRUD and batch insertion,
immutable DoubleArrayTrie construction, SCDAWG substring search, persistent
ARTrie CRUD/checkpoint/reopen, and persistent vocabulary reverse lookup.

## Native library

The stubs link the shared library `libdictenstein`. Build it and put it on the
loader path:

```sh
cargo build --release --no-default-features --features ffi
export LD_LIBRARY_PATH="$PWD/target/release:$LD_LIBRARY_PATH"
```

The in-repo `include/` directory carries copies of `libdictenstein.h` and
`vinary_tree_interop.h` for the opam build.

## Quickstart

```ocaml
open Vinary_tree_libdictenstein

let () =
  let d = dynamic_dawg () in                       (* Unicode-scalar default *)
  ignore (put_many d [| ("cat", Some 1L); ("cot", Some 2L); ("cut", None) |]);
  assert (length d = 3);
  let hit = get d "cot" in
  assert (hit.found && hit.value = Some 2L);

  let s = scdawg () in
  ignore (put s "cat" None);
  ignore (put s "cot" None);
  assert (contains_substring s "ot");
  assert (substring_frequency s "t" = 2);
  close d;
  close s
```

Pass `~domain:Vinary_tree_interop.Byte` (or `U64`) to a constructor to select a
non-default unit domain.

## Backends and capabilities

| Constructor | Kind | Unit domains | Capabilities |
|-------------|------|--------------|--------------|
| `dynamic_dawg` | 1 | Byte, Unicode_scalar, U64 | read, insert, remove, clear, compact |
| `double_array_trie` | 2 | Byte, Unicode_scalar | read (immutable) |
| `scdawg` | 3 | Byte, Unicode_scalar | read, insert, substring |
| `create/open_persistent_artrie` | 4 | Byte, Unicode_scalar, U64 | read, insert, remove, checkpoint |
| `create/open_persistent_vocabulary` | 5 | Unicode_scalar | read, insert, checkpoint |

`kind` and `capabilities` report the runtime backend id and `LDICT_CAP_*`
bitset.

## Values and domains

Text terms are `string` (UTF-8 for the Unicode-scalar backends, which validate
it). The u64 API (`put_u64`, `contains_u64`, `get_u64`, `remove_u64`) takes
`int64 array`. Values are `int64 option`; because OCaml's native `int64` is
signed, the two's-complement bit pattern is passed through to the u64 slot, so
the full `0 .. UINT64_MAX` range is expressible. `None` and `Some 0L` are
distinct. `get` returns `{ found : bool; value : int64 option }`.

## Error handling

Non-OK statuses raise `Failure` carrying the thread-local
`ldict_last_error_message()`. Backend-unsupported operations surface the
`UNSUPPORTED` status; wrong-domain terms surface `DOMAIN_MISMATCH` (9).

## Retained resource handoff

`resource` returns the shared `Vinary_tree_interop.resource`. An independently
packaged liblevenshtein transducer retains it in constant time and keeps its
query-start revision valid after `close`.

## Ordered entry collections

`with_entries_seq` scopes a lazy, native-lexicographic `Seq.t` to one immutable
dictionary revision. Every native batch is validated, copied, and released
before a sequence node reaches OCaml; `Fun.protect` closes the cursor on full
drain, early return, or exception. `fold_entries` is the synchronous reducer.
Keys are `Bytes`, UTF-8 `Unicode`, or raw-bit-pattern `U64` arrays, and values
remain `int64 option`, so present-unvalued differs from `Some 0L`.

The package benchmark supports `materialized`, `stream`, `stream-cancel`, and
`reduce` and prints one `libdictenstein.host-collection-traversal.v1` record:

```sh
dune exec --root bindings/ocaml bin/collection_traversal_profile.exe -- \
  --arm stream-cancel --entries 65536 --batch-size 64 --early-cancel 64
```

<!-- BEGIN GENERATED BINDING OPERATIONS; DO NOT EDIT -->

## Support and package contract

| Property | Contract |
|---|---|
| Binding | OCaml |
| Languages/runtime | OCaml 5 |
| Support tier | Tier 3 |
| Distribution | opam `libdictenstein` |
| Native boundary | C stubs over the stable ABI |
| Canonical facade source | [`bindings/ocaml/vinary_tree_libdictenstein.mli`](../../bindings/ocaml/vinary_tree_libdictenstein.mli) |

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

The canonical checked example is [`bindings/ocaml/test/conformance.ml`](../../bindings/ocaml/test/conformance.ml). CI runs
the public package path with:

```sh
opam exec -- dune runtest --root bindings/ocaml
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

`with_entries_seq` supplies a scoped, repeatable `Seq.t`
whose keys are `Bytes`, UTF-8 `Unicode`, or raw-bit-pattern `U64` arrays and
whose values remain `int64 option`. `fold_entries` uses the native synchronous
reducer. `Fun.protect` closes after full drain, early return, or exception, and
the sequence cannot escape its owning callback; a finalizer only contains an
abandoned custom cursor.


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

Use explicit close functions or `Fun.protect`; finalizers only protect abandoned values.

An exported resource is borrowed until its base-vtable `retain` succeeds. A
captured snapshot arrives owning one retain and may outlive the mutable
dictionary. Later inserts, updates, removals, compaction, or checkpoints do not
alter a pinned revision. Every successful retain has exactly one release, and
failed construction transfers no ownership.

## Errors and failure containment

Statuses become typed OCaml exceptions with copied diagnostics. Branch on the typed status or exception, not diagnostic text.
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
