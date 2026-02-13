# Lock-Free CAS-Based ARTrie Design

This document describes the lock-free concurrent insert mechanism for `PersistentARTrie` and `PersistentARTrieChar` using persistent (immutable) data structures and Compare-And-Swap (CAS) operations.

## Overview

Traditional concurrent trie implementations use locks (RwLock) which serialize writes and can cause contention when many threads insert concurrently. This design uses **persistent data structures** from the `im` crate combined with **CAS operations** to achieve truly lock-free concurrent inserts.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                      Lock-Free ARTrie Architecture                       │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│   ┌──────────────────┐                                                  │
│   │   AtomicNodePtr  │  ◄── Root pointer (CAS target)                   │
│   │   (lockfree_root)│                                                  │
│   └────────┬─────────┘                                                  │
│            │ load()                                                      │
│            ▼                                                            │
│   ┌──────────────────┐     Immutable Node Structure                     │
│   │ PersistentNode   │     ┌────────────────────────────┐              │
│   │ ├─ version       │     │ Modifications create NEW   │              │
│   │ ├─ keys (im::Vec)├────►│ nodes, never mutate old    │              │
│   │ ├─ children      │     │ ones. Old nodes are        │              │
│   │ ├─ flags (atomic)│     │ reclaimed via Arc refcount │              │
│   │ └─ value (atomic)│     └────────────────────────────┘              │
│   └──────────────────┘                                                  │
│                                                                          │
│   ┌──────────────────┐     Fast-path for duplicate detection            │
│   │    DashMap       │     ┌────────────────────────────┐              │
│   │ (lockfree_cache) │────►│ Term → bool (lock-free     │              │
│   │                  │     │ sharded HashMap)           │              │
│   └──────────────────┘     └────────────────────────────┘              │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

## Key Components

### 1. PersistentNode (Immutable Node)

Uses `im::Vector` for keys and children to enable O(log n) structural sharing:

```rust
pub struct PersistentNode {
    version: AtomicU64,           // Monotonic version counter
    keys: im::Vector<u8>,         // Sorted child keys
    children: im::Vector<SwizzledPtr>, // Child pointers
    flags: AtomicU8,              // IS_FINAL, HAS_VALUE, etc.
    value: AtomicU64,             // Value for final nodes
    prefix: Arc<[u8]>,            // Path compression
}
```

**Key property:** `with_child(key, child)` returns a NEW node, never mutates `self`.

### 2. AtomicNodePtr (CAS-able Pointer)

Wraps `Arc<PersistentNode>` for atomic compare-and-swap:

```rust
pub struct AtomicNodePtr {
    ptr: AtomicU64,  // Raw pointer stored as u64
}

impl AtomicNodePtr {
    fn compare_exchange(
        &self,
        expected: &Arc<PersistentNode>,
        new: Arc<PersistentNode>,
    ) -> Result<(), Arc<PersistentNode>>;
}
```

## Lock-Free Insert Algorithm

### Phase 1: Build New Tree Structure

```
Input: term = "cat"

       OLD ROOT                    NEW ROOT (built recursively)
          │                              │
          ▼                              ▼
     ┌────────┐                    ┌────────┐
     │ Node A │ ─────────────────► │ Node A'│  (new version)
     │ 'a'─►B │                    │ 'a'─►B │
     │ 'c'─►? │ (no child)         │ 'c'─►C'│  (new child)
     └────────┘                    └────────┘
                                        │
                                        ▼
                                   ┌────────┐
                                   │ Node C'│  (new)
                                   │ 'a'─►D'│
                                   └────────┘
                                        │
                                        ▼
                                   ┌────────┐
                                   │ Node D'│  (leaf, FINAL)
                                   │ 't'─►  │
                                   └────────┘
```

### Phase 2: CAS at Root

```
Thread 1                          Thread 2
─────────                         ─────────
1. Load root                      1. Load root
2. Build new tree for "cat"       2. Build new tree for "dog"
3. CAS(root, old → new_cat)       3. CAS(root, old → new_dog)
   │                                 │
   ▼                                 ▼
   SUCCESS!                       FAIL (root changed!)
                                     │
                                     ▼
                                  4. Retry from step 1
                                     (re-read new root)
```

### Recursive Path Building (Bottom-Up)

The algorithm builds the path from **leaf to root**:

```
insert_recursive(node, "cat", depth=0):
  │
  ├─ depth=0: key='c'
  │   └─ No child for 'c' → create_path("at")
  │       │
  │       ├─ Create leaf (final node for 't')
  │       ├─ Wrap with 'a' → intermediate node
  │       └─ Return (subtree_root, leaf)
  │
  └─ Return node.with_child('c', subtree_root)

create_path("at"):
  ┌─────────┐
  │ leaf    │  ◄── Created first (will be marked FINAL)
  │ (empty) │
  └─────────┘
       ▲
       │ wrap with 't'
  ┌─────────┐
  │ 't' ──► │
  └─────────┘
       ▲
       │ wrap with 'a'
  ┌─────────┐
  │ 'a' ──► │  ◄── subtree_root (returned)
  └─────────┘
```

## Memory Management

### Arc Reference Counting

```
┌────────────────────────────────────────────────────────────────┐
│                    Arc Lifecycle During CAS                     │
├────────────────────────────────────────────────────────────────┤
│                                                                 │
│  1. Load from AtomicNodePtr                                    │
│     ┌─────────────────┐                                        │
│     │ Arc::increment_ │  ← Increment refcount BEFORE returning │
│     │ strong_count    │    (prevents use-after-free)           │
│     └─────────────────┘                                        │
│                                                                 │
│  2. Build new tree (creates new Arcs via Arc::new)             │
│     ┌─────────────────┐                                        │
│     │ Arc::into_raw   │  ← Convert to raw pointer for storage  │
│     └─────────────────┘    in SwizzledPtr                      │
│                                                                 │
│  3. CAS fails                                                   │
│     ┌─────────────────┐                                        │
│     │ Arc::from_raw   │  ← Reclaim rejected Arc to drop it     │
│     └─────────────────┘                                        │
│                                                                 │
│  4. Old nodes unreachable                                       │
│     ┌─────────────────┐                                        │
│     │ Arc refcount→0  │  ← Automatic deallocation              │
│     └─────────────────┘                                        │
│                                                                 │
└────────────────────────────────────────────────────────────────┘
```

### Epoch-Based Reclamation

The epoch manager protects against ABA problems:

```
Thread A (reader)              Thread B (writer)
─────────────────              ─────────────────
enter_read()
  │                            CAS old → new
  │ (pinned to epoch)              │
  ▼                                ▼
Read old node safely           Old node queued for reclamation
  │                                │
exit_read()                        │
  │                                ▼
  └────────────────────────────► Epoch advances, old node freed
```

## Finalization Race Handling

When multiple threads insert the same term:

```
Thread 1                          Thread 2
─────────                         ─────────
1. Build path to "hello"          1. Build path to "hello"
2. CAS root → new_tree            2. CAS fails (retry)
3. try_set_final(leaf)            3. Re-build with new root
   │                              4. CAS root → new_tree_2
   ▼                              5. try_set_final(leaf_2)
   SUCCESS (wins race)               │
   Return true (inserted)            ▼
                                  FAIL (already set)
                                  Return false (already exists)
```

The `try_set_final()` uses atomic `fetch_or`:

```rust
pub fn try_set_final(&self) -> bool {
    let old = self.flags.fetch_or(IS_FINAL, Ordering::AcqRel);
    (old & IS_FINAL) == 0  // Returns true only if THIS call set it
}
```

## Structural Sharing with im::Vector

```
┌─────────────────────────────────────────────────────────────────┐
│                     Structural Sharing                           │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Original: keys = [a, b, c, d, e, f, g, h]                      │
│                                                                  │
│            ┌───┬───┬───┬───┬───┬───┬───┬───┐                    │
│            │ a │ b │ c │ d │ e │ f │ g │ h │                    │
│            └───┴───┴───┴───┴───┴───┴───┴───┘                    │
│                        ▲                                         │
│                        │ (shared)                                │
│                        │                                         │
│  After insert('x'):    │                                         │
│            ┌───┬───┬───┼───┬───┬───┬───┬───┬───┐                │
│            │ a │ b │ c │ d │ e │ f │ g │ h │ x │  ◄─ New node   │
│            └───┴───┴───┴───┴───┴───┴───┴───┴───┘    only copies │
│                                                      O(log n)    │
│                                                      elements    │
│                                                                  │
│  Original vector UNCHANGED - both versions coexist              │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## Performance Characteristics

| Operation | Complexity | Notes |
|-----------|------------|-------|
| Cache lookup | O(1) | DashMap sharded access |
| Tree traversal | O(k) | k = term length |
| Node modification | O(log n) | n = children count (structural sharing) |
| CAS retry | O(1) expected | Contention-dependent |

### CAS Retry Statistics

Under typical workloads with unique terms per thread:
- **Retry rate: < 1%** for unique terms
- **Retry rate: ~10-50%** when multiple threads insert same terms
- Tracked via `cas_retries` atomic counter

## Integration with Persistent Storage

```
┌─────────────────────────────────────────────────────────────────┐
│                  Lock-Free + Persistent Hybrid                   │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  1. DURING IMPORT (high concurrency)                            │
│     ┌──────────────────────────────────────────────────────┐   │
│     │              Lock-Free Overlay                        │   │
│     │  ┌─────────────┐     ┌─────────────┐                 │   │
│     │  │ AtomicNodePtr│     │  DashMap    │                 │   │
│     │  │   (root)     │     │  (cache)    │                 │   │
│     │  └─────────────┘     └─────────────┘                 │   │
│     └──────────────────────────────────────────────────────┘   │
│                         ▲                                        │
│                         │ insert_cas()                          │
│                         │                                        │
│     ════════════════════╧════════════════════════════════════   │
│                                                                  │
│  2. ON CHECKPOINT (merge to persistent)                         │
│     ┌──────────────────────────────────────────────────────┐   │
│     │            merge_lockfree_to_persistent()             │   │
│     │  - Iterates cache entries                             │   │
│     │  - Inserts into persistent trie (with WAL)            │   │
│     │  - Clears cache                                       │   │
│     └──────────────────────────────────────────────────────┘   │
│                         │                                        │
│                         ▼                                        │
│     ┌──────────────────────────────────────────────────────┐   │
│     │              Persistent ARTrie                        │   │
│     │  ┌─────────────┐     ┌─────────────┐                 │   │
│     │  │  Disk-backed│     │    WAL      │                 │   │
│     │  │    nodes    │     │  (recovery) │                 │   │
│     │  └─────────────┘     └─────────────┘                 │   │
│     └──────────────────────────────────────────────────────┘   │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## API Usage

### Enable Lock-Free Mode

```rust
let mut trie = PersistentARTrie::create("vocab.part")?;
trie.enable_lockfree();  // Must call before using CAS methods
```

### Concurrent Inserts (No Locks!)

```rust
let trie = Arc::new(trie);  // No RwLock needed!

let handles: Vec<_> = (0..12).map(|i| {
    let t = Arc::clone(&trie);
    thread::spawn(move || {
        for term in get_terms_for_thread(i) {
            t.insert_cas(term.as_bytes());
        }
    })
}).collect();
```

### Check Existence

```rust
if trie.contains_lockfree(b"hello") {
    println!("Found!");
}
```

### Merge to Persistent Storage

```rust
let merged_count = trie.merge_lockfree_to_persistent()?;
```

## Thread Safety Guarantees

| Component | Thread-Safe? | Mechanism |
|-----------|:------------:|-----------|
| `insert_cas()` | ✓ | CAS + retry loop |
| `contains_lockfree()` | ✓ | Immutable traversal |
| `try_set_final()` | ✓ | Atomic fetch_or |
| `lockfree_cache` | ✓ | DashMap (sharded) |
| `AtomicNodePtr` | ✓ | AtomicU64 + Arc |

## Files

| File | Description |
|------|-------------|
| `persistent_artrie/nodes/persistent_node.rs` | PersistentNode (u8 keys) |
| `persistent_artrie/nodes/atomic_ptr.rs` | AtomicNodePtr for u8 nodes |
| `persistent_artrie/dict_impl.rs` | Lock-free methods for PersistentARTrie |
| `persistent_artrie_char/nodes/persistent_node.rs` | PersistentCharNode (u32 keys) |
| `persistent_artrie_char/nodes/atomic_ptr.rs` | AtomicNodePtr for char nodes |
| `persistent_artrie_char/dict_impl_char.rs` | Lock-free methods for PersistentARTrieChar |
| `persistent_vocab_artrie/lockfree.rs` | LockFreeVocab with atomic index allocation |

## References

- [im crate documentation](https://docs.rs/im/)
- [Epoch-based memory reclamation](https://www.cl.cam.ac.uk/techreports/UCAM-CL-TR-579.pdf)
- [ART: Adaptive Radix Tree](https://db.in.tum.de/~leis/papers/ART.pdf)
