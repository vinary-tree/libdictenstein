# Pure Rust collection and iterator idioms

**Status:** shipped and gated. The common collection views, borrowed and
snapshot-owning iteration, infallible construction traits, explicit fallible
bulk APIs, and lazy zipper traversal described below are implemented.

Rust is the semantic and performance reference for collection traversal. The
native API must be pleasant without a foreign ABI, and foreign facades must not
force callbacks, vtables, leased C buffers, or dynamic dispatch into the
monomorphic Rust hot path.

## Shipped surface

The crate provides one lossless, snapshot-consistent collection layer:

- `Dictionary`, `MappedDictionary`, `MutableDictionary`, and related capability
  traits separate membership, values, and supported mutation;
- `DictionaryEntry<U, V>` represents every stored final as an owned key plus
  `Option<V>`; `None` means present without a mapped value, not absent;
- `DictionaryEntries`, `DictionaryTerms`, `DictionaryKeys`, and
  `DictionaryValues` provide consistent views, while `fold_entries` and
  `try_fold_entries` let cursor-backed implementations reuse one path buffer;
- borrowed `IntoIterator` captures one revision across Dynamic DAWG,
  double-array trie, PathMap, persistent ARTrie, suffix, SCDAWG, vocabulary,
  and bijective families in their applicable byte, scalar, and `u64` forms;
- public PathMap snapshot types also implement consuming `IntoIterator`;
- `Arc<D>` shared handles delegate the same collection views to `D` and retain
  the same captured-revision behavior;
- byte, character, and `u64` Dynamic DAWGs implement bulk-builder-backed
  `FromIterator` for owned and borrowed text/unit keys and key/value pairs;
- byte and character double-array tries implement the corresponding
  `FromIterator` forms through their two-phase static builder;
- mutable Dynamic DAWG, PathMap, SCDAWG, and suffix-automaton families implement
  `Extend`; immutable double-array tries deliberately do not;
- persistent and invariant-checked stores expose named `try_from_iter`,
  `try_extend`, entry, and stable-sorted variants instead of standard traits
  that cannot return their errors; and
- `ZipperCollection` and `ValuedZipperCollection` traverse union,
  intersection, difference, symmetric-difference, and other zipper views lazily
  without materializing a result dictionary.

`Index` is intentionally not a target. Concurrent mutation and cloned values do
not generally permit a sound, stable `&V`. `Deref` to `HashMap`/`BTreeMap` would
also misrepresent the automata and persistence contracts.

## Target user experience

Infallible in-memory dictionaries should support ordinary Rust composition:

```rust,ignore
let mut dictionary: DynamicDawg<u64> = entries.into_iter().collect();
std::iter::Extend::extend(&mut dictionary, more_entries);

for entry in &dictionary {
    consume(entry);
}

let selected: Vec<_> = dictionary
    .entries()
    .filter(|entry| predicate(entry))
    .collect();
```

Fallible persistent stores remain honest about failure:

```rust,ignore
let dictionary = PersistentARTrie::try_from_entries(entries)?;
dictionary.try_extend_entries(more_entries)?;

for entry in dictionary.entries() {
    consume(entry);
}
```

These bulk methods have explicit prefix-commit semantics: successful writes
before the first error remain visible. Sorted variants stably sort first, then
apply the sorted prefix. A failing `try_from_*` constructor returns no partial
dictionary, because its private partial value is dropped.

### The two `extend` APIs

The Dynamic DAWG types predate their standard collection implementations and
already expose an inherent, key-only `extend(&self, terms) -> usize`; the
[`MutableDictionary::extend`] capability trait has the same count-returning
shape. Inherent methods win method lookup, so use explicit UFCS whenever the
standard trait is intended, especially for key/value pairs:

```rust,ignore
let added = MutableDictionary::extend(&dictionary, more_keys);
std::iter::Extend::extend(&mut dictionary, more_entries);
```

The first expression reports newly added keys. The second follows the standard
`Extend` contract and returns `()`. This distinction is retained for source
compatibility rather than silently changing the established batch API.

## Generic implementation shape

Narrow capability traits avoid pretending that every backend has the same
mutation or storage contract:

- `DictionaryEntries` for lossless key/value-state traversal;
- `DictionaryTerms`, `DictionaryKeys`, and `DictionaryValues` for derived views;
- `DictionaryEntries::{fold_entries, try_fold_entries}` for
  allocation-reusing callbacks;
- `ZipperCollection` and `ValuedZipperCollection` for lazy derived sets; and
- existing mutation/persistence traits for construction and update.

Associated iterator types or generic associated types preserve static dispatch.
One `SnapshotEntryTraversal` should use the existing zipper/snapshot-root and
compact-graph seams. An automaton may specialize its cursor representation, but
it must share the entry, order, snapshot, and property-law layer. Specialization
requires repeatable evidence of a material benefit.

The iterator implementation must be iterative and stack-safe. Capture one
immutable root, retain no read lock across user code, maintain one reusable DFS
stack/path arena, and construct an owned key only at a terminal. Traverse child
labels in an order that yields lexicographic keys without a whole-output sort.
For DAWGs, traversal state represents paths rather than globally marking shared
nodes visited, because one shared final can correspond to multiple keys.

## Standard trait policy

Implement traits only when their laws and complexity remain true:

- `IntoIterator for &D`: borrowed dictionary, iterator owns a revision pin;
- `IntoIterator for Snapshot<D>`: consuming snapshot, naturally owning;
- `FusedIterator`: after `None`, always `None`;
- `size_hint`: exact only from snapshot cardinality and only without filtering;
- `ExactSizeIterator`: only when remaining length is maintained in O(1);
- `FromIterator`/`Extend`: only for infallible operations, routed to optimized
  bulk builders;
- `try_from_iter`/`try_extend`: persistent I/O, allocation, transactional, or
  validation failure;
- `DoubleEndedIterator`: only for a native reverse traversal; and
- `Send`/`Sync`: derived from owned snapshot/cursor state, never asserted to
  paper over a backend restriction.

Provide inherent methods even when standard traits exist so raw byte/scalar/
`u64` domains and tri-state values stay explicit. Compatibility aliases can be
deprecated only after all downstream crates and examples migrate.

## Optimization requirements

The direct Rust path is benchmarked separately from the ABI and must have:

1. no FFI call, C descriptor conversion, vtable dispatch, or per-edge atomic;
2. no dictionary-wide pre-materialization for a streaming iterator;
3. O(1) snapshot capture through structural sharing;
4. O(nodes + yielded units) traversal and O(depth) iterator memory;
5. allocation reuse in fold/visitor paths and exact `collect` reservation when
   cardinality is known;
6. early-drop reclamation with bounded destructor work; and
7. no lock held while user code processes an item.

Measure direct backend iteration, generic traversal, visitor/fold, prefix
iteration, early cancellation, and `collect` independently. Record throughput,
latency distributions, allocations, bytes copied, peak memory, and scalability.
Profile transition/edge processing, path materialization, snapshot retention,
and arena access using the family
[optimization methodology](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/benchmarks/optimization-and-profiling-methodology.md).

## Cross-automaton conformance matrix

Every applicable concrete type must be classified before implementation:

| Family | Variants to gate | Special semantic concern |
|---|---|---|
| Dynamic DAWG | byte, scalar, `u64` | shared suffix paths; concurrent immutable revisions |
| Double-array trie | byte, scalar | static cardinality and compact-index fast path |
| PathMap | byte, scalar and snapshots | dependency cursor/order behavior |
| Persistent ARTrie | byte, scalar, `u64`, shared | fallible I/O, overlays, term-only finals, snapshot pinning |
| Suffix automaton | byte, scalar, persistent variants | term dictionary versus substring language semantics |
| SCDAWG | byte, scalar, persistent variants | stored entries versus substring occurrences |
| Vocabulary ARTrie | persistent/shared | bijection and stable indices |
| Bijective dictionary | supported unit domains | key and reverse-value views |
| Set-operation zippers | union/intersection/difference/symmetric difference | lazy derived view order and duplicate elimination |

Property tests compare each emitted collection to a `BTreeSet` or `BTreeMap`
reference, including empty keys, arbitrary bytes, Unicode, `u64::MAX`, duplicate
construction, shared suffixes, term-only/valued entries, concurrent mutation,
compaction, checkpoint/reopen, prefix bounds, and early iterator drop.

## Verification

`tests/borrowed_into_iterator_laws.rs` gates snapshot ownership, lossless
values, exact size where sound, mutation after iterator start, and fused
exhaustion. `tests/standard_collection_construction.rs` and
`tests/collection_idiom_laws.rs` provide compile-time trait matrices and
reference laws for construction, folds, consuming snapshots, fallible bulk
methods, and lazy set-operation traversal. Suffix-specific laws separately
ensure stored source records are not confused with the recognized substring
language.

The cross-language target and lifecycle rationale are normative in the family
[collection-protocol plan](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/bindings/collection-protocols.md).
