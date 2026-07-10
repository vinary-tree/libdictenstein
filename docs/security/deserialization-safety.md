# Deserialization safety

**Navigation**: [← Security](README.md) · [Threat model](threat-model.md)

Loading a dictionary from bytes is the one place libdictenstein turns **adversary-controlled data**
into in-memory structure. This document states the trust boundary of each deserialization path: what
it validates, and where a crafted input can still request a large allocation. Notation follows
[`docs/notation.md`](../notation.md).

The headline: **parsing is fail-closed** — every path returns an error rather than reading out of
bounds or panicking on malformed data — but **allocation sizing is the residual edge**: a few sites
read a count from untrusted input and reserve capacity *before* fully validating it, so a crafted
input can request a large allocation (an OOM abort, not memory corruption). Bound the input size
before deserializing from an untrusted source.

## The volatile serializers (feature `serialization` / `protobuf` / `compression`)

All volatile serializers decode to a **term list** (`Vec<String>` or `Vec<(String, V)>`) and rebuild
the dictionary with `from_terms` / `from_terms_with_values`. No serializer reconstructs internal node
arrays directly from bytes, so a malformed encoding can at worst produce a *wrong-but-valid*
dictionary — it **cannot** corrupt in-memory invariants. Sources under
[`src/serialization/`](../../src/serialization/).

| Format | Parse safety | Allocation edge |
|--------|--------------|-----------------|
| **bincode** (`bincode_compat.rs`) | `bincode::config::legacy()` — fixint LE, strict trailing-byte check | **no `.with_limit()`** — no crate-level byte cap; OOM resistance rests on bincode's own per-collection strategy |
| **JSON** (`json_impl.rs`) | `serde_json::from_reader` — streaming, no length-prefix preallocation | low risk |
| **protobuf** (`protobuf_impl.rs`, feat. `protobuf`) | IDs range-checked, `edge_data.len() % 3` checked, final-node deltas use `checked_add`, `validate_term_count` cross-checks | **two real weaknesses, below** |
| **gzip** (`compression_impl.rs`, feat. `compression`) | wraps a reader in `GzDecoder` and delegates | **decompression bomb** — no size cap on the decompressed stream |

### protobuf: the two weaknesses

1. **OOM by preallocation-before-validation.** `decode_dat_terms` does
   `Vec::with_capacity(term_count)` where `term_count` is an untrusted `u64` protobuf field, and it
   does so *before* `validate_term_count` runs. A crafted `term_count` near $`10^{18}`$ requests a
   capacity that aborts the process.
2. **Recursion over attacker-controlled graph depth.** `terms_from_adjacency::dfs` and
   `ensure_reachable_acyclic::visit` recurse on the decoded graph, so a degenerate deep chain can
   overflow the stack. Note the sharp contrast: the generic `extract_terms` / `extract_terms_char`
   paths in `serialization/mod.rs` were **deliberately rewritten to iterative** (explicit `Vec`
   stack, with `test_extract_terms_deep_chain_*` guarding ~50k-deep chains) — the protobuf import DFS
   was not given the same treatment. This is a known asymmetry.

### gzip: decompression bomb

`GzDecoder` will happily inflate a tiny input into a huge stream, which then feeds the (uncapped)
protobuf/bincode path. Treat a compressed blob's *decompressed* size as attacker-chosen.

## The persistent loaders (feature `persistent-artrie`)

Opening a persistent ARTrie replays a WAL and maps a checkpoint image. These parsers are
**parse-safe / fail-closed**: every slice read is length-guarded and returns
`WalError::CorruptedRecord` / `PersistentARTrieError::corrupted` rather than panicking — e.g.
`wal/codec.rs` guards `payload.len() < offset + N` on every field, and `wal/reader.rs` rejects a
record whose declared `length` is below the header size, so the subtraction can never underflow.

The residual edge is the same as the serializers': **upfront allocation sized from an untrusted
`u32`**, before the data backing it is fully verified. Representative sites:

- `wal/reader.rs` — `vec![0u8; payload_len]` where `payload_len` comes from a `u32` record length,
  allocated *before* the CRC is checked (up to ~4 GiB per crafted record).
- `wal/codec.rs` — `Vec::with_capacity(count)` for record entry vectors.
- `arena.rs::from_bytes` — `Vec::with_capacity(node_count)` for the mmap image loader.

**The good model to imitate** lives right beside these: `persistent_artrie/u64.rs` checks
`MAX_NODE_COUNT` (16 Mi), `MAX_PREFIX_UNITS`, `MAX_VALUE_BYTES`, and `MAX_CHILDREN_PER_NODE`
*before* allocating, and the persistent suffix families cap `MAX_WAL_RECORD_BYTES` (64 MiB). These
bound the allocation to a sane ceiling regardless of the declared count. The unbounded sites above
are candidates to adopt the same pattern.

> **Coverage note.** The persistent parsers are covered *for torn-record and partial-replay
> correctness* by TLA⁺ models (`PointerOwnership`, `StorageSyscallOutcome`) and reopen tests. The
> *allocation-sizing from a `u32` field* is not itself modeled — a genuine, documented gap.

## Guidance for callers

1. **Never deserialize an unbounded untrusted blob.** Impose a maximum input length appropriate to
   your dictionary sizes before calling `deserialize`; the library does not (yet) impose a universal
   one for you.
2. **Compressed input:** bound the *decompressed* size, not just the compressed size.
3. **protobuf from an untrusted peer:** be aware of the recursion and preallocation edges above; a
   size cap on the input mitigates both in practice (it bounds both the declared counts that fit and
   the graph depth that fits).
4. **A failed load is safe:** deserialization returns `Err` on malformed data and leaves you with no
   partially-built dictionary — retry or reject, but you will not get a corrupt structure.
