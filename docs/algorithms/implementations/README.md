# Per-backend implementation guides

This directory holds one deep-dive per dictionary backend: its data structure, construction and
query algorithms, complexity, memory layout, and worked usage. Start at the
[algorithms overview](../README.md) for the trait layer, or the root
[backend selector](../../../README.md#backend-selector) to choose a backend; then read the matching
guide below.

Notation and terminology follow [`docs/notation.md`](../../notation.md).

---

## In-memory (volatile) backends

RAM-resident, non-durable. Every one is generic over a value type `V` (default `()`, which makes a
*set*); the edge-label alphabet is fixed per type via the [`CharUnit`](../../architecture/abstractions.md)
abstraction (`u8` bytes, `char` Unicode scalar values, or `u64` tokens). Lookup is
$`O(\lvert q\rvert)`$ for a query `q`, independent of the number of stored terms.

| Guide | Types | Alphabet | Best for |
|-------|-------|----------|----------|
| [double-array-trie.md](double-array-trie.md) | `DoubleArrayTrie<V>` | `u8` | fastest, most compact read-mostly lookup |
| [double-array-trie-char.md](double-array-trie-char.md) | `DoubleArrayTrieChar<V>` | `char` | Unicode double-array trie |
| [dynamic-dawg.md](dynamic-dawg.md) | `DynamicDawg<V>` | `u8` | runtime insert **and** remove; suffix sharing |
| [dynamic-dawg-char.md](dynamic-dawg-char.md) | `DynamicDawgChar<V>` | `char` | Unicode dynamic DAWG |
| [dynamic-dawg-u64.md](dynamic-dawg-u64.md) | `DynamicDawgU64<V>` | `u64` | token / time-series sequence DAWG |
| [suffix-automaton.md](suffix-automaton.md) | `SuffixAutomaton<V>`, `SuffixAutomatonChar<V>` | `u8` / `char` | **substring** (infix) search |
| [scdawg.md](scdawg.md) | `Scdawg<V>`, `ScdawgChar<V>` | `u8` / `char` | static, compact **bidirectional** substring index |
| [pathmap-dictionary.md](pathmap-dictionary.md) | `PathMapDictionary<V>`, `…Char`, snapshot/ref variants | `u8` / `char` | structural-sharing mutable trie *(feature `pathmap-backend`)* |
| [bijective.md](bijective.md) | `BijectiveMap<V>` | `char` | bidirectional `term ↔ value` map |

The [`DictionaryFactory`](../../../README.md#core-traits) enum-dispatches all of these from a single
call; value-bearing dictionaries are constructed directly with `from_terms_with_values` /
`insert_with_value`.

## Persistent (durable) backends

Disk-backed, crash-durable, feature-gated behind `persistent-artrie`. Documented under
[`docs/persistence/`](../../persistence/README.md) and [`docs/algorithms/`](../README.md)
(`native-u64-and-cx.md`, `persistent-suffix-graphs.md`, `vocab-trie.md`) rather than here, because
their creation, recovery, and checkpoint lifecycle are part of the API surface.

---

## Trait-support matrix (in-memory backends)

Which traits ([`docs/algorithms/README.md`](../README.md) defines them) each backend honors. A blank
cell means the trait is *not* implemented for that type — see the asymmetry notes below, because
several are load-bearing.

| Backend | `Dictionary` | `MappedDictionary` | `MutableDictionary` | `CompactableDictionary` | `MutableMappedDictionary` | `SubstringDictionary` | `BijectiveDictionary` |
|---------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| `DoubleArrayTrie` / `…Char` | ✓ | ✓ | | | | | |
| `DynamicDawg` / `…Char` | ✓ | ✓ | ✓ | ✓ | ✓ | | |
| `DynamicDawgU64` | ✓ | | ✓ | ✓ | | | |
| `SuffixAutomaton` / `…Char` | ✓ | ✓ | | | ✓ | | |
| `Scdawg` / `…Char` | ✓ | ✓ | | | | ✓ | |
| `PathMapDictionary` / `…Char` | ✓ | ✓ | ✓ | | ✓ | | |
| `PathMapSnapshot` / `PathMapRef` / `…Char` | ✓ | ✓ | | | | | |
| `BijectiveMap` | ✓ | ✓ | | | | | ✓ |

### Asymmetries worth knowing (all verified against `src/`)

These surprise trait-driven callers, so they are stated explicitly:

- **`SuffixAutomaton` / `…Char` do *not* implement `MutableDictionary`.** They are mutable — an
  inherent `remove(&self, text)` exists and they implement `MutableMappedDictionary` — but not
  through the `MutableDictionary` trait. They advertise their substring nature by returning `true`
  from `Dictionary::is_suffix_based()`, and they do *not* implement `SubstringDictionary`; substring
  queries go through node/zipper traversal (this is what the companion transducer consumes).
- **`Scdawg` / `…Char` implement `SubstringDictionary`** (the trait with `find_exact_substring`) but
  leave `is_suffix_based()` at its default `false`. So the two substring families answer "am I a
  substring index?" through *different* signals — by design.
- **`DynamicDawgU64` implements neither `MappedDictionary` nor `MutableMappedDictionary`**, even
  though it stores values. Its primary surface is sequence-based (`insert_sequence`,
  `insert_sequence_with_value`, `contains_sequence`, `update_or_insert_sequence`); values are read
  back through its `ValuedDictZipper`, not the mapped-dictionary traits.
- **`DoubleArrayTrie` / `…Char` are insert-only.** Construction is from a sorted term list; there is
  no `remove`, so they do not implement `MutableDictionary`. Use a DAWG or PathMap for runtime
  deletion.

---

## Two concurrency strategies

Every mutable in-memory backend is lock-free for readers, but they reach it two different ways
(see [`docs/architecture/in-memory-dictionaries.md`](../../architecture/in-memory-dictionaries.md)
for the full treatment):

1. **Path-copy plus root CAS** — the DAWG family (`DynamicDawg`, `…Char`, `…U64`) retains immutable
   roots, copies only the inserted/removed path, and publishes one replacement `GraphVersion`.
2. **Whole-graph snapshot (copy-on-write)** — the suffix automaton, SCDAWG, and PathMap families
   publish a freshly built revision of the entire structure through one `ArcSwap`, so readers always
   observe an internally consistent snapshot.

`DoubleArrayTrie` is immutable after construction (`Arc<Vec<…>>` arrays, no writer path).
