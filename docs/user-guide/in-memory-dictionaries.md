# In-memory dictionaries — a guided tour

**Navigation**: [← User guide](README.md) | [Cookbook →](cookbook.md)

The volatile (RAM-resident, non-durable) backends all share the [trait API](../algorithms/README.md)
and all answer membership in $`O(\lvert q\rvert)`$, but they differ on three axes that decide which
one you want. This guide walks those axes; the [backend selector](backends.md) is the quick lookup,
and each backend's [implementation guide](../algorithms/implementations/README.md) is the deep dive.

For the shared design — the `CharUnit` alphabet seam, the monomorphized cores, and the two lock-free
concurrency strategies — see [the in-memory architecture](../architecture/in-memory-dictionaries.md).

---

## Axis 1 — set or map?

Every backend is generic over a value type `V` with default `()`:

- `DoubleArrayTrie` (i.e. `DoubleArrayTrie<()>`) is a **set**: it answers `contains`.
- `DoubleArrayTrie<u64>` is a **map**: it additionally answers `get_value`.

`()` is zero-sized, so the set form costs nothing extra. Any `V: Clone + Send + Sync + 'static`
works — integers, `String`, `Vec<T>`, `HashSet<T>`, `SmallVec` — via the `DictionaryValue` blanket
impls. Values are attached with `from_terms_with_values` at build time or `insert_with_value` at
runtime.

## Axis 2 — what shape of query?

| You need to find… | Backend family | Trait signal |
|-------------------|----------------|--------------|
| whole terms (exact / prefix) | double-array trie, dynamic DAWG, PathMap | plain `Dictionary` |
| a pattern **anywhere** inside indexed text (substring / infix) | suffix automaton, SCDAWG | `is_suffix_based()` / `SubstringDictionary` |
| a value's term (reverse lookup) | `BijectiveMap` | `BijectiveDictionary` |

**Prefix** queries (command completion) are supported by every trie-shaped backend through the
[zipper](../algorithms/zippers.md) layer, not a dedicated trait. **Substring** search is the
distinguishing capability of the suffix-automaton and SCDAWG families — they index *every* substring
of the text, so `contains_substring("quick")` matches inside `"the quick brown fox"`.

## Axis 3 — mutable at runtime, or build-once?

| Update mode | Backends | Notes |
|-------------|----------|-------|
| **insert + remove** | `DynamicDawg` / `…Char` / `…U64`, `PathMapDictionary` / `…Char` | lock-free readers throughout |
| **insert-only** (append) | `DoubleArrayTrie` / `…Char`, `Scdawg` / `…Char` | rebuild to delete |
| **build-once** | reading a `SuffixAutomaton` after construction | has an inherent `remove`, but it is an $`O(n)`$ rebuild |

If you delete terms at runtime, reach for a DAWG or PathMap. If the term set is fixed (a shipped
word list, a compiled lexicon), the double-array trie gives the cheapest, most cache-resident lookup.

## Axis 4 — which alphabet?

The edge-label unit is fixed per type via [`CharUnit`](../architecture/abstractions.md):

- **`u8`** (byte) — ASCII / Latin-1 / raw bytes; smallest and fastest.
- **`char`** (Unicode scalar value) — correct character-level semantics for arbitrary text; a query
  walks whole characters, not UTF-8 bytes.
- **`u64`** (token) — integer sequences: vocabulary IDs, hashes, or `f64` time-series samples via
  `f64::to_bits` ([`DynamicDawgU64`](../algorithms/implementations/dynamic-dawg-u64.md) only).

## The roster at a glance

| Backend | Alphabet | Query | Updates | One-line reason |
|---------|----------|-------|---------|-----------------|
| [`DoubleArrayTrie`](../algorithms/implementations/double-array-trie.md) / `…Char` | `u8` / `char` | exact / prefix | insert-only | fastest, most compact read-mostly lookup |
| [`DynamicDawg`](../algorithms/implementations/dynamic-dawg.md) / `…Char` | `u8` / `char` | exact / prefix | insert + remove | runtime mutation with suffix sharing |
| [`DynamicDawgU64`](../algorithms/implementations/dynamic-dawg-u64.md) | `u64` | exact / prefix | insert + remove | token / time-series sequences |
| [`SuffixAutomaton`](../algorithms/implementations/suffix-automaton.md) / `…Char` | `u8` / `char` | **substring** | online insert | match a pattern anywhere in the text |
| [`Scdawg`](../algorithms/implementations/scdawg.md) / `…Char` | `u8` / `char` | **substring**, bidirectional | build-once | compact two-way substring index |
| [`PathMapDictionary`](../algorithms/implementations/pathmap-dictionary.md) / `…Char` | `u8` / `char` | exact / prefix | insert + remove | structural-sharing snapshots *(feat. `pathmap-backend`)* |
| [`BijectiveMap`](../algorithms/implementations/bijective.md) | `char` | exact + reverse | insert | `term ↔ value` bijection |

## Worked example: build every backend, query it

Because they share the trait API, the same term list feeds any backend and the same `contains` call
queries it. Unicode across four backends (from the crate's own tests):

```rust
use libdictenstein::prelude::*;
use libdictenstein::double_array_trie::char::DoubleArrayTrieChar;
use libdictenstein::dynamic_dawg::char::DynamicDawgChar;
use libdictenstein::suffix_automaton::char::SuffixAutomatonChar;
use libdictenstein::scdawg::char::ScdawgChar;

let terms = vec!["café", "naïve", "日本語"];
let dat  = DoubleArrayTrieChar::from_terms(terms.clone());
let dawg = DynamicDawgChar::from_terms(terms.clone());
let sa   = SuffixAutomatonChar::from_texts(terms.clone());
let sc   = ScdawgChar::from_terms(&terms);

assert!(dat.contains("café"));
assert!(dawg.contains("日本語"));
assert!(sc.contains_substring("本"));               // substring: matches inside "日本語"
```

Note the one API difference the tour's axes predict: the suffix automaton is built from *texts* to
index (`from_texts`) rather than a term *set*, because it indexes every substring of each text.

## Trait asymmetries to keep in mind

A few backends do not implement the trait their capability suggests — surface, not internals. The
full list is in the [implementation-guide index](../algorithms/implementations/README.md#asymmetries-worth-knowing-all-verified-against-src);
the load-bearing ones:

- `SuffixAutomaton` is mutable and substring-capable but implements neither `MutableDictionary` nor
  `SubstringDictionary`; it signals substring support via `is_suffix_based() == true`.
- `Scdawg` implements `SubstringDictionary` but leaves `is_suffix_based()` at `false`.
- `DynamicDawgU64` stores values but implements neither `MappedDictionary` nor
  `MutableMappedDictionary`; read values through its zipper.
