# C5 Handoff: Make DAT family generic over `Unit`

## Goal

Generify `DoubleArrayTrie<V>` and `DoubleArrayTrieChar<V>` into a single
`DATCore<U: CharUnit, V>` type. Both public types become aliases.

## Why this couldn't fit in one session

The audit estimated 1-2 weeks. Inspection:

- `src/double_array_trie.rs` — 1176 LOC.
- `src/double_array_trie_char.rs` — 1251 LOC.
- `src/double_array_trie_zipper.rs` — 268 LOC.
- `src/double_array_trie_char_zipper.rs` — 305 LOC.
- Total: 3000 LOC across the 4 files.

The duplication is real but the algorithms are tightly coupled to the
specific edge type (`Vec<u8>` vs `Vec<char>`). BASE/CHECK manipulation
uses `child_state = base[parent] + (edge as i32)` — works for `u8`
naturally but needs `(edge as u32 as i32)` or `CharUnit::to_i32_offset()`
for `char`.

## Pre-flight

1. Decide whether `CharUnit` needs a new method
   `to_dat_offset(&self) -> i32` or whether `as i32` + cast tricks
   suffice for both `u8` and `char`. (`char` is 32-bit so the cast
   should work but verify safety.)
2. Check if `DATShared<V>` / `DATSharedChar<V>` storage can use
   `Arc<Vec<Vec<U>>>` generically. They currently have a serde
   attribute that references `serialize_arc_vec_vec` — that helper is
   already generic over `T: Serialize`, so should work for both `u8`
   and `char`.

## Step-by-step plan

(Each step a commit.)

### Step 1: Create `DATCoreShared<U, V>`

In `src/dat_core/shared.rs`:

```rust,no_run
pub(crate) struct DATCoreShared<U: CharUnit + Serialize + DeserializeOwned, V: DictionaryValue> {
    pub(crate) base: Arc<Vec<i32>>,
    pub(crate) check: Arc<Vec<i32>>,
    pub(crate) is_final: Arc<Vec<bool>>,
    pub(crate) edges: Arc<Vec<Vec<U>>>,
    pub(crate) values: Arc<Vec<Option<V>>>,
}
```

### Step 2: Port `DATCore<U, V>` lookup methods

`contains`, `get_value`, `transition_state`, `iter_edges_at(state)` —
all generic over `U`.

### Step 3: Port the builder (`DATBuilder<U, V>`)

The bulk of the LOC reduction is here — the placement algorithm,
free-list management, xor-relocation are the same regardless of
`U`-type. Move them into `dat_core/builder.rs`.

### Step 4: Migrate `DoubleArrayTrie<V>` → `DATCore<u8, V>`

Replace the public type with `pub type DoubleArrayTrie<V = ()> =
DATCore<u8, V>;` and keep the old methods either as inherent on
`DATCore` (generic) or as trait impls.

### Step 5: Migrate `DoubleArrayTrieChar<V>` → `DATCore<char, V>`

Same.

### Step 6: Migrate the zipper variants

`DoubleArrayTrieZipper` / `DoubleArrayTrieCharZipper` → `DATCoreZipper<U, V>`.

### Step 7: Verify

- Existing 16 DAT tests pass.
- `cargo bench --bench serialization_benchmarks` shows no > 2 %
  regression.

## Expected LOC reduction

- 4 files × ~750 LOC average → 4 thin alias files × ~50 LOC + 1 generic
  module ~1500 LOC.
- Net: ~1200 LOC removed.

## Risks

- On-disk format compatibility: the byte and char variants serialize
  with the same Serde attributes (now `crate::serialization::
  serde_helpers::*` after C2). The byte format will be byte-for-byte
  unchanged. The char format will change subtly because `Vec<Vec<u8>>`
  → `Vec<Vec<char>>` serialization differs. Plan a version bump for
  char on-disk format.
- The `free_list` and `rebuild_threshold` fields are RESERVED-FOR-FUTURE
  (see B5). They stay in the generic struct but remain `dead_code`.
