# Unsafe Boundary Verification Notes

Updated: 2026-05-23

This document records the current Rust implementation boundary that is not
covered by Rocq extraction or TLC state exploration. The goal is to keep every
unsafe primitive tied to an executable correspondence check or an explicit
remaining proof obligation.

## Boundary Map

| Boundary | Rust Surface | Current Assurance |
|----------|--------------|-------------------|
| CAS-style `Arc` node pointer ownership | `persistent_artrie::nodes::atomic_ptr`, `persistent_artrie_char::nodes::atomic_ptr` | The previous raw atomic `Arc` slot has been replaced with a lock-guarded `Arc` slot, avoiding load-vs-replace refcount races. Correspondence and unit tests check byte-node refcount balance and single-winner CAS visibility. |
| Swizzled disk/memory pointer encoding | `persistent_artrie_core::swizzled_ptr` | Correspondence tests check disk-location encoding at max bounds, pure raw disk-pointer roundtrip, swizzle/unswizzle transition, and single-winner null initialization. |
| Optimistic mutable cell | `persistent_artrie_core::concurrency::OptimisticCell` | The data path is guarded by `RwLock` so the safe API no longer permits Rust-level read/write races; tests check concurrent writer serialization, version parity, and final value. |
| WAL crash/recovery prefix | `persistent_artrie_core::wal`, `persistent_artrie_core::recovery`, `PersistentARTrie::open` | Parser checks are extended with end-to-end reopen tests over real WAL bytes: every complete record-boundary prefix, torn header/payload after a durable prefix, and committed-vs-incomplete transaction replay. |
| mmap/io_uring storage access | `persistent_artrie_core::{disk_manager,io_uring_disk_manager}` and trie constructors | `MmapBlockStorage.tla` checks the bounded allocation/remap/access protocol, and storage correspondence tests cover concurrent mmap allocation uniqueness, sub-block bounds rejection, sync/reopen checksum refresh, `raw_ptr` bounds, and io_uring range rejection when the backend is available. io_uring internals remain trusted implementation code. |
| Raw trie child pointers and byte lock-free CAS paths | `persistent_artrie::{lockfree_cas,nodes/atomic_ptr,nodes/persistent_node}` | `LockFreeARTrieLinearizability.tla` checks the bounded root-CAS/cache/contains/merge publication contract. Loom tests cover single-winner root CAS, duplicate insert linearization, insert-vs-contains visibility, merge snapshot behavior, and child-pointer Arc handoff. Char/vocab lock-free paths remain future extension scope. |
| Unsafe `Send`/`Sync` impls outside the persistent ART core | DAWG/SCDawg/vocab variants | Documented as outside the current ARTrie formal model. They should be audited under a library-wide unsafe ledger before stronger whole-crate claims. |

## Current Claim

The repository now checks representative unsafe-boundary behavior where it
intersects the formal ARTrie model:

- pointer encodings preserve the disk/memory state partition, including a pure
  raw disk-pointer roundtrip that does not depend on mmap/io_uring;
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

This is not a RustBelt, Iris, Miri, Kani, or certified-compilation result. The
remaining high-value proof extension is schedule exploration for the char/vocab
lock-free trie paths and unsafe `Send`/`Sync` contracts outside this byte
ARTrie/storage boundary.

## Next Obligations

1. Extend Loom or Shuttle models to char/vocab lock-free paths, version-GC
   readers, group commit publication, and lock-free increments.
2. Add more Miri-compatible unit targets for raw pointer reconstruction paths
   beyond the covered `SwizzledPtr` disk-pointer raw roundtrip.
3. Split remaining unsafe impls into documented safety contracts with one
   nearby regression test per contract.
4. Keep certified compilation out of scope unless the project adopts a verified
   Rust subset or extracted reference implementation as production code.
