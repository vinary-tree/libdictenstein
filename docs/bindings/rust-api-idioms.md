# Pure Rust collection and iterator idioms

**Status:** confirmed surface audit and implementation plan. Existing APIs named
below ship today; target traits and uniformity requirements do not ship until
their work package passes the stated gates.

Rust is the semantic and performance reference for collection traversal. The
native API must be pleasant without a foreign ABI, and foreign facades must not
force callbacks, vtables, leased C buffers, or dynamic dispatch into the
monomorphic Rust hot path.

## Confirmed current surface

The crate already provides a strong base:

- `Dictionary`, `MappedDictionary`, `MutableDictionary`, and related capability
  traits separate membership, values, and supported mutation;
- `DictionaryIterator<Z>` and `DictionaryTermIterator<Z>` provide iterative
  zipper traversal;
- byte and character Dynamic DAWG, double-array trie, and suffix-automaton types
  implement borrowed `IntoIterator`;
- the byte PathMap dictionary implements borrowed `IntoIterator`;
- persistent byte, character, and `u64` ARTrie variants expose backend-specific
  iteration, including a lazy character `entries_stream` pinned to an immutable
  overlay revision; and
- `PersistentARTrieChar` implements `FromIterator<String>` and
  `FromIterator<&str>`.

The surface is not yet uniform. The following are confirmed gaps rather than
aspirational guesses:

| Area | Current inconsistency | Target |
|---|---|---|
| Backend coverage | Borrowed `IntoIterator` is limited to seven in-memory byte/character types | All applicable DAWG, double-array, PathMap, ARTrie, suffix, SCDAWG, vocabulary, bijective, byte/scalar/`u64`, persistent/shared variants |
| Construction | Only character persistent ARTrie implements standard `FromIterator`; it loops over individual inserts | Bulk-builder-backed `FromIterator` for infallible types; named fallible and sorted constructors for durable stores |
| Mutation | No uniform `Extend` surface | `Extend` only where mutation is infallible; `try_extend` and batch atomicity policy elsewhere |
| Entry model | Some iterators emit `(key, V)`, some terms only, some skip term-only finals, some use `(key, Option<V>)` | One lossless entry model preserving final-without-value separately from absence |
| Naming | `iter`, `iter_bytes`, `iter_chars`, `iter_terms`, `iter_sequences`, `iter_with_values`, and `entries_stream` differ by backend | Consistent `keys`, `entries`, `values`, raw-domain names, prefix variants, and compatibility aliases |
| Laziness | Several persistent convenience iterators first collect a complete `Vec` | Snapshot-pinned O(depth) streaming by default; explicit `to_*` materializers |
| Iterator traits | Generic iterators implement `Iterator` but do not uniformly expose `FusedIterator` or useful `size_hint` | Every sound standard iterator trait, with no false exactness |
| Ordering | Backend traversal order is not one documented cross-backend contract | Deterministic lexicographic unit order or an explicitly named unordered traversal |
| Query consumers | liblevenshtein query types are lazy iterators but trait metadata and borrowed/reducer forms vary | Common exhaustion, order, cancellation-by-drop, collection, and reducer laws |

`Index` is intentionally not a target. Concurrent mutation and cloned values do
not generally permit a sound, stable `&V`. `Deref` to `HashMap`/`BTreeMap` would
also misrepresent the automata and persistence contracts.

## Target user experience

Infallible in-memory dictionaries should support ordinary Rust composition:

```rust,ignore
let dictionary: DynamicDawg<u64> = entries.into_iter().collect();
dictionary.extend(more_entries);

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
let dictionary = PersistentARTrie::try_from_iter(path, entries)?;
dictionary.try_extend(more_entries)?;

let snapshot = dictionary.snapshot();
for entry in snapshot.entries() {
    consume(entry?);
}
```

The actual item should be infallible after a successfully captured in-memory
snapshot whenever possible. If lazy storage I/O can still fail, use an
`Iterator<Item = Result<Entry, Error>>`; never discard the error or silently
terminate iteration.

## Generic implementation shape

Introduce narrow capability traits rather than one universal collection trait:

- `DictionaryKeys` for term membership traversal;
- `DictionaryEntries` for lossless key/value-state traversal;
- `SnapshotEntries` for an owned immutable revision;
- `DictionaryFold` for allocation-reusing callbacks; and
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

## Delivery sequence

1. Specify the lossless entry and capability traits; compile-fail-test invalid
   combinations.
2. Build the generic snapshot traversal and reference-model law suite.
3. Migrate the existing seven borrowed `IntoIterator` implementations and the
   character persistent stream to the common engine without regression.
4. Add uncovered automata and unit domains; retain only measured specializations.
5. Add standard construction/mutation traits and explicit fallible variants,
   routing all of them through optimized bulk paths.
6. Complete query-result iterator idioms in liblevenshtein.
7. Expose the optional batched family ABI and build language-native collection
   adapters from it.
8. Run semantic, Miri/sanitizer, concurrency, allocation, profiler, and admitted
   performance gates before documenting a surface as shipped.

The cross-language target and lifecycle rationale are normative in the family
[collection-protocol plan](https://github.com/vinary-tree/liblevenshtein-rust/blob/master/docs/bindings/collection-protocols.md).
