# C4 Handoff: Extract `ScdawgCore<U, V>`

## Goal

Factor `ScdawgCore<U: CharUnit, V>` parallel to `SuffixAutomatonCore`.
The byte-keyed (`Scdawg<V>`, `src/scdawg.rs`) and char-keyed
(`ScdawgChar<V>`, `src/scdawg_char.rs`) variants become thin wrappers.

## Why this couldn't fit in one session

The audit estimated 3-4 weeks. Inspection:

- `src/scdawg.rs` — 1213 LOC, batch construction + IS-features
  (`find`, `match_positions`, `count_substring`) per Blumer 1987.
- `src/scdawg_char.rs` — 1045 LOC, parallel.

Differs from C3 (SuffixAutomatonCore) in that SCDAWG is batch-
constructed; per-character on-line construction isn't supported. The
generic factor would mirror C3's shape minus the per-character
`extend()` state machine.

## Pre-flight + step-by-step plan

Follow the C3 handoff structure (see
[c3-suffix-automaton-core-handoff.md](c3-suffix-automaton-core-handoff.md))
with these substitutions:

- `SuffixAutomatonCore` → `ScdawgCore`
- `extend(u: U)` step omitted; instead port `compute_left_edges()` and
  the batch `from_terms(I)` body.
- The Blumer "IS-features" (`find` / `match_positions` /
  `count_substring`) are inherent methods on the public type today;
  they need to become generic on `ScdawgCore`.

## Expected LOC reduction

- `scdawg.rs`: ~1213 → ~50 LOC
- `scdawg_char.rs`: ~1045 → ~50 LOC
- New `scdawg_core/`: ~1200 LOC shared
- Net: ~900 LOC removed.

## Risks

- The `compute_left_edges()` post-pass uses `inner.write()` and walks the
  whole node graph. Generification needs to keep this pass type-safe
  across `U: CharUnit`.
- The `ScdawgNodeHandle<V>` type today exposes `ScdawgInner<V>`
  internals. After generification this needs to become
  `ScdawgCoreHandle<U, V>` exposing `ScdawgCoreInner<U, V>`, with the
  same `Dictionary` / `MappedDictionaryNode` impls.
