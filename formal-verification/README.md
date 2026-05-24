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
│   ├── ByzantineStorage.tla   # Authenticated committed-record recovery
│   ├── HotStuffConsensus.tla  # Bounded Byzantine quorum/log safety model
│   ├── NodeTransitions.tla    # Node growth transitions
│   ├── EpochCheckpoint.tla    # Epoch lifecycle
│   ├── PART.tla               # Main composed specification
│   ├── PART.cfg               # TLC configuration (no crash)
│   └── PART_crash.cfg         # TLC configuration (with crash)
│
└── rocq/                      # Rocq/Coq proofs (35 .v files, 14,230+ LOC,
    │                            650+ theorem/lemma propositions,
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
    │   ├── PersistentPrefixSpec.v # Persistent char prefix iteration/removal laws
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
| Async WAL Durability | Safety | Group commit publishes a durable LSN prefix |
| Version Reclamation | Safety | Reclaimed versions are not referenced by readers |
| Durability Frontier | Safety | Synced LSNs, checkpoints, recovery, and VersionGc reclamation stay within the durable prefix |
| Raw Pointer Ownership | Safety | Raw slot pointers, node-map entries, and borrows do not outlive in-memory ownership |
| Vocab Persistence Ownership | Safety | Vocab checkpoint/reopen preserves stable term-index bijections, and eviction invalidates node-map raw entries before drop |
| Mmap Block Storage | Safety | Allocation/remap protocol maps blocks before successful access |
| Storage Syscall Outcomes | Safety | Short/error/interrupted/cancelled/missing write or sync outcomes cannot advance the reported durable prefix; recovery applies only the durable prefix |
| Byte Lock-Free ARTrie Publication | Safety | Root CAS, cache publication, contains, and merge snapshot points are linearizable |
| Indexed Lock-Free Overlays | Safety | Char increments preserve value sums, and vocab CAS inserts preserve stable unique indices while allowing sparse claims |
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

# Bounded focused models added in 2026-05-22/2026-05-23 refreshes
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
tag is defined in `formal-verification/UNSAFE_CONTRACTS.tsv`. The default
harness includes the DynamicDawg mutation, DynamicDawgU64 sequence, Bloom filter,
double-array trie, valued set-combinator, persistent merge, persistent prefix,
substring, SCDAWG
occurrence, and fuzzy candidate coverage targets plus the feature-gated valued semiring and public
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
13. **Substring Candidate Correctness**: non-empty exact substring queries return
   precisely the reference `(term, position, length)` candidate set needed by
   fuzzy-search transducers
   — see `Spec/SubstringSearchSpec.v`
14. **SCDAWG Occurrence Construction**: forward traversal, left-extension
   closure, handle-based `locations_at`, public `locations`, and `freq` refine
   the same reference occurrence relation
   — see `Spec/ScdawgOccurrenceSpec.v`
15. **Fuzzy Candidate Coverage**: a `budget + 1` nonempty query-piece split
   leaves at least one exact piece candidate for any term whose edit witness
   damages at most `budget` pieces
   — see `Spec/FuzzyCandidateCoverageSpec.v`
15. **Serialization Roundtrip Correctness**: public serializers preserve
   term-membership, mapped lookup values, gzip wrapper payloads, protobuf graph
   formats, DAT protobuf terms, and suffix-automaton source languages according
   to their wire format, and invalid payloads fail closed
   — see `Spec/SerializationRoundtripSpec.v`

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

As of 2026-05-24: all modules **Complete** — 0 `Admitted` / 0 `Axiom` /
0 `Parameter` across the 34 .v files (verified by grep, see
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
| PersistentPrefixSpec.v | Complete | Persistent char prefix filter, valued/arena projection, ordinary removal, and batched-removal equivalence laws |
| SubstringSearchSpec.v | Complete | Exact substring candidate, occurrence-position, and limited-result laws |
| ScdawgOccurrenceSpec.v | Complete | SCDAWG forward traversal, left-extension closure, `locations`, and `freq` occurrence exactness laws |
| FuzzyCandidateCoverageSpec.v | Complete | WallBreaker query-piece pigeonhole and fuzzy candidate coverage laws |
| SerializationRoundtripSpec.v | Complete | Public serializer membership/value roundtrip, legacy value-dropping, gzip/protobuf feature-codec, and fail-closed malformed-payload laws |
| ARTrieSpec.v | Complete (0 Admitted) | ARTrie specification incl. normalized checked construction and insert/delete correctness theorems |
| ReplicatedMapSpec.v | Complete | Replicated put/remove log replay over the map-entry reference model |
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
| `AsyncWalGroupCommit.tla` | `src/persistent_artrie_core/group_commit.rs` and `src/persistent_artrie_core/wal/async_writer.rs` |
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
| `ByzantineStorage.tla` | authenticated WAL/storage recovery filtering in `src/persistent_artrie_core/recovery.rs` |
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
| `PersistentPrefixSpec.v` | `tests/persistent_prefix_correspondence.rs`, `PersistentARTrieChar::iter_prefix*`, `iter_prefix_with_*_arena`, `remove_prefix`, and `remove_prefix_batched` |
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
  direct `node_map`/parent-chain rebuild checks after reopen, plus
  crate-internal eviction checks that reject parent eviction while a child is
  resident, remove stale `node_map` entries before dropping evicted leaves, and
  keep sibling queries on live nodes;
- unsafe-boundary regressions for swizzled-pointer transitions, atomic node
  pointer CAS ownership, optimistic-cell writer serialization, raw char and
  `VocabTrieNode` child ownership transfer/replacement/deep-clone paths,
  swizzled raw extraction only after confirmed in-memory state, lazy-load loser
  reclamation, explicit `Send`/`Sync` contract checks, fixed-buffer fallback
  when a backend does not support registration, fixed-capable BufferManager
  write-guard mutation and unregister-before-owner-drop behavior,
  unsafe-inventory contract-tag drift, torn-WAL header/payload reopen, and
  persistent dictionary law traces.
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
  snapshots, vocab duplicate insert/index stability, sparse `next_index`
  behavior, vocab cache/root/persistent agreement, group-commit durable
  frontier publication, async WAL gap handling, checkpoint publication, and
  version-GC reader/reclaim races.

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

- Exact byte layouts beyond the WAL header/record, bucket-page, and public
  serializer correspondence tests
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
  publication, char/vocab indexed overlays, vocab persistence/eviction
  ownership, representative ARTrie pointer/concurrency, durability-frontier
  publication, mmap storage checks, storage syscall outcome checks, io_uring
  fixed-buffer registration, BufferManager fixed-buffer lifetime, and io_uring
  SQE/CQE completion checking are executable, but kernel syscall internals and whole-crate unsafe
  `Send`/`Sync` contracts remain a future proof target

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
   byte lock-free publication, char/vocab indexed overlays, and the
   durability/reclamation frontier now have bounded models and executable
   correspondence tests. Vocab persistence/reopen and eviction invalidation
   have both a bounded model and Rust correspondence checks. Storage syscall
   outcome handling, io_uring fixed-buffer ownership, and SQE/CQE completion
   checking now have bounded models, harness wiring, and CI jobs. Next, keep
   the Miri/io_uring/scheduled TLC jobs green and deepen the remaining
   syscall/kernel and unsafe `Send`/`Sync` contracts where the library still
   relies on trusted Rust or kernel behavior.

## References

- [TLA+ Specification Language](https://lamport.azurewebsites.net/tla/tla.html)
- [The Rocq Prover](https://rocq-prover.org/)
- [Adaptive Radix Trees](https://db.in.tum.de/~leis/papers/ART.pdf)
- [ARIES Recovery Algorithm](https://cs.stanford.edu/people/chr101/cs345/aries.pdf)

## License

MIT License - Copyright (c) 2026 F1r3fly.io
