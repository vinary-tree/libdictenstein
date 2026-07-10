# Theory: volatile automata and their optimizations

Paper-grounded theory for the **in-memory** dictionary structures that the
[`docs/theory/scdawg/`](../scdawg/) suffix-automaton series does not already cover: DAWG
minimization, the double-array trie, and the two optimizations the mutable backends lean on (Bloom
pre-filtering and bit-parallel child scanning). It is the theoretical companion to the
[implementation guides](../../algorithms/implementations/README.md) and the
[in-memory architecture](../../architecture/in-memory-dictionaries.md). Notation follows
[`docs/notation.md`](../../notation.md).

## Where each structure's theory lives

The theory corpus is split by structure. This cluster fills the gaps for the volatile family:

| Structure | Theory |
|-----------|--------|
| Suffix automaton / CDAWG / SCDAWG | [`theory/scdawg/`](../scdawg/) (its own 7-part series) |
| Adaptive Radix Tree (persistent) | [`theory/disk-tries/`](../disk-tries/) |
| **DAWG minimization** (Daciuk MADFA + signature hashing) | [01-dawg-minimization.md](01-dawg-minimization.md) |
| **Double-array trie** (Aoe / Yata BASE·CHECK) | [02-double-array-tries.md](02-double-array-tries.md) |
| **Bloom filters** (probabilistic membership) | [03-bloom-filters.md](03-bloom-filters.md) |
| **Bit-parallel child scan** (PathMap's `ByteMask`) | [04-bit-parallel-child-scan.md](04-bit-parallel-child-scan.md) |

## Reading order

1. [01-dawg-minimization.md](01-dawg-minimization.md) — why a trie collapses into a DAWG and how the
   crate minimizes incrementally with a single `u64` signature per node.
2. [02-double-array-tries.md](02-double-array-tries.md) — how a trie becomes two integer arrays with
   arithmetic, pointer-free transitions.
3. [03-bloom-filters.md](03-bloom-filters.md) — the probabilistic membership filter, its false-positive
   arithmetic, and an honest account of its (limited) role on the current read path.
4. [04-bit-parallel-child-scan.md](04-bit-parallel-child-scan.md) — how child sets are scanned with
   word-parallel bit tricks, and where (and where **not**) SIMD appears in this crate.

## A note on honesty

Two of these docs correct a common over-claim. The Bloom filter and any "SIMD child scan" are often
described as if they were on every hot path; in libdictenstein they are **not**. The DAWG read path
is an exact wait-free traversal that consults no Bloom filter (the `with_config` capacity argument is
vestigial), and the **volatile** tree contains **no SIMD** at all — the only SIMD in the crate is the
persistent ART Node16 byte scan. These docs state the theory *and* where the code actually uses it.
