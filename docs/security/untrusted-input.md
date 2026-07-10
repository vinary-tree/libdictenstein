# Untrusted input: adversarial keys, DoS, and panics

**Navigation**: [← Security](README.md) · [Threat model](threat-model.md)

This document analyzes what an adversary who controls the **term set**, the **queries**, or the
**concurrent schedule** can do — the availability (DoS) and panic surface of the in-memory
dictionaries. The separate concern of adversarial *serialized bytes* is
[deserialization-safety.md](deserialization-safety.md). Notation follows [`docs/notation.md`](../notation.md).

Every source citation below was checked against the tree; line numbers are approximate anchors.

## Memory safety under adversarial keys — safe by construction

The in-memory dictionaries store nodes in a **flat `Vec` arena addressed by integer index**, with
edges holding `usize` child indices rather than `Box`/`Arc` child pointers:
`DawgCore` ([`src/dynamic_dawg/core.rs`](../../src/dynamic_dawg/core.rs)),
`SuffixAutomatonInner` ([`src/suffix_automaton/core/inner.rs`](../../src/suffix_automaton/core/inner.rs)),
`ScdawgCoreInner` ([`src/scdawg/core/inner.rs`](../../src/scdawg/core/inner.rs)), and the
`BASE`/`CHECK` integer arrays of the double-array trie. Two consequences follow, and both close DoS
vectors that pointer-based tries are prone to:

- **No recursive drop.** There is no manual `impl Drop` anywhere in the volatile tree; tearing down a
  dictionary drops a `Vec`, which is iterative. A pathologically deep or long key therefore **cannot
  overflow the stack on drop** — the classic failure mode of a `Box`-linked trie.
- **No recursive traversal.** Lookup, iteration, and mutation walk the arena by index (`edges()`),
  not by recursion; there is no `fn dfs`/`visit` over key length or trie depth in the volatile code.
  A very long query walks a bounded loop, not a growing call stack.

## Denial-of-service surface

| Vector | Bound | Verdict |
|--------|-------|---------|
| Deep/long keys → stack overflow | arena + iterative traversal (above) | **not exploitable** |
| Recursive drop | no manual `Drop`; `Vec` drop is iterative | **not exploitable** |
| Suffix-automaton state blow-up | $`\le 2\lvert T\rvert - 1`$ states, $`\le 3\lvert T\rvert - 4`$ transitions — **linear** in indexed text | expected cost, not pathological |
| DAWG minimization | signature is one `u64` FxHash + structural re-check; per-node $`O(\text{edges}\log\text{edges})`$ | not amplifiable beyond the term set |
| **`char` double-array build with high-codepoint keys** | per-node offset span is the full codepoint range (up to `0x10FFFF`) | **build-time memory amplification** — see below |
| Adversarial *query* against a built dictionary | wrapping-add then bounds-check on every transition | safe; a hostile query is rejected, never OOB |
| Concurrent writers | lock-free CAS with bounded `CasBackoff` | no live-lock; see [volatile-concurrency](../design/volatile-concurrency.md) |

### The one real amplification: `char` double-array construction

`char::to_dat_offset` maps a code point to an array offset by its scalar value
([`src/char_unit.rs`](../../src/char_unit.rs)), so a `DoubleArrayTrieChar` built from keys containing
very high code points spreads its `BASE`/`CHECK` arena across a correspondingly large index range.
This is a **build-time** memory cost proportional to the codepoint range of the key set, not a
query-time cost. If you build a `char` double-array trie from **untrusted** Unicode keys, treat the
build as an allocation proportional to the maximum code point present, and prefer a
[`DynamicDawgChar`](../algorithms/implementations/dynamic-dawg-char.md) (which does not densely index
the codepoint range) when that is a concern. Byte and `char` *reads* are unaffected: transitions use
`wrapping_add` followed by a `check.len()` bounds test, so even a wrapped index yields a rejected
transition, never an out-of-bounds access
([`src/double_array_trie/core/shared.rs`](../../src/double_array_trie/core/shared.rs);
`char.rs` / `char_zipper.rs`).

### No fuzzing harness yet

The crate has no `cargo-fuzz` / libFuzzer / AFL target. The natural first fuzz targets, given the
analysis here and in [deserialization-safety.md](deserialization-safety.md), are the **protobuf
importer** and the **persistent WAL / arena loaders** — the paths that turn adversarial *bytes* into
structure. This is a known gap, recorded here rather than hidden.

## Panic surface

The library favors `Result`/`Option` over panics on the query and mutation surface, so the
adversary-reachable panic surface is small and enumerable.

- **The one caller-reachable production panic: `BijectiveMap::insert`.**
  [`src/bijective/bijective_map.rs`](../../src/bijective/bijective_map.rs) documents that `insert`
  (and `from_pairs`, which uses it) **panics** on a duplicate term *or* duplicate value, to protect
  the bijection invariant. If either the term or the value can come from an untrusted source, use the
  non-panicking **`try_insert`**, which returns `Result<(), InsertError>`. This is the single place
  in the volatile tree where attacker-influenced input can panic by design.
- **No `unreachable!` / `unimplemented!` / `todo!`** exists anywhere in the volatile tree.
- **`expect`/`unwrap`** occurrences are overwhelmingly in test code or guard *internal invariants*
  checked one line earlier (e.g. an `unwrap` after an explicit `is_some()` test); no input-derived
  `unwrap` was found on the volatile read path.
- **The trait surface returns options.** `Dictionary` / `MappedDictionary` / `MutableDictionary`
  ([`src/lib.rs`](../../src/lib.rs)) return `Option` / `bool`; serialization returns
  `Result<_, SerializationError>`; persistent open/mutation returns `Result<_, PersistentARTrieError>`.

> A crate-health scan may report a large "may panic" count. That figure is inflated by test
> assertions and by every indexing/arithmetic site the analyzer conservatively flags; it is **not**
> the count of production panics reachable from attacker input. The reachable set is the one item
> above (`BijectiveMap::insert`) plus the allocation-sizing aborts covered in
> [deserialization-safety.md](deserialization-safety.md).

## Integer handling

- `to_dat_offset`: `u8` and `char` map to an offset losslessly. The `u64` implementation is a
  deliberate `u64 → u32 → usize` truncation, documented in [`src/char_unit.rs`](../../src/char_unit.rs)
  as intentional — `u64` backends are not double-array-backed, so this path is not used for DAT
  indexing.
- Double-array **reads** are `wrapping_add` then bounds-checked (safe, above). The least-defended
  integer site is the `char` DAT **builder**
  ([`src/double_array_trie/char.rs`](../../src/double_array_trie/char.rs)), which indexes internal
  arrays by the wrapped offset; this is a build-time internal invariant (the allocator sizes the
  arena to fit), not reachable from a query.

## Recommendations for callers handling untrusted input

1. **Untrusted term/value into a `BijectiveMap`?** Use `try_insert`, not `insert`.
2. **Untrusted Unicode key set?** Prefer a `DynamicDawgChar` over a `DoubleArrayTrieChar` if the
   codepoint range could be adversarially large, or cap the build size.
3. **Untrusted serialized blob?** Read [deserialization-safety.md](deserialization-safety.md) first —
   bound the input size before handing it to `deserialize`.
