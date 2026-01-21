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
│   ├── WAL.tla                # Write-ahead log specification
│   ├── Concurrency.tla        # Optimistic lock coupling model
│   ├── CrashRecovery.tla      # ARIES-style recovery
│   ├── NodeTransitions.tla    # Node growth transitions
│   ├── EpochCheckpoint.tla    # Epoch lifecycle
│   ├── PART.tla               # Main composed specification
│   ├── PART.cfg               # TLC configuration (no crash)
│   └── PART_crash.cfg         # TLC configuration (with crash)
│
└── rocq/                      # Rocq/Coq proofs
    ├── Makefile               # Build system
    ├── Spec/                  # Specifications
    │   ├── MapSpec.v          # Abstract map specification
    │   └── ARTrieSpec.v       # ARTrie-specific specification
    ├── Model/                 # Data structure models
    │   ├── Key.v              # Key representation
    │   ├── NodeTypes.v        # Node4, Node16, Node48, Node256
    │   ├── Bucket.v           # B-trie bucket model
    │   └── PathCompression.v  # Prefix compression
    ├── Invariants/            # Invariant definitions and proofs
    │   ├── StructuralInvariants.v
    │   └── TransitionInvariants.v
    ├── Operations/            # Operation proofs (TODO)
    └── Proofs/                # Main theorem proofs (TODO)
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

### Running TLC Model Checker

```bash
# Basic safety checking (no crash)
tlc -workers 8 PART.tla -config PART.cfg

# With crash recovery verification
tlc -workers 8 PART.tla -config PART_crash.cfg
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
3. **Delete Correctness**: `interpret_trie (art_delete t k) = delete_map (interpret_trie t) k`
4. **Node Transition Correctness**: Transitions preserve all children
5. **Map Refinement**: ARTrie correctly implements Map ADT

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

| Module | Status | Description |
|--------|--------|-------------|
| Key.v | Complete | Key representation and operations |
| NodeTypes.v | Complete | Node type definitions |
| Bucket.v | Partial | Bucket operations (some admitted) |
| PathCompression.v | Partial | Prefix matching (some admitted) |
| MapSpec.v | Complete | Abstract map specification |
| ARTrieSpec.v | Partial | ARTrie specification (needs insert/delete) |
| StructuralInvariants.v | Partial | Structural invariants |
| TransitionInvariants.v | Complete | Node transition proofs |

## Relationship to Implementation

The formal specifications model the key components of the Rust implementation:

| Specification | Rust Source |
|---------------|-------------|
| `ARTrieTypes.tla` | `src/persistent_artrie/nodes/mod.rs` |
| `WAL.tla` | `src/persistent_artrie/wal.rs` |
| `Concurrency.tla` | `src/persistent_artrie/concurrency.rs` |
| `CrashRecovery.tla` | `src/persistent_artrie/recovery.rs` |
| `NodeTypes.v` | `src/persistent_artrie/nodes/*.rs` |
| `Bucket.v` | `src/persistent_artrie/bucket.rs` |

## Abstractions

The following details are abstracted in the specifications:

- Exact byte layouts and CRC32 checksums
- SIMD implementation details
- Memory allocation/deallocation
- File I/O buffering
- Thread scheduling details

## Future Work

1. **Operations Module**: Complete implementation of insert/delete in Rocq
2. **Map Refinement Proof**: Formal proof that ARTrie refines Map ADT
3. **Iris Integration**: Separation logic proofs for mutable state
4. **Liveness Proofs**: Complete TLA+ liveness verification
5. **Coverage**: Map TLA+ states to code coverage

## References

- [TLA+ Specification Language](https://lamport.azurewebsites.net/tla/tla.html)
- [The Rocq Prover](https://rocq-prover.org/)
- [Adaptive Radix Trees](https://db.in.tum.de/~leis/papers/ART.pdf)
- [ARIES Recovery Algorithm](https://cs.stanford.edu/people/chr101/cs345/aries.pdf)

## License

MIT License - Copyright (c) 2026 F1r3fly.io
