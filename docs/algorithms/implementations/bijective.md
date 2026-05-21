# BijectiveMap Implementation

**Navigation**: [← Dictionary Layer](../README.md) | [PersistentVocabARTrie](../../persistence/mmap-architecture.md) | [Algorithms Home](../../README.md)

## Table of Contents

1. [Overview](#overview)
2. [Bijection Invariant](#bijection-invariant)
3. [Data Structure](#data-structure)
4. [API](#api)
5. [Cow Return Type](#cow-return-type)
6. [Thread Safety](#thread-safety)
7. [Comparison with Vocab Tries](#comparison-with-vocab-tries)
8. [Usage Examples](#usage-examples)
9. [Performance Analysis](#performance-analysis)
10. [When to Use](#when-to-use)

## Overview

`BijectiveMap<V>` is a bidirectional map enforcing a 1:1 correspondence
between string terms and arbitrary hashable values. It supports both
forward lookup (`term → value`) and reverse lookup (`value → term`) in
amortized O(1) time.

### Key Properties

- 🔁 **Strict 1:1**: Inserting a duplicate term or value panics (by
  default) to preserve the invariant; `try_insert` returns
  `Result<(), InsertError>` for non-panicking callers.
- 🔒 **Thread-safe**: `RwLock`-based concurrency; multiple readers may
  proceed in parallel.
- 🧮 **Generic value type**: any `V: Eq + Hash + DictionaryValue` works.
- ⚙️ **Bijection trait**: implements
  [`BijectiveDictionary`](../../../src/bijective/mod.rs), shared with
  `PersistentVocabARTrie` and `SharedVocabARTrie`.

## Bijection Invariant

For every `(k, v)` pair in the map:

```text
get_value(k) == Some(v)  ⟺  get_term(&v) == Some(k)
```

Concretely:

- Every term maps to exactly one value.
- Every value maps to exactly one term.
- No two terms share the same value.
- No value exists without a corresponding term.

Insertion attempts that would violate this invariant either panic
(via `insert`) or return `Err(InsertError::DuplicateTerm | DuplicateValue)`
(via `try_insert`).

## Data Structure

```rust,ignore
pub struct BijectiveMap<V> {
    forward: BijectiveForward<V>,                // wraps an internal hash map: term → value
    reverse: RwLock<HashMap<V, String>>,         // value → term
}
```

The forward map is itself a thread-safe hash map (currently built on
parking_lot's `RwLock<HashMap>`). The reverse map mirrors the same data
keyed by value.

Both maps are append-only — the public API has no `remove` method. This
keeps the invariant trivially satisfiable and means lookups don't need to
worry about stale entries.

## API

Forward lookup: `MappedDictionary` trait via `get_value(&self, term: &str) -> Option<V>`.

Reverse lookup: `BijectiveDictionary` trait — see the next section for the
`Cow` return type discussion.

Mutation:

- `insert(&self, term: &str, value: V)` — panics on duplicate term or value.
- `try_insert(&self, term: &str, value: V) -> Result<(), InsertError>` —
  non-panicking variant.
- No `remove` — by design (preserves the bijection without invalidating
  iteration).

Inherent:

- `BijectiveMap::get_term(&self, value: &V) -> Option<String>` — returns
  a freshly-cloned `String` (no borrow concerns).
- `BijectiveMap::contains_term`, `BijectiveMap::contains_value`.

## Cow Return Type

The `BijectiveDictionary::get_term` trait method signature is

```rust,ignore
fn get_term(&self, value: &Self::Value) -> Option<std::borrow::Cow<'_, str>>;
```

This was changed from `Option<&str>` in plan item **A1**. The original
signature forced impls into one of:

- (`BijectiveMap`) Returning a raw pointer dereferenced inside `unsafe`,
  which is **unsound** under concurrent inserts that may rehash the
  `HashMap`.
- (`PersistentVocabARTrie` / `SharedVocabARTrie`) Returning `None`
  unconditionally because the term is reconstructed on-the-fly from
  parent pointers and has no stable in-memory storage to borrow from.

Switching to `Option<Cow<'_, str>>` lets each impl be honest:

- `BijectiveMap` clones the `String` from its reverse map into
  `Cow::Owned(String)`. The clone replaces the previous unsafe pointer
  dereference.
- `PersistentVocabARTrie` reconstructs the term via parent-pointer
  backtracking and wraps the result in `Cow::Owned(String)`.
- `SharedVocabARTrie` acquires the read guard, reconstructs the term,
  drops the guard, and wraps in `Cow::Owned`.

If you only need to compare against a string literal, prefer the
`Cow::as_deref()` shortcut:

```rust,no_run
use libdictenstein::bijective::{BijectiveDictionary, BijectiveMap};

let bimap = BijectiveMap::from_pairs([("alpha", 0u64), ("beta", 1)]);
let got = BijectiveDictionary::get_term(&bimap, &0u64);
assert_eq!(got.as_deref(), Some("alpha"));
```

## Thread Safety

`BijectiveMap` is thread-safe under the standard reader-writer contract:

- `get_value` / `get_term` / `contains_term` / `contains_value` /
  `bijection_len` acquire **read** guards. Multiple readers proceed in
  parallel.
- `insert` / `try_insert` acquire **write** guards on both the forward
  and reverse maps (in fixed order to avoid deadlock).

The reverse map's `Cow::Owned(String)` return type means the read guard
on `reverse` is dropped before the function returns — callers cannot
hold a reference into the map while inserts proceed.

## Comparison with Vocab Tries

| Feature | `BijectiveMap<V>` | `PersistentVocabARTrie` |
|---|---|---|
| Value type | any `V: Eq + Hash + DictionaryValue` | `u64` (auto-assigned) |
| Backing store | in-memory HashMap | disk-backed ARTrie |
| User-supplied values | yes (via `insert(term, value)`) | no (`insert_with_value` is a no-op, see A4) |
| Persistence | none (in-memory only) | mmap + WAL |
| Cost of reverse lookup | O(1) avg | O(depth of trie) — reconstructed |
| Remove support | none (append-only) | none (append-only) |

Use `BijectiveMap` for in-memory mappings with user-controlled values.
Use `PersistentVocabARTrie` when you need durable storage and the values
are just internal IDs you don't care about choosing.

## Usage Examples

### Token-to-index vocabulary

```rust,no_run
use libdictenstein::bijective::BijectiveMap;
use libdictenstein::MappedDictionary;

let vocab: BijectiveMap<u32> = BijectiveMap::new();
vocab.insert("hello", 0);
vocab.insert("world", 1);

// Forward
assert_eq!(vocab.get_value("hello"), Some(0));
// Reverse
assert_eq!(vocab.get_term(&1), Some("world".to_string()));
```

### Symbol table

```rust,no_run
use libdictenstein::bijective::BijectiveMap;
use libdictenstein::MappedDictionary;

#[derive(Clone, Default, Hash, PartialEq, Eq, Debug)]
struct SymbolId(u64);

impl libdictenstein::value::DictionaryValue for SymbolId {}

let symbols: BijectiveMap<SymbolId> = BijectiveMap::new();
symbols.insert("main", SymbolId(1001));
symbols.insert("foo", SymbolId(1002));

assert_eq!(symbols.get_value("main"), Some(SymbolId(1001)));
assert_eq!(symbols.get_term(&SymbolId(1002)), Some("foo".to_string()));
```

### Try-insert (non-panicking)

```rust,no_run
use libdictenstein::bijective::{BijectiveMap, InsertError};

let bimap: BijectiveMap<i32> = BijectiveMap::new();
bimap.insert("alpha", 1);

// Duplicate term:
assert!(matches!(
    bimap.try_insert("alpha", 2),
    Err(InsertError::DuplicateTerm)
));

// Duplicate value:
assert!(matches!(
    bimap.try_insert("beta", 1),
    Err(InsertError::DuplicateValue)
));
```

## Performance Analysis

| Operation | Time (avg) | Time (worst) |
|---|---|---|
| `insert` / `try_insert` | O(1) hash + write lock | O(n) on rehash |
| `get_value` | O(1) hash + read lock | O(n) on collision |
| `get_term` | O(1) hash + read lock + 1 `String` clone | O(n + \|term\|) |
| `contains_term` / `contains_value` | O(1) hash | O(n) on collision |
| `bijection_len` | O(1) | O(1) |

Memory: roughly `2 × (sizeof(String) + sizeof(V)) × N` where N is the
number of pairs. The hash map storage is duplicated for fast bidirectional
lookup; there is no shared underlying tree.

## When to Use

✅ In-memory bidirectional maps (token vocabularies, symbol tables,
language tag tables).
✅ User-supplied values (not just auto-assigned IDs).
✅ When `Cow::Owned(String)` return on reverse lookup is acceptable.

❌ When you need disk-backed persistence → use `PersistentVocabARTrie`.
❌ When you need to remove entries → no impl supports this; redesign
your data flow.
❌ When you need ordered iteration → `BijectiveMap` uses a `HashMap`
and offers no ordering guarantee.
