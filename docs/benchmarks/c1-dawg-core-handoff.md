# C1 Handoff: Migrate DAWG variants to `DawgCore<U, V>`

## Goal

Make `DynamicDawg<V>` / `DynamicDawgChar<V>` / `DynamicDawgU64<V>` thin
wrappers over `crate::dawg_core::DawgCore<U, V>`. Delete the
duplicate `BloomFilter` / `NodeSignature` / `DynamicDawgInner`
declarations local to each variant.

## Why this couldn't fit in one session

The audit estimated 2-3 weeks. Verified by inspection:

- `src/dynamic_dawg.rs` — 2321 LOC. Local declarations at line 98
  (`BloomFilter`), line 110 (`NodeSignature`), line 1114
  (`DynamicDawgInner`). Method count: ~25 public methods + ~20 internal
  helpers.
- `src/dynamic_dawg_char.rs` — 2148 LOC. Identical layout, `char`-keyed.
- `src/dynamic_dawg_u64.rs` — smaller (~600 LOC) but its `u64`-keyed
  algorithms are subtly different (Vec edge format).

Each method needs to be migrated to use `DawgCore`'s generic helpers
while preserving public API. Per-impl test verification required at
every step (existing test suite is large).

## Pre-flight checklist

1. Verify `dawg_core::DawgCore<U, V>` already has the full method
   surface needed:
   - `pub fn new() -> Self`
   - `pub fn from_terms<I, S>(terms: I) -> Self where I: IntoIterator<Item = S>, S: AsRef<str>`
   - `insert(&self, term: &str) -> bool`
   - `insert_with_value(&self, term: &str, value: V) -> bool`
   - `remove(&self, term: &str) -> bool`
   - `contains(&self, term: &str) -> bool`
   - `get_value(&self, term: &str) -> Option<V>`
   - `compact(&self)`, `needs_compaction(&self)`, `minimize(&self)`
   - Zipper integration
2. If `DawgCore` is missing methods, extend it FIRST (commit each
   addition individually). Tests in `dawg_core/tests.rs`.

## Step-by-step migration plan

Each step is its own commit.

### Step 1: Audit DawgCore API surface

Run `rg -n 'pub fn|pub struct|impl' src/dawg_core.rs > /tmp/dawg_core_api.txt`.
Cross-reference with the public methods on `DynamicDawg<V>`. Identify gaps.

### Step 2: Extend DawgCore for any missing methods

Add methods to `DawgCore<U, V>`. One commit per method or small group.

### Step 3: Migrate `DynamicDawg<V>` to `DawgCore<u8, V>`

- Replace the inner type with `DawgCore<u8, V>`.
- Delete the local `BloomFilter`, `NodeSignature`, `DynamicDawgInner`
  declarations.
- Each public method forwards to the core's method.
- Run `cargo test --all-features -- dynamic_dawg::` at each step.

### Step 4: Migrate `DynamicDawgChar<V>` to `DawgCore<char, V>`

Same pattern as Step 3.

### Step 5: Migrate `DynamicDawgU64<V>` to `DawgCore<u64, V>`

Trickier due to the `u64`-keyed differences. May require extending
`DawgCore`'s edge-storage abstraction.

### Step 6: Verify, benchmark

- `cargo test --all-features` clean.
- `cargo bench --bench dawg_*` — no > 2% regression vs pre-migration
  baseline. Record numbers in `docs/benchmarks/tier1-ledger.md`.

## Expected LOC reduction

- `dynamic_dawg.rs`: ~2321 → ~400 LOC (thin wrapper)
- `dynamic_dawg_char.rs`: ~2148 → ~400 LOC
- `dynamic_dawg_u64.rs`: ~600 → ~300 LOC
- Net: ~3700 LOC removed across the family.

## Risks

- `DynamicDawgU64`'s `u64` algorithm has subtly different invariants
  (8-byte-aligned chunks, padding). May need `DawgCore` to gain a
  generic edge-storage trait before this works cleanly.
- Bloom filter false-positive rate is tuned per-variant; ensure
  `DawgCore`'s shared bloom filter uses the same constants.
- The `find_or_create_suffix` cache is currently disabled (B4); if
  someone re-enables it in `DawgCore`, the char and u64 variants get
  it "for free" — verify the invariant holds for all three before
  enabling.
