# Disabled: `find_or_create_suffix` (Phase 2.1 suffix-share cache)

## Status

The Phase 2.1 suffix-share cache (`find_or_create_suffix` +
`compute_suffix_hash` + `verify_suffix_match` + `create_suffix_chain`) in
`src/dynamic_dawg.rs:1130-1259` and the parallel block in
`src/dynamic_dawg_char.rs:957-1090` is **structurally present but
unreachable**: every method is marked `#[allow(dead_code)]` and is not called
from `insert()` or `insert_with_value()`.

This file documents why, and what would need to change to re-enable it.

## Intent

The cache memoizes suffix-chain endpoints (e.g. the chain `"ing"` or
`"tion"`) so a second insertion sharing the same suffix can reuse the chain
instead of rebuilding it. For natural-language dictionaries with heavy
suffix overlap the original estimate was a **20–40 % memory reduction**.

## Why it's currently disabled

The dynamic-insertion path violates the cache's "suffix endpoint is final
and has no other in-edges" invariant: a subsequent insert can attach a new
in-edge to an interior node of a cached suffix chain, at which point sharing
that endpoint with a later insert breaks language semantics (the cached
endpoint now represents more than one suffix). The previous attempt to
honor this invariant via `verify_suffix_match` introduced false negatives
that wasted the cache lookup; the version saved on disk represents the
state at which the team chose to disable the feature pending a correct
design.

## What's preserved

Following the project policy *never disable code by deleting it*, the
cache methods stay in the source tree under `#[allow(dead_code)]`. They
include:

- `DynamicDawgInner::suffix_cache` (the `FxHashMap<u64, usize>` storage)
- `DynamicDawgInner::find_or_create_suffix` (entry point)
- `DynamicDawgInner::compute_suffix_hash` (FxHasher over `(suffix, is_final)`)
- `DynamicDawgInner::verify_suffix_match` (collision check)
- `DynamicDawgInner::create_suffix_chain` (linear-chain builder)
- Mirrored quartet in `DynamicDawgCharInner`

The `is_final` flag passed through the API hints at where the previous
team had been trying to draw the validity line. Future work should
re-examine whether `is_final` is the right cache key or whether the cache
should also key on "no incoming edges other than the one being added."

## To re-enable

1. Reproduce the failing scenario as a regression test under
   `tests/dawg_suffix_cache_correctness.rs` (does not exist yet — needs
   creation). The failing scenarios the original team encountered involved
   inserting `["testing", "test", "tester"]` in that order, then verifying
   each is still recoverable by `contains()`.
2. Decide on the validity rule. Two candidates:
   - Only cache suffixes terminating in a final, leaf-only state. Cheap
     invariant; gives up the ability to share `"est"` between `"test"` and
     `"tester"`.
   - Maintain an in-edge count per node and invalidate cache entries when
     the count rises above 1 on any node along the chain. More expensive
     bookkeeping but better sharing.
3. Wire the chosen rule into the relevant `find_or_create_suffix` call
   sites inside `insert()` and `insert_with_value()`.
4. Bench against a real-world wordlist (Birkbeck / SCOWL / etc.) before and
   after the change. The 20–40 % memory-reduction target needs evidence,
   not just intuition.

## Where it would plug in

`DynamicDawg::insert` currently builds new chains node-by-node. The cache
would replace the body of the inner loop that appends bytes after the
common-prefix walk: instead of allocating a new chain end-to-end, look up
the remaining suffix's hash, and either splice in the cached endpoint or
fall back to `create_suffix_chain`.

## Tracked under

Plan item B4 ("Disabled suffix-share cache") in
`/home/dylon/.claude/plans/rust-backtrace-1-rust-log-debug-cargo-n-purrfect-lemon.md`.
