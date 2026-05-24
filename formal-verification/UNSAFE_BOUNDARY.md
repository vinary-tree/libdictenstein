# Unsafe Boundary Verification Notes

Updated: 2026-05-24

This document records the current Rust implementation boundary that is not
covered by Rocq extraction or TLC state exploration. The goal is to keep every
unsafe primitive tied to an executable correspondence check or an explicit
remaining proof obligation.

## Boundary Map

| Boundary | Rust Surface | Current Assurance |
|----------|--------------|-------------------|
| CAS-style `Arc` node pointer ownership | `persistent_artrie::nodes::atomic_ptr`, `persistent_artrie_char::nodes::atomic_ptr` | The previous raw atomic `Arc` slot has been replaced with a lock-guarded `Arc` slot, avoiding load-vs-replace refcount races. Correspondence and unit tests check byte-node refcount balance and single-winner CAS visibility. |
| Swizzled disk/memory pointer encoding | `persistent_artrie_core::swizzled_ptr` | `PointerOwnership.tla` now models disk slots, lazy-load candidates, winner publication, and losing-candidate cleanup. Correspondence tests check disk-location encoding at max bounds, pure raw disk-pointer roundtrip, swizzle/unswizzle transition, raw extraction only after confirmed in-memory state, single-winner null initialization, and reclaiming an unpublished losing lazy-load candidate exactly once. |
| Optimistic mutable cell | `persistent_artrie_core::concurrency::OptimisticCell` | The data path is guarded by `RwLock` so the safe API no longer permits Rust-level read/write races; tests check concurrent writer serialization, version parity, and final value. |
| WAL crash/recovery prefix | `persistent_artrie_core::wal`, `persistent_artrie_core::recovery`, `PersistentARTrie::open` | Parser checks are extended with end-to-end reopen tests over real WAL bytes: every complete record-boundary prefix, torn header/payload after a durable prefix, and committed-vs-incomplete transaction replay. |
| Durability and reclamation frontier | `persistent_artrie_core::{group_commit,version_gc,wal/async_writer}` | `DurabilityFrontier.tla` checks prefix-closed synced LSN publication, no early group-commit acknowledgements, checkpoint/recovery within the durable frontier, and durable VersionGc-before-reclaim. Loom tests cover bounded schedule races for the same obligations. |
| Raw char/vocab child-pointer ownership | `persistent_artrie_char::types`, `persistent_vocab_artrie::{types,mod,disk_io}`, and unsafe `Send`/`Sync` surfaces | `PointerOwnership.tla` checks bounded raw slot pointer ownership, disk-slot/lazy-load publication, node-map raw reference liveness, borrow exclusivity, unswizzle/drop no-dangling-reference obligations, and no double drop. `VocabPersistenceOwnership.tla` checks stable vocab indexes across checkpoint/reopen and requires eviction to invalidate `node_map` before drop. Correspondence tests cover char and vocab child remove/replacement/deep-clone ownership transfer, checkpoint/reopen bijection, direct `node_map`/parent-chain rebuild after reopen, eviction invalidation, sibling query preservation after leaf eviction, and compile-time `Send`/`Sync` contracts. Optional Miri execution is wired through `RUN_MIRI=1`. |
| mmap/io_uring storage access | `persistent_artrie_core::{disk_manager,io_uring_disk_manager}` and trie constructors | `MmapBlockStorage.tla` checks the bounded allocation/remap/access protocol. `StorageSyscallOutcome.tla` checks the bounded write/sync outcome boundary: only full writes followed by successful syncs advance the durable/reported/recovered prefix. `IoUringFixedBufferOwnership.tla` checks fixed-buffer registration, in-flight fixed I/O, unregister, invalid registration, fallback, and owner-drop ordering. `IoUringSqeCqeLifecycle.tla` checks the bounded submit/complete lifecycle: each submitted request owns one live buffer until exactly one CQE is checked, short/error completions fail closed, and temporary buffers are returned only after checking. Storage correspondence tests cover concurrent mmap allocation uniqueness, sub-block bounds rejection, sync/reopen checksum refresh, `raw_ptr` bounds, failed WAL fsync frontier handling, io_uring range rejection, fixed-buffer registration input validation, and unregister-before-owner-drop behavior when the backend is available. Kernel io_uring internals remain trusted implementation code. |
| Raw trie child pointers and byte lock-free CAS paths | `persistent_artrie::{lockfree_cas,nodes/atomic_ptr,nodes/persistent_node}` | `LockFreeARTrieLinearizability.tla` checks the bounded root-CAS/cache/contains/merge publication contract. Loom tests cover single-winner root CAS, duplicate insert linearization, insert-vs-contains visibility, merge snapshot behavior, and child-pointer Arc handoff. |
| Indexed char/vocab lock-free overlays | `persistent_artrie_char::lockfree_cas`, `persistent_vocab_artrie::lockfree_cas` | `LockFreeIndexedOverlay.tla` checks char increment value preservation, merge-prefix behavior, vocab duplicate insert stability, committed-index uniqueness, sparse `next_index` claims, and cache/root/persistent agreement. Loom tests cover the same bounded schedule obligations. |
| Whole-crate unsafe inventory | `formal-verification/UNSAFE_INVENTORY.tsv`, `formal-verification/UNSAFE_CONTRACTS.tsv`, `scripts/verify-unsafe-boundary-inventory.sh` | The verification harness now compares the live `src/**/*.rs` unsafe blocks, unsafe functions, and unsafe impls against the reviewed inventory, then checks that every inventory contract tag has a reviewed contract entry. New or changed unsafe sites fail the correspondence script until the pattern, count, and contract tag are updated intentionally. |
| Unsafe `Send`/`Sync` impls outside the persistent ART core | SCDAWG handles, vocab variants, test mock nodes | The explicit unsafe impl surface is inventoried. Persistent ARTrie/vocab contracts are type-checked in `persistent_artrie_formal_correspondence`; SCDAWG byte/char handle contracts are type-checked and exercised under concurrent read traversal in `unsafe_boundary_contracts`. |

## Safety Contract Matrix

| Contract | Enforced By |
|----------|-------------|
| Disk slots and raw in-memory slot pointers are mutually exclusive. | `PointerOwnership.tla` `SlotDiskAndRawStatesAreDisjoint` invariant plus swizzle/unswizzle correspondence tests. |
| Lazy-load race losers keep private ownership and are dropped without publication. | `PointerOwnership.tla` `LoadingPointersAreThreadLocal` / `NoLoadCandidateAliasing` invariants plus `swizzled_pointer_losing_lazy_load_candidate_can_be_reclaimed_once`. |
| Raw child replacement returns the old `Box` and does not alias the new child. | Char and vocab remove/replace/deep-clone correspondence tests, Miri-gated in the harness. |
| Storage write/sync outcomes advance durability only after full write plus successful sync. | `StorageSyscallOutcome.tla`, failed fsync frontier correspondence, io_uring completion helper tests, and cached-write dirty re-marking on failed single/batched/fixed-buffer writes. |
| io_uring fixed buffers are non-null, block-sized, aligned, used only while registered, and unregistered before owner drop. | `IoUringFixedBufferOwnership.tla`, `IoUringDiskManager::register_buffer_pool` validation, `BufferManager` fixed-capability gating, `io_uring_*registration*` storage correspondence tests, and the fixed-capable `BufferManager` storage double. |
| io_uring submitted requests keep ownership of one buffer until completion checking, and short/error CQEs fail closed. | `IoUringSqeCqeLifecycle.tla` plus `IoUringDiskManager` completion-count, negative-result, short-read/write, and temporary-buffer return checks. |
| Backends that accept the default no-op registration do not accidentally enable fixed I/O. | `BufferManager` requires both registration success and `supports_fixed_buffers()`, with a regression in `tests/unsafe_boundary_contracts.rs`. |
| Every unsafe source pattern has a reviewed contract tag. | `scripts/verify-unsafe-boundary-inventory.sh` compares `src/**/*.rs` against `formal-verification/UNSAFE_INVENTORY.tsv` and checks every tag against `formal-verification/UNSAFE_CONTRACTS.tsv` before the correspondence tests run. |

## Current Claim

The repository now checks representative unsafe-boundary behavior where it
intersects the formal ARTrie model:

- pointer encodings preserve the disk/memory state partition, including a pure
  raw disk-pointer roundtrip that does not depend on mmap/io_uring, and losing
  lazy-load candidates stay unpublished until reclaimed;
- unchecked swizzled raw extraction is exercised only after the safe API
  confirms an in-memory pointer, and returns to a disk-only state after
  unswizzle;
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
  visible key as absent, and merge persists only a cache snapshot prefix in the
  bounded model.
- char lock-free increments add each successful delta exactly once, create at
  most one visible leaf for a raced key, and merge only persists a value visible
  at the snapshot point in the bounded model;
- vocab lock-free inserts publish one stable index per committed term, preserve
  uniqueness across distinct terms, and allow sparse `next_index` values when a
  duplicate race wastes a claimed index.
- synced LSNs are prefix-closed, group-commit waiters are acknowledged only
  after their LSN is inside the durable frontier, checkpoints cannot publish
  beyond that frontier, recovery only applies durable records, and VersionGc
  reclamation requires both no active readers and a durable GC decision.
- raw char and vocabulary child slots transfer ownership through
  remove/replace/deep clone without aliasing the returned `Box`, and the
  explicit unsafe `Send`/`Sync` surface remains type-checked by the
  correspondence target.
- the whole-crate unsafe inventory is checked for drift before the
  correspondence harness runs; the SCDAWG byte and char handle contracts are
  exercised by concurrent read traversal tests outside the persistent ARTrie
  feature gate, and every unsafe inventory tag resolves to a reviewed
  `UNSAFE_CONTRACTS.tsv` entry.
- vocabulary checkpoint/reopen preserves stable forward and reverse indexes,
  duplicate inserts keep the original index, checkpoint/reopen rebuilds
  `node_map` and parent-chain entries for live nodes, and leaf eviction removes
  stale `node_map` raw entries before dropping the in-memory node while sibling
  queries remain on live nodes.
- io_uring fixed-buffer registration rejects invalid buffers, fixed I/O is
  enabled only after backend capability confirmation, and the fixed-buffer
  capability is cleared before the registered buffer owner is dropped.
- `BufferManager` write-guard mutation over aligned blocks is covered by a
  fixed-capable storage double that observes batched fixed flush and
  unregister-before-owner-drop behavior.
- io_uring submitted requests keep a live buffer until one CQE is checked,
  negative and short completions fail closed, and temporary aligned buffers are
  not returned to the pool before completion checking.
- failed WAL segment fsync attempts do not advance `global_synced_lsn` or make
  `wait_for_lsn_timeout` report the target LSN as durable, and failed
  io_uring single, batched, or fixed-buffer writes re-mark updated cached
  blocks dirty so a later sync can retry.

This is not a RustBelt, Iris, Miri, Kani, or certified-compilation result. The
current harness has expanded Miri-compatible targets, including raw child
ownership, swizzled raw extraction, vocab reopen/eviction ownership, and
buffer-manager fixed-buffer lifetime. The remaining high-value proof extensions
are Miri execution for those targets on a nightly toolchain, broader mechanized
unsafe-boundary proofs for the raw-pointer and kernel io_uring internals, and
keeping the bounded correspondence CI jobs green.

## Next Obligations

1. Run `RUN_MIRI=1 scripts/verify-formal-correspondence.sh` on a nightly/Miri
   toolchain and keep the target small enough for CI.
2. Keep the no-TLC correspondence, Miri-gated, io_uring-gated, and
   scheduled/manual TLC CI jobs green.
3. Keep the unsafe inventory gate mandatory in local and CI verification so new
   unsafe blocks cannot bypass review.
4. Keep `StorageSyscallOutcome.tla` and the corresponding Rust fsync/CQE
   checks green; deepen the trusted kernel/syscall boundary only if the project
   needs claims below the syscall outcome abstraction.
5. Keep certified compilation out of scope unless the project adopts a verified
   Rust subset or extracted reference implementation as production code.
