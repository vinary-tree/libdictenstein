# The lock-free overlay — the live representation

**Navigation**: [↑ Persistence architecture](README.md) · [Durability & recovery](durability-and-recovery.md) · [Concurrency model](concurrency-model.md) · [Families](families.md) · [Core abstractions](../architecture/abstractions.md)

The **overlay** is the heart of the persistent-ARTrie family: a single generic,
**immutable, copy-on-write** radix trie that is the *live* representation of every variant.
There is no separate "owned" tree behind it — since the L3.3 campaign the overlay *is* the
production structure. This document covers its node model, its lock-free write and read
paths, and why both are correct with **zero `unsafe`**.

## Terms of art (defined before first use)

| Term | Definition |
|------|-----------|
| **immutable / persistent (data structure)** | A structure whose nodes are never mutated after publication; an update produces a *new* version that shares unchanged subtrees with the old one (Driscoll et al. 1989). "Persistent" here is the data-structure sense, distinct from "persistent" = on-disk. |
| **path copying** | The technique that realizes persistence: to change a leaf, copy just the root-to-leaf spine ($`O(m)`$ nodes for a key of length $`m`$) and share everything else by pointer. |
| **CAS** | *Compare-And-Swap* — atomically replace $`x_{\text{old}}`$ with $`x_{\text{new}}`$ only if the cell still holds $`x_{\text{old}}`$. |
| **arc-swap** | `arc_swap::ArcSwapOption<T>` — a genuinely-atomic, lock-free cell holding an `Option<Arc<T>>`; readers `load` without a lock, writers replace by CAS, and reclamation is hazard-protected. |
| **fault-in** | Loading an evicted, on-disk child back into memory on access. |

## The immutable node

Every variant's node is one generic type, `OverlayNode<K: KeyEncoding, V>`
(`core/overlay/node.rs`) — the G4 unification that replaced two token-for-token-identical
byte and char nodes (they differed only in `K::Unit`, `K::MAX_PREFIX_LEN`, and
`K::UNIT_ZERO`). Byte and char alias it:

```rust
// byte:  pub type PersistentNode<V = ()>     = OverlayNode<ByteKey, V>;
// char:  pub type PersistentCharNode<V = ()> = OverlayNode<CharKey, V>;
// vocab: consumes the char alias at V = u64
```

Its fields:

| Field | Type | Role |
|-------|------|------|
| `store` | `ChildStore<K, V>` (a newtype over `AdaptiveEdgeStore<K::Unit, Child<K, V>>`) | the tiered child map (see [storage-backends.md](storage-backends.md#adaptive-edge-storage)) |
| `value` | `Option<V>` | the **immutable** leaf value (membership uses `V = ()`, counting uses `u64`) |
| `prefix` | `Arc<[K::Unit]>` (+ `prefix_len`) | the path-compressed prefix, capped at `K::MAX_PREFIX_LEN` |
| `flags` | `AtomicU8` | `IS_FINAL` / `IS_DIRTY` / `IS_LEAF` / `HAS_VALUE` |
| `version` | `AtomicU64` | optimistic-lock / diagnostic version counter |
| `serial_disk_ptr` | `AtomicU64` | the **durable-location stamp** — the eviction-safety lynchpin (§[Eviction safety](#eviction-safety-the-serial_disk_ptr-stamp)) |

### The owned `Child` enum — the leak fix, with zero `unsafe`

A node's children are an owned enum, not a bare pointer:

```rust
pub enum Child<K: KeyEncoding, V = ()> {
    InMem(Arc<OverlayNode<K, V>>),  // owned; reclaimed by Arc refcount on drop
    OnDisk(SwizzledPtr),            // a serialized block location; Drop is a no-op
}
```

The overlay *used to* smuggle an in-memory `Arc` through a `SwizzledPtr` (a `u64`) via
`Arc::into_raw`. Because a `SwizzledPtr` is a plain integer with no `Drop`, **every
superseded node version leaked its children** — the refcount was never decremented — and
every traversal needed `unsafe { Arc::from_raw(..) }`. The `Child` enum makes ownership
explicit: dropping a parent drops its `InMem` `Arc`s, so a child is freed *exactly* when no
live version references it (including versions still held by concurrent readers through the
arc-swap root). Reclamation falls out of ordinary `Arc` refcounting — **no epoch machinery
is required for correctness**, and there is **no `unsafe`**.

## One node, three alphabets

The overlay is generic over `K: KeyEncoding`, so `OverlayNode<ByteKey, V>`,
`OverlayNode<CharKey, V>`, and `OverlayNode<U64Key, V>` are distinct, independently
optimized monomorphizations of *one* body. A single blanket
`impl<K, V> TrieRoot for OverlayNode<K, V>` gives the MVCC snapshot interface for every
alphabet at once. The seam is detailed in [abstractions.md](../architecture/abstractions.md)
and [families.md](families.md#one-implementation-three-alphabets).

## The write path — path-copy, then publish by CAS

A writer never mutates a node. It path-copies the touched spine and publishes a new root:

<img src="../diagrams/path-copy.svg" alt="A copy-on-write diagram. The old root R0 (grey) points to child A (grey) and subtree B (green); A points to leaf C (green) and subtree D (green). An insert through the a→c path produces a new teal spine: a new root R1 with a copy A′ and a new final leaf C′. Crucially, R1 also points to the SAME subtree B (a shared green pointer, no copy) and A′ points to the SAME subtree D (shared) — only the R0→A→C spine is duplicated (O(key length) new nodes). The arc-swap root cell publishes the change with a single red compare_exchange(R0, R1) edge, labelled the linearization point." width="90%"/>

The publication is a single CAS on the `AtomicNodePtr` root; the **winning CAS is the
linearization point**. On a lost race the writer re-reads the current root and retries on
the newer base. The full descent-and-publish skeleton — claim the commit generation, load
the root, find the leaf (faulting an evicted `OnDisk` child if needed), build the CoW
spine, publish, retry-without-re-appending — is the shared `OverlayCasWalk`:

<img src="../diagrams/cas-walk.svg" alt="A CAS-walk publish sequence: the writer claims a commit generation, loads the current root, descends to find the target leaf, resolves-or-faults an evicted OnDisk child, builds the copy-on-write spine, publishes via try_set_final / a root CAS, and on a lost CAS retries WITHOUT re-appending the WAL record; once the CAS wins the leaf is visible." width="90%"/>

Concurrent finalization of the *same* node is arbitrated by `OverlayNode::try_set_final`, an
`AtomicU8` `fetch_or` that exactly one thread wins. This write path is wrapped by the
**Order-A durability protocol** (append the WAL record *before* publishing) — see
[durability-and-recovery.md](durability-and-recovery.md#the-order-a-write-protocol).

## The read path — a hazard-protected snapshot, no lock

Readers never take a lock. `overlay_root_node()` is `AtomicNodePtr::load()`, which is
`ArcSwapOption::load_full()` — a **hazard-protected** clone of the current root `Arc`.
Because nodes are immutable and children are owned `Arc`s, a reader that has loaded a root
sees a complete, self-consistent point-in-time tree even while writers publish new roots.
A point read walks children via `find_child` → `Child::InMem`; a read that reaches a
`Child::OnDisk` arm **faults the subtree in** (the bug-#46 fix: an in-process eviction can
flip an interior node to `OnDisk` and hide finals beneath it, so the read path must fault,
not stop). Enumeration, `len`, and prefix walks recurse the resident-finals tree, faulting
evicted path nodes read-only.

### Why the root cell is both lock-free *and* sound

`AtomicNodePtr` stores its `Arc` in `arc_swap::ArcSwapOption` (`core/overlay/atomic_ptr.rs`).
This is the *third* iteration, and the reasoning is worth preserving:

1. **`AtomicU64` of a raw `Arc` pointer** — lock-free but **unsound**: `load()` can race a
   replacement and try to increment a freed allocation.
2. **`RwLock<Arc<..>>`** — sound but reintroduces a **lock** on every "CAS".
3. **`ArcSwapOption`** — sound *and* lock-free: `load_full()` is protected by arc-swap's
   guarded reclamation, so a reader never touches a freed allocation, and no lock serializes
   readers against writers. `compare_exchange` swaps only when the stored `Arc` is
   pointer-equal to the expected one.

## Eviction safety: the `serial_disk_ptr` stamp

`serial_disk_ptr` (the M-2a lynchpin) is the durable-location stamp that makes eviction
race-free against concurrent readers: a node is safe to unswizzle to disk *iff* its stamp
still equals its registered disk pointer. The full argument — including why a stale image
cannot be re-published — is in
[concurrency-model.md](concurrency-model.md#eviction-safety-the-serial_disk_ptr-stamp) and
[eviction.md](eviction.md), and is model-checked by `OverlayEvictionCas.tla` /
`OverlayEvictionStale.tla`.

## Cost and correctness summary

| Property | Statement |
|----------|-----------|
| Insert allocation | $`O(m)`$ new node versions for a key of length $`m`$; all other subtrees shared by `Arc`. |
| Read | $`O(m)`$ pointer chases on an immutable snapshot; wait-free, lock-free, no allocation. |
| Reclamation | `Arc` refcount; a version is freed when its last reader drops it. No `unsafe`, no epoch needed *for correctness*. |
| Linearizability | the winning root CAS is the single linearization point; proved in `LockFreeARTrieLinearizability.tla` and Loom. |

The proof correspondence for the overlay's CAS, value-CAS, and remove-CAS invariants is
catalogued in [formal-verification-map.md](formal-verification-map.md). The mechanism-level
design records are [`overlay-backed-dictionary-node.md`](../design/overlay-backed-dictionary-node.md),
[`g4-unify-overlay-node.md`](../design/g4-unify-overlay-node.md), and
[`lockfree-cas-artrie.md`](../design/lockfree-cas-artrie.md).

## References

- J. Driscoll, N. Sarnak, D. Sleator, R. Tarjan. *Making data structures persistent.*
  JCSS 38(1), 1989. [DOI:10.1016/0022-0000(89)90034-2](https://doi.org/10.1016/0022-0000(89)90034-2)
- V. Leis, A. Kemper, T. Neumann. *The Adaptive Radix Tree.* ICDE 2013.
  [DOI:10.1109/ICDE.2013.6544812](https://doi.org/10.1109/ICDE.2013.6544812)
