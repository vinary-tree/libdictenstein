# Persistent Storage Architecture

This document describes the current persistent storage architecture used by the
`persistent-artrie` feature. It covers the byte/char/u64 ARTrie family,
`PersistentVocabARTrie`, and the native persistent suffix graph variants.

## Current Persistent Families

| Family | Live representation | Durable image | Write concurrency |
|--------|---------------------|---------------|-------------------|
| `PersistentARTrie` / `PersistentARTrieChar` | Lock-free overlay nodes over `ByteKey` / `CharKey` | CX checkpoint image + WAL | WAL-before-CAS lock-free publication |
| `PersistentARTrieU64Compact` | Lock-free overlay nodes over native `U64Key` | u64 CX checkpoint image + shared WAL records | WAL-before-CAS lock-free publication |
| `PersistentVocabARTrie` | Lock-free char overlay at `V = u64` + reverse map | Dense char overlay checkpoint + WAL | WAL-before-CAS lock-free publication |
| `PersistentSuffixAutomaton` / `PersistentSuffixTree` / `PersistentScdawg` | Immutable native suffix graph snapshot | Native graph snapshot + length-prefixed operation WAL | Serialized rebuild, copy-on-write publish |

The older owned-tree/native-bincode snapshot paths are not the live
representation for the ARTrie family. The u64 ARTrie in particular no longer
keeps the removed bincode-based snapshot/WAL format in source; old formats can be examined
from git history when needed for benchmark controls.

## Files And Recovery

Persistent dictionaries store two kinds of files:

- A checkpoint image, usually at the path passed to `create`.
- A WAL beside it, usually with the `.wal` extension or variant-specific naming.

On open/recovery, the implementation loads the latest checkpoint image and
replays retained WAL records. Checkpoints are conservative: they publish a dense
image but retain enough WAL state to recover acknowledged operations after a
crash. Experimentation with WAL truncation and compaction is documented in the
benchmark/design ledgers, not assumed by the public API.

## ARTrie Overlay And CX Checkpoints

The byte, char, vocab, and u64 ARTrie variants share the same core shape:

```text
client operation
    -> append durable WAL record
    -> clone/modify immutable overlay path
    -> publish new root by CAS
    -> checkpoint captures overlay as a dense CX image
```

Overlay nodes are immutable after publication. Writers allocate a modified copy
of the affected path and attempt to publish it with atomic root or child-pointer
CAS. Readers traverse the currently published root and do not take a global
mutation lock.

Immediate durable writes still wait for WAL append/fsync, and the synchronous
WAL writer serializes file writes. The lock-free/non-blocking claim for the
ARTrie family is the overlay traversal and publication path: reads do not wait
on a mutation lock, and writers do not serialize through a trie-wide mutation
lock before publishing their immutable path copy.

`KeyEncoding` selects the persistent key model:

- `ByteKey`: byte keys (`u8` units)
- `CharKey`: Unicode scalar keys stored as `u32` units
- `U64Key`: native `u64` sequence keys

The shared `AdaptiveEdgeStore` stores child edges without forcing all labels
into bytes. Byte labels use the ART-style Node4/16/48/256 tiers; char and u64
labels use inline, sorted, and sparse-indexed native-label storage.

## U64 Profile Formats

`PersistentARTrieU64Compact` is the default profile. It uses `U64Key` with a
prefix-4 CX budget and stores one native `u64` edge per transition. It is the
profile for new time-series or token-sequence data.

`PersistentARTrieU64Prefix3Compat` uses the prefix-3 CX budget. It exists for
opening prefix-3 images and for benchmark/control comparisons. New code should
name the compact alias unless it intentionally needs prefix-3 compatibility.

The u64 checkpoint format is a native u64 CX projection. It uses swizzled-pointer
raw tokens internally as node indexes in the checkpoint image, not as the old
native bincode tree snapshot.

## Persistent Suffix Graphs

Persistent suffix graph variants are not ARTrie overlays. They persist native
substring graph records:

- `PersistentSuffixAutomaton` stores a suffix automaton graph.
- `PersistentSuffixTree` stores a compact suffix-tree-compatible graph with
  handles for frequency and locations.
- `PersistentScdawg` stores a compact SCDAWG graph with bidirectional substring
  metadata.

Each byte/char pair stores active source or term records, appends operation WAL
records, rebuilds a new graph revision for writes, and publishes it
copy-on-write. Reads traverse immutable graph snapshots without taking the
writer lock, but the write path is intentionally serialized around graph
rebuild/publish.

## Storage Backends

`MmapDiskManager` is the default block storage implementation. With
`io-uring-backend`, persistent constructors are also available over the Linux
`io_uring` block manager. Both backends are implementation details behind the
persistent dictionary APIs; callers normally choose by type parameter or
feature-gated constructor.

## Benchmark Evidence

Earlier fixed-sample u64 experiments used a seeded time-series workload and
Welch's unequal-variance t-test. The compact prefix-4 profile produced a
`656,679` byte checkpoint versus `1,585,249` bytes for byte-encoded `u64` keys,
and lookup averaged `350.72 ns/query` versus `455.01 ns/query` for the encoded
control. The prefix-4 checkpoint budget also reduced bytes per entry versus the
prefix-3 profile (`320.97` vs `336.74`). Raw samples were appended to pgmcp
artifacts `111` and `112`.

The post-watermark/CommitRank registered pgmcp run on 2026-06-13 appended new
evidence after native u64 was aligned with the byte/char Order-A WAL discipline.
The native prefix-4 profile beat byte-encoded u64 lookup (`357.25 ns/query` vs
`455.35`, `p = 2.82e-35`) and the eight-reader/one-writer read path
(`148.35 ns/read` vs `204.30`, `p = 4.42e-9`). Prefix-4 checkpoint density beat
prefix-3 (`453.98` vs `469.76` bytes/entry, `p = 4.61e-127`). The raw ledger is
[`docs/experiments/persistent-u64-watermark-commitrank-2026-06-13.md`](../experiments/persistent-u64-watermark-commitrank-2026-06-13.md);
pgmcp experiments `53`-`55` and artifact `132` hold the structured records.
