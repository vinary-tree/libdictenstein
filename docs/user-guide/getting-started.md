# Getting started

**Navigation**: [← User guide](README.md) | [Choosing a backend →](backends.md)

## Install

```toml
[dependencies]
libdictenstein = "0.2"
```

The default build pulls in the in-memory backends. Optional families are behind feature flags —
`persistent-artrie` for the durable disk-backed ARTrie, `pathmap-backend` for the structural-sharing
trie, `serialization` for save/load. See [Feature flags](../engineering/feature-flags.md).

## Build, query, traverse

Any in-memory backend answers the same three questions. Here is a read-mostly
[double-array trie](../algorithms/implementations/double-array-trie.md):

```rust
use libdictenstein::prelude::*;                       // Dictionary, DictionaryNode, Mutable*, …
use libdictenstein::double_array_trie::DoubleArrayTrie;

let dict = DoubleArrayTrie::from_terms(vec!["hello", "help", "world"]);

assert!(dict.contains("hello"));
assert!(!dict.contains("hel"));                       // "hel" is a prefix, not a term

// Walk the automaton edge by edge — this is exactly what a fuzzy transducer does:
let root = dict.root();
if let Some(next) = root.transition(b'h') {
    assert!(next.transition(b'e').is_some());
}
```

Three things to notice, because they hold for *every* backend:

- **Membership is not prefix-hood.** `contains("hel")` is `false` even though `"hel"` is a prefix of
  `"hello"` — a term must reach a *final* node. Prefix queries are a separate operation (see the
  [cookbook](cookbook.md#prefix-completion-command-completion)).
- **`root()` + `transition()` is the universal traversal API.** You never need to know which backend
  you hold to walk it; that uniformity is the whole point of the trait layer.
- **The unit type follows the backend.** The byte backends transition on `u8` (`b'h'`); the `char`
  backends transition on `char`; the `u64` backend on `u64`. This is the
  [`CharUnit`](../architecture/abstractions.md) abstraction.

## Associate values with terms (a *map*)

Give the dictionary a value type other than `()` and it becomes a map. A byte-level DAWG counting
occurrences:

```rust
use libdictenstein::prelude::*;
use libdictenstein::dynamic_dawg::DynamicDawg;

let counts: DynamicDawg<u64> = DynamicDawg::new();
counts.insert_with_value("apple", 3);
counts.insert_with_value("apricot", 1);
assert_eq!(counts.get_value("apple"), Some(3));
```

## Don't know which backend? Use the factory

If you want to choose a backend by name (e.g. from config) and get a uniform handle, use the
[`DictionaryFactory`](../algorithms/README.md):

```rust
use libdictenstein::factory::{DictionaryFactory, DictionaryBackend};

let dict = DictionaryFactory::create(
    DictionaryBackend::DynamicDawg,
    vec!["hello", "world"],
);
assert!(dict.contains("hello"));
```

The factory constructs any of the in-memory backends from one call. Value-bearing dictionaries are
built directly (as in the counting example above), because the factory's unified container is
set-like.

## Unicode

Every byte backend has a `char` sibling that transitions on Unicode scalar values, so a query walks
whole characters rather than UTF-8 bytes:

```rust
use libdictenstein::prelude::*;
use libdictenstein::double_array_trie::char::DoubleArrayTrieChar;

let dict = DoubleArrayTrieChar::from_terms(vec!["café", "naïve", "日本語"]);
assert!(dict.contains("日本語"));
```

## Next

- **[Choosing a backend](backends.md)** — match a backend to your workload.
- **[In-memory dictionaries](in-memory-dictionaries.md)** — the full tour of volatile backends.
- **[Cookbook](cookbook.md)** — recipes for counting, completion, substring search, and more.
- **[Persistence](../persistence/README.md)** — when you need the dictionary to survive a restart.
