# Formal Verification Gap Ledger

Updated: 2026-05-23

This ledger records the current proof/modeling boundary for the formal
verification artifacts. "Closed" means the repository now contains a checked
Rocq proof or a TLC-checked bounded model. "Scoped" means the claim is
intentionally narrower than the production system.

## Closed In This Update

| Area | Artifact | Status |
|------|----------|--------|
| Bucket model | `rocq/Model/Bucket.v` | Added `wf_sorted_bucket`, empty sorted-bucket proof, and a nontrivial `binary_search_in_bounds` theorem. |
| Checked bucket construction | `rocq/Spec/ARTrieSpec.v` | Added `canonical_bucket_checked_wf`, proving successful checked bucket construction preserves `wf_bucket` under suffix uniqueness. |
| Structural obligations | `rocq/Invariants/StructuralInvariants.v`, `rocq/Proofs/StructuralPreservation.v` | Re-scoped named preservation obligations to successful checked operations and proved the empty checked-builder preservation case. |
| Binary-search partition | `rocq/Model/Bucket.v` | Added `binary_search_partition`: for sorted bucket entries, the returned lower-bound position partitions entries before the probe from entries at/after the probe. |
| Canonical entry normalization | `rocq/Spec/ARTrieSpec.v` | Added sorted/deduplicating checked-entry normalization, proved lookup preservation, and proved normalized entries induce sorted, duplicate-free canonical buckets. |
| Nonempty checked canonical structure | `rocq/Proofs/StructuralPreservation.v` | Proved successful nonempty checked canonical construction satisfies the full `structural_invariant`, including root child-count accuracy and bucket sortedness. |
| Checked insert/delete preservation | `rocq/Proofs/StructuralPreservation.v` | Proved `insert_preserves_structural_obligation` and `delete_preserves_structural_obligation` for successful checked operations. |
| Byzantine storage faults | `rocq/Proofs/ByzantineRecovery.v`, `tla+/ByzantineStorage.tla` | Added fail-closed proofs/models: recovery applies only committed and authenticated records. TLC passed with 11,059,201 generated states and 331,776 distinct states. |
| Byzantine quorum safety | `rocq/Model/HotStuff.v`, `rocq/Proofs/HotStuffSafety.v`, `tla+/HotStuffConsensus.tla` | Added a HotStuff/PBFT-style safety layer: 2f+1 quorums over 3f+1 replicas intersect, honest vote locks force prefix-compatible committed logs, and TLC passed the bounded one-Byzantine branch-conflict model with 17,991 generated states and 2,940 distinct states. |
| Certified reference boundary | `rocq/Proofs/CertifiedReference.v` | Added theorem wrappers for the proved `WFARTrie` reference interface and documented the TCB boundary. |
| Proof-carrying replay boundary | `rocq/Proofs/ProofCarryingExtraction.v`, `tests/persistent_artrie_formal_correspondence.rs` | Added a certified trace checker proof: valid certificates replay to the reference command-log semantics and invalid post-states are rejected. Rust correspondence tests cover valid replay and corrupt-certificate rejection. |
| Document transactions | `tla+/DocumentTransactions.tla` | Added bounded commit/abort/staged-write model. TLC passed with 39,205 generated states and 10,057 distinct states. |
| Async WAL/group commit | `tla+/AsyncWalGroupCommit.tla` | Added bounded pending/durable/group-queue model. TLC passed with 36 generated states and 19 distinct states. |
| Version lifecycle | `tla+/VersionLifecycle.tla` | Added bounded reader/retire/reclaim/durable model. TLC passed with 963 generated states and 177 distinct states. |
| Mmap block-storage synchronization | `tla+/MmapBlockStorage.tla` | Added bounded allocation/remap/access model for `MmapDiskManager`: block IDs are unique, published `file_size` never exceeds `mmapLen`, completed allocations are mapped before access, and successful reads/writes stay within the published mapping. TLC passed with 1,618,433 generated states and 540,928 distinct states. |
| Byte lock-free ARTrie linearizability | `tla+/LockFreeARTrieLinearizability.tla`, `tests/persistent_artrie_loom_correspondence.rs` | Added a bounded root-CAS/cache/contains/merge model for the byte lock-free overlay. TLC passed with 38,379 generated states and 7,593 distinct states. Loom schedule checks cover single-winner root CAS, duplicate insert linearization, insert-vs-contains visibility, merge snapshot behavior, and child-pointer Arc handoff. |
| Rust correspondence harness | `tests/persistent_artrie_formal_correspondence.rs`, `scripts/verify-formal-correspondence.sh` | Added executable checks for bucket ordering/search partitions, split/merge preservation, trie-vs-`BTreeMap` traces, deterministic large/reopen traces, document transaction visibility, WAL CRC fail-closed behavior, version-GC reader protection, group-commit LSN durability, proof-carrying replay/corrupt-certificate rejection, end-to-end crash-prefix recovery, storage boundaries, and Loom schedule checks. Full script, including then-current TLC modules, passed on 2026-05-22; the no-TLC verification script and focused Rust targets passed on 2026-05-23. |
| BlockStorage correspondence | `tests/persistent_artrie_storage_correspondence.rs`, `src/persistent_artrie_core/disk_manager.rs` | Added storage-boundary tests for concurrent mmap allocation uniqueness/accessibility, fail-closed sub-block range checks, header-checksum refresh on sync/reopen, `raw_ptr` out-of-block rejection, pure swizzled disk-pointer raw roundtrip, and optional io_uring range rejection. Mmap sub-block I/O now rejects cross-block ranges and `sync()` refreshes the header checksum before flushing. |
| WAL codec and torn-tail recovery | `tests/persistent_artrie_formal_correspondence.rs` | Added executable correspondence checks for all public WAL record variants: payload roundtrip, serialized-size accounting, truncated-payload rejection, invalid type rejection, durable-prefix preservation when a later payload is torn, and safe ignoring of a partial trailing record header. |
| End-to-end crash-prefix recovery | `tests/persistent_artrie_formal_correspondence.rs` | Added `PersistentARTrie::open` correspondence cases over real WAL bytes: header-only recovery, every complete record-boundary prefix, torn payload after a durable prefix, and transaction replay where committed records apply atomically while incomplete transactions are ignored. |
| On-disk parser rejection | `tests/persistent_artrie_formal_correspondence.rs` | Added byte-level checks for WAL header roundtrip/rejection and bucket page roundtrip/rejection, including bad magic, unsupported version, and invalid-size cases. |
| Group-commit LSN correspondence | `src/persistent_artrie_core/group_commit.rs`, `src/persistent_artrie_core/wal/{writer,async_writer}.rs` | Group commit now writes queued records with the LSN reserved and returned to callers, so the durable WAL record LSN matches the coordinator's published LSN. |
| WAL value replay correspondence | `src/persistent_artrie_char/{mmap_ctor,io_uring_ctor}.rs` | Direct open/replay now deserializes value-bearing insert records instead of replaying them as term-only inserts. |
| Unsafe-boundary executable checks | `formal-verification/UNSAFE_BOUNDARY.md`, `src/persistent_artrie_core/concurrency.rs`, `src/persistent_artrie*/nodes/atomic_ptr.rs`, `tests/persistent_artrie_formal_correspondence.rs` | Added an unsafe-boundary ledger, replaced the raw atomic `Arc` node slot with a lock-guarded `Arc` slot, made `OptimisticCell` serialize Rust-level data access with `RwLock`, and added tests for swizzled-pointer transitions, single-winner CAS, node-pointer refcount balance, concurrent optimistic writes, end-to-end torn-WAL reopen, and dictionary law traces. |

## Scoped Claims

| Claim | Scope |
|-------|-------|
| Byzantine fault handling | Storage/WAL records may be dropped, duplicated, or corrupted, and the bounded quorum model covers one-Byzantine HotStuff/PBFT-style safety. The model does not claim production Byzantine networking, liveness, malicious CPU execution, compromised cryptography, or arbitrary Rust memory corruption. |
| Certified compilation | The checked claim is for the Rocq reference map interface and proof-carrying trace checker boundary. It does not certify the Rust binary or LLVM/codegen pipeline. |
| Structural preservation | The sound target is successful checked construction. Raw total rebuilds can exceed bucket capacity and are not claimed to preserve structure for every input. |
| Unsafe/concurrency verification | The current work adds executable unsafe-boundary checks for representative ARTrie pointer/concurrency primitives, storage-boundary checks for mmap allocation/remap safety, and bounded Loom exploration for the byte lock-free publication path. A full unsafe-boundary proof, Miri pass, or schedule exploration over char/vocab lock-free paths remains future work. |

## Remaining Extension Scope

| Area | Boundary |
|------|----------------------|
| Production Byzantine consensus | Out of scope. The current proof/model covers quorum/log safety for a bounded HotStuff/PBFT-style model, not a production network protocol, pacemaker, view-change liveness proof, or cryptographic implementation. |
| Certified Rust compilation | Out of scope. The checked claim covers the Rocq reference/proof-carrying boundary plus executable Rust correspondence tests, not a certified Rust/LLVM/codegen pipeline. |
| Unsafe-boundary proof | Partially covered by executable ARTrie checks, `MmapBlockStorage.tla`, `LockFreeARTrieLinearizability.tla`, storage/loom correspondence tests, and `UNSAFE_BOUNDARY.md`. A mechanized proof for all raw pointers, io_uring internals, and unsafe `Send`/`Sync` implementations remains out of scope. |
