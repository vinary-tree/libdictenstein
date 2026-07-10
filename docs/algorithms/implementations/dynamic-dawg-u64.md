# DynamicDawgU64 — the 64-bit-token sequence DAWG

**Navigation**: [← Implementation guides](README.md) | [Algorithms home](../README.md)

`DynamicDawgU64<V>` is the member of the [dynamic DAWG](dynamic-dawg.md) family whose edge-label
alphabet is the full 64-bit integer space rather than bytes or Unicode scalar values. It indexes
**sequences of `u64` tokens** — interned vocabulary IDs, hashes, packed records, or the bit patterns
of `f64` samples in a time series — and, like its byte and `char` siblings, answers membership in
$`O(\lvert q\rvert)`$ for a query sequence `q`, independent of how many sequences are stored.

It lives at [`src/dynamic_dawg/u64.rs`](../../../src/dynamic_dawg/u64.rs) and is re-exported as
`libdictenstein::dynamic_dawg::u64::DynamicDawgU64`.

Notation follows [`docs/notation.md`](../../notation.md).

---

## When to use it

- ✅ Keys are **already integers** — token IDs from a vocabulary, feature hashes, or fixed-width
  packed records — and you want prefix/sequence sharing without paying to re-encode them as bytes.
- ✅ **Time series**: a run of `f64` samples, indexed by their IEEE-754 bit patterns via
  `f64::to_bits`, so that exact-match and prefix queries over numeric sequences become trie walks.
- ✅ You need **runtime insertion** with lock-free readers (see [Concurrency](#concurrency)).
- ⚠️ Prefer [`DynamicDawg`](dynamic-dawg.md) (`u8`) or [`DynamicDawgChar`](dynamic-dawg-char.md)
  (`char`) when the keys are genuinely text — a byte or `char` DAWG is more compact for those.
- ⚠️ For a *durable* `u64` sequence index, use the persistent
  [`PersistentARTrieU64Compact`](../native-u64-and-cx.md) instead; `DynamicDawgU64` is volatile.

## Why a distinct type instead of `DynamicDawg<…>` over `u64`

The byte and `char` DAWGs share one arena-backed core (`DawgCore`, see [dynamic-dawg.md](dynamic-dawg.md#node-representation)).
`DynamicDawgU64` deliberately uses a **different representation** — a vector of reference-counted
nodes, each carrying its own atomically swappable edge list — because a `u64` alphabet makes two
assumptions of the shared core false:

- **No byte expansion.** A `char`/`u64` label is stored natively as one edge, not spread across up
  to four byte-edges, so per-transition work stays $`O(1)`$ amortized without a byte-decoding step.
- **Wait-free reads over a mutating graph.** Each node holds its edges behind an
  [`arc_swap::ArcSwap`](https://docs.rs/arc-swap), so a reader loads a stable edge list with no lock
  and no risk of tearing while a writer publishes a new one by compare-and-swap (*CAS*).

The trade-off is space: a per-node `Arc` plus an atomic edge-list pointer costs roughly
2–3$`\times`$ the bytes of the shared arena core. You buy wait-free reads and fine-grained writes
with that memory.

## Node representation

<img src="../../diagrams/dawg-u64-node.svg" alt="Structure of a DynamicDawgU64 node. The dictionary holds a vector of Arc-wrapped DawgNodeU64 values. Each node has three fields: an ArcSwap pointer to an EdgeList (a small-vector of (u64 label, Arc to child node) pairs, inline up to four then spilling to the heap), an ArcSwapOption pointer to the node's optional value V, and an atomic is-final flag. A writer publishes an edit by CAS-swapping the ArcSwap edge-list pointer to a freshly built list; concurrent readers keep observing the old list until they reload." width="560"/>

```rust
// src/dynamic_dawg/u64.rs (shape; field types verified against source)
pub(crate) struct DawgNodeU64<V: DictionaryValue> {
    edges: ArcSwap<EdgeList<V>>,       // atomically swappable child list
    value: ArcSwapOption<V>,           // optional associated value
    // … atomic is_final / bookkeeping …
}

type EdgeList<V> = SmallVec<[(u64, Arc<DawgNodeU64<V>>); 4]>;   // inline ≤ 4 edges, then heap
```

Child lookup within a node scans linearly while the edge count is below
`EDGE_LINEAR_SCAN_LIMIT` (16) and binary-searches the label-sorted list at or above it — the same
adaptive crossover the byte/`char` DAWGs use, which keeps small nodes branch-predictable and large
nodes logarithmic.

## Concurrency

`DynamicDawgU64<V>` is `Arc`-cloneable and shares its node vector across clones. Reads are
**wait-free**: `contains`, `contains_sequence`, and node `transition` load an `ArcSwap` snapshot and
walk it without taking a lock. Writes are **lock-free**: an insert builds the new edge list for each
touched node and publishes it with a CAS retry loop (`CasBackoff`), so a writer never blocks a
reader and readers never observe a partially linked node. This is the **per-node CAS** strategy
described in [the in-memory architecture doc](../../architecture/in-memory-dictionaries.md).

## Complexity

Let `q` be a query sequence and `n` the total number of `u64` tokens stored.

| Operation | Cost |
|-----------|------|
| `contains_sequence(q)` | $`O(\lvert q\rvert)`$ |
| `insert_sequence(q)` | $`O(\lvert q\rvert)`$ amortized |
| `update_or_insert_sequence(q, …)` | $`O(\lvert q\rvert)`$ amortized (may re-run the update closure on CAS conflict) |
| `compact()` | $`O(n)`$ |
| space | near-minimal after `compact()`; per-node `Arc` + atomic edge list, roughly 2–4$`\times`$ the shared-arena core between compactions |

## Trait support

`DynamicDawgU64<V>` implements [`Dictionary`](../README.md), [`MutableDictionary`](../README.md)
(`insert` / `remove` / `extend` over the string projection), and
[`CompactableDictionary`](../README.md) (`compact` / `minimize`). It deliberately does **not**
implement `MappedDictionary` or `MutableMappedDictionary`: values are attached with
`insert_sequence_with_value` / `insert_with_value` and read back through the type's
[`ValuedDictZipper`](../zippers.md), not through `get_value`. See the
[trait-support matrix](README.md#trait-support-matrix-in-memory-backends) for how this compares to
the rest of the family.

## Usage

### Sequences of tokens

```rust,ignore
use libdictenstein::dynamic_dawg::u64::DynamicDawgU64;

let dict = DynamicDawgU64::<i64>::new();
dict.insert_sequence(&[1, 2, 3]);
dict.insert_sequence_with_value(&[10, 20], 7);
assert!(dict.contains_sequence(&[10, 20]));

// Accumulate into the value already stored at a sequence (or seed it, then update):
dict.update_or_insert_sequence(&[10, 20], 0, |v| *v += 1);   // value at [10,20] is now 8
```

### Numeric time series via `f64::to_bits`

```rust,ignore
use libdictenstein::dynamic_dawg::u64::DynamicDawgU64;

// A window of samples is indexed by the bit patterns of its f64 values, so exact-match and
// prefix queries over the numeric series become trie walks.
let series: DynamicDawgU64 = DynamicDawgU64::new();
series.insert_f64(&[42.5, 43.0, 42.5]);
assert!(series.contains_f64(&[42.5, 43.0, 42.5]));
```

`insert_f64` / `contains_f64` are thin wrappers that map each `f64` through `f64::to_bits` and
delegate to the `u64`-sequence path, so `+0.0` and `-0.0` (distinct bit patterns) are distinct keys,
and `NaN` payloads are preserved bit-for-bit — the exact behavior you want for a lossless index and
the behavior you must account for if you expect IEEE-754 numeric equality.

## Related

- [dynamic-dawg.md](dynamic-dawg.md) — the byte DAWG and the shared minimization theory.
- [DAWG minimization theory](../../theory/volatile-automata/01-dawg-minimization.md) — why suffix
  sharing keeps the graph small.
- [native-u64-and-cx.md](../native-u64-and-cx.md) — the *durable* `u64` sequence index.
- [zippers.md](../zippers.md) — how to read values and enumerate sequences from this backend.
