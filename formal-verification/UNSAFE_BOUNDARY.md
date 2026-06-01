# Unsafe Boundary Verification Notes

Updated: 2026-06-01

This document records the current Rust implementation boundary that is not
covered by Rocq extraction or TLC state exploration. The goal is to keep every
unsafe primitive tied to an explicit coverage class, an executable
correspondence check or bounded model, and any explicit abstraction boundary.

## Boundary Map

| Boundary | Rust Surface | Current Assurance |
|----------|--------------|-------------------|
| CAS-style `Arc` node pointer ownership | `persistent_artrie::nodes::atomic_ptr`, `persistent_artrie_char::nodes::atomic_ptr` | The previous raw atomic `Arc` slot has been replaced with a lock-guarded `Arc` slot, avoiding load-vs-replace refcount races. Correspondence and unit tests check byte-node refcount balance and single-winner CAS visibility. |
| Swizzled disk/memory pointer encoding | `persistent_artrie_core::swizzled_ptr` | `PointerOwnership.tla` now models null/disk/installing/memory/evicting slot states, lazy-load candidates, winner publication, losing-candidate cleanup, and transient publication/removal exclusivity. Correspondence and unit tests check disk-location encoding at max bounds, pure raw disk-pointer roundtrip, swizzle/unswizzle transition, raw extraction only after confirmed in-memory state, strict-provenance memory sentinels that cannot reconstruct pointers from integers, single-winner null initialization, and reclaiming an unpublished losing lazy-load candidate exactly once. |
| Optimistic mutable cell | `persistent_artrie_core::concurrency::OptimisticCell` | The data path is guarded by `RwLock` so the safe API no longer permits Rust-level read/write races; tests check concurrent writer serialization, version parity, and final value. |
| WAL crash/recovery prefix | `persistent_artrie_core::wal`, `persistent_artrie_core::recovery`, `PersistentARTrie::open` | Parser checks are extended with end-to-end reopen tests over real WAL bytes: every complete record-boundary prefix, torn header/payload after a durable prefix, and committed-vs-incomplete transaction replay. |
| Durability and reclamation frontier | `persistent_artrie_core::{group_commit,version_gc,wal/async_writer}` | `DurabilityFrontier.tla` checks prefix-closed synced LSN publication, no early group-commit acknowledgements, checkpoint/recovery within the durable frontier, and durable VersionGc-before-reclaim. Loom tests cover bounded schedule races for the same obligations. |
| Public durability acknowledgement | `PersistentARTrie`, `PersistentARTrieChar`, and `PersistentVocabARTrie` public mutation/sync paths | `PublicDurabilityPolicy.tla` checks full-policy acknowledgement coverage by the synced WAL frontier and weak-policy non-overclaiming. `PublicDurabilityPolicySpec.v` proves the corresponding Immediate/GroupCommit, async sync, checkpoint, and recovery-prefix laws. Rust correspondence tests cover byte/char/vocab public mutation paths, byte transaction commit sync, `sync_async` handles, and `Periodic` non-overclaiming. |
| Public read traversal snapshot | `PersistentARTrie`, `PersistentARTrieChar`, and `PersistentVocabARTrie` public iterator/prefix traversal paths | `PublicReadSnapshotTraversal.tla` checks successful read exactness and fail-closed lazy/disk corruption. `PersistentReadTraversalSpec.v` proves prefix soundness/completeness, no-fabrication, and read-preserves-snapshot laws. Rust correspondence tests cover byte/char/vocab checkpoint/reopen traversal and char lazy traversal corruption without WAL append. |
| Shared persistent public concurrency | `persistent_artrie::SharedARTrie`, `persistent_artrie_char::SharedCharARTrie`, `persistent_vocab_artrie::SharedVocabARTrie` | `SharedPersistentConcurrency.tla` checks the bounded `Arc<RwLock<...>>` public write/read/sync/checkpoint/recovery protocol. `SharedPersistentConcurrencySpec.v` proves the corresponding lock, checkpoint, sync, read, and recovery laws. Rust correspondence tests cover byte/char/vocab shared checkpoint/write/sync/reopen races and caught the byte shared checkpoint lock-publication bug. |
| Raw char/vocab child-pointer ownership | `persistent_artrie_char::types`, `persistent_vocab_artrie::{types,mod,disk_io}`, and unsafe `Send`/`Sync` surfaces | `PointerOwnership.tla` checks bounded raw slot pointer ownership, disk-slot/lazy-load publication, node-map raw reference liveness, borrow exclusivity, unswizzle/drop no-dangling-reference obligations, and no double drop. `VocabPersistenceOwnership.tla` checks stable vocab indexes across checkpoint/reopen and requires eviction to invalidate `node_map` before drop. Correspondence tests cover char and vocab child remove/replacement/deep-clone ownership transfer, unique `get_or_create_child` mutation borrow paths, checkpoint/reopen bijection, direct `node_map`/parent-chain rebuild after reopen, heap-only node-map parent-chain liveness, eviction invalidation, sibling query preservation after leaf eviction, and compile-time `Send`/`Sync` contracts. Miri execution is wired through `RUN_MIRI=1`, with `FORMAL_MIRI_TOOLCHAIN=nightly` support and filesystem isolation disabled for the heap-only persistence targets. |
| mmap/io_uring storage access | `persistent_artrie_core::{disk_manager,io_uring_disk_manager}` and trie constructors | `MmapBlockStorage.tla` checks the bounded allocation/remap/access protocol. `StorageSyscallOutcome.tla` checks the bounded write/sync outcome boundary: only full writes followed by successful syncs advance the durable/reported/recovered prefix. `IoUringFixedBufferOwnership.tla` checks fixed-buffer registration, in-flight fixed I/O, unregister, invalid registration, fallback, and owner-drop ordering. `IoUringSqeCqeLifecycle.tla` checks the bounded submit/complete lifecycle: each submitted request owns one live buffer until exactly one CQE is checked, short/error completions fail closed, and temporary buffers are returned only after checking. Storage correspondence tests cover concurrent mmap allocation uniqueness, sub-block bounds rejection, sync/reopen checksum refresh, `raw_ptr` bounds, failed WAL fsync frontier handling, io_uring range rejection, fixed-buffer registration input validation, and unregister-before-owner-drop behavior when the backend is available. Kernel io_uring internals remain trusted implementation code. |
| BufferManager page leases and cached raw page refs | `persistent_artrie_core::{buffer_manager,traversal_context}` | `BufferPageLease.tla` checks that read leases and write leases never alias, write leases are exclusive, dirty flushes do not run while a write lease is active, cached traversal pointers remain backed by read leases, and cached/dirty/write-leased frames remain resident. Focused Rust unit tests cover read-vs-write lease exclusion, mutable lease exclusion, dirty-flush rejection during an active mutable lease, TraversalContext cached-page pin retention until `clear`, and FIFO cache eviction releasing the prior frame lease. |
| Vocab reverse-index mmap/remap publication | `persistent_vocab_artrie::reverse_index` | `ReverseIndexMmap.tla` checks that the live mapping never exceeds the backing file, the published header capacity never exceeds the live mapping, entries never exceed the header capacity, and publication remains within the file after create/open/grow/remap transitions. Reverse-index tests and the unsafe inventory gate bind the mmap/raw-slice sites to that contract. |
| Raw trie child pointers and byte lock-free CAS paths | `persistent_artrie::{lockfree_cas,nodes/atomic_ptr,nodes/persistent_node}` | `LockFreeARTrieLinearizability.tla` checks the bounded root-CAS/cache/contains/merge publication contract. `LockFreeCounterMergeAtomicity.tla` checks checked counter increments and all-or-nothing value merge into one `BatchIncrement`. Loom tests cover single-winner root CAS, duplicate insert linearization, insert-vs-contains visibility, merge snapshot behavior, checked overflow/merge failure preservation, and child-pointer Arc handoff. |
| Indexed char/vocab lock-free overlays | `persistent_artrie_char::lockfree_cas`, `persistent_vocab_artrie::lockfree_cas` | `LockFreeIndexedOverlay.tla` checks char increment value preservation, merge-prefix behavior, vocab duplicate insert stability, committed-index uniqueness, sparse `next_index` claims, and cache/root/persistent agreement. `LockFreeCounterMergeAtomicity.tla` also covers checked char counter overflow and merge failure preservation for the persistent counter boundary. Loom tests cover the same bounded schedule obligations. |
| Whole-crate unsafe inventory | `formal-verification/UNSAFE_INVENTORY.tsv`, `formal-verification/UNSAFE_CONTRACTS.tsv`, `scripts/verify-unsafe-boundary-inventory.sh` | The verification harness now compares the live `src/**/*.rs` unsafe blocks, unsafe functions, and unsafe impls against the reviewed inventory, checks that every inventory tag has a reviewed contract entry, and rejects contract rows without a valid coverage/status classification. New or changed unsafe sites fail the correspondence script until the pattern, count, contract tag, coverage class, status, and evidence are updated intentionally. |
| Unsafe `Send`/`Sync` impls outside the persistent ART core | SCDAWG handles, vocab variants, test mock nodes | The explicit unsafe impl surface is inventoried. Persistent ARTrie/vocab contracts are type-checked in `persistent_artrie_formal_correspondence`; SCDAWG byte/char handle contracts are type-checked and exercised under concurrent read traversal in `unsafe_boundary_contracts`. |

## Safety Contract Matrix

| Contract | Enforced By |
|----------|-------------|
| Disk/null slots and raw in-memory slot pointers are mutually exclusive, and transient installing/evicting slots remain unborrowed until they publish or clear. | `PointerOwnership.tla` `SlotDiskAndRawStatesAreDisjoint` and `TransientSlotsHaveExclusiveOwner` invariants plus strict-provenance swizzle/unswizzle correspondence tests. |
| Lazy-load race losers keep private ownership and are dropped without publication. | `PointerOwnership.tla` `LoadingPointersAreThreadLocal` / `NoLoadCandidateAliasing` invariants plus `swizzled_pointer_losing_lazy_load_candidate_can_be_reclaimed_once`. |
| Raw child replacement returns the old `Box` and does not alias the new child. | Char and vocab remove/replace/deep-clone correspondence tests, Miri-gated in the harness. |
| Storage write/sync outcomes advance durability only after full write plus successful sync. | `StorageSyscallOutcome.tla`, failed fsync frontier correspondence, io_uring completion helper tests, and cached-write dirty re-marking on failed single/batched/fixed-buffer writes. |
| Public full-policy acknowledgements are not returned until the appended WAL LSN is covered by the synced frontier. | `PublicDurabilityPolicy.tla`, `PublicDurabilityPolicySpec.v`, and `tests/persistent_public_durability_policy_correspondence.rs` across byte/char/vocab public mutation and sync paths. |
| Public read traversals return exact visible snapshots or fail closed on lazy/disk corruption. | `PublicReadSnapshotTraversal.tla`, `PersistentReadTraversalSpec.v`, and `tests/persistent_read_snapshot_correspondence.rs` across byte/char/vocab iteration, prefix iteration, and checkpoint/reopen paths. |
| io_uring fixed buffers are non-null, block-sized, aligned, used only while registered, and unregistered before owner drop. | `IoUringFixedBufferOwnership.tla`, `IoUringDiskManager::register_buffer_pool` validation, `BufferManager` fixed-capability gating, `io_uring_*registration*` storage correspondence tests, and the fixed-capable `BufferManager` storage double. |
| io_uring submitted requests keep ownership of one buffer until completion checking, and short/error CQEs fail closed. | `IoUringSqeCqeLifecycle.tla` plus `IoUringDiskManager` completion-count, negative-result, short-read/write, and temporary-buffer return checks. |
| Backends that accept the default no-op registration do not accidentally enable fixed I/O. | `BufferManager` requires both registration success and `supports_fixed_buffers()`, with a regression in `tests/unsafe_boundary_contracts.rs`. |
| Cached page references are backed by active read leases, mutable page references exclude all other leases, and dirty flushes do not read through an active mutable lease. | `BufferPageLease.tla` invariants `CachedPagesPinned`, `NoReadWriteAlias`, `CachedFramesResident`, `FlushesExcludeWriteLease`, and focused `BufferManager`/`TraversalContext` lease tests. |
| Reverse-index header and entry counts are published only inside the live mmap/file capacity. | `ReverseIndexMmap.tla` invariants `MappedWithinFile`, `HeaderWithinMap`, `EntriesWithinHeader`, and `PublishedHeaderWithinFile`, plus reverse-index tests. |
| Every unsafe source pattern has a reviewed contract tag and coverage class. | `scripts/verify-unsafe-boundary-inventory.sh` compares `src/**/*.rs` against `formal-verification/UNSAFE_INVENTORY.tsv`, checks every tag against `formal-verification/UNSAFE_CONTRACTS.tsv`, validates coverage tokens (`rocq`, `tla`, `loom`, `miri`, `correspondence`, `compile-time`, `unit`, or `trusted-boundary`), and rejects persistence unsafe contracts that are not `covered` or `miri-wired`. |

## Current Claim

The repository now checks representative unsafe-boundary behavior where it
intersects the formal ARTrie model:

- pointer encodings preserve the disk/memory state partition, including a pure
  raw disk-pointer roundtrip that does not depend on mmap/io_uring, and losing
  lazy-load candidates stay unpublished until reclaimed;
- unchecked swizzled raw extraction is exercised only after the safe API
  confirms a published in-memory pointer, memory raw-state sentinels cannot
  fabricate pointer provenance after serialization, and unswizzle returns to a
  disk-only state after clearing the runtime pointer slot;
- node pointer replacement stays in safe `Arc` ownership and exposes a single
  visible CAS-style winner;
- atomic child initialization has a single visible winner;
- optimistic writes are serialized at the Rust memory-safety level;
- reopening a persistent trie preserves the exact durable reference-map prefix
  at every complete WAL record boundary;
- torn trailing WAL headers and payloads do not make recovery apply partial
  records;
- committed WAL transactions replay atomically while incomplete transactions
  are ignored.
- mmap allocation publishes unique block IDs and does not permit successful
  reads/writes past the published mapped file size in the bounded model;
- Rust `BlockStorage` tests reject cross-block sub-block I/O, refresh mmap
  header checksums on sync/reopen, and reject out-of-block raw pointer offsets.
- byte lock-free insert linearizes at successful root publication, cache entries
  are only published after the root contains the key, contains never reports a
  visible key as absent, merge persists only a cache snapshot prefix in the
  bounded model, and checked value-counter merges reject overflow before
  overlay/persistent/WAL mutation while successful merges publish one
  `BatchIncrement`.
- char lock-free increments add each successful delta exactly once, create at
  most one visible leaf for a raced key, merge only persists a value visible
  at the snapshot point in the bounded model, and checked overflow or merge
  failure preserves overlay, persistent state, and WAL state;
- vocab lock-free inserts publish one stable index per committed term, preserve
  uniqueness across distinct terms, and allow sparse `next_index` values when a
  duplicate race wastes a claimed index.
- synced LSNs are prefix-closed, group-commit waiters are acknowledged only
  after their LSN is inside the durable frontier, checkpoints cannot publish
  beyond that frontier, recovery only applies durable records, and VersionGc
  reclamation requires both no active readers and a durable GC decision.
- raw char and vocabulary child slots transfer ownership through
  remove/replace/deep clone without aliasing the returned `Box`; unique
  `get_or_create_child` mutation paths reuse the same child borrow without
  inventing side-table aliases, and the explicit unsafe `Send`/`Sync` surface
  remains type-checked by the correspondence target.
- the whole-crate unsafe inventory is checked for drift before the
  correspondence harness runs; the SCDAWG byte and char handle contracts are
  exercised by concurrent read traversal tests outside the persistent ARTrie
  feature gate, and every unsafe inventory tag resolves to a reviewed
  `UNSAFE_CONTRACTS.tsv` entry with machine-checked coverage/status metadata.
- vocabulary checkpoint/reopen preserves stable forward and reverse indexes,
  duplicate inserts keep the original index, checkpoint/reopen rebuilds
  `node_map` and parent-chain entries for live nodes, shared-prefix inserts
  reuse the existing child `NodeRef` instead of allocating a duplicate side-table
  entry, and leaf eviction removes stale `node_map` raw entries before dropping
  the in-memory node while sibling queries remain on live nodes.
- io_uring fixed-buffer registration rejects invalid buffers, fixed I/O is
  enabled only after backend capability confirmation, and the fixed-buffer
  capability is cleared before the registered buffer owner is dropped.
- `BufferManager` write-guard mutation over aligned blocks is covered by a
  fixed-capable storage double that observes batched fixed flush and
  unregister-before-owner-drop behavior.
- `BufferManager` page references are leased: shared references hold read
  leases, mutable references require the exclusive write lease, cached
  TraversalContext raw pointers keep their frame pinned until cache eviction,
  `clear`, or drop, dirty flushes fail closed while a mutable lease is active,
  and leased/cached frames cannot be evicted.
- reverse-index mmap/remap publication keeps the mapped capacity inside the
  backing file, publishes header capacity only after the mapping exists, and
  keeps entry counts within the published header capacity.
- io_uring submitted requests keep a live buffer until one CQE is checked,
  negative and short completions fail closed, and temporary aligned buffers are
  not returned to the pool before completion checking.
- failed WAL segment fsync attempts do not advance `global_synced_lsn` or make
  `wait_for_lsn_timeout` report the target LSN as durable, and failed
  io_uring single, batched, or fixed-buffer writes re-mark updated cached
  blocks dirty so a later sync can retry.

This is not a RustBelt, Iris, Kani, or certified-compilation result. The
current harness runs strict-provenance Miri-compatible targets for raw child
ownership, unique child mutation, swizzled raw extraction, `SwizzledPtr` unit
state transitions, heap-only vocab node-map/eviction ownership, public
read-traversal lazy failure closure, and buffer-manager fixed-buffer lifetime,
and the unsafe inventory gate now rejects
coverage metadata drift. `SwizzledPtr` no longer reconstructs in-memory
pointers from integers. Kernel io_uring/syscall internals and certified
Rust/LLVM compilation are explicit abstraction boundaries below the modeled
syscall-outcome and executable-correspondence claims.

## Maintenance Invariants

1. Keep `RUN_MIRI=1 FORMAL_MIRI_TOOLCHAIN=nightly
   scripts/verify-formal-correspondence.sh` green and small enough for CI.
2. Keep the no-TLC correspondence, Miri-gated, io_uring-gated, and
   scheduled/manual TLC CI jobs green.
3. Keep the unsafe inventory gate mandatory in local and CI verification so new
   unsafe blocks cannot bypass review or coverage classification.
4. Keep `BufferPageLease.tla`, `ReverseIndexMmap.tla`,
   `StorageSyscallOutcome.tla`, and the corresponding Rust fsync/CQE
   checks green; deepen the trusted kernel/syscall boundary only if the project
   needs claims below the syscall outcome abstraction.
5. Keep strict-provenance `SwizzledPtr` Miri coverage mandatory and extend the
   same provenance-preserving pattern to any new serialized pointer state.
6. Treat certified compilation as an explicit abstraction boundary unless the
   project adopts a verified Rust subset or extracted reference implementation
   as production code.
