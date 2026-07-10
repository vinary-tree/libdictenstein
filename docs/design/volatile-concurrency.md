# Design: concurrency model of the in-memory dictionaries

This design record explains *why* the volatile (in-memory) dictionaries are built the way they are
for concurrency, the invariants that make them correct, and how those invariants are tested. The
*what* — the structures themselves — is in
[architecture/in-memory-dictionaries.md](../architecture/in-memory-dictionaries.md); this document is
the rationale and the verification story. Notation follows [`docs/notation.md`](../notation.md).

## Goal and non-goals

**Goal.** Every mutable in-memory backend must give **wait-free reads** (a lookup never blocks, never
spins, and completes in a bounded number of steps regardless of concurrent writers) and **lock-free
writes** (a writer never holds a global mutation lock; system-wide progress is guaranteed). This
matters because these dictionaries sit on the hot path of a fuzzy transducer that fans a single
query into thousands of concurrent node traversals — a reader-side lock there would serialize the
whole search.

**Non-goals.** Linearizable *multi-key transactions* are out of scope: each `insert` / `remove` /
`contains` is individually linearizable, but there is no cross-term atomic batch. Durability is also
out of scope here — these backends are volatile by definition; the durable story is the
[persistent ARTrie](../persistence/README.md).

## Why not a `RwLock`?

The obvious design — wrap the structure in a `parking_lot::RwLock` — fails the goal on the read side:
readers contend on the lock's atomics and, under a writer, block entirely. For a workload that is
overwhelmingly reads issued in parallel, that is the wrong trade. The chosen design instead makes the
*published state immutable*, so readers need no lock at all: they load an `Arc` snapshot and walk it,
and the snapshot cannot change under them because writers publish *new* immutable state rather than
mutating the old.

> The `default = ["parking_lot"]` feature still pulls in `parking_lot`, but for the *dynamic
> backends'* internal bookkeeping, not as a reader-visible global lock on the trie. The lock-free
> claim is about the read/publish path.

## The two strategies and why each fits

The rationale for the split (detailed in
[the architecture doc](../architecture/in-memory-dictionaries.md#3-two-lock-free-concurrency-strategies))
comes down to **edit locality**:

- **Per-node CAS** (`DynamicDawg`, `…Char`, `…U64`) — a DAWG edit touches only the nodes on one
  root-to-node path (plus minimization merges), so publishing per-node is cheap and maximizes
  reader/writer independence: a writer editing `"cat"` does not disturb a reader traversing `"dog"`.
- **Whole-graph snapshot / copy-on-write** (`SuffixAutomaton`, `Scdawg`, `PathMap`) — an edit here is
  *not* path-local (a suffix-automaton `extend` can clone-split a state and rewire suffix links
  graph-wide), so the simplest correct linearization point is a single CAS on the root pointer after
  building the new revision. PathMap makes this especially cheap because it is itself a persistent
  trie: cloning it is an $`O(1)`$ structural share, not a deep copy.

## Invariants

The design rests on four invariants, each a direct consequence of *publish-immutable-state-by-CAS*:

1. **No torn reads.** A reader observes either the pre-edit state or the post-edit state of any cell
   it loads, never a partially linked one — because a writer never mutates published data in place;
   it CAS-swaps a pointer to freshly built, immutable data.
2. **Linearizable single-key ops.** Each `insert` / `remove` / `contains` has a single atomic point
   at which it takes effect (the CAS that publishes the edit, or the `load` that reads the current
   pointer), so concurrent operations are equivalent to *some* sequential order.
3. **No lost writes.** Two writers racing on the same cell resolve by CAS: the loser observes the
   winner's new state and *retries* against it (via `CasBackoff`, [`src/nonblocking`](../../src/nonblocking.rs)),
   so no update is silently dropped. `BijectiveMap` additionally runs a rollback step to keep its
   term↔value bijection consistent if a reverse-map race is lost.
4. **Safe reclamation.** A node or revision replaced by a writer is freed exactly when the last
   reader `Arc` referencing it drops — reference counting *is* the reclamation scheme, sound because
   replaced data is immutable once unpublished. No epoch machinery is needed here (unlike the
   persistent overlay, which manages raw pointers).

## The retry primitive

Lock-free writers use `CasBackoff` (`src/nonblocking`), a bounded exponential-backoff helper around
the compare-and-swap loop. It caps spinning so a heavily contended cell degrades gracefully rather
than live-locking. Because a retry re-reads the current published state and rebuilds against it,
invariant 3 (no lost writes) holds no matter how many writers contend.

## Verification

The model is checked, not asserted:

- **Exhaustive interleaving (loom).** [`tests/dynamic_dawg_u64_correspondence.rs`](../../tests/dynamic_dawg_u64_correspondence.rs)
  runs the per-node CAS path under [`loom`](https://docs.rs/loom), which explores *all* legal thread
  interleavings of the `ArcSwap` publish/load, so a lost-write or torn-read bug cannot hide behind a
  lucky schedule. [`tests/bloom_filter_correspondence.rs`](../../tests/bloom_filter_correspondence.rs)
  gives the shared bit-vector helper the same treatment.
- **Concurrent stress across every family.** [`tests/volatile_lockfree_concurrency.rs`](../../tests/volatile_lockfree_concurrency.rs)
  drives `DynamicDawg`, `SuffixAutomaton`, `Scdawg`, `PathMap`, and `BijectiveMap` under concurrent
  readers and writers, asserting that reads always observe a consistent dictionary and that no write
  is lost.
- **Trait-law correspondence.** [`tests/dictionary_law_correspondence.rs`](../../tests/dictionary_law_correspondence.rs)
  and [`tests/dynamic_dawg_mutation_correspondence.rs`](../../tests/dynamic_dawg_mutation_correspondence.rs)
  pin the observable semantics (insert-then-contains, remove-then-not-contains, idempotence) that the
  concurrent implementation must preserve.
- **Sanitizers.** The suite runs under ThreadSanitizer (see [engineering/testing-strategy.md](../engineering/testing-strategy.md)),
  which flags any data race the design might have missed at the memory-model level.

## Related

- [architecture/in-memory-dictionaries.md](../architecture/in-memory-dictionaries.md) — the structures.
- [security/untrusted-input.md](../security/untrusted-input.md) — why the arena representation also
  removes recursive-drop and stack-overflow DoS surface.
- [persistence/concurrency-model.md](../persistence/concurrency-model.md) — the *durable* engine's
  more elaborate lock-free + epoch model, for contrast.
