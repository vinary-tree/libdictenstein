# Eviction Strategy for Persistent ARTrie

**Navigation**: [↑ Persistence architecture](README.md) · [Concurrency model](concurrency-model.md) · [Lock-free overlay](lock-free-overlay.md) · [Storage backends](storage-backends.md)

This document describes the memory pressure-driven eviction system for the persistent ARTrie (Adaptive Radix Trie) data structure: **what** each component is, **how** the pieces cooperate to reclaim RAM, and **why** the design is safe under lock-free concurrent reads.

### What eviction is, in one paragraph

The persistent ARTrie keeps hot dictionary entries resident in memory for native-speed reads, but a dictionary can be far larger than RAM. *Eviction* is the mechanism that bounds the resident set: when the operating system reports memory pressure, a background thread converts the coldest in-memory nodes back into compact on-disk references (`DiskRef`), freeing their RAM. The node's bytes are not lost — they were already written to disk at the last checkpoint, so a future access simply faults the node back in. The hard part is doing this **without blocking readers and without freeing memory a concurrent reader could still be dereferencing**; that safety property is delivered by *epoch-based reclamation* (EBR), defined below.

### Glossary (terms used throughout)

| Term | Definition |
|------|------------|
| **EBR** — epoch-based reclamation | A safe-memory-reclamation scheme. Readers announce they are active by entering a global *epoch*; a reclaimer that wants to free a node advances the epoch and then waits until every reader from the old epoch has departed before freeing, so no reader can hold a dangling pointer. Implemented by `EpochManager` in `core/concurrency.rs`. |
| **Quiescence** | The condition in which no reader from the pre-eviction epoch is still active (`active_readers == 0` after an `advance()`). Reaching quiescence is the precondition for freeing an evicted node. |
| **LRU** — least recently used | The selection policy that ranks nodes by *coldness* (a recency-and-frequency score) so the coldest nodes are evicted first and hot nodes stay resident. Implemented by `LruRegistry` in `core/eviction/lru_tracker.rs`. |
| **Urgency** | How aggressively a single eviction pass should run, derived from the memory-pressure level. The `EvictionUrgency` enum has three rungs — `Moderate`, `Urgent`, `Emergency` — that scale the batch size ($`\times 1`$/$`\times 2`$/$`\times 4`$). |
| **Swizzle / unswizzle** | A *swizzled* child pointer points at a live in-memory node (fast path); an *unswizzled* pointer is a compact `DiskRef` naming a `block_id` + location on disk. *Swizzling* faults a node in (disk → memory); *unswizzling* is the atomic swap eviction performs (memory → disk). Implemented by `SwizzledPtr` in `core/swizzled_ptr.rs`. |
| **Pin / unpin** | A buffer-pool *frame* is *pinned* while a lease (read or write) is held on it; a pinned frame may not be evicted from the pool. *Unpinning* releases the lease. |
| **Checkpoint** | The durable write of the trie to disk that (re)populates the `DiskLocationRegistry`. Only nodes registered by the most recent checkpoint are eligible for eviction. |

## Table of Contents

1. [Overview & Motivation](#overview--motivation)
2. [Architecture](#architecture)
3. [Component Documentation](#component-documentation)
4. [Data Flow](#data-flow)
5. [Concurrency & Safety](#concurrency--safety)
6. [Configuration Guide](#configuration-guide)
7. [API Reference](#api-reference)
8. [Edge Cases & Error Handling](#edge-cases--error-handling)
9. [Statistics & Monitoring](#statistics--monitoring)
10. [Source Files](#source-files)

---

## Overview & Motivation

### Problem Statement

The persistent ARTrie stores dictionary entries in memory for fast access. Without bounds on memory usage, large dictionaries can exhaust available RAM, leading to:

- Out-of-memory (OOM) crashes
- Excessive swapping and degraded performance
- Inability to process dictionaries larger than available RAM

### Solution: SQLite-Style Memory Management

The eviction system implements SQLite-style bounded memory operation:

1. **Memory pressure-driven** - Eviction is triggered by system memory pressure, not after every checkpoint
2. **Asynchronous** - Background eviction thread, non-blocking for client operations
3. **Epoch-based safety** - Uses `EpochManager` to safely evict nodes without blocking readers
4. **LRU-based selection** - Evicts "cold" (least recently used) nodes first, keeping hot data in memory

### Key Principles

| Principle | Description |
|-----------|-------------|
| **Non-blocking** | Client operations (insert, lookup, iterate) are never blocked by eviction |
| **Epoch-safe** | Nodes are only evicted after all old-epoch readers complete |
| **LRU-ordered** | Cold nodes evicted first; hot nodes stay in memory |
| **Checkpoint-aware** | Only nodes with valid disk representations can be evicted |
| **Configurable** | Thresholds, batch sizes, and timing are all tunable |

---

## Architecture

**What.** Four cooperating components, all owned by `PersistentARTrie<V>`: a `MemoryPressureMonitor` that watches the OS, an `EvictionCoordinator` that queues and serializes work, a background *eviction thread* that performs the reclamation, and two indices — the `LruRegistry` (which nodes are cold) and the `DiskLocationRegistry` (which nodes have a current disk image and may therefore be evicted).

**How.** The data flows in one direction: the monitor detects pressure and fires a callback; the callback maps the pressure *level* to an `EvictionUrgency` and enqueues a request; the eviction thread dequeues it, waits for epoch quiescence, asks the registries for the coldest evictable nodes, atomically unswizzles each ($`\text{ChildNode} \to \text{DiskRef}`$), and records statistics. The figure below traces that path end-to-end; the urgency bands are colored amber → red by severity.

<img src="../diagrams/eviction-pipeline.svg" alt="Eviction pipeline: pressure band to urgency to queue to async thread to quiescence to LRU select to unswizzle" width="980"/>

*Figure 1 — The node-eviction pipeline. `MemoryPressureMonitor` classifies available RAM into `Normal`/`Low`/`Critical`; `request_eviction` maps `Low` $`\Rightarrow`$ `Moderate` and `Critical` $`\Rightarrow`$ `Emergency` (`Normal` is a no-op); the async `artrie-eviction` thread runs `cooldown → wait_for_quiescence → select_for_eviction (LRU) → atomic unswizzle → record stats`, after which the cold node lives on disk as a `DiskRef` and is re-faulted on next access.*

**Why this shape.** Detection, policy, and mechanism are separated so each can be tuned independently: the monitor's thresholds bound *when* eviction starts, the LRU policy decides *what* leaves first, and the epoch machinery makes the *mechanism* safe. Making the eviction thread asynchronous (rather than evicting inline after each checkpoint, the way naïve bounded caches do) keeps client `insert`/`lookup`/`iterate` latency off the eviction critical path.

> **Note — `MemoryPressureLevel` vs `EvictionUrgency`.** They are distinct enums. `MemoryPressureLevel` (`Normal`, `Low`, `Critical`) describes the *system*; `EvictionUrgency` (`Moderate`, `Urgent`, `Emergency`) describes *how hard a pass works*. The monitor callback in `coordinator.rs` performs the mapping: $`\text{Normal} \Rightarrow`$ no request, $`\text{Low} \Rightarrow \text{Moderate}`$, $`\text{Critical} \Rightarrow \text{Emergency}`$. (The `Urgent` rung exists for callers that invoke `request_eviction(EvictionUrgency::Urgent)` directly.)

---

## Component Documentation

### EvictionConfig

Configuration structure controlling eviction behavior.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Master switch for eviction |
| `target_memory_fraction` | `f64` | `0.70` | Target available memory after eviction (50%-90%) |
| `min_eviction_depth` | `usize` | `1` | Minimum trie depth for eviction (0=all, 1=keep root children) |
| `batch_size` | `usize` | `256` | Nodes processed per eviction cycle (16-4096) |
| `quiescence_timeout` | `Duration` | `100ms` | Max wait for epoch quiescence |
| `quiescence_poll_interval` | `Duration` | `100us` | Polling interval during quiescence wait |
| `cooldown_period` | `Duration` | `100ms` | Minimum time between eviction cycles |
| `use_lru_tracking` | `bool` | `true` | Enable LRU-based node selection |
| `enable_memory_pressure_monitor` | `bool` | `true` | Auto-start memory pressure monitoring |
| `memory_pressure_config` | `Option<MemoryPressureConfig>` | `None` | Custom memory pressure thresholds |
| `resident_budget_bytes` | `Option<usize>` | `None` | Post-checkpoint resident-overlay budget in serialized bytes plus the family-specific structural overhead |
| `resident_budget_eviction_cap` | `Option<usize>` | `None` | Maximum positive-gain resident records admitted by one checkpoint-tail pass; `None` is uncapped |

**Source:** `src/persistent_artrie/core/eviction/config.rs` (`EvictionConfig`)

### EvictionCoordinator

The central orchestrator for asynchronous, epoch-safe node eviction.

```rust
pub struct EvictionCoordinator {
    config: EvictionConfig,
    epoch_manager: Arc<EpochManager>,
    lru_registry: Arc<LruRegistry>,
    request_queue: Mutex<VecDeque<EvictionRequest>>, // polled by the Weak-driven worker; no condvar
    shutdown: AtomicBool,
    eviction_thread: Mutex<Option<JoinHandle<()>>>,
    stats: Arc<EvictionStatsAtomic>,
    last_eviction: AtomicU64,
    disk_registry: RwLock<DiskLocationRegistry>,
    running: AtomicBool,
    memory_monitor: RwLock<Option<Arc<MemoryPressureMonitor>>>,
}
```

The worker thread holds only a `Weak<EvictionCoordinator>` and polls `request_queue` ($`\approx`$`100 ms`), so the coordinator can be dropped promptly by its owning trie — the earlier `Condvar`-based design was removed because pinning a strong `Arc` in the worker leaked one OS thread per trie instance.

**Key Methods:**

| Method | Description |
|--------|-------------|
| `new(config, epoch_manager)` | Create coordinator in stopped state |
| `start(callback)` | Deprecated detached byte-callback worker |
| `start_char(callback)` | Deprecated detached character-callback worker |
| `start_memory_monitor()` | Enable automatic memory pressure monitoring |
| `request_eviction(urgency)` | Queue an eviction request |
| `force_eviction(target_bytes)` | Synchronous eviction for testing |
| `try_install_detached_compatibility_catalog(registry)` | Install an advisory, non-authoritative legacy catalog |
| `clear_detached_compatibility_catalog()` | Atomically clear only the detached advisory catalog |
| `shutdown()` | Stop eviction thread and memory monitor |

**Source:** `src/persistent_artrie/core/eviction/coordinator.rs` (`EvictionCoordinator`)

### LruRegistry

Concurrent sharded registry for tracking node access patterns using DashMap.
DashMap operations may take internal shard locks; the planned exact path-ID
tracker replaces this with atomic stamps for the scalable policy.

```rust
pub struct LruRegistry {
    trackers: DashMap<u64, AccessTracker>,
    epoch_start: Instant,
    max_entries: usize,
}
```

**Key Methods:**

| Method | Description |
|--------|-------------|
| `touch(path)` | Record access for a byte path |
| `touch_hash(hash)` | Record access with pre-computed hash |
| `coldness_score(path)` | Get coldness score (higher = evict first) |
| `coldness_score_hash(hash)` | Coldness score with pre-computed hash |
| `coldest_n(n)` | Get N coldest path hashes |
| `prune_to(target_size)` | Remove coldest entries to reach target |
| `path_hash(path)` | Compute FNV-1a hash for a path |

**Memory Overhead:** ~32 bytes per tracked node (8 bytes hash + 16 bytes tracker + 8 bytes DashMap overhead)

**Source:** `src/persistent_artrie/core/eviction/lru_tracker.rs` (`LruRegistry`)

### AccessTracker

Lightweight atomic tracker for individual node access patterns.

```rust
pub struct AccessTracker {
    last_access: AtomicU64,   // Epoch-relative microseconds
    access_count: AtomicU64,  // Total accesses (tie-breaker)
}
```

**Coldness Score Calculation:**

```
coldness = (now - last_access) / max(access_count, 1)
```

Higher coldness scores indicate nodes that should be evicted first (older, less frequently accessed).

**Source:** `src/persistent_artrie/core/eviction/lru_tracker.rs` (`AccessTracker`)

### DiskLocationRegistry

The registry is a generation-qualified structural index produced by checkpoint
publication. It does not retain one independently allocated absolute path per
record. Byte and character paths live in immutable, segmented preorder
`PathTopology` tables; each entry stores only its parent, local segment, depth,
hash, and finalized subtree end. Dense `Arc<Vec<Option<...>>>` record tables map
topology identifiers to disk pointers, serialized sizes, and node types. Exact
disk-address indexes support fault and graft resolution, while compact bitsets
track point residency independently of durable structural metadata.

The authority state is deliberately separate from structural validity:

| Builder state | Structural inspection | Exact eviction/fault authority |
|---------------|-----------------------|--------------------------------|
| `Detached` | Allowed | None |
| `Publishing` | Rejected | None |
| `Valid` | Allowed | None by itself |
| `Invalid` | Rejected | None |

Exact authority lives only in the currently published root revision. Its
immutable `PublishedRegistryCatalog`, generation identity, packed residency
ordinal, and resident totals travel together in that revision. The mutable
builder registry is retained for checkpoint construction, compatibility
inspection, and cold lifecycle bookkeeping; no method on it can authorize an
exact eviction or fault transition.

A `CompactEvictionBatch` carries path identifiers plus the immutable topology
and exact generation that give those identifiers meaning. Absolute paths are
materialized only at compatibility boundaries. The exact byte/character
checkpoint paths use compact batches throughout, so selection performs no
per-candidate path allocation.

#### Owned compatibility inspection

`DiskLocationRegistry` exposes `get_owned(path_hash)` for byte paths and
`get_char_owned(path_hash)` for character paths. These names make the ownership
and cost boundary explicit: a successful call reconstructs one absolute path
from the segmented topology and returns an independent `EvictableNode` or
`EvictableCharNode`. The result remains valid after the registry changes or is
dropped. It is not an authority token and cannot authorize an exact root
transition.

The registry deliberately does **not** cache that owned result in every entry.
On a 64-bit target, a byte or character entry and its dense `Option<Entry>` slot
are each 48 bytes. The rejected per-entry `OnceLock<...>` layout was 120 bytes,
an additional 72 bytes per occurrence (68.7 MiB per million occurrences) before
counting any materialized path allocation. Removing it also eliminates the
cache-initialization branch and durable-pointer clone from registration. Exact
selection is unchanged and continues to materialize no paths.

The compatibility algorithm is:

```text
owned_lookup(hash):
    occurrence := ordered_collision_bucket(hash).last()
    path := topology.materialize(occurrence.path_id)
    if path failed: return None without mutation
    return owned(path, clone(occurrence.durable_pointer), occurrence.metadata)

owned_remove(hash):
    occurrence := ordered_collision_bucket(hash).last()
    path := topology.materialize(occurrence.path_id)
    if path failed: return None without mutation
    commit removal and accounting updates without allocation
    return owned(path, move(occurrence.durable_pointer), occurrence.metadata)
```

Repeated owned lookup is therefore $`O(\mathit{depth})`$ and allocates the returned path
each time. This is intentional: callers that need repeated inspection should
retain the owned result, while checkpoint construction and eviction selection
avoid paying permanent memory for a rarely used compatibility API. Collision
semantics remain deterministic: lookup and removal both select the last live
occurrence in the ordered hash bucket. Character materialization additionally
rejects non-Unicode-scalar UTF-32 units before mutation.

This is an intentional prerelease API migration: borrowed `get` and `get_char`
were replaced by the explicit `get_owned` and `get_char_owned` names. A caller
typically changes only the method name; because the result is owned, it no
longer borrows the registry:

```rust
let record = registry
    .get_owned(path_hash)
    .expect("registered byte occurrence");
drop(registry);
assert_eq!(record.path, b"expected/path");
```

The functional refinement and failure transaction are proved in
`formal-verification/rocq/Spec/DetachedCallbackSeparationSpec.v` and exhaustively
model-checked for the bounded collision/removal protocol in
`formal-verification/tla+/CachelessOwnedRegistry.tla`. The permanent formal gate
also runs rejected-design controls that must find counterexamples for selecting
the first collision occurrence and mutating before materialization succeeds.

**Source:** `src/persistent_artrie/core/eviction/disk_registry.rs` (`DiskLocationRegistry`)

### EpochManager

Coordinates reader/writer epochs for safe memory reclamation.

```rust
pub struct EpochManager {
    global_epoch: AtomicU64,
    active_readers: AtomicUsize,
}
```

**Key Methods:**

| Method | Description |
|--------|-------------|
| `enter_read()` | Increment reader count, return current epoch |
| `exit_read()` | Decrement reader count |
| `advance()` | Increment global epoch |
| `has_active_readers()` | Check if any readers are active |
| `wait_for_quiescence(timeout, poll)` | Wait for readers to drain |
| `try_quiescence()` | Non-blocking quiescence attempt |

**Source:** `src/persistent_artrie/core/concurrency.rs` (`EpochManager`)

---

## Data Flow

### Eviction Trigger to Completion

The end-to-end trigger-to-completion path is **Figure 1** above. In prose, the steps the async `artrie-eviction` thread performs per request are:

1. **Dequeue.** `MemoryPressureLevel::Low`/`Critical` $`\Rightarrow`$ `request_eviction(urgency)` pushes (or urgency-merges) an `EvictionRequest` onto the coordinator's `VecDeque`. The worker does **not** sleep on a condvar — it holds only a `Weak<EvictionCoordinator>`, upgrades once per iteration, and polls `try_pop_request` roughly every `100 ms`, dropping the strong reference before sleeping. (This `Weak`-driven poll replaced an earlier condvar design that leaked one OS thread per trie by keeping the coordinator alive; see `eviction_loop` in `core/eviction/coordinator.rs`.)
2. **Cooldown check.** Skip (and `record_skip`) if the request is older than `5 s` or arrives inside the cooldown window.
3. **Epoch quiescence.** `advance()` the epoch, then `wait_for_quiescence()`; on timeout, `record_quiescence_timeout` and skip the cycle.
4. **Select cold nodes.** Ask the `DiskLocationRegistry` for the coldest evictable candidates, scored by the `LruRegistry` (see the algorithm below).
5. **Unswizzle.** Invoke the callback, which atomically swaps each selected $`\text{ChildNode} \to \text{DiskRef}`$.
6. **Record statistics.** `record_eviction(nodes, bytes, duration_ms)`.

### The two selection and execution policies

Manual and memory-pressure eviction preserves the historical
`DescendantFirst` policy: candidates are ranked by local coldness, deepest
successful endpoints suppress overlapping ancestors, and candidate-local
weights are sufficient because this path is an opportunistic reclamation
request rather than a resident-budget guarantee.

The checkpoint tail uses `ResidentBudgetAncestorClosure`. Its target is an
exact resident-byte delta, so overlapping subtrees must be counted as a union.
For a resident record $`x`$, let

```math
w(x)=\text{serialized\_bytes}(x)+\text{family\_overhead}.
```

The topology is preorder and each subtree is one interval. Selection performs:

1. Capture one LRU timestamp and rank each eligible anchor by its *warmest*
   resident descendant. Larger coldness ranks first; ties use greater depth,
   then ascending path identifier.
2. Give every resident record $`x`$ to the earliest-ranked eligible ancestor
   on its root-to-$`x`$ path. If that rank is $`\rho(x)`$, define

   ```math
   g_r=\sum_{x:\rho(x)=r}w(x).
   ```

3. Select the smallest priority prefix whose $`\sum g_r`$ reaches the target,
   or the configured cap. These gain sets are disjoint and partition the
   selected subtree union, so `planned_bytes` is exact for the captured
   authoritative generation.
4. Execute ancestor-first. The first selected ancestor whose live stamp equals
   its exact disk pointer replaces the complete subtree. A stale ancestor is
   skipped and its already-ranked descendants remain fallbacks.

Warmest-descendant aggregation implies every resident descendant ranks before
its ancestor; positive weights therefore make the selected set downward-closed.
The configured cap bounds selected resident records, successful replacement
endpoints, and registry-clear work. It does not bound bytes because record sizes
vary. Root advancement, authority loss, or mutation can make committed
reclamation smaller than the plan, never larger.

Selection uses one reverse topology pass, one forward effective-rank pass, and
a coldness sort: $`O(n\log n)`$ time, $`O(n)`$ fallible transient memory, and
constant native-stack use. Unary and branching execution are explicit PDAs;
neither recurses with trie depth.

### Lock-free resident snapshot

Resident scoring captures and helps one immutable root revision, then reads the
packed residency cells named by that root's exact catalog and ordinal. It clones
only immutable topology and record-table `Arc`s; it does not take a registry or
lifecycle lock. After the residency scan, the coordinator validates that the
root still has the same identity, catalog, generation, and ordinal. A changed
root yields `Retry`; lost exact authority yields `Unavailable`.

The resulting snapshot is a planning input, not an authority lease. The exact
root CAS revalidates the captured identity and binding before it can clear any
successor residency. Consequently a stale snapshot can be ranked safely but can
never mutate a newer root revision or catalog generation.

### Checkpoint Integration

<img src="../diagrams/eviction-checkpoint-flow.svg" alt="Checkpoint integration flow: checkpoint() serializes the trie to disk via DFS traversal, builds a new DiskRegistry by registering each written node's path, disk pointer, size, depth, and type, then updates the coordinator by atomically replacing the old registry." width="70%"/>

---

## Concurrency & Safety

### Epoch-Based Reclamation (EBR)

**What.** EBR is the safe-memory-reclamation discipline that lets the eviction thread free a node while lock-free readers run concurrently, with no use-after-free. **How.** A reader brackets its traversal with `EpochManager::enter_read()` / `exit_read()`; the eviction thread calls `advance()` to open an epoch boundary, then `wait_for_quiescence()` to block until `active_readers` drains to zero, and only *then* performs the `unswizzle` swap and frees the old allocation. **Why.** Once a node pointer has been *swizzled* it is followed at native speed with no lock and no buffer-manager lookup; the only way to reclaim that node safely is to prove no reader can still hold the raw pointer — which is exactly what quiescence proves.

<img src="../diagrams/epoch-reclamation.svg" alt="Epoch-based reclamation sequence: eviction defers the free until every old-epoch reader departs, then swaps and frees" width="900"/>

*Figure 2 — Epoch-based reclamation. `Reader A` pins epoch 5 and may hold a raw `*const Node` into the victim. The eviction thread `advance()`s to epoch 6 and, seeing `A` still active, **defers** the free. After `A` calls `exit_read()` and quiescence is reached, the thread `unswizzle`s the victim ($`\text{ChildNode} \to \text{DiskRef}`$) and frees it. `Reader B`, which entered after the boundary, observes the already-published `DiskRef` and faults the node back in — it never touches freed memory.*

**Guarantee:** a node is freed only after **all** readers from the pre-eviction epoch have completed. The ordering is $`\text{advance} \to \text{wait for quiescence} \to \text{swap} \to \text{free}`$.

**Memory-ordering note.** `enter_read`/`exit_read` use `SeqCst` on the `active_readers` counter, and the reclaimer's `has_active_readers()` check is also `SeqCst`. This is the StoreLoad barrier EBR requires: it guarantees that if the reclaimer's scan fails to observe a reader, that reader is guaranteed to observe the reclaimer's unlink (and re-fault a fresh node) rather than dereference a freed one. `AcqRel`/`Acquire` alone would permit the StoreLoad reordering and would **not** be sufficient (see the rationale comments on `EpochManager::enter_read` in `core/concurrency.rs`).

### Thread Safety Primitives

| Component | Primitive | Purpose |
|-----------|-----------|---------|
| `LruRegistry.trackers` | `DashMap` | Lock-free concurrent access tracking |
| `AccessTracker` fields | `AtomicU64` | Lock-free timestamp/count updates |
| `EpochManager.global_epoch` | `AtomicU64` | Lock-free epoch advancement |
| `EpochManager.active_readers` | `AtomicUsize` | Lock-free reader counting |
| `EvictionCoordinator.request_queue` | `Mutex` | Thread-safe request queueing (drained by the `Weak`-driven poll loop; no condvar) |
| `EvictionCoordinator.disk_registry` | `RwLock` | Concurrent registry access |

### The Buffer-Pool Layer Underneath (Page Lifecycle)

Node eviction (above) reclaims *trie nodes*; beneath it, the block-storage **buffer pool** manages fixed-size *pages* (256 KB frames) and is what physically reads a node in (*fault-in*) and writes a dirty node out (*flush*). Understanding the page lifecycle clarifies the $`\text{DiskRef} \to \text{fault-in} \to \text{resident}`$ round-trip that eviction reverses.

**What.** A buffer-pool frame's per-frame state (`FrameMetadata`, `core/buffer_manager.rs`) carries a `block_id`, a `lease_state` (a read-pin count or the exclusive `WRITE_LEASE`), a `dirty` flag, and a `reference_bit` for the CLOCK replacement algorithm. There is no single `enum PageState`; a page's condition is the product of $`\{\text{resident}, \text{on-disk}\} \times \{\text{clean}, \text{dirty}\} \times \{\text{pinned}, \text{unpinned}\}`$. **How.** `load_page`/`pin_page_data` fault a page in; `pin_read`/`pin_write` pin it; `mark_dirty` flags a write; `flush_page`/`flush_all` write it back and `clear_dirty`; and `get_free_frame` reuses an unpinned, unreferenced frame as a CLOCK victim. **Why.** Two invariants make this safe and are visible in the figure: a page is **never** a CLOCK victim while *pinned*, and a *dirty* page may **not** be flushed while a `WRITE_LEASE` is held (a dirty victim is written back before its frame is reused, so no acknowledged bytes are lost).

<img src="../diagrams/buffer-page-lifecycle.svg" alt="Buffer-pool page lifecycle, part 1 of 2: the top-level Disk ⇄ Resident ⇄ eviction cycle — fault-in, pin/unpin, flush, and CLOCK write-back" width="520"/>
<img src="../diagrams/buffer-page-lifecycle-2.svg" alt="Buffer-pool page lifecycle, part 2 of 2: the Resident frame's internal substates — Clean/Dirty crossed with Pinned/Unpinned" width="590"/>

*Figure 3 — Buffer-pool page (frame) lifecycle. A page faults in from disk into a free frame, then cycles through `Clean ⇄ Dirty` (on write under a `WRITE_LEASE`) and `Pinned ⇄ Unpinned` (on lease acquire/release). A clean, unpinned frame whose `reference_bit` is clear is reused in place by the CLOCK algorithm; a dirty victim is written back (`clear_dirty`) first. In-memory states are green; the on-disk-only state is blue; fault-in / write-back I/O is amber.*

> This page-level CLOCK eviction (reclaiming a *frame* in the fixed buffer pool) is **distinct** from the node-level eviction subsystem documented above (reclaiming a cold *node*'s RAM via EBR + `DiskRef` swap). They operate at different layers and compose: node eviction turns a hot `ChildNode` into a `DiskRef`; a later access re-faults it through this buffer pool.

### Non-Blocking Guarantees

| Operation | Blocking Behavior |
|-----------|-------------------|
| `touch_node()` | Non-blocking (atomic DashMap ops) |
| `request_eviction()` | Non-blocking (brief `request_queue` mutex; no condvar) |
| `lookup()` / `contains()` | Non-blocking (epoch enter/exit) |
| `insert()` | Non-blocking (its root CAS clears the exact eviction binding) |
| Actual eviction | Happens in background thread only |

---

## Configuration Guide

### Preset Configurations

| Profile | Use Case | `target_memory_fraction` | `min_eviction_depth` | `batch_size` |
|---------|----------|--------------------------|----------------------|--------------|
| `default()` | Balanced workloads | 0.70 | 1 | 256 |
| `memory_constrained()` | Limited RAM systems | 0.80 | 0 | 512 |
| `read_optimized()` | Read-heavy workloads | 0.50 | 3 | 128 |
| `disabled()` | Testing, unlimited RAM | N/A | N/A | N/A |
| `without_memory_monitor()` | Manual eviction only | 0.70 | 1 | 256 |

### Configuration Examples

**Default (Balanced):**
```rust
let config = EvictionConfig::default();
// enabled: true
// target_memory_fraction: 0.70
// min_eviction_depth: 1
// batch_size: 256
// use_lru_tracking: true
// enable_memory_pressure_monitor: true
```

**Memory-Constrained Environment:**
```rust
let config = EvictionConfig::memory_constrained();
// target_memory_fraction: 0.80 (more aggressive)
// min_eviction_depth: 0 (all nodes evictable)
// batch_size: 512 (larger batches)
// shorter timeouts and cooldowns
```

**Read-Heavy Workload:**
```rust
let config = EvictionConfig::read_optimized();
// target_memory_fraction: 0.50 (keep more in memory)
// min_eviction_depth: 3 (protect upper tree levels)
// batch_size: 128 (smaller, less disruptive)
// longer timeouts
```

**Custom Configuration:**
```rust
let config = EvictionConfig {
    enabled: true,
    target_memory_fraction: 0.75,
    min_eviction_depth: 2,
    batch_size: 512,
    quiescence_timeout: Duration::from_millis(200),
    quiescence_poll_interval: Duration::from_micros(50),
    cooldown_period: Duration::from_millis(50),
    use_lru_tracking: true,
    enable_memory_pressure_monitor: true,
    memory_pressure_config: Some(MemoryPressureConfig {
        low_memory_threshold: 0.25,      // 25% available triggers Low
        critical_memory_threshold: 0.10, // 10% available triggers Critical
        ..Default::default()
    }),
    resident_budget_bytes: Some(2 * 1024 * 1024 * 1024),
    resident_budget_eviction_cap: None,
};
```

**Resident-budget latency cap:**

An uncapped tail reaches any structurally reachable budget in one quiescent
checkpoint. With `Some(n)`, convergence depends on *weight*, not merely node
count: the exact resident bytes covered by the first `n` cold-ranked anchors
must exceed resident growth between checkpoints. `min_eviction_depth` can pin
shallow residency; exhaustion is reported separately from cap exhaustion.

### Tuning Guidelines

| Scenario | Recommendation |
|----------|----------------|
| Large dictionary, limited RAM | Increase `batch_size`, decrease `min_eviction_depth` |
| Read-heavy workload | Increase `min_eviction_depth`, decrease `target_memory_fraction` |
| Write-heavy workload | Increase `cooldown_period` to reduce thrashing |
| Latency-sensitive | Decrease `batch_size`, increase `quiescence_timeout` |
| Memory spikes | Decrease `low_memory_threshold` for earlier eviction |

---

## API Reference

### EvictableARTrie Trait

```rust
pub trait EvictableARTrie: ARTrie {
    /// Enable memory pressure-driven eviction.
    ///
    /// Starts a background eviction thread that monitors memory pressure
    /// and evicts cold nodes to disk when pressure is detected.
    fn enable_eviction(&mut self, config: EvictionConfig) -> Result<()>;

    /// Disable eviction and release resources.
    ///
    /// Stops the background eviction thread. Nodes in memory remain
    /// in memory until the trie is closed.
    fn disable_eviction(&mut self) -> Result<()>;

    /// Check if eviction is currently enabled.
    fn eviction_enabled(&self) -> bool;

    /// Get eviction statistics snapshot.
    fn eviction_stats(&self) -> EvictionStats;

    /// Manually trigger eviction (for testing/debugging).
    ///
    /// Forces immediate eviction, bypassing memory pressure checks.
    /// Returns (nodes_evicted, bytes_freed).
    fn force_eviction(&mut self, target_bytes: usize) -> Result<(usize, usize)>;

    /// Record a node access for LRU tracking.
    ///
    /// Called internally during traversal. User code typically
    /// does not need to call this directly.
    fn touch_node(&self, path: &[Self::Unit]);
}
```

**Source:** `src/artrie_trait.rs:624`

### Usage Example

```rust
use libdictenstein::persistent_artrie::{PersistentARTrie, EvictionConfig};
use libdictenstein::EvictableARTrie;

// Create or open a trie
let mut trie = PersistentARTrie::<()>::create("words.part")?;

// Enable memory pressure-driven eviction
let config = EvictionConfig::default();
trie.enable_eviction(config)?;

// Normal operations continue...
trie.insert("hello");
trie.insert("world");

// Checkpoint to create disk representations
trie.checkpoint()?;

// Eviction happens automatically when memory pressure is detected
// Check stats for eviction activity
let stats = trie.eviction_stats();
println!("Nodes evicted: {}", stats.nodes_evicted);
println!("Bytes freed: {} MB", stats.bytes_freed / (1024 * 1024));
println!("Eviction cycles: {}", stats.eviction_cycles);

// Manual eviction for testing
let (nodes, bytes) = trie.force_eviction(1024 * 1024)?; // Target 1MB
println!("Manually evicted {} nodes ({} bytes)", nodes, bytes);

// Disable eviction when done
trie.disable_eviction()?;
```

---

## Edge Cases & Error Handling

### Root Node Protection

The root node is **never evicted**. This ensures:
- The trie always has a valid entry point
- Path navigation always starts from a valid in-memory node

```rust
fn evict_node_at_path(&mut self, path: &[u8], disk_ptr: SwizzledPtr) -> bool {
    if path.is_empty() {
        // Cannot evict root
        return false;
    }
    // ...
}
```

### Exact fault provenance and re-eviction

Every exact byte or character decoder stamps the top-level decoded node with
the source `SwizzledPtr::to_raw()` before attempting publication. For a
compressed record, only the head of the expanded span receives that stamp;
synthetic descendants have no independent disk identity and remain zero.
Uncompressed exact records follow the same rule. Structural path copies always
clear the stamp.

The stamp is provenance, not authority. A winning fault must additionally match
the current registry generation, exact path, disk address, root binding, and
nonresident occurrence. The coordinator publishes the new root and marks that
one occurrence resident in the same immutable-root CAS. A losing fault load
remains private and changes neither the published stamp nor residency. This
protocol makes `evict → exact fault → evict` non-vacuously repeatable without
allowing a stale or detached load to authorize reclamation.

### Exact-root authority and cold lifecycle publication

A durable registry is authoritative only when the current immutable root
revision names its exact catalog and generation. Let $`R`$ be the current root,
$`B(R)`$ its optional eviction binding, $`C`$ a coordinator identity, and $`G`$
a registry generation. Exact authority is:

```math
\operatorname{Authority}(R,C,G) \iff B(R) = \operatorname{Some}(C,G)
\land \neg \operatorname{Retired}(C).
```

The root identity is the linearization authority. There is no runtime semantic
permit counter, callback counter, or writer-side publication lock.
`SemanticMutationPublicationPermit` is only a zero-sized compile-time witness
that durable writers use the semantic root-CAS path; it has no lock, atomic,
allocation, branch, or drop-time action.

The implementation protocol is:

```text
semantic mutation (hot path):
  append and, when policy requires, sync the data WAL
  capture one root revision
  prepare a semantic successor with no eviction binding
  CAS(captured root, semantic successor)
  retry from a fresh capture after a lost CAS

exact eviction or fault (hot path):
  capture and help one bound root revision
  prepare a packed residency successor for its exact catalog and ordinal
  CAS(captured root, exact successor)
  on loss, classify the returned defeating revision; mutate no side state

checkpoint publication (cold path):
  build the immutable catalog, packed residency, and deferred stamps
  lock the trie-lifetime lifecycle gate and reject a retired coordinator
  CAS the captured root to the prepared bound revision
  on success, apply stamps and activate the builder mirror only if the root
    still names that exact generation; otherwise fail closed
  unlock the lifecycle gate

retirement (cold path):
  lock the lifecycle gate and set the irreversible retired bit
  publish an unconditional unbound root fence, retrying only on root advance
  invalidate the builder mirror and clear the detached catalog
  unlock the lifecycle gate

detached compatibility install (cold path):
  finalize the structural registry
  lock the lifecycle gate and reject retirement
  ArcSwap an immutable detached catalog; unlock
```

An immutable detached catalog is a different capability type. Compatibility
callbacks retain an `Arc` snapshot across replacement, but that snapshot cannot
construct an exact root transition. Thus callbacks may overlap semantic writes
without blocking them and without becoming eviction authority.

The source-compatible `update_disk_registry` wrapper remains infallible in its
signature and is total: structural-finalization failure or coordinator
retirement rejects the candidate without publishing it. A concurrent successful
install may independently replace the discovery slot. The wrapper deliberately
invokes no user-supplied logging callback, because such a callback could unwind
and violate the wrapper's totality guarantee. Code that needs explicit failure
handling should call `try_install_detached_compatibility_catalog`.
Concurrent semantic writes and compatibility callbacks are not rejection
conditions because the detached `ArcSwap` catalog is independent of exact root
authority.

This ordering closes the durable WAL-before-CAS race and the checkpoint/write
race at the root itself. A checkpoint may serialize while a writer is active;
if the writer publishes first, the checkpoint's exact root CAS loses and its
catalog never becomes authority. If the checkpoint publishes first, the
writer's next successful semantic CAS atomically removes that binding.

### Concurrent Reads During Eviction

Epoch-based safety ensures readers are not affected:

1. **Before eviction:** Epoch is advanced
2. **During quiescence wait:** All old-epoch readers complete
3. **During eviction:** New readers see updated epoch, old readers have finished
4. **Result:** No reader observes a partially-evicted node

### Quiescence Timeout Handling

If readers don't drain within the timeout:

```rust
if !self.wait_for_quiescence() {
    self.stats.record_quiescence_timeout();
    continue; // Skip this eviction cycle
}
```

The eviction cycle is skipped (not retried with a longer timeout) to prevent indefinite blocking. The next memory pressure event will trigger another attempt.

### Coordinator retirement and re-enable

`close()` and `disable_eviction()` retire the installed coordinator while the
trie still holds its coordinator-slot mutex. Retirement acquires the temporary
exact-publication lifecycle boundary, irreversibly marks the coordinator
retired, publishes an unbound root fence, and clears exact registry authority
before the slot can become empty. A stale `Arc` may still exist, but it can no
longer publish an exact registry, install a detached catalog, fault through
exact authority, or commit an already-selected compact batch.

Re-enabling eviction installs a fresh coordinator identity. Semantic writers
do not enter the lifecycle boundary: their root CAS clears exact authority
atomically and carries only a zero-sized compile-time witness. Detached legacy
callbacks retain immutable Arc snapshots and never block the replacement
coordinator. Their catalog is separate from the exact registry, so clearing or
replacing it cannot orphan or manufacture a root binding. Retirement is not a
substitute for reader-epoch quiescence.

### Already-Evicted Nodes

Attempting to evict an already-evicted node (DiskRef) is a no-op:

```rust
match child {
    ChildNode::DiskRef { .. } => {
        // Already evicted
        return false;
    }
    ChildNode::Bucket(_) | ChildNode::ArtNode { .. } => {
        // Replace with DiskRef
        *child = ChildNode::DiskRef { ptr: disk_ptr };
        return true;
    }
}
```

---

## Statistics & Monitoring

### EvictionStats Structure

```rust
pub struct EvictionStats {
    pub nodes_evicted: u64,           // Total nodes evicted
    pub bytes_freed: u64,             // Total bytes freed
    pub eviction_cycles: u64,         // Completed eviction cycles
    pub last_eviction_duration_ms: u64, // Duration of last cycle
    pub eviction_requests: u64,       // Total eviction requests received
    pub skipped_evictions: u64,       // Skipped (cooldown/timeout)
    pub quiescence_timeouts: u64,     // Epoch quiescence timeouts
}
```

### Derived Metrics

| Metric | Formula | Meaning |
|--------|---------|---------|
| `nodes_per_cycle()` | `nodes_evicted / eviction_cycles` | Average eviction efficiency |
| `bytes_per_cycle()` | `bytes_freed / eviction_cycles` | Average memory freed per cycle |
| `skip_rate()` | `skipped_evictions / eviction_requests` | Fraction of skipped requests |

### Monitoring Example

```rust
let stats = trie.eviction_stats();

println!("=== Eviction Statistics ===");
println!("Total nodes evicted: {}", stats.nodes_evicted);
println!("Total bytes freed: {} MB", stats.bytes_freed / (1024 * 1024));
println!("Eviction cycles: {}", stats.eviction_cycles);
println!("Avg nodes/cycle: {:.1}", stats.nodes_per_cycle());
println!("Avg bytes/cycle: {:.1} KB", stats.bytes_per_cycle() / 1024.0);
println!("Last cycle duration: {} ms", stats.last_eviction_duration_ms);
println!("Skip rate: {:.1}%", stats.skip_rate() * 100.0);
println!("Quiescence timeouts: {}", stats.quiescence_timeouts);
```

### Health Indicators

| Indicator | Healthy Range | Action if Unhealthy |
|-----------|---------------|---------------------|
| Skip rate | < 30% | Increase `cooldown_period` |
| Quiescence timeouts | < 5% of cycles | Increase `quiescence_timeout` |
| Avg nodes/cycle | > batch_size * 0.5 | Check that checkpoint is being called |
| Last cycle duration | < 100ms | Decrease `batch_size` if latency-sensitive |

---

## Source Files

The eviction subsystem lives under the unit-agnostic `core/` of the persistent ARTrie crate (`src/persistent_artrie/core/`).

| File | Content |
|------|---------|
| `src/persistent_artrie/core/eviction/mod.rs` | Module structure, public exports (`EvictionConfig`, `EvictionCoordinator`, `DiskLocationRegistry`, `LruRegistry`, `AccessTracker`) |
| `src/persistent_artrie/core/eviction/config.rs` | `EvictionConfig` (incl. `resident_budget_bytes`), `EvictionUrgency` (`Moderate`/`Urgent`/`Emergency`), `EvictionStats`, `EvictionStatsAtomic` |
| `src/persistent_artrie/core/eviction/coordinator.rs` | `EvictionCoordinator` — request queue, `Weak`-driven async eviction loop, cooldown/quiescence, byte+char+resident-budget eviction arities |
| `src/persistent_artrie/core/eviction/lru_tracker.rs` | `LruRegistry`, `AccessTracker`, FNV-1a path hashing, coldness scoring |
| `src/persistent_artrie/core/eviction/disk_registry.rs` | Segmented byte/character `PathTopology`, dense durable records and residency, generation-qualified selection snapshots, compact manual and exact resident-closure selectors |
| `src/persistent_artrie/core/memory_monitor.rs` | `MemoryPressureMonitor`, `MemoryPressureLevel` (`Normal`/`Low`/`Critical`), `MemoryPressureConfig`, `sysinfo`/PSI-based detection |
| `src/persistent_artrie/core/concurrency.rs` | `EpochManager` (EBR: `enter_read`/`exit_read`/`advance`/`wait_for_quiescence`) and `EpochGuard` |
| `src/persistent_artrie/core/swizzled_ptr.rs` | `SwizzledPtr` — atomic `swizzle`/`unswizzle`, `DiskLocation`, `NodeType` |
| `src/persistent_artrie/core/buffer_manager.rs` | Buffer-pool frame (`FrameMetadata`) lifecycle (pin/unpin, mark-dirty, flush, CLOCK eviction) |
| `src/artrie_trait.rs` | `EvictableARTrie` trait definition (`enable_eviction`, `force_eviction`, `eviction_stats`, `touch_node`) |

> The byte/char/vocab `EvictableARTrie` *implementations* are wired through each variant's Phase-6 eviction sub-modules (e.g. `src/persistent_artrie/*/eviction*.rs` and the `atomic_ops`/`persist` sub-modules), which adapt the shared `core/eviction` machinery to that variant's node type. Overlay eviction is functional and proven for the byte and char variants (the shared `OverlayEvictable` primitives in `core/overlay/evict.rs`, driven from the eviction coordinator and the checkpoint-integrated callback); the vocab variant installs a no-op eviction callback for API parity (its overlay-only vocabulary never evicts finals).

## Related documentation

- [Concurrency model](concurrency-model.md) — the F4 lock hierarchy (`EC` is the eviction-coordinator leaf), the `serial_disk_ptr` eviction-safety stamp, and epoch-based reclamation in full.
- [Lock-free overlay](lock-free-overlay.md) — the immutable `OverlayNode` whose cold subtrees eviction unswizzles into a `Child::OnDisk(SwizzledPtr)` (the on-disk *DiskRef* of this document), and the read-path fault-in.
- [Durability & recovery](durability-and-recovery.md) — the checkpoint that writes a node's bytes to disk *before* eviction can reclaim its RAM, so an evicted node is never lost.
- [Persistence architecture](README.md) — the whole stack, with eviction as a cross-cutting concern.

## References

- K. Fraser. *Practical Lock-Freedom.* PhD thesis, University of Cambridge, 2004 — epoch-based
  reclamation (EBR), the safe-memory-reclamation discipline used here. Technical Report
  [UCAM-CL-TR-579](https://www.cl.cam.ac.uk/techreports/UCAM-CL-TR-579.html).
- *Dynamic Memory Allocation in SQLite* — the bounded page-cache / capped-memory model this
  subsystem's `resident_budget_bytes` design follows. <https://www.sqlite.org/malloc.html>

*(The **CLOCK** second-chance victim selection and the **LRU** coldness heuristic are classical
page-cache algorithms, described mechanically in the sections above.)*
