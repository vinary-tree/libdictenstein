# Bit-parallel child scan (and where SIMD actually lives)

**Navigation**: [← Bloom filters](03-bloom-filters.md) · [Volatile automata](README.md)

Finding a node's child on a given unit is the innermost operation of every trie walk, so how a node
represents and scans its child set matters. This document covers the **bit-parallel** child scan the
PathMap backend uses (`ByteMask`), contrasts it with the **SIMD** node scan of the adaptive radix
tree, and states plainly where each appears in libdictenstein — because "SIMD child scan" is easy to
over-claim. Notation follows [`docs/notation.md`](../../notation.md).

## The problem

For a byte alphabet a node can have up to 256 children. Three representations trade space for scan
cost:

- **Dense array** (256 slots): $`O(1)`$ lookup, but wastes space on sparse nodes.
- **Sorted list**: compact, but $`O(\log d)`$ per lookup for out-degree `d`, with branch
  mispredictions.
- **Bitmap of present children** (256 bits): compact *and* allows scanning only the children that
  exist, using word-parallel bit operations.

## Bit-parallel scanning with a 256-bit mask

libdictenstein's PathMap backend takes the bitmap route. A node's child set is a **`ByteMask`** — a
256-bit mask held as four `u64` words — where bit `b` is set iff byte `b` labels an existing child
(`TrieRefLike::child_mask`, [`src/pathmap/core.rs`](../../../src/pathmap/core.rs)). Two hardware
primitives make this fast, both operating on a whole 64-bit word at once:

- **`count_ones` (popcount)** — the number of children in a word (or the whole mask) in one
  instruction, so `child_count` is $`O(1)`$ per word.
- **`trailing_zeros` (tzcnt)** — the index of the next set bit, so iterating the *present* children
  is $`O(\text{popcount})`$, not $`O(256)`$.

Crucially, iteration **skips empty words**: a word that is all zero contributes no children and is
passed over in one comparison. So scanning a sparse node costs work proportional to the number of
children, and the four-word mask means a full 256-way node is four popcount/tzcnt steps rather than a
256-iteration loop. This is what lets PathMap descend in $`O(1)`$ from a focus with no root replay
and no lock — the property the [PathMap guide](../../algorithms/implementations/pathmap-dictionary.md)
relies on.

```text
mask = [ w0 | w1 | w2 | w3 ]      // four u64 words, 256 bits total
iterate children:
    for each word wi that is non-zero:      // skip all-zero words in one test
        while wi != 0:
            b   = base(i) + trailing_zeros(wi)   // next present child byte
            wi &= wi - 1                          // clear that lowest set bit
            yield b
```

This is **bit-level data parallelism** — one machine-word operation acts on 64 candidate bytes at
once — realized with ordinary scalar integer instructions (`popcnt`, `tzcnt`, `and`). It is *not*
SIMD in the vector-register sense, and the distinction matters for the next section.

## The other technique: SIMD node scan in the adaptive radix tree

The **adaptive radix tree** (ART; Leis et al. 2013,
[10.1109/ICDE.2013.6544812](https://doi.org/10.1109/ICDE.2013.6544812)) uses a genuinely different
trick for its medium-fanout node. A `Node16` stores up to 16 child *keys* in a 16-byte array; to find
a byte `c`, it broadcasts `c` across a vector register and compares all 16 keys **in one SIMD
instruction** (`_mm_cmpeq_epi8`), then extracts a match bitmask (`movemask`) and takes its
trailing-zero count as the slot:

```text
find_child_node16(keys[0..16], c):
    m   = _mm_cmpeq_epi8( splat(c), keys )   // 16 byte-equality tests, one instruction
    bit = movemask(m) & ((1 << n) - 1)       // 16-bit match mask, restricted to live keys
    return bit != 0 ? children[trailing_zeros(bit)] : none
```

This turns up to 16 scalar comparisons into a single vector compare — a real SIMD acceleration of the
child scan.

## Where each lives in libdictenstein (the honest accounting)

The two techniques live in different halves of the crate, and it is worth being precise because "SIMD
child scan" is often attributed to the whole library:

| Technique | Where | Kind |
|-----------|-------|------|
| 256-bit `ByteMask` word-skipping scan | `src/pathmap/` (volatile, feature `pathmap-backend`) | scalar bit-parallelism (`popcnt`/`tzcnt`) |
| `Node16` `_mm_cmpeq_epi8` SIMD scan | `src/persistent_artrie/nodes/node16.rs` (**persistent**) | true SIMD vector instruction |

**The volatile dictionary tree contains no SIMD at all.** A search of
`src/{double_array_trie,dynamic_dawg,suffix_automaton,scdawg,pathmap,bijective}` for `std::simd`,
`_mm_*`, or `core::arch` intrinsics returns nothing; the volatile DAWG/suffix/SCDAWG cores use the
adaptive linear-scan-then-binary-search crossover described in
[architecture §2](../../architecture/in-memory-dictionaries.md#2-monomorphized-cores), and PathMap
uses the bit-parallel mask above. The only `_mm_cmpeq_epi8` SIMD in the crate is the **persistent**
ART Node16, documented in the [ART theory](../disk-tries/03-adaptive-radix-tree.md). So: bit
*parallelism* is used in-memory (PathMap); vector *SIMD* is used only on disk-backed ART nodes.

## Complexity

| Scan | Cost | Notes |
|------|------|-------|
| `ByteMask` present-child iteration | $`O(\text{children})`$ | empty words skipped; $`O(1)`$ `count_ones` per word |
| ART `Node16` SIMD find | $`O(1)`$ (one vector compare) | up to 16 keys per instruction |
| Volatile DAWG/suffix node find | linear scan below 16 edges, binary search above | branch-predictable small nodes, logarithmic large ones |

## References

- Leis, V., Kemper, A., Neumann, T. (2013). *The Adaptive Radix Tree: ARTful Indexing for Main-Memory
  Databases.* IEEE ICDE. [10.1109/ICDE.2013.6544812](https://doi.org/10.1109/ICDE.2013.6544812)
- Knuth, D. E. (1997). *The Art of Computer Programming, Vol. 3: Sorting and Searching* (2nd ed.),
  §6.3 (digital searching) — background on trie node representations.
