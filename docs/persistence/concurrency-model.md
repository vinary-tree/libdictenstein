# The concurrency model

**Navigation**: [↑ Persistence architecture](README.md) · [Lock-free overlay](lock-free-overlay.md) · [Durability & recovery](durability-and-recovery.md) · [Eviction](eviction.md)

The persistent-ARTrie family is **lock-free by default**: `SharedARTrie`,
`SharedCharARTrie`, and `SharedVocabARTrie` are bare `Arc<T>` handles, and both reads and
writes proceed without a global lock. The only operations that take *any* mutual exclusion
are concurrent checkpoints, the dormant owned-path fallback, and eviction — and those obey
one strict, acyclic lock order. This document is the concurrency contract: the F4 lock
collapse, MVCC snapshot reads, the two distinct "epoch" mechanisms, version GC, and the
eviction-safety stamp.

## Terms of art (defined before first use)

| Term | Definition |
|------|-----------|
| **lock-free** | System-wide progress is guaranteed: some thread always makes progress, even if others stall. Readers here are additionally **wait-free** (every reader finishes in bounded steps). |
| **linearizable** | Every operation appears to take effect atomically at a single point between its call and return. For a write, that point is its winning root CAS. |
| **MVCC** | *Multi-Version Concurrency Control* — readers observe a consistent *version* (snapshot) unaffected by concurrent writers. |
| **EBR** | *Epoch-Based Reclamation* — deferring the freeing of a retired object until all readers that could still reference it have left a shared *epoch*. |
| **F4 lock collapse** | The change that deleted the outer trie `RwLock`, making the `Shared*` handles bare `Arc<T>` with lock-free reads *and* writes. |

## The F4 lock collapse

Before F4, each handle was `Arc<RwLock<T>>`; a writer took the write lock, serializing all
mutation. F4 (`core/shared_access.rs`) **deletes that outer `RwLock`**: the handle is a bare
`Arc<T>`, every live write target is the lock-free overlay CAS root, and mutators became
`&self`, routing to CAS internally.

Backward compatibility is preserved without rewriting ~270 `handle.read()` / `handle.write()`
call sites (plus the cross-repo `liblevenshtein-rust` sibling): the `SharedTrieAccess`
extension trait adds `.read()` / `.write()` to the bare `Arc<T>`, each returning a
transparent, **`Deref`-only** `TrieAccessGuard` that hands back a shared `&T` — **there is
no lock**. An existing `let mut g = handle.write(); g.insert(term)` still compiles because
`insert` is now `&self`. The two `Copy`-enum fields that a lifecycle setter mutates
(`durability_policy`) live in an `AtomicEnumCell<E: U8Enum>` — a single `AtomicU8` with
lock-free `&self` load/store, strictly cheaper than the old `RwLock`-guarded read, with **no
new `unsafe`**.

### The lock hierarchy

The residual locks obey a strict acyclic order — acquire only top-to-bottom:

<img src="../diagrams/f4-lock-hierarchy.svg" alt="The F4 lock hierarchy as a top-to-bottom ordering graph. At the top, the lock-free overlay (teal) handles reads AND writes with no lock; a dashed edge notes that only the residual operations take a lock. Below it, four red lock rungs in strict acquire order: CK (checkpoint_lock Mutex, serialize concurrent checkpoints), then merge_lock (Mutex, serialize merge-vs-merge, byte/char only), then OR (owned_root RwLock, dormant kill-switch/WAL-replay, capture-read), then EC (eviction_coordinator Mutex, a LEAF never held across a lock or a worker join). A dashed grey cluster shows the vocab subset: overlay-only, so only CK and EC, and because the vocab eviction callback is a no-op, CK does not read EC, making the order trivially acyclic." width="80%"/>

$$
\text{CK} \;>\; \text{merge\_lock} \;>\; \text{OR} \;>\; \text{EC}
$$

- **CK** — `checkpoint_lock: Mutex<()>` serializes concurrent checkpoints.
- **merge_lock** — serializes merge-vs-merge (byte/char only).
- **OR** — the `owned_root: RwLock<TrieRoot>`, a *dormant* kill-switch / WAL-replay path, read during checkpoint capture.
- **EC** — `eviction_coordinator: Mutex<Option<..>>`, a **leaf**: never held across acquiring `CK`/`merge_lock`/`OR`, and never held across a worker `.join()` — the **drop-before-join** discipline, `let x = field.lock().take(); x.shutdown();`.

**Vocab is a strict subset.** `SharedVocabARTrie` is overlay-only, so it has *no* `merge_lock`
and *no* owned root — only `CK` and `EC`. Its eviction callback is a no-op, so `CK` never
reads `EC`; the two are independent and the order is trivially acyclic. The lock hierarchy
is exhaustively exercised by `tests/persistent_lockfree_f4_lock_hierarchy_loom.rs` and
`tests/vocab_lockfree_f4_lock_hierarchy_loom.rs`, and the shared-handle linearizability by
`SharedPersistentConcurrency.tla` (byte + char + vocab). The full field-disposition record
is [`../design/f4-lock-collapse-implementation.md`](../design/f4-lock-collapse-implementation.md).

## MVCC & snapshot reads

Overlay nodes are immutable and `Arc`-refcounted, so a plain read already sees a consistent
snapshot and a retired version is freed when its last holder drops it — **no epoch is needed
for basic node reclamation** (see [lock-free-overlay.md](lock-free-overlay.md#the-read-path--a-hazard-protected-snapshot-no-lock)).
On top of that, `core/mvcc.rs` provides an explicit snapshot-transaction layer: `TrieRoot`
(the snapshot interface, blanket-implemented for `OverlayNode<K, V>`) and `ReadTransaction`,
which `begin(root, epoch_manager)` pins an epoch and freezes an `Arc` root, so
`contains` / `get` observe one immutable version for the transaction's lifetime. The
`EpochManager` (`core/concurrency.rs`) is the EBR substrate that additionally gates
*eviction* and *version GC*:

<img src="../diagrams/epoch-reclamation.svg" alt="An epoch-based reclamation sequence: a reader enters the current epoch (pinning it), traverses the immutable snapshot lock-free, and leaves the epoch on completion; a writer retires an old version but the reclaimer defers freeing it until all readers pinned at the retiring epoch have left, so no reader ever touches freed memory." width="88%"/>

## The two "epoch" concepts — do not conflate

The codebase uses "epoch" for two unrelated mechanisms:

<img src="../diagrams/two-epochs.svg" alt="A side-by-side comparison of two concepts both called 'epoch'. Left, blue: EBR reader-safety epochs in core/concurrency.rs and core/mvcc.rs (EpochManager, EpochGuard, ReadTransaction) — purpose: safe memory reclamation and snapshot reads under lock-free traversal; a reader pins the current epoch and a superseded version is reclaimed only after every reader pinned at its epoch has left, noting overlay nodes are also Arc-refcounted so this layer gates snapshot transactions and eviction rather than basic node freeing. Right, indigo: durability/checkpoint epochs in core/epoch.rs (CheckpointManager, EpochMetadata, CheckpointMeta) — purpose: checkpoint accounting and WAL rotation; the WAL is divided into per-epoch segments epoch_XXXX.wal, op-count or size thresholds advance the epoch, and a durable epoch is trusted only after its trie checkpoint publishes. A grey warning box states: same word, different concept — the first is about WHEN memory is safe to free, the second about HOW the WAL is segmented for checkpoints." width="100%"/>

| | **① EBR — reader-safety epochs** | **② Durability / checkpoint epochs** |
|---|----------------------------------|--------------------------------------|
| Module | `core/concurrency.rs`, `core/mvcc.rs` | `core/epoch.rs` |
| Answers | *when* is a retired version safe to free? | *how* is the WAL segmented for checkpoints? |
| Mechanism | pin an epoch during a read; reclaim after all pinners leave | per-epoch WAL segments; threshold-driven advancement; trusted after checkpoint publish |

## Version checkpoint & GC

For point-in-time / time-travel versions, `core/version_checkpoint.rs`
(`VersionCheckpointManager`, `VersionSnapshot`) tracks current-vs-durable version ids, and
`core/version_gc.rs` (`VersionGcRegistry`, `ReaderGuard`) reclaims a superseded version only
when (a) no active reader is pinned and (b) a durable GC decision has been recorded — the
reader guard blocks reclamation until it drops. This "active readers block reclaim" race is
model-checked by `VersionLifecycle.tla`.

## Eviction safety — the `serial_disk_ptr` stamp

Eviction unswizzles cold overlay subtrees to disk *concurrently with readers and writers*.
The lynchpin that makes it race-free is the node's `serial_disk_ptr` stamp (the M-2a guard):
a node is safe to unswizzle *iff* its stamp still equals the disk pointer the eviction
registry recorded. A concurrent writer that path-copies the node publishes a new version
with a fresh identity, so the registered stamp no longer matches — and the evictor **backs
off** rather than publish a stale image:

<img src="../diagrams/serial-disk-ptr-stamp.svg" alt="A sequence diagram of the serial_disk_ptr eviction guard. Setup: when node N is checkpointed to disk_ptr D, the DiskLocationRegistry records path→D and N.serial_disk_ptr is stamped D. Case A (safe): the evictor looks up the registry (D), reads N's stamp (D), sees D==D, and unswizzles the child to OnDisk(D) — safe, because readers can fault it back in from D. Case B (race): a concurrent writer path-copies N into N′ and publishes a new root, so N′'s stamp differs from D; the evictor looks up the registry (still D, stale), reads the live node N′'s stamp ($\ne$ D), sees the mismatch, and backs off — refusing to publish the stale image that would hide the writer's update." width="100%"/>

This is model-checked by `OverlayEvictionCas.tla` and `OverlayEvictionStale.tla` (each with
an `_Unsafe` negative control that removes the stamp and exhibits the loss). The full
subsystem is in [eviction.md](eviction.md).

## Guarantees & proofs

| Guarantee | Where proved |
|-----------|--------------|
| Deadlock-freedom of the F4 hierarchy | `tests/persistent_lockfree_f4_lock_hierarchy_loom.rs`, `tests/vocab_lockfree_f4_lock_hierarchy_loom.rs` |
| Linearizability of shared reads/writes/checkpoints (byte/char/vocab) | `SharedPersistentConcurrency.tla`, `ConcurrentVocabLinearizability.tla`, `LockFreeARTrieLinearizability.tla` |
| No lost update on the value/counter CAS | `LockFreeOverlayValueCas.tla`, `LockFreeCounterMergeAtomicity.tla` |
| Reads observe only completed visible state | `SharedPersistentConcurrency.tla` (`ReadsObserveCompletedVisibleState`) |
| Eviction stamp safety | `OverlayEvictionCas.tla`, `OverlayEvictionStale.tla` |

The full invariant $\leftrightarrow$ model $\leftrightarrow$ proof correspondence is in
[formal-verification-map.md](formal-verification-map.md).

> **Status.** The lock-free/F4 design described here is the **current, verified**
> architecture. The byte and char collapse is committed; the vocab-F4 extension is complete
> and model-checked (`SharedPersistentConcurrency.tla` clean; the three `vocab_*` loom/
> concurrency tests pass) and is landing in the working tree at time of writing.

## References

- M. Herlihy, N. Shavit. *The Art of Multiprocessor Programming.* Morgan Kaufmann, 2008
  (lock-freedom, linearizability).
- K. Fraser. *Practical Lock-Freedom.* PhD thesis, University of Cambridge, 2004 (EBR).
