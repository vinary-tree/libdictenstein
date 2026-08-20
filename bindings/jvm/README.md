# Vinary Tree libdictenstein for the JVM

A Java Foreign Function &amp; Memory (Panama) facade over libdictenstein's stable
`ldict_*` C ABI. The Maven coordinate is `io.vinarytree:libdictenstein`; the
package is `io.vinarytree.libdictenstein`. It exposes DynamicDAWG CRUD and batch
insertion, immutable DoubleArrayTrie construction, SCDAWG substring search,
persistent ARTrie CRUD/checkpoint/reopen, and persistent vocabulary reverse
lookup. The Clojure facade (`io.vinarytree/libdictenstein-clojure`) is a thin
layer over these classes.

## Requirements and native library

The facade uses `java.lang.foreign`, so run on a recent JDK with native access
enabled (`--enable-native-access=ALL-UNNAMED`). The published jar bundles the
native library under `META-INF/native/<platform>/`; `NativeLibraryLoader`
extracts and loads it, falling back to `System.loadLibrary("libdictenstein")`.
For a source checkout, build the library and put it on `java.library.path`:

```sh
cargo build --release --no-default-features --features ffi
# then run with: -Djava.library.path=target/release --enable-native-access=ALL-UNNAMED
```

## Quickstart

```java
import io.vinarytree.libdictenstein.*;
import java.util.Map;
import java.util.OptionalLong;

try (var dictionary = new DynamicDawg()) {          // Unicode-scalar by default
    dictionary.putAllStrings(Map.of(
            "cat", OptionalLong.of(1),
            "cot", OptionalLong.of(2),
            "cut", OptionalLong.empty()));           // valueless term
    Dictionary.Lookup hit = dictionary.get("cot");
    assert hit.present() && hit.value().equals(OptionalLong.of(2));
}

try (var suffixes = new Scdawg()) {
    suffixes.put("cat", OptionalLong.empty());
    suffixes.put("cot", OptionalLong.empty());
    assert suffixes.containsSubstring("ot");
    assert suffixes.frequency("t") == 2;
}
```

`Dictionary` implements `AutoCloseable`; `close()` (via try-with-resources)
frees the handle once, and a `Cleaner` reclaims any handle a caller forgets.

## Backends and capabilities

| Class | Kind | Unit domains | Capabilities |
|-------|------|--------------|--------------|
| `DynamicDawg` | 1 | byte, unicode-scalar, u64 | read, insert, remove, clear, compact |
| `DoubleArrayTrie` | 2 | byte, unicode-scalar | read (immutable) |
| `Scdawg` | 3 | byte, unicode-scalar | read, insert, substring |
| `PersistentARTrie` | 4 | byte, unicode-scalar, u64 | read, insert, remove, checkpoint |
| `PersistentVocabulary` | 5 | unicode-scalar | read, insert, checkpoint |

`kind()` and `capabilities()` report the runtime backend id and `LDICT_CAP_*`
bitset; `domain()` returns the `UnitDomain` fixed at construction.

## Text domains and values

`String` terms are UTF-8 encoded; `byte[]` terms are passed verbatim (byte
domain); `long[]` terms drive the u64 domain. The Unicode-scalar backends
validate UTF-8. Values are `OptionalLong` interpreted as unsigned across the
whole 64-bit range; `OptionalLong.empty()` and `OptionalLong.of(0)` are
distinct. `Lookup` is a record of `present()` and `value()`. Batch inserts are
`putAllStrings` / `putAllBytes` / `putAllU64`.

## Error handling

Non-OK statuses throw `NativeException`, whose `status()` is the numeric
`LdictStatus` and whose message is the thread-local
`ldict_last_error_message()`. Backend-unsupported operations surface as
`UNSUPPORTED`; wrong-domain terms surface as `DOMAIN_MISMATCH` (9).

## Retained resource handoff

`resourceSegment()` exposes the shared `VtResource`, so a liblevenshtein
transducer retains the dictionary in constant time and keeps its query-start
revision valid after `close`.

## Testing

```sh
./gradlew test
```

from `bindings/jvm`, with the native library discoverable. See
[`BackendIntegrationTest.java`](src/test/java/io/vinarytree/libdictenstein/BackendIntegrationTest.java).

<!-- BEGIN GENERATED BINDING OPERATIONS; DO NOT EDIT -->

## Support and package contract

| Property | Contract |
|---|---|
| Binding | JVM |
| Languages/runtime | Java 22+, Kotlin, and Scala |
| Support tier | Tier 1 |
| Distribution | Maven `io.vinarytree:libdictenstein` |
| Native boundary | Java FFM over the stable C ABI |
| Canonical facade source | [`bindings/jvm/src/main/java/io/vinarytree/libdictenstein`](../../bindings/jvm/src/main/java/io/vinarytree/libdictenstein) |

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

The canonical checked example is [`bindings/jvm/src/test/java/io/vinarytree/libdictenstein/BackendIntegrationTest.java`](../../bindings/jvm/src/test/java/io/vinarytree/libdictenstein/BackendIntegrationTest.java). CI runs
the public package path with:

```sh
./gradlew -p bindings/jvm test
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

`Dictionary.snapshot()` returns an immutable ordered
`DictionarySnapshot` with `Collection`, `List`, `Set`, and `Map` views over
value-semantic `DictionaryKey`/`DictionaryEntry` objects. The dictionary is
repeatably `Iterable`; `openEntryStream` is a closeable `Iterator`/`Spliterator`,
and `streamEntries` is a sequential Java `Stream`. Java uses
try-with-resources, Kotlin uses collections/`asSequence()`/`use`, and Scala uses
collection converters/`Using`; all share the same FFM cursor and never depend
on garbage collection for native cleanup.


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

Use try-with-resources, Kotlin `use`, or Scala `Using`; `Cleaner` is not deterministic cleanup.

An exported resource is borrowed until its base-vtable `retain` succeeds. A
captured snapshot arrives owning one retain and may outlive the mutable
dictionary. Later inserts, updates, removals, compaction, or checkpoints do not
alter a pinned revision. Every successful retain has exactly one release, and
failed construction transfers no ownership.

## Errors and failure containment

Native statuses become typed JVM exceptions with symbolic status and diagnostic. Branch on the typed status or exception, not diagnostic text.
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
