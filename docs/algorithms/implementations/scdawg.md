# Scdawg Implementation

**Navigation**: [← Dictionary Layer](../README.md) | [SuffixAutomaton](suffix-automaton.md) | [Algorithms Home](../../README.md)

## Table of Contents

1. [Overview](#overview)
2. [Theory: Compact Suffix DAWG](#theory-compact-suffix-dawg)
3. [Data Structure](#data-structure)
4. [Construction](#construction)
5. [Substring Operations](#substring-operations)
6. [Byte vs. Char Variants](#byte-vs-char-variants)
7. [Usage Examples](#usage-examples)
8. [Performance Analysis](#performance-analysis)
9. [When to Use](#when-to-use)
10. [References](#references)

## Overview

`Scdawg` (Symmetric Compact DAWG / Compact Suffix DAWG) is a substring-search
data structure that represents **all suffixes** of a set of input strings in
a minimal acyclic graph. Unlike `SuffixAutomaton`, which is constructed
on-line and supports per-character insertion, `Scdawg` is built batch-mode
from a complete set of input texts and is more memory-compact for static
inputs.

Two variants are provided:

- [`Scdawg<V>`](../../../src/scdawg.rs) — byte-keyed, suitable for ASCII or
  binary inputs.
- [`ScdawgChar<V>`](../../../src/scdawg_char.rs) — character-keyed,
  Unicode-aware (each transition consumes one Rust `char`).

### Key Advantages

- 🔍 **Substring recognition**: Any path from any state represents a
  substring of the indexed corpus.
- 📦 **Compact**: Asymptotically tighter than a generic suffix automaton
  because state merging is performed eagerly during batch construction.
- ⚡ **Find operations**: O(|pattern|) lookup time.
- 🌐 **Unicode (char variant)**: Correct multi-byte handling.

### When to Use

✅ **Use Scdawg when:**
- The full text corpus is known at construction time (no later inserts).
- Memory is constrained but substring lookups must remain fast.
- You need `find()` / `match_positions()` / `count_substring()` operations
  that the basic `Dictionary` trait doesn't expose.

⚠️ **Consider alternatives when:**
- You need to add new texts at runtime → use
  [`SuffixAutomaton`](suffix-automaton.md), which supports on-line
  construction.
- You only need exact whole-word lookup (no substring search) → use
  [`DoubleArrayTrie`](double-array-trie.md) for read-mostly or
  [`DynamicDawg`](dynamic-dawg.md) for dynamic.

## Theory: Compact Suffix DAWG

Blumer et al. (1987) introduced the compact suffix DAWG ("Symmetric Compact
DAWG", or simply SCDAWG) as a refinement of the suffix automaton with two
properties:

1. **Right extensions are unique**: For each state q and each character c,
   there is at most one outgoing transition q → q' on c. (Same as a normal
   DFA.)
2. **Left extensions are factored**: Every set of states sharing the same
   set of right-context characters is merged into a single state. This is
   the "compact" refinement — it eliminates redundancy that the basic
   suffix automaton retains for on-line constructibility.

The resulting graph has at most n states for an input of length n (Blumer
et al. 1987, Theorem 4.1), giving a strict memory bound smaller than the
suffix automaton's 2n-1.

### Endpos equivalence

Like the basic suffix automaton, SCDAWG states group substrings by ending
positions ("endpos"). Two substrings end at the same set of positions in
the original text ⇔ they belong to the same state. SCDAWG additionally
merges states whose endpos sets satisfy a Blumer–Blumer "compactness"
relation, eliminating states that would otherwise be redundant after batch
construction.

## Data Structure

`Scdawg<V>` wraps an internal `ScdawgInner` inside an `Arc<RwLock<…>>` for
thread-safe access:

```rust,ignore
pub struct Scdawg<V: DictionaryValue = ()> {
    inner: Arc<RwLock<ScdawgInner<V>>>,
}
```

`ScdawgInner` holds:

- `nodes: Vec<ScdawgNode<V>>` — the state array (each state has its edges,
  is_final flag, optional value, and left-edge metadata).
- `term_count: usize` — number of distinct terms indexed.
- `string_count: usize` — number of distinct source texts (substring
  matching is offered against this aggregate).

The char variant `ScdawgChar<V>` has the same shape with `char`-keyed
edges (`ScdawgCharNode<V>` storing `Vec<(char, usize)>` edge tuples).

## Construction

### Batch (recommended)

```rust,no_run
use libdictenstein::prelude::*;
use libdictenstein::scdawg::Scdawg;

let dict: Scdawg<()> = Scdawg::from_terms(vec!["apple", "apply", "application"]);
assert!(dict.contains("apple"));
assert!(dict.contains("appli"));   // substring of "application"
```

`from_terms` collects all terms first (so the inner allocator can size the
node array), inserts each, then runs `compute_left_edges()` to finalize the
left-edge metadata used by `find()`.

### Value-bearing

```rust,no_run
use libdictenstein::prelude::*;
use libdictenstein::scdawg::Scdawg;

let dict: Scdawg<u32> =
    Scdawg::from_terms_with_values(vec![("alpha", 1), ("beta", 2)]);
assert_eq!(dict.get_value("alpha"), Some(1));
assert_eq!(dict.get_value("beta"), Some(2));
```

Value preservation through serialization round-trips works via
`BincodeSerializer::serialize_with_values` (A3 plumbing).

### Incremental (NOT recommended)

`Scdawg::insert(&self, term)` exists for protocol completeness but rebuilds
the left-edge index every call, making batch insertion via `from_terms`
strictly faster. The char variant has the same characteristic.

## Substring Operations

The IS-features of Blumer et al. 1987 are exposed via inherent methods:

- `find(pattern) -> Option<NodeHandle>` — locate the state representing a
  substring; returns `None` if the substring isn't present.
- `count_substring(pattern) -> usize` — number of occurrences across the
  indexed corpus.
- `match_positions(pattern) -> Vec<(string_id, offset)>` — every
  start-position of `pattern` in the original texts.

These operations all run in `O(|pattern|)` time.

## Byte vs. Char Variants

| Property | `Scdawg<V>` | `ScdawgChar<V>` |
|---|---|---|
| Edge label type | `u8` | `char` (32-bit) |
| Edge count per state | up to 256 | unbounded (Unicode) |
| Memory per state | smaller | larger (per-edge tuple is `(char, usize)`) |
| Unicode correctness | per-byte only | per-codepoint |
| Best for | ASCII text, binary keys | Multilingual text |

Both variants share the same trait surface (`Dictionary`,
`MappedDictionary`, `SubstringDictionary`). Test parity is maintained via
the value-roundtrip integration tests.

## Usage Examples

### Building from documents

```rust,no_run
use libdictenstein::prelude::*;
use libdictenstein::scdawg::Scdawg;

let docs = vec![
    "Levenshtein automata for approximate matching",
    "Suffix trees and suffix arrays for pattern search",
];
let dict: Scdawg<()> = Scdawg::from_terms(docs);

assert!(dict.contains("approximate"));
assert!(dict.contains("pattern search"));   // substring spanning multiple words
```

### Char variant with Unicode

```rust,no_run
use libdictenstein::prelude::*;
use libdictenstein::scdawg_char::ScdawgChar;

let dict: ScdawgChar<()> = ScdawgChar::from_terms(vec!["café", "naïve", "日本語"]);
assert!(dict.contains("café"));
assert!(dict.contains("ï"));   // substring (single codepoint)
```

### With Levenshtein automaton

`Scdawg` implements `Dictionary` + `MappedDictionaryNode`, so wrap it in
[liblevenshtein](https://github.com/universal-automata/liblevenshtein-rust)'s
`Transducer` for fuzzy substring search.

## Performance Analysis

For an input corpus of total length n:

| Operation | Time | Space |
|---|---|---|
| `from_terms` (batch build) | O(n) amortized | O(n) states |
| `contains(s)` / `find(s)` | O(\|s\|) | O(1) extra |
| `match_positions(s)` | O(\|s\| + k) where k is hit count | O(k) returned |
| `count_substring(s)` | O(\|s\|) | O(1) |

Memory: typically 1.5-1.8× smaller than `SuffixAutomaton` for the same
input corpus, since SCDAWG merges left-redundant states that the
constructible-online suffix automaton keeps separate.

## When to Use

✅ Static substring search over a known corpus.
✅ Code search, literature search, log search with a precomputed index.
✅ Memory-constrained environments needing substring matching.

❌ Live-updating dictionaries → `SuffixAutomaton`.
❌ Pure prefix dictionaries → `DoubleArrayTrie`.

## References

- Blumer, A., Blumer, J., Haussler, D., McConnell, R., & Ehrenfeucht, A.
  (1987). *Complete inverted files for efficient text retrieval and
  analysis*. Journal of the ACM, 34(3), 578-595. — defines the SCDAWG and
  the IS-features (`find` / `match_positions` / `count_substring`).
- Crochemore, M., & Vérin, R. (1997). *Direct construction of compact
  directed acyclic word graphs*. Combinatorial Pattern Matching, 116-129.
  — algorithmic walk-through of batch construction.
