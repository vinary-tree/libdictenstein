# Formal Verification for ARTrie

This directory contains formal verification specifications and proofs for the Persistent Adaptive Radix Trie (PART) implementation in `libdictenstein`.

## Overview

The verification uses a two-pronged approach:

1. **TLA+ (Model Checking)**: Verifies concurrent safety, crash recovery, and linearizability via state space exploration
2. **Rocq/Coq (Theorem Proving)**: Proves functional correctness, node transitions, and refinement to abstract map ADT

## Directory Structure

```
formal-verification/
├── tla+/                      # TLA+ specifications
│   ├── ARTrieTypes.tla        # Type definitions and constants
│   ├── ARTrieState.tla        # Abstract trie state
│   ├── ArenaManager.tla       # Arena allocation/free-list model
│   ├── SequentialSiblings.tla # Sibling-list traversal semantics
│   ├── WAL.tla                # Write-ahead log specification
│   ├── WAL_FileSystem.tla     # WAL refined onto POSIX filesystem
│   ├── FileSystem.tla         # POSIX filesystem model (TOCTOU-aware)
│   ├── Concurrency.tla        # Optimistic lock coupling model
│   ├── CrashRecovery.tla      # ARIES-style recovery
│   ├── DocumentTransactions.tla # Bounded staged-write transaction model
│   ├── AsyncWalGroupCommit.tla # Async WAL/group commit model
│   ├── VersionLifecycle.tla   # MVCC reader/reclaim lifecycle
│   ├── DurabilityFrontier.tla # Durable-prefix/checkpoint/GC frontier model
│   ├── PointerOwnership.tla   # Raw pointer ownership/borrow model
│   ├── VocabPersistenceOwnership.tla # Vocab checkpoint/reopen/eviction ownership model
│   ├── MmapBlockStorage.tla   # mmap allocation/remap/access model
│   ├── StorageSyscallOutcome.tla # write/sync outcome durability boundary model
│   ├── IoUringFixedBufferOwnership.tla # io_uring fixed-buffer lifetime model
│   ├── IoUringSqeCqeLifecycle.tla # io_uring submit/complete/buffer lifecycle model
│   ├── LockFreeARTrieLinearizability.tla # Byte lock-free root/cache model
│   ├── LockFreeIndexedOverlay.tla # Char/vocab value/index overlay model
│   ├── LockFreeCounterMergeAtomicity.tla # Checked counter merge model
│   ├── ConcurrentCheckpointPublication.tla # Mutation/checkpoint race model
│   ├── SharedPersistentConcurrency.tla # Shared public RwLock/checkpoint model
│   ├── PublicDurabilityPolicy.tla # Public mutation/sync acknowledgement model
│   ├── PersistentEndToEndTrace.tla # Public checkpoint/WAL/compaction/reopen trace model
│   ├── PublicReadSnapshotTraversal.tla # Public read traversal snapshot model
│   ├── ConcurrentVocabLinearizability.tla # Public concurrent vocab history model
│   ├── EpochCheckpointRecovery.tla # Epoch checkpoint/recovery ordering model
│   ├── PersistentCharBulkMutationRecovery.tla # Char bulk-delete/RMW recovery model
│   ├── PersistentTransactionIncrementRecovery.tla # Tx increment/replay overflow model
│   ├── ByzantineStorage.tla   # Authenticated committed-record recovery
│   ├── HotStuffConsensus.tla  # Bounded Byzantine quorum/log safety model
│   ├── NodeTransitions.tla    # Node growth transitions
│   ├── EpochCheckpoint.tla    # Epoch lifecycle
│   ├── PART.tla               # Main composed specification
│   ├── PART.cfg               # TLC configuration (no crash)
│   └── PART_crash.cfg         # TLC configuration (with crash)
│
└── rocq/                      # Rocq/Coq proofs (58 .v files, 24,405 LOC,
    │                            1,192 theorem/lemma/corollary propositions,
    │                            0 Admitted / 0 Axiom / 0 Parameter)
    ├── Makefile               # Build system
    ├── Spec/                  # Specifications
    │   ├── MapSpec.v          # Abstract map specification
    │   ├── DictionaryLawSpec.v # Public dictionary/set/map/bijection laws
    │   ├── DynamicDawgMutationSpec.v # DynamicDawg mutation/compaction laws
    │   ├── DynamicDawgU64Spec.v # u64 sequence DAWG semantics and adapters
    │   ├── DoubleArrayTrieSpec.v # BASE/CHECK DAT construction/traversal laws
    │   ├── ZipperLanguageSpec.v # Traversal-language equivalence laws
    │   ├── ValuedSetCombinatorSpec.v # Union/intersection value-merge laws
    │   ├── BloomFilterSpec.v # Bloom filter no-false-negative laws
    │   ├── PersistentMergeSpec.v # Cursor pagination and persistent merge laws
    │   ├── PersistentPrefixSpec.v # Persistent char prefix/removal/recovery laws
    │   ├── PathMapFactorySpec.v # Optional PathMap and factory dispatch laws
    │   ├── RelativeEncodingSpec.v # Persistent child-pointer encoding laws
    │   ├── ArenaReservationSpec.v # Arena slot-store/reservation/flush laws
    │   ├── DedupArenaSpec.v # Persistent dedup cache soundness laws
    │   ├── RootDescriptorReopenSpec.v # Root descriptor/reopen recovery laws
    │   ├── PersistentLazyMutationSpec.v # Lazy mutation/WAL atomicity laws
    │   ├── PersistentWalAtomicitySpec.v # Persistent write-before-mutation laws
    │   ├── LockFreeCounterMergeSpec.v # Checked counter merge atomicity laws
    │   ├── SharedPersistentConcurrencySpec.v # Shared public API concurrency laws
    │   ├── PublicDurabilityPolicySpec.v # Public durability acknowledgement laws
    │   ├── PersistentPublicWalLifecycleSpec.v # Public WAL open/replay lifecycle laws
    │   ├── PersistentEndToEndTraceSpec.v # Composed checkpoint/WAL/compaction/reopen trace laws
    │   ├── PersistentVocabWalAtomicitySpec.v # Vocab WAL atomicity and bijection laws
    │   ├── PersistentVocabCheckpointSpec.v # Vocab checkpoint/sidecar publication laws
    │   ├── PersistentCheckpointRetentionSpec.v # Checkpoint/WAL retention laws
    │   ├── PersistentDirtyCheckpointSpec.v # Dirty checkpoint publication laws
    │   ├── PersistentWalSegmentLifecycleSpec.v # WAL segment lifecycle laws
    │   ├── PersistentRecoveryPlannerSpec.v # Recovery planner/durable-prefix laws
    │   ├── PersistentRecoveryReplayCompletenessSpec.v # Recovery replay completeness laws
    │   ├── PersistentCompactionSpec.v # Persistent compaction rewrite/finalization laws
    │   ├── PersistentRewriteCompactionSpec.v # Char/vocab rewrite checkpoint laws
    │   ├── SubstringSearchSpec.v # Exact substring candidate laws
    │   ├── ScdawgOccurrenceSpec.v # SCDAWG occurrence-construction laws
    │   ├── FuzzyCandidateCoverageSpec.v # WallBreaker candidate coverage laws
    │   ├── SerializationRoundtripSpec.v # Public serializer/codec roundtrip/fail-closed laws
    │   ├── ARTrieSpec.v       # ARTrie-specific specification (incl.
    │                            trie_insert/_delete + correctness theorems)
    │   └── ReplicatedMapSpec.v # Replicated command-log replay spec
    ├── Model/                 # Data structure models
    │   ├── Key.v              # Key representation
    │   ├── NodeTypes.v        # Node4, Node16, Node48, Node256
    │   ├── Bucket.v           # B-trie bucket model
    │   ├── HotStuff.v         # Byzantine quorum/log-safety model
    │   ├── PathCompression.v  # Prefix compression
    │   ├── FileSystem.v       # POSIX filesystem model
    │   ├── ArenaManager.v     # Arena allocation/free-list model
    │   └── SequentialSiblings.v   # Sibling-list traversal semantics
    ├── Invariants/            # Invariant definitions and proofs
    │   ├── StructuralInvariants.v
    │   ├── TransitionInvariants.v
    │   ├── ArenaInvariants.v
    │   └── SequentialSiblingsInvariants.v
    ├── Operations/            # (empty) — reserved for extracted
    │                            imperative variants of insert/delete
    └── Proofs/                # Main theorem proofs
        ├── FileSystemSafety.v # TOCTOU safety
        ├── MapRefinement.v    # ARTrie refines Map ADT (WFARTrieMapImpl)
        ├── StructuralPreservation.v # Checked structural contracts
        ├── ByzantineRecovery.v # Fail-closed recovery under storage faults
        ├── CertifiedReference.v # Certified reference interface boundary
        ├── HotStuffSafety.v # Replicated-map safety over compatible logs
        └── ProofCarryingExtraction.v # Certified trace checker/replay proof
```

## TLA+ Specifications

### Properties Verified

| Property | Category | Description |
|----------|----------|-------------|
| Completeness | Safety | All inserted items are retrievable |
| Consistency | Safety | Removed items are not retrievable |
| Exclusive Write | Safety | No concurrent writers to same node |
| Version Consistency | Safety | Odd version implies exclusively locked |
| Node Capacity | Safety | Node type respects child count limits |
| Transition Correctness | Safety | Node transitions preserve all children |
| Crash Recovery | Safety | Committed operations survive crashes |
| Linearizability | Safety | Operations appear atomic |
| Writers Release | Liveness | Writers eventually release locks |
| Recovery Liveness | Liveness | Crash leads to eventual recovery |
| Document Transaction Visibility | Safety | Visible document values come from committed staged writes |
| Async WAL Durability | Safety | Group commit preserves FIFO LSN order and publishes only durable LSN prefixes |
| Version Reclamation | Safety | Reclaimed versions are not referenced by readers |
| Durability Frontier | Safety | Synced LSNs, checkpoints, recovery, and VersionGc reclamation stay within the durable prefix |
| Raw Pointer Ownership | Safety | Raw slot pointers, node-map entries, and borrows do not outlive in-memory ownership |
| Vocab Persistence Ownership | Safety | Vocab checkpoint/reopen preserves stable term-index bijections, and eviction invalidates node-map raw entries before drop |
| Mmap Block Storage | Safety | Allocation/remap protocol maps blocks before successful access |
| Storage Syscall Outcomes | Safety | Short/error/interrupted/cancelled/missing write or sync outcomes cannot advance the reported durable prefix; recovery applies only the durable prefix |
| Byte Lock-Free ARTrie Publication | Safety | Root CAS, cache publication, contains, and merge snapshot points are linearizable |
| Indexed Lock-Free Overlays | Safety | Char increments preserve value sums, and vocab CAS inserts preserve stable unique indices while allowing sparse claims |
| Lock-Free Counter Merge Atomicity | Safety | Checked byte/char counter overlays reject overflow before mutation and merge as one all-or-nothing `BatchIncrement` |
| Concurrent Checkpoint Publication | Safety | Inserts and checkpoint publication are mutually ordered so snapshots do not lose visible mutations or truncate needed WAL tails |
| Shared Persistent Public API Concurrency | Safety | Shared byte/char/vocab writes, reads, sync, checkpoint, and recovery stay linearizable through `Arc<RwLock<...>>` and do not truncate replay evidence for racing visible writes |
| Public Durability Policy | Safety | `Immediate` and `GroupCommit` public mutation/sync acknowledgements are covered by the synced WAL frontier; `Periodic`/`None` do not overclaim synced durability |
| Public WAL Lifecycle | Safety | Public byte/char/vocab open recovers persisted checkpoints plus synced retained WAL tails, and group-commit returned LSNs match durable WAL record order |
| Persistent End-to-End Trace | Safety | Public mutations, checkpoint publication, byte compaction rewrite, crash/reopen replay, and vocab bijection preservation compose into one recoverable trace |
| Public Read Snapshot Traversal | Safety | Byte/char/vocab public iteration, prefix iteration, and zipper-style traversal return exact visible snapshots or fail closed on lazy/disk corruption |
| Public DictionaryNode Traversal | Safety | The faulting `DictionaryNode` walk reaches exactly the snapshot regardless of residency (`WalkReachesAllKeys`); faulting/reopen never drop a key; the non-faulting walk is sound but incomplete over swizzled children |
| Eviction Walk EBR | Safety | No active reader observes a freed node under the gated unlink → retire → drain → free reclaim (`NoUseAfterFree`); a linked node is never freed; the gate is necessary (the property is violated with `Gated = FALSE`) |
| Concurrent Vocab Linearizability | Safety | Public insert/read/batch/checkpoint/recover histories have a sequential vocabulary-map explanation respecting real-time order |
| Epoch Checkpoint Recovery | Safety | Epoch metadata is published only after the trie checkpoint, and WAL cleanup retains recovery evidence for visible operations |
| Transaction Increment Recovery | Safety | Transaction increment aggregation/current-value overflow fails before commit WAL publication, and replay stops before overflowed `BatchIncrement` suffixes |
| io_uring Fixed-Buffer Ownership | Safety | Fixed-buffer I/O is used only for registered live buffers, and owners drop buffers only after unregister |
| io_uring SQE/CQE Lifecycle | Safety | Submitted requests own one live buffer until exactly one CQE is checked; short/error completions fail closed |
| Byzantine Storage Filtering | Safety | Recovery applies only committed authenticated records |
| Byzantine Quorum Safety | Safety | HotStuff/PBFT-style committed logs remain prefix-compatible despite one Byzantine voter |
| Proof-Carrying Replay | Safety | Certified traces replay to the reference map semantics and reject invalid steps |
| Public Dictionary Laws | Safety | Exact dictionaries, mapped dictionaries, set-zippers, mutation traces, and bijective maps refine their reference set/map laws |
| DynamicDawg Mutation Preservation | Safety | DynamicDawg insert/update/remove/batch operations, compaction, and minimization preserve reference set/map semantics |
| Bloom Filter Lookup Rejection | Safety | Inserted byte/string payloads are never rejected, clear removes evidence, and parameter normalization prevents zero-hash vacuity |
| Double-Array Trie Construction | Safety | BASE/CHECK transitions, child-edge iteration, duplicate normalization, mapped lookup, and root-state traversal refine reference set/map semantics |
| Zipper Language Equivalence | Safety | Zipper descent, children, finality, iteration, values, and combinators refine reference languages/maps |
| Valued Set-Combinator Merge | Safety | Union/intersection duplicate-value conflict strategies refine ordered reference folds, including first-wins, last-wins, lattice join/meet, and semiring join boundaries |
| Persistent Merge Equivalence | Safety | Cursor pagination, ordinary batched merge, arena-grouped batched merge, and parallel partitioned merge refine the same reference map merge |
| Persistent Prefix Semantics | Safety | Persistent char prefix iteration, valued/arena variants, and batched deletion refine reference-map prefix filtering/removal |
| Arena Reservation Integrity | Safety | Arena allocation, reserved sibling slots, dirty-slot flushing, and load/reopen directory reconstruction preserve exact slot payloads |
| Persistent Deduplication Soundness | Safety | Verified dedup hits reuse only live matching payloads, stale or colliding entries allocate fresh slots, and the compatibility setter keeps verification enabled |
| Root Descriptor Reopen | Safety | Persistent root descriptors are trusted only when valid and loaded; malformed descriptors, bad arena counts, and lazy-load failures replay or fail closed |
| Persistent Lazy Mutation Atomicity | Safety | Lazy-load errors reject public mutations before WAL append, no-op mutations avoid replayable records, and successful mutations replay to the in-memory post-state |
| Persistent WAL Write Atomicity | Safety | Serialization/preflight and WAL append failures reject byte/char persistent writes before memory changes; successful atomic writes and committed document batches replay to the in-memory post-state |
| Persistent Vocab WAL Atomicity | Safety | Vocabulary term/index assignment appends and syncs WAL before visible mutation, rejects reindex/collision attempts, preserves state on WAL failure, and keeps reverse-index membership exact |
| Dirty Checkpoint Publication | Safety | Dirty arena/slot evidence, root descriptors, checkpoint records, and WAL truncation thresholds compose only after successful flush/sync |
| Recovery Planner Durable Prefix | Safety | Root/checkpoint trust, corrupt-file rebuild selection, and byte/char recovery replay stop at the first corrupt WAL record |
| Recovery Replay Completeness | Safety | Every mutating WAL variant maps to byte/char/archive/incremental no-WAL replay, batch records expand in order, failed CAS is ignored, and corrupt suffixes are not trusted |
| Persistent Compaction Rewrite | Safety | Byte trie compaction preserves the exact key/value snapshot, rejects WAL sidecar collisions, and recovers crash points around stale-WAL backup, data rename, and cleanup |
| Substring Candidate Correctness | Safety | Exact substring APIs return the reference occurrence set needed by fuzzy-search candidate generation |
| SCDAWG Occurrence Construction | Safety | Forward traversal, left-extension closure, `locations`, and `freq` refine exact reference substring occurrences |
| Fuzzy Candidate Coverage | Safety | Splitting a query into `budget + 1` nonempty pieces guarantees at least one surviving exact substring candidate for in-budget terms |
| Public Serialization Roundtrip | Safety | Term-only, value-aware, gzip-wrapped, protobuf, DAT-protobuf, and suffix-protobuf serializers preserve their reference semantics, and malformed payloads fail closed |

### Running TLC Model Checker

```bash
# Basic safety checking (no crash)
tlc -workers 8 PART.tla -config PART.cfg

# With crash recovery verification
tlc -workers 8 PART.tla -config PART_crash.cfg

# Bounded focused models added in 2026-05-22/2026-05-26 refreshes
tlc -workers 1 -config DocumentTransactions.cfg DocumentTransactions.tla
tlc -workers 1 -config AsyncWalGroupCommit.cfg AsyncWalGroupCommit.tla
tlc -workers 1 -config VersionLifecycle.cfg VersionLifecycle.tla
tlc -workers 1 -config DurabilityFrontier.cfg DurabilityFrontier.tla
tlc -workers 1 -config PointerOwnership.cfg PointerOwnership.tla
tlc -workers 1 -config VocabPersistenceOwnership.cfg VocabPersistenceOwnership.tla
tlc -workers 1 -config MmapBlockStorage.cfg MmapBlockStorage.tla
tlc -workers 1 -config StorageSyscallOutcome.cfg StorageSyscallOutcome.tla
tlc -workers 1 -config IoUringFixedBufferOwnership.cfg IoUringFixedBufferOwnership.tla
tlc -workers 1 -config IoUringSqeCqeLifecycle.cfg IoUringSqeCqeLifecycle.tla
tlc -workers 1 -config LockFreeARTrieLinearizability.cfg LockFreeARTrieLinearizability.tla
tlc -workers 1 -config LockFreeIndexedOverlayCounter.cfg LockFreeIndexedOverlay.tla
tlc -workers 1 -config LockFreeIndexedOverlayVocabulary.cfg LockFreeIndexedOverlay.tla
tlc -workers 1 -config LockFreeCounterMergeAtomicity.cfg LockFreeCounterMergeAtomicity.tla
tlc -workers 1 -config ConcurrentCheckpointPublication.cfg ConcurrentCheckpointPublication.tla
tlc -workers 1 -config SharedPersistentConcurrency.cfg SharedPersistentConcurrency.tla
tlc -workers 1 -config PersistentEndToEndTrace.cfg PersistentEndToEndTrace.tla
tlc -workers 1 -config ConcurrentVocabLinearizability.cfg ConcurrentVocabLinearizability.tla
tlc -workers 1 -config EpochCheckpointRecovery.cfg EpochCheckpointRecovery.tla
tlc -workers 1 -config ByzantineStorage.cfg ByzantineStorage.tla
tlc -workers 1 -config HotStuffConsensus.cfg HotStuffConsensus.tla
```

### End-to-End Correspondence Check

Run the CI-practical proof/model/source alignment harness from the repository
root:

```bash
scripts/verify-formal-correspondence.sh
```

This runs the Rust correspondence tests, the Rocq proof build, and SANY checks
for the focused TLA+ models. It also runs
`scripts/verify-unsafe-boundary-inventory.sh`, which checks the live
`src/**/*.rs` unsafe surface against
`formal-verification/UNSAFE_INVENTORY.tsv` and ensures every inventory contract
tag is defined in `formal-verification/UNSAFE_CONTRACTS.tsv` with a valid
coverage class (`rocq`, `tla`, `loom`, `miri`, `correspondence`,
`compile-time`, `unit`, or `trusted-boundary`) and status. Persistence unsafe
contracts must be marked `covered` or `miri-wired`. The default
harness includes the DynamicDawg mutation, DynamicDawgU64 sequence, Bloom filter,
double-array trie, valued set-combinator, persistent merge, persistent prefix,
root descriptor/reopen, persistent lazy mutation, persistent WAL atomicity,
persistent char bulk mutation, persistent transaction increment,
lock-free counter merge atomicity, persistent vocab WAL atomicity,
persistent vocab checkpoint publication,
concurrent checkpoint publication, checkpoint retention, dirty checkpoint
publication, WAL segment lifecycle, recovery planner durable-prefix replay,
recovery replay completeness, persistent public lifecycle, persistent
end-to-end trace, epoch checkpoint recovery, substring, SCDAWG occurrence, and
fuzzy candidate coverage targets plus the
feature-gated valued semiring and public
serialization targets under `--features lling-llang`,
`--features serialization`, `--features "serialization protobuf compression"`,
and `--features "persistent-artrie parallel-merge"`. Set
`FORMAL_RSS_LIMIT_BYTES=0` to disable the default 8GiB per-process RSS cap. Set
`RUN_TLC=1` to also run the
bounded TLC model checks:

```bash
RUN_TLC=1 scripts/verify-formal-correspondence.sh
```

Set `RUN_MIRI=1` to add the Miri-compatible raw child-pointer ownership,
swizzled raw-extraction, lazy-load candidate cleanup, vocab reopen/eviction
ownership, and BufferManager fixed-buffer lifetime checks:

```bash
RUN_MIRI=1 scripts/verify-formal-correspondence.sh
```

Set `RUN_IO_URING=1` on an io_uring-capable Linux host to add the optional
storage backend checks:

```bash
RUN_IO_URING=1 scripts/verify-formal-correspondence.sh
```

### Configuration Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| NumThreads | 3 | Concurrent threads |
| MaxKeys | 4 | Maximum keys in model |
| MaxLSN | 15 | Maximum log sequence number |
| MaxEpoch | 3 | Maximum epoch count |
| EnableCrash | FALSE | Enable crash modeling |

## Rocq/Coq Proofs

### Key Theorems

1. **Lookup Correctness**: `art_lookup t k = interpret_trie t k`
2. **Insert Correctness**: `interpret_trie (art_insert t k v) = insert_map (interpret_trie t) k v`
   — see `trie_insert_correct` at `Spec/ARTrieSpec.v:706`
3. **Delete Correctness**: `interpret_trie (art_delete t k) = delete_map (interpret_trie t) k`
   — see `trie_delete_correct` at `Spec/ARTrieSpec.v:719`
4. **Node Transition Correctness**: Transitions preserve all children
   — see `growth_type_appropriate_after_insert` and
   `shrink_type_appropriate_with_lower_bound` in
   `Invariants/TransitionInvariants.v`
5. **Map Refinement**: ARTrie correctly implements Map ADT
   — see `WFARTrieMapImpl` Instance in `Proofs/MapRefinement.v`
6. **Public Dictionary Laws**: exact membership, mapped lookup/domain
   preservation, set-zippers, mutation traces, and bijective forward/reverse
   maps satisfy the backend-neutral public law spec
   — see `Spec/DictionaryLawSpec.v`
7. **DynamicDawg Mutation and Compaction Preservation**: insert,
   insert-with-value, update-or-insert, remove, compact, minimize, extend, and
   remove-many preserve reference set/map semantics and return-value laws
   — see `Spec/DynamicDawgMutationSpec.v`
8. **DynamicDawgU64 Sequence Semantics**: u64 sequence insert,
   insert-with-value, update-or-insert, remove, adapters, iterators, zippers,
   and bounded snapshot-concurrency checks refine reference set/map semantics
   — see `Spec/DynamicDawgU64Spec.v`
9. **Double-Array Trie Construction and Traversal**: byte and Unicode DAT
   BASE/CHECK walks, child-edge iteration, duplicate normalization, and mapped
   lookup refine reference set/map semantics
   — see `Spec/DoubleArrayTrieSpec.v`
10. **Zipper Language Equivalence**: descent, child iteration, finality,
   valued lookup, prefix/excluding filters, and set/value-diff combinators
   refine reference languages/maps
   — see `Spec/ZipperLanguageSpec.v`
11. **Persistent Merge Equivalence**: cursor pagination, ordinary batched
   merge, arena-grouped batched merge, and parallel partitioned merge refine
   the same reference map merge
   — see `Spec/PersistentMergeSpec.v`
12. **Persistent Prefix Semantics**: persistent char prefix iteration,
   valued/arena-aware prefix views, and ordinary/batched prefix deletion refine
   reference-map prefix filtering/removal
   — see `Spec/PersistentPrefixSpec.v`
13. **Arena Reservation Integrity**: arena allocation/read, same-size updates,
   contiguous sibling reservation, dirty-slot flushing, and load/reopen refine
   the abstract slot-store model
   — see `Spec/ArenaReservationSpec.v`
14. **Persistent Deduplication Soundness**: verified cache hits reuse only
   slots that still store the requested bytes, stale or colliding entries
   allocate fresh slots, cache clear/take remove reuse evidence, and
   the compatibility setter keeps verification enabled
   — see `Spec/DedupArenaSpec.v`
15. **Root Descriptor/Reopen Refinement**: root descriptors are trusted only
   after kind/final-flag/payload/arena-count validation and a successful root
   load; otherwise reopen ignores checkpoint skip thresholds, replays WAL from
   zero, and lazy public reads fail closed
   — see `Spec/RootDescriptorReopenSpec.v`
16. **Persistent Lazy Mutation Atomicity**: public lazy mutations reject
   lazy-load errors before appending WAL, no-op mutations leave WAL unchanged,
   and successful mutation replay matches the in-memory post-state
   — see `Spec/PersistentLazyMutationSpec.v`
17. **Persistent WAL Write Atomicity**: byte and char persistent writes reject
   serialization/preflight and WAL append failures before mutating memory, while
   successful atomic writes and committed document batches replay to the same
   reference map state
   — see `Spec/PersistentWalAtomicitySpec.v`
18. **Persistent Vocab WAL Atomicity**: vocabulary inserts, manual index
   assignments, and batches append/sync WAL before visible mutation, reject
   term reindexing and index collisions, preserve state on WAL failure, and
   keep `contains_index` exact with the reverse map
   — see `Spec/PersistentVocabWalAtomicitySpec.v`
19. **Persistent Vocab Checkpoint Publication**: successful checkpoints reopen
   the same forward/reverse bijection, failed publication retains dirty/WAL
   replay evidence, post-checkpoint inserts stay above the checkpoint LSN,
   `rotate_wal`/`sync_to_disk` do not act as checkpoints, and missing/corrupt
   Bloom or reverse-index sidecars rebuild safely
   — see `Spec/PersistentVocabCheckpointSpec.v`
20. **Checkpoint/WAL Retention Safety**: valid checkpoints may justify replay
   skip thresholds, invalid checkpoints cannot justify truncation, and active
   WAL tails are retained before corruption rebuild
   — see `Spec/PersistentCheckpointRetentionSpec.v`
21. **Dirty Checkpoint Publication Safety**: dirty arena/slot evidence is
   cleared only after successful flush/sync, late slot tracking preserves
   existing dirty coverage, and WAL truncation/replay skipping requires a valid
   synced root descriptor plus a synced checkpoint record
   — see `Spec/PersistentDirtyCheckpointSpec.v`
22. **WAL Segment Lifecycle Safety**: archive/pending/active segment collection
   is ordered by record LSNs rather than filenames, rotations preserve
   monotonic LSN state, reopen continues after retained archives, and
   checkpoint-covered archive pruning is safe
   — see `Spec/PersistentWalSegmentLifecycleSpec.v`
23. **Recovery Planner Durable-Prefix Safety**: root/checkpoint trust, corrupt
   file rebuild selection, retained segment replay, and byte/char recovery
   entry points all stop at the first corrupt WAL record and use only the
   durable prefix
   — see `Spec/PersistentRecoveryPlannerSpec.v`
24. **Recovery Replay Completeness**: byte, char, archive, recovery-manager,
   and incremental entry points share the same WAL-record-to-replay mapping;
   batch insert/increment records expand in order, failed CAS records do not
   mutate state, corrupt suffixes are ignored, and recovery uses no-WAL
   mutations
   — see `Spec/PersistentRecoveryReplayCompletenessSpec.v`
25. **Persistent Compaction Rewrite Safety**: byte trie compaction copies the
   exact key/value/term-only snapshot, rejects temporary WAL sidecar
   collisions, and recovers finalization crashes by restoring the old WAL
   before data publication or suppressing stale WAL replay after data
   publication
   — see `Spec/PersistentCompactionSpec.v`
26. **Char/Vocab Rewrite Checkpoint Safety**: char and vocabulary checkpoint
   rewrites preserve Unicode value snapshots and sparse vocab index bijections;
   post-checkpoint WAL tails replay over the checkpoint, and failed publication
   keeps dirty/WAL evidence retryable
   — see `Spec/PersistentRewriteCompactionSpec.v`
27. **Substring Candidate Correctness**: non-empty exact substring queries return
   precisely the reference `(term, position, length)` candidate set needed by
   fuzzy-search transducers
   — see `Spec/SubstringSearchSpec.v`
28. **SCDAWG Occurrence Construction**: forward traversal, left-extension
   closure, handle-based `locations_at`, public `locations`, and `freq` refine
   the same reference occurrence relation
   — see `Spec/ScdawgOccurrenceSpec.v`
29. **Fuzzy Candidate Coverage**: a `budget + 1` nonempty query-piece split
   leaves at least one exact piece candidate for any term whose edit witness
   damages at most `budget` pieces
   — see `Spec/FuzzyCandidateCoverageSpec.v`
30. **Serialization Roundtrip Correctness**: public serializers preserve
   term-membership, mapped lookup values, gzip wrapper payloads, protobuf graph
   formats, DAT protobuf terms, and suffix-automaton source languages according
   to their wire format, and invalid payloads fail closed
   — see `Spec/SerializationRoundtripSpec.v`
31. **End-to-End Persistent Trace Refinement**: public mutation, checkpoint,
   byte compaction rewrite, crash/reopen replay, and vocab bijection laws
   compose over one persistent trace
   — see `Spec/PersistentEndToEndTraceSpec.v`

### Building Proofs

```bash
cd rocq

# Generate dependencies
make depend

# Build all proofs
make

# Build with resource limits (for memory-intensive proofs)
make build-safe

# Check a single file
make check-Model/Key
```

### Proof Status

As of 2026-05-26: all modules **Complete** — 0 `Admitted` / 0 `Axiom` /
0 `Parameter` across the 58 .v files (verified by grep, see
[VERIFICATION_RESULTS.md](VERIFICATION_RESULTS.md) for the per-file tally).

| Module | Status | Description |
|--------|--------|-------------|
| Key.v | Complete (0 Admitted) | Key representation and operations |
| NodeTypes.v | Complete | Node type definitions |
| Bucket.v | Complete (0 Admitted) | Bucket operations plus sorted-bucket contract and binary-search partition proof |
| HotStuff.v | Complete | Quorum arithmetic plus HotStuff/PBFT-style honest-intersection log safety |
| PathCompression.v | Complete (0 Admitted) | Prefix matching |
| MapSpec.v | Complete | Abstract map specification |
| DictionaryLawSpec.v | Complete | Public exact-set, mapped-dictionary, zipper, trace replay, and bijective dictionary laws |
| DynamicDawgMutationSpec.v | Complete | DynamicDawg insert/update/remove, batch, compaction, minimization, return-value, and valued-domain consistency laws |
| DynamicDawgU64Spec.v | Complete | u64 sequence set/map mutation laws, string/f64 adapter refinement, iterator/zipper exactness, and bounded snapshot-concurrency boundaries |
| DoubleArrayTrieSpec.v | Complete | Generic BASE/CHECK transition, traversal, normalization, and lookup/domain refinement laws for byte and Unicode DATs |
| ZipperLanguageSpec.v | Complete | Zipper traversal-language, valued lookup, prefix/excluding, and set-combinator laws |
| ValuedSetCombinatorSpec.v | Complete | Ordered value-merge laws for union/intersection zippers, first-wins/last-wins/custom strategies, lattice join/meet, and semiring join-only boundaries |
| BloomFilterSpec.v | Complete | No-false-negative Bloom filter insertion, clear, byte/string refinement, duplicate insert, and nonvacuous parameter laws |
| PersistentMergeSpec.v | Complete | Cursor pagination, ordinary batched merge, grouped batched merge, and parallel partition merge equivalence laws |
| PersistentPrefixSpec.v | Complete | Persistent char prefix filter, valued/arena projection, ordinary removal, batched-removal equivalence, durable-prefix deletion, and checked RMW overflow laws |
| PersistentReadTraversalSpec.v | Complete | Public read traversal snapshot exactness, prefix safety, no-fabrication, fail-closed lazy/disk error, and read-preserves-snapshot laws |
| PathMapFactorySpec.v | Complete | Optional PathMap byte/char mutation, node-edge, UTF-8 character traversal, and factory dispatch laws |
| RelativeEncodingSpec.v | Complete | Persistent byte/char child-pointer encoding, checked decode rejection, sequential overflow, and dedup-cache soundness laws |
| ArenaReservationSpec.v | Complete | Arena slot allocation/read/update, contiguous reservation, fail-closed dirty flush, and load/reopen directory reconstruction laws |
| DedupArenaSpec.v | Complete | Persistent byte/char deduplicating arena cache hits, stale/collision fail-closed allocation, verify-false compatibility, clear/take, and legacy unverified-mode assumptions |
| RootDescriptorReopenSpec.v | Complete | Persistent root descriptor validity, arena-count bounds, checkpoint replay fallback, lazy-load error propagation, public fail-closed reads, and byte/char parity laws |
| PersistentLazyMutationSpec.v | Complete | Public lazy mutation preflight, no-WAL-on-error/no-op, successful mutation replay, and byte/char parity laws |
| PersistentWalAtomicitySpec.v | Complete | Persistent byte/char write preflight, serialization failure, WAL append failure, no-op atomic writes, checked increment overflow, successful replay, and document commit ordering laws |
| LockFreeCounterMergeSpec.v | Complete | Checked lock-free counter increment/merge preflight, all-or-nothing batch append, WAL failure, and overlay-retention laws |
| SharedPersistentConcurrencySpec.v | Complete | Shared public write/checkpoint lock exclusion, sync non-publication, checkpoint success/failure, read observation, and crash-recovery laws |
| PublicDurabilityPolicySpec.v | Complete | Public durability acknowledgement laws for full-policy mutation sync, weak-policy non-overclaiming, async sync completion, checkpoint frontier, and recovery-prefix bounds |
| PersistentPublicWalLifecycleSpec.v | Complete | Public open/recovery equivalence, retained-tail checkpoint frontier, durable-prefix bounds, exact reserved-LSN writes, ordered group-commit queue replay, and checkpoint-record no-op laws |
| PersistentVocabWalAtomicitySpec.v | Complete | Persistent vocabulary WAL-before-mutation, duplicate stability, reindex/collision rejection, exact reverse-index membership, and batch first-occurrence deduplication laws |
| PersistentVocabCheckpointSpec.v | Complete | Persistent vocabulary checkpoint/reopen bijection, failure-retained WAL/dirty evidence, post-checkpoint LSN continuation, non-checkpoint WAL sync/rotation, and Bloom/reverse-index sidecar rebuild laws |
| PersistentCheckpointRetentionSpec.v | Complete | Checkpoint skip-threshold safety, invalid-checkpoint no-skip behavior, active WAL retention, archive/pending/active replay order, safe truncation, and byte/char parity laws |
| PersistentDirtyCheckpointSpec.v | Complete | Dirty arena/slot evidence, retry after failed write/sync, late slot-tracking coverage, trusted checkpoint publication, and WAL truncation/replay threshold laws |
| PersistentWalSegmentLifecycleSpec.v | Complete | WAL segment state transitions, LSN-based segment collection order, monotonic LSN and synced-frontier continuation after rotation/reopen, checkpoint-covered pruning, and archive/active replay laws |
| PersistentRecoveryPlannerSpec.v | Complete | Recovery mode selection, root/checkpoint trust, durable-prefix replay after corrupt WAL records, retained segment order, and byte/char planner parity laws |
| PersistentRecoveryReplayCompletenessSpec.v | Complete | Shared WAL-record replay mapping, batch expansion, failed-CAS elision, durable-prefix stopping, endpoint parity, and no-WAL recovery application laws |
| PersistentCompactionSpec.v | Complete | Byte persistent compaction snapshot identity, term-count insufficiency, temp/WAL sidecar disjointness, failure preservation, stale-WAL replay hazard, crash-finalization recovery, and output-file preservation laws |
| PersistentRewriteCompactionSpec.v | Complete | Char/vocab rewrite checkpoint snapshot preservation, sparse index bijection, post-checkpoint WAL-tail replay, and failed publication dirty/WAL retention laws |
| SubstringSearchSpec.v | Complete | Exact substring candidate, occurrence-position, and limited-result laws |
| ScdawgOccurrenceSpec.v | Complete | SCDAWG forward traversal, left-extension closure, `locations`, and `freq` occurrence exactness laws |
| FuzzyCandidateCoverageSpec.v | Complete | WallBreaker query-piece pigeonhole and fuzzy candidate coverage laws |
| SerializationRoundtripSpec.v | Complete | Public serializer membership/value roundtrip, legacy value-dropping, gzip/protobuf feature-codec, and fail-closed malformed-payload laws |
| ARTrieSpec.v | Complete (0 Admitted) | ARTrie specification incl. normalized checked construction and insert/delete correctness theorems |
| ReplicatedMapSpec.v | Complete | Replicated put/remove log replay over the map-entry reference model |
| DictionaryNodeReopenTraversalSpec.v | Complete (0 Admitted) | Faulting `DictionaryNode` traversal is residency-invariant (equals the snapshot regardless of swizzled children), reopen preserves it, `edges` enumerates all children, and the non-faulting walk is sound but strictly incomplete over swizzled children |
| PersistentCharEpochReclamationSpec.v | Complete (0 Admitted) | Eviction-vs-walk EBR: no active reader observes a freed node, preserved as a state invariant of the gated unlink → retire → drain → free protocol |
| StructuralInvariants.v | Complete (0 Admitted) | Structural invariants |
| TransitionInvariants.v | Complete (0 Admitted) | Node transition proofs (corrected `_after_insert` / `_with_lower_bound` variants) |
| ArenaInvariants.v | Complete | Arena allocation invariants |
| SequentialSiblingsInvariants.v | Complete | Sibling-list invariants |
| FileSystem.v | Complete | POSIX filesystem model |
| ArenaManager.v | Complete | Arena allocator model |
| SequentialSiblings.v | Complete | Sibling-list operations |
| Proofs/FileSystemSafety.v | Complete | TOCTOU safety |
| Proofs/MapRefinement.v | Complete | ARTrie refines Map ADT |
| Proofs/StructuralPreservation.v | Complete | Full checked-builder and checked-operation structural preservation proofs |
| Proofs/ByzantineRecovery.v | Complete | Fail-closed bounded Byzantine storage recovery |
| Proofs/CertifiedReference.v | Complete | Certified reference map boundary |
| Proofs/HotStuffSafety.v | Complete | Replicated committed logs replay over prefix-compatible states |
| Proofs/ProofCarryingExtraction.v | Complete | Proof-carrying trace checker correctness and invalid-step rejection |

## Relationship to Implementation

The formal specifications model the key components of the Rust implementation:

| Specification | Rust Source |
|---------------|-------------|
| `ARTrieTypes.tla` | `src/persistent_artrie/nodes/mod.rs` |
| `WAL.tla` | `src/persistent_artrie_core/wal.rs` and `src/persistent_artrie_core/wal/*.rs` |
| `Concurrency.tla` | `src/persistent_artrie_core/concurrency.rs` |
| `CrashRecovery.tla` | `src/persistent_artrie_core/recovery.rs` |
| `DocumentTransactions.tla` | `src/persistent_artrie/document_tx.rs` and `src/persistent_artrie/transactions.rs` |
| `AsyncWalGroupCommit.tla` | `src/persistent_artrie_core/group_commit.rs`, `src/persistent_artrie_core/wal/async_writer.rs`, and `tests/persistent_public_lifecycle_correspondence.rs` |
| `VersionLifecycle.tla` | `src/persistent_artrie_core/version_gc.rs` |
| `DurabilityFrontier.tla` | `src/persistent_artrie_core/{group_commit.rs,version_gc.rs,wal/async_writer.rs}` and `tests/persistent_artrie_loom_correspondence.rs` |
| `PointerOwnership.tla` | `src/persistent_artrie_core/swizzled_ptr.rs`, `src/persistent_artrie_char/types.rs`, `src/persistent_vocab_artrie/types.rs`, unsafe `Send`/`Sync` impl surfaces, and `tests/persistent_artrie_formal_correspondence.rs` |
| `VocabPersistenceOwnership.tla` | `src/persistent_vocab_artrie/{mod.rs,disk_io.rs}`, `tests/persistent_artrie_formal_correspondence.rs`, and crate-internal vocab eviction tests |
| `MmapBlockStorage.tla` | `src/persistent_artrie_core/{disk_manager,block_storage}.rs` and `tests/persistent_artrie_storage_correspondence.rs` |
| `StorageSyscallOutcome.tla` | `src/persistent_artrie_core/{io_uring_disk_manager.rs,wal/sync_backend.rs,wal/async_writer.rs}`, `tests/persistent_artrie_formal_correspondence.rs`, and optional io_uring completion contract tests |
| `IoUringFixedBufferOwnership.tla` | `src/persistent_artrie_core/{block_storage,buffer_manager,io_uring_disk_manager}.rs`, `tests/unsafe_boundary_contracts.rs`, and optional io_uring storage correspondence tests |
| `IoUringSqeCqeLifecycle.tla` | `src/persistent_artrie_core/io_uring_disk_manager.rs`, the io_uring completion paths, temporary aligned-buffer handling, and optional io_uring storage correspondence tests |
| `LockFreeARTrieLinearizability.tla` | `src/persistent_artrie/{lockfree_cas.rs,nodes/atomic_ptr.rs}` and `tests/persistent_artrie_loom_correspondence.rs` |
| `LockFreeIndexedOverlay.tla` | `src/persistent_artrie_char/lockfree_cas.rs`, `src/persistent_vocab_artrie/lockfree_cas.rs`, and `tests/persistent_artrie_loom_correspondence.rs` |
| `LockFreeCounterMergeAtomicity.tla` | `src/persistent_artrie{,_char}/lockfree_cas.rs`, lock-free persistent-node counter cells, `tests/persistent_lockfree_merge_correspondence.rs`, and `tests/persistent_artrie_loom_correspondence.rs` |
| `ConcurrentCheckpointPublication.tla` | `src/persistent_vocab_artrie/concurrent.rs`, `tests/concurrent_checkpoint_publication_correspondence.rs`, and `tests/persistent_artrie_loom_correspondence.rs` |
| `SharedPersistentConcurrency.tla` | `src/persistent_artrie/shared_trait_impl.rs`, `src/persistent_artrie_char/mod.rs`, `src/persistent_vocab_artrie/mod.rs`, and `tests/persistent_shared_concurrency_correspondence.rs` |
| `PublicDurabilityPolicy.tla` | `src/persistent_artrie/{persistence_api.rs,mutation_core.rs,mutation_api.rs,atomic_ops.rs,document_tx.rs,lockfree_cas.rs}`, `src/persistent_artrie_char/wal_helpers.rs`, `src/persistent_vocab_artrie/mutation_api.rs`, and `tests/persistent_public_durability_policy_correspondence.rs` |
| `ConcurrentVocabLinearizability.tla` | `src/persistent_vocab_artrie/concurrent.rs`, `tests/concurrent_checkpoint_publication_correspondence.rs`, and history-checking Loom tests in `tests/persistent_artrie_loom_correspondence.rs` |
| `EpochCheckpointRecovery.tla` | `src/persistent_artrie_char/{epoch_checkpointing.rs,wal_helpers.rs,persist.rs}`, `src/persistent_artrie_core/epoch.rs`, and `tests/epoch_checkpoint_recovery_correspondence.rs` |
| `PersistentCharBulkMutationRecovery.tla` | `src/persistent_artrie_char/prefix_api.rs`, `src/persistent_artrie{,_char}/atomic_ops.rs`, and `tests/persistent_bulk_mutation_correspondence.rs` |
| `PersistentTransactionIncrementRecovery.tla` | `src/persistent_artrie{,_char}/{document_tx.rs,transactions.rs}`, `src/persistent_artrie_char/{atomic_ops.rs,mmap_ctor.rs,io_uring_ctor.rs}`, shared recovery replay, and `tests/persistent_transaction_increment_correspondence.rs` |
| `ByzantineStorage.tla` | authenticated WAL/storage recovery filtering in `src/persistent_artrie_core/recovery.rs` |
| `BackgroundWorkerLifecycle.tla` | Background-daemon shutdown protocol after the Weak-handle thread-leak fix: `src/persistent_artrie_core/eviction/coordinator.rs` (`shutdown`/`Drop`, Weak worker + 100 ms poll), `wal/async_writer.rs` (`SegmentSyncManager::stop`/`Drop`, `AsyncWalWriter::stop_sync`), `persistent_artrie{,_char}` `close()`/`Drop`; `tests/persistent_{char,byte}_thread_lifecycle.rs`, `tests/persistent_char_thread_lifecycle_proptest.rs`, `tests/persistent_worker_lifecycle_loom.rs`, and `rocq/WorkerLifecycle.v` |
| `HotStuffConsensus.tla` | proof/model-only Byzantine quorum safety boundary |
| `NodeTypes.v` | `src/persistent_artrie/nodes/*.rs` |
| `Bucket.v` | `src/persistent_artrie/bucket.rs` |
| `HotStuff.v` / `HotStuffSafety.v` | proof/model-only replicated log safety boundary |
| `ReplicatedMapSpec.v` | command-log replay model exercised by trace correspondence tests |
| `DictionaryLawSpec.v` | `tests/dictionary_law_correspondence.rs`, public `Dictionary` / `MappedDictionary` / zipper / bijective APIs, and SCDAWG exact-value storage |
| `DynamicDawgMutationSpec.v` | `src/dawg_core.rs`, `src/dynamic_dawg.rs`, `src/dynamic_dawg_char.rs`, and `tests/dynamic_dawg_mutation_correspondence.rs` for byte/Unicode DynamicDawg mutation, batch, compaction, minimization, value preservation, and copy-on-write shared-node updates |
| `DynamicDawgU64Spec.v` | `src/dynamic_dawg_u64.rs`, `src/dynamic_dawg_u64_zipper.rs`, and `tests/dynamic_dawg_u64_correspondence.rs` for u64 sequence mutation, value preservation, string/f64 adapter refinement, iterator/zipper exactness, and bounded read-snapshot safety |
| `DoubleArrayTrieSpec.v` | `tests/double_array_trie_correspondence.rs`, byte and Unicode `DoubleArrayTrie` construction, lookup, child traversal, zipper values, and duplicate normalization |
| `ZipperLanguageSpec.v` | `tests/zipper_language_correspondence.rs`, public `DictZipper` / `ValuedDictZipper` traversal, iterator, prefix/excluding, set-combinator, value-diff, suffix, SCDAWG, and persistent zipper APIs |
| `ValuedSetCombinatorSpec.v` | `src/union_zipper/*`, `src/intersection_zipper.rs`, and `tests/valued_set_combinator_correspondence.rs` for ordered duplicate-value merge strategies, lattice values, Unicode/byte/DynamicDawg zippers, and `lling-llang` semiring join |
| `BloomFilterSpec.v` | `src/bloom_filter.rs`, Bloom-backed `DynamicDawg` lookup, and `tests/bloom_filter_correspondence.rs` for no-false-negative lookup rejection, clear/reinsert traces, byte/string refinement, duplicate inserts, and parameter normalization |
| `PersistentMergeSpec.v` | `tests/persistent_merge_correspondence.rs`, `PersistentARTrie::iter_prefix_from_cursor`, `merge_from_batched`, `merge_from_batched_grouped`, and `SharedARTrieParallelExt::merge_from_parallel` |
| `PersistentPrefixSpec.v` | `tests/persistent_prefix_correspondence.rs`, `tests/persistent_bulk_mutation_correspondence.rs`, `PersistentARTrieChar::iter_prefix*`, `iter_prefix_with_*_arena`, `remove_prefix`, `remove_prefix_batched`, and byte/char checked `increment`/`fetch_add` overflow |
| `PersistentReadTraversalSpec.v` | `tests/persistent_read_snapshot_correspondence.rs`, byte `PersistentARTrie::iter*`/`iter_prefix*`, char `PersistentARTrieChar::iter*`/`iter_prefix*`, and vocab `iter_terms*` checkpoint/reopen traversal |
| `DictionaryNodeReopenTraversalSpec.v` + `PublicDictionaryNodeTraversal.tla` | `tests/dictionary_node_reopen_traversal_correspondence.rs`, char `PersistentARTrieCharNode::{transition,edges,is_final,value}` faulting after checkpoint/reopen (the `DictionaryNode` walk a transducer drives) |
| `PersistentCharEpochReclamationSpec.v` + `EvictionWalkEBR.tla` | `tests/persistent_char_ebr_correspondence.rs` (threaded, TSan/ASan) + `tests/persistent_artrie_loom_correspondence.rs` (swizzle-install race), char `evict_char_nodes`/`reclaim::CharRetireList` + `CharWalkGuard` epoch pin |
| `PathMapFactorySpec.v` | `tests/pathmap_factory_correspondence.rs`, optional `PathMapDictionary`, `PathMapDictionaryChar`, `PathMapZipper`, `MutableDictionary`/`MutableMappedDictionary` impls, and `DictionaryFactory` dispatch under `pathmap-backend` |
| `RelativeEncodingSpec.v` | `tests/relative_encoding_correspondence.rs`, byte and char `relative_encoding` checked APIs, `serialization.rs`, `serialization_char.rs`, and persistent fail-closed child-pointer deserialization |
| `ArenaReservationSpec.v` | `tests/arena_manager_correspondence.rs`, byte and char `ArenaManager` allocation/reservation/update/dirty-flush/load paths |
| `DedupArenaSpec.v` | `tests/dedup_arena_correspondence.rs`, byte and char `DeduplicatingArenaManager`, `NodeDeduplicator`, and `BatchDeduplicator` cache/reuse paths |
| `RootDescriptorReopenSpec.v` | `tests/root_descriptor_reopen_correspondence.rs`, byte and char root descriptor publication/loading, arena-count validation, WAL checkpoint skip-threshold fallback, and char lazy-load fail-closed query paths |
| `PersistentLazyMutationSpec.v` | `tests/persistent_lazy_mutation_correspondence.rs`, char lazy mutation preflight, WAL append ordering, no-op duplicate insert behavior, failed insert/value-insert/remove errors, and replay after successful lazy mutation |
| `PersistentWalAtomicitySpec.v` | `tests/persistent_wal_atomicity_correspondence.rs`, `tests/persistent_transaction_increment_correspondence.rs`, byte and char value-write serialization failures, WAL-before-mutation ordering for atomic writes, checked transaction increment overflow, document commit ordering, and replay after successful atomic writes |
| `SharedPersistentConcurrencySpec.v` | `tests/persistent_shared_concurrency_correspondence.rs`, byte/char/vocab shared public checkpoint/write/sync/reopen schedules, and `SharedARTrie::checkpoint` lock publication |
| `PublicDurabilityPolicySpec.v` | `tests/persistent_public_durability_policy_correspondence.rs`, byte/char/vocab public mutation paths, byte transaction commit sync, explicit async sync handles, and `DurabilityPolicy` full-vs-weak acknowledgement semantics |
| `PersistentPublicWalLifecycleSpec.v` | `tests/persistent_public_lifecycle_correspondence.rs`, byte/char/vocab public open after synced WAL tails and checkpoint-plus-tail replay, and group-commit returned-LSN/durable-record correspondence |
| `PersistentVocabWalAtomicitySpec.v` | `tests/persistent_vocab_wal_atomicity_correspondence.rs`, `src/persistent_vocab_artrie/{mutation_api,disk_io,query_api}.rs`, and public vocab mutation APIs for WAL-before-visible-mutation ordering, manual-index replay, duplicate batch index stability, collision/reindex rejection, and exact reverse-index membership |
| `PersistentVocabCheckpointSpec.v` | `tests/persistent_vocab_checkpoint_correspondence.rs`, `src/persistent_vocab_artrie/{persistence_api,mmap_ctor,io_uring_ctor}.rs`, and WAL LSN/truncation paths for checkpoint/reopen bijection, replay retention until checkpoint, post-checkpoint insert replay, `rotate_wal`/`sync_to_disk` non-checkpoint behavior, and Bloom/reverse-index sidecar rebuild |
| `PersistentCheckpointRetentionSpec.v` | `tests/checkpoint_retention_correspondence.rs`, byte and char corruption rebuild from retained archive/pending/active WAL segments, active-tail preservation, batch insert replay, and remove replay |
| `PersistentDirtyCheckpointSpec.v` | `tests/dirty_checkpoint_correspondence.rs`, byte and char dirty-slot write/sync retry behavior, late slot-tracking coverage, and descriptor publication before WAL truncation |
| `PersistentWalSegmentLifecycleSpec.v` | `tests/wal_segment_lifecycle_correspondence.rs`, `src/persistent_artrie_core/wal/{writer,async_writer}.rs`, and `src/persistent_artrie_core/recovery.rs` LSN-ordered segment collection, monotonic rotation/reopen LSN and synced-frontier continuation, archive pruning to `max_segments`, and collision-resistant archive segment names |
| `PersistentRecoveryPlannerSpec.v` | `tests/recovery_planner_correspondence.rs`, `src/persistent_artrie_core/recovery.rs`, and byte/char recovery constructors for durable-prefix replay after corrupt WAL records and corruption rebuild mode parity |
| `PersistentRecoveryReplayCompletenessSpec.v` | `tests/recovery_replay_completeness_correspondence.rs`, `tests/persistent_transaction_increment_correspondence.rs`, `src/persistent_artrie_core/recovery.rs`, byte `mmap`/`io_uring` replay application, and char archive/recovery-manager replay for complete mutating WAL variant coverage, no-WAL replay, corrupt/invalid-arithmetic durable-prefix stopping |
| `PersistentCompactionSpec.v` | `tests/persistent_compaction_correspondence.rs` and `src/persistent_artrie/compaction_impl.rs` for byte persistent compaction exact snapshot preservation, term-only and non-UTF8 byte-key copying, temp WAL sidecar collision rejection, stale original WAL cleanup after finalization, and stale-WAL backup recovery across the data-file rename |
| `PersistentRewriteCompactionSpec.v` | `tests/persistent_rewrite_compaction_correspondence.rs`, `src/persistent_artrie_char/persist.rs`, and `src/persistent_vocab_artrie/persistence_api.rs` for char/vocab rewrite checkpoint preservation, post-checkpoint WAL-tail replay, and failed publication dirty/WAL retention |
| `SubstringSearchSpec.v` | `tests/substring_candidate_correspondence.rs`, public `SubstringDictionary` APIs for byte and Unicode SCDAWG candidate generation |
| `ScdawgOccurrenceSpec.v` | `tests/scdawg_occurrence_correspondence.rs`, byte and Unicode SCDAWG `find`/`freq`/`locations`, handle-based `freq_at`/`locations_at`, and left-extension traversal |
| `FuzzyCandidateCoverageSpec.v` | `tests/fuzzy_candidate_coverage_correspondence.rs`, WallBreaker-style query-piece candidate coverage over byte and Unicode SCDAWG substring APIs |
| `SerializationRoundtripSpec.v` | `tests/serialization_correspondence.rs`, `tests/serialization_value_roundtrip.rs`, `tests/protobuf_compression_correspondence.rs`, and public Bincode/JSON/plaintext/gzip/protobuf serializer APIs under `--features serialization` and `--features "serialization protobuf compression"` |
| `ByzantineRecovery.v` | `src/persistent_artrie_core/recovery.rs` authenticated-record boundary |
| `CertifiedReference.v` / `ProofCarryingExtraction.v` | reference and certified-trace boundaries exercised by `tests/persistent_artrie_formal_correspondence.rs` |

The executable correspondence harness covers:

- bucket sortedness, split/merge preservation, and binary-search partitions;
- bucket page roundtrip plus malformed page rejection;
- WAL header and record codec roundtrips plus malformed/truncated WAL rejection;
- durable-prefix behavior for truncated WAL payloads and partial trailing
  record headers;
- end-to-end `PersistentARTrie::open` recovery for header-only WALs, every
  complete record-boundary prefix, and torn payloads after a durable prefix;
- trie traces checked against `BTreeMap`, including deterministic large and
  reopen/recovery traces;
- document transaction staged-write visibility and WAL transaction replay
  where committed transactions apply and incomplete transactions are ignored;
- WAL CRC fail-closed behavior;
- version-GC reader protection;
- group-commit LSN publication matching durable WAL record LSNs;
- durability-frontier schedule checks for contiguous synced-LSN publication,
  no early group-commit acknowledgements, checkpoint publication, and
  reader-guarded VersionGc reclamation;
- proof-carrying trace replay, including corrupt-certificate rejection;
- public dictionary law traces for static and dynamic in-memory backends,
  Unicode backends, set-zippers, suffix substring semantics, bijective maps,
  `PersistentARTrie`, and `PersistentVocabARTrie`;
- DynamicDawg mutation traces for byte and Unicode insert-with-value,
  update-or-insert, remove, compact, minimize, extend, remove-many,
  value-preserving rebuilds, copy-on-write after minimization, and
  Bloom-filter-backed lookup;
- Bloom filter traces for no-false-negative byte/string insertion,
  deterministic reference-bitset correspondence, clear/reinsert behavior,
  duplicate inserts, parameter normalization, and Bloom-backed DAWG lookup;
- double-array trie traces for byte and Unicode construction, lookup,
  mapped-value last-write-wins normalization, node walks, child-edge iteration,
  zipper paths, and sorted Unicode construction;
- zipper language traces for byte and Unicode zippers, valued iteration,
  prefix/excluding filters, set and value-diff zipper combinators, suffix
  automaton substring languages, SCDAWG exact/substring queries, and
  persistent byte/char zippers;
- valued set-combinator traces for byte, Unicode, and DynamicDawg zippers,
  union/intersection first-wins, last-wins, custom sum, lattice join/meet, set
  values, empty/disjoint domains, navigation-vs-iteration agreement, and
  feature-gated `lling-llang` semiring join behavior;
- persistent merge traces for cursor pagination, ordinary batched merge,
  arena-grouped batched merge, and feature-gated parallel merge against
  `BTreeMap` reference merges;
- persistent char prefix traces for ASCII/Unicode/empty prefixes, valued and
  arena-aware views, idempotent ordinary/batched deletion, zero batch-size
  normalization, and sync/reopen persistence;
- public read traversal traces for byte/char/vocab all-term and prefix
  iteration after checkpoint/reopen, valued snapshot preservation, and
  fail-closed lazy-load traversal errors without WAL append;
- PathMap/factory traces for byte PathMap map and zipper traversal,
  PathMapChar Unicode siblings sharing UTF-8 prefixes, mapped mutation/union,
  and all factory backends under `pathmap-backend`;
- relative encoding traces for byte/char child-pointer roundtrip,
  same-arena full-encoding fallback, malformed decode rejection, sequential
  overflow rejection, and char v2 deserialization corruption handling;
- deduplicating arena traces for byte/char equal-payload reuse, stale-cache
  fail-closed allocation, verify-false compatibility behavior, direct
  allocation bypass, cache clear, and batch take semantics;
- root descriptor/reopen traces for byte and char checkpoint/reopen,
  malformed root kinds, invalid arena counts, checkpoint skip-threshold
  fallback, and lazy-load error fail-closed public reads;
- persistent lazy mutation traces for char insert, value insert, duplicate
  insert, remove, no-WAL-on-error behavior, and successful replay after lazy
  reopen;
- persistent WAL atomicity traces for byte and char value serialization
  failures, atomic write no-mutation-on-error behavior, document commit
  fail-closed behavior, and replay after successful atomic writes;
- persistent vocabulary WAL atomicity traces for single inserts, manual index
  inserts, duplicate batch terms, duplicate terms across reopen, sparse manual
  index bijections, and collision/reindex rejection without mutation;
- checkpoint/WAL retention traces for byte and char corruption rebuild,
  including active WAL tails after the last checkpoint, batch inserts, and
  removes;
- dirty checkpoint publication traces for byte and char dirty-slot retry after
  injected write/sync failure, late slot-tracking coverage for already dirty
  arenas, and descriptor publication before WAL truncation with a replayed WAL
  tail;
- WAL segment lifecycle traces for LSN-ordered collection even when archive
  filenames disagree, monotonic async rotation/reopen LSN and synced-frontier
  continuation, and archive pruning to the configured segment limit;
- recovery planner traces for direct WAL rebuild, `RecoveryManager`,
  incremental recovery, and byte and char corruption rebuilds stopping at the
  first corrupt WAL record rather than applying later suffix records;
- recovery replay completeness traces for shared operation expansion, byte
  corruption rebuild replay of batch increments and CAS, char archive replay of
  all mutating variants without active-WAL echoing, and corrupt archive-prefix
  stopping;
- transaction increment traces for byte/char overflow fail-closed behavior,
  checked char aggregate overflow, and byte/char `BatchIncrement` replay
  stopping before overflowed suffix records;
- substring candidate traces for byte and Unicode SCDAWG exact substring
  search, including repeated/overlapping occurrences, duplicate-term
  suppression, limited-result prefixes, and explicit empty-pattern behavior;
- SCDAWG occurrence traces for byte and Unicode `find`, `freq`, `locations`,
  handle-based `freq_at`/`locations_at`, left-extension traversal, duplicate
  value updates, and repeated/overlapping occurrence sets;
- fuzzy candidate coverage traces for byte and Unicode SCDAWG APIs, including
  substitution/insertion/deletion examples, deterministic generated
  substitution matrices, and short-query scope checks;
- serialization roundtrip traces for Bincode, JSON, and plaintext serializers,
  including byte and Unicode value-bearing backends, SCDAWG value paths,
  generated cases, legacy term-only value dropping, and malformed-payload
  rejection;
- vocab checkpoint/reopen preservation for Unicode terms and duplicate inserts,
  sparse/manual indices, batch duplicates, post-checkpoint WAL replay,
  recovery WAL retention until checkpoint, non-checkpoint `rotate_wal` and
  `sync_to_disk`, missing/corrupt/stale reverse-index sidecars, missing/corrupt
  Bloom sidecars, failed sidecar publication dirty/WAL retention, direct
  `node_map`/parent-chain rebuild checks after reopen, heap-only shared-prefix
  `node_map` parent-chain liveness checks that caught duplicate `NodeRef`
  allocation for existing children, plus
  crate-internal eviction checks that reject parent eviction while a child is
  resident, remove stale `node_map` entries before dropping evicted leaves, and
  keep sibling queries on live nodes;
- unsafe-boundary regressions for swizzled-pointer transitions, atomic node
  pointer CAS ownership, optimistic-cell writer serialization, raw char and
  `VocabTrieNode` child ownership transfer/replacement/deep-clone paths,
  unique `get_or_create_child` mutation borrows, swizzled raw extraction only
  after confirmed in-memory state, lazy-load loser reclamation, explicit
  `Send`/`Sync` contract checks, fixed-buffer fallback when a backend does not
  support registration, fixed-capable BufferManager
  write-guard mutation and unregister-before-owner-drop behavior,
  unsafe-inventory contract-tag plus coverage/status drift, torn-WAL
  header/payload reopen, and persistent dictionary law traces.
- storage-boundary regressions for mmap block allocation uniqueness, sub-block
  bounds rejection, sync/reopen header checksum refresh, and raw-pointer
  bounds rejection, plus a Miri-friendly swizzled disk-pointer raw roundtrip
  and optional io_uring range rejection plus fixed-buffer registration
  lifetime and SQE/CQE completion lifecycle checks when that backend is
  enabled.
- fail-closed storage syscall outcome checks: failed WAL segment fsync does
  not advance `SegmentSyncManager::global_synced_lsn`, failed waits do not
  report the target LSN as durable, and optional io_uring completion helpers
  reject negative, short, and missing completions.
- bounded loom schedule checks for byte lock-free root CAS publication,
  duplicate insert linearization, insert/contains visibility, merge snapshot
  behavior, child-pointer Arc handoff, char value increments, char merge
  snapshots, checked counter overflow/merge failure preservation, vocab duplicate insert/index stability, sparse `next_index`
  behavior, vocab cache/root/persistent agreement, group-commit durable
  frontier publication, async WAL gap handling, checkpoint publication, and
  version-GC reader/reclaim races.
- public durability acknowledgement regressions for byte/char/vocab
  `Immediate` and `GroupCommit` mutation paths, byte transaction commit sync,
  explicit async sync handles, and `Periodic` non-overclaiming of the synced
  WAL frontier.

See [UNSAFE_BOUNDARY.md](UNSAFE_BOUNDARY.md) for the scoped Rust unsafe
boundary ledger, the checked unsafe-source inventory gate, and the remaining
proof obligations.

### Filesystem Operations Correspondence

This section documents the mapping between formal filesystem operations and Rust methods,
specifically addressing TOCTOU (Time-of-Check to Time-of-Use) safety.

#### Formal Model (FileSystem.v)

The formal model defines safe filesystem operations that avoid TOCTOU races:

```coq
(* Idempotent directory creation *)
Definition mkdir_all (fs : FileSystem) (path : Path) := ...

(* Atomic exclusive file creation *)
Definition open_create (fs : FileSystem) (path : Path) := ...

(* Atomic file open (existing only) *)
Definition open_existing (fs : FileSystem) (path : Path) := ...

(* TOCTOU-safe open or create pattern *)
Definition open_or_create_safe (fs : FileSystem) (path : Path) :=
  let fs' := mkdir_all fs (parent_dir path) in  (* 1. Ensure parent *)
  match open_existing fs' path with             (* 2. Try open *)
  | Ok f => Ok f
  | NotFound => open_create fs' path            (* 3. Fall back to create *)
  | err => err
  end.
```

#### Rust Implementation Mapping

| Formal Operation | Rust Implementation | Atomicity Guarantee |
|------------------|---------------------|---------------------|
| `mkdir_all` | `std::fs::create_dir_all()` | POSIX idempotent |
| `open_create` | `WalWriter::create()` with `create_new(true)` | `O_CREAT \| O_EXCL` |
| `open_existing` | `WalWriter::open()` without pre-check | Direct `open()` |
| `open_or_create_safe` | `WalWriter::open_or_create()` | mkdir_all + atomic fallback |

#### Error Type Mapping

| Formal Error | Rust Error | Notes |
|--------------|------------|-------|
| `FsError::Ok` | `Ok(...)` | Success case |
| `FsError::NotFound` | `WalError::NotFound` | File doesn't exist |
| `FsError::ParentNotFound` | `WalError::ParentNotFound` | Parent directory missing |
| `FsError::AlreadyExists` | `WalError::AlreadyExists` | Exclusive create failed |

#### TOCTOU Safety Proofs

The following theorems in `FileSystemSafety.v` establish TOCTOU safety:

1. **`open_or_create_safe_no_parent_error`**: Safe pattern never returns `ParentNotFound`
2. **`open_or_create_safe_always_ok`**: Safe pattern always succeeds (given sufficient permissions)
3. **`mkdir_all_idempotent`**: Directory creation is idempotent
4. **`vulnerable_can_fail_parent_not_found`**: Demonstrates that check-then-act patterns ARE vulnerable

The Rust implementation MUST use the safe patterns to satisfy these proofs:

```rust
// WRONG (vulnerable to TOCTOU):
if path.exists() {                    // RACE: check
    File::open(path)?                 // RACE: act - file may be deleted
} else {
    File::create(path)?               // RACE: act - file may be created
}

// CORRECT (matches formal model):
WalWriter::open_or_create(path)?      // Atomic pattern with proper fallback
```

## Abstractions

The following details are abstracted in the specifications:

- Exact byte layouts beyond the WAL header/record, bucket-page, root
  descriptor, and public serializer correspondence tests
- CRC32 internals beyond fail-closed corruption checks
- SIMD implementation details
- Memory allocation/deallocation
- File I/O buffering
- Kernel/filesystem internals behind storage syscalls; syscall outcomes are
  modeled at the write/sync completion boundary, but kernel implementation
  correctness remains trusted
- Thread scheduling details
- Byzantine networking/liveness, malicious CPU execution, and compromised
  cryptography; current Byzantine proofs cover storage filtering and bounded
  quorum/log safety
- Certified Rust/LLVM binary generation; current proofs certify the reference
  interface and proof-carrying replay checker, not compiled Rust artifacts
- Gzip/flate and prost internals, and cross-language protobuf implementation
  compatibility beyond libdictenstein's public codec semantics
- Full Rust memory-safety proof for every unsafe site; byte lock-free
  publication, char/vocab indexed overlays, lock-free counter merge atomicity,
  vocab persistence/eviction
  ownership, representative ARTrie pointer/concurrency, durability-frontier
  publication, persistent root descriptor/reopen, persistent lazy mutation,
  persistent WAL atomicity, persistent vocab WAL atomicity, persistent vocab
  checkpoint publication,
  checkpoint/WAL retention, dirty checkpoint publication, recovery planner
  durable-prefix replay, recovery replay completeness, mmap storage checks,
  storage syscall outcome checks,
  io_uring fixed-buffer registration,
  BufferManager fixed-buffer lifetime, and io_uring SQE/CQE completion
  checking are executable, the Miri gate runs under
  `FORMAL_MIRI_TOOLCHAIN=nightly` with strict provenance enabled by default,
  and the whole-crate unsafe inventory now enforces coverage/status metadata
  for every reviewed contract. `SwizzledPtr` now stores in-memory pointers in a
  provenance-preserving runtime slot instead of reconstructing them from raw
  integers; kernel syscall internals and fully mechanized Rust memory-safety
  proofs remain future proof targets

See [GAP_LEDGER.md](GAP_LEDGER.md) for the current scoped claims and remaining
proof obligations.

## Future Work

1. ~~**Operations Module**: Complete implementation of insert/delete in Rocq~~
   **Done.** `trie_insert` / `trie_delete` are defined in `Spec/ARTrieSpec.v`
   and proven correct under the `entries_of_trie_complete` hypothesis. The
   `Operations/` directory is reserved for future extraction of concrete
   imperative variants.
2. ~~**Map Refinement Proof**: Formal proof that ARTrie refines Map ADT~~
   **Done** — `Proofs/MapRefinement.v` defines `WFARTrie` + the
   `WFARTrieMapImpl` Instance with 3 `Qed`-closed theorems.
3. **Iris Integration**: Separation logic proofs for mutable state
4. **Liveness Proofs**: Complete TLA+ liveness verification
5. **Coverage**: Map TLA+ states to code coverage
6. **TLA+ state-dump refresh**: state-space dumps under
   `formal-verification/tla+/states/` were last regenerated 2026-01-24; the
   main composed PART model is unchanged since.
7. **Unsafe-boundary assurance**: initial ledger and representative ARTrie
   regressions are in [UNSAFE_BOUNDARY.md](UNSAFE_BOUNDARY.md). Mmap storage,
   byte lock-free publication, char/vocab indexed overlays, lock-free counter
   merge atomicity, persistent root descriptor/reopen, persistent lazy
   mutation, persistent WAL atomicity, persistent vocab WAL atomicity,
   persistent vocab checkpoint publication,
   concurrent checkpoint publication, shared persistent public API concurrency,
   public durability acknowledgement policy,
   checkpoint/WAL retention, dirty checkpoint
   publication, recovery planner durable-prefix replay, recovery replay
   completeness, and the
   durability/reclamation frontier now have bounded models or Rocq laws plus
   executable correspondence tests. Vocab
   persistence/reopen and eviction invalidation have both a bounded model and
   Rust/Miri correspondence checks. Storage syscall outcome handling, io_uring
   fixed-buffer ownership, and SQE/CQE completion checking now have bounded
   models, harness wiring, and CI jobs. `SwizzledPtr` strict-provenance Miri
   coverage is now wired into the harness. Next, keep the
   Miri/io_uring/scheduled TLC jobs green and deepen syscall/kernel and unsafe
   raw-pointer/`Send`/`Sync` contracts where the library still relies on
   trusted Rust or kernel behavior.

## References

- [TLA+ Specification Language](https://lamport.azurewebsites.net/tla/tla.html)
- [The Rocq Prover](https://rocq-prover.org/)
- [Adaptive Radix Trees](https://db.in.tum.de/~leis/papers/ART.pdf)
- [ARIES Recovery Algorithm](https://cs.stanford.edu/people/chr101/cs345/aries.pdf)

## License

MIT License - Copyright (c) 2026 F1r3fly.io
