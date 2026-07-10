# Cookbook

**Navigation**: [← User guide](README.md) | [In-memory tour](in-memory-dictionaries.md)

Copy-paste recipes for common tasks. Each names the backend it uses and links to the deeper
reference. Notation follows [`docs/notation.md`](../notation.md).

---

## Count occurrences (a term → count map)

Use a mapped, mutable DAWG and `update_or_insert` to seed-or-accumulate in one call:

```rust
use libdictenstein::prelude::*;
use libdictenstein::dynamic_dawg::DynamicDawg;

let counts: DynamicDawg<u64> = DynamicDawg::new();
for word in ["the", "cat", "sat", "on", "the", "mat", "the"] {
    counts.update_or_insert(word, 1, |n| *n += 1);
}
assert_eq!(counts.get_value("the"), Some(3));
assert_eq!(counts.get_value("cat"), Some(1));
```

`update_or_insert(term, default, f)` inserts `default` if the term is absent, otherwise applies `f`
to the stored value. On a lock-free CAS conflict it may re-run `f`, so keep `f` a pure function of
its argument.

## Enumerate every term (and value)

Every backend streams its contents. `.iter()` yields `(String, V)`; `.iter_terms()` yields terms:

```rust
use libdictenstein::double_array_trie::DoubleArrayTrie;

let dict = DoubleArrayTrie::from_terms_with_values(vec![("cat", 1u64), ("dog", 2), ("cats", 3)]);
let mut pairs: Vec<(String, u64)> = dict.iter().collect();
pairs.sort();
assert_eq!(pairs, vec![("cat".into(), 1), ("cats".into(), 3), ("dog".into(), 2)]);
```

Iteration is a depth-first walk with lazy path reconstruction (the path is materialized only at
final nodes), so it costs $`O(\text{total units})`$ overall.

## Substring (infix) search

Whole-term backends match at word boundaries; to match a pattern **anywhere**, index texts in a
[SCDAWG](../algorithms/implementations/scdawg.md) (or [suffix automaton](../algorithms/implementations/suffix-automaton.md)):

```rust
use libdictenstein::scdawg::Scdawg;
use libdictenstein::Dictionary;

let index: Scdawg = Scdawg::from_terms(&["cathedral", "category", "catering", "cat", "car"]);
assert!(index.contains_substring("cat"));        // inside every "cat…" term
assert!(index.contains_substring("eri"));         // inside "catering"
assert!(!index.contains_substring("xyz"));
```

For positions, `find_exact_substring(pattern)` returns a `Vec<SubstringMatch<_>>` carrying each
matching term, its start position, and length.

## Bidirectional term ↔ value

When you need reverse lookup (value → term) as well as forward, use a
[`BijectiveMap`](../algorithms/implementations/bijective.md):

```rust
use libdictenstein::bijective::BijectiveMap;
use libdictenstein::{Dictionary, MappedDictionary};

let bimap: BijectiveMap<String> = BijectiveMap::new();
bimap.insert("key1", "value1".to_string());
assert_eq!(bimap.get_value("key1"), Some("value1".to_string()));           // forward
assert_eq!(bimap.get_term(&"value1".to_string()).as_deref(), Some("key1")); // reverse
```

`insert` panics on a duplicate term *or* duplicate value (the bijection invariant); use
`try_insert` to get a `Result<(), InsertError>` instead of a panic.

## Numeric time-series index

Index runs of `f64` samples by their bit patterns with
[`DynamicDawgU64`](../algorithms/implementations/dynamic-dawg-u64.md):

```rust
use libdictenstein::dynamic_dawg::u64::DynamicDawgU64;

let series: DynamicDawgU64 = DynamicDawgU64::new();
series.insert_f64(&[42.5, 43.0, 42.5]);
assert!(series.contains_f64(&[42.5, 43.0, 42.5]));
```

## Prefix completion (autocomplete)

Navigate to a prefix, then stream every term beneath it, in $`O(k + m)`$ (prefix length `k`,
matches `m`) — far cheaper than filtering full iteration. This is the `PrefixZipper` combinator; the
[zippers guide](../algorithms/zippers.md) has the full, verified recipe.

## Set algebra over two dictionaries

Union, intersection, difference, and symmetric difference of any two backends compose **lazily** as
zippers — no intermediate dictionary is materialized. Values that collide are reconciled by a
pluggable merge strategy (last-wins, first-wins, or a lattice join/meet). See the
[zippers guide](../algorithms/zippers.md) for the combinators and their value-merge
semantics; the entry point is `UnionZipper::new(vec![z1, z2])` (and the `Difference` /
`Intersection` / `SymmetricDifference` siblings).

## Save and load

With the `serialization` feature, dictionaries round-trip through bincode, JSON, or (with
`protobuf`) Protobuf; `compression` adds a gzip wrapper. The wire format is always the *term list*
(and values, with the `*_with_values` variants), so a load rebuilds a valid dictionary regardless of
the on-disk graph. See the [serialization guide](../algorithms/serialization.md) for formats,
value-preservation, and version compatibility.

## Make it durable (survive a restart)

When the dictionary must outlive the process, use the persistent ARTrie family (feature
`persistent-artrie`): it is disk-backed, write-ahead-logged, and crash-recoverable.

```rust,no_run
use libdictenstein::persistent_artrie::char::PersistentARTrieChar;

let trie = PersistentARTrieChar::<u64>::create("words.artc")?;
trie.upsert("hello", 1)?;              // durably logged, then published
trie.checkpoint()?;                    // fold the overlay into a dense on-disk image

let reopened = PersistentARTrieChar::<u64>::open("words.artc")?;
assert_eq!(reopened.get("hello"), Some(1));
# Ok::<(), Box<dyn std::error::Error>>(())
```

The full durability model — the Order-A log-before-publish protocol, checkpointing, recovery, and
eviction — is documented under [`docs/persistence/`](../persistence/README.md).
