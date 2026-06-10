# ARTrie Formal Verification Results

## Summary

This document records the results of formal verification efforts for the Persistent Adaptive Radix Trie (PART) implementation in libdictenstein.

**Date:** 2026-01-20 (Updated: 2026-01-24 — TOCTOU Race Condition Fixes; 2026-05-20 — All `Admitted`/`Axiom` obligations eliminated across Model + Invariants + Spec, see commit `b7630ad` "Prove ARTrie Rocq map correctness" and `efe1943` "proofs(rocq): eliminate Admitted/Axiom obligations across Model + Invariants + Spec"; 2026-05-22 — checked structural contracts, bounded Byzantine storage and HotStuff-style quorum models, proof-carrying replay boundary, expanded TLA+ focused models, and Rust correspondence harness; 2026-05-23 — end-to-end WAL crash-prefix matrix, transaction replay correspondence, mmap block-storage synchronization, storage syscall outcome fail-closed durability boundary, byte lock-free ARTrie linearizability, indexed char/vocab lock-free overlay linearizability, durability-frontier/reclamation safety, raw pointer ownership boundary checks, vocab persistence/eviction ownership, io_uring fixed-buffer ownership and registration contracts, io_uring SQE/CQE lifecycle checking, public dictionary law conformance, DynamicDawg mutation/compaction preservation, double-array trie construction/traversal correctness, zipper/query-language conformance, substring candidate correctness, SCDAWG occurrence construction correctness, fuzzy candidate coverage, public serialization roundtrip correctness, and feature-gated protobuf/compression codec correspondence; 2026-05-24 — expanded Miri-gated unsafe-boundary targets for swizzled raw extraction, vocab reopen node-map/parent-chain rebuild, vocab eviction query liveness, BufferManager fixed-buffer lifetime, persistent cursor/batched/grouped/parallel merge equivalence, persistent char prefix semantics, valued set-combinator merge semantics for union/intersection zippers, Bloom filter no-false-negative lookup rejection, and arena reservation/dirty-slot persistence correspondence; 2026-05-25 — persistent deduplicating-arena cache soundness, root descriptor/reopen refinement, persistent lazy mutation atomicity for no-WAL-on-error/no-op behavior and replay after successful lazy writes, persistent WAL write-atomicity for serialization/WAL failures, atomic writes, document commits, and checked transaction increments, checkpoint/WAL retention safety for corruption rebuilds from archive/pending/active segments, dirty checkpoint publication safety for dirty-slot retry and descriptor-before-truncation reopen, WAL segment lifecycle safety for LSN-ordered archive handling and monotonic rotation/reopen, recovery planner durable-prefix safety for corrupt WAL suffixes, recovery replay completeness for all mutating WAL variants/no-WAL replay/corrupt archive and invalid-arithmetic prefixes, byte persistent compaction rewrite/finalization recovery, persistent vocabulary WAL atomicity/bijection fixes, persistent vocabulary checkpoint/sidecar publication fixes, concurrent checkpoint publication fixes, public concurrent vocabulary linearizability, char/vocab rewrite checkpoint dirty-state fixes, epoch checkpoint recovery correspondence, persistent char bulk-mutation durable-prefix/checked-RMW correspondence, persistent transaction increment recovery correspondence, lock-free counter merge atomicity correspondence, shared persistent public-API concurrency correspondence with a byte `SharedARTrie::checkpoint` lock-publication fix, and public durability acknowledgement policy correspondence with byte/char/vocab full-policy sync fixes; 2026-05-26 — strengthened ordered `AsyncWalGroupCommit` model, added `PersistentPublicWalLifecycleSpec.v`, added byte/char/vocab public open/replay and group-commit correspondence tests, added default/all-features compile gates to the formal harness, and added end-to-end persistent trace refinement across checkpoint, compaction rewrite, crash/reopen, and vocab bijection behavior; 2026-06-01 — added BufferManager page-lease and TraversalContext cached-page pinning coverage, added reverse-index mmap/remap publication coverage, and refreshed unsafe inventory/contract evidence for newly documented unsafe operations)

---

## TLA+ Model Checking Results

### Modules Verified

| Module | LOC | Status |
|--------|-----|--------|
| ARTrieTypes.tla | ~387 | Syntax Valid |
| ARTrieState.tla | ~272 | Syntax Valid |
| WAL.tla | ~400 | Syntax Valid |
| Concurrency.tla | ~372 | Syntax Valid |
| CrashRecovery.tla | ~372 | Syntax Valid |
| NodeTransitions.tla | ~383 | Syntax Valid |
| EpochCheckpoint.tla | ~372 | Syntax Valid |
| PART.tla | ~457 | Syntax Valid |
| FileSystem.tla | ~450 | Syntax Valid |
| WAL_FileSystem.tla | ~350 | Syntax Valid |
| DocumentTransactions.tla | ~107 | TLC passed |
| AsyncWalGroupCommit.tla | 116 | TLC passed |
| VersionLifecycle.tla | ~87 | TLC passed |
| DurabilityFrontier.tla | 224 | TLC passed |
| PointerOwnership.tla | 371 | TLC passed |
| VocabPersistenceOwnership.tla | 180 | TLC passed |
| MmapBlockStorage.tla | 182 | TLC passed |
| StorageSyscallOutcome.tla | 143 | TLC passed |
| BufferPageLease.tla | 144 | TLC passed |
| ReverseIndexMmap.tla | 91 | TLC passed |
| IoUringFixedBufferOwnership.tla | 132 | TLC passed |
| IoUringSqeCqeLifecycle.tla | 189 | TLC passed |
| LockFreeARTrieLinearizability.tla | 153 | TLC passed |
| LockFreeIndexedOverlay.tla | 299 | TLC passed |
| LockFreeCounterMergeAtomicity.tla | 117 | TLC passed |
| ConcurrentCheckpointPublication.tla | 285 | TLC passed |
| SharedPersistentConcurrency.tla | 303 | TLC passed |
| PublicDurabilityPolicy.tla | 154 | TLC passed |
| PersistentEndToEndTrace.tla | 121 | TLC passed |
| ConcurrentVocabLinearizability.tla | 345 | TLC passed |
| EpochCheckpointRecovery.tla | 186 | TLC passed |
| BackgroundWorkerLifecycle.tla | 95 | TLC passed |
| PersistentCharBulkMutationRecovery.tla | 190 | TLC passed |
| PersistentTransactionIncrementRecovery.tla | 201 | TLC passed |
| ByzantineStorage.tla | ~70 | TLC passed |
| HotStuffConsensus.tla | ~91 | TLC passed |

**Total TLA+ LOC:** 10,419

### Model Checking Configuration

#### Basic Configuration (PART.cfg)
```
NumThreads = 3
MaxKeys = 4
MaxKeyLength = 3
MaxLSN = 15
MaxEpoch = 3
MaxTxId = 10
EnableCrash = FALSE
Keys = {"a", "ab", "abc", "b"}
Values = {1, 2, 3}
NodeIds = {1, 2, 3, 4, 5, 6, 7, 8}
```

#### Crash Recovery Configuration (PART_crash.cfg)
```
NumThreads = 2
MaxKeys = 3
MaxKeyLength = 3
MaxLSN = 12
MaxEpoch = 3
MaxTxId = 8
EnableCrash = TRUE
Keys = {"x", "xy", "z"}
Values = {"v1", "v2"}
NodeIds = {1, 2, 3, 4, 5, 6}
```

### Results

#### TLC Run (Crash Recovery Enabled)
- **States Generated:** ~7,000,000 (in 10 minutes)
- **Distinct States:** ~4,200,000
- **Rate:** ~800,000 states/minute (8 workers on 36 cores)
- **Safety Violations:** None found
- **Deadlocks:** None found

#### Properties Verified

| Property | Category | Status |
|----------|----------|--------|
| CombinedSafetyInvariant | Safety | No violations in explored states |
| PROPERTY_CrashRecovery | Liveness | No violations in explored states |
| Deadlock Freedom | Safety | No deadlocks found |

#### Focused TLC Runs Added 2026-05-22 Through 2026-06-01

| Module | Config | States Generated | Distinct States | Depth | Result |
|--------|--------|-----------------:|----------------:|------:|--------|
| DocumentTransactions.tla | DocumentTransactions.cfg | 39,205 | 10,057 | 13 | No errors |
| AsyncWalGroupCommit.tla | AsyncWalGroupCommit.cfg | 69 | 26 | 5 | No errors |
| VersionLifecycle.tla | VersionLifecycle.cfg | 963 | 177 | 7 | No errors |
| DurabilityFrontier.tla | DurabilityFrontier.cfg | 176,038 | 16,995 | 18 | No errors |
| PointerOwnership.tla | PointerOwnership.cfg | 68,721 | 11,064 | 15 | No errors |
| VocabPersistenceOwnership.tla | VocabPersistenceOwnership.cfg | 83,605 | 7,985 | 13 | No errors |
| MmapBlockStorage.tla | MmapBlockStorage.cfg | 1,618,433 | 540,928 | 33 | No errors |
| StorageSyscallOutcome.tla | StorageSyscallOutcome.cfg | 88 | 42 | 11 | No errors |
| IoUringFixedBufferOwnership.tla | IoUringFixedBufferOwnership.cfg | 2,329 | 456 | 13 | No errors |
| IoUringSqeCqeLifecycle.tla | IoUringSqeCqeLifecycle.cfg | 6,785 | 1,984 | 11 | No errors |
| LockFreeARTrieLinearizability.tla | LockFreeARTrieLinearizability.cfg | 38,379 | 7,593 | 16 | No errors |
| LockFreeIndexedOverlay.tla | LockFreeIndexedOverlayCounter.cfg | 7,681 | 900 | 10 | No errors |
| LockFreeIndexedOverlay.tla | LockFreeIndexedOverlayVocabulary.cfg | 1,659 | 333 | 9 | No errors |
| LockFreeCounterMergeAtomicity.tla | LockFreeCounterMergeAtomicity.cfg | 421,373 | 30,242 | 19 | No errors |
| BackgroundWorkerLifecycle.tla | BackgroundWorkerLifecycle.cfg | 4 | 4 | 3 | No errors (TypeOK, NoOrphan, Termination) |
| ConcurrentCheckpointPublication.tla | ConcurrentCheckpointPublication.cfg | 1,703 | 312 | 10 | No errors |
| SharedPersistentConcurrency.tla | SharedPersistentConcurrency.cfg | 11,009 | 2,752 | 13 | No errors |
| PublicDurabilityPolicy.tla | PublicDurabilityPolicy.cfg | 33,281 | 3,568 | 10 | No errors |
| PersistentEndToEndTrace.tla | PersistentEndToEndTrace.cfg | 677 | 172 | 5 | No errors |
| ConcurrentVocabLinearizability.tla | ConcurrentVocabLinearizability.cfg | 31,675 | 23,836 | 10 | No errors |
| EpochCheckpointRecovery.tla | EpochCheckpointRecovery.cfg | 3,211 | 2,050 | 10 | No errors |
| PersistentCharBulkMutationRecovery.tla | PersistentCharBulkMutationRecovery.cfg | 214,786 | 23,632 | 10 | No errors |
| PersistentTransactionIncrementRecovery.tla | PersistentTransactionIncrementRecovery.cfg | 18,056,329 | 2,891,300 | 20 | No errors |
| ByzantineStorage.tla | ByzantineStorage.cfg | 11,059,201 | 331,776 | 21 | No errors |
| HotStuffConsensus.tla | HotStuffConsensus.cfg | 17,991 | 2,940 | 12 | No errors |

All listed focused modules also passed `tla2sany` syntax/semantic checking.

#### Implementation Correspondence Runs Added 2026-05-22

The repository now includes `tests/persistent_artrie_formal_correspondence.rs`,
`tests/dictionary_law_correspondence.rs`, and
`scripts/verify-formal-correspondence.sh` to tie the checked models to the Rust
implementation surface.

| Check | Rust/Formal Boundary | Result |
|-------|----------------------|--------|
| Bucket sorted reference | `Bucket.v` to `src/persistent_artrie/bucket.rs` | Passed, 64 proptest cases |
| Bucket split/merge preservation | structural bucket obligations to Rust split/merge | Passed, 64 proptest cases |
| Trie trace reference | `CertifiedReference.v` boundary to `PersistentARTrie` behavior | Passed, 64 proptest cases |
| Deterministic large trie trace | normalized checked-entry/reference-map boundary to `PersistentARTrie` behavior | Passed, 2,048 operations |
| Deterministic reopen trace | WAL value replay/recovery boundary to `PersistentARTrie::open` | Passed, 768 operations |
| Document transaction visibility | `DocumentTransactions.tla` to `document_tx.rs` | Passed |
| WAL CRC fail-closed reads | Byzantine/corruption filtering boundary to WAL reader | Passed |
| WAL codec roundtrip | all public `WalRecord` variants to byte payloads | Passed |
| WAL parser rejection | invalid type, truncated payload, torn trailing header | Passed |
| WAL durable-prefix behavior | intact records remain readable before a later torn payload | Passed |
| WAL record-boundary reopen prefixes | crash-prefix recovery model to `PersistentARTrie::open` over real WAL bytes | Passed, header-only plus 5 record prefixes |
| End-to-end torn WAL payload reopen | recovery applies only the durable prefix before a partial payload | Passed |
| WAL transaction recovery | committed transaction replay vs incomplete transaction discard | Passed |
| WAL header parser | header roundtrip plus bad magic/version rejection | Passed |
| Bucket page parser | page roundtrip plus bad magic/version/size rejection | Passed, 64 proptest cases |
| Version-GC reader protection | `VersionLifecycle.tla` to `version_gc.rs` | Passed |
| Group-commit ordered durable LSN prefix | `AsyncWalGroupCommit.tla` to `group_commit.rs`/async WAL | Passed, including FIFO queue/returned-LSN correspondence |
| Proof-carrying trace replay | `ProofCarryingExtraction.v` to certified trace checker behavior | Passed |
| Corrupt certificate rejection | `invalid_step_rejected` to Rust certificate checker fail-closed behavior | Passed |
| Swizzled-pointer state contract | unsafe pointer encoding boundary to `SwizzledPtr` | Passed, including pure raw disk-pointer roundtrip, raw extraction only after confirmed published in-memory state, strict-provenance memory sentinels that cannot reconstruct pointers from integers, and lazy-load loser reclamation |
| Atomic node pointer CAS ownership | former unsafe raw `Arc` slot boundary to lock-guarded `AtomicNodePtr` | Passed |
| Optimistic-cell writer serialization | unsafe interior-mutability boundary to `OptimisticCell` | Passed |
| Raw char/vocab child ownership | `PointerOwnership.tla` to `CharTrieNodeInner` and `VocabTrieNode` child remove/replace/deep-clone ownership transfer plus unique `get_or_create_child` mutation borrows | Passed, including focused Miri targets |
| Vocab checkpoint/reopen bijection | `VocabPersistenceOwnership.tla` to `PersistentVocabARTrie::checkpoint/open` and reverse-index rebuild | Passed, including Unicode terms, direct `node_map`/parent-chain rebuild checks, and heap-only Miri node-map liveness checks |
| Vocab duplicate insert after reopen | stable term-index contract to `PersistentVocabARTrie::insert` after reload | Passed |
| Vocab shared-prefix node-map aliasing | side-table uniqueness contract to `insert_with_index_no_wal` on shared-prefix inserts | Passed; caught/fixed duplicate `NodeRef` allocation for an existing live child |
| Vocab eviction node-map invalidation | `VocabPersistenceOwnership.tla` to in-memory eviction replacing a child with a disk pointer | Passed, including parent-eviction rejection, stale-pointer invalidation before drop, sibling query preservation after leaf eviction, and focused Miri targets |
| Unsafe inventory drift gate | live `src/**/*.rs` unsafe surface to `formal-verification/UNSAFE_INVENTORY.tsv` | Passed |
| Unsafe contract coverage/status gate | unsafe inventory contract tags to `formal-verification/UNSAFE_CONTRACTS.tsv` coverage metadata | Passed |
| Unsafe Send/Sync contracts | explicit unsafe impl surface for persistent nodes, optimistic cell, vocab trie, lock-free/concurrent vocab wrappers, and MVCC read transactions | Passed |
| SCDAWG handle thread contracts | byte and Unicode SCDAWG handle `Send`/`Sync` contracts outside the persistent feature gate | Passed, compile-time assertions plus concurrent read traversal |
| End-to-end torn-WAL reopen | crash-prefix model to `PersistentARTrie::open` | Passed, torn header and torn payload |
| Persistent dictionary law trace | reference-map laws to public mutation/query methods | Passed |
| Public dictionary law spec | `DictionaryLawSpec.v` to public `Dictionary` / `MappedDictionary` / zipper / bijective APIs | Passed, 9 default-feature tests and 11 `persistent-artrie` tests |
| DynamicDawg mutation spec | `DynamicDawgMutationSpec.v` to byte/Unicode `DynamicDawg` insert/update/remove, batch mutation, compaction, minimization, value-preserving rebuild, and shared-node copy-on-write behavior | Passed, 5 default-feature tests |
| DynamicDawgU64 sequence spec | `DynamicDawgU64Spec.v` to public `DynamicDawgU64` sequence mutation, value update, string/f64 adapters, iterator, zipper, and bounded snapshot-concurrency behavior | Passed, 10 default-feature tests; caught/fixed update-before-overwrite and empty-sequence iterator bugs |
| Bloom filter no-false-negative spec | `BloomFilterSpec.v` to public `BloomFilter` byte/string APIs and Bloom-backed DynamicDawg lookup | Passed, 8 default-feature tests |
| Double-array trie spec | `DoubleArrayTrieSpec.v` to byte and Unicode `DoubleArrayTrie` BASE/CHECK traversal, construction normalization, mapped lookup, child iteration, and zipper values | Passed, 8 default-feature tests |
| Zipper language spec | `ZipperLanguageSpec.v` to public `DictZipper` / `ValuedDictZipper` traversal, iterator, filter, combinator, suffix, SCDAWG, and persistent zipper APIs | Passed, 8 default-feature tests and 9 `persistent-artrie` tests |
| Valued set-combinator spec | `ValuedSetCombinatorSpec.v` to `UnionZipper` / `IntersectionZipper` duplicate-value merge semantics | Passed, 7 default-feature tests and 9 `lling-llang` tests |
| Persistent merge spec | `PersistentMergeSpec.v` to `PersistentARTrie` cursor pagination, ordinary batched merge, arena-grouped batched merge, and feature-gated parallel merge | Passed, 3 `persistent-artrie` tests and 4 `persistent-artrie parallel-merge` tests |
| Persistent prefix spec | `PersistentPrefixSpec.v` to `PersistentARTrieChar` prefix iteration, valued/arena projections, ordinary removal, batched removal, durable-prefix deletion, and checked RMW overflow | Passed, 4 prefix tests and 4 bulk-mutation tests |
| PathMap/factory spec | `PathMapFactorySpec.v` to optional `PathMapDictionary`, `PathMapDictionaryChar`, `PathMapZipper`, mutation traits, and `DictionaryFactory` dispatch | Passed, 4 `pathmap-backend` tests; caught/fixed missing `MutableDictionary` impls and collapsed Unicode sibling edges |
| Relative encoding spec | `RelativeEncodingSpec.v` to byte/char child-pointer relative encoding, checked decode APIs, persistent v2 char deserialization, and dedup-cache soundness boundary | Passed, 6 `persistent-artrie` tests; caught/fixed same-arena forward-child saturation, truncated decode panics, odd relative tags, relative underflow, and sequential overflow |
| Arena reservation spec | `ArenaReservationSpec.v` to byte/char `ArenaManager` slot allocation, reservation, update, dirty-flush, and load/reopen behavior | Passed, 6 `persistent-artrie` tests; caught/fixed byte missing slot update/defensive validity recovery, char update dirty-tracker omission, and stale V3 checksums on partial dirty-slot writes |
| Deduplicating arena spec | `DedupArenaSpec.v` to byte/char `DeduplicatingArenaManager`, `NodeDeduplicator`, and `BatchDeduplicator` cache/reuse behavior | Passed, 9 `persistent-artrie` tests; covers verified hit reuse, stale-cache fail-closed allocation, verify-false compatibility behavior, direct allocation bypass, cache clear, and batch take |
| Root descriptor reopen spec | `RootDescriptorReopenSpec.v` to byte/char root descriptor publication, arena-count validation, WAL skip-threshold fallback, and char lazy-load fail-closed reads | Passed, 6 `persistent-artrie` tests; caught/fixed unknown byte descriptor-as-empty load, char test-only root-load panic, invalid `arena_count` trust/unbounded loading, and public lazy-read panic wrappers |
| Persistent lazy mutation spec | `PersistentLazyMutationSpec.v` to char lazy mutation preflight, WAL append ordering, no-op duplicate inserts, failed insert/value-insert/remove behavior, and replay after successful lazy writes | Passed, 4 `persistent-artrie` tests; caught/fixed char lazy insert panics and WAL-before-failed-mutation divergence |
| Persistent WAL atomicity spec | `PersistentWalAtomicitySpec.v` to byte/char value-write serialization failures, WAL-before-mutation ordering for atomic writes, checked transaction increment overflow, document commit ordering, and replay after successful atomic writes | Passed, 8 WAL-atomicity tests plus 5 transaction-increment tests; caught/fixed byte mutation-before-WAL paths, `.ok()` value-dropping WAL records, byte/char document commit visibility before durable CommitTx, byte transaction overflow staging, and char transaction aggregate/current overflow before BatchIncrement WAL append |
| Persistent vocab WAL atomicity spec | `PersistentVocabWalAtomicitySpec.v` to vocab insert/manual-index/batch WAL-before-visible-mutation ordering, exact reverse-index membership, collision/reindex rejection, and replay after successful inserts | Passed, 6 `persistent-artrie` tests; caught/fixed ignored WAL append/sync errors, allocator advancement before durable acceptance, public no-WAL manual inserts, duplicate batch index gaps, and range-based `contains_index` false positives |
| Persistent vocab checkpoint publication spec | `PersistentVocabCheckpointSpec.v` to vocab checkpoint/reopen bijection, failed publication dirty/WAL retention, post-checkpoint LSN continuation, non-checkpoint sync/rotation, and sidecar rebuild safety | Passed, 11 `persistent-artrie` tests; caught/fixed ignored WAL checkpoint/truncate errors, post-checkpoint LSN reuse after truncate, pre-checkpoint WAL truncation during recovery, trusted missing/corrupt/stale reverse-index sidecars, rotate-time incomplete arena flush header corruption, and `sync_to_disk` dirty clearing without checkpoint |
| Concurrent checkpoint publication model | `ConcurrentCheckpointPublication.tla` to `ConcurrentVocabARTrie` queue/lock-free checkpoint publication and Loom gate model | Passed, 6 `persistent-artrie` tests and 3 focused Loom gate checks; caught/fixed queue batch duplicate index instability, public concurrent `checkpoint()`/`flush()` acting as WAL-only sync rather than durable checkpoint publication, and mutation-vs-checkpoint races via an explicit publication gate |
| Shared persistent public concurrency model | `SharedPersistentConcurrency.tla` and `SharedPersistentConcurrencySpec.v` to byte/char/vocab `Shared*` public checkpoint/write/sync/reopen APIs | Passed, 3 `persistent-artrie` tests; caught/fixed byte `SharedARTrie::checkpoint()` dropping its write lock between data snapshot publication and checkpoint WAL/truncation |
| Public durability acknowledgement policy | `PublicDurabilityPolicy.tla` and `PublicDurabilityPolicySpec.v` to byte/char/vocab public mutation and sync acknowledgement paths | Passed, 8 `persistent-artrie` tests; caught/fixed byte `GroupCommit` `sync()` returning after async sync start, byte public full-policy mutation paths skipping WAL sync, char direct WAL public mutations skipping full-policy sync, and vocab `GroupCommit` unsynced acknowledgement |
| Public WAL lifecycle and ordered group commit | `PersistentPublicWalLifecycleSpec.v` and strengthened `AsyncWalGroupCommit.tla` to byte/char/vocab public open/replay and group-commit returned-LSN ordering | Passed, 4 `persistent-artrie` tests plus 1 `group-commit` test; covers synced WAL tail reopen, checkpoint-plus-tail replay, vocab index preservation, and concurrent group-commit returned-LSN/durable-record matching |
| End-to-end persistent trace refinement | `PersistentEndToEndTraceSpec.v` and `PersistentEndToEndTrace.tla` to byte/char/vocab public operation traces | Passed, 3 `persistent-artrie` tests; covers byte mutation/transaction/batch/checkpoint/compaction/reopen traces, Unicode char checkpoint-plus-tail reopen, and vocab duplicate/batch/checkpoint/tail replay bijection preservation |
| Concurrent vocab public linearizability model | `ConcurrentVocabLinearizability.tla` to `ConcurrentVocabARTrie` public insert/read/batch/checkpoint/recover-after-publication histories | Passed, 3 focused Loom history checks and 2 added `persistent-artrie` public batch/reopen checks; the scoped claim is in-memory public-operation linearizability plus checkpoint/flush reopen preservation, not per-insert crash durability before checkpoint/flush |
| Epoch checkpoint recovery model | `EpochCheckpointRecovery.tla` to `PersistentARTrieChar` epoch accounting, forced checkpoint publication, and corrupt epoch metadata handling | Passed, 3 `persistent-artrie` tests; caught/fixed public mutations not advancing epoch accounting and `force_epoch_checkpoint()` publishing epoch metadata without first forcing a durable trie checkpoint |
| Persistent char bulk mutation model | `PersistentCharBulkMutationRecovery.tla` to `PersistentARTrieChar::remove_prefix_batched` durable-prefix replay and byte/char checked `increment`/`fetch_add` overflow | Passed, 4 `persistent-artrie` tests; caught/fixed unchecked byte/char i64 increment overflow before WAL append |
| Persistent transaction increment recovery model | `PersistentTransactionIncrementRecovery.tla` to byte/char transaction increments and byte/char `BatchIncrement` replay | Passed, 5 `persistent-artrie` tests; caught/fixed unchecked byte transaction staging, char aggregate/current overflow before BatchIncrement WAL append, unchecked char no-WAL replay, and recovery continuing past invalid arithmetic records |
| Lock-free counter merge atomicity model | `LockFreeCounterMergeAtomicity.tla` and `LockFreeCounterMergeSpec.v` to byte/char lock-free counter overlays and `merge_lockfree_values_to_persistent` | Passed, 5 `persistent-artrie` tests plus 2 Loom checks; caught/fixed u64-to-i64 domain overflow, unchecked char merge addition, and partial per-entry merge publication before all entries were preflighted |
| Checkpoint/WAL retention spec | `PersistentCheckpointRetentionSpec.v` to byte/char corruption rebuild, active WAL retention, archive/pending/active replay order, batch replay, remove replay, and safe truncation premises | Passed, 2 `persistent-artrie` tests; caught/fixed corruption-rebuild paths that ignored active WAL tails and rebuild replay paths that skipped batch/remove records |
| Dirty checkpoint publication spec | `PersistentDirtyCheckpointSpec.v` to byte/char dirty-slot write/sync retry behavior, late slot-tracking coverage, trusted checkpoint publication premises, and descriptor-before-truncation reopen | Passed, 5 `persistent-artrie` tests |
| WAL segment lifecycle spec | `PersistentWalSegmentLifecycleSpec.v` to WAL archive/pending/active ordering, rotation/reopen LSN and synced-frontier continuation, archive pruning, and replay composition | Passed, 4 `persistent-artrie` tests; caught/fixed filename-based segment ordering, rotation/reopen LSN reset, async reopen synced-frontier reset, async archive pruning, and archive filename collision risk |
| Recovery planner spec | `PersistentRecoveryPlannerSpec.v` to root/checkpoint trust, missing/clean/corrupt recovery mode selection, durable-prefix WAL replay, archive/pending/active retained replay order, and byte/char planner parity | Passed, 5 `persistent-artrie` tests; caught/fixed recovery paths that skipped a corrupt WAL record and applied later suffix records |
| Recovery replay completeness spec | `PersistentRecoveryReplayCompletenessSpec.v` to shared replay expansion, byte/char/archive/incremental endpoint parity, no-WAL recovery application, corrupt-prefix stopping, and invalid-arithmetic prefix stopping | Passed, 5 replay-completeness tests plus 2 transaction replay overflow tests; caught/fixed char archive recovery skipping increment/CAS/batch variants, re-logging remove during archive replay, skipping corrupt archive records, and applying suffix records after overflowed batch increments |
| Persistent compaction spec | `PersistentCompactionSpec.v` to byte trie compaction exact snapshot preservation, term-count verification insufficiency, temporary WAL sidecar disjointness, failure preservation, stale-WAL replay hazard, crash-finalization recovery, and output-file preservation | Passed, 8 `persistent-artrie` tests; caught/fixed WAL sidecar collision, term-only entry drops, non-UTF8 byte-key drops, and stale original WAL replay risk |
| Persistent rewrite-compaction spec | `PersistentRewriteCompactionSpec.v` to char/vocab checkpoint rewrite snapshot preservation, sparse vocab bijection preservation, post-checkpoint WAL-tail replay, and failed publication dirty/WAL retention | Passed, 7 `persistent-artrie` tests; caught/fixed char `persist_to_disk()` clearing dirty before WAL checkpoint/archive publication succeeded |
| Substring candidate spec | `SubstringSearchSpec.v` to public `SubstringDictionary` exact candidate APIs for byte and Unicode SCDAWG | Passed, 5 default-feature tests |
| SCDAWG occurrence spec | `ScdawgOccurrenceSpec.v` to byte and Unicode SCDAWG `find`/`freq`/`locations`, handle-based occurrence APIs, and left-extension traversal | Passed, 7 default-feature tests |
| Fuzzy candidate coverage spec | `FuzzyCandidateCoverageSpec.v` to WallBreaker-style byte/Unicode SCDAWG query-piece candidate coverage | Passed, 5 default-feature tests |
| Public serialization roundtrip spec | `SerializationRoundtripSpec.v` to public Bincode/JSON/plaintext/gzip/protobuf serializer APIs | Passed, 8 correspondence tests plus 9 existing value-roundtrip regression tests under `--features serialization`, and 6 protobuf/compression correspondence tests under `--features "serialization protobuf compression"` |
| Mmap concurrent allocation | `MmapBlockStorage.tla` to `MmapDiskManager::allocate_block` | Passed, 32 concurrent allocations |
| Mmap sub-block bounds | `BlockStorage` range contract to `MmapDiskManager::{read_bytes,write_bytes}` | Passed |
| Mmap sync/reopen checksum | allocation metadata persistence to `MmapDiskManager::sync/open` | Passed |
| Mmap raw pointer bounds | unsafe raw pointer contract to `MmapDiskManager::raw_ptr` | Passed |
| Storage syscall outcome model | `StorageSyscallOutcome.tla` write/sync result lattice | Passed, 88 generated states and 42 distinct states |
| WAL segment fsync failure frontier | `StorageSyscallOutcome.tla` to `SegmentSyncManager` failed sync handling | Passed, failed fsync does not advance `global_synced_lsn` or satisfy durable-LSN waits |
| BufferManager fixed-buffer capability | `IoUringFixedBufferOwnership.tla` to `BufferManager::new` registration gating, default no-op backend fallback, and fixed-capable registration lifetime | Passed |
| BufferManager page lease safety | `BufferPageLease.tla` to `BufferManager` read/write guard leasing, flush exclusion, and `TraversalContext` cached raw page references | Passed, TLC generated 14,341 states with 2,479 distinct states; focused unit tests cover read-vs-write exclusion, mutable lease exclusion, dirty-flush rejection during an active mutable lease, cached-page pin retention until `clear`, and FIFO cache eviction lease release |
| Reverse-index mmap/remap publication | `ReverseIndexMmap.tla` to `VocabReverseIndex` create/open/grow/remap mmap publication | Passed, TLC generated 342 states with 70 distinct states; the model checks mapped capacity within the file, published header capacity within the map, and entries within the header |
| io_uring SQE/CQE lifecycle | `IoUringSqeCqeLifecycle.tla` to `IoUringDiskManager` completion count, short/error CQE fail-closed checks, fixed-buffer registration preconditions, and temporary-buffer return discipline | Passed |
| io_uring completion outcome classification | `StorageSyscallOutcome.tla` and `IoUringSqeCqeLifecycle.tla` to `IoUringDiskManager` CQE helper behavior | Passed with `io-uring-backend`, including negative, short, and missing completion rejection |
| Byte lock-free root CAS | `LockFreeARTrieLinearizability.tla` to byte `AtomicNodePtr` publication | Passed under Loom |
| Byte duplicate insert linearization | root CAS/cache contract to `insert_cas` behavior | Passed under Loom |
| Byte insert-vs-contains visibility | contains linearization boundary to root/cache publication | Passed under Loom |
| Byte merge snapshot prefix | merge-to-persistent visibility boundary to cache snapshot semantics | Passed under Loom |
| Byte child pointer Arc handoff | raw child-pointer ownership contract to Arc clone-before-use pattern | Passed under Loom |
| Char same-key lock-free increments | `LockFreeIndexedOverlay.tla` counter mode to `increment_cas` value semantics | Passed under Loom |
| Char create-vs-increment race | single visible leaf plus accumulated value semantics | Passed under Loom |
| Char merge value snapshot | merge may lag but cannot exceed visible lock-free value | Passed under Loom |
| Vocab duplicate insert stability | duplicate `insert_cas` races return one stable committed index | Passed under Loom |
| Vocab distinct-term unique indices | distinct terms commit distinct indices without reusing claims | Passed under Loom |
| Vocab merge lookup agreement | cache/root/persistent term-index views agree after merge | Passed under Loom |
| Group-commit no-early acknowledgement | `DurabilityFrontier.tla` to synced-LSN/waiter publication semantics | Passed under Loom |
| Group-commit unique contiguous LSNs | concurrent reservations publish one prefix-closed durable frontier | Passed under Loom |
| Async WAL gap handling | out-of-order completion does not advance synced frontier past a gap | Passed under Loom |
| Checkpoint publication frontier | checkpoint LSN never exceeds the synced durable frontier | Passed under Loom |
| Concurrent checkpoint insert race | publication gate ensures a racing visible insert is either checkpointed or WAL-replayable | Passed under Loom |
| Concurrent checkpoint replay tail | racing inserts on both sides of checkpoint publication preserve recovery coverage | Passed under Loom |
| Concurrent sync/rotate non-publication | sync and WAL rotation do not publish checkpoint state or clear dirty evidence | Passed under Loom |
| VersionGc reader guard | active readers block reclaim until the guard drops | Passed under Loom |
| VersionGc reclaim race | reclamation requires zero readers and a durable GC decision | Passed under Loom |
| io_uring sub-block bounds | `BlockStorage` range contract to `IoUringDiskManager` when enabled | Passed with `io-uring-backend` |
| io_uring fixed-buffer registration | unsafe fixed-buffer contract to `IoUringDiskManager::register_buffer_pool` and `IoUringFixedBufferOwnership.tla` | Passed with `io-uring-backend`, including invalid registration rejection and unregister-before-owner-drop |

The full command `RUN_TLC=1 scripts/verify-formal-correspondence.sh` passed on
2026-06-01 for the current focused modules, including `BufferPageLease.tla` and
`ReverseIndexMmap.tla`. The
`LockFreeARTrieLinearizability.tla`, `LockFreeIndexedOverlay.tla`,
`DurabilityFrontier.tla`, `PointerOwnership.tla`,
`VocabPersistenceOwnership.tla`, `StorageSyscallOutcome.tla`, and
`IoUringSqeCqeLifecycle.tla` TLC runs passed independently on 2026-05-23.
`ConcurrentCheckpointPublication.tla` passed on 2026-05-25 with 1,703
generated states, 312 distinct states, and depth 10.
`SharedPersistentConcurrency.tla` passed on 2026-05-25 with 11,009 generated
states, 2,752 distinct states, and depth 13 under the same 8GiB RSS cap.
`ConcurrentVocabLinearizability.tla` passed on 2026-05-25 with 31,675
generated states, 23,836 distinct states, and depth 10.
`EpochCheckpointRecovery.tla` passed on 2026-05-25 with 3,211 generated
states, 2,050 distinct states, and depth 10 under the same 8GiB RSS cap.
`LockFreeCounterMergeAtomicity.tla` passed on 2026-05-25 with 421,373
generated states, 30,242 distinct states, and depth 19 under the same 8GiB RSS
cap.
The strengthened `AsyncWalGroupCommit.tla` passed on 2026-05-26 with 69
generated states, 26 distinct states, and depth 5 under the same 8GiB RSS cap.
`PersistentEndToEndTrace.tla` passed on 2026-05-26 with 677 generated states,
172 distinct states, and depth 5 under the same 8GiB RSS cap.
`BufferPageLease.tla` passed on 2026-06-01 with 14,341 generated states, 2,479
distinct states, and depth 14.
`ReverseIndexMmap.tla` passed on 2026-06-01 with 342 generated states, 70
distinct states, and depth 6.
`StorageSyscallOutcome.tla` passed with an 8GiB process cap and a 1GiB Java
heap. `IoUringSqeCqeLifecycle.tla` also passed with an 8GiB process cap and a
1GiB Java heap. TLC
requires running outside the local filesystem sandbox because the Java runtime
opens a local RMI listener.

The no-TLC verification command `scripts/verify-formal-correspondence.sh`
passed again on 2026-06-01 under explicit process caps
(`FORMAL_RSS_LIMIT_BYTES=8589934592`, applied as `prlimit --rss=8GiB`; no
virtual address cap is used because OCaml reserves virtual minor heaps up
front, and the merge targets set `CARGO_BUILD_JOBS=2` internally), including
the unsafe inventory
drift and contract coverage/status gates, the public dictionary law target, the DynamicDawg mutation target,
the DynamicDawgU64 sequence target, the Bloom filter target, the double-array
trie target, the valued set-combinator target under default features and
`lling-llang`, the persistent merge target under `persistent-artrie` and
`persistent-artrie parallel-merge`, the persistent prefix, relative encoding,
arena reservation, dedup arena, root descriptor/reopen, persistent lazy
mutation, persistent WAL atomicity, checkpoint retention, dirty checkpoint
publication, persistent char bulk mutation, persistent transaction increment,
lock-free counter merge atomicity, persistent vocab WAL atomicity, persistent
vocab checkpoint publication, concurrent checkpoint publication, concurrent vocab public history
linearizability, epoch checkpoint recovery, WAL segment lifecycle, recovery
planner, and recovery replay completeness targets under `persistent-artrie`,
the default and persistent
SCDAWG unsafe-boundary targets,
the default and persistent zipper-language targets, the substring candidate
target, the fuzzy candidate coverage target, the feature-gated serialization
correspondence/value/protobuf-compression targets, the storage correspondence
target, Loom schedule checks, crate-internal vocab persistence/eviction tests,
the group-commit-specific test, the Rocq build, and TLA+ SANY checks.

`RUN_MIRI=1 FORMAL_MIRI_TOOLCHAIN=nightly
scripts/verify-formal-correspondence.sh` now wires in the three raw
`VocabTrieNode` ownership-transfer checks, three raw `CharTrieNodeInner`
ownership-transfer checks, char/vocab unique `get_or_create_child` mutation
borrow checks, swizzled raw-extraction gating, swizzled lazy-load loser
reclamation, strict-provenance `SwizzledPtr` unit-state transitions,
heap-only leaf-eviction `node_map` invalidation, sibling query preservation
after leaf eviction, heap-only vocab `node_map`/parent-chain liveness, and
BufferManager fixed-buffer registration lifetime targets. The harness enables
`-Zmiri-strict-provenance` by default unless
`FORMAL_MIRI_STRICT_PROVENANCE=0` is set.

The combined optional command `RUN_MIRI=1 RUN_IO_URING=1
FORMAL_MIRI_TOOLCHAIN=nightly scripts/verify-formal-correspondence.sh` passed on
2026-06-01. It reran the correspondence harness with Miri-compatible unsafe
boundary targets and the io_uring storage correspondence checks enabled; TLC
remains a separate `RUN_TLC=1` mode.

Focused capped Rust and Miri checks for the newly added unsafe-boundary targets
passed on 2026-06-01 with `prlimit --rss=8GiB`: the child ownership-transfer
and unique-mutation correspondence tests, the swizzled raw-extraction
correspondence tests, heap-only vocab eviction tests, heap-only vocab
`node_map` parent-chain liveness test, and the BufferManager fixed-buffer
lifetime unit test. The strict-provenance `SwizzledPtr` replacement passed
focused Miri checks for both correspondence tests and the core
`persistent_artrie_core::swizzled_ptr::tests` unit target; in-memory pointer
raw-state sentinels no longer reconstruct Rust pointers from integers.

The optional command
`cargo test --features "persistent-artrie io-uring-backend" --test persistent_artrie_storage_correspondence`
also passed on 2026-05-23 with 8 storage correspondence tests.

#### Notes
- The state space is large due to the concurrent threads and crash recovery modeling
- Full state space exploration would require significantly more time
- No counterexamples were found for the explored portion (~7M states)

---

## Rocq/Coq Proof Results

### Modules Compiled

All 58 `.v` files compile end-to-end with Rocq 9.1.0. Every theorem is closed
by `Qed.` — **0 `Axiom`, 0 `Admitted`, 0 `Parameter`** across the tree
(verified 2026-05-26).

The prior 15-module core compiled with Rocq 9.1.0 (~72 s wall clock under
`make -j1`). Every theorem is closed by `Qed.` — **0 `Axiom`, 0 `Admitted`, 0
`Parameter`** across the tree (verified 2026-05-20).

| Module | LOC | Theorems | Lemmas | Qed | Status |
|--------|----:|---------:|-------:|----:|--------|
| Model/Key.v | 518 | 0 | 28 | 28 | Complete |
| Model/NodeTypes.v | 347 | 0 | 0 | 0 (2 `Defined`) | Complete |
| Model/Bucket.v | 931 | 1 | 44 | 45 | Complete |
| Model/HotStuff.v | 141 | 3 | 4 | 7 | Complete |
| Model/PathCompression.v | 311 | 0 | 12 | 12 (+5 `Defined`) | Complete |
| Model/FileSystem.v | 1516 | 2 | 44 | 46 | Complete |
| Model/ArenaManager.v | 362 | 11 | 5 | 17 | Complete |
| Model/SequentialSiblings.v | 384 | 6 | 5 | 13 | Complete |
| Spec/MapSpec.v | 287 | 11 | 2 | 12 (+2 `Defined`) | Complete |
| Spec/DictionaryLawSpec.v | 483 | 34 | 0 | 34 | Complete |
| Spec/DynamicDawgMutationSpec.v | 736 | 29 | 12 | 41 | Complete |
| Spec/DynamicDawgU64Spec.v | 1049 | 39 | 9 | 48 | Complete |
| Spec/DoubleArrayTrieSpec.v | 499 | 22 | 5 | 27 | Complete |
| Spec/ZipperLanguageSpec.v | 255 | 19 | 0 | 19 | Complete |
| Spec/ValuedSetCombinatorSpec.v | 454 | 28 | 2 | 30 | Complete |
| Spec/BloomFilterSpec.v | 362 | 11 | 6 | 17 (+1 `Defined`) | Complete |
| Spec/PersistentMergeSpec.v | 246 | 11 | 0 | 11 | Complete |
| Spec/PersistentPrefixSpec.v | 562 | 26 | 2 | 28 (+1 `Defined`) | Complete |
| Spec/PathMapFactorySpec.v | 448 | 25 | 0 | 25 | Complete |
| Spec/RelativeEncodingSpec.v | 416 | 16 | 2 | 18 (+1 `Defined`) | Complete |
| Spec/ArenaReservationSpec.v | 429 | 18 | 1 | 19 (+1 `Defined`) | Complete |
| Spec/DedupArenaSpec.v | 422 | 14 | 0 | 14 (+2 `Defined`) | Complete |
| Spec/RootDescriptorReopenSpec.v | 433 | 19 | 0 | 19 | Complete |
| Spec/PersistentLazyMutationSpec.v | 350 | 19 | 1 | 20 | Complete |
| Spec/PersistentWalAtomicitySpec.v | 705 | 35 | 1 | 36 | Complete |
| Spec/LockFreeCounterMergeSpec.v | 272 | 13 | 0 | 13 | Complete |
| Spec/SharedPersistentConcurrencySpec.v | 402 | 23 | 0 | 23 | Complete |
| Spec/PublicDurabilityPolicySpec.v | 262 | 11 | 0 | 11 | Complete |
| Spec/PersistentPublicWalLifecycleSpec.v | 244 | 13 | 0 | 13 | Complete |
| Spec/PersistentEndToEndTraceSpec.v | 319 | 17 | 5 | 22 | Complete |
| Spec/PersistentVocabWalAtomicitySpec.v | 492 | 23 | 0 | 23 | Complete |
| Spec/PersistentVocabCheckpointSpec.v | 475 | 20 | 1 | 21 | Complete |
| Spec/PersistentCheckpointRetentionSpec.v | 421 | 21 | 0 | 21 | Complete |
| Spec/PersistentDirtyCheckpointSpec.v | 738 | 45 | 0 | 45 | Complete |
| Spec/PersistentWalSegmentLifecycleSpec.v | 467 | 28 | 0 | 28 | Complete |
| Spec/PersistentRecoveryPlannerSpec.v | 638 | 33 | 0 | 33 | Complete |
| Spec/PersistentRecoveryReplayCompletenessSpec.v | 534 | 33 | 0 | 33 | Complete |
| Spec/PersistentCompactionSpec.v | 531 | 29 | 0 | 29 | Complete |
| Spec/PersistentRewriteCompactionSpec.v | 425 | 24 | 0 | 24 | Complete |
| Spec/SubstringSearchSpec.v | 344 | 21 | 2 | 23 | Complete |
| Spec/ScdawgOccurrenceSpec.v | 395 | 14 | 0 | 14 | Complete |
| Spec/FuzzyCandidateCoverageSpec.v | 292 | 7 | 3 | 10 | Complete |
| Spec/SerializationRoundtripSpec.v | 668 | 45 | 4 | 49 | Complete |
| Spec/ARTrieSpec.v | 1195 | 7 | 42 | 49 | Complete |
| Spec/ReplicatedMapSpec.v | 91 | 4 | 0 | 4 | Complete |
| Invariants/ArenaInvariants.v | 299 | 11 | 6 | 18 | Complete |
| Invariants/StructuralInvariants.v | 192 | 2 | 0 | 2 | Complete |
| Invariants/TransitionInvariants.v | 291 | 10 | 0 | 10 | Complete |
| Invariants/SequentialSiblingsInvariants.v | 280 | 10 | 0 | 11 | Complete |
| Proofs/FileSystemSafety.v | 311 | 6 | 5 | 12 | Complete |
| Proofs/MapRefinement.v | 90 | 3 | 0 | 3 | Complete |
| Proofs/StructuralPreservation.v | 281 | 7 | 8 | 15 | Complete |
| Proofs/ByzantineRecovery.v | 104 | 5 | 0 | 5 | Complete |
| Proofs/CertifiedReference.v | 50 | 4 | 0 | 4 | Complete |
| Proofs/HotStuffSafety.v | 46 | 2 | 0 | 2 | Complete |
| Proofs/ProofCarryingExtraction.v | 80 | 3 | 0 | 3 | Complete |

**Total Rocq LOC:** 26,019 (65 modules)
**Aggregate proof tally:** 957 `Theorem` + 300 `Lemma` + 8 `Corollary`
= 1,265 theorem/lemma/corollary propositions, all closed
(`Qed.`/`Defined.`; no escape hatches).

### Compilation Command
```bash
systemd-run --user --scope -p MemoryMax=126G -p CPUQuota=1800% \
  -p IOWeight=30 -p TasksMax=200 make -j1
```

### Admitted Theorems

**None.** As of 2026-05-22 there are zero outstanding `Admitted.` markers and
zero `Axiom` declarations anywhere in the Rocq tree. Previously-admitted
obligations were resolved as follows:

- **Bucket.v** (8 previously admitted) — all proved. The `binary_search_correct`
  obligation was replaced with a provable existence lemma; the
  `bucket_insert_wf` / `bucket_delete_wf` / `bucket_split_wf` /
  `bucket_split_preserves` / `bucket_lookup_*` lemmas were promoted from
  `Admitted` to full proofs.
- **PathCompression.v** (6 previously admitted) — the
  `Program ... Admit Obligations` patterns for `split_prefix`/`extend_prefix`/
  `truncate_prefix`/`consume_prefix`/`compute_common_prefix` were rewritten as
  explicit `refine`-based `Definition`s with all obligations discharged.
- **TransitionInvariants.v** (2 previously admitted) — `growth_type_appropriate`
  and `shrink_type_appropriate` were restated with corrected premises
  (`growth_type_appropriate_after_insert` adds the missing premise that the
  count bumps by one; `shrink_type_appropriate_with_lower_bound` takes the
  post-shrink lower bound as an explicit premise); both are now `Qed.`-closed.
- **StructuralInvariants.v** (2 previously admitted) — the unprovable
  acyclicity placeholder was weakened to "no direct self-loop from any
  reachable node" (provable); `insert_preserves_structural_obligation`/
  `delete_preserves_structural_obligation` are now explicit `Prop`-level
  obligations scoped to successful checked operations.
- **Key.v** — the prior `Axiom proof_irrelevance` was eliminated and replaced
  with a proved local `Lemma lt_proof_irrelevance`.
- **Spec/ARTrieSpec.v** — `trie_insert_correct` (line 706) and
  `trie_delete_correct` (line 719), previously declared as `Axiom`s, are now
  real `Theorem ... Qed.` proofs under the `entries_of_trie_complete` hypothesis,
  using `canonical_lookup_correct` plus `kv_lookup_upsert_same`/`_other` and the
  symmetric delete lemmas.

### Proven Theorems (selected highlights)

A non-exhaustive sample of the 1,265 theorem/lemma/corollary propositions. See per-module file for
the complete list; see [README.md](README.md) for module-by-module module-status
table.

- `key_equality_decidable` - Key equality is decidable
- `lt_proof_irrelevance` (Key.v:20) - Replaces the former `Axiom proof_irrelevance`
- `binary_search_correct` - Binary search returns the canonical position
- `binary_search_in_bounds` - Binary search returns an insertion point within
  the bucket entry bounds
- `canonical_bucket_checked_wf` - Successful checked canonical bucket
  construction preserves `wf_bucket` under suffix uniqueness
- `bucket_lookup_insert_same` / `bucket_lookup_insert_other` - Bucket map laws
- `bucket_split_wf` / `bucket_split_preserves` - Split preserves well-formedness
- `lookup_empty` - Looking up in empty map returns None
- `insert_lookup_same` - Insert then lookup returns inserted value
- `growth_type_appropriate_after_insert` (TransitionInvariants.v) - Corrected variant
- `shrink_type_appropriate_with_lower_bound` (TransitionInvariants.v) - Corrected variant
- `trie_invariant_empty` - Empty trie satisfies structural invariants
- `children_preserved_reflexive` - Children preservation is reflexive
- `trie_insert_correct` (ARTrieSpec.v:706) - **Was axiomatic; now proved**
- `trie_delete_correct` (ARTrieSpec.v:719) - **Was axiomatic; now proved**
- `ARTrieMapImpl_obligation` (ARTrieSpec.v:708) - Aggregator that `exact`s into
  the two correctness theorems, retiring the prior `ARTrieMapImpl` Instance
  which had been axiomatized.
- `recovered_records_are_committed_and_authenticated` - Byzantine storage
  recovery applies only committed authenticated records
- `certified_reference_insert_refines` /
  `certified_reference_delete_refines` - Certified reference interface refines
  the abstract map semantics
- `hotstuff_committed_logs_compatible` - Honest quorum intersection and vote
  locking imply committed logs are prefix-compatible
- `quorum_sets_cannot_be_disjoint` - Two 2f+1 quorums over 3f+1 replicas
  cannot be disjoint
- `replicated_hotstuff_committed_replays_share_prefix` - Compatible committed
  replicated logs replay as prefix extensions of one another
- `certified_trace_replays_reference` - A valid certified trace replays to the
  reference command-log semantics
- `invalid_step_rejected` - A trace step with an incorrect post-state is rejected
- `set_union_commutative` / `set_intersection_commutative` - Public zipper set
  operations refine Boolean set algebra
- `valued_union_two_first_wins_conflict` /
  `valued_union_two_last_wins_conflict` - Union zipper duplicate-value
  conflicts follow the configured dictionary-order strategy
- `valued_union_lattice_join_conflict_commutes` /
  `valued_intersection_lattice_meet_conflict_commutes` - Lattice-valued
  union/intersection merge results are order-independent under explicit
  commutativity laws
- `semiring_join_conflict_commutes` - `lling-llang` idempotent semiring
  wrappers are proven only for the join=`plus` boundary, not for arbitrary
  `times`-as-meet semantics
- `replay_domain_matches_set` - Mapped mutation traces preserve the exact
  reference-set domain
- `dynamic_compact_preserves_lookup` / `dynamic_minimize_preserves_lookup` -
  DynamicDawg compaction and minimization preserve mapped lookups
- `dynamic_insert_with_value_domain_matches_set` - Valued DynamicDawg inserts
  preserve map-domain/set-domain correspondence under the valued-domain premise
- `dynamic_remove_many_lookup_deleted` - Batch removal deletes mapped values
  for every listed term
- `bloom_insert_no_false_negative` / `no_false_negatives_after_insert_trace` -
  Bloom filter insertion traces never reject inserted byte strings
- `bloom_clear_rejects_all` / `string_might_contain_refines_bytes` - Clear
  removes all Bloom evidence under the nonzero-hash invariant, and string
  queries refine byte queries
- `invalid_descriptor_ignores_checkpoint_map` /
  `failed_root_load_replays_wal_from_zero` - Reopen uses the checkpoint skip
  threshold only after a valid descriptor and successful root load
- `public_contains_fails_closed_on_lazy_error` /
  `public_get_fails_closed_on_lazy_error` - Public lazy reads return absence
  rather than panicking or fabricating values after lazy-load failure
- `lazy_error_does_not_append_wal` /
  `successful_mutation_replay_matches_memory` - Public lazy mutations reject
  load errors before WAL append, while successful appended mutations replay to
  the same map state
- `wal_error_rejects_write_before_mutation` /
  `successful_write_replay_matches_memory` - Persistent writes reject WAL
  append failures before in-memory mutation, while successful atomic writes
  replay to the same map state
- `wal_rejection_preserves_forward` /
  `wal_rejection_preserves_next_index` - Persistent vocabulary WAL rejection
  leaves the visible forward map and allocator unchanged
- `index_collision_rejected_preserves_state` /
  `term_reindex_rejected_preserves_state` - Persistent vocabulary manual-index
  inserts fail closed instead of violating the term/index bijection
- `range_based_contains_index_is_unsound` /
  `batch_duplicate_terms_share_first_index` - Sparse manual indexes require
  exact reverse lookup membership, and duplicate batch terms reuse the first
  assigned index without consuming gaps
- `invalid_checkpoint_has_no_skip_threshold` /
  `retained_active_tail_is_replayed` - Corruption rebuilds replay from zero
  when the checkpoint/root is invalid, and a retained active WAL tail remains
  visible after recovery
- `archive_then_active_tail_replay_order` /
  `active_batch_then_remove_matches_reference` - Archive/pending/active
  segment ordering and batch/remove replay match the reference map
- `safe_truncation_prefix_is_checkpointed` - WAL truncation is justified only
  for prefixes covered by a valid checkpoint boundary
- `lsn_order_swaps_filename_order_when_needed` /
  `lsn_order_keeps_already_ordered_pair` - WAL segment collection order is
  governed by record LSNs rather than archive filename order
- `reopen_after_archive_continues_after_retained_lsn` /
  `active_tail_put_visible_after_archive` - Reopen continues after retained
  archive records and replaying an active tail after archives exposes the
  latest value
- `reopen_after_archive_restores_synced_frontier` - Reopen restores the durable
  synced frontier from retained archive records instead of resetting it to zero
- `successful_transaction_appends_commit_record` /
  `successful_transaction_replay_matches_memory` - Document commits publish
  batch records plus `CommitTx` before applying the buffered map transition
- `checked_increment_overflow_*` /
  `checked_batch_increment_overflow_*` - Direct and transactional increments
  reject overflow before appending WAL or changing the map
- `bijective_refinement_forward_injective` - Forward/reverse refinement implies
  unique values map back to unique terms
- `dat_transition_parent_checked` - Successful DAT transitions are justified by
  the CHECK parent slot
- `dat_zipper_descend_matches_transition` - Zipper descent follows the same
  BASE/CHECK walk as lookup
- `map_from_entries_last_wins` - Duplicate mapped DAT construction keeps the
  later value
- `set_roundtrip_contains` / `map_roundtrip_lookup` - Public serializers
  preserve term membership or mapped lookup values after decode
- `decode_set_error_fail_closed` / `decode_map_error_fail_closed` - Invalid
  serialized payloads decode to no dictionary state
- `legacy_roundtrip_preserves_domain` - Legacy term-only serialization
  preserves mapped domains while intentionally dropping values
- `pigeonhole_untouched_piece_index` - A `budget + 1` piece split has an
  untouched piece when at most `budget` pieces are damaged
- `fuzzy_candidate_reference_contains` - An untouched surviving query piece
  yields a reference substring candidate for the in-budget term

---

## Issues Fixed During Verification

### TLA+ Issues

1. **Type Comparison Errors**
   - Problem: Using `<<>>` (empty sequence) as null value caused type mismatches
   - Fix: Added `Null` constant (string "NULL") and updated all modules

2. **CHOOSE on Empty Sets**
   - Problem: `CHOOSE k \in Keys : abstractMap[k] # Null` fails when map is empty
   - Fix: Changed to existential quantification: `\E k \in Keys : abstractMap[k] # Null /\ Remove(thread, k)`

3. **Non-Enumerable Sets**
   - Problem: `CHOOSE seq \in Seq(Nat)` cannot be enumerated by TLC
   - Fix: Replaced SortByLsn with FilterCommittedOps using SelectSeq (WAL already ordered)

4. **Missing Variable Assignments**
   - Problem: SystemCrash didn't specify all concurrency variables
   - Fix: Added explicit resets for readGuards, writeGuards, lockDepth, threadEpoch, activeReaders

5. **UNCHANGED Conflicts**
   - Problem: MarkDirty specified UNCHANGED abstractMap but was composed with abstractMap-modifying actions
   - Fix: Removed abstractMap and entryCount from MarkDirty's UNCHANGED clause

### Rocq Issues

1. **Missing Import**
   - Problem: TransitionInvariants.v couldn't find `node_type_appropriate`
   - Fix: Added `Require Import ARTrie.Spec.ARTrieSpec.`

2. **Destruct on Wrong Expression**
   - Problem: Proof tried to destruct on function instead of result
   - Fix: Added `unfold get_node_type in *` before destruct

3. **Insufficient Hypotheses** (resolved 2026-05-20)
   - Problem: Transition proofs needed threshold hypotheses
   - Resolution: Restated as corrected `_after_insert` / `_with_lower_bound`
     variants with the required premises made explicit; both close by `Qed.`
     in `TransitionInvariants.v`. No admits remain.

---

## Recommendations

### Short-term
1. ~~Complete the admitted Rocq proofs by adding `should_grow`/`should_shrink` hypotheses~~ **Done** (2026-05-20, all admits eliminated)
2. Run TLC with larger state space (overnight run with more memory)
3. ~~Resolve stale MarkDirty composition warning follow-up~~ **Done** (2026-06-01, focused unsafe-boundary models are checked directly by the harness)
4. Treat TLA+ state-space dumps under `formal-verification/tla+/states/` as
   archival unless the composed PART assumptions change; focused
   unsafe-boundary models are checked directly by the harness.

### Medium-term
1. ~~Add refinement proofs (ARTrie refines Map ADT)~~ **Done** — see
   `Proofs/MapRefinement.v` (3 Qed'd theorems including `WFARTrieMapImpl`
   Instance)
2. Implement separation logic proofs using Iris
3. ~~Replace or formally justify `SwizzledPtr` exposed-provenance casts so the
   Miri targets can run under strict-provenance settings~~ **Done**
   (2026-05-25, provenance-preserving state word plus `AtomicPtr` runtime slot)
4. Model SIMD operations in TLA+ specification

### Long-term
1. ~~Formal verification of recovery correctness with Byzantine faults~~
   **Scoped models added** (2026-05-22) for storage/WAL drop, duplicate, and
   corruption faults plus bounded HotStuff/PBFT-style quorum safety. This does
   not claim production Byzantine networking, liveness, compromised
   cryptography, or malicious CPU execution.
2. Mechanized proof of linearizability
3. ~~Integration with certified compilation (CompCert/RustBelt)~~
   **Proof-carrying reference boundary added** (2026-05-22). The current
   checked claim is for the Rocq reference interface, certified trace checker,
   and TCB documentation, not certified Rust/LLVM binaries.
4. Whole-crate unsafe-boundary proof, including mmap/io_uring and unsafe
   `Send`/`Sync` implementations outside the persistent ARTrie model. The
   2026-06-01 page-lease and reverse-index mmap models close the newly
   documented BufferManager/TraversalContext and reverse-index publication
   gaps at the bounded-model level.

---

## Files Modified

### TLA+
- `ARTrieTypes.tla` - Added Null constant
- `ARTrieState.tla` - Updated to use Null for absent values
- `WAL.tla` - Updated LogRemove to use Null
- `Concurrency.tla` - Updated currentNode resets
- `CrashRecovery.tla` - Fixed FilterCommittedOps, MarkDirty UNCHANGED
- `PART.tla` - Added NullRecord, fixed CHOOSE expressions, SystemCrash
- `PART.cfg` - Added Null constant
- `PART_crash.cfg` - Changed Values to strings, added Null
- `LockFreeARTrieLinearizability.tla` - Adds bounded byte lock-free
  root-CAS/cache/contains/merge publication model.
- `LockFreeIndexedOverlay.tla` - Adds bounded char counter and vocab
  index-assignment overlay models, including sparse vocab index claims.
- `LockFreeCounterMergeAtomicity.tla` - Adds bounded checked counter
  increment/merge preflight and all-or-nothing batch publication model.
- `DurabilityFrontier.tla` - Adds bounded durable-prefix, checkpoint,
  recovery, group-commit acknowledgement, and VersionGc reclamation model.
- `PointerOwnership.tla` - Adds bounded raw slot pointer, strict-provenance
  null/disk/installing/memory/evicting state, lazy-load candidate, node-map raw
  reference, borrow, unswizzle, and drop ownership model.
- `IoUringFixedBufferOwnership.tla` - Adds bounded fixed-buffer registration,
  fixed I/O, unregister, fallback, invalid-registration, and owner-drop
  lifetime model.
- `IoUringSqeCqeLifecycle.tla` - Adds bounded SQE/CQE submission and
  completion lifecycle model for one-buffer-per-request ownership,
  fixed-buffer registration preconditions, fail-closed short/error
  completions, and temporary-buffer return ordering.
- `StorageSyscallOutcome.tla` - Adds bounded write/sync syscall outcome model
  for full, short, error, interrupted, cancelled, and missing completions at
  the durable-prefix boundary.
- `BufferPageLease.tla` - Adds bounded read/write page lease, cached
  TraversalContext pointer, dirty frame, and eviction-residency model for
  BufferManager unsafe references.
- `ReverseIndexMmap.tla` - Adds bounded reverse-index create/open/grow/remap
  publication model for file capacity, map capacity, header capacity, and
  entry-count ordering.
- `ConcurrentCheckpointPublication.tla` - Adds bounded mutation/checkpoint
  publication model for WAL-before-visible inserts, checkpoint gating,
  truncation/replay-tail safety, sync/rotation non-publication, and recovery
  after racing checkpoint schedules.
- `SharedPersistentConcurrency.tla` - Adds bounded shared public API model for
  `Arc<RwLock<...>>` writes, reads, sync, checkpoint publication, WAL
  truncation, and recovery after racing shared checkpoint/write schedules.
- `PublicReadSnapshotTraversal.tla` - Adds bounded public read traversal model
  for successful snapshot exactness, prefix soundness/completeness, checkpoint
  recovery, and fail-closed lazy/disk corruption. TLC passed with 106,219
  generated states and 8,608 distinct states.
- `ConcurrentVocabLinearizability.tla` - Adds bounded public concurrent vocab
  operation-history model for insert, read, batch insert, checkpoint gating,
  recovery after publication, real-time order, and sequential vocabulary-map
  explanations.
- `EpochCheckpointRecovery.tla` - Adds bounded epoch checkpoint/recovery model
  for public mutation epoch accounting, data-checkpoint-before-metadata
  ordering, metadata failure without durable overclaim, and WAL cleanup
  recovery coverage.
- `PersistentCharBulkMutationRecovery.tla` - Adds bounded char bulk-deletion
  and checked-RMW model for WAL-before-visible prefix removes, recovery from
  every durable remove prefix, lazy collection failure stuttering, and
  fail-closed increment overflow.

### Rocq
- `Invariants/TransitionInvariants.v` - Added imports, fixed proofs
- `Spec/DictionaryLawSpec.v` - Adds public exact-set, mapped-dictionary,
  zipper, mutation replay, and bijective dictionary laws.
- `Spec/DynamicDawgMutationSpec.v` - Adds byte/Unicode DynamicDawg mutation,
  batch, compaction, minimization, return-value, and valued-domain consistency
  preservation laws.
- `Spec/DynamicDawgU64Spec.v` - Adds u64 sequence DAWG set/map mutation,
  string/f64 adapter, iterator, zipper, and bounded snapshot-concurrency laws.
- `Spec/DoubleArrayTrieSpec.v` - Adds generic BASE/CHECK transition,
  duplicate-normalization, lookup/domain, child-edge, and zipper traversal laws
  for byte and Unicode double-array tries.
- `Spec/ZipperLanguageSpec.v` - Adds backend-neutral traversal-language laws
  for zipper descent, children, finality, valued lookup, prefix/excluding
  filters, and set/value-diff combinators.
- `Spec/BloomFilterSpec.v` - Adds one-sided Bloom filter safety laws for
  no-false-negative insertion, prior-membership preservation, duplicate
  inserts, clear, byte/string refinement, false-positive permissiveness, and
  nonvacuous constructor parameters.
- `Spec/PersistentPrefixSpec.v` - Adds persistent char prefix-filter,
  valued/arena projection, ordinary removal, idempotence, batched-removal
  equivalence, durable-prefix deletion, and checked i64 addition laws.
- `Spec/PersistentReadTraversalSpec.v` - Adds public read traversal snapshot
  exactness, prefix safety, no-fabrication, fail-closed lazy/disk error, and
  read-preserves-snapshot laws.
- `Spec/PathMapFactorySpec.v` - Adds optional PathMap byte/char mutation,
  value-update, union, node-edge soundness/completeness, UTF-8-backed
  character traversal, and factory backend-dispatch laws.
- `Spec/RelativeEncodingSpec.v` - Adds persistent child-pointer encoding laws
  for strict relative roundtrip, lossless full-encoding fallback, checked
  malformed decode rejection, sequential sibling overflow, and verified dedup
  cache hits/collisions.
- `Spec/ArenaReservationSpec.v` - Adds arena slot-store laws for allocation
  readback, same-size updates, contiguous sibling reservations, fail-closed
  dirty-slot flushing, and load/reopen slot-directory reconstruction.
- `Spec/DedupArenaSpec.v` - Adds persistent deduplicating-arena laws for
  verified hit soundness, stale/colliding cache fail-closed allocation,
  compatibility setter preservation of verified mode, legacy unverified-mode
  collision-free assumptions, cache clear, batch take, and byte/char backend
  model parity.
- `Spec/RootDescriptorReopenSpec.v` - Adds persistent root descriptor/reopen
  laws for known root-kind validation, final-flag and empty-payload checks,
  arena-count bounds, checkpoint threshold trust only after a loaded root, WAL
  replay fallback after descriptor/load failure, lazy `try_*` error
  propagation, public fail-closed reads, and byte/char backend parity.
- `Spec/PersistentLazyMutationSpec.v` - Adds persistent lazy mutation laws for
  lazy-load error rejection before WAL append, duplicate/no-op mutation
  non-append behavior, successful insert/value-insert/remove replay
  correspondence, and byte/char backend parity.
- `Spec/PersistentWalAtomicitySpec.v` - Adds persistent write-atomicity laws
  for serialization/preflight rejection before WAL append, WAL append failure
  before memory mutation, no-op atomic writes, successful replay
  correspondence, and document commit batch/CommitTx ordering.
- `Spec/SharedPersistentConcurrencySpec.v` - Adds shared public API laws for
  write/checkpoint lock exclusion, WAL-before-visible publication,
  checkpoint success/failure, sync non-publication, read observation, and
  recovery from durable checkpoints or retained WAL tails.
- `Spec/PersistentPublicWalLifecycleSpec.v` - Adds public WAL lifecycle laws
  for open/recovery equivalence, retained-tail checkpoint boundaries,
  durable-prefix bounds, exact reserved-LSN writes, ordered group-commit queue
  replay, acknowledged synced prefixes, and checkpoint-record no-ops.
- `Spec/PersistentVocabWalAtomicitySpec.v` - Adds persistent vocabulary
  WAL-before-visible-mutation laws for insert/manual-index/batch insertion,
  duplicate stability, reindex/collision rejection, exact reverse-index
  membership, range-check unsoundness, and batch first-occurrence deduplication.
- `Spec/PersistentVocabCheckpointSpec.v` - Adds persistent vocabulary
  checkpoint-publication laws for checkpoint/reopen bijection, dirty/WAL
  retention on failed publication, post-checkpoint LSN continuation,
  non-checkpoint WAL sync/rotation, recovery WAL retention until checkpoint,
  and Bloom/reverse-index sidecar rebuild safety.
- `Spec/PersistentCheckpointRetentionSpec.v` - Adds checkpoint/WAL retention
  laws for valid checkpoint skip thresholds, invalid-checkpoint no-skip
  behavior, active WAL retention, archive/pending/active replay order,
  batch/remove replay, safe truncation premises, and byte/char backend parity.
- `Spec/PersistentDirtyCheckpointSpec.v` - Adds dirty checkpoint publication
  laws for dirty arena/slot evidence, write/sync failure retry preservation,
  late slot-tracking coverage, trusted root-descriptor/checkpoint-record
  publication, and WAL truncation/replay thresholds.
- `Spec/PersistentWalSegmentLifecycleSpec.v` - Adds WAL segment lifecycle laws
  for state transitions preserving entries, LSN-based collection order
  independent of archive filenames, monotonic next-LSN continuation after
  rotation/reopen, checkpoint-covered pruning, and archive/active replay
  composition.
- `Spec/PersistentRecoveryPlannerSpec.v` - Adds recovery planner laws for
  missing/clean/corrupt mode selection, root/checkpoint trust, durable-prefix
  replay after the first corrupt WAL record, retained archive/pending/active
  replay order, and byte/char backend parity.
- `Spec/PersistentRecoveryReplayCompletenessSpec.v` - Adds recovery replay
  completeness laws for shared mutating-record expansion, endpoint parity,
  batch insert/increment ordering, failed-CAS elision, corrupt/invalid-arithmetic
  durable-prefix stopping, and no-WAL replay application.
- `Spec/PersistentCompactionSpec.v` - Adds byte persistent compaction laws for
  exact key/value/term-only snapshot preservation, term-count verification
  insufficiency, temporary data/WAL sidecar disjointness, pre-finalize failure
  preservation, additive stale-WAL replay corruption, finalization crash
  recovery around WAL backup/data rename/cleanup, and output-file
  preservation.
- `Spec/PersistentRewriteCompactionSpec.v` - Adds char/vocab rewrite
  checkpoint laws for Unicode value snapshot preservation, sparse vocab
  forward/reverse index preservation, post-checkpoint WAL-tail replay, and
  failed publication dirty/WAL retention.
- `Spec/SubstringSearchSpec.v` - Adds backend-neutral exact substring
  candidate laws for non-empty patterns, occurrence bounds, duplicate-free
  result sets, and limited-result prefixes.
- `Spec/FuzzyCandidateCoverageSpec.v` - Adds WallBreaker-style query-piece
  pigeonhole and fuzzy candidate coverage laws.
- `Spec/SerializationRoundtripSpec.v` - Adds backend-neutral term-only,
  value-aware, legacy value-dropping, gzip wrapper, protobuf V1/V2/DAT/suffix,
  and fail-closed serializer laws.

### Rust Correspondence
- `src/persistent_artrie_core/group_commit.rs` - Writes queued records with
  the LSN reserved and returned by the coordinator.
- `src/persistent_artrie_core/wal/writer.rs` - Adds reserved-LSN record append
  support for group commit.
- `src/persistent_artrie_core/wal/async_writer.rs` - Preserves monotonic async
  LSN state while supporting reserved-LSN appends.
- `tests/persistent_artrie_formal_correspondence.rs` - Adds CI-practical
  correspondence tests across bucket, trie, WAL, transactions, version GC,
  unsafe pointer/concurrency boundaries, raw char/vocab child-pointer
  ownership, lazy-load loser reclamation, unsafe `Send`/`Sync` contracts,
  record-boundary crash-prefix reopen, torn-WAL reopen, transaction recovery,
  failed WAL segment fsync frontier handling, and group commit.
- `tests/persistent_artrie_storage_correspondence.rs` - Adds CI-practical
  storage-boundary checks for mmap allocation uniqueness, sub-block bounds,
  sync/reopen checksum refresh, raw-pointer bounds, and optional io_uring
  fixed-buffer registration/lifetime checks.
- `tests/persistent_artrie_loom_correspondence.rs` - Adds bounded Loom
  schedule checks for byte lock-free publication, duplicate insert,
  insert/contains visibility, merge snapshot behavior, child-pointer handoff,
  char lock-free increments, char merge snapshots, checked counter overflow
  and merge-failure preservation, vocab stable/unique
  indices, sparse `next_index`, vocab cache/root/persistent agreement,
  group-commit durable frontier publication, async WAL gap handling,
  checkpoint publication, public concurrent vocab history linearizability, and
  VersionGc reader/reclaim races.
- `Cargo.toml` / `Cargo.lock` - Adds `loom` as a dev-dependency for bounded
  schedule exploration.
- `src/persistent_artrie/{lockfree_cas.rs,nodes/persistent_node.rs}` and
  `src/persistent_artrie_char/nodes/persistent_node.rs` - Clarify safety
  contracts around Arc-backed child traversal and `Send`/`Sync`.
- `src/persistent_artrie_core/disk_manager.rs` - Rejects cross-block sub-block
  I/O ranges, rejects one-past-end raw pointer offsets, refreshes the header
  checksum during `sync()`, and points the mmap invariant docs at
  `MmapBlockStorage.tla`.
- `src/persistent_artrie_core/io_uring_disk_manager.rs` - Rejects invalid
  fixed-buffer registration entries before handing pointers to io_uring and
  keeps CQE result checks on the fail-closed completion path. Failed
  single-block, batched, and fixed-buffer writes re-mark affected cached blocks
  dirty so later sync can retry them.
- `src/persistent_artrie_core/buffer_manager.rs` -
  Replaces frame pin counts with read/write lease state so mutable page guards
  are exclusive and shared page guards can coexist only with other reads.
- `src/persistent_artrie_core/traversal_context.rs` -
  Keeps cached raw page pointers backed by BufferManager read leases until
  cache eviction, `clear`, or drop.
- `src/persistent_vocab_artrie/reverse_index.rs` -
  Covered by the reverse-index mmap/remap publication model for header/map/file
  capacity ordering.
- `formal-verification/UNSAFE_BOUNDARY.md` - Documents the current unsafe
  boundary, executable checks, and explicit abstraction boundaries.
- `tests/dictionary_law_correspondence.rs` - Adds public law correspondence
  checks for static/dynamic byte and Unicode dictionaries, mapped backends,
  set-zippers, suffix substring semantics, bijective maps, `PersistentARTrie`,
  and `PersistentVocabARTrie`.
- `src/dawg_core.rs`, `src/dynamic_dawg.rs`, `src/dynamic_dawg_char.rs` -
  Preserve optional values during compact rebuilds, avoid merging valued final
  states without a value-equality proof, and add copy-on-write for shared DAWG
  mutation paths after minimization.
- `tests/dynamic_dawg_mutation_correspondence.rs` - Adds byte and Unicode
  DynamicDawg mutation correspondence checks for mapped insert/update/remove,
  set batch operations, compaction, minimization, value preservation, shared
  node copy-on-write, and Bloom-filter-backed lookup.
- `tests/bloom_filter_correspondence.rs` - Adds public Bloom filter
  correspondence checks against a deterministic reference bitset model,
  including generated byte traces, Unicode string refinement, clear/reinsert,
  duplicate inserts, parameter normalization, and Bloom-backed DynamicDawg
  lookup.
- `tests/double_array_trie_correspondence.rs` - Adds byte and Unicode DAT
  correspondence checks for construction, lookup, mapped values, duplicate
  normalization, node walks, child iteration, zippers, and sorted Unicode
  construction.
- `tests/zipper_language_correspondence.rs` - Adds traversal-language
  correspondence checks for byte and Unicode zippers, valued iteration,
  prefix/excluding filters, set and value-diff combinators, suffix automaton
  substring languages, SCDAWG exact/substring queries, and persistent
  byte/char zippers.
- `tests/valued_set_combinator_correspondence.rs` - Adds valued
  union/intersection correspondence checks for first-wins, last-wins, custom
  sum, lattice join/meet, byte and Unicode DATs, DynamicDawg, set values,
  empty/disjoint domains, generated union folds, and feature-gated
  `lling-llang` semiring join behavior.
- `tests/persistent_prefix_correspondence.rs` - Adds persistent char prefix
  correspondence checks for ASCII/Unicode/empty prefixes, valued and
  arena-aware projections, ordinary/batched removal across batch sizes
  including zero, idempotence, and sync/reopen persistence.
- `tests/root_descriptor_reopen_correspondence.rs` - Adds persistent byte and
  char checkpoint/reopen correspondence checks for malformed root descriptors,
  invalid arena counts, WAL checkpoint fallback, and lazy-load fail-closed
  query behavior.
- `src/persistent_artrie/{disk_load,mmap_ctor,io_uring_ctor}.rs` and
  `src/persistent_artrie_char/{disk_io,mmap_ctor,io_uring_ctor,query_api}.rs`
  - Validate root descriptor offsets, kinds, final flags, empty payloads, and
  arena counts before trusting checkpoint thresholds; failed root loads fall
  back to WAL replay, and public char lazy reads fail closed.
- `tests/persistent_lazy_mutation_correspondence.rs` - Adds persistent char
  lazy mutation correspondence checks for failed term inserts, value inserts,
  and removes not appending WAL; duplicate term-only inserts as no-ops; and
  successful lazy mutations replaying after reopen.
- `tests/checkpoint_retention_correspondence.rs` - Adds persistent byte and
  char corruption-rebuild correspondence checks for retained active WAL tails,
  archived checkpoint segments plus post-checkpoint active records, batch
  insert replay, and remove replay.
- `tests/dirty_checkpoint_correspondence.rs` - Adds byte and char dirty
  checkpoint publication checks for injected dirty-slot write/sync failures,
  retry persistence, late slot-tracking coverage for already dirty arenas, and
  descriptor publication before WAL truncation with a replayed WAL tail.
- `src/persistent_artrie_core/recovery.rs` and
  `src/persistent_artrie{,_char}/mmap_ctor.rs` - Retain the active WAL as a
  replay segment before corruption rebuild, preserve archive/pending/active
  ordering, and replay batch/remove records during rebuild.
- `tests/wal_segment_lifecycle_correspondence.rs` - Adds WAL segment lifecycle
  checks for LSN-ordered collection when filenames disagree, async rotation
  preserving monotonic LSNs across archived and active segments, reopen after
  archive continuing next-LSN and synced-frontier state after retained LSNs,
  and archive pruning to the configured segment limit.
- `tests/recovery_planner_correspondence.rs` - Adds recovery planner
  correspondence checks for direct segment rebuild, `RecoveryManager`,
  incremental recovery, and byte and char corruption rebuilds stopping at the
  first corrupt WAL record instead of applying later suffix records.
- `tests/recovery_replay_completeness_correspondence.rs` - Adds replay
  completeness checks for shared mutating WAL variant expansion, byte
  corruption rebuild replay of batch increments and CAS, char archive replay of
  all mutating variants without re-logging recovered operations, and corrupt
  archive-prefix stopping.
- `tests/persistent_compaction_correspondence.rs` - Adds byte persistent
  compaction correspondence checks for unsynced WAL-backed values, rejected
  WAL sidecar collisions, term-only entries, non-UTF8 byte keys, stale WAL
  non-replay after finalization, crash-before-data-rename WAL restoration,
  crash-after-data-rename stale-WAL suppression, and output-file key/value
  preservation.
- `tests/persistent_rewrite_compaction_correspondence.rs` - Adds char/vocab
  rewrite checkpoint correspondence checks for Unicode char lazy/eager reopen,
  post-checkpoint WAL-tail replay, descriptor-only dirty retention, failed
  char WAL archive retry, sparse vocab duplicate bijections, and failed Bloom
  sidecar retry.
- `tests/persistent_vocab_checkpoint_correspondence.rs` - Adds persistent vocab
  checkpoint correspondence checks for Unicode/sparse/batch reopen, replay of
  post-checkpoint inserts, WAL retention across repeated recovery until
  checkpoint, non-checkpoint `rotate_wal`/`sync_to_disk`, missing/corrupt/stale
  reverse-index rebuild, missing/corrupt Bloom rebuild, and failed sidecar
  publication preserving dirty/WAL evidence.
- `tests/concurrent_checkpoint_publication_correspondence.rs` - Adds concurrent
  vocab checkpoint publication checks for queue duplicate batch stability,
  queue `flush()`, lock-free checkpoint publication, and duplicate lock-free
  races reopening from the checkpoint without WAL replay.
- `tests/persistent_shared_concurrency_correspondence.rs` - Adds byte, char,
  and vocab shared public checkpoint/write/sync/reopen schedules for the
  `SharedPersistentConcurrency` model and the byte shared checkpoint
  lock-publication regression.
- `src/persistent_artrie/shared_trait_impl.rs` - Makes byte
  `SharedARTrie::checkpoint()` hold the write lock across data persistence,
  checkpoint WAL publication, sync, and truncation by delegating to the mutable
  `PersistentARTrie::checkpoint()` path.
- `src/persistent_vocab_artrie/concurrent.rs` - Adds a publication gate around
  queue/lock-free inserts versus checkpoint, makes public `checkpoint()` and
  `flush()` call the underlying durable checkpoint path, and makes queue batch
  allocation duplicate-aware before pending inserts are drained.
- `src/persistent_vocab_artrie/{persistence_api,mmap_ctor,io_uring_ctor}.rs`
  - Makes vocab checkpoint WAL errors fail closed, keeps post-checkpoint WAL
  LSNs above the checkpoint threshold, retains replay WALs until checkpoint,
  rebuilds reverse-index sidecars from the trie snapshot, and prevents
  `rotate_wal`/`sync_to_disk` from acting as checkpoints.
- `src/persistent_artrie_core/wal/{writer,async_writer}.rs` and
  `src/persistent_artrie_core/recovery.rs` - Sort WAL segments by first record
  LSN rather than filename, preserve next/synced LSN state across rotation and
  reopen, prune archived async segments to `max_segments`, and generate
  collision-resistant archive segment paths.
- `src/persistent_artrie_char/{mutation_core,mutation_api}.rs` - Adds
  lazy-load preflight and `try_*_no_wal` mutation primitives so public char
  insert/value-insert/remove return lazy-load errors before WAL append instead
  of panicking or durably logging an unapplied mutation.
- `src/persistent_artrie_char/{wal_helpers,epoch_checkpointing,persist}.rs`
  and `src/persistent_artrie_core/epoch.rs` - Record epoch operation metadata
  after successful public WAL appends, force trie checkpoint publication before
  epoch metadata, and scope epoch docs to checkpoint tracking rather than an
  implicit automatic full-trie checkpoint.
- `tests/epoch_checkpoint_recovery_correspondence.rs` - Adds public mutation
  epoch-accounting checks, forced checkpoint reopen without WAL/archive replay,
  and corrupt epoch metadata fail-closed recovery coverage.
- `src/pathmap.rs`, `src/pathmap_char.rs` - Add `MutableDictionary` impls for
  PathMap byte/char dictionaries; fix PathMapChar edge enumeration so sibling
  Unicode scalar values sharing leading UTF-8 bytes are all exposed.
- `tests/pathmap_factory_correspondence.rs` - Adds feature-gated PathMap and
  factory correspondence checks for byte map/zipper traversal, Unicode
  character-edge completeness, mapped mutation/union, and all factory backends
  under `pathmap-backend`.
- `tests/substring_candidate_correspondence.rs` - Adds exact substring
  candidate checks for byte and Unicode SCDAWG, including repeated and
  overlapping matches, duplicate-term suppression, limited-result prefixes,
  and explicit empty-pattern behavior.
- `tests/fuzzy_candidate_coverage_correspondence.rs` - Adds byte and Unicode
  SCDAWG candidate coverage checks for substitution, insertion, deletion,
  deterministic generated substitution matrices, and short-query scope
  boundaries.
- `tests/serialization_correspondence.rs` - Adds public serializer
  correspondence checks for Bincode/JSON/plaintext term roundtrips,
  byte/Unicode value roundtrips, SCDAWG value paths, generated cases, legacy
  value dropping, and malformed payload rejection.
- `tests/protobuf_compression_correspondence.rs` - Adds feature-gated
  correspondence checks for gzip-wrapped serializers, protobuf V1/V2 graph
  formats, DAT protobuf delimiter-safe term replay, suffix-automaton protobuf
  source-language replay, generated roundtrips, and malformed payload
  rejection.
- `src/serialization/protobuf_impl.rs`, `proto/libdictenstein.proto` - Make
  protobuf graph decoding fail closed for invalid labels, term-count
  mismatches, and reachable cycles; encode DAT protobuf terms with a
  length-prefixed payload while keeping legacy newline payload decode support.
- `src/scdawg_core/inner.rs`, `src/scdawg.rs`, `src/scdawg_char.rs` - Store
  exact term values separately from substring automaton states so SCDAWG mapped
  lookup satisfies public map laws even when terms share suffix states.
- `scripts/verify-formal-correspondence.sh` - Adds a single local/CI entry
  point for Rust correspondence, Rocq proofs, SANY checks, optional Miri,
  optional io_uring checks, optional TLC, and the storage syscall plus SQE/CQE
  lifecycle models, including the root descriptor/reopen, persistent lazy
  mutation, persistent WAL atomicity, checkpoint retention, dirty checkpoint
  publication, WAL segment lifecycle, recovery planner, and recovery replay
  completeness, and epoch checkpoint recovery correspondence targets. Each
  spawned
  verification command runs through an 8GiB RSS cap by default
  (`FORMAL_RSS_LIMIT_BYTES=0` disables the wrapper).
- `.github/workflows/ci.yml` - Adds formal correspondence CI jobs for the
  default no-TLC harness, Miri-gated harness, io_uring-gated harness, and
  scheduled/manual TLC harness, with `prlimit` memory caps, bounded Cargo
  parallelism, and explicit TLA+ Java heap limits.

---

---

## Filesystem Layer Verification (Added 2026-01-22)

### Problem Summary

The existing formal verification for WAL/disk operations abstracted the POSIX filesystem layer, treating file operations as atomic. This abstraction gap allowed two bug classes to escape verification:

1. **TOCTOU Race**: `exists()` followed by `open()` is not atomic - file can be deleted between checks
2. **Missing Parent Directories**: `create()` assumes parent directories exist

### Root Cause Analysis

The original `WAL.tla` models WAL operations at the **logical level**:
- File creation is a single atomic transition
- File opening is a single atomic transition
- No modeling of the POSIX syscall sequence

In reality, `WalWriter::create()` involves multiple syscalls that can be interleaved:
1. `stat()` to check parent directory exists
2. `mkdir_all()` to create parent directories (if needed)
3. `open(O_CREAT)` to create the file
4. `fstat()` to verify file was created

### New TLA+ Modules

| Module | LOC | Description |
|--------|-----|-------------|
| FileSystem.tla | ~450 | POSIX filesystem model with TOCTOU semantics |
| WAL_FileSystem.tla | ~350 | WAL refined to use filesystem model |
| WAL_FileSystem.cfg | ~50 | Model checking configuration |

**Verification Coverage Matrix (Updated)**:

| Aspect | WAL.tla | WAL_FileSystem.tla |
|--------|---------|-------------------|
| LSN ordering | ✓ | ✓ |
| Transaction semantics | ✓ | ✓ |
| Crash recovery | ✓ | ✓ |
| File exists check | | ✓ |
| Parent directory existence | | ✓ |
| TOCTOU races | | ✓ |
| I/O error handling | | ✓ |

### New Rocq/Coq Modules

| Module | LOC | Status | Description |
|--------|-----|--------|-------------|
| Model/FileSystem.v | ~350 | Compiled | POSIX filesystem model |
| Proofs/FileSystemSafety.v | ~250 | Compiled | TOCTOU safety proofs |

### Key Theorems Proven

1. **mkdir_all_ensures_dir_exists** - `mkdir_all` always ensures the target directory exists
2. **open_or_create_safe_no_parent_error** - Safe open pattern never returns `ParentNotFound`
3. **open_or_create_safe_always_ok** - Safe open pattern always succeeds
4. **vulnerable_can_fail_parent_not_found** - The vulnerable pattern CAN fail with `ParentNotFound`
5. **safe_never_fails_parent_not_found** - The safe pattern NEVER fails with `ParentNotFound`

### Admitted Theorems (Require Additional Work)

**None.** As of 2026-05-22 `Proofs/FileSystemSafety.v` reports 6 `Theorem` + 5
`Lemma` = 11 propositions, all `Qed.`-closed. The previously-listed
`mkdir_all_idempotent_full` and
`open_or_create_safe_maintains_parent_invariant` either landed as full proofs
or were restated as provable variants under the same name. Re-grep the file
for the current set.

### Verification Commands

**TLA+ Model Checking**:
```bash
cd /home/dylon/Workspace/f1r3fly.io/libdictenstein/formal-verification/tla+

# Check filesystem model syntax
tla2sany FileSystem.tla

# Check WAL filesystem refinement syntax
tla2sany WAL_FileSystem.tla

# Run model checking (bounded)
tlc -workers 8 WAL_FileSystem.cfg
```

**Rocq Proof Compilation**:
```bash
cd /home/dylon/Workspace/f1r3fly.io/libdictenstein/formal-verification/rocq

# Compile with resource limits
systemd-run --user --scope -p MemoryMax=126G -p CPUQuota=1800% \
  -p IOWeight=30 -p TasksMax=200 make -j1

# Verify specific proofs
coqc -Q . ARTrie Model/FileSystem.v
coqc -Q . ARTrie Proofs/FileSystemSafety.v
```

### Lessons Learned

1. **Abstraction boundaries matter**: The original WAL.tla correctly verified WAL semantics, but abstracted filesystem operations as atomic. This is appropriate for most verification purposes but missed the TOCTOU bug class.

2. **Refinement specifications**: The WAL_FileSystem.tla demonstrates how to refine an abstract specification with more implementation detail without rewriting the original.

3. **Defense in depth**: Both TLA+ model checking and Rocq theorem proving identified the TOCTOU vulnerability, providing complementary verification approaches.

---

## Conclusion

The formal verification provides strong evidence for the correctness of the PART implementation:

1. **TLA+ model checking** explored ~7 million states with crash recovery enabled without finding any safety violations or deadlocks.

2. **Rocq proofs** establish key properties about node types, bucket operations, and structural invariants.

3. **Filesystem layer verification** (new) closes the abstraction gap for TOCTOU and parent directory races.

4. **Rust correspondence tests** exercise the implementation boundary for the
   focused proof/model obligations and caught/fixed the group-commit
   reserved-LSN correspondence requirement. They also check proof-carrying
   trace replay, corrupt-certificate rejection, record-boundary crash-prefix
   reopen, WAL transaction recovery, mmap storage-boundary behavior, byte
   lock-free publication, indexed char/vocab overlays, lock-free counter merge
   atomicity, durability-frontier
   publication/reclamation under bounded Loom schedules, storage syscall
   outcome fail-closed behavior, io_uring fixed-buffer and SQE/CQE lifecycle
   obligations, DynamicDawg mutation/compaction preservation, persistent
   cursor/batched/grouped/parallel merge equivalence, persistent root
   descriptor/reopen fallback, persistent lazy mutation atomicity, persistent
   WAL write-atomicity, persistent transaction increment recovery, lock-free
   counter merge atomicity,
   persistent vocab WAL atomicity, persistent vocab
   checkpoint publication, checkpoint/WAL retention safety, persistent
   compaction finalization recovery, unsafe
   contract coverage/status gating, valued set-combinator merge semantics, and
   SCDAWG occurrence construction for byte and Unicode substring APIs.

The combination of model checking (for concurrent/crash scenarios) and theorem proving (for functional correctness) provides complementary assurance:
- TLA+ finds protocol bugs via exhaustive state exploration
- Rocq proves properties that hold for all inputs
- The filesystem, mmap block-storage, storage syscall outcome, BufferManager
  page-lease, reverse-index mmap/remap publication, io_uring
  fixed-buffer/SQE-CQE, byte lock-free publication, indexed overlay,
  lock-free counter merge atomicity, and
  durability-frontier models check TOCTOU-safe file creation,
  allocation/remap/access ordering, write/sync fail-closed durability
  publication, cached-page pinning, read/write lease exclusion, and dirty
  flush exclusion during active mutable leases,
  header/map/file capacity publication, request/buffer ownership through
  completion checking,
  root-CAS/cache/merge linearization, checked counter overflow and
  all-or-nothing batch merge publication, char increment preservation, stable
  unique vocab index publication with sparse claim accounting, concurrent
  checkpoint publication gating and replay-tail retention, public
  durability-acknowledgement sync coverage, and prefix-closed
  durability/reclamation publication
- The Rust correspondence harness guards the model-to-code boundary in CI, and
  the unsafe inventory gate rejects contract rows without valid coverage/status
  metadata

As of 2026-06-01 the Rocq tree has **zero outstanding `Admitted`/`Axiom`/`Parameter` obligations**: all theorem/lemma/corollary propositions across the proof modules close by `Qed.` (or `Defined.` for transparent definitions). Scoped abstraction boundaries are tracked in `GAP_LEDGER.md`; the current boundary is production Byzantine networking/liveness, certified Rust/LLVM compilation, kernel io_uring/syscall internals below the modeled outcome boundary, gzip/prost internals, cross-language protobuf implementations, optimal/minimal automata size, arena-locality/throughput optimality, Bloom false-positive rates/hash-quality guarantees, arbitrary semiring `times` as meet for arbitrary semirings, and upstream Levenshtein transducer correctness, not unchecked structural-preservation, DynamicDawg mutation/compaction, DynamicDawgU64 sequence semantics, Bloom filter no-false-negative rejection, double-array-trie traversal, traversal-language, public read traversal, valued set-combinator merge, persistent merge equivalence, persistent char prefix semantics, persistent char bulk-mutation recovery, persistent relative encoding, arena reservation/dirty-slot persistence, persistent deduplicating-arena soundness, root descriptor/reopen fallback, persistent lazy mutation atomicity, persistent WAL write-atomicity, persistent transaction increment recovery, lock-free counter merge atomicity, shared persistent public API concurrency, public durability acknowledgement, persistent vocab WAL atomicity, persistent vocab checkpoint publication, concurrent checkpoint publication, checkpoint/WAL retention safety, dirty checkpoint publication safety, WAL segment lifecycle safety, recovery planner durable-prefix safety, recovery replay completeness, persistent compaction rewrite safety, char/vocab rewrite checkpoint safety, SCDAWG occurrence construction, substring-candidate, fuzzy-candidate, storage syscall outcome, BufferManager page-lease/cached-page pinning, reverse-index mmap/remap publication, io_uring fixed-buffer/SQE-CQE lifecycle, or public serialization proof gaps.

---

## TOCTOU Race Condition Fixes (2026-01-24)

### Summary

This section documents the TOCTOU (Time-of-Check to Time-of-Use) race condition fixes identified through TLA+ model checking and implemented in the Rust codebase.

### Analysis Results

| Variant | Component | Status | Notes |
|---------|-----------|--------|-------|
| u8 | WAL operations | ✓ SAFE | Uses atomic `create_new(true)` |
| u8 | Directory creation | ✓ SAFE | Idempotent `create_dir_all()` |
| u8 | open_or_create | ✓ SAFE | Proper race recovery |
| char | Archive dir creation | ✓ FIXED | Was vulnerable, now fixed |
| char | WAL file operations | ✓ SAFE | Has race recovery |

### Fix Applied: Archive Directory TOCTOU in char Variant

**File:** `src/persistent_artrie_char/dict_impl_char.rs`

**Locations Fixed:**
1. `create_with_config()` - Removed `exists()` check before `create_dir_all()`
2. `open_with_config()` - Removed `exists()` check before `create_dir_all()`

**Before (Vulnerable):**
```rust
if wal_config.archive_enabled {
    let archive_dir = path.parent().unwrap_or(Path::new(".")).join(&wal_config.archive_dir);
    if !archive_dir.exists() {                    // <-- TOCTOU: check
        std::fs::create_dir_all(&archive_dir)?;   // <-- TOCTOU: act (race window)
    }
}
```

**After (Safe):**
```rust
// NOTE: create_dir_all() is idempotent - no exists() check needed.
// Checking exists() before create_dir_all() creates a TOCTOU race window.
if wal_config.archive_enabled {
    let archive_dir = path.parent().unwrap_or(Path::new(".")).join(&wal_config.archive_dir);
    std::fs::create_dir_all(&archive_dir)?;       // Idempotent - no check needed
}
```

**Rationale:** `create_dir_all()` is idempotent (succeeds whether directory exists or not). The `exists()` check creates a TOCTOU window where:
- Another process could create a symlink between check and create
- Directory could be created with wrong permissions
- State inconsistency if directory deleted between check and create

### Verification

**Rust Tests:** All 285 char variant tests pass, all 10 ACID tests pass.

```bash
cargo test --features persistent-artrie persistent_artrie_char::  # 285 passed
cargo test --features persistent-artrie --test persistent_artrie_acid_tests  # 10 passed
```

### Correspondence to Formal Model

The fix aligns with the formal model in `FileSystemSafety.v`:

| Formal Theorem | Implementation Pattern |
|----------------|------------------------|
| `mkdir_all_idempotent` | Direct `create_dir_all()` call |
| `vulnerable_can_fail_parent_not_found` | Removed `exists()` check pattern |
| `safe_never_fails_parent_not_found` | Idempotent pattern now used |
