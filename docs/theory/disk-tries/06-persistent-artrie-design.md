# Persistent ARTrie Design

This document describes the current persistent ARTrie design used by
libdictenstein. It supersedes the older hybrid owned-tree/B-trie proposal.

## Design Goals

| Goal | Current mechanism |
|------|-------------------|
| Crash-durable key/value dictionaries | WAL-before-publish writes and checkpoint/recovery |
| Concurrent reads and writes | Immutable overlay nodes and atomic publication |
| Native byte, Unicode, and u64 labels | `ByteKey`, `CharKey`, and `U64Key` encodings |
| Compact checkpoints | CX overlay serialization with variant-specific projection |
| Large durable vocabularies | `PersistentVocabARTrie` overlay plus rebuilt reverse map |

The persistent ARTrie is optimized for dictionary traversal. It is not an LSM
tree, not a read-only FST, and not the old owned-tree snapshot design.

## Core Architecture

```text
Dictionary API
    -> KeyEncoding converts public keys to units
    -> immutable OverlayNode path is cloned and edited
    -> WAL record is appended before visibility
    -> new root/child pointer is published atomically
    -> checkpoint serializes the overlay into a dense CX image
```

Published overlay nodes are immutable. This gives readers a stable view without
holding a global mutation lock. Writers publish a new version through CAS after
durability requirements are satisfied.

## Key Encodings

| Encoding | Unit | Public profile |
|----------|------|----------------|
| `ByteKey` | `u8` | `PersistentARTrie` |
| `CharKey` | `u32` Unicode scalar value | `PersistentARTrieChar` |
| `U64Key<PREFIX>` | `u64` | `PersistentARTrieU64Compact` and `PersistentARTrieU64Prefix3Compat` |

The u64 profiles keep 64-bit labels native. They do not expand every label into
eight byte transitions, which is important for time-series and token workloads.

## Adaptive Edge Storage

The shared overlay uses immutable adaptive edge storage:

- Tiny and small inline stores avoid heap work for low fanout.
- Sorted stores keep medium fanout compact.
- Sparse indexed stores accelerate high-fanout non-byte labels.
- Byte labels can use ART-style dense tiers for byte-indexed lookup.

This is the common architecture for byte, char, vocab, and u64 ARTrie variants.

## U64 Profiles

`PersistentARTrieU64Compact` is the default profile:

- `U64Key<U64_CX_PREFIX_COMPACT>`
- prefix-4 CX checkpoint budget
- native `u64` edge labels
- shared WAL record codec
- shared overlay-node publication

`PersistentARTrieU64Prefix3Compat` is explicit:

- `U64Key<U64_CX_PREFIX_COMPAT>`
- prefix-3 CX checkpoint budget
- compatibility and benchmark baseline use

The old native bincode snapshot/WAL path is not retained in source. When a
historical control is needed, create a git worktree at the relevant historical
commit and benchmark that implementation separately.

## Durability And Checkpoints

The byte/char/vocab ARTrie paths use the Order-A discipline:

```text
append durable WAL record
    -> publish overlay root by CAS
    -> record commit rank / committed watermark where applicable
    -> acknowledge
```

The u64 profile follows the same log-before-publish visibility rule with shared
WAL records and a u64 CX checkpoint image. Durable u64 writes append the data
record, publish by CAS, append `CommitRank`, and advance the committed-prefix
watermark. Checkpoint capture serializes the published overlay into a dense disk
image, records the safe `checkpoint_lsn`, and retains the WAL tail for recovery.
Recovery loads the checkpoint, reconciles ranked WAL records, and replays only
operations not covered by the checkpoint watermark.

## Concurrency Model

- Reads are non-blocking on the mutation path.
- Write publication is lock-free CAS for the ARTrie overlay family.
- Immediate durable acknowledgements still wait for WAL append/fsync; storage
  I/O can block the caller even though overlay publication has no trie-wide
  mutation lock.
- Checkpoints are serialized by a checkpoint lock to avoid torn checkpoint
  publication.
- Memory safety relies on immutable published nodes and the same reclamation
  discipline used by the lock-free overlay.

Do not apply this write-concurrency claim to the persistent suffix graph family:
those types use snapshot reads and serialized graph rebuild/publish writes.

## Empirical Status

The u64 compact profile was benchmarked with a seeded time-series workload. An
earlier fixed-sample run showed:

- checkpoint: `656,679` bytes for native prefix-4 vs `1,585,249` bytes for
  byte-encoded u64 keys
- lookup: `350.72 ns/query` native prefix-4 vs `455.01 ns/query` encoded control
- prefix budget: `320.97` bytes/entry prefix-4 vs `336.74` prefix-3

Welch's t-test found statistically significant improvement for prefix-4 storage
versus prefix-3 and for native prefix-4 lookup versus byte-encoded lookup. Raw
samples were appended to pgmcp artifacts `111` and `112`.

The post-watermark/CommitRank run on 2026-06-13 appended a registered pgmcp
experiment set:

- lookup: `357.25 ns/query` native prefix-4 vs `455.35 ns/query` byte-encoded
  u64 control, accepted at `p = 2.82e-35`
- parallel readers plus writer: `148.35 ns/read` native prefix-4 vs
  `204.30 ns/read` byte-encoded u64 control, accepted at `p = 4.42e-9`
- checkpoint density: `453.98` bytes/entry prefix-4 vs `469.76` prefix-3,
  accepted at `p = 4.61e-127`
- full checkpoint directory: `929,096` bytes native prefix-4 vs `1,585,249`
  byte-encoded u64 control

Raw samples are in
[`docs/experiments/persistent-u64-watermark-commitrank-2026-06-13.md`](../../experiments/persistent-u64-watermark-commitrank-2026-06-13.md)
and pgmcp experiments `53`-`55` with artifact `132`.

## Related Material

- [Persistent storage architecture](../../persistence/mmap-architecture.md)
- [Persistence architecture README](../../architecture/persistence/README.md)
- [Root README persistent section](../../../README.md#persistent-artrie--lock-free--durable)
