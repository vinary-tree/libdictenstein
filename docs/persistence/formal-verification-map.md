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

**Aggregate:** 75 Rocq `.v` files, 1,449 propositions (1,095 Theorem + 340 Lemma + 14
Corollary), **0 Admitted / 0 Axiom / 0 Parameter**; 60 TLA⁺ modules, 84 `.cfg` TLC configs,
all SANY-clean (SANY = the TLA⁺ syntactic analyzer). Many models ship a paired **`_Unsafe.cfg` negative control** that must
*violate* the named invariant, proving the checker has teeth. The correspondence
harness gives every TLC invocation a distinct on-disk state directory. A
negative control succeeds only when TLC returns status `12` and the diagnostic
names the required invariant (or, for the liveness control, status `13` and a
temporal-property violation). Parser failures, resource failures,
timeouts, and state-directory collisions therefore fail the gate instead of
masquerading as successful negative controls.

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
| acknowledged $`\implies`$ durable; `checkpoint_lsn` = committed prefix; no truncation loss | `LockFreeDurableCheckpoint.tla` (+ `_Unsafe`), `SharedPersistentConcurrency.tla` | `Spec/PublicDurabilityPolicySpec.v`, `Spec/PersistentWalAtomicitySpec.v` | [durability-and-recovery](durability-and-recovery.md) |
| Checkpoint snapshot $`\subseteq`$ visible; publication serialized by `CK` | `ConcurrentCheckpointPublication.tla`, `ConcurrentCheckpointSerialization.tla` (+ `_Unsafe` `NoTornDescriptor`) | `Spec/PersistentCheckpointRetentionSpec.v`, `Spec/PersistentDirtyCheckpointSpec.v` | [durability-and-recovery](durability-and-recovery.md) |
| Durable checkpoint under exact eviction authority: committed-watermark reclaim, captured-root publication, stamped catalogs, exact-use revalidation, and recovery independent of detached state | `LockFreeDurableCheckpointEviction.tla` (+ six named unsafe controls), `EvictionExactRootPublication.tla` | `Spec/PersistentDirtyCheckpointSpec.v`, `Spec/PersistentCharEvictionRegistrySpec.v`, `Spec/EvictionExactRootPublicationSpec.v` | [durability-and-recovery](durability-and-recovery.md), [eviction](eviction.md) |

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
| Character-node V2/V3 writer-reader compatibility, exact child-type preservation, committed-arena publication, and crash/reopen | `CharV3ArenaPublication.tla` (+ seven named unsafe controls) | `Spec/CharV3TypeEncodingSpec.v`, `tests/char_node_format_compatibility.rs`, `tests/char_v3_crash_reopen_correspondence.rs` | [char-node-format](char-node-format.md) |
| Public API feature visibility | — | `Spec/ApiFeatureVisibilitySpec.v`, downstream compile fixtures exercised by `scripts/verify-api-feature-visibility.sh` | [persistence overview](README.md) |

### Eviction & epoch reclamation

| Invariant | TLA⁺ model | Rocq proof | Doc |
|-----------|-----------|-----------|-----|
| Eviction-CAS overwrite-race, stale-image safety, and exact fault provenance | `OverlayEvictionCas.tla`, `OverlayEvictionStale.tla` (+ stale-eviction and fault-stamp unsafe controls) | `Spec/OverlayFaultProvenanceSpec.v`, `tests/persistent_artrie_loom_correspondence.rs` | [eviction](eviction.md), [concurrency-model](concurrency-model.md) |
| Exact resident-budget closure, ancestor suppression, cap enforcement, and snapshot revalidation | `ResidentBudgetEviction.tla` (+ local-descendant and stale-snapshot unsafe controls) | `Spec/ResidentBudgetEvictionSpec.v`, resident-budget property/unit tests in `src/persistent_artrie/core/eviction/coordinator.rs` | [eviction](eviction.md), [concurrency-model](concurrency-model.md) |
| Eviction-walk epoch reclamation; lock-free exact-root publication and helped residency; detached callback separation; irreversible coordinator retirement and re-enable | `EvictionWalkEBR.tla`, `EvictionExactRootPublication.tla`, `HelpedRootResidency.tla`, `DetachedCallbackSeparation.tla` (+ exact-root, helped-residency, and capability-separation unsafe controls) | `Spec/PersistentCharEpochReclamationSpec.v`, `Spec/EvictionExactRootPublicationSpec.v`, `Spec/HelpedRootResidencySpec.v`, `Spec/DetachedCallbackSeparationSpec.v`, `tests/persistent_eviction_publication_gate_loom.rs` | [eviction](eviction.md), [concurrency-model](concurrency-model.md) |
| Detached compatibility authority and cacheless owned lookup/removal: last-collision selection, read-only lookup, materialize-before-mutation failure atomicity, exact accounting, and detached results | `DetachedCallbackSeparation.tla`, `CachelessOwnedRegistry.tla` (+ five authority-separation controls and first-collision/mutate-before-materialize controls) | `Spec/DetachedCallbackSeparationSpec.v`, focused unit tests in `src/persistent_artrie/core/eviction/disk_registry.rs`, `tests/public_eviction_registry_api_correspondence.rs` | [eviction](eviction.md), [concurrency-model](concurrency-model.md) |
| Packed-residency ordinal exhaustion: one exact root winner, distinct fresh address domain, complete payload materialization, and old-helper isolation | `PackedResidencyFreshCatalog.tla` (+ reuse, wrong-helper, partial-copy, and non-exact-root unsafe controls) | `Spec/PackedResidencyRefinementSpec.v`, fresh-catalog Loom and byte/char parity regressions in `tests/persistent_lockfree_overlay_loom.rs` and `src/persistent_artrie/core/eviction/coordinator.rs` | [eviction](eviction.md), [concurrency-model](concurrency-model.md) |
| Stack-safe overlay serialization specialization: revision-bound arborescence authority, tree/DAG trace equivalence, guarded deferred stamps, and zero census work | `OverlayTreeWitness.tla` (+ forged-DAG, stale-witness, unwitnessed-fast-path, and admitted-cycle unsafe controls) | `Spec/OverlayArborescenceSerializationSpec.v`, focused policy/ownership regressions in `src/persistent_artrie/core/overlay/compressed_serialize.rs` | [eviction](eviction.md), [concurrency-model](concurrency-model.md) |
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

### ABI resource layer (vt.dictionary.v1 producer)

The language-binding boundary joined the correspondence program in wave W2
of the family ABI plan. Its registry is
[`formal-verification/ABI_INVARIANTS.tsv`](../../formal-verification/ABI_INVARIANTS.tsv)
(checked by `scripts/check-abi-invariants.py` inside this gate), and its
artifacts follow the same spec ↔ test convention as every concern above:

| Model | Checks | Executable mirror |
|---|---|---|
| `formal-verification/tla+/AbiProducerSnapshot.tla` (+`_Unsafe.cfg` negative control) | immutable-capture law, fresh-capture visibility, content-preserving maintenance publishes [LDICT-SNAP-1..3] | `tests/ffi_snapshot_law.rs`, `tests/ffi_crud_model_correspondence.rs`, torn-capture regression in `tests/ffi_concurrent_snapshot_stress.rs` |
| `formal-verification/rocq/Spec/AbiTraversalSnapshotSpec.v` | ABI-local node-id arena laws: stable, append-only, unambiguous, write-once memoization, well-formedness preservation [LDICT-ARENA-1..5] | `src/bindings.rs` node-id unit tests |
| `formal-verification/rocq/Spec/AbiPagingProducerSpec.v` | paging bounds, out_total stability, lossless page decomposition [LDICT-PAGE-1..2] | `tests/ffi_resource_paging_proptest.rs` |
| `formal-verification/rocq/Spec/AbiStatusMappingSpec.v` | status-table bijections and the deliberate project/interop divergence (raw-integer reuse provably misroutes) [LDICT-STAT-1..2] | `tests/ffi_status_matrix.rs` |

Lifecycle (retain/release) is deliberately NOT re-modeled here: the protocol
is owned by the interop lifecycle model in the liblevenshtein-rust repo
(`docs/verification/tla/AbiResourceLifecycle.tla`, invariants VT-LIFE-1..6);
this repo realizes it and pins the realization with the
`abi-owned-resource-release-once` unsafe-contract row plus the balance and
cross-thread teardown tests [LDICT-LIFE-1..2].

## The negative-control methodology

A model that only checks the *safe* configuration proves nothing if its invariant is
vacuous. Most durability/eviction models therefore ship an `_Unsafe.cfg` that deliberately
breaks the mechanism (e.g. a frontier-bounded reclaim instead of the watermark, or eviction
without the stamp) and asserts the checker **finds the violation**. The pair
"safe cfg passes $`\land`$ `_Unsafe` cfg fails" is what makes the safe result meaningful — a
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
- [`formal-verification/VERIFICATION_RESULTS.md`](../../formal-verification/VERIFICATION_RESULTS.md) — the full model-checking + proof results and the spec $`\leftrightarrow`$ Rust correspondence tables.
- [`formal-verification/GAP_LEDGER.md`](../../formal-verification/GAP_LEDGER.md) — the tracked verification gaps and footguns (e.g. the `#41` watermark hazard).
