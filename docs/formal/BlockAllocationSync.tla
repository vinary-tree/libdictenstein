---------------------------- MODULE BlockAllocationSync ----------------------------
(*
 * TLA+ Model: Block Allocation Synchronization for PersistentVocabARTrie
 *
 * This specification models the lock-ordered synchronization protocol that
 * prevents SIGBUS errors during concurrent block allocation in DiskManager.
 *
 * The key invariant is:
 *   "file_size is updated WHILE holding the mmap write lock, and readers
 *    must acquire the mmap lock FIRST, then check file_size."
 *
 * This ensures:
 *   - If a reader sees updated file_size, it has the lock and sees the new mmap
 *   - If a reader sees old file_size while allocator holds write lock, it will block
 *)

EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS
    NumAllocators,      \* Number of allocator threads
    NumReaders,         \* Number of reader threads
    MaxBlockCount       \* Maximum number of blocks to allocate

VARIABLES
    (* Shared state *)
    block_count,        \* Atomic u32 - in-memory block count (source of truth for CAS)
    file_size,          \* Atomic u64 - current accessible file size
    actual_file_len,    \* Actual file length on disk (set by set_len)
    mmap_len,           \* Current mmap length (updated during remap)
    mmap_lock,          \* RwLock state: {} (free), {-1} (write held), or set of reader IDs (>= 1)

    (* Thread-local state *)
    allocator_pc,       \* Program counter for each allocator
    allocator_block,    \* Block ID being allocated by each allocator
    allocator_new_size, \* New file size calculated by each allocator
    reader_pc,          \* Program counter for each reader
    reader_block,       \* Block ID being read by each reader
    reader_offset       \* End offset needed by each reader

(* Type definitions *)
Allocators == 1..NumAllocators
Readers == (NumAllocators+1)..(NumAllocators+NumReaders)
AllThreads == Allocators \cup Readers

(* Special value for write lock holder *)
WRITER == -1

(* Block size in abstract units *)
BLOCK_SIZE == 1

(* Initial state *)
Init ==
    /\ block_count = 1          \* Block 0 is the header
    /\ file_size = BLOCK_SIZE   \* Initially one block
    /\ actual_file_len = BLOCK_SIZE
    /\ mmap_len = BLOCK_SIZE
    /\ mmap_lock = {}
    (* Allocator state *)
    /\ allocator_pc = [a \in Allocators |-> "idle"]
    /\ allocator_block = [a \in Allocators |-> 0]
    /\ allocator_new_size = [a \in Allocators |-> 0]
    (* Reader state *)
    /\ reader_pc = [r \in Readers |-> "idle"]
    /\ reader_block = [r \in Readers |-> 0]
    /\ reader_offset = [r \in Readers |-> 0]

(* Helper: Is lock free? *)
LockIsFree ==
    mmap_lock = {}

(* Helper: Is lock held by writer? *)
LockIsWriteHeld ==
    mmap_lock = {WRITER}

(* Helper: Is lock held by readers? *)
LockIsReadHeld ==
    /\ mmap_lock /= {}
    /\ WRITER \notin mmap_lock

(* Helper: Can acquire read lock? *)
CanAcquireReadLock ==
    \/ LockIsFree
    \/ LockIsReadHeld  \* Already held by readers, can join

(* Helper: Can acquire write lock? *)
CanAcquireWriteLock ==
    LockIsFree

(* ==================== ALLOCATOR ACTIONS ==================== *)

(* Step 1: Start allocation - CAS on block_count *)
AllocatorStartCAS(a) ==
    /\ allocator_pc[a] = "idle"
    /\ block_count < MaxBlockCount
    /\ allocator_pc' = [allocator_pc EXCEPT ![a] = "cas_won"]
    /\ allocator_block' = [allocator_block EXCEPT ![a] = block_count]
    /\ allocator_new_size' = [allocator_new_size EXCEPT ![a] = (block_count + 1) * BLOCK_SIZE]
    /\ block_count' = block_count + 1
    /\ UNCHANGED <<file_size, actual_file_len, mmap_len, mmap_lock,
                   reader_pc, reader_block, reader_offset>>

(* Step 2: Acquire write lock *)
AllocatorAcquireLock(a) ==
    /\ allocator_pc[a] = "cas_won"
    /\ CanAcquireWriteLock
    /\ mmap_lock' = {WRITER}
    /\ allocator_pc' = [allocator_pc EXCEPT ![a] = "lock_held"]
    /\ UNCHANGED <<block_count, file_size, actual_file_len, mmap_len,
                   allocator_block, allocator_new_size,
                   reader_pc, reader_block, reader_offset>>

(* Step 3: Extend file (set_len) - while holding lock *)
AllocatorExtendFile(a) ==
    /\ allocator_pc[a] = "lock_held"
    /\ LockIsWriteHeld
    (* File extension: actual_file_len becomes max of current and new_size *)
    /\ actual_file_len' = IF allocator_new_size[a] > actual_file_len
                          THEN allocator_new_size[a]
                          ELSE actual_file_len
    /\ allocator_pc' = [allocator_pc EXCEPT ![a] = "file_extended"]
    /\ UNCHANGED <<block_count, file_size, mmap_len, mmap_lock,
                   allocator_block, allocator_new_size,
                   reader_pc, reader_block, reader_offset>>

(* Step 4: Remap mmap - while holding lock *)
AllocatorRemap(a) ==
    /\ allocator_pc[a] = "file_extended"
    /\ LockIsWriteHeld
    (* Remap to actual file length *)
    /\ mmap_len' = actual_file_len
    /\ allocator_pc' = [allocator_pc EXCEPT ![a] = "remapped"]
    /\ UNCHANGED <<block_count, file_size, actual_file_len, mmap_lock,
                   allocator_block, allocator_new_size,
                   reader_pc, reader_block, reader_offset>>

(* Step 5: Update file_size - CRITICAL: while still holding lock *)
AllocatorUpdateFileSize(a) ==
    /\ allocator_pc[a] = "remapped"
    /\ LockIsWriteHeld
    (* Update file_size to mmap_len (monotonic increase via CAS) *)
    /\ file_size' = IF mmap_len > file_size THEN mmap_len ELSE file_size
    /\ allocator_pc' = [allocator_pc EXCEPT ![a] = "file_size_updated"]
    /\ UNCHANGED <<block_count, actual_file_len, mmap_len, mmap_lock,
                   allocator_block, allocator_new_size,
                   reader_pc, reader_block, reader_offset>>

(* Step 6: Release write lock *)
AllocatorReleaseLock(a) ==
    /\ allocator_pc[a] = "file_size_updated"
    /\ LockIsWriteHeld
    /\ mmap_lock' = {}
    /\ allocator_pc' = [allocator_pc EXCEPT ![a] = "done"]
    /\ UNCHANGED <<block_count, file_size, actual_file_len, mmap_len,
                   allocator_block, allocator_new_size,
                   reader_pc, reader_block, reader_offset>>

(* Step 7: Return to idle (for repeated allocations) *)
AllocatorReset(a) ==
    /\ allocator_pc[a] = "done"
    /\ allocator_pc' = [allocator_pc EXCEPT ![a] = "idle"]
    /\ UNCHANGED <<block_count, file_size, actual_file_len, mmap_len, mmap_lock,
                   allocator_block, allocator_new_size,
                   reader_pc, reader_block, reader_offset>>

(* ==================== READER ACTIONS ==================== *)

(* Step 1: Pick a block to read (based on current block_count) *)
ReaderPickBlock(r) ==
    /\ reader_pc[r] = "idle"
    /\ block_count > 1  \* There's at least one data block
    (* Choose any block from 1 to block_count-1 *)
    /\ \E b \in 1..(block_count - 1):
        /\ reader_block' = [reader_block EXCEPT ![r] = b]
        /\ reader_offset' = [reader_offset EXCEPT ![r] = (b + 1) * BLOCK_SIZE]
    /\ reader_pc' = [reader_pc EXCEPT ![r] = "block_picked"]
    /\ UNCHANGED <<block_count, file_size, actual_file_len, mmap_len, mmap_lock,
                   allocator_pc, allocator_block, allocator_new_size>>

(* Step 2: Acquire read lock - CRITICAL: before checking file_size *)
ReaderAcquireLock(r) ==
    /\ reader_pc[r] = "block_picked"
    /\ CanAcquireReadLock
    /\ mmap_lock' = mmap_lock \cup {r}
    /\ reader_pc' = [reader_pc EXCEPT ![r] = "lock_held"]
    /\ UNCHANGED <<block_count, file_size, actual_file_len, mmap_len,
                   allocator_pc, allocator_block, allocator_new_size,
                   reader_block, reader_offset>>

(* Step 3: Check file_size - CRITICAL: after acquiring lock *)
ReaderCheckFileSize(r) ==
    /\ reader_pc[r] = "lock_held"
    /\ r \in mmap_lock  \* We hold the lock
    /\ IF reader_offset[r] <= file_size
       THEN reader_pc' = [reader_pc EXCEPT ![r] = "check_passed"]
       ELSE reader_pc' = [reader_pc EXCEPT ![r] = "check_failed"]
    /\ UNCHANGED <<block_count, file_size, actual_file_len, mmap_len, mmap_lock,
                   allocator_pc, allocator_block, allocator_new_size,
                   reader_block, reader_offset>>

(* Step 4a: Access memory (only if check passed) *)
ReaderAccessMemory(r) ==
    /\ reader_pc[r] = "check_passed"
    /\ r \in mmap_lock
    (* SAFETY CHECK: offset must be within mmap_len *)
    /\ reader_offset[r] <= mmap_len  \* This is the invariant we're verifying!
    /\ reader_pc' = [reader_pc EXCEPT ![r] = "access_done"]
    /\ UNCHANGED <<block_count, file_size, actual_file_len, mmap_len, mmap_lock,
                   allocator_pc, allocator_block, allocator_new_size,
                   reader_block, reader_offset>>

(* Step 4b: Fail gracefully (if check failed) *)
ReaderFail(r) ==
    /\ reader_pc[r] = "check_failed"
    /\ r \in mmap_lock
    /\ reader_pc' = [reader_pc EXCEPT ![r] = "access_done"]
    /\ UNCHANGED <<block_count, file_size, actual_file_len, mmap_len, mmap_lock,
                   allocator_pc, allocator_block, allocator_new_size,
                   reader_block, reader_offset>>

(* Step 5: Release read lock *)
ReaderReleaseLock(r) ==
    /\ reader_pc[r] = "access_done"
    /\ r \in mmap_lock
    /\ mmap_lock' = mmap_lock \ {r}
    /\ reader_pc' = [reader_pc EXCEPT ![r] = "done"]
    /\ UNCHANGED <<block_count, file_size, actual_file_len, mmap_len,
                   allocator_pc, allocator_block, allocator_new_size,
                   reader_block, reader_offset>>

(* Step 6: Return to idle *)
ReaderReset(r) ==
    /\ reader_pc[r] = "done"
    /\ reader_pc' = [reader_pc EXCEPT ![r] = "idle"]
    /\ UNCHANGED <<block_count, file_size, actual_file_len, mmap_len, mmap_lock,
                   allocator_pc, allocator_block, allocator_new_size,
                   reader_block, reader_offset>>

(* ==================== COMBINED NEXT STATE ==================== *)

AllocatorNext(a) ==
    \/ AllocatorStartCAS(a)
    \/ AllocatorAcquireLock(a)
    \/ AllocatorExtendFile(a)
    \/ AllocatorRemap(a)
    \/ AllocatorUpdateFileSize(a)
    \/ AllocatorReleaseLock(a)
    \/ AllocatorReset(a)

ReaderNext(r) ==
    \/ ReaderPickBlock(r)
    \/ ReaderAcquireLock(r)
    \/ ReaderCheckFileSize(r)
    \/ ReaderAccessMemory(r)
    \/ ReaderFail(r)
    \/ ReaderReleaseLock(r)
    \/ ReaderReset(r)

Next ==
    \/ \E a \in Allocators: AllocatorNext(a)
    \/ \E r \in Readers: ReaderNext(r)

(* ==================== INVARIANTS ==================== *)

(*
 * CRITICAL SAFETY PROPERTY: No SIGBUS
 *
 * A reader that passes the file_size check MUST be able to access memory safely.
 * This means: if file_size >= offset, then mmap_len >= offset.
 *
 * The invariant that guarantees this is:
 *   file_size <= mmap_len
 *
 * Because file_size is only updated while holding the write lock AND after
 * remapping, and readers acquire the lock before checking file_size.
 *)
NoSIGBUS ==
    \A r \in Readers:
        (reader_pc[r] = "check_passed" /\ r \in mmap_lock)
        => reader_offset[r] <= mmap_len

(*
 * Memory Ordering Invariant
 *
 * file_size must never exceed mmap_len. This is guaranteed because:
 * 1. file_size is updated AFTER mmap_len (remap happens first)
 * 2. Both updates happen while holding the write lock
 *)
FileSizeInvariant ==
    file_size <= mmap_len

(*
 * Lock Protocol Invariant
 *
 * If a reader holds the lock and has passed the file_size check,
 * the allocator cannot be in the middle of a remap.
 *)
LockProtocol ==
    \A r \in Readers:
        (reader_pc[r] = "check_passed" /\ r \in mmap_lock)
        => \A a \in Allocators:
            allocator_pc[a] /= "remapped"

(*
 * Monotonicity Invariants
 *)
FileSizeMonotonic ==
    file_size >= BLOCK_SIZE

MmapLenMonotonic ==
    mmap_len >= BLOCK_SIZE

BlockCountMonotonic ==
    block_count >= 1

(* Combined invariant *)
TypeInvariant ==
    /\ block_count \in 1..(MaxBlockCount + NumAllocators)
    /\ file_size \in 1..((MaxBlockCount + NumAllocators) * BLOCK_SIZE)
    /\ actual_file_len \in 1..((MaxBlockCount + NumAllocators) * BLOCK_SIZE)
    /\ mmap_len \in 1..((MaxBlockCount + NumAllocators) * BLOCK_SIZE)
    /\ mmap_lock \subseteq (Readers \cup {WRITER})

Invariant ==
    /\ TypeInvariant
    /\ FileSizeInvariant
    /\ NoSIGBUS
    /\ FileSizeMonotonic
    /\ MmapLenMonotonic
    /\ BlockCountMonotonic

(* ==================== LIVENESS PROPERTIES ==================== *)

(*
 * An allocator that starts will eventually complete.
 *)
AllocatorProgress ==
    \A a \in Allocators:
        allocator_pc[a] = "cas_won" ~> allocator_pc[a] = "done"

(*
 * A reader that picks a block will eventually complete (success or failure).
 *)
ReaderProgress ==
    \A r \in Readers:
        reader_pc[r] = "block_picked" ~> reader_pc[r] = "done"

(* Fairness conditions for liveness *)
Fairness ==
    /\ \A a \in Allocators: WF_<<block_count, file_size, actual_file_len, mmap_len, mmap_lock,
                               allocator_pc, allocator_block, allocator_new_size>>(AllocatorNext(a))
    /\ \A r \in Readers: WF_<<mmap_lock, reader_pc, reader_block, reader_offset>>(ReaderNext(r))

Spec == Init /\ [][Next]_<<block_count, file_size, actual_file_len, mmap_len, mmap_lock,
                          allocator_pc, allocator_block, allocator_new_size,
                          reader_pc, reader_block, reader_offset>>

FairSpec == Spec /\ Fairness

=============================================================================
