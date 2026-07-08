# Formal-verification map — the invariant–model–proof correspondence

**Navigation**: [↑ Persistence architecture](README.md) · [Durability & recovery](durability-and-recovery.md) · [Concurrency model](concurrency-model.md) · [Formal-verification tree](../../formal-verification/)

The durability and concurrency claims made throughout this corpus are not asserted — they
are **machine-checked**. This page is the correspondence index: for each persistence
invariant, it names the TLA⁺ model that model-checks it and the Rocq proof that establishes
it, and links the doc that relies on it. It does not restate the proofs; the authoritative
source is [`formal-verification/`](../../formal-verification/) and its
[`VERIFICATION_RESULTS.md`](../../formal-verification/VERIFICATION_RESULTS.md).

## The two-pronged strategy

Verification divides the obligation space cleanly between two tools, tied to the running Rust
by an executable correspondence harness:

The map is drawn as two companion figures — one per prong — so each fits the prose column; both prongs tie to the same implementation-under-test and correspondence harness.

<img src="../diagrams/proof-artifact-map.svg" alt="Proof-artifact map, part ① of ② — the TLA+/TLC model-checking prong. Prong ① (blue) exhaustively explores bounded concurrency and crash interleavings: representative specs LockFreeARTrieLinearizability, CrashRecovery, DurabilityFrontier, and EvictionWalkEBR establish the properties linearizability, no-lost-writes, and crash-recovery completeness with zero TLC violations. The prong models the concurrency and crashes of the Rust implementation under test (grey, src/persistent_artrie — overlay, WAL, arena), and an amber correspondence harness (scripts/verify-formal-correspondence.sh) feeds SANY+TLC into the TLA prong and trace tests into the implementation, tying the models to the code alongside the Rocq make and the unsafe-inventory gate. The Rocq theorem-proving prong is the companion figure proof-artifact-map-2." width="100%"/>

<img src="../diagrams/proof-artifact-map-2.svg" alt="Proof-artifact map, part ② of ② — the Rocq theorem-proving prong. Prong ② (green) proves properties for all inputs: representative specs MapRefinement, ARTrieSpec, DictionaryLawSpec, and EpochReclamationSpec establish functional correctness, map-ADT refinement, and no-use-after-free (gated EBR), all Qed-closed with zero Admitted/Axiom/Parameter. The prong proves the function of the same Rust implementation under test (grey, src/persistent_artrie — overlay, WAL, arena), and the amber correspondence harness (scripts/verify-formal-correspondence.sh) drives the Rocq make. The TLA+/TLC model-checking prong is the companion figure proof-artifact-map." width="100%"/>

| Prong | Tool | Establishes | Scope |
|-------|------|-------------|-------|
| ① Model checking | TLA⁺ / TLC | temporal **safety + liveness** under concurrency and crashes | *bounded* instances, *all* interleavings |
| ② Theorem proving | Rocq / Coq | **functional correctness** + abstract-data-type (ADT) refinement | *all* inputs, machine-checked `Qed.` |

**Aggregate:** 69 Rocq `.v` files, 1,301 propositions (992 Theorem + 301 Lemma + 8
Corollary), **0 Admitted / 0 Axiom / 0 Parameter**; 55 TLA⁺ modules, 65 `.cfg` TLC configs,
all SANY-clean (SANY = the TLA⁺ syntactic analyzer). Many models ship a paired **`_Unsafe.cfg` negative control** that must
*violate* the invariant, proving the checker has teeth.

## Correspondence by concern

### Lock-free overlay (CAS, values, removal)

| Invariant | TLA⁺ model | Rocq proof | Doc |
|-----------|-----------|-----------|-----|
| Root-CAS **linearizability** (duplicate-insert, insert-vs-contains) | `LockFreeARTrieLinearizability.tla` | `Spec/ARTrieSpec.v`, `Proofs/MapRefinement.v` | [lock-free-overlay](lock-free-overlay.md) |
| Indexed-overlay counter accumulation + vocab index uniqueness | `LockFreeIndexedOverlay.tla` | `Spec/LockFreeCounterMergeSpec.v` | [lock-free-overlay](lock-free-overlay.md) |
| Arbitrary-`V` value-CAS + remove-CAS correctness | `LockFreeOverlayValueCas.tla`, `LockFreeOverlayRemoveCas.tla` (+ `_Unsafe`) | `Spec/ARTrieSpec.v` | [lock-free-overlay](lock-free-overlay.md) |
| Counter-merge no-lost-update, order-independent | `LockFreeCounterMergeAtomicity.tla` | `Spec/LockFreeCounterMergeSpec.v` | [lock-free-overlay](lock-free-overlay.md) |

### Durability, watermark & checkpoint

| Invariant | TLA⁺ model | Rocq proof | Doc |
|-----------|-----------|-----------|-----|
| acknowledged $\implies$ durable; `checkpoint_lsn` = committed prefix; no truncation loss | `LockFreeDurableCheckpoint.tla` (+ `_Unsafe`), `SharedPersistentConcurrency.tla` | `Spec/PublicDurabilityPolicySpec.v`, `Spec/PersistentWalAtomicitySpec.v` | [durability-and-recovery](durability-and-recovery.md) |
| Checkpoint snapshot $\subseteq$ visible; publication serialized by `CK` | `ConcurrentCheckpointPublication.tla`, `ConcurrentCheckpointSerialization.tla` (+ `_Unsafe` `NoTornDescriptor`) | `Spec/PersistentCheckpointRetentionSpec.v`, `Spec/PersistentDirtyCheckpointSpec.v` | [durability-and-recovery](durability-and-recovery.md) |
| Durable checkpoint under eviction | `LockFreeDurableCheckpointEviction.tla` (+ `_Unsafe`) | `Spec/PersistentDirtyCheckpointSpec.v` | [durability-and-recovery](durability-and-recovery.md) |

### Recovery & crash-safety

| Invariant | TLA⁺ model | Rocq proof | Doc |
|-----------|-----------|-----------|-----|
| Reopen replay reproduces exactly the visible state; drop uncommitted | `LockFreeOverlayDurableReplay.tla` (+ `_Unsafe`), `CrashRecovery.tla` | `Spec/PersistentRecoveryPlannerSpec.v`, `Spec/PersistentRecoveryReplayCompletenessSpec.v` | [durability-and-recovery](durability-and-recovery.md) |
| Epoch accounting + recovery | `EpochCheckpoint.tla`, `EpochCheckpointRecovery.tla` | `Spec/PersistentWalSegmentLifecycleSpec.v` | [durability-and-recovery](durability-and-recovery.md) |
| End-to-end refinement across checkpoint/compaction/crash/vocab | — | `Spec/PersistentEndToEndTraceSpec.v` | [durability-and-recovery](durability-and-recovery.md) |

### Concurrency & the F4 lock hierarchy

| Invariant | TLA⁺ model | Rocq / test | Doc |
|-----------|-----------|-------------|-----|
| Shared-handle linearizability (byte + char + **vocab**), reads observe completed visible state | `SharedPersistentConcurrency.tla`, `ConcurrentVocabLinearizability.tla` | `Spec/SharedPersistentConcurrencySpec.v` | [concurrency-model](concurrency-model.md) |
| F4 deadlock-freedom (`CK > merge_lock > EC`, drop-before-join) | — | `tests/persistent_lockfree_f4_lock_hierarchy_loom.rs`, `tests/vocab_lockfree_f4_lock_hierarchy_loom.rs` | [concurrency-model](concurrency-model.md) |

### Eviction & epoch reclamation

| Invariant | TLA⁺ model | Rocq proof | Doc |
|-----------|-----------|-----------|-----|
| Eviction-CAS overwrite-race + stale-image safety (`serial_disk_ptr` stamp) | `OverlayEvictionCas.tla`, `OverlayEvictionStale.tla` (+ `_Unsafe` each) | — | [eviction](eviction.md), [concurrency-model](concurrency-model.md) |
| Eviction-walk epoch reclamation + registry publish | `EvictionWalkEBR.tla`, `EvictionRegistryPublication.tla` | `Spec/PersistentCharEpochReclamationSpec.v` | [eviction](eviction.md) |
| Version-GC reader-guard reclaim safety | `VersionLifecycle.tla` | — | [concurrency-model](concurrency-model.md) |

### WAL, storage & group commit

| Invariant | TLA⁺ model | Rocq proof | Doc |
|-----------|-----------|-----------|-----|
| WAL-before-mutation ordering, fail-closed on WAL error | `WAL.tla`, `WAL_FileSystem.tla` | `Spec/PersistentWalAtomicitySpec.v`, `Spec/PersistentVocabWalAtomicitySpec.v` | [wal-format](wal-format.md) |
| Fail-closed write/sync durability boundary | `StorageSyscallOutcome.tla` | — | [durability-and-recovery](durability-and-recovery.md) |
| Backend contracts (mmap block storage, io_uring SQE/CQE + fixed-buffer ownership) | `MmapBlockStorage.tla`, `IoUringSqeCqeLifecycle.tla`, `IoUringFixedBufferOwnership.tla` | — | [storage-backends](storage-backends.md) |
| Buffer-page lease exclusion; raw-child-pointer ownership | `BufferPageLease.tla`, `PointerOwnership.tla` | — | [storage-backends](storage-backends.md) |
| Group-commit frontier: no early ack; ordered FIFO / returned-LSN | `DurabilityFrontier.tla`, `AsyncWalGroupCommit.tla` | — | [group-commit](group-commit.md) |
| Vocab reopen bijection ownership | `VocabPersistenceOwnership.tla` | `Spec/PersistentVocabCheckpointSpec.v` | [families](families.md) |

## The negative-control methodology

A model that only checks the *safe* configuration proves nothing if its invariant is
vacuous. Most durability/eviction models therefore ship an `_Unsafe.cfg` that deliberately
breaks the mechanism (e.g. a frontier-bounded reclaim instead of the watermark, or eviction
without the stamp) and asserts the checker **finds the violation**. The pair
"safe cfg passes $\land$ `_Unsafe` cfg fails" is what makes the safe result meaningful — a
falsifiable experiment in the scientific sense.

## The CI correspondence gate

The two prongs are tied to the running code by `scripts/verify-formal-correspondence.sh`:
it runs the Rust **trace-correspondence** tests (the code's observable trace matches the
spec), the Rocq `make` (asserting 0 Admitted/Axiom/Parameter), TLA⁺ SANY parsing (and,
under `RUN_TLC`, model checking), and the **`unsafe`-inventory gate** (every `unsafe` block
in `src/` must be reconciled row-for-row against `formal-verification/UNSAFE_INVENTORY.tsv`
+ `UNSAFE_CONTRACTS.tsv`, by set-equality). A drifted proof, a new unreconciled `unsafe`,
or a stale count fails CI.

## Running it locally

```bash
# Rocq proofs (resource-limited, per CLAUDE.md guidance):
systemd-run --user --scope -p MemoryMax=32G -p CPUQuota=1800% \
  make -C formal-verification/rocq -j1

# TLA+ model checking (bounded):
RUN_TLC=1 scripts/verify-formal-correspondence.sh
```

## See also

- [`formal-verification/README.md`](../../formal-verification/README.md) — the properties table and theorem list.
- [`formal-verification/VERIFICATION_RESULTS.md`](../../formal-verification/VERIFICATION_RESULTS.md) — the full model-checking + proof results and the spec $\leftrightarrow$ Rust correspondence tables.
- [`formal-verification/GAP_LEDGER.md`](../../formal-verification/GAP_LEDGER.md) — the tracked verification gaps and footguns (e.g. the `#41` watermark hazard).
