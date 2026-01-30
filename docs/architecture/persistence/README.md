# ARTrie Persistence Architecture

**The Core Question: If memory-mapped files are immutable, how can ARTrie be read-from and written-to?**

This document provides a thorough, pedagogical explanation of how the Persistent ARTrie achieves durable read-write operations despite using memory-mapped files for fast access. We build from simple concepts to the complete architecture using visual diagrams throughout.

---

## Table of Contents

1. [The Apparent Paradox](#1-the-apparent-paradox)
2. [The Solution: Hybrid Architecture](#2-the-solution-hybrid-architecture)
3. [Visual Diagrams](#3-visual-diagrams)
   - [Overall I/O Stack](#31-overall-io-stack)
   - [Arena (Slotted Page) Structure](#32-arena-slotted-page-structure)
   - [Disk File Layout](#33-disk-file-layout)
   - [Write Flow (Three-Phase Commit)](#34-write-flow-three-phase-commit)
   - [Crash Recovery Flow](#35-crash-recovery-flow)
   - [Dirty Tracking (Incremental Checkpoint)](#36-dirty-tracking-incremental-checkpoint)
   - [Relative Offset Encoding](#37-relative-offset-encoding)
4. [Key Concepts Explained](#4-key-concepts-explained)
   - [Write-Ahead Logging (WAL)](#41-write-ahead-logging-wal)
   - [Arena-Based Allocation](#42-arena-based-allocation)
   - [Buffer Manager](#43-buffer-manager)
   - [Dirty Tracking](#44-dirty-tracking)
   - [Three-Phase Commit](#45-three-phase-commit)
5. [Worked Example: Insert Operation](#5-worked-example-insert-operation)
6. [Comparison with Alternatives](#6-comparison-with-alternatives)
7. [ACID Guarantees](#7-acid-guarantees)
8. [Summary](#8-summary)
9. [Cross-References](#9-cross-references)

---

## 1. The Apparent Paradox

Memory-mapped files (`mmap`) provide zero-copy access to disk data by mapping file contents directly into process address space. This is extremely efficient for reads:

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Traditional mmap Read                           │
│                                                                     │
│   Process Memory                         Disk File                  │
│  ┌──────────────┐                      ┌──────────────┐             │
│  │ Virtual Addr │ ────── mmap ──────►  │ File Offset  │             │
│  │  0x7fff...   │    (page table)      │     0x0      │             │
│  └──────────────┘                      └──────────────┘             │
│                                                                     │
│   Read via pointer dereference:                                     │
│   ✓ Zero-copy (no memcpy into buffer)                               │
│   ✓ Kernel manages page caching                                     │
│   ✓ Lazy loading (pages fetched on demand)                          │
└─────────────────────────────────────────────────────────────────────┘
```

**The Problem**: While mmap allows reads, writes to the mapped region are problematic:

1. **No durability guarantee** — The kernel may delay writeback indefinitely
2. **No atomicity** — A crash during writeback can leave partial writes
3. **No ordering guarantee** — Pages may be written in any order
4. **No transactional semantics** — Can't rollback failed operations

If we write directly to the mmap'd region:

```
┌─────────────────────────────────────────────────────────────────────┐
│                    UNSAFE: Direct mmap Write                        │
│                                                                     │
│   ┌─────────────────┐        ┌─────────────────┐                   │
│   │ Process writes  │        │   Disk File     │                   │
│   │ to mmap'd memory│  ???   │                 │                   │
│   │   node.key = X  │ ─────► │  When written?  │                   │
│   └─────────────────┘        │  Complete?      │                   │
│                              │  Ordered?       │                   │
│        ❌ CRASH              └─────────────────┘                   │
│   ┌────────────────────────────────────────────┐                   │
│   │ File corruption: partial node, lost data,  │                   │
│   │ inconsistent pointers, unreachable nodes   │                   │
│   └────────────────────────────────────────────┘                   │
└─────────────────────────────────────────────────────────────────────┘
```

**The Paradox**: We want mmap's read performance but also want safe, durable writes.

---

## 2. The Solution: Hybrid Architecture

The Persistent ARTrie solves this with a **hybrid write path**:

1. **Reads**: Use mmap for zero-copy access (fast path)
2. **Writes**: Use a Write-Ahead Log (WAL) plus explicit I/O (safe path)

The key insight is **separation of concerns**:

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Hybrid Architecture Overview                    │
│                                                                     │
│   ┌─────────────────────────────────────────────────────────────┐  │
│   │                        READ PATH (Fast)                      │  │
│   │                                                              │  │
│   │    Application                                               │  │
│   │        │                                                     │  │
│   │        ▼                                                     │  │
│   │   SwizzledPtr ──► is_swizzled()? ──► Memory Pointer          │  │
│   │                        │                                     │  │
│   │                        ▼ (on first access)                   │  │
│   │               Buffer Manager ──► mmap'd Data File            │  │
│   │                        │                                     │  │
│   │                        ▼                                     │  │
│   │               Swizzle to Memory                              │  │
│   └─────────────────────────────────────────────────────────────┘  │
│                                                                     │
│   ┌─────────────────────────────────────────────────────────────┐  │
│   │                       WRITE PATH (Safe)                      │  │
│   │                                                              │  │
│   │    Application                                               │  │
│   │        │                                                     │  │
│   │        ▼                                                     │  │
│   │    1. Log to WAL (fsync) ────────────────► WAL File          │  │
│   │        │                                                     │  │
│   │        ▼                                                     │  │
│   │    2. Update In-Memory Arena                                 │  │
│   │        │                                                     │  │
│   │        ▼                                                     │  │
│   │    3. Mark Arena Dirty ──────────────────► DirtyTracker      │  │
│   │        │                                                     │  │
│   │        ▼ (on checkpoint)                                     │  │
│   │    4. Flush Dirty Arenas ────────────────► Data File         │  │
│   └─────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

**Why This Works**:

- The **WAL** guarantees durability — once logged, the operation survives crashes
- **In-memory arenas** provide fast writes — no disk I/O until checkpoint
- **Checkpoint** makes changes permanent — WAL can be truncated after
- **Crash recovery** replays WAL to rebuild any lost in-memory state

---

## 3. Visual Diagrams

### 3.1 Overall I/O Stack

This diagram shows all layers from application down to disk:

```
┌─────────────────────────────────────────────────────────────────────┐
│                         APPLICATION LAYER                           │
│  ┌───────────────┐  ┌───────────────┐  ┌─────────────────────────┐ │
│  │  Dictionary   │  │  MappedDict   │  │ MutableMappedDictionary │ │
│  │    trait      │  │    trait      │  │   trait (insert/remove) │ │
│  └───────────────┘  └───────────────┘  └─────────────────────────┘ │
└─────────────────────────────┬───────────────────────────────────────┘
                              │
┌─────────────────────────────┴───────────────────────────────────────┐
│                         TRIE LAYER                                  │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │              PersistentARTrie / PersistentARTrieChar          │  │
│  │  ┌─────────────────────────────────────────────────────────┐ │  │
│  │  │ Root SwizzledPtr ──► Node4/16/48/256 ──► ... ──► Bucket │ │  │
│  │  └─────────────────────────────────────────────────────────┘ │  │
│  └──────────────────────────────────────────────────────────────┘  │
└─────────────────────────────┬───────────────────────────────────────┘
                              │
┌─────────────────────────────┴───────────────────────────────────────┐
│                        STORAGE LAYER                                │
│                                                                     │
│  ┌──────────────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │    Arena Manager     │  │  WAL Writer  │  │   Buffer Manager │  │
│  │  (slotted pages,     │  │  (durability │  │  (page cache,    │  │
│  │   bump allocation)   │  │   logging)   │  │   LRU eviction)  │  │
│  └──────────┬───────────┘  └──────┬───────┘  └────────┬─────────┘  │
│             │                     │                   │             │
│  ┌──────────┴─────────────────────┴───────────────────┴──────────┐ │
│  │                        Disk Manager                            │ │
│  │               (block I/O, file management)                     │ │
│  └────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────┬───────────────────────────────────────┘
                              │
┌─────────────────────────────┴───────────────────────────────────────┐
│                         DISK LAYER                                  │
│                                                                     │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │   data.part       │     data.wal      │   wal_archive/*.seg  │  │
│  │  (main data)      │ (write-ahead log) │ (archived WAL segs)  │  │
│  └──────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

### 3.2 Arena (Slotted Page) Structure

Arenas pack multiple nodes efficiently using a slotted page design:

```
┌─────────────────────────────────────────────────────────────────────┐
│                    Arena Layout (256 KB Block)                      │
│                                                                     │
│  Byte 0                                                  Byte 262143│
│  ┌─────────────────────────────────────────────────────────────────┐
│  │                                                                 │
│  │  ┌─────────────────────────────────────────────────────────┐   │
│  │  │                   ArenaHeader (64 bytes)                 │   │
│  │  │  magic: "BYTARANA" │ version │ flags │ node_count      │   │
│  │  │  free_offset       │ directory_start │ checksum        │   │
│  │  └─────────────────────────────────────────────────────────┘   │
│  │                                                                 │
│  │  ┌─────────────────────────────────────────────────────────┐   │
│  │  │              Data Area (grows ↓ downward)               │   │
│  │  │                                                         │   │
│  │  │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐  │   │
│  │  │  │  Node 0  │ │  Node 1  │ │  Node 2  │ │  Node 3  │  │   │
│  │  │  │ (48 B)   │ │ (160 B)  │ │ (64 B)   │ │ (656 B)  │  │   │
│  │  │  └──────────┘ └──────────┘ └──────────┘ └──────────┘  │   │
│  │  │                                                         │   │
│  │  │  ◄─────────── free_offset points here ──────────────►  │   │
│  │  │                                                         │   │
│  │  │                   [  Free Space  ]                      │   │
│  │  │                                                         │   │
│  │  │  ◄────────── directory_start points here ────────────► │   │
│  │  │                                                         │   │
│  │  └─────────────────────────────────────────────────────────┘   │
│  │                                                                 │
│  │  ┌─────────────────────────────────────────────────────────┐   │
│  │  │            Slot Directory (grows ↑ upward)              │   │
│  │  │                                                         │   │
│  │  │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐  │   │
│  │  │  │ Slot 3   │ │ Slot 2   │ │ Slot 1   │ │ Slot 0   │  │   │
│  │  │  │off:len   │ │off:len   │ │off:len   │ │off:len   │  │   │
│  │  │  └──────────┘ └──────────┘ └──────────┘ └──────────┘  │   │
│  │  └─────────────────────────────────────────────────────────┘   │
│  │                                                                 │
│  └─────────────────────────────────────────────────────────────────┘
│                                                                     │
│  Key Points:                                                        │
│  • Data grows downward from header (bump allocation)                │
│  • Directory grows upward from end (slot tracking)                  │
│  • When they meet, arena is full → allocate new arena               │
│  • Slot ID = index in directory → stable reference even if moved    │
└─────────────────────────────────────────────────────────────────────┘
```

### 3.3 Disk File Layout

The main data file organization:

```
┌─────────────────────────────────────────────────────────────────────┐
│                      data.part File Layout                          │
│                                                                     │
│  Block 0 (Header):                                                  │
│  ┌─────────────────────────────────────────────────────────────────┐
│  │  Magic: 0x5041_5254_0001_0000 ("PART" + version)                │
│  │  Root Descriptor: (arena_id, slot_id, node_type)                │
│  │  Arena Count, Entry Count, Checksum, etc.                       │
│  └─────────────────────────────────────────────────────────────────┘
│                                                                     │
│  Blocks 1..N (Arenas):                                              │
│  ┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐       │
│  │  Arena 0        │ │  Arena 1        │ │  Arena 2        │  ...  │
│  │  (Block 1)      │ │  (Block 2)      │ │  (Block 3)      │       │
│  │                 │ │                 │ │                 │       │
│  │  ┌───────────┐  │ │  ┌───────────┐  │ │  ┌───────────┐  │       │
│  │  │ Node4     │  │ │  │ Node16    │  │ │  │ Node48    │  │       │
│  │  │ Node16    │  │ │  │ Node4     │  │ │  │ Bucket    │  │       │
│  │  │ Node4     │  │ │  │ Node4     │  │ │  │ Node16    │  │       │
│  │  │ ...       │  │ │  │ ...       │  │ │  │ ...       │  │       │
│  │  └───────────┘  │ │  └───────────┘  │ │  └───────────┘  │       │
│  │                 │ │                 │ │                 │       │
│  │  [Directory]    │ │  [Directory]    │ │  [Directory]    │       │
│  └─────────────────┘ └─────────────────┘ └─────────────────┘       │
│                                                                     │
│  Addressing:                                                        │
│  • Arena N is stored in Block N+1 (Block 0 = header)               │
│  • Node address = (arena_id, slot_id) → (block_id = arena_id + 1)  │
│  • SwizzledPtr encodes: block_id (23 bits) + offset (22 bits)      │
│                                                                     │
│  With 256KB blocks: 8M blocks × 256KB = 2TB addressable            │
└─────────────────────────────────────────────────────────────────────┘
```

### 3.4 Write Flow (Three-Phase Commit)

How an insert operation achieves durability:

```
┌─────────────────────────────────────────────────────────────────────┐
│                    Three-Phase Write Commit                         │
│                                                                     │
│  ════════════════════════════════════════════════════════════════  │
│                       PHASE 1: LOG (Durability)                     │
│  ════════════════════════════════════════════════════════════════  │
│                                                                     │
│     Application                           WAL File (data.wal)       │
│    ┌───────────┐                         ┌───────────────────┐     │
│    │ insert(   │   1. Serialize          │  ┌─────────────┐  │     │
│    │  "hello", │ ─────────────────────►  │  │ LSN: 1001   │  │     │
│    │  value    │   2. Append record      │  │ Type: INSERT│  │     │
│    │ )         │                         │  │ Term: hello │  │     │
│    └───────────┘   3. fsync() ──────────►│  │ Value: ...  │  │     │
│                    [DURABLE POINT]       │  │ CRC: 0x...  │  │     │
│                                          │  └─────────────┘  │     │
│    After fsync: operation survives       └───────────────────┘     │
│    crash even if never written to                                   │
│    data file!                                                       │
│                                                                     │
│  ════════════════════════════════════════════════════════════════  │
│                       PHASE 2: APPLY (In-Memory)                    │
│  ════════════════════════════════════════════════════════════════  │
│                                                                     │
│     In-Memory Arena                      DirtyTracker               │
│    ┌───────────────────┐                ┌───────────────────┐      │
│    │                   │                │                   │      │
│    │  4. Allocate slot │                │  6. Mark arena    │      │
│    │     in arena      │                │     as dirty      │      │
│    │                   │                │                   │      │
│    │  5. Write node    │  ─────────────►│  dirty_arenas:    │      │
│    │     bytes         │                │    {0, 3, 7, ...} │      │
│    │                   │                │                   │      │
│    │  ┌─────────────┐  │                └───────────────────┘      │
│    │  │ New Node    │  │                                           │
│    │  │ "hello"     │  │   No disk I/O here — very fast!           │
│    │  └─────────────┘  │                                           │
│    └───────────────────┘                                           │
│                                                                     │
│  ════════════════════════════════════════════════════════════════  │
│                      PHASE 3: CHECKPOINT (Persistence)              │
│  ════════════════════════════════════════════════════════════════  │
│                                                                     │
│    (Later, on checkpoint trigger)                                   │
│                                                                     │
│     DirtyTracker          Arena Manager          Data File          │
│    ┌─────────────┐       ┌─────────────┐       ┌─────────────┐     │
│    │ dirty:      │ 7.    │             │ 8.    │             │     │
│    │ {0, 3, 7}   │ ────► │ Get arena 0 │ ────► │ Write block │     │
│    │             │ iter  │ Get arena 3 │ write │ 1, 4, 8     │     │
│    │ 9. clear()  │ ◄──── │ Get arena 7 │ ◄──── │ fsync()     │     │
│    └─────────────┘       └─────────────┘       └─────────────┘     │
│                                                                     │
│    10. Write checkpoint record to WAL                               │
│    11. Truncate/rotate WAL (old records no longer needed)           │
│                                                                     │
│  ════════════════════════════════════════════════════════════════  │
│                                                                     │
│  CRASH SCENARIOS:                                                   │
│                                                                     │
│  • Crash before Phase 1 fsync: Operation lost (acceptable)          │
│  • Crash after Phase 1, before Phase 3: WAL replay recovers it      │
│  • Crash during Phase 3: WAL replay recovers incomplete writes      │
│  • Crash after Phase 3: Fully persisted, no recovery needed         │
└─────────────────────────────────────────────────────────────────────┘
```

### 3.5 Crash Recovery Flow

How the system recovers from a crash:

```
┌─────────────────────────────────────────────────────────────────────┐
│                      Crash Recovery Flow                            │
│                                                                     │
│                        System Startup                               │
│                             │                                       │
│                             ▼                                       │
│                   ┌─────────────────┐                               │
│                   │ Check for WAL   │                               │
│                   │ file exists?    │                               │
│                   └────────┬────────┘                               │
│                            │                                        │
│              ┌─────────────┴─────────────┐                          │
│              │                           │                          │
│              ▼ No                        ▼ Yes                      │
│    ┌─────────────────┐         ┌─────────────────────┐              │
│    │ Normal startup  │         │ Recovery needed     │              │
│    │ Load data file  │         │                     │              │
│    └─────────────────┘         └──────────┬──────────┘              │
│                                           │                         │
│                                           ▼                         │
│                               ┌─────────────────────┐               │
│                               │  ANALYSIS PHASE     │               │
│                               │                     │               │
│                               │  1. Read WAL header │               │
│                               │  2. Find last       │               │
│                               │     checkpoint LSN  │               │
│                               │  3. Scan for        │               │
│                               │     committed txns  │               │
│                               └──────────┬──────────┘               │
│                                          │                          │
│                                          ▼                          │
│                               ┌─────────────────────┐               │
│                               │    REDO PHASE       │               │
│                               │                     │               │
│                               │  For each record    │               │
│                               │  after checkpoint:  │               │
│                               │                     │               │
│                               │  ┌───────────────┐  │               │
│                               │  │ LSN: 1001     │  │               │
│                               │  │ INSERT hello  │──┼──► Apply      │
│                               │  └───────────────┘  │    to trie    │
│                               │  ┌───────────────┐  │               │
│                               │  │ LSN: 1002     │  │               │
│                               │  │ INSERT world  │──┼──► Apply      │
│                               │  └───────────────┘  │    to trie    │
│                               │  ┌───────────────┐  │               │
│                               │  │ LSN: 1003     │  │               │
│                               │  │ REMOVE hello  │──┼──► Apply      │
│                               │  └───────────────┘  │    to trie    │
│                               │                     │               │
│                               └──────────┬──────────┘               │
│                                          │                          │
│                                          ▼                          │
│                               ┌─────────────────────┐               │
│                               │   CLEANUP PHASE     │               │
│                               │                     │               │
│                               │  1. Checkpoint      │               │
│                               │     recovered state │               │
│                               │                     │               │
│                               │  2. Truncate WAL    │               │
│                               │     (or rotate to   │               │
│                               │      archive)       │               │
│                               │                     │               │
│                               │  3. Resume normal   │               │
│                               │     operation       │               │
│                               └─────────────────────┘               │
│                                                                     │
│  Key Invariant:                                                     │
│  After checkpoint LSN, all operations are in WAL → nothing lost     │
│  Before checkpoint LSN, all operations are in data file → durable   │
└─────────────────────────────────────────────────────────────────────┘
```

### 3.6 Dirty Tracking (Incremental Checkpoint)

How dirty tracking minimizes checkpoint I/O:

```
┌─────────────────────────────────────────────────────────────────────┐
│                   Dirty Tracking for Incremental Checkpoint         │
│                                                                     │
│  PROBLEM: Full checkpoint writes all arenas, even unchanged ones    │
│                                                                     │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │  Before: 1 node modified out of 1 million nodes (~100 arenas)  │ │
│  │  Full checkpoint: write all 100 arenas                         │ │
│  │  I/O: 100 × 256KB = 25.6 MB                                    │ │
│  └────────────────────────────────────────────────────────────────┘ │
│                                                                     │
│  SOLUTION: Track which arenas have been modified                    │
│                                                                     │
│  ┌────────────────────────────────────────────────────────────────┐ │
│  │  After: 1 node modified in arena 42                            │ │
│  │  Incremental checkpoint: write only arena 42                   │ │
│  │  I/O: 1 × 256KB = 256 KB (99% reduction!)                      │ │
│  └────────────────────────────────────────────────────────────────┘ │
│                                                                     │
│  ════════════════════════════════════════════════════════════════  │
│                                                                     │
│  DirtyTracker State:                                                │
│                                                                     │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │                    dirty_arenas: HashSet<u32>                 │  │
│  │                                                               │  │
│  │   Arena ID:  0   1   2   3   4   5   6   7   8   9  ...      │  │
│  │   Status:   [ ] [█] [ ] [ ] [█] [ ] [ ] [█] [ ] [ ]          │  │
│  │                  ▲           ▲           ▲                    │  │
│  │                  │           │           │                    │  │
│  │              modified    modified    modified                 │  │
│  │                                                               │  │
│  │   dirty_arenas = {1, 4, 7}                                   │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                     │
│  ════════════════════════════════════════════════════════════════  │
│                                                                     │
│  Timeline:                                                          │
│                                                                     │
│    Time ──────────────────────────────────────────────────────►    │
│                                                                     │
│    insert("a")     insert("b")     insert("c")    CHECKPOINT        │
│        │               │               │              │             │
│        ▼               ▼               ▼              ▼             │
│    mark_dirty(1)   mark_dirty(1)   mark_dirty(4)  flush({1,4})     │
│                                                       │             │
│    {1}             {1}             {1, 4}         clear()           │
│                                                       │             │
│                                                    {} (empty)       │
│                                                                     │
│  ════════════════════════════════════════════════════════════════  │
│                                                                     │
│  Slot-Level Tracking (Implemented)                                  │
│                                                                     │
│  For even finer granularity, slot-level dirty tracking writes only │
│  modified slots instead of entire arenas. Enabled via FlushConfig:  │
│                                                                     │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │  FlushConfig: Opt-in configuration for slot-level tracking    │  │
│  │                                                               │  │
│  │  struct FlushConfig {                                         │  │
│  │      slot_level_tracking: bool,   // Enable slot tracking     │  │
│  │      full_arena_threshold: f64,   // Default: 0.5 (50%)       │  │
│  │  }                                                            │  │
│  │                                                               │  │
│  │  When enabled:                                                │  │
│  │  - Individual dirty slots are tracked during allocation       │  │
│  │  - Checkpoint writes only modified slots + header + directory │  │
│  │  - Falls back to full arena write when >50% slots dirty       │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                     │
│  Per-Arena Slot Tracking:                                           │
│                                                                     │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │  dirty_slots: HashMap<u32, HashSet<u32>>                      │  │
│  │                                                               │  │
│  │  Arena 1: {slot 5, slot 12, slot 89}                         │  │
│  │  Arena 4: {slot 3}                                            │  │
│  │  Arena 7: {slot 0, slot 1, slot 2}                           │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                     │
│  I/O Savings for single-slot update:                                │
│    Full flush:    256KB (entire arena)                              │
│    Slot-level:    ~200 bytes (header + slot + directory entry)     │
│    Savings:       99.9%                                             │
│                                                                     │
│  ════════════════════════════════════════════════════════════════  │
│                                                                     │
│  FlushStats: Checkpoint Statistics                                  │
│                                                                     │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │  struct FlushStats {                                          │  │
│  │      full_arena_writes: usize,  // Arenas written fully       │  │
│  │      partial_writes: usize,     // Arenas with slot writes    │  │
│  │      slots_written: usize,      // Individual slots written   │  │
│  │      bytes_written: usize,      // Total bytes flushed        │  │
│  │      bytes_saved: usize,        // Bytes avoided vs full      │  │
│  │  }                                                            │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                     │
│  ════════════════════════════════════════════════════════════════  │
│                                                                     │
│  Arena-Level I/O Savings Calculation:                               │
│                                                                     │
│    full_bytes = total_arenas × arena_size                          │
│    incremental_bytes = dirty_arenas.len() × arena_size             │
│    savings = (1 - incremental_bytes / full_bytes) × 100%           │
│                                                                     │
│    Example: 100 arenas, 3 dirty                                     │
│    savings = (1 - 3/100) × 100% = 97%                              │
└─────────────────────────────────────────────────────────────────────┘
```

### 3.7 Relative Offset Encoding

How child pointers achieve space efficiency:

```
┌─────────────────────────────────────────────────────────────────────┐
│                    Relative Offset Encoding                         │
│                                                                     │
│  PROBLEM: Full pointers waste space                                 │
│                                                                     │
│  Fixed encoding: 8 bytes per child pointer                          │
│  10 children = 80 bytes just for pointers!                          │
│                                                                     │
│  OBSERVATION: Children are usually in the same arena as parent      │
│  (post-order serialization allocates children first)                │
│                                                                     │
│  ════════════════════════════════════════════════════════════════  │
│                                                                     │
│  SOLUTION: Relative offsets for same-arena pointers                 │
│                                                                     │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │                      Same Arena                               │  │
│  │                                                               │  │
│  │  Slot:    90   91   92   93   94   95   96   97   98   99    │  │
│  │           │    │    │    │    │    │    │    │    │    │     │  │
│  │          C0   C1   C2   C3   C4   C5   C6   C7   C8  Parent  │  │
│  │           ▲    ▲    ▲    ▲    ▲    ▲    ▲    ▲    ▲    │     │  │
│  │           └────┴────┴────┴────┴────┴────┴────┴────┴────┘     │  │
│  │                          children                             │  │
│  │                                                               │  │
│  │  Parent at slot 99 references child at slot 95:               │  │
│  │  delta = 99 - 95 = 4                                          │  │
│  │                                                               │  │
│  │  Encoding: (delta << 1) | 0 = (4 << 1) | 0 = 8                │  │
│  │  Storage: 1 byte (varint)                                     │  │
│  │                                                               │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                     │
│  ════════════════════════════════════════════════════════════════  │
│                                                                     │
│  Encoding Scheme:                                                   │
│                                                                     │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │                                                               │  │
│  │  Bit 0 = 0: Same-arena relative offset                        │  │
│  │  ┌─────────────────────────────────────────────────────────┐ │  │
│  │  │  Varint: (delta << 1) | 0                               │ │  │
│  │  │                                                         │ │  │
│  │  │  delta=1  → 0x02 (1 byte)                               │ │  │
│  │  │  delta=5  → 0x0A (1 byte)                               │ │  │
│  │  │  delta=63 → 0x7E (1 byte)                               │ │  │
│  │  │  delta=64 → 0x80 0x01 (2 bytes)                         │ │  │
│  │  └─────────────────────────────────────────────────────────┘ │  │
│  │                                                               │  │
│  │  Bit 0 = 1: Cross-arena full pointer                          │  │
│  │  ┌─────────────────────────────────────────────────────────┐ │  │
│  │  │  0x01 | arena_id (4 bytes) | slot_id (4 bytes)          │ │  │
│  │  │                                                         │ │  │
│  │  │  Total: 9 bytes (rare case)                             │ │  │
│  │  └─────────────────────────────────────────────────────────┘ │  │
│  │                                                               │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                     │
│  ════════════════════════════════════════════════════════════════  │
│                                                                     │
│  Space Savings Example:                                             │
│                                                                     │
│  Node16 with 10 children (all same arena, small deltas):            │
│                                                                     │
│  Fixed encoding:   10 × 8 bytes = 80 bytes                          │
│  Relative encoding: 10 × 1 byte = 10 bytes                          │
│  Savings: 87.5%                                                     │
│                                                                     │
│  Sequential Siblings Optimization:                                  │
│                                                                     │
│  If children are allocated consecutively (slots N, N+1, N+2, ...):  │
│  Store only: first_child_slot + count                               │
│  Even more savings for dense node allocations!                      │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 4. Key Concepts Explained

### 4.1 Write-Ahead Logging (WAL)

The WAL is the foundation of durability. Its core protocol:

1. **Log before apply**: Every mutation is written to WAL before modifying data
2. **Sync guarantees durability**: `fsync()` ensures the log record is on stable storage
3. **Replay on crash**: If crash occurs, replay WAL to recover lost changes

**WAL Record Format**:

```
┌──────────┬──────────┬──────────┬──────────┬────────────┐
│  CRC32   │  Length  │   LSN    │   Type   │  Payload   │
│ (4 bytes)│ (4 bytes)│ (8 bytes)│ (1 byte) │  (varies)  │
└──────────┴──────────┴──────────┴──────────┴────────────┘
```

**Record Types**:
- `Insert`: Add a term (with optional value)
- `Remove`: Delete a term
- `Checkpoint`: Mark a durability point
- `BatchInsert`: Multiple inserts in one record (reduces overhead)

**Group Commit**: Multiple operations can share a single `fsync()` for better throughput. Writers append to a buffer; a background thread periodically flushes.

### 4.2 Arena-Based Allocation

Traditional allocators fragment memory over time. Arenas solve this:

- **Bump allocation**: Allocate by moving a pointer forward (O(1))
- **No fragmentation**: Nodes packed contiguously
- **Efficient I/O**: Write entire arena as single block (256KB)
- **Simple deallocation**: Free entire arena at once (for compaction)

**Slot addressing**: Each node gets a stable `slot_id` that doesn't change even if the underlying bytes move during compaction.

### 4.3 Buffer Manager

The buffer manager bridges memory and disk:

- **Page table**: Maps `(arena_id)` → in-memory arena
- **LRU eviction**: When memory is full, evict least-recently-used arenas
- **Pin counting**: Prevent eviction of actively-used arenas
- **Prefetching**: Load arenas before they're needed (for traversal patterns)

**Swizzled Pointers**: A 64-bit value that can be either:
- Memory pointer (MSB = 1): Direct access, no lookup
- Disk reference (MSB = 0): Contains block_id + slot_id, needs loading

On first access, disk references are "swizzled" to memory pointers atomically.

### 4.4 Dirty Tracking

Dirty tracking enables incremental checkpoints with two granularity levels:

```rust
struct DirtyTracker {
    dirty_arenas: HashSet<u32>,              // Arena IDs modified since last checkpoint
    dirty_slots: HashMap<u32, HashSet<u32>>, // Arena → slot IDs (if slot-level tracking)
    epoch: AtomicU64,                        // Incremented on each checkpoint
    track_slots: bool,                       // Whether slot-level tracking is enabled
}
```

**Arena-Level Operations** (always available):
- `mark_arena_dirty(arena_id)`: Called after modifying an arena
- `is_arena_dirty(arena_id)`: Check if arena needs flushing
- `dirty_arena_ids()`: Returns iterator over dirty arenas
- `dirty_arena_count()`: Get number of dirty arenas
- `checkpoint_complete()`: Clears dirty set, increments epoch

**Slot-Level Operations** (when `track_slots: true`):
- `mark_slot_dirty(arena_id, slot_id)`: Track individual slot modification
- `is_slot_dirty(arena_id, slot_id)`: Check if specific slot is dirty
- `dirty_slot_ids(arena_id)`: Get dirty slots for a specific arena
- `dirty_slot_count()`: Total dirty slots across all arenas

**Flush Configuration** (controls checkpoint behavior):

```rust
struct FlushConfig {
    slot_level_tracking: bool,   // Enable slot-level dirty tracking
    full_arena_threshold: f64,   // Threshold for full vs partial write (default: 0.5)
}
```

**ArenaManager Integration**:
- `ArenaManager::with_config(FlushConfig)`: Create with slot tracking
- `ArenaManager::flush_dirty_slots()`: Incremental checkpoint returning FlushStats
- `ArenaManager::has_slot_tracking()`: Check if slot tracking is enabled
- `ArenaManager::dirty_tracker_stats()`: Get tracking statistics

**Threshold-Based Write Decision**:
During `flush_dirty_slots()`, for each dirty arena:
1. Calculate dirty ratio = dirty_slots / total_slots
2. If ratio ≥ `full_arena_threshold` (default 50%): write entire arena
3. Otherwise: write only header + dirty slots + their directory entries

### 4.5 Three-Phase Commit

The three phases ensure both durability and consistency:

| Phase | Action | Survives Crash? |
|-------|--------|-----------------|
| 1. Log | Write to WAL + fsync | Yes (in WAL) |
| 2. Apply | Update in-memory arena | No (but WAL has it) |
| 3. Checkpoint | Flush dirty arenas | Yes (in data file) |

**Critical insight**: Phase 1 completion is the durability point. Even if we never reach Phase 3, crash recovery replays Phase 1's logged operations.

---

## 5. Worked Example: Insert Operation

Let's trace `insert("hello", 42)` through the entire system:

```
┌─────────────────────────────────────────────────────────────────────┐
│              Step-by-Step: insert("hello", 42)                      │
└─────────────────────────────────────────────────────────────────────┘

STEP 1: API Call
────────────────
Application calls:
    trie.insert("hello", 42);

STEP 2: WAL Logging
───────────────────
WalWriter creates record:
    ┌─────────────────────────────────┐
    │ CRC32: 0x1234ABCD               │
    │ Length: 32                       │
    │ LSN: 1001                        │
    │ Type: INSERT (1)                 │
    │ Payload:                         │
    │   term_len: 5                    │
    │   term: "hello"                  │
    │   has_value: 1                   │
    │   value_len: 8                   │
    │   value: 42 (as i64 bytes)       │
    └─────────────────────────────────┘

    file.write(record_bytes);
    file.fsync();  // ← DURABILITY POINT

STEP 3: Trie Traversal
──────────────────────
Starting from root, traverse for "hello":

    Root (Node16)
        │ 'h'
        ▼
    Node4 (path: "h")
        │ 'e'
        ▼
    Node4 (path: "he")
        │ 'l'
        ▼
    Node4 (path: "hel")
        │ 'l'
        ▼
    Node4 (path: "hell")
        │ 'o'
        ▼
    [Need to create leaf for "hello"]

STEP 4: Node Allocation
───────────────────────
ArenaManager allocates in current arena (id=3):

    Before:
    ┌─────────────────────────────────┐
    │ Arena 3                         │
    │ free_offset: 1024               │
    │ node_count: 15                  │
    │                                 │
    │ [Node0][Node1]...[Node14]       │
    │                    ▲            │
    │                    └── free     │
    └─────────────────────────────────┘

    Serialize new leaf node (let's say 48 bytes)

    After:
    ┌─────────────────────────────────┐
    │ Arena 3                         │
    │ free_offset: 1072               │
    │ node_count: 16                  │
    │                                 │
    │ [Node0][Node1]...[Node14][Leaf] │
    │                           ▲     │
    │                           └ new │
    │                                 │
    │ slot_id: 15                     │
    └─────────────────────────────────┘

STEP 5: Parent Update
─────────────────────
Update parent Node4 to point to new leaf:

    Node4.children['o'] = SwizzledPtr::on_disk(
        block_id: 4,      // arena 3 → block 4
        offset: 15,       // slot_id
        node_type: Bucket
    );

    (Or with relative encoding: delta from parent slot)

STEP 6: Dirty Tracking
──────────────────────
DirtyTracker.mark_dirty(arena_id: 3);

    Before: dirty_arenas = {1, 7}
    After:  dirty_arenas = {1, 3, 7}

STEP 7: Return Success
──────────────────────
Return to application. Operation complete!

    Note: Data file NOT written yet.
    But operation is durable because it's in the WAL.

STEP 8: (Later) Checkpoint
──────────────────────────
When checkpoint triggers:

    for arena_id in dirty_tracker.dirty_arena_ids() {
        arena = arena_manager.get(arena_id);
        disk_manager.write_block(arena_id + 1, arena.as_bytes());
    }
    disk_manager.fsync();

    wal.write_checkpoint_record(current_lsn);
    wal.truncate_before(checkpoint_lsn);

    dirty_tracker.checkpoint_complete();

Now "hello" → 42 is persisted in both WAL (until truncation) and data file.
```

---

## 6. Comparison with Alternatives

| Approach | Read Perf | Write Perf | Durability | Complexity |
|----------|-----------|------------|------------|------------|
| **Our Hybrid (WAL + mmap)** | Excellent | Good | Strong | Medium |
| Direct mmap writes | Excellent | Excellent | None | Low |
| mmap + msync | Excellent | Poor | Weak | Low |
| Copy-on-Write (COW) | Excellent | Good | Strong | High |
| Pure buffered I/O | Good | Good | Strong | Medium |
| LSM-Tree | Good | Excellent | Strong | High |

**Why we chose the hybrid approach**:

- **Direct mmap writes**: No durability guarantees; unacceptable for production
- **mmap + msync**: Forces full page writes; slow; no ordering guarantees
- **Copy-on-Write**: Higher memory overhead; complex GC for old versions
- **Pure buffered I/O**: No zero-copy reads; higher CPU usage
- **LSM-Tree**: Optimized for writes; poor for our read-heavy trie traversals

Our hybrid gets:
- Zero-copy reads via mmap (fast traversal for Levenshtein automata)
- Durable writes via WAL (crash safety)
- Efficient checkpoints via dirty tracking (minimize I/O)

---

## 7. ACID Guarantees

| Property | How Achieved |
|----------|--------------|
| **Atomicity** | WAL records are atomic; either fully written or not |
| **Consistency** | Three-phase commit ensures only complete operations visible |
| **Isolation** | Single-writer design; readers see consistent snapshots |
| **Durability** | WAL fsync before operation returns; checkpoint persists to data file |

**Isolation Details**:

- Writes are serialized (single writer)
- Reads can proceed concurrently (via swizzled pointers)
- Readers see either old or new state, never partial updates
- Epoch-based visibility (future enhancement for MVCC)

---

## 8. Summary

**The Core Answer**: Memory-mapped files aren't directly written for durability. Instead:

1. **Writes go to WAL first** — Logged before any modification (durability)
2. **Then to in-memory arenas** — Fast updates without disk I/O
3. **Finally to data file on checkpoint** — WAL can be truncated after

This hybrid architecture provides:

| Goal | Mechanism |
|------|-----------|
| Fast reads | mmap + swizzled pointers |
| Durable writes | Write-ahead logging |
| Crash recovery | WAL replay |
| Efficient checkpoints | Dirty tracking |
| Space efficiency | Arena allocation + relative encoding |

**Key Invariants**:

1. If the WAL contains a record with LSN > checkpoint_lsn, the operation is pending
2. If the data file has arena with version >= checkpoint, the operation is committed
3. Recovery replays WAL from checkpoint_lsn to reconstruct pending operations

---

## 9. Cross-References

For deeper understanding, see these related documents:

### Theoretical Foundations

| Document | Topics |
|----------|--------|
| [01-foundations.md](../../theory/disk-tries/01-foundations.md) | Trie basics, disk I/O fundamentals |
| [02-b-trie.md](../../theory/disk-tries/02-b-trie.md) | B-trie architecture |
| [03-adaptive-radix-tree.md](../../theory/disk-tries/03-adaptive-radix-tree.md) | ART theory, node types |
| [04-persistent-art.md](../../theory/disk-tries/04-persistent-art.md) | Pointer swizzling deep-dive |
| [05-buffer-management.md](../../theory/disk-tries/05-buffer-management.md) | Page cache, WAL theory, ARIES recovery |
| [06-persistent-artrie-design.md](../../theory/disk-tries/06-persistent-artrie-design.md) | Full design specification |

### Source Code Locations

| Component | File | Key Exports |
|-----------|------|-------------|
| Arena implementation | `src/persistent_artrie/arena.rs` | `ByteNodeArena` |
| Arena manager | `src/persistent_artrie/arena_manager.rs` | `ArenaManager`, `FlushConfig`, `FlushStats` |
| WAL implementation | `src/persistent_artrie/wal.rs` | `WalWriter`, `WalReader` |
| Dirty tracker | `src/persistent_artrie/dirty_tracker.rs` | `DirtyTracker`, `DirtyTrackerStats` |
| Recovery manager | `src/persistent_artrie/recovery.rs` | `RecoveryManager` |
| Swizzled pointers | `src/persistent_artrie/swizzled_ptr.rs` | `SwizzledPtr` |
| Relative encoding | `src/persistent_artrie/relative_encoding.rs` | Relative offset utilities |
| Buffer manager | `src/persistent_artrie/buffer_manager.rs` | `BufferManager` |
| Disk manager | `src/persistent_artrie/disk_manager.rs` | `DiskManager`, `BLOCK_SIZE` |

---

*This document is part of the libdictenstein architecture documentation.*
