# C3 Handoff: Extract `SuffixAutomatonCore<U, V>`

## Goal

Factor `SuffixAutomatonCore<U: CharUnit, V>` parallel to `DawgCore`. Both
the byte-keyed (`SuffixAutomaton<V>`, `src/suffix_automaton.rs`) and
char-keyed (`SuffixAutomatonChar<V>`,
`src/suffix_automaton_char.rs`) variants become thin wrappers.

## Why this couldn't fit in one session

The audit estimated 3-4 weeks. Inspection confirms:

- `src/suffix_automaton.rs` — 1584 LOC plus 6 trait impls and the
  on-line construction state machine.
- `src/suffix_automaton_char.rs` — 1656 LOC, nearly identical structure
  but `char`-keyed.

Suffix-automaton construction state has invariants that span the entire
state graph (suffix links, length intervals, equiv classes). Generifying
without breaking these invariants requires careful refactoring,
including the `extend()` per-character growth algorithm and the
`clone()`-based split for off-suffix-link transitions.

## Pre-flight checklist

1. Read Blumer 1986 / Crochemore Vérin 1997 to refresh the on-line
   construction invariants.
2. Inventory the public surface of each existing variant
   (`cargo doc --no-deps -p libdictenstein` then read the rendered
   pages).
3. Build a generic `SuffixAutomatonInner<U, V>` first (no public
   API change yet) and verify each variant can switch to use it
   internally one method at a time.

## Step-by-step plan

(Each step a commit.)

### Step 1: Sketch the generic state machine

Create `src/suffix_automaton_core/mod.rs` with:

```rust,no_run
pub struct SuffixAutomatonInner<U: CharUnit, V> {
    pub nodes: Vec<SuffixNode<U, V>>,
    pub last_state: usize,
    pub source_texts: Vec<String>,
    pub positions: std::collections::HashMap<usize, Vec<(usize, usize)>>,
    pub string_count: usize,
    // …
}

pub struct SuffixNode<U: CharUnit, V> {
    pub edges: Vec<(U, usize)>,
    pub suffix_link: Option<usize>,
    pub max_length: usize,
    pub is_final: bool,
    pub value: Option<V>,
}
```

### Step 2: Port `extend(u: U)` to the generic state

The current `extend(u8)` in `suffix_automaton.rs:341` and `extend(char)`
in `suffix_automaton_char.rs` are nearly identical apart from edge-label
typing. Move both to `SuffixAutomatonInner::extend(u: U)`.

### Step 3: Port `insert(&str)`, `from_text(&str)`, `from_texts(I)`

For each variant, replace the inherent method's body with a call to
`U::iter_str(s)` then `inner.extend(u)` per unit.

### Step 4: Port `contains`, `match_positions`, `count_substring`,
`find`, `state_count`, `iter_terms`, `source_texts`

Generic over `U: CharUnit`. Each method body becomes:

```rust,no_run
pub fn contains(&self, term: &str) -> bool {
    self.contains_units(&U::from_str(term).as_slice())
}
fn contains_units(&self, units: &[U]) -> bool { ... }
```

### Step 5: Migrate trait impls

`impl<V> Dictionary for SuffixAutomaton<V>` becomes `impl<V> Dictionary
for SuffixAutomatonCore<u8, V>`. The byte/char variants become aliases:

```rust,no_run
pub type SuffixAutomaton<V = ()> = SuffixAutomatonCore<u8, V>;
pub type SuffixAutomatonChar<V = ()> = SuffixAutomatonCore<char, V>;
```

Public API preserved via type aliases.

### Step 6: Tests + benchmarks

Run `cargo test --all-features -- suffix_automaton::` and confirm no
regression. Re-run `cargo bench --bench suffix_*`.

## Expected LOC reduction

- `suffix_automaton.rs`: ~1584 → ~50 LOC (type alias + re-exports)
- `suffix_automaton_char.rs`: ~1656 → ~50 LOC
- New `suffix_automaton_core/`: ~1600 LOC shared
- Net: ~1500 LOC removed.

## Risks

- The `clone()`-based suffix-automaton split in `extend()` creates a
  new node and copies edges; the byte variant uses `find_edge(u8)` and
  the char variant uses `find_edge(char)`. These need a generic
  `find_edge(u: U)` method on `SuffixNode<U, V>`.
- Serialization format: both variants serialize the node array. After
  generification, the format will technically change (Vec<(U, usize)>
  vs Vec<(u8, usize)>) — but this is the same byte-level layout for
  `U = u8` and a format bump for `U = char`. Plan a serialization
  back-compat shim if there are existing on-disk indexes.
