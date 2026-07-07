# Scdawg Implementation

**Navigation**: [← Dictionary Layer](../README.md) | [SuffixAutomaton](suffix-automaton.md) | [SCDAWG theory →](../../theory/scdawg/) | [Algorithms Home](../../README.md)

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

`Scdawg` (**SCDAWG** — *Symmetric Compact Directed Acyclic Word Graph*, also
called the **CDAWG** — *Compact DAWG*) is a substring-search data structure that
indexes **every substring** of a set of input strings in a minimal acyclic
graph. A **DAWG** (*Directed Acyclic Word Graph*) is the minimal acyclic
deterministic automaton recognizing a finite set of strings — it shares both
prefixes *and* suffixes. The *suffix* DAWG additionally recognizes every
substring (because every substring is a prefix of some suffix), and the
*compact* refinement contracts each non-branching chain of states into a single
edge labelled with the whole factor, so the graph stays linear in the input
size.

Unlike [`SuffixAutomaton`](suffix-automaton.md), which is constructed on-line
and supports per-character insertion, `Scdawg` is built batch-mode from a
complete set of input texts and is more memory-compact for static inputs.

Two variants are provided:

- [`Scdawg<V>`](../../../src/scdawg/ascii.rs) — byte-keyed (`u8` labels), suitable
  for ASCII or binary inputs.
- [`ScdawgChar<V>`](../../../src/scdawg/char.rs) — character-keyed (`char` /
  32-bit labels), Unicode-aware (each transition consumes one Rust `char`).

Both live under the shared core [`src/scdawg/core/`](../../../src/scdawg/core/)
(node + inner state machine), with the byte/char shells in `ascii.rs` / `char.rs`.

### Key Advantages

- 🔍 **Substring recognition**: any path from any state spells a substring of
  the indexed corpus, so `contains_substring(p)` answers in $O(\mid p\mid )$.
- 📦 **Compact**: asymptotically tighter than a generic suffix automaton because
  state merging and edge contraction are performed eagerly during batch
  construction ($\le n$ branching states for an input of total length `n`, versus
  the suffix automaton's $\le 2n−1$ states).
- ⚡ **IS-features**: the *index structure* operations of Blumer et al. (1987) —
  `freq` (occurrence count) and `locations` (every start position) — run in
  $O(\mid p\mid + k)$ for `k` occurrences.
- 🌐 **Unicode (char variant)**: correct multi-byte handling at the code-point
  level.

### When to Use

✅ **Use Scdawg when:**

- The full text corpus is known at construction time (no later inserts).
- Memory is constrained but substring lookups must remain fast.
- You need `find()` / `freq()` / `locations()` / `find_exact_substring()`
  operations that the basic `Dictionary` trait doesn't expose.

⚠️ **Consider alternatives when:**

- You need to add new texts at runtime → use
  [`SuffixAutomaton`](suffix-automaton.md), which supports on-line construction.
- You only need exact whole-word lookup (no substring search) → use
  [`DoubleArrayTrie`](double-array-trie.md) for read-mostly, or
  [`DynamicDawg`](dynamic-dawg.md) for dynamic.

## Theory: Compact Suffix DAWG

The DAWG was introduced by Blumer et al. (1985) — "The smallest automaton
recognizing the subwords of a text"
([10.1016/0304-3975(85)90157-4](https://doi.org/10.1016/0304-3975(85)90157-4))
— as the minimal automaton recognizing all factors (substrings) of a text.
Blumer et al. (1987), "Complete inverted files for efficient text retrieval and
analysis", refined it into the **compact** form (the CDAWG / SCDAWG) and defined
the *IS-features* (`freq` / `locations`) that `Scdawg` exposes. Inenaga et al.
(2005), "On-line construction of symmetric compact directed acyclic word graphs"
([10.1016/j.dam.2004.04.012](https://doi.org/10.1016/j.dam.2004.04.012)), gave
the symmetric on-line construction this implementation follows.

The structure has two defining properties:

1. **Right extensions are deterministic**: for each state `q` and each label
   `c`, there is at most one outgoing transition $q \to q'$ on `c` (as in any DFA).
2. **Non-branching chains are contracted**: a maximal run of states each with a
   single in-edge and single out-edge is collapsed into one edge carrying the
   whole factor. This is the "compact" refinement — it removes the redundancy a
   plain suffix automaton keeps for on-line constructibility.

The resulting graph has at most `n` branching states for an input of total
length `n`, a strict improvement over the suffix automaton's $\le 2n−1$ states.

### Endpos equivalence

A deeper treatment lives under [SCDAWG theory →](../../theory/scdawg/); the
essentials:

> **endpos** (*ending-position set*) of a substring `x` is the set of positions
> at which an occurrence of `x` ends in the indexed text.

Like the basic suffix automaton, the SCDAWG groups substrings by their `endpos`
sets: two substrings end at the same set of positions $\iff$ they share a state.
The compact refinement additionally contracts chains of states whose `endpos`
sets are identical except for the implied offset, eliminating states that would
otherwise be redundant after batch construction.

## Data Structure

`Scdawg<V>` wraps an internal `ScdawgInner<V>` (from
[`src/scdawg/core/inner.rs`](../../../src/scdawg/core/inner.rs)) behind a
lock-free atomic snapshot ([`src/scdawg/lockfree.rs`](../../../src/scdawg/lockfree.rs))
for thread-safe shared access:

```rust,ignore
pub struct Scdawg<V: DictionaryValue = ()> {
    inner: LockFreeScdawg<u8, V>,
}

// The snapshot cell: the whole inner graph is published as one immutable Arc.
pub(crate) struct LockFreeScdawg<U: CharUnit, V: DictionaryValue = ()> {
    inner: Arc<ArcSwap<ScdawgCoreInner<U, V>>>,
}
```

The `Arc<ArcSwap<…>>` publishes the entire `ScdawgInner` graph as an *immutable
snapshot*. A reader takes one `load_full()` snapshot — a single atomic load plus
an `Arc` clone — so reads are **wait-free** and never observe a torn graph. A
writer clones the current snapshot, applies its mutation to that private copy,
and installs the new graph with a single `compare_and_swap`; on a losing race it
retries under a bounded [`CasBackoff`](../../../src/nonblocking.rs), so writes
are **lock-free** whole-graph copy-on-write. No blocking lock is involved;
readers and writers never block one another.

`ScdawgInner<V>` holds the state array plus the metadata the IS-features need.
Each node (`ScdawgNode<V>`) carries:

- `forward_edges` — standard CDAWG edges that append characters.
- `suffix_link` — the longest proper suffix in a *different* `endpos` class.
- `left_edges` — left-extension edges (prepending characters), derived from the
  suffix links by `compute_left_edges()`; these make the graph *symmetric* and
  power `locations()`.
- `length` — the maximum length of strings in this equivalence class.
- `is_final` flag and optional value `V`.

The char variant `ScdawgChar<V>` has the same shape with `char`-keyed edges.

## Construction

### Batch (recommended)

```rust,no_run
use libdictenstein::prelude::*;          // brings Scdawg, Dictionary, …
use libdictenstein::SubstringDictionary; // not in the prelude

let dict: Scdawg<()> = Scdawg::from_terms(["apple", "apply", "application"]);
assert!(dict.contains("apple"));
assert!(dict.contains_substring("appli"));   // substring of "application"
```

`from_terms` collects all terms first (so the inner allocator can size the node
array via `with_capacity`), inserts each, then runs `compute_left_edges()` once
to finalize the left-edge metadata used by `find()` / `locations()`.

### Value-bearing

```rust,no_run
use libdictenstein::prelude::*;

let dict: Scdawg<u32> =
    Scdawg::from_terms_with_values([("alpha", 1u32), ("beta", 2)]);
assert_eq!(dict.get_value("alpha"), Some(1));
assert_eq!(dict.get_value("beta"), Some(2));
```

Value preservation through serialization round-trips works via
`BincodeSerializer::serialize_with_values` (A3 plumbing).

### Incremental (NOT recommended)

`Scdawg::insert(&self, term)` exists for protocol completeness but re-runs
`compute_left_edges()` on every call, making batch insertion via `from_terms`
strictly faster. The char variant has the same characteristic.

## Substring Operations

The IS-features of Blumer et al. (1987) are exposed via inherent methods and the
[`SubstringDictionary`](../../../src/substring.rs) trait. Let `p` be the query
pattern and `k` the number of occurrences:

| Method | Returns | Semantics |
|---|---|---|
| `contains_substring(p)` | `bool` | is `p` a substring of any indexed term? |
| `find(p)` | `Option<ScdawgNodeHandle<V>>` | the state representing `p`, or `None` |
| `freq(p)` | `usize` | total occurrence count across the corpus |
| `freq_at(handle)` | `usize` | occurrence count for a state already located via `find` |
| `locations(p)` | `Vec<(String, usize)>` | `(term, start-position)` for every occurrence |
| `find_exact_substring(p)` | `Vec<SubstringMatch<Node>>` | rich matches (term, position, length, end-node) |

`contains_substring`, `find`, and `freq` run in $O(\mid p\mid )$; `locations` /
`find_exact_substring` run in $O(\mid p\mid + k)$ because they additionally enumerate
the `k` hits. Use `find` once and then `freq_at` / `locations_at` to amortize the
$O(\mid p\mid )$ descent across repeated queries against the same state.

```rust,no_run
use libdictenstein::scdawg::Scdawg;
use libdictenstein::SubstringDictionary;

let dict: Scdawg<()> = Scdawg::from_terms(["abab", "bab"]);

assert!(dict.contains_substring("ab"));
assert_eq!(dict.freq("ab"), 3);              // 2 in "abab" + 1 in "bab"
let locs = dict.locations("ab");             // (term, start) per occurrence
assert_eq!(locs.len(), 3);

// find_exact_substring returns the matched term + position + length:
let matches = dict.find_exact_substring("ab");
assert_eq!(matches.len(), 3);
```

## Byte vs. Char Variants

| Property | `Scdawg<V>` | `ScdawgChar<V>` |
|---|---|---|
| Edge label type | `u8` | `char` (32-bit) |
| Edge count per state | up to 256 | unbounded (Unicode) |
| Memory per state | smaller | larger (per-edge tuple is wider) |
| Unicode correctness | per-byte only | per-code-point |
| Position units in `locations` | byte offsets | character offsets |
| Best for | ASCII text, binary keys | multilingual text |

Both variants implement the same trait surface (`Dictionary`,
`MappedDictionary`, `SubstringDictionary`). Test parity is maintained via the
value-roundtrip integration tests.

## Usage Examples

### Building from documents

```rust,no_run
use libdictenstein::prelude::*;
use libdictenstein::scdawg::Scdawg;
use libdictenstein::SubstringDictionary;

let docs = [
    "Levenshtein automata for approximate matching",
    "Suffix trees and suffix arrays for pattern search",
];
let dict: Scdawg<()> = Scdawg::from_terms(docs);

assert!(dict.contains_substring("approximate"));
assert!(dict.contains_substring("pattern search"));   // spans a word boundary
```

### Char variant with Unicode

```rust,no_run
use libdictenstein::prelude::*;
use libdictenstein::scdawg::ScdawgChar;
use libdictenstein::SubstringDictionary;

let dict: ScdawgChar<()> = ScdawgChar::from_terms(["café", "naïve", "日本語"]);
assert!(dict.contains_substring("café"));
assert!(dict.contains_substring("ï"));   // single code-point substring
```

### With a Levenshtein automaton

`Scdawg` implements `Dictionary`, so wrap it in
[liblevenshtein](https://github.com/universal-automata/liblevenshtein-rust)'s
`LevenshteinAutomaton` for fuzzy substring search — the automaton walks the
SCDAWG via `DictionaryNode::transition`, exactly as it would any other backend.

## Performance Analysis

For an input corpus of total length `n` and a query pattern of length $\mid p\mid$
with `k` occurrences:

| Operation | Time | Space |
|---|---|---|
| `from_terms` (batch build) | `O(n)` amortized | `O(n)` states |
| `contains_substring(p)` / `find(p)` | $O(\mid p\mid )$ | `O(1)` extra |
| `freq(p)` | $O(\mid p\mid + k)$ | `O(1)` |
| `locations(p)` / `find_exact_substring(p)` | $O(\mid p\mid + k)$ | `O(k)` returned |

Memory is smaller than `SuffixAutomaton` for the same corpus, since the compact
form contracts the non-branching chains the constructible-online suffix
automaton keeps separate. Treat the exact ratio as workload-dependent; the
benchmarking ledgers under [`../../benchmarks/`](../../benchmarks/) carry
reproducible numbers.

## When to Use

✅ Static substring search over a known corpus.
✅ Code search, literature search, log search with a precomputed index.
✅ Memory-constrained environments needing substring matching.

❌ Live-updating dictionaries → `SuffixAutomaton`.
❌ Pure prefix dictionaries → `DoubleArrayTrie`.

## References

- **Blumer, A., Blumer, J., Ehrenfeucht, A., Haussler, D., & McConnell, R. M.
  (1985)**. "The smallest automaton recognizing the subwords of a text".
  *Theoretical Computer Science*, 40, 31–55.
  DOI: [10.1016/0304-3975(85)90157-4](https://doi.org/10.1016/0304-3975(85)90157-4).
  — defines the DAWG.
- **Blumer, A., Blumer, J., Haussler, D., McConnell, R., & Ehrenfeucht, A.
  (1987)**. "Complete inverted files for efficient text retrieval and analysis".
  *Journal of the ACM*, 34(3), 578–595.
  DOI: [10.1145/28869.28873](https://doi.org/10.1145/28869.28873).
  — defines the compact suffix DAWG and the IS-features
  (`find` / `freq` / `locations`).
- **Inenaga, S., Hoshino, H., Shinohara, A., Takeda, M., & Arikawa, S. (2005)**.
  "On-line construction of symmetric compact directed acyclic word graphs".
  *Discrete Applied Mathematics*, 146(2), 156–179.
  DOI: [10.1016/j.dam.2004.04.012](https://doi.org/10.1016/j.dam.2004.04.012).
  — the symmetric on-line construction this backend follows.
- **Crochemore, M. (1986)**. "Transducers and repetitions".
  *Theoretical Computer Science*, 45(1), 63–86.
  DOI: [10.1016/0304-3975(86)90041-1](https://doi.org/10.1016/0304-3975(86)90041-1).
  — the online suffix-automaton construction the SCDAWG specializes.
