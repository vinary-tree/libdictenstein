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

### Minimize incrementally, not just on demand

A DAWG's value is that identical suffixes are shared (see
[minimization theory](../theory/volatile-automata/01-dawg-minimization.md)). The design keeps the
graph near-minimal *as it mutates* (`minimize_incremental`) rather than only on an explicit
`compact()`, so memory does not balloon between compactions. Minimization is driven by a single
`u64` **signature** per node ([`src/node_signature.rs`](../../src/node_signature.rs)) with a
structural re-check on hash collision — an $`O(1)`$ merge test instead of a recursive right-language
comparison. `compact()` remains available for a full $`O(n)`$ rebuild to the exact minimum.

### Copy-on-write before mutating a shared path

Because suffix sharing means many terms' paths run through the same nodes, mutating a term in place
could corrupt an unrelated term that shares those nodes. The design does **copy-on-write**
(`make_path_unique`) on the shared portion of a path *before* editing it, so an edit never disturbs a
co-resident term. This is the invariant that lets minimization and mutation coexist safely.

### Per-node lock-free CAS for concurrency

The live representation is lock-free: `LockFreeDawgNode` holds its edge list behind an
`ArcSwap<LockFreeEdgeList>` and its value behind an `ArcSwapOption`
([`src/dynamic_dawg/lockfree.rs`](../../src/dynamic_dawg/lockfree.rs)). Readers load an edge-list
snapshot **wait-free**; a writer rebuilds and CAS-publishes only the edge lists of the nodes on the
edited path. The rationale for **per-node** CAS (rather than the whole-graph snapshot the suffix
automaton uses) is edit locality: a DAWG insert/remove touches one root-to-node path plus whatever
minimization merges, so publishing per node maximizes reader/writer independence — a writer editing
`"cat"` never disturbs a reader on `"dog"`. The full contrast is in
[design/volatile-concurrency.md](volatile-concurrency.md).

### A separate `u64` type, not `DynamicDawg` over `u64`

`DynamicDawg` and `DynamicDawgChar` share one arena core (`DawgCore<U, V>`). `DynamicDawgU64` does
**not** reuse it, because a 64-bit-token alphabet breaks the shared core's byte-expansion assumptions;
it uses per-node `Arc` nodes with atomic edge lists instead. The rationale (and the memory trade-off)
is documented at
[dynamic-dawg-u64.md §Why a distinct type](../algorithms/implementations/dynamic-dawg-u64.md#why-a-distinct-type-instead-of-dynamicdawg-over-u64).

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
| `insert` / `remove` | $`O(m)`$ amortized (per-node CAS, `CasBackoff` retry) |
| `compact()` | $`O(n)`$ |
| Space | near-minimal (exact minimum immediately after `compact()`) |

## Verification

The mutation semantics and lock-free safety are checked, not assumed:
[`tests/dynamic_dawg_mutation_correspondence.rs`](../../tests/dynamic_dawg_mutation_correspondence.rs)
pins insert/remove/idempotence semantics;
[`tests/dynamic_dawg_u64_correspondence.rs`](../../tests/dynamic_dawg_u64_correspondence.rs) runs the
per-node CAS path under **loom**; and
[`tests/volatile_lockfree_concurrency.rs`](../../tests/volatile_lockfree_concurrency.rs) drives it
under concurrent readers and writers. See [engineering/testing-strategy.md](../engineering/testing-strategy.md).

## Related

- [theory/volatile-automata/01-dawg-minimization.md](../theory/volatile-automata/01-dawg-minimization.md) — the minimization theory.
- [algorithms/implementations/dynamic-dawg.md](../algorithms/implementations/dynamic-dawg.md) — API, node representation, usage.
- [architecture/in-memory-dictionaries.md](../architecture/in-memory-dictionaries.md) — the shared cores and the two concurrency strategies.
- [design/volatile-concurrency.md](volatile-concurrency.md) — why per-node CAS here.
