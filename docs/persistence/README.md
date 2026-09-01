# Persistence Architecture — the durable, lock-free ARTrie engine

**Navigation**: [↑ Documentation index](../README.md) · [Core abstractions](../architecture/abstractions.md) · [Disk-trie theory](../theory/disk-tries/) · [Formal verification](../../formal-verification/)

This is the **architect's entry point** to `libdictenstein`'s persistence layer: one
place to see the whole disk-backed dictionary engine — how a write becomes durable,
how a read stays lock-free, how a crash is survived, and how the pieces compose — before
descending into any single subsystem. Each layer below links to its deep-dive; the depth
lives there, so this page stays a map, not a manual.

> **One-sentence summary.** The persistent-ARTrie family is a **lock-free, crash-safe,
> disk-backed** Adaptive Radix Trie: writes are made **durable in a write-ahead log
> before they become visible** (so *$`\text{acknowledged}\implies\text{durable}`$*), reads traverse an
> **immutable copy-on-write overlay** with no lock, and memory is bounded by evicting cold
> subtrees to disk and faulting them back on demand.

---

## Terms of art (defined before first use)

| Term | Definition |
|------|-----------|
| **ART** | *Adaptive Radix Tree* — a trie whose interior nodes adapt their fan-out representation to their child count ($`4/16/48/256`$ slots), keeping each node compact (Leis et al. 2013). |
| **overlay** | The single **immutable, path-copied** radix trie that is the *live* representation of the dictionary. New versions are published atomically; readers see a consistent snapshot. |
| **CAS** | *Compare-And-Swap* — an atomic "swap $`x`$ to $`y`$ only if it still holds $`x_{\text{old}}`$" primitive. The overlay's root pointer is published by CAS; the winning CAS is the write's linearization point. |
| **WAL** | *Write-Ahead Log* — an append-only, `fsync`-durable log of operations written *before* a change is made visible, so a crash can be replayed (Mohan et al. 1992, ARIES). |
| **LSN** | *Log-Sequence Number* — the monotonically increasing position of a record in the WAL. |
| **checkpoint** | A periodic background fold of the live overlay into a dense on-disk *image*, after which the WAL below the checkpoint can be reclaimed. |
| **watermark** | The largest LSN $`L`$ such that *every* LSN in $`1..=L`$ has committed — the only safe `checkpoint_lsn` under out-of-order lock-free commit. |
| **swizzling** | Storing a child link as *either* an in-memory pointer *or* an on-disk block location in one word, so subtrees load lazily. |
| **CX image** | The **compact snapshot** — the dense, path-compressed on-disk *image* a checkpoint folds the live overlay into (magic `AR64CX01`). "CX" is the format's codename, not an acronym. |

---

## The whole stack, on one page

A **write** descends the spine — it must reach a durable WAL record *before* the overlay
publishes it. A **read** traverses *only* the lock-free overlay — never the WAL, buffer,
or disk on the hot path. A **reopen** rebuilds the overlay from the checkpoint image plus
the retained WAL tail. Two concerns cut across every layer: **concurrency** (the handles
are bare `Arc`s; reads *and* writes are lock-free) and **eviction** (memory pressure
unswizzles cold subtrees to disk).

The stack is drawn as two companion figures so each fits the prose column: part ① is the six layers with the live WRITE and READ paths; part ② is the checkpoint fold, the reopen replay, and the cross-cutting concerns.

<img src="../diagrams/persistence-stack.svg" alt="The persistent-ARTrie durability stack, part ① of ② — the six layers and the live WRITE and READ paths. Layer ① Client API (green) exposes PersistentARTrie/…Char/…U64/…Vocab with insert/upsert/get/iter_prefix. Layer ② the lock-free overlay engine (teal, core/overlay/) holds the immutable OverlayNode with an arc-swap AtomicNodePtr root, the copy-on-write cas_walk, and the Order-A durable_write skeleton — the live representation. Layer ③ WAL + durability (orange, core/wal + durability.rs + committed_watermark.rs) appends and fsyncs before publish and computes the safe checkpoint_lsn. Layer ④ the buffer manager (grey infrastructure) pools and flushes pages. Layer ⑤ the disk manager (blue, the BlockStorage seam) is MmapDiskManager by default or IoUringDiskManager. Layer ⑥ arena/block storage (blue) places 256 KB aligned blocks with swizzled pointers. Solid down-arrows trace a write descending the spine — write enters, the orange WAL append+fsync gate before publish, then stage pages, flush blocks, and arena slots; a dashed teal arrow traces a read into the overlay only, never touching WAL or disk. The checkpoint fold, the reopen replay, and the cross-cutting concurrency and eviction concerns are the companion figure persistence-stack-2." width="100%"/>

<img src="../diagrams/persistence-stack-2.svg" alt="The persistent-ARTrie durability stack, part ② of ② — the checkpoint fold, the reopen replay, and the cross-cutting concerns. Layers ② overlay, ⑤ disk, and ⑥ arena appear here as anchors (their sub-module detail is in part ①). An indigo checkpoint box folds the overlay into a dense CX image and publishes it to disk at checkpoint_lsn = the committed watermark, via a dashed indigo snapshot arrow then a publish-image-and-advance-watermark arrow. A dashed blue REOPEN arrow rebuilds the overlay from the disk image plus the retained WAL tail. A red cross-cutting concurrency box notes the handles are bare Arc, reads and writes lock-free, with the CK > merge > EC lock hierarchy for checkpoint/merge/eviction only, and governs the overlay; a grey eviction box unswizzles cold subtrees on memory pressure and faults them back on read (serial_disk_ptr), and reclaims the overlay. The six layers ①–⑥ with the WRITE and READ paths are the companion figure persistence-stack." width="100%"/>

---

## A guided tour of the layers

Follow the numbered layers of the figure top-to-bottom; each links to its deep-dive.

### ① Client API — what a caller touches
The family is four durable ARTrie **profiles** plus a substring family, all behind the
same [dictionary trait layer](../algorithms/README.md):

- `PersistentARTrie<V>` — byte keys (`u8`) through `ByteKey`.
- `PersistentARTrieChar<V>` — Unicode scalar keys (`u32`) through `CharKey`.
- `PersistentARTrieU64Compact<V>` — native 64-bit sequence keys through `U64Key`.
- `PersistentVocabARTrie` — a durable `term $\leftrightarrow$ u64` bijection.

The full profile table, the suffix-graph family, and the module **layering invariant**
are in **[families.md](families.md)**.

### ② Lock-free overlay engine — the live representation
The heart. A single generic, **immutable** `OverlayNode<K, V>` (`core/overlay/node.rs`)
whose root cell `AtomicNodePtr<K, V>` (an `arc_swap::ArcSwapOption`) is published by CAS.
A writer **path-copies** the touched spine and swaps the root; a reader loads a
hazard-protected `Arc` snapshot and walks it with no lock. One implementation serves
byte, char, and `u64` via the `K: KeyEncoding` seam. Deep-dive:
**[lock-free-overlay.md](lock-free-overlay.md)**.

### ③ WAL + durability control — "log before you publish"
Every state-changing write appends an `fsync`-durable WAL record (`core/wal/`) *before*
the overlay publishes it, so **acknowledged $`\implies`$ durable**. The
`CommittedWatermark` (`core/committed_watermark.rs`) tracks the contiguous committed
prefix — the only safe `checkpoint_lsn`. Policies (`DurabilityPolicy`) trade latency for
batching. The on-disk record codec is in **[wal-format.md](wal-format.md)**; the write
protocol and recovery are in **[durability-and-recovery.md](durability-and-recovery.md)**.

### ④ Buffer manager — the page cache
`BufferManager` (`core/buffer_manager.rs`) pools fixed-size frames over the block
backend, faults pages in on demand, and batches dirty-page flushes (tracked by
`DirtyTracker`). Covered under **[storage-backends.md](storage-backends.md)**.

### ⑤ Disk manager — the `BlockStorage` seam
Everything above is generic over `S: BlockStorage`. The default `MmapDiskManager` wins
on single-block I/O; the feature-gated `IoUringDiskManager` (`O_DIRECT`, batched async)
wins on batch I/O and true durability. See **[storage-backends.md](storage-backends.md)**.

### ⑥ Arena · block storage — bytes on disk
Nodes are packed into 256 KB `AlignedBlock`s in arena pages; high-fan-out string leaves
use B-trie buckets; child links are **swizzled** pointers. The full on-disk format —
block-0 `FileHeader`, arena pages, buckets — is in **[storage-backends.md](storage-backends.md)**.

### Checkpoint — the periodic background fold
A checkpoint captures the immutable overlay snapshot into a dense **CX image**, publishes
it, and advances the reclaimable WAL watermark to `checkpoint_lsn = committed watermark`.
Mechanics: **[durability-and-recovery.md](durability-and-recovery.md)**.

### Cross-cutting: concurrency
`SharedARTrie` / `SharedCharARTrie` / `SharedVocabARTrie` are **bare `Arc<T>`** — there is
no outer `RwLock`; reads *and* writes are lock-free. Locks serialize *only* checkpoint,
merge, and eviction, under a strict acyclic hierarchy
$`\text{CK} > \text{merge\_lock} > \text{EC}`$. Deep-dive:
**[concurrency-model.md](concurrency-model.md)**.

### Cross-cutting: eviction
Under memory pressure the eviction subsystem (`core/eviction/`) unswizzles cold overlay
subtrees to disk; reads fault them back in. The `serial_disk_ptr` stamp is the safety
lynchpin that makes eviction race-free against concurrent readers. Deep-dive:
**[eviction.md](eviction.md)**. The character-node V2/V3 compatibility and
migration contract is specified separately in
**[char-node-format.md](char-node-format.md)**.

---

## The three data-flow paths and what they guarantee

| Path | Route | Guarantee |
|------|-------|-----------|
| **Write** | API → overlay → **WAL append + fsync** → root-CAS publish → commit-rank → mark watermark | *Acknowledged $`\implies`$ durable.* The winning root-CAS is the **linearization point**. |
| **Read** | API → published overlay root only | Snapshot-consistent, lock-free, wait-free traversal; no WAL/disk on the hot path. |
| **Reopen** | load checkpoint image → replay WAL tail with $`\text{LSN} > n`$ → reconcile per-term commit order | Recovery reproduces *exactly* the visible-and-acknowledged pre-crash state — no lost writes, no invented state. |

Here $`n`$ is the image-coverage frontier stamped in the block-0 header: the maximum WAL LSN
already folded into the on-disk image, so replay skips $`\text{LSN} \le n`$ to avoid
double-applying a checkpointed record.

### The load-bearing invariants (all machine-checked)

```math
\text{visible}(t) \;\implies\; \text{WAL-durable}(t) \ \text{at}\ \mathrm{lsn}(t) \le \mathrm{syncedLsn}
```

```math
\text{checkpoint\_lsn} \;=\; \max\{\,L : \forall\, \ell \in 1..=L,\ \text{committed}(\ell)\,\}
\quad(\text{a committed contiguous prefix})
```

```math
\text{RecoveredMap} \;=\; \text{durableCheckpoint} \,\cup\, \text{WAL-tail}[\,\text{walRetainedFrom},\ \mathrm{syncedLsn}\,]
\;=\; \text{visible}
```

These are proven in TLA⁺ (`SharedPersistentConcurrency.tla`, and the `LockFree*` /
`Concurrent*` model family) and Rocq (the `Spec/Persistent*` specs). The
invariant $`\leftrightarrow`$ model $`\leftrightarrow`$ proof correspondence is catalogued in
**[formal-verification-map.md](formal-verification-map.md)**.

---

## Reading map

- **Build on this substrate** → **[durable-storage-kernel.md](durable-storage-kernel.md)** —
  `core/` presented as a *reusable* durable-storage engine, for authoring a new persistent
  file layer that is not a dictionary.
- **The family & module structure** → [families.md](families.md).
- **The lock-free overlay** → [lock-free-overlay.md](lock-free-overlay.md).
- **Durability, checkpoint & recovery** → [durability-and-recovery.md](durability-and-recovery.md).
- **Concurrency model** → [concurrency-model.md](concurrency-model.md).
- **Storage backends & on-disk format** → [storage-backends.md](storage-backends.md) ·
  [wal-format.md](wal-format.md).
- **Memory-pressure eviction** → [eviction.md](eviction.md).
- **Group commit (experimental)** → [group-commit.md](group-commit.md).
- **Proof correspondence** → [formal-verification-map.md](formal-verification-map.md).
- **Theory foundations** → [disk-trie theory](../theory/disk-tries/) (trie → B-trie → ART →
  persistent-ART → buffer management → PART).
- **Core generic abstractions** → [abstractions.md](../architecture/abstractions.md)
  (`CharUnit` + `KeyEncoding`).

## References

- V. Leis, A. Kemper, T. Neumann. *The Adaptive Radix Tree: ARTful Indexing for
  Main-Memory Databases.* ICDE 2013. [DOI:10.1109/ICDE.2013.6544812](https://doi.org/10.1109/ICDE.2013.6544812)
- N. Askitis, J. Zobel. *B-tries for disk-based string management.* The VLDB Journal
  18(1), 2009. [DOI:10.1007/s00778-008-0094-1](https://doi.org/10.1007/s00778-008-0094-1)
- J. Driscoll, N. Sarnak, D. Sleator, R. Tarjan. *Making data structures persistent.*
  JCSS 38(1), 1989. [DOI:10.1016/0022-0000(89)90034-2](https://doi.org/10.1016/0022-0000(89)90034-2)
- C. Mohan, D. Haderle, B. Lindsay, H. Pirahesh, P. Schwarz. *ARIES: A Transaction
  Recovery Method…* ACM TODS 17(1), 1992. [DOI:10.1145/128765.128770](https://doi.org/10.1145/128765.128770)
