# Design: the dynamic DAWG

Design rationale for the **mutable, minimized** DAWG backends — `DynamicDawg`, `DynamicDawgChar`, and
`DynamicDawgU64` — why they exist and why they are built as they are. The *minimization theory* is in
[`theory/volatile-automata/01-dawg-minimization.md`](../theory/volatile-automata/01-dawg-minimization.md);
the *API and usage* are in
[`algorithms/implementations/dynamic-dawg.md`](../algorithms/implementations/dynamic-dawg.md) (and
[`dynamic-dawg-u64.md`](../algorithms/implementations/dynamic-dawg-u64.md)); the *shared
architecture* is [`architecture/in-memory-dictionaries.md`](../architecture/in-memory-dictionaries.md).
This document is the "why". Notation follows [`docs/notation.md`](../notation.md).

## The requirement it uniquely fills

Among the in-memory backends, the dynamic DAWG is the one that is **mutable at runtime (insert *and*
remove) while staying near-minimal in space**. The [double-array trie](../algorithms/implementations/double-array-trie.md)
is more compact and faster to read but insert-only; the [suffix automaton](suffix-automaton.md) and
[SCDAWG](../algorithms/implementations/scdawg.md) index substrings, not whole terms. When the term
set changes at runtime and you want suffix sharing to keep it small, the dynamic DAWG is the answer.

## Design choices and their rationale

### Preserve sharing and minimize explicitly

A DAWG's value is that identical suffixes are shared (see
[minimization theory](../theory/volatile-automata/01-dawg-minimization.md)). An update retains every
unchanged subgraph by `Arc` and path-copies only its root-to-terminal route. Exact suffix merging is
performed by `compact()` / `minimize()`, which rebuilds and interns equivalent non-value nodes in
$`O(n)`$. The current revision can drift away from the exact minimum between compactions, but it
never copies the whole graph for an ordinary insert, update, or remove.

### Copy-on-write instead of mutating a shared path

Because suffix sharing means many terms' paths run through the same nodes, mutating a term in place
could corrupt an unrelated term or a long-lived reader. Published nodes are therefore immutable.
The design path-copies the edited route bottom-up and structurally shares every unchanged branch,
so an edit never disturbs a co-resident term or any retained query-start revision.

### Immutable revisions with one lock-free root CAS

The live representation is lock-free: one `ArcSwap<GraphVersion>` owns the immutable root and its
revision metadata ([`src/dynamic_dawg/lockfree.rs`](../../src/dynamic_dawg/lockfree.rs)). A reader
loads the root once and traverses it **wait-free**. A writer path-copies only the edited route and
publishes the replacement version with one root `compare_and_swap`; if another writer wins, it
retries from that newer root. Consequently one long-lived iterator sees exactly its query-start
terms and values while fresh iterators see later revisions.

This publication discipline is deliberately adapted from the persistent ARTrie design documented
in [`lockfree-cas-artrie.md`](lockfree-cas-artrie.md): immutable structurally shared nodes plus a
single atomic root publication point. DynamicDAWG uses the same snapshot idea, but its rewrite and
compaction logic is tailored to ordered edges and suffix-sharing DAWG invariants.

### One unit-generic core, separate public alphabets

`DynamicDawg`, `DynamicDawgChar`, and `DynamicDawgU64` present distinct public APIs for byte,
Unicode-scalar, and 64-bit-token alphabets. All three now reuse `LockFreeDawg<U, V>` for identical
revision, path-copy, compaction, and snapshot semantics; the separate public types retain their
alphabet-specific convenience methods and compatibility formats.

### The Bloom-filter knob is vestigial

`with_config` accepts a `bloom_filter_capacity`, a remnant of an earlier design that pre-filtered
negative lookups. The current lock-free read path does an **exact wait-free traversal and consults no
Bloom filter**; the argument is accepted for API compatibility and ignored on the read path. The
reasoning — why an exact walk beat a Bloom pre-filter for an in-memory trie — is in
[theory/volatile-automata/03-bloom-filters.md](../theory/volatile-automata/03-bloom-filters.md).

## Complexity (design targets)

Let `m` be a term's length and `n` the total stored units.

| Operation | Cost |
|-----------|------|
| `contains` | $`O(m)`$, wait-free |
| `insert` / `remove` | $`O(m)`$ path copy, plus root-CAS `CasBackoff` retry under write contention |
| `compact()` | $`O(n)`$ |
| Space | near-minimal (exact minimum immediately after `compact()`) |

## Verification

The mutation semantics and lock-free safety are checked, not assumed:
[`tests/dynamic_dawg_mutation_correspondence.rs`](../../tests/dynamic_dawg_mutation_correspondence.rs)
pins insert/remove/idempotence semantics;
[`tests/query_start_snapshot_correspondence.rs`](../../tests/query_start_snapshot_correspondence.rs)
checks exact retained-root behavior across byte, character, and u64 alphabets; and
[`tests/volatile_lockfree_concurrency.rs`](../../tests/volatile_lockfree_concurrency.rs) drives it
under concurrent readers and writers. See [engineering/testing-strategy.md](../engineering/testing-strategy.md).

## Related

- [theory/volatile-automata/01-dawg-minimization.md](../theory/volatile-automata/01-dawg-minimization.md) — the minimization theory.
- [algorithms/implementations/dynamic-dawg.md](../algorithms/implementations/dynamic-dawg.md) — API, node representation, usage.
- [architecture/in-memory-dictionaries.md](../architecture/in-memory-dictionaries.md) — the shared cores and the two concurrency strategies.
- [design/lockfree-cas-artrie.md](lockfree-cas-artrie.md) — the persistent-node/root-CAS model that inspired revision publication here.
- [design/volatile-concurrency.md](volatile-concurrency.md) — the broader in-memory concurrency model.
