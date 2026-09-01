# Deserialization safety

**Navigation**: [← Security](README.md) · [Threat model](threat-model.md)

Loading a dictionary from bytes is the one place libdictenstein turns **adversary-controlled data**
into in-memory structure. This document states the trust boundary of each deserialization path: what
it validates, and where a crafted input can still request a large allocation. Notation follows
[`docs/notation.md`](../notation.md).

The headline: **parsing is fail-closed** — every path returns an error rather than reading out of
bounds or panicking on malformed data. The protobuf graph walkers are iterative, declared DAT term
counts cannot cause an unchecked reservation, and duplicate outgoing labels are rejected. The
remaining resource boundary is the size of the byte stream itself: high-level readers consume a
complete message and gzip can expand a small input substantially. Bound input and decompressed
output before deserializing from an untrusted source.

## The volatile serializers (feature `serialization` / `protobuf` / `compression`)

All volatile serializers decode to a **term list** (`Vec<String>` or `Vec<(String, V)>`) and rebuild
the dictionary with `from_terms` / `from_terms_with_values`. No serializer reconstructs internal node
arrays directly from bytes, so a malformed encoding can at worst produce a *wrong-but-valid*
dictionary — it **cannot** corrupt in-memory invariants. Sources under
[`src/serialization/`](../../src/serialization/).

| Format | Parse safety | Allocation edge |
|--------|--------------|-----------------|
| **bincode** (`bincode_compat.rs`) | `bincode::config::legacy()` — fixint LE, strict trailing-byte check | **no `.with_limit()`** — no crate-level byte cap; OOM resistance rests on bincode's own per-collection strategy |
| **protobuf** (`protobuf_impl.rs`, feat. `protobuf`) | IDs range-checked, duplicate labels rejected, packed-edge shape checked, final-node deltas use `checked_add`, reachable acyclicity and declared term count checked | complete message and reconstructed terms reside in memory |
| **gzip** (`compression_impl.rs`, feat. `compression`) | wraps a reader in `GzDecoder` and delegates | **decompression bomb** — no size cap on the decompressed stream |

JSON, TOML, and plaintext dictionary persistence are not part of the API.

### protobuf: bounded count hints and iterative graph traversal

`decode_dat_terms` treats the declared term count as a consistency value and a *bounded* capacity
hint. Its initial capacity is capped by the number of minimum-size length-delimited records that can
fit in the actual payload, so a field such as `u64::MAX` cannot independently request an enormous
allocation. The post-parse equality check still rejects a false count.

The general protobuf decoders use explicit heap stacks for both reachable-acyclic validation and
term enumeration. Graph depth therefore consumes bounded heap storage instead of call-stack frames;
a 50,000-edge regression fixture exercises the path. Each node's outgoing labels must also be
unique. That requirement preserves the deterministic-dictionary invariant instead of letting two
encoded edges compete for the same input byte.

These checks bound allocations by encoded structure rather than by unauthenticated count hints.
They do not impose a universal maximum message size: the entire encoded message, adjacency, and
reconstructed terms can still be large when the input itself is large.

### gzip: decompression bomb

`GzDecoder` will happily inflate a tiny input into a huge stream, which then feeds the (uncapped)
protobuf/bincode path. Treat a compressed blob's *decompressed* size as attacker-chosen.

## The persistent loaders (feature `persistent-artrie`)

Opening a persistent ARTrie replays a WAL and maps a checkpoint image. These parsers are
**parse-safe / fail-closed**: every slice read is length-guarded and returns
`WalError::CorruptedRecord` / `PersistentARTrieError::corrupted` rather than panicking — e.g.
`wal/codec.rs` guards `payload.len() < offset + N` on every field, and `wal/reader.rs` rejects a
record whose declared `length` is below the header size, so the subtraction can never underflow.
An otherwise valid image whose bounded working collection cannot be reserved returns
`PersistentARTrieError::AllocationFailed`. That error is transient and is deliberately not
classified as corruption, so resource pressure cannot cause a valid checkpoint to be mistaken for
damaged data.

The residual edge is the same as the serializers': **upfront allocation sized from an untrusted
`u32`**, before the data backing it is fully verified. Representative sites:

- `wal/reader.rs` — `vec![0u8; payload_len]` where `payload_len` comes from a `u32` record length,
  allocated *before* the CRC is checked (up to ~4 GiB per crafted record).
- `wal/codec.rs` — `Vec::with_capacity(count)` for record entry vectors.
- `arena.rs::from_bytes` — `Vec::with_capacity(node_count)` for the mmap image loader.

**The good model to imitate** lives right beside these: `persistent_artrie/u64.rs` checks
`MAX_NODE_COUNT` (4,194,304 records, exactly the cardinality of the checkpoint pointer's 22-bit
offset), `MAX_PREFIX_UNITS`, `MAX_VALUE_BYTES`, and `MAX_CHILDREN_PER_NODE`. It additionally proves
that the remaining encoded extent can contain each declared table before reserving it and uses
fallible exact reservations for decoded tables, mapped child vectors, and input-sized sparse child
indexes. The persistent suffix families cap `MAX_WAL_RECORD_BYTES` (64 MiB). These checks bound
allocation independently of attacker-declared counts. The unbounded sites above are candidates to
adopt the same extent-proof pattern.

The native-u64 loader also separates validation from construction: a tri-color
explicit-frame pass rejects reachable cycles before any `Arc` edge is installed,
then a child-before-parent postorder pass materializes one resident overlay node
per reachable disk record and preserves valid DAG sharing. Native-u64 iterators
therefore require a fully resident root. Their `try_iter_*` variants return a
typed corruption error and terminate if an unresolved `OnDisk` child is ever
observed, rather than treating that branch as absent. This remains fail-closed if
future eviction work accidentally crosses the current resident-only boundary.

> **Coverage note.** The persistent parsers are covered *for torn-record and partial-replay
> correctness* by TLA⁺ models (`PointerOwnership`, `StorageSyscallOutcome`) and reopen tests. The
> *allocation-sizing from a `u32` field* is not itself modeled — a genuine, documented gap.

## Guidance for callers

1. **Never deserialize an unbounded untrusted blob.** Impose a maximum input length appropriate to
   your dictionary sizes before calling `deserialize`; the library does not (yet) impose a universal
   one for you.
2. **Compressed input:** bound the *decompressed* size, not just the compressed size.
3. **protobuf from an untrusted peer:** cap the input even though count hints and graph traversal are
   hardened; a structurally valid message may still contain a very large graph or term language.
4. **A failed load is safe:** deserialization returns `Err` on malformed data and leaves you with no
   partially-built dictionary — retry or reject, but you will not get a corrupt structure.
