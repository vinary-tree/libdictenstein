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
│   ├── MmapBlockStorage.tla   # mmap allocation/remap/access model
│   ├── LockFreeARTrieLinearizability.tla # Byte lock-free root/cache model
│   ├── ByzantineStorage.tla   # Authenticated committed-record recovery
│   ├── HotStuffConsensus.tla  # Bounded Byzantine quorum/log safety model
│   ├── NodeTransitions.tla    # Node growth transitions
│   ├── EpochCheckpoint.tla    # Epoch lifecycle
│   ├── PART.tla               # Main composed specification
│   ├── PART.cfg               # TLC configuration (no crash)
│   └── PART_crash.cfg         # TLC configuration (with crash)
│
└── rocq/                      # Rocq/Coq proofs (22 .v files, ~8,107 LOC,
    │                            313 theorem/lemma propositions,
    │                            0 Admitted / 0 Axiom / 0 Parameter)
    ├── Makefile               # Build system
    ├── Spec/                  # Specifications
    │   ├── MapSpec.v          # Abstract map specification
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
| Mmap Block Storage | Safety | Allocation/remap protocol maps blocks before successful access |
| Byte Lock-Free ARTrie Publication | Safety | Root CAS, cache publication, contains, and merge snapshot points are linearizable |
| Byzantine Storage Filtering | Safety | Recovery applies only committed authenticated records |
| Byzantine Quorum Safety | Safety | HotStuff/PBFT-style committed logs remain prefix-compatible despite one Byzantine voter |
| Proof-Carrying Replay | Safety | Certified traces replay to the reference map semantics and reject invalid steps |

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
tlc -workers 1 -config MmapBlockStorage.cfg MmapBlockStorage.tla
tlc -workers 1 -config LockFreeARTrieLinearizability.cfg LockFreeARTrieLinearizability.tla
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
for the focused TLA+ models. Set `RUN_TLC=1` to also run the bounded TLC model
checks:

```bash
RUN_TLC=1 scripts/verify-formal-correspondence.sh
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

As of 2026-05-22: all modules **Complete** — 0 `Admitted` / 0 `Axiom` /
0 `Parameter` across the 22 .v files (verified by grep, see
[VERIFICATION_RESULTS.md](VERIFICATION_RESULTS.md) for the per-file tally).

| Module | Status | Description |
|--------|--------|-------------|
| Key.v | Complete (0 Admitted) | Key representation and operations |
| NodeTypes.v | Complete | Node type definitions |
| Bucket.v | Complete (0 Admitted) | Bucket operations plus sorted-bucket contract and binary-search partition proof |
| HotStuff.v | Complete | Quorum arithmetic plus HotStuff/PBFT-style honest-intersection log safety |
| PathCompression.v | Complete (0 Admitted) | Prefix matching |
| MapSpec.v | Complete | Abstract map specification |
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
| `MmapBlockStorage.tla` | `src/persistent_artrie_core/{disk_manager,block_storage}.rs` and `tests/persistent_artrie_storage_correspondence.rs` |
| `LockFreeARTrieLinearizability.tla` | `src/persistent_artrie/{lockfree_cas.rs,nodes/atomic_ptr.rs}` and `tests/persistent_artrie_loom_correspondence.rs` |
| `ByzantineStorage.tla` | authenticated WAL/storage recovery filtering in `src/persistent_artrie_core/recovery.rs` |
| `HotStuffConsensus.tla` | proof/model-only Byzantine quorum safety boundary |
| `NodeTypes.v` | `src/persistent_artrie/nodes/*.rs` |
| `Bucket.v` | `src/persistent_artrie/bucket.rs` |
| `HotStuff.v` / `HotStuffSafety.v` | proof/model-only replicated log safety boundary |
| `ReplicatedMapSpec.v` | command-log replay model exercised by trace correspondence tests |
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
- proof-carrying trace replay, including corrupt-certificate rejection;
- unsafe-boundary regressions for swizzled-pointer transitions, atomic node
  pointer CAS ownership, optimistic-cell writer serialization, torn-WAL
  header/payload reopen, and persistent dictionary law traces.
- storage-boundary regressions for mmap block allocation uniqueness, sub-block
  bounds rejection, sync/reopen header checksum refresh, and raw-pointer
  bounds rejection, plus a Miri-friendly swizzled disk-pointer raw roundtrip
  and optional io_uring range rejection when that backend is enabled.
- bounded loom schedule checks for byte lock-free root CAS publication,
  duplicate insert linearization, insert/contains visibility, merge snapshot
  behavior, and child-pointer Arc handoff.

See [UNSAFE_BOUNDARY.md](UNSAFE_BOUNDARY.md) for the scoped Rust unsafe
boundary ledger and the remaining proof obligations.

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

- Exact byte layouts beyond the WAL header/record and bucket-page
  correspondence tests
- CRC32 internals beyond fail-closed corruption checks
- SIMD implementation details
- Memory allocation/deallocation
- File I/O buffering
- Thread scheduling details
- Byzantine networking/liveness, malicious CPU execution, and compromised
  cryptography; current Byzantine proofs cover storage filtering and bounded
  quorum/log safety
- Certified Rust/LLVM binary generation; current proofs certify the reference
  interface and proof-carrying replay checker, not compiled Rust artifacts
- Full Rust memory-safety proof for every unsafe site; byte lock-free
  publication, representative ARTrie pointer/concurrency, and mmap storage
  checks are executable, but io_uring internals and whole-crate unsafe
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
   regressions are in [UNSAFE_BOUNDARY.md](UNSAFE_BOUNDARY.md). Mmap storage
   allocation/remap and byte lock-free publication now have bounded models and
   executable correspondence tests. Next, extend schedule exploration to char
   and vocabulary lock-free paths, add more Miri-compatible raw-pointer
   targets beyond `SwizzledPtr`, and write whole-crate safety contracts for
   io_uring and unsafe `Send`/`Sync`.

## References

- [TLA+ Specification Language](https://lamport.azurewebsites.net/tla/tla.html)
- [The Rocq Prover](https://rocq-prover.org/)
- [Adaptive Radix Trees](https://db.in.tum.de/~leis/papers/ART.pdf)
- [ARIES Recovery Algorithm](https://cs.stanford.edu/people/chr101/cs345/aries.pdf)

## License

MIT License - Copyright (c) 2026 F1r3fly.io
