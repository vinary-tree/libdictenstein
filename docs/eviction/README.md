# Eviction Strategy for Persistent ARTrie

This document describes the memory pressure-driven eviction system for the persistent ARTrie data structure.

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

### High-Level System Diagram

```
+-----------------------------------------------------------------------+
|                         PersistentARTrie<V>                           |
+-----------------------------------------------------------------------+
|                                                                       |
|  +-------------------------+    +----------------------------------+  |
|  | MemoryPressureMonitor   |    |         LruRegistry              |  |
|  | (background thread)     |    | (DashMap<hash, AccessTracker>)   |  |
|  +------------+------------+    +----------------------------------+  |
|               |                              ^                        |
|               | Low/Critical pressure        | touch_hash()           |
|               v                              |                        |
|  +------------+------------+                 |                        |
|  |  EvictionCoordinator    +-----------------+                        |
|  |  (request queue, state) |                                          |
|  +------------+------------+                                          |
|               |                                                       |
|               | Processes eviction requests                           |
|               v                                                       |
|  +------------+------------+                                          |
|  |   Eviction Thread       |                                          |
|  |   (background)          |                                          |
|  +------------+------------+                                          |
|               |                                                       |
|               +---> 1. Wait for epoch quiescence (EpochManager)       |
|               |                                                       |
|               +---> 2. Select cold nodes (DiskLocationRegistry + LRU) |
|               |                                                       |
|               +---> 3. Atomically swap ArtNode -> DiskRef             |
|                                                                       |
+-----------------------------------------------------------------------+
```

### Component Interaction Flow

```
                    Memory Pressure Detected
                            |
                            v
        +-------------------+-------------------+
        |     MemoryPressureMonitor             |
        |  (monitors system memory via sysinfo) |
        +-------------------+-------------------+
                            |
                            | Callback with MemoryPressureLevel
                            v
        +-------------------+-------------------+
        |      EvictionCoordinator              |
        |  request_eviction(urgency)            |
        +-------------------+-------------------+
                            |
                            | Queues EvictionRequest
                            v
        +-------------------+-------------------+
        |       Eviction Thread                 |
        | (artrie-eviction background thread)   |
        +-------------------+-------------------+
                            |
            +---------------+---------------+
            |               |               |
            v               v               v
      +---------+    +------------+   +-------------+
      | Cooldown|    |   Epoch    |   |   Select    |
      |  Check  |    | Quiescence |   | Cold Nodes  |
      +---------+    +------------+   +-------------+
                            |
                            v
        +-------------------+-------------------+
        |        Eviction Callback              |
        |  (Replace ArtNode with DiskRef)       |
        +-------------------+-------------------+
                            |
                            v
        +-------------------+-------------------+
        |     Update Statistics                 |
        |  (nodes_evicted, bytes_freed, etc.)   |
        +---------------------------------------+
```

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

**Source:** `src/persistent_artrie/eviction/config.rs:24-110`

### EvictionCoordinator

The central orchestrator for asynchronous, epoch-safe node eviction.

```rust
pub struct EvictionCoordinator {
    config: EvictionConfig,
    epoch_manager: Arc<EpochManager>,
    lru_registry: Arc<LruRegistry>,
    request_queue: Mutex<VecDeque<EvictionRequest>>,
    request_condvar: Condvar,
    shutdown: AtomicBool,
    eviction_thread: Mutex<Option<JoinHandle<()>>>,
    stats: Arc<EvictionStatsAtomic>,
    last_eviction: AtomicU64,
    disk_registry: RwLock<DiskLocationRegistry>,
    running: AtomicBool,
    memory_monitor: RwLock<Option<Arc<MemoryPressureMonitor>>>,
}
```

**Key Methods:**

| Method | Description |
|--------|-------------|
| `new(config, epoch_manager)` | Create coordinator in stopped state |
| `start(callback)` | Start eviction thread with byte-level callback |
| `start_char(callback)` | Start eviction thread with char-level callback |
| `start_memory_monitor()` | Enable automatic memory pressure monitoring |
| `request_eviction(urgency)` | Queue an eviction request |
| `force_eviction(target_bytes)` | Synchronous eviction for testing |
| `update_disk_registry(registry)` | Replace disk registry after checkpoint |
| `invalidate_registry()` | Mark registry invalid on write operations |
| `shutdown()` | Stop eviction thread and memory monitor |

**Source:** `src/persistent_artrie/eviction/coordinator.rs:53-78`

### LruRegistry

Lock-free registry for tracking node access patterns using DashMap.

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

**Source:** `src/persistent_artrie/eviction/lru_tracker.rs:148-321`

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

**Source:** `src/persistent_artrie/eviction/lru_tracker.rs:22-109`

### DiskLocationRegistry

Maps node paths to their disk locations after checkpoint.

```rust
pub struct DiskLocationRegistry {
    locations: HashMap<u64, EvictableNode>,       // Byte-level nodes
    char_locations: HashMap<u64, EvictableCharNode>, // Char-level nodes
    total_size_bytes: usize,
    node_type_counts: HashMap<NodeType, usize>,
    valid: bool,
}
```

**EvictableNode Structure:**

| Field | Type | Description |
|-------|------|-------------|
| `path` | `Vec<u8>` | Path from root (edge labels) |
| `disk_ptr` | `SwizzledPtr` | Disk location from checkpoint |
| `size_bytes` | `usize` | Estimated memory size |
| `depth` | `usize` | Depth in trie (0 = root children) |
| `node_type` | `NodeType` | Node variant (Node4, Node16, etc.) |

**Key Methods:**

| Method | Description |
|--------|-------------|
| `register(path, ptr, size, depth, type)` | Record node's disk location |
| `select_for_eviction(target, lru, depth, max)` | Select cold nodes for eviction |
| `invalidate()` | Mark registry as invalid (on write ops) |
| `is_valid()` | Check if registry is usable |

**Memory Overhead:** ~50 bytes per node (path + 8 bytes ptr + 8 bytes size + 8 bytes depth + overhead)

**Source:** `src/persistent_artrie/eviction/disk_registry.rs:79-401`

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

**Source:** `src/persistent_artrie/concurrency.rs:233-362`

---

## Data Flow

### Eviction Trigger to Completion

```
+------------------+
| Memory Pressure  |  (MemoryPressureLevel::Low or Critical)
+--------+---------+
         |
         v
+--------+---------+
| request_eviction |  Maps pressure level to EvictionUrgency
| (urgency)        |
+--------+---------+
         |
         v
+--------+---------+
|  Request Queue   |  VecDeque<EvictionRequest>
|  (condvar wake)  |  Higher urgency merges with pending request
+--------+---------+
         |
         v
+--------+---------+
| Eviction Thread  |  Wakes on condvar notification
|    (loop)        |
+--------+---------+
         |
         +---> Cooldown Check (skip if too recent)
         |
         +---> Epoch Quiescence (advance epoch, wait for readers)
         |
         +---> Select Cold Nodes (DiskLocationRegistry + LruRegistry)
         |
         +---> Invoke Callback (replace ArtNode with DiskRef)
         |
         +---> Update Statistics (record eviction metrics)
```

### Node Selection Algorithm

```
DiskLocationRegistry.select_for_eviction(target_bytes, lru, min_depth, max_count):

  1. FILTER: locations where depth >= min_depth

  2. SCORE: For each node, compute coldness via LruRegistry
     coldness = lru_registry.coldness_score_hash(path_hash)

  3. SORT: By coldness descending (coldest first)

  4. SELECT: Accumulate nodes until:
     - total_bytes >= target_bytes, OR
     - count >= max_count

  5. RETURN: Vec<(path_hash, EvictableNode)>
```

### Checkpoint Integration

```
+----------------+
|  checkpoint()  |
+-------+--------+
        |
        v
+-------+--------+
| Serialize Trie |  DFS traversal, write nodes to disk
|  to Disk       |
+-------+--------+
        |
        | During serialization:
        v
+-------+--------+
| Build New      |  For each node written:
| DiskRegistry   |    registry.register(path, disk_ptr, size, depth, type)
+-------+--------+
        |
        v
+-------+--------+
| Update         |  coordinator.update_disk_registry(new_registry)
| Coordinator    |  (replaces old registry atomically)
+----------------+
```

---

## Concurrency & Safety

### Epoch-Based Reclamation

```
         Reader 1        Global Epoch        Eviction Thread
            |                 |                    |
            |  enter_read()   |                    |
            +-------->--------+ epoch=5            |
            |                 |                    |
            |                 |   advance()        |
            |                 +<-------------------+
            |                 | epoch=6            |
            |                 |                    |
            |                 |  wait for readers  |
            |                 +<-------------------+
            |                 |                    |
            |  exit_read()    |                    |
            +-------->--------+                    |
            |                 |                    |
            |                 | no readers         |
            |                 +----------->--------+
            |                 |       (safe to evict)
```

**Guarantee:** Nodes are only evicted after all readers from the pre-eviction epoch have completed their operations.

### Thread Safety Primitives

| Component | Primitive | Purpose |
|-----------|-----------|---------|
| `LruRegistry.trackers` | `DashMap` | Lock-free concurrent access tracking |
| `AccessTracker` fields | `AtomicU64` | Lock-free timestamp/count updates |
| `EpochManager.global_epoch` | `AtomicU64` | Lock-free epoch advancement |
| `EpochManager.active_readers` | `AtomicUsize` | Lock-free reader counting |
| `EvictionCoordinator.request_queue` | `Mutex + Condvar` | Thread-safe request queueing |
| `EvictionCoordinator.disk_registry` | `RwLock` | Concurrent registry access |

### Non-Blocking Guarantees

| Operation | Blocking Behavior |
|-----------|-------------------|
| `touch_node()` | Non-blocking (atomic DashMap ops) |
| `request_eviction()` | Non-blocking (mutex + condvar) |
| `lookup()` / `contains()` | Non-blocking (epoch enter/exit) |
| `insert()` | Non-blocking (invalidates registry) |
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
};
```

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

**Source:** `src/artrie_trait.rs:513-584`

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

### Dirty Nodes (Modified After Checkpoint)

Nodes modified after the last checkpoint cannot be evicted because:
1. Their disk representation is stale
2. Evicting them would lose uncommitted changes

The `DiskLocationRegistry` is **invalidated** on any write operation:

```rust
pub fn invalidate_registry(&self) {
    self.disk_registry.write().invalidate();
}
```

Eviction is skipped when the registry is invalid:

```rust
if !disk_registry.is_valid() {
    return (0, 0);
}
```

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

### Registry Invalidation on Writes

Any write operation (insert, remove) invalidates the disk registry:

```rust
// In insert():
if let Some(coordinator) = &self.eviction_coordinator {
    coordinator.invalidate_registry();
}
```

A new registry is populated during the next checkpoint.

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

| File | Lines | Content |
|------|-------|---------|
| `src/persistent_artrie/eviction/mod.rs` | 1-63 | Module structure, public exports |
| `src/persistent_artrie/eviction/config.rs` | 1-483 | `EvictionConfig`, `EvictionUrgency`, `EvictionStats` |
| `src/persistent_artrie/eviction/coordinator.rs` | 1-707 | `EvictionCoordinator` implementation |
| `src/persistent_artrie/eviction/lru_tracker.rs` | 1-494 | `LruRegistry`, `AccessTracker` |
| `src/persistent_artrie/eviction/disk_registry.rs` | 1-582 | `DiskLocationRegistry`, `EvictableNode` |
| `src/persistent_artrie/concurrency.rs` | 233-362 | `EpochManager` |
| `src/artrie_trait.rs` | 513-584 | `EvictableARTrie` trait definition |
| `src/persistent_artrie/dict_impl.rs` | 6060-6202 | `EvictableARTrie` implementation |
