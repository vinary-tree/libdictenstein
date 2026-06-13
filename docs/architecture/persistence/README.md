# Persistence Architecture

The `persistent-artrie` feature contains several disk-backed dictionaries. They
share durability infrastructure, but they do not all share the same graph
representation.

## Architecture At A Glance

```text
Persistent ARTrie byte/char/u64/vocab
    Dictionary API
      -> immutable overlay nodes
      -> WAL-before-publish durability
      -> CX/dense checkpoint image

Persistent suffix automaton/tree/SCDAWG
    Dictionary + SubstringDictionary APIs
      -> immutable native graph snapshots
      -> prepared/commit operation WAL
      -> CAS copy-on-write graph rebuild/publish
```

## ARTrie Family

The ARTrie family is the lock-free durable path. Writes append a WAL record
before publishing a new immutable overlay root. The successful CAS is the
visibility point. Readers use the current published root and do not take a
global mutation lock.

Profiles:

- `PersistentARTrie<V>`: byte keys through `ByteKey`.
- `PersistentARTrieChar<V>`: Unicode scalar keys through `CharKey`.
- `PersistentARTrieU64Compact<V>`: native `u64` sequence keys through `U64Key`
  with the prefix-4 compact CX budget.
- `PersistentARTrieU64Prefix3Compat<V>`: prefix-3 u64 CX compatibility and
  benchmark baseline profile.
- `PersistentVocabARTrie`: specialized durable vocabulary with forward
  `term -> u64` overlay lookup and reverse `u64 -> term` map rebuilt on reopen.

The shared edge store adapts to label width. Byte keys use ART-style dense tiers
for high fanout; char and u64 keys retain native labels and use inline, sorted,
or sparse-indexed storage as fanout grows.

## Suffix Graph Family

The persistent suffix graph types provide durable substring APIs without
encoding suffixes as ARTrie keys.

- `PersistentSuffixAutomaton` / `PersistentSuffixAutomatonChar`
- `PersistentSuffixTree` / `PersistentSuffixTreeChar`
- `PersistentScdawg` / `PersistentScdawgChar`

Reads are snapshot-based and non-blocking with respect to graph mutation. Writes
append a prepared operation, publish the rebuilt graph with pointer-identity
CAS, and append a commit marker before acknowledging the caller. Mapped
`update_or_insert` uses a retry-safe `Fn(&mut V)` updater so CAS conflicts can
recompute against the newest snapshot without taking a writer lock.

## Durability Model

The persistent APIs distinguish visibility from durability:

- ARTrie overlay writes log before publication, so acknowledged writes are not
  visible before they are durable.
- Checkpoints fold the published state into a dense image.
- Recovery loads the checkpoint and replays retained WAL records.
- Suffix graph checkpoints retain WAL records and skip operations at or below
  the checkpoint operation watermark; recovery ignores prepared records without
  commit markers. Under continuous writer churn, a suffix graph checkpoint may
  skip image publication and rely on retained WAL replay instead of blocking
  writers.

Do not describe the current u64 ARTrie as using the old native bincode
snapshot/WAL path. That implementation was removed from source; benchmark
controls for it should be taken from git history/worktrees.

## Operational Notes

- Use `target/bench-scratch` or another non-`/tmp` location for large
  persistence benchmarks; `/tmp` may be tmpfs.
- `group-commit` remains experimental and should be re-benchmarked before use.
- `io-uring-backend` is Linux-specific and requires an appropriate kernel.

## Related Docs

- [User backend guide](../../user-guide/backends.md)
- [Persistent storage architecture](../../persistence/mmap-architecture.md)
- [Persistent ARTrie design](../../theory/disk-tries/06-persistent-artrie-design.md)
- [Root README persistent section](../../../README.md#persistent-artrie--lock-free--durable)
