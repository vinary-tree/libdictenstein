# Memory-Mapped I/O Architecture for Persistent ARTrie

This document explains how the persistent ARTrie implementation uses memory-mapped
files (mmap) for disk I/O, including reading, writing, synchronization with on-disk
state, and file growth mechanisms.

## Table of Contents

1. [Introduction](#1-introduction)
2. [File Layout and Structure](#2-file-layout-and-structure)
3. [Memory-Mapped I/O Fundamentals](#3-memory-mapped-io-fundamentals)
4. [Read Operations](#4-read-operations)
5. [Write Operations](#5-write-operations)
6. [File Growth and Remapping](#6-file-growth-and-remapping)
7. [Synchronization and Durability](#7-synchronization-and-durability)
8. [SwizzledPtr: Bridging Memory and Disk](#8-swizzledptr-bridging-memory-and-disk)

---

## 1. Introduction

The persistent ARTrie uses memory-mapped files to provide efficient, transparent
access to on-disk data structures. This approach offers several advantages:

- **Zero-copy reads**: Data is accessed directly from the kernel page cache
- **Unified addressing**: Memory pointers and disk offsets share a common model
- **Kernel-managed caching**: The operating system handles page replacement
- **Lazy loading**: Pages are loaded on demand via page faults

### Key Components

| Component | File | Responsibility |
|-----------|------|----------------|
| `DiskManager` | `disk_manager.rs` | Low-level mmap management, block allocation |
| `BufferManager` | `buffer_manager.rs` | Page cache with Clock/LRU eviction |
| `ArenaManager` | `arena_manager.rs` | Node storage in 256KB arenas |
| `SwizzledPtr` | `swizzled_ptr.rs` | Transparent memory/disk pointer abstraction |

---

## 2. File Layout and Structure

### Block Size Rationale

The persistent ARTrie uses **256KB blocks** (262,144 bytes), chosen for:

- Optimal NVMe I/O granularity (typical 4KB-256KB sweet spot)
- Reduced metadata overhead compared to smaller pages
- Good cache locality for sequential access
- Alignment with memory-mapped page boundaries

### File Organization

```
+------------------------------------------------------------------+
|                         File Layout                               |
+------------------------------------------------------------------+
| Block 0: File Header (256KB)                                      |
|   +----------------------------------------------------------+   |
|   | Offset 0-7:   Magic number (0x5041_5254_0001_0000)        |   |
|   | Offset 8-11:  Version (u32)                               |   |
|   | Offset 12-15: Flags (u32, reserved)                       |   |
|   | Offset 16-23: Root pointer (AtomicU64, swizzled format)   |   |
|   | Offset 24-27: Block count (AtomicU32)                     |   |
|   | Offset 28-31: Padding                                     |   |
|   | Offset 32-39: Free list head (AtomicU64)                  |   |
|   | Offset 40-47: Entry count (AtomicU64)                     |   |
|   | Offset 48-55: Checksum (FNV-1a, u64)                      |   |
|   | Offset 56-63: Reserved                                    |   |
|   | Offset 64+:   Reserved for future use                     |   |
|   +----------------------------------------------------------+   |
+------------------------------------------------------------------+
| Block 1: Data Block (256KB) - Arena 0                             |
+------------------------------------------------------------------+
| Block 2: Data Block (256KB) - Arena 1                             |
+------------------------------------------------------------------+
| Block 3: Data Block (256KB) - Arena 2                             |
+------------------------------------------------------------------+
| ...                                                               |
+------------------------------------------------------------------+
```

### Header Structure (64 bytes, cacheline-aligned)

The `FileHeader` structure occupies the first 64 bytes of Block 0:

```rust
#[repr(C, align(64))]
pub struct FileHeader {
    pub magic: u64,                    // File identification
    pub version: u32,                  // Format version
    pub flags: u32,                    // Reserved
    pub root_ptr: AtomicU64,           // Root node pointer
    pub block_count: AtomicU32,        // Total allocated blocks
    _pad1: u32,                        // Alignment padding
    pub free_list_head: AtomicU64,     // Head of free block list
    pub entry_count: AtomicU64,        // Total dictionary entries
    pub checksum: u64,                 // FNV-1a checksum
}
```

---

## 3. Memory-Mapped I/O Fundamentals

### How mmap Creates a Virtual Memory View

When a file is memory-mapped, the kernel creates a mapping between virtual memory
addresses and file offsets. The actual data transfer happens lazily via page faults:

```
                          Virtual Address Space
                    +-----------------------------+
                    |                             |
                    |     Process Memory          |
                    |                             |
                    +-----------------------------+
                    |                             |
    mmap region --> |    Mapped File Region       | <-- 0x7f0000000000
                    |    (256KB * N blocks)       |
                    |                             |
                    +-----------------------------+
                    |                             |
                    |     Stack, Heap, etc.       |
                    |                             |
                    +-----------------------------+
                              |
                              | Page Fault on Access
                              v
                    +-----------------------------+
                    |     Kernel Page Cache       |
                    |   (Shared with other        |
                    |    processes)               |
                    +-----------------------------+
                              |
                              | Disk I/O (if not cached)
                              v
                    +-----------------------------+
                    |        Disk File            |
                    |   (data.part, 256KB blocks) |
                    +-----------------------------+
```

### Why Reads Always Reflect On-Disk State

Memory-mapped reads go through the kernel page cache, which is the authoritative
source for file contents:

1. **First access**: Page fault triggers disk read into page cache
2. **Subsequent accesses**: Read directly from page cache (no syscall)
3. **External modifications**: Kernel invalidates cached pages when file changes
4. **Coherence**: All processes see the same view of the file

This means reads via mmap are always consistent with the on-disk state (modulo
pages that have been modified but not yet flushed).

---

## 4. Read Operations

### Lock-Ordered Synchronization Protocol

To prevent SIGBUS errors during concurrent allocation and access, reads follow
a strict lock-ordering protocol:

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Read Operation Flow                             │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  1. Quick-reject: Check block_id < block_count (atomic)             │
│                           │                                         │
│                           │ Fail fast if block doesn't exist        │
│                           v                                         │
│  2. Acquire mmap read lock ──────────────────────┐                  │
│                           │                      │                  │
│                           │ BLOCKS if allocator  │                  │
│                           │ holds write lock     │                  │
│                           v                      │                  │
│  3. Check file_size >= end_offset (atomic)       │                  │
│                           │                      │                  │
│       ┌───────────────────┴───────────────────┐  │                  │
│       │                                       │  │                  │
│       v                                       v  │                  │
│   file_size OK                          file_size too small         │
│       │                                       │  │                  │
│       v                                       v  │                  │
│  4. Access memory safely              Return error                  │
│       │                                       │  │                  │
│       v                                       v  v                  │
│  5. Release lock ────────────────────────────────┘                  │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Thread Safety Guarantees

The lock ordering ensures:

- **If reader sees updated `file_size`**: It has the lock, so allocator has finished
  remapping. The new mmap is in place and safe to access.

- **If reader sees old `file_size` while allocator holds write lock**: Reader blocks
  until allocator releases the lock, at which point both the new mmap and new
  `file_size` are visible.

### Code Reference

```rust
// From disk_manager.rs:928-971
pub fn read_block(&self, block_id: u32, buffer: &mut [u8; BLOCK_SIZE]) -> Result<()> {
    // Step 1: Quick-reject against block_count (source of truth)
    let current_block_count = self.block_count.load(Ordering::Acquire);
    if block_id >= current_block_count {
        return Err(PersistentARTrieError::InvalidBlockId { ... });
    }

    // Step 2: Acquire mmap lock FIRST
    let mmap = mmap_guard.read();

    // Step 3: THEN check file_size
    let current_file_size = self.file_size.load(Ordering::Acquire);
    if end_offset as u64 > current_file_size {
        return Err(PersistentARTrieError::InvalidBlockId { ... });
    }

    // Step 4: Safe to access
    buffer.copy_from_slice(&mmap[offset..end_offset]);
    Ok(())
}
```

---

## 5. Write Operations

### Write Path Through mmap

Writes to memory-mapped regions modify the kernel page cache, not the disk directly:

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Write Operation Flow                            │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  Application writes to mmap region                                  │
│                           │                                         │
│                           v                                         │
│  ┌─────────────────────────────────────────┐                        │
│  │         Kernel Page Cache               │                        │
│  │  ┌─────────────────────────────────┐    │                        │
│  │  │  Page marked DIRTY by kernel    │    │                        │
│  │  └─────────────────────────────────┘    │                        │
│  └─────────────────────────────────────────┘                        │
│                           │                                         │
│                           │ (Asynchronous, kernel-controlled)       │
│                           │ OR                                      │
│                           │ mmap.flush() forces write               │
│                           v                                         │
│  ┌─────────────────────────────────────────┐                        │
│  │              Disk File                  │                        │
│  │     (not guaranteed durable until       │                        │
│  │      fsync/sync_all is called)          │                        │
│  └─────────────────────────────────────────┘                        │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Dirty Page Tracking

The system tracks dirty state at multiple levels:

1. **Kernel-level**: Pages modified via mmap are marked dirty by the kernel
2. **BufferManager-level**: `FrameMetadata.dirty` flag tracks modified buffer frames
3. **ArenaManager-level**: `ByteNodeArena.is_dirty()` tracks modified arenas
4. **DirtyTracker**: Optional slot-level tracking for incremental checkpoints

### Write Operation Steps

```rust
// From disk_manager.rs:987-1027
pub fn write_block(&self, block_id: u32, buffer: &[u8; BLOCK_SIZE]) -> Result<()> {
    // Step 1: Quick-reject against block_count
    // Step 2: Acquire mmap write lock
    // Step 3: Check file_size
    // Step 4: Copy data to mmap region
    mmap[offset..end_offset].copy_from_slice(buffer);
    Ok(())
}
```

---

## 6. File Growth and Remapping

### Lock-Free CAS-Based Block Allocation

Block allocation uses a compare-and-swap (CAS) loop on the in-memory `block_count`
atomic to ensure concurrent allocations never receive duplicate block IDs:

```
┌─────────────────────────────────────────────────────────────────────┐
│              Block Allocation Sequence (Allocator Thread)           │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  1. CAS on block_count ──────────────────┐                          │
│          │                               │                          │
│          │ Only ONE thread wins          │ Losers retry             │
│          v                               │                          │
│  2. Acquire mmap WRITE lock              │                          │
│          │                               │                          │
│          │ (Readers blocked here)        │                          │
│          v                               │                          │
│  3. file.set_len(new_size)               │                          │
│          │                               │                          │
│          │ Extends file on disk          │                          │
│          v                               │                          │
│  4. pwrite() to materialize sparse region│                          │
│          │                               │                          │
│          │ Prevents SIGBUS on some FS    │                          │
│          v                               │                          │
│  5. Create new mmap                      │                          │
│          │                               │                          │
│          │ Replaces old mapping          │                          │
│          v                               │                          │
│  6. Memory barrier (SeqCst fence)        │                          │
│          │                               │                          │
│          │ Ensures mmap visible before   │                          │
│          │ file_size update              │                          │
│          v                               │                          │
│  7. file_size.store(new_size) ───────────┘                          │
│          │                                                          │
│          │ CAS ensures monotonic increase                           │
│          v                                                          │
│  8. Release write lock                                              │
│          │                                                          │
│          │ Readers can now proceed                                  │
│          v                                                          │
│  Return allocated block_id                                          │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Mmap Replacement Strategy (Not Double-Mapping)

The system creates a **new** mmap covering the extended file rather than attempting
to extend the existing mapping in place:

```rust
// From disk_manager.rs:718-727
if remap_size as usize > mmap.len() {
    let new_mmap = unsafe {
        MmapOptions::new()
            .len(remap_size as usize)
            .map_mut(&self.file)
            .map_err(|e| PersistentARTrieError::MmapError { ... })?
    };
    *mmap = new_mmap;  // Replace old mmap
}
```

This approach:
- Avoids the complexity of `mremap()` (Linux-specific)
- Ensures a clean, consistent mapping
- Old mapping is unmapped when replaced

### SIGBUS Prevention

SIGBUS errors occur when accessing memory beyond the mapped file size. The system
prevents this via:

1. **Lock ordering**: Acquire lock before checking `file_size`
2. **Sparse file materialization**: `pwrite()` ensures pages exist on some filesystems
3. **Size validation**: Validate `file_size` after acquiring lock

### Formal Verification (TLA+)

The synchronization protocol has been formally verified using TLA+ model checking.
The specification is located at `docs/formal/BlockAllocationSync.tla`.

**Key invariant verified**:
```
FileSizeInvariant == file_size <= mmap_len
```

This ensures any reader that passes the `file_size` check can safely access memory.

---

## 7. Synchronization and Durability

### Two-Phase Sync Protocol

To ensure durability, the system uses a two-phase sync:

```rust
// From disk_manager.rs:1134-1150
pub fn sync(&self) -> Result<()> {
    // Phase 1: Flush mmap dirty pages to kernel buffer
    if let Some(mmap_guard) = &self.mmap {
        let mmap = mmap_guard.read();
        mmap.flush()?;  // msync(MS_SYNC)
    }

    // Phase 2: Sync kernel buffer to disk
    self.file.sync_all()?;  // fsync()

    Ok(())
}
```

**Phase 1: `mmap.flush()`**
- Triggers `msync(MS_SYNC)` system call
- Writes dirty pages from page cache to filesystem buffer

**Phase 2: `file.sync_all()`**
- Triggers `fsync()` system call
- Ensures filesystem buffer is written to physical disk
- Includes file metadata (size, modification time)

### Checkpoint Triggers and WAL Integration

Checkpoints integrate with the Write-Ahead Log (WAL) for crash recovery:

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Checkpoint Process                              │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  1. Write CHECKPOINT_BEGIN to WAL                                   │
│                           │                                         │
│                           v                                         │
│  2. Flush dirty arenas to disk (ArenaManager::flush())              │
│                           │                                         │
│                           v                                         │
│  3. Update file header (root_ptr, entry_count, checksum)            │
│                           │                                         │
│                           v                                         │
│  4. disk_manager.sync() (two-phase)                                 │
│                           │                                         │
│                           v                                         │
│  5. Write CHECKPOINT_END to WAL                                     │
│                           │                                         │
│                           v                                         │
│  6. Truncate WAL (remove committed records before checkpoint)       │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Crash Recovery Semantics

On restart, the recovery process:

1. **Analysis Phase**: Scan WAL from last checkpoint, identify committed transactions
2. **Redo Phase**: Replay committed operations to rebuild state
3. **Cleanup Phase**: Truncate WAL after recovery

The `block_count` is recovered from `file_size / BLOCK_SIZE`, making it resilient
to crashes during allocation:

```rust
// From disk_manager.rs:417-418
// Recover block count from file size (source of truth, handles crashes)
let block_count = (file_size / BLOCK_SIZE as u64) as u32;
```

---

## 8. SwizzledPtr: Bridging Memory and Disk

### Bit Layout

`SwizzledPtr` uses a single 64-bit value to represent either a memory pointer
or a disk reference:

```
┌─────────────────────────────────────────────────────────────────────┐
│                    SwizzledPtr Bit Layout (64 bits)                 │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  When MSB = 1 (Swizzled / In-Memory):                               │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │ Bit 63 │ Bits 62-0                                           │   │
│  │   1    │ Memory address (63 bits)                            │   │
│  └──────────────────────────────────────────────────────────────┘   │
│                                                                     │
│  When MSB = 0 (Unswizzled / On-Disk):                               │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │ Bit 63 │ Bits 62-40    │ Bits 39-18   │ Bits 17-0            │   │
│  │   0    │ Block ID      │ Offset       │ Flags (NodeType)     │   │
│  │        │ (23 bits)     │ (22 bits)    │ (18 bits)            │   │
│  │        │ 8M blocks max │ 4MB offset   │                      │   │
│  └──────────────────────────────────────────────────────────────┘   │
│                                                                     │
│  Null pointer: All bits = 0                                         │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Capacity Limits

| Field | Bits | Maximum Value | Practical Limit |
|-------|------|---------------|-----------------|
| Block ID | 23 | 8,388,607 | ~2TB at 256KB blocks |
| Offset | 22 | 4,194,303 | 4MB within block |
| Flags | 18 | 262,143 | Node type + future use |

### Swizzle/Unswizzle Operations

**Swizzling** converts a disk reference to a memory pointer after loading:

```rust
// Atomic CAS ensures thread-safe swizzling
pub fn swizzle<T>(&self, ptr: *const T) -> Result<(), SwizzleError> {
    let old = self.0.load(Ordering::Acquire);

    // Already swizzled or null?
    if old & SWIZZLE_FLAG != 0 || old == 0 {
        return Err(SwizzleError::AlreadySwizzled);
    }

    let new = (ptr as u64) | SWIZZLE_FLAG;

    self.0.compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| SwizzleError::RaceCondition)
}
```

**Unswizzling** converts back to a disk reference during eviction:

```rust
pub fn unswizzle<T>(
    &self,
    block_id: u32,
    offset: u32,
    node_type: NodeType,
) -> Result<*const T, SwizzleError> {
    let old = self.0.load(Ordering::Acquire);

    // Not swizzled?
    if old & SWIZZLE_FLAG == 0 {
        return Err(SwizzleError::AlreadyUnswizzled);
    }

    let new = ((block_id as u64) << BLOCK_ID_SHIFT)
            | ((offset as u64) << OFFSET_SHIFT)
            | (node_type as u64);

    self.0.compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
        .map(|v| (v & PTR_MASK) as *const T)
        .map_err(|_| SwizzleError::RaceCondition)
}
```

### Lazy Loading Pattern

The typical access pattern for lazy loading:

```
┌─────────────────────────────────────────────────────────────────────┐
│                     Lazy Loading Flow                               │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  Access SwizzledPtr                                                 │
│          │                                                          │
│          v                                                          │
│  ┌───────────────────┐                                              │
│  │ is_swizzled()?    │                                              │
│  └───────────────────┘                                              │
│          │                                                          │
│     ┌────┴────┐                                                     │
│     │         │                                                     │
│     v         v                                                     │
│   YES        NO                                                     │
│     │         │                                                     │
│     │         v                                                     │
│     │    ┌────────────────────┐                                     │
│     │    │ disk_location()    │                                     │
│     │    │ -> block_id,offset │                                     │
│     │    └────────────────────┘                                     │
│     │         │                                                     │
│     │         v                                                     │
│     │    ┌────────────────────┐                                     │
│     │    │ Load from disk     │                                     │
│     │    │ via BufferManager  │                                     │
│     │    └────────────────────┘                                     │
│     │         │                                                     │
│     │         v                                                     │
│     │    ┌────────────────────┐                                     │
│     │    │ swizzle(ptr)       │                                     │
│     │    │ (atomic CAS)       │                                     │
│     │    └────────────────────┘                                     │
│     │         │                                                     │
│     v         v                                                     │
│  ┌─────────────────────┐                                            │
│  │ as_ptr_unchecked()  │                                            │
│  │ -> *const T         │                                            │
│  └─────────────────────┘                                            │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Arena Slot Mapping

`SwizzledPtr` integrates with the arena system via `ArenaSlot`:

- Arena N is stored in Block N+1 (Block 0 is the header)
- The offset field stores the slot ID within the arena

```rust
// Convert SwizzledPtr to ArenaSlot
pub fn as_arena_slot(&self) -> Option<ArenaSlot> {
    let loc = self.disk_location()?;
    let arena_id = loc.block_id.checked_sub(1)?;  // Block 1 = Arena 0
    Some(ArenaSlot::new(arena_id, loc.offset))
}

// Create SwizzledPtr from ArenaSlot
pub fn from_arena_slot(slot: ArenaSlot, node_type: NodeType) -> Self {
    let block_id = slot.arena_id.saturating_add(1);  // Arena 0 = Block 1
    Self::on_disk(block_id, slot.slot_id, node_type)
}
```

---

## Summary

The persistent ARTrie's mmap-based I/O architecture provides:

1. **Efficient access**: Zero-copy reads via kernel page cache
2. **Thread safety**: Lock-ordered synchronization prevents SIGBUS
3. **Durability**: Two-phase sync ensures crash consistency
4. **Lazy loading**: SwizzledPtr enables transparent memory/disk bridging
5. **Formal verification**: TLA+ model checking validates synchronization protocol

For implementation details, see:
- `src/persistent_artrie/disk_manager.rs` - Core mmap management
- `src/persistent_artrie/buffer_manager.rs` - Page cache implementation
- `src/persistent_artrie/swizzled_ptr.rs` - Pointer abstraction
- `docs/formal/BlockAllocationSync.tla` - Formal specification
