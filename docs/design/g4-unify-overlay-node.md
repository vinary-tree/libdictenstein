Confirmed. `KeyEncoding` is defined but barely adopted (only `key_encoding.rs` + two `arena.rs` magic-constant assertions). `SwizzledPtr` is canonically in `persistent_artrie_core::swizzled_ptr` and re-exported through `persistent_artrie::swizzled_ptr` — so the shared overlay node in `core` can import `crate::persistent_artrie_core::swizzled_ptr::SwizzledPtr` with zero layering violation. The Inline filler is `[0u8;4]`/`[0u32;4]` — dead padding, needs only a `zero()` provider on the key trait. Byte does not reference char/vocab (layering clean).

I have exhaustively verified the codebase. Here is the complete design.

---

# G4 — Unify the Lock-Free Overlay Node into a Single Generic `OverlayNode<U, V>`

**Status:** implementation-ready design (reversible G4; G5 production flip is out of scope).
**Audience:** the engineer who will execute this. Persist to `docs/design/g4-unify-overlay-node.md`.
**Verified against:** byte/char/vocab overlay sources, the `CharUnit`/`KeyEncoding` traits, the C3/C4/C5 precedent, the loom/proptest/TLA suite, and the unsafe-inventory gate — all read in full (paths cited inline).

---

## 0. Executive summary / the shape of the answer

The byte overlay node (`persistent_artrie/nodes/persistent_node.rs`) and the char overlay node (`persistent_artrie_char/nodes/persistent_node.rs`) are, after G1–G3, **token-for-token identical except for four things**:

| # | Difference | byte | char | Absorbed by |
|---|-----------|------|------|-------------|
| 1 | Key-unit type | `u8` | `u32` | `U = <KeyUnit>::Unit` |
| 2 | `MAX_PREFIX_LEN` | `12` | `6` | associated const on the key trait |
| 3 | Inline-array zero filler | `[0u8; 4]` | `[0u32; 4]` | `U::ZERO` (trait const) |
| 4 | Doc text / module headers | "bytes" | "chars" | prose only |

Everything else — `Child<V>`, `ChildStore<V>` (Inline `[U;4]`/Heap `Vec<U>`, linear-scan ≤4 / binary-search ≥5, promotion/demotion at 4↔5), the `Option<V>` immutable value, the `AtomicU8` flags + `try_set_final` two-phase, `version: AtomicU64`, `prefix: Arc<[U]>`, the manual `Debug`, `impl<V: Clone>` blocks, auto-derived `Send`/`Sync`, the commented-out atomic mutators — is **already byte-identical between the two files** (I diffed them line by line; the only token deltas are exactly rows 1–4 above).

**There is NO key-specialized SIMD, tier-threshold, or sorted-search divergence in the overlay node.** The AVX2 SIMD `find_child` lives only in the *owned* ART nodes (`node16_char.rs`), which are out of scope. The overlay `ChildStore::find_child` is plain linear/binary on both sides (`persistent_node.rs:297-322` byte and char are identical modulo `u8`/`u32`). **Feasibility verdict: fully unifiable; zero fallback components.**

The single piece of *real algorithmic work* is unrelated to the node struct: **byte's increment is still pre-G1**. `persistent_artrie/lockfree_cas.rs:540-577` calls `leaf.try_increment_value(delta, MAX)` (the in-place `AtomicU64` mutator) and `leaf.get_value()` expecting `u64`. But the byte *node file* is already at the G4 target shape (`value: Option<V>`, mutators commented out at `persistent_node.rs:717-740`). **These two files are mutually inconsistent as committed** — the node is post-G1, the increment caller is pre-G1. The plan's Phase 1 reconciles them by porting char's proven path-copy increment (`build_value_path_recursive`, `lockfree_cas.rs:813-848`) to byte *before* any unification, so byte is brought to behavioral parity with char first; then the node merges trivially.

The shared node lands in **`persistent_artrie_core/overlay/`** (new sub-module), parameterized `OverlayNode<U, V>`. The three variants alias it:
- byte: `pub type PersistentNode<V = ()> = OverlayNode<ByteUnit, V>;`
- char: `pub type PersistentCharNode<V = ()> = OverlayNode<CharUnit32, V>;`
- vocab: consumes the char alias at `<u64>` exactly as today (its alias block is unchanged).

This satisfies the layering invariant (core has zero upward refs; `SwizzledPtr` is already canonically in `persistent_artrie_core::swizzled_ptr`, verified at `core/mod.rs` + `persistent_artrie/mod.rs:131`).

---

## 1. Feasibility verdict (grounded in the code)

### 1.1 Enumerated differences and their absorption

I read both `persistent_node.rs` files completely. Here is **every** difference, with the absorption mechanism:

**(D1) Key-unit type `u8` vs `u32`.** Appears in: `ChildStore::Inline.keys: [u8;4]`/`[u32;4]`, `Heap.keys: Vec<u8>`/`Vec<u32>`, every `find_child(key: u8)`/`(key: u32)`, `child_at -> (&u8,..)`/`(&u32,..)`, `slices -> (&[u8],..)`, `with_child`/`without_child` key params, `prefix: Arc<[u8]>`/`Arc<[u32]>`, `match_prefix(&[u8])`/`(&[u32])`. → **Absorbed by a single type parameter** carrying `Copy+Ord+Eq+...`. This is exactly what `SuffixNode<U,V>` (`suffix_automaton_core/node.rs:38`), `ScdawgNode<U,V>`, and `DATCoreShared<U,V>` already do with `U: CharUnit` over `u8`/`char`.

**(D2) `MAX_PREFIX_LEN` = 12 (byte) vs 6 (char).** Used in `with_prefix`, `with_prefix_replaced` (`.min(MAX_PREFIX_LEN)`). → **Absorbed by an associated const** `<K>::MAX_PREFIX_LEN` on the key trait. (Char caps at 6 `u32`s = 24 bytes; byte at 12 `u8`s = 12 bytes — both ≤ a small fixed budget; differing values are fine as a const.)

**(D3) Inline filler `[0u8;4]` vs `[0u32;4]`** (`persistent_node.rs:247,503` byte; `:267,523` char). These slots are **dead padding** — only `keys[..count]` are ever read (documented at both files' `Inline` doc and confirmed by `find_child`/`slices`/`child_at` all slicing `[..count]`). → **Absorbed by `U::ZERO`** (a trait const) OR by `U: Default` + `U::default()`. I recommend an explicit `ZERO` const (see §1.2) to avoid coupling to `Default`.

**(D4) Doc/module headers** ("u8/ASCII variant", "byte values", "up to 12 bytes" vs "Character Node", "Unicode code points", "up to 6 chars"). → Prose; rewritten once in the shared module with both variants mentioned, exactly as `suffix_automaton_core/node.rs:1-7` does ("shared between byte and char").

**Nothing else differs.** Confirmed absent in the overlay node:
- **No SIMD.** `grep target_feature|_mm|avx|sse` over both overlay `persistent_node.rs` → zero hits (only doc-comment word "early"). SIMD is in `node16_char.rs` (owned tree), not the overlay.
- **No tier-threshold divergence.** `INLINE_CAPACITY = 4` on both; promotion at 5th child, demotion at `new_len <= 4` on both — identical code.
- **No sorted-search divergence.** Both use linear-scan-with-early-exit for Inline and `binary_search` for Heap — identical.

### 1.2 The trait-bound decision: `KeyEncoding`, `CharUnit`, or a small new trait?

This is the one genuine design choice. Three candidates, evaluated against the actual bounds the node needs and the layering invariant:

**The node needs from its key unit:** `Copy`, `Ord`/`Eq` (sorted keys, `binary_search`, `find_child` comparisons), `Send + Sync + 'static` (auto-`Send`/`Sync` derivation; `ArcSwapOption<OverlayNode>` needs `'static`), a zero filler, and `MAX_PREFIX_LEN`. It does **not** need `from_str`/`to_string`/`iter_str` (decoding happens in the *trie* layer — `insert_cas` does `term.chars().map(|c| c as u32)` at `lockfree_cas.rs:106`, never in the node).

Candidate A — **reuse `crate::CharUnit`** (the C3/C4/C5 choice, `char_unit.rs:30`). Bounds: `Copy+Clone+Default+Eq+Ord+Hash+Debug+Send+Sync+'static` plus `from_str/to_string/iter_str/to_dat_offset`. **Fatal problem:** `CharUnit` is impl'd for `u8`, `char`, `u64` — **not `u32`** (`char_unit.rs:84,110,151`). The overlay's char key is genuinely `u32` (Unicode scalar as `u32`, per `keys: [u32;4]` and every `key as u32`), *not* `char`. C3/C4/C5 chose `char` for their edge labels; the overlay deliberately uses `u32` (it stores raw code points, including in the on-disk format `[len][u32...]`). Switching the overlay to `char` would be an on-disk-format and API change (violates Constraint 3) and a much larger blast radius. `CharUnit` also drags `from_str`/`to_dat_offset` the node doesn't want. **Reject A.**

Candidate B — **reuse `KeyEncoding`** (`persistent_artrie_core/key_encoding.rs:121`). It already has `ByteKey{Unit=u8}` and `CharKey{Unit=u32}` — exactly the overlay's unit types — and lives in `persistent_artrie_core` (correct layer). Bounds on `Unit`: `Copy+Eq+Ord+Hash+Send+Sync+'static+Debug` — **precisely what the node needs**, no surplus. It is purpose-built ("the seam that lets shared modules be generic over the key-unit width") and currently barely adopted (only `key_encoding.rs` + two arena magic-const assertions — verified). **Two small gaps:** (i) no `MAX_PREFIX_LEN` const; (ii) no `Unit` zero-filler. Both are trivial additive extensions.

Candidate C — a brand-new bespoke trait. Redundant with `KeyEncoding`, which already exists for this exact purpose. **Reject C** (violates DRY against the in-repo seam).

**Decision: extend `KeyEncoding` (Candidate B).** Add to the trait:

```rust
// persistent_artrie_core/key_encoding.rs — additive extension to the existing trait
pub trait KeyEncoding: 'static + Copy + Send + Sync + Debug {
    type Unit: Copy + Eq + Ord + Hash + Send + Sync + 'static + Debug;
    // ... existing: KEY_BYTES, ARENA_MAGIC, ARENA_MAGIC_V2, FILE_MAGIC, NAME,
    //                units_from_str, unit_to_le_bytes, unit_from_le_bytes ...

    /// G4: max path-compression prefix length, in key units.
    /// 12 for byte (12 B), 6 for char (24 B). Used by the shared `OverlayNode`.
    const MAX_PREFIX_LEN: usize;

    /// G4: the zero-valued unit used as dead filler in the inline child array's
    /// unused `[count..]` slots (never read; only `keys[..count]` are live).
    const UNIT_ZERO: Self::Unit;
}
```

Then `impl KeyEncoding for ByteKey { ...; const MAX_PREFIX_LEN = 12; const UNIT_ZERO = 0u8; }` and `for CharKey { ...; const MAX_PREFIX_LEN = 6; const UNIT_ZERO = 0u32; }`.

**Why a const, not `U: Default`:** `Default::default()` for `u8`/`u32` is `0`, so `U: Default` + `Default::default()` also works and would let us drop `UNIT_ZERO`. But `KeyEncoding::Unit` does not currently bound `Default`, and adding `Default` to the associated-type bound is a wider change than adding one const; the explicit `UNIT_ZERO` keeps the filler's intent self-documenting ("this is dead padding"). Either is defensible — `UNIT_ZERO` is the lower-risk additive change. (Naming note: I use the marker type `ByteKey`/`CharKey` as the bound `K`, matching `KeyEncoding`'s existing markers; the unit is `K::Unit`.)

**Layering check:** `KeyEncoding` and the new `OverlayNode` both live in `persistent_artrie_core`. `SwizzledPtr` is canonically `persistent_artrie_core::swizzled_ptr` (verified `core/mod.rs:swizzled_ptr` + `persistent_artrie/mod.rs:131 pub use crate::persistent_artrie_core::swizzled_ptr`). So `OverlayNode` imports `crate::persistent_artrie_core::swizzled_ptr::SwizzledPtr` — **no upward reference**. Byte/char/vocab depend on core; core depends on nothing upward. Invariant preserved.

---

## 2. The unified types (precise Rust signatures + module locations)

**Location:** new module `src/persistent_artrie_core/overlay/` with:
- `overlay/mod.rs` — re-exports `OverlayNode`, `Child`, `AtomicNodePtr` (and `flags`).
- `overlay/node.rs` — `OverlayNode<K, V>`, `Child<K, V>`, `ChildStore<K, V>`, `flags`.
- `overlay/atomic_ptr.rs` — `AtomicNodePtr<K, V>`.

Register in `persistent_artrie_core/mod.rs`: `pub mod overlay;`.

### 2.1 `Child<K, V>`

```rust
// persistent_artrie_core/overlay/node.rs
use crate::persistent_artrie_core::key_encoding::KeyEncoding;
use crate::persistent_artrie_core::swizzled_ptr::SwizzledPtr;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

/// A child slot in an [`OverlayNode`]. `InMem` owns the child by `Arc` (reclaimed
/// on drop via refcount — the leak-fix); `OnDisk` is an ownership-free serialized
/// block location. Zero `unsafe`. Generic over the key encoding `K` (so the in-mem
/// arm names the correctly-parameterized node) and value `V`.
pub enum Child<K: KeyEncoding, V = ()> {
    InMem(Arc<OverlayNode<K, V>>),
    OnDisk(SwizzledPtr),
}

#[derive(Clone)]  // derives once child slots are `Child` (both arms Clone)
// ^ NOTE: cannot `#[derive(Clone)]` directly because the derive would demand
//   `K: Clone + V: Clone` on the *type params*; instead hand-write to bound only V:
impl<K: KeyEncoding, V: Clone> Clone for Child<K, V> { /* match → InMem(Arc::clone)/OnDisk(p.clone()) */ }

// Manual Debug so neither K's Unit nor V need Debug-recursion through the node.
impl<K: KeyEncoding, V> std::fmt::Debug for Child<K, V> { /* "Child::InMem(..)" / OnDisk(p) — verbatim from byte:139-146 */ }

impl<K: KeyEncoding, V> Child<K, V> {
    #[inline] fn empty() -> Self { Child::OnDisk(SwizzledPtr::null()) }
    #[inline] pub fn is_null(&self) -> bool { matches!(self, Child::OnDisk(p) if p.is_null()) }
    #[inline] pub fn is_on_disk(&self) -> bool { matches!(self, Child::OnDisk(_)) }
    #[inline] pub fn as_in_mem(&self) -> Option<&Arc<OverlayNode<K, V>>> { /* verbatim */ }
    #[inline] pub fn as_on_disk(&self) -> Option<&SwizzledPtr> { /* verbatim */ }
}
```

**Note on `Clone`:** today both files write `#[derive(Clone)]` on `Child<V>` (byte:129, char:121) because the only type param is `V`. With two params `K, V`, a derive would over-constrain (`K: Clone`). Marker types *are* `Clone` (`ByteKey`/`CharKey` derive `Clone, Copy`), so `#[derive(Clone)]` would actually still compile — but to keep the bound minimal (`V: Clone` only, matching the existing method blocks) and avoid a spurious `K: Clone` bound leaking into call-sites, **hand-write `impl<K: KeyEncoding, V: Clone> Clone`**. Same pattern for `ChildStore` and `OverlayNode`.

### 2.2 `ChildStore<K, V>`

```rust
const INLINE_CAPACITY: usize = 4;

enum ChildStore<K: KeyEncoding, V = ()> {
    Inline { count: u8, keys: [K::Unit; INLINE_CAPACITY], children: [Child<K, V>; INLINE_CAPACITY] },
    Heap   { keys: Vec<K::Unit>, children: Vec<Child<K, V>> },
}

impl<K: KeyEncoding, V: Clone> Clone for ChildStore<K, V> { /* hand-written, bounds V:Clone */ }
impl<K: KeyEncoding, V> std::fmt::Debug for ChildStore<K, V> { /* verbatim from byte:226-239 */ }

impl<K: KeyEncoding, V: Clone> ChildStore<K, V> {
    #[inline] fn new() -> Self {
        ChildStore::Inline {
            count: 0,
            keys: [K::UNIT_ZERO; INLINE_CAPACITY],          // <- the only key-typed literal change
            children: [Child::empty(), Child::empty(), Child::empty(), Child::empty()],
        }
    }
    #[inline] fn len(&self) -> usize { /* verbatim */ }
    #[inline] fn is_empty(&self) -> bool { self.len() == 0 }
    #[inline] fn find_child(&self, key: K::Unit) -> Option<&Child<K, V>> { /* verbatim linear/binary */ }
    #[inline] fn has_child(&self, key: K::Unit) -> bool { self.find_child(key).is_some() }
    #[inline] fn child_at(&self, i: usize) -> Option<(&K::Unit, &Child<K, V>)> { /* verbatim */ }
    #[inline] fn slices(&self) -> (&[K::Unit], &[Child<K, V>]) { /* verbatim */ }
    fn with_child(&self, key: K::Unit, child: Child<K, V>) -> Self { /* verbatim; only `[0u8/0u32]`→`[K::UNIT_ZERO]` if any local arrays */ }
    fn without_child(&self, key: K::Unit) -> Option<Self> {
        // verbatim EXCEPT two spots that clear the demoted/last slot:
        //   byte:487  new_keys[n-1] = 0;   char:507  new_keys[n-1] = 0;
        //   byte:503  let mut new_keys = [0u8; ..];  char:523  [0u32; ..];
        // → `K::UNIT_ZERO`
    }
    fn memory_usage(&self) -> usize { /* verbatim; size_of::<u8>()/<u32>() → size_of::<K::Unit>() */ }
}
```

The `binary_search(&key)` and `keys[i] > key` comparisons all type-check via `K::Unit: Ord`. The `new_keys[n-1] = 0` and `[0u8/0u32; 4]` are the **only** literals that become `K::UNIT_ZERO` (4 sites total per file).

### 2.3 `OverlayNode<K, V>`

```rust
pub struct OverlayNode<K: KeyEncoding, V = ()> {
    version: AtomicU64,
    store: ChildStore<K, V>,
    flags: AtomicU8,
    /// Immutable per the G1/G4 model (was AtomicU64). `()` for membership.
    value: Option<V>,
    prefix: Arc<[K::Unit]>,
    prefix_len: u8,
}

pub mod flags {  // identical on both today; lift once
    pub const IS_FINAL: u8 = 0b0000_0001;
    pub const IS_DIRTY: u8 = 0b0000_0010;
    pub const IS_LEAF:  u8 = 0b0000_0100;
    pub const HAS_VALUE:u8 = 0b0000_1000;
}

impl<K: KeyEncoding, V: Clone> std::fmt::Debug for OverlayNode<K, V> { /* verbatim from byte:615-624 */ }

impl<K: KeyEncoding, V: Clone> OverlayNode<K, V> {
    pub fn new() -> Self { /* verbatim */ }
    pub fn with_prefix(prefix: &[K::Unit]) -> Self {
        let prefix_len = prefix.len().min(K::MAX_PREFIX_LEN) as u8;   // <- const, was MAX_PREFIX_LEN
        let prefix_data: Arc<[K::Unit]> = prefix[..prefix_len as usize].into();
        /* ... verbatim ... */
    }
    #[inline] pub fn version(&self) -> u64 { /* verbatim */ }
    #[inline] pub fn num_children(&self) -> usize { self.store.len() }
    #[inline] pub fn is_empty(&self) -> bool { self.store.is_empty() }
    #[inline] pub fn prefix(&self) -> &[K::Unit] { &self.prefix[..self.prefix_len as usize] }
    #[inline] pub fn prefix_len(&self) -> usize { self.prefix_len as usize }
    #[inline] pub fn is_final(&self) -> bool { /* verbatim */ }
    #[inline] pub fn has_value(&self) -> bool { self.value.is_some() }
    #[inline] pub fn get_value(&self) -> Option<V> { self.value.clone() }
    #[inline] pub fn try_set_final(&self) -> bool { /* verbatim fetch_or — the proven two-phase arbiter */ }
    #[inline] pub fn find_child(&self, key: K::Unit) -> Option<&Child<K, V>> { self.store.find_child(key) }
    #[inline] pub fn has_child(&self, key: K::Unit) -> bool { self.store.has_child(key) }
    #[inline] pub fn child_at(&self, i: usize) -> Option<(&K::Unit, &Child<K, V>)> { self.store.child_at(i) }
    pub fn iter_children(&self) -> impl Iterator<Item = (&K::Unit, &Child<K, V>)> { /* verbatim zip */ }
    pub fn with_child(&self, key: K::Unit, child: Child<K, V>) -> Self { /* verbatim */ }
    pub fn without_child(&self, key: K::Unit) -> Option<Self> { /* verbatim */ }
    pub fn with_prefix_replaced(&self, prefix: &[K::Unit]) -> Self { /* verbatim; MAX_PREFIX_LEN → K::MAX_PREFIX_LEN */ }
    pub fn as_final(&self) -> Self { /* verbatim */ }
    pub fn with_value(&self, value: V) -> Self { /* verbatim */ }
    pub fn match_prefix(&self, key: &[K::Unit]) -> usize { /* verbatim */ }
    #[inline] pub fn prefix_matches(&self, key: &[K::Unit]) -> bool { /* verbatim */ }
    pub fn memory_usage(&self) -> usize { /* verbatim; size_of::<u8/u32>() → size_of::<K::Unit>() */ }
}

impl<K: KeyEncoding, V: Clone> Default for OverlayNode<K, V> { fn default() -> Self { Self::new() } }
impl<K: KeyEncoding, V: Clone> Clone   for OverlayNode<K, V> { /* verbatim field-by-field */ }
// Send/Sync auto-derive: every field is Send+Sync when K::Unit: Send+Sync (it is)
// and V: Send+Sync (guaranteed by the DictionaryValue bound callers supply). NO unsafe impl.
```

### 2.4 `AtomicNodePtr<K, V>`

```rust
// persistent_artrie_core/overlay/atomic_ptr.rs
use arc_swap::ArcSwapOption;
pub struct AtomicNodePtr<K: KeyEncoding, V = ()> {
    ptr: ArcSwapOption<OverlayNode<K, V>>,
}
impl<K: KeyEncoding, V> std::fmt::Debug for AtomicNodePtr<K, V> { /* verbatim char:87-93 */ }
impl<K: KeyEncoding, V: Clone> AtomicNodePtr<K, V> {
    pub fn new(node: Arc<OverlayNode<K, V>>) -> Self { /* verbatim */ }
    pub fn null() -> Self { /* verbatim */ }
    #[inline] pub fn is_null(&self) -> bool { /* verbatim */ }
    pub fn load(&self) -> Option<Arc<OverlayNode<K, V>>> { self.ptr.load_full() }
    #[inline] pub fn load_unchecked(&self) -> Arc<OverlayNode<K, V>> { /* verbatim */ }
    pub fn store(&self, node: Arc<OverlayNode<K, V>>) { /* verbatim */ }
    pub fn take(&self) -> Option<Arc<OverlayNode<K, V>>> { /* verbatim */ }
    pub fn compare_exchange(&self, expected: &Arc<OverlayNode<K,V>>, new: Arc<OverlayNode<K,V>>)
        -> Result<Arc<OverlayNode<K,V>>, Arc<OverlayNode<K,V>>> {
        // verbatim char:200-206 — EXCEPT the None arm:
        //   char today: None => Err(Arc::new(PersistentCharNode::new())),
        // generic:     None => Err(Arc::new(OverlayNode::new())),   // needs V: Clone (already bounded)
    }
    pub fn compare_exchange_weak(&self, ..) { self.compare_exchange(..) }
    pub fn try_init(&self, new: Arc<OverlayNode<K,V>>) -> Result<(), Arc<OverlayNode<K,V>>> { /* verbatim */ }
    #[inline] pub fn as_raw(&self) -> u64 { /* verbatim */ }
}
// Clone/Default: char today impls these for the bare `AtomicNodePtr` (V=()) only
// (atomic_ptr.rs:255,264). Generic version: impl for <K, V: Clone> uniformly
// (the byte AtomicNodePtr Clone/Default are also V=()). Widening to <K,V:Clone>
// is strictly more general and removes the char inconsistency — confirm no
// call-site relied on the non-generic-only impl (none do; both are `Self::new`/`null`).
impl<K: KeyEncoding, V: Clone> Clone   for AtomicNodePtr<K, V> { /* load→new/null */ }
impl<K: KeyEncoding, V: Clone> Default for AtomicNodePtr<K, V> { fn default() -> Self { Self::null() } }
```

### 2.5 `LockfreeInsertResult` — does it need `K`?

It needs `V` (already `LockfreeInsertResult<V>` in char `dict_impl_char.rs:70`; byte's is non-generic `LockfreeInsertResult` at `lockfree_cas.rs:32`). It holds `Inserted(Arc<OverlayNode<K, V>>)` — so it needs **both** `K` and `V`. But it is a `pub(super)`/private per-variant enum (char `pub(super) enum`, byte file-private `enum`), used only inside each variant's `lockfree_cas.rs`. **Recommendation: keep it per-variant** (not in `core`), each defined over the variant's concrete `K`:
- byte: `enum LockfreeInsertResult<V = ()> { Inserted(Arc<OverlayNode<ByteKey, V>>), AlreadyExists, Conflict }`
- char: `pub(super) enum LockfreeInsertResult<V = ()> { Inserted(Arc<OverlayNode<CharKey, V>>), AlreadyExists, Conflict }`

It is a 3-line enum; centralizing it would force a `<K, V>` and buy nothing (the algorithms that produce/consume it are per-variant — see §3). This matches the C-dedup experience where small per-variant result types stayed local.

---

## 3. Can the lock-free ALGORITHMS also be DRY-shared?

**Recommendation: keep the algorithm methods per-variant (thin), but share via the unified node's API — do NOT extract a generic algorithm trait in G4.** Rationale grounded in the code:

The membership algorithms (`insert_cas`, `try_insert_lockfree_path`, `build_path_recursive`, `create_lockfree_path`, `insert_lockfree_recursive`, `contains_lockfree`, `find_in_lockfree_trie`, `find_leaf_lockfree`, `find_leaf_recursive`, `merge_lockfree_to_persistent`) are **already nearly identical** between byte (`lockfree_cas.rs:74-507`) and char (`lockfree_cas.rs:88-539`) — the differences are exactly:
- key decoding: byte takes `&[u8]` directly; char does `term.chars().map(|c| c as u32).collect::<Vec<u32>>()` first. This is `K::units_from_str` territory but the *call signatures* differ (`&[u8]` vs `&str`), and these are `pub` API methods (`insert_cas(&[u8])` vs `insert_cas(&str)`).
- they are `impl` methods on **different owning structs** (`PersistentARTrie<V,S>` vs `PersistentARTrieChar<V,S>`) that carry different surrounding state (`epoch_manager`, `cas_retries`, `lockfree_cache`, WAL, `committed_watermark`).
- the counter/merge halves differ structurally: byte's increment is in-place (pre-G1, `try_increment_value`), char's is path-copy (`build_value_path_recursive`); byte's merge serializes `V` through bincode to reach `i64` (`current_i64_for_lockfree_merge` at byte:411-433), char's merge has `V=u64` concretely (char:937-950). These are genuinely different value-domain plumbings.

**Two options:**

**(3a) Minimal-share (recommended for G4):** Leave each variant's `lockfree_cas.rs` algorithm methods in place. They become DRY-*at-the-node-level* automatically: once both call `OverlayNode<K,V>` with identical `find_child`/`with_child`/`try_set_final`/`as_final`/`with_value` signatures, the *bodies* of `build_path_recursive` etc. become textually identical modulo the `&str`→`Vec<u32>` decode and the `super::nodes` path. The duplication that remains is ~150 LOC of recursion per variant — acceptable, and exactly the line the C3/C4/C5 dedup drew (it unified the *node*, not every traversal method; the DAT dedup note at `dat_core/mod.rs:15` says "each algorithm method becomes `DATCoreShared::method<U>`" — i.e. they *did* share DAT algorithms, but those are free functions over a shared storage struct, whereas here the algorithms are entangled with per-trie WAL/epoch/cache state).

**(3b) Maximal-share (defer to a follow-up, NOT G4):** Extract a generic free-function helper layer keyed by a small trait that abstracts "the overlay context":

```rust
// SKETCH ONLY — proposed for a later DRY pass, not G4.
trait OverlayCtx {
    type K: KeyEncoding;
    type V: Clone;
    fn root(&self) -> &AtomicNodePtr<Self::K, Self::V>;
    fn bump_cas_retry(&self);
    fn enter_read(&self) -> EpochGuard;
}
// Then shared free fns:
fn build_path_recursive<C: OverlayCtx>(ctx: &C, node: &Arc<OverlayNode<C::K, C::V>>, units: &[<C::K as KeyEncoding>::Unit], depth: usize) -> Result<(Arc<..>, Arc<..>), ()> { /* the one true copy */ }
fn find_leaf_recursive<C: OverlayCtx>(..) -> Option<Arc<OverlayNode<C::K, C::V>>> { /* one copy */ }
```

This would collapse the membership recursion to a single copy. **But** it (i) is a larger refactor touching three `lockfree_cas.rs` files' control flow, (ii) risks the proven loom/proptest correspondence (the proofs model the *concrete* `insert_one_char`/`insert_one_key` shapes), and (iii) the counter/durable halves still can't share (different value domains, WAL records, watermarks). Per Constraint 1 (reuse proofs, don't re-derive) and Constraint 5 (reversible, green at every step), **G4 should land the node unification (3a) and explicitly note 3b as a separable, lower-priority follow-up.** Default to DRY at the node (the owner's decision); pursue DRY at the algorithm layer only after G4 is green, as its own reversible change.

**The decode seam, if you want one cheap win in G4:** the only per-variant divergence in the membership recursion is `&str`→units. `KeyEncoding::units_from_str` already exists (`key_encoding.rs:147`) and returns `SmallVec<[Unit;32]>`. You may optionally route both variants' public `insert_cas` through `K::units_from_str(term)` to make the *internal* recursion identical — but note byte's public API is `insert_cas(&[u8])` (no decode needed) while char's is `insert_cas(&str)`; the byte path would bypass `units_from_str`. Keep the public signatures as-is.

---

## 4. Variant migration (char alias, vocab, byte instantiation, MVCC)

### 4.1 CHAR migration — pure re-export, zero behavior change

`persistent_artrie_char/nodes/persistent_node.rs` shrinks to:

```rust
//! Char overlay node: a `<CharKey>` instantiation of the shared `OverlayNode`.
pub use crate::persistent_artrie_core::overlay::{flags, AtomicNodePtr as _, Child as ChildGeneric, OverlayNode};
use crate::persistent_artrie_core::key_encoding::CharKey;

/// The char overlay node (Unicode code-point keys). Now an alias of the shared generic.
pub type PersistentCharNode<V = ()> = OverlayNode<CharKey, V>;
/// The char child slot.
pub type Child<V = ()> = crate::persistent_artrie_core::overlay::Child<CharKey, V>;
pub const MAX_PREFIX_LEN: usize = <CharKey as crate::persistent_artrie_core::key_encoding::KeyEncoding>::MAX_PREFIX_LEN; // 6, for any external referent

#[cfg(test)] mod tests { /* the existing char tests move here unchanged: they already
   alias `type PersistentCharNode = super::PersistentCharNode<()>` etc. — they exercise
   the alias and PASS verbatim, proving behavioral identity. */ }
```

`persistent_artrie_char/nodes/atomic_ptr.rs` shrinks to `pub type AtomicNodePtr<V = ()> = crate::persistent_artrie_core::overlay::AtomicNodePtr<CharKey, V>;` (plus moving its tests, which alias `<()>` and pass verbatim).

`persistent_artrie_char/nodes/mod.rs` keeps its `pub use persistent_node::PersistentCharNode;` and `pub use atomic_ptr::AtomicNodePtr;` — call-sites (`lockfree_cas.rs`, vocab, `mvcc.rs`, `persist.rs::overlay_to_inner`) **do not change** because the names resolve identically. `overlay_to_inner<V>(node: &PersistentCharNode<V>) -> CharTrieNodeInner<V>` (`persist.rs:966`) reads only `is_final()/get_value()/prefix()/iter_children()/as_in_mem()` — all preserved on the alias — so the serializer and on-disk format are untouched (Constraint 3 ✓).

`persistent_artrie_char/mvcc.rs` (`impl<V: Clone+Send+Sync+'static> TrieRoot for PersistentCharNode<V>`, `mvcc.rs:13`): because `PersistentCharNode<V>` is now `OverlayNode<CharKey, V>`, this `impl` is on a type alias = an `impl` on `OverlayNode<CharKey, V>`. **Coherence:** `TrieRoot` is defined in `persistent_artrie_core::mvcc`; `OverlayNode` is in `persistent_artrie_core::overlay`. The `impl TrieRoot for OverlayNode<CharKey, V>` can live **either** in `persistent_artrie_char/mvcc.rs` (as now — the alias makes it `impl ... for OverlayNode<CharKey,V>`, allowed because the variant crate is downstream of both) **or** be unified (see 4.4). Keep it in char `mvcc.rs` for the minimal diff; it compiles unchanged.

### 4.2 VOCAB migration — the alias block is already correct; verify and leave

Both vocab consumers already use the generic char node through aliases:
- `persistent_vocab_artrie/lockfree.rs:68-79`: `type PersistentCharNode = PersistentCharNodeGeneric<u64>;` etc.
- `persistent_vocab_artrie/lockfree_cas.rs:19-28`: same.
- `persistent_vocab_artrie/dict_impl.rs:193`: `lockfree_root: Option<AtomicNodePtr<u64>>`.

Because `PersistentCharNodeGeneric` = `persistent_artrie_char::nodes::PersistentCharNode` = (now) `OverlayNode<CharKey, _>`, the vocab aliases transparently become `OverlayNode<CharKey, u64>`. **Nothing in vocab changes.** Specifically:
- The **reverse index** (`index_term_storage: RwLock<Vec<Option<String>>>` in `lockfree.rs:149`, and `reverse_index.rs`/`reverse_cache.rs`) lives entirely *outside* the node — untouched (Constraint: "vocab's reverse-index lives OUTSIDE the node and is untouched" ✓).
- The **`unreachable!` on-disk branch** (`lockfree.rs:272-278` in `get_index`) keys on `child.as_in_mem()` returning `None` — that method is preserved verbatim on the alias, so the `unreachable!` semantics are identical.
- Vocab uses `as_final().with_value(index)`, `find_child`, `is_null`, `get_value` — all preserved.
- `LockFreeVocab`'s `unsafe impl Send/Sync` (`lockfree.rs:597-598`) is on `LockFreeVocab` itself (a wrapper struct), **not** on the node — it is unrelated to G4 and stays as-is (it's already in the unsafe inventory; G4 adds/removes nothing there).

**Verification action:** after the char alias lands, `cargo build` the vocab crate; expect zero edits. (If the `Child`/`AtomicNodePtr`/`PersistentCharNode` generic aliases in vocab were importing `...::Child as ChildGeneric` etc., confirm those import paths still resolve — they import from `persistent_artrie_char::nodes::{...}`, which re-exports the aliases. ✓)

### 4.3 BYTE migration — two sub-steps (the only real work)

**Byte sub-step A (precursor, brings byte to char's proven model):** Port the path-copy increment. This is required because the committed byte `lockfree_cas.rs:540-577` still uses in-place `try_increment_value` (pre-G1) while the byte *node* already removed it. Replace byte's counter block to mirror char `lockfree_cas.rs:565-848`:
- Move byte counter methods (`get_lockfree`, `try_increment_cas`, `increment_cas`, `merge_lockfree_values_to_persistent`, the `collect_*`/`prepare_*`/`*_to_i64` helpers) into an `impl<S: BlockStorage> PersistentARTrie<u64, S>` block (mirroring char's `impl<S> PersistentARTrieChar<u64, S>` at char:565). **Wait** — byte's counter domain: byte's MVCC `Value=u64` (`persistent_artrie/mvcc.rs:23`) and byte merge widens `V` via bincode to `i64` (`current_i64_for_lockfree_merge`, byte:411). Byte's generic membership block is `impl<V: DictionaryValue, S>` and its counter methods currently live in that same block calling `try_increment_value`. Restructure to: generic membership in `<V: DictionaryValue, S>`, counter in `<u64, S>` with `build_value_path_recursive` (verbatim port of char:813-848 with `u32`→`u8`, `chars`→`term`). This makes byte's increment a path-copy CAS with the root-CAS linearization point — identical proof obligation to char's (already discharged by the loom test).
- Delete byte's `try_increment_value`/`get_value` in-place expectations.

**Byte sub-step B (the unification):** Replace byte `persistent_node.rs` and `atomic_ptr.rs` with aliases, exactly as char:

```rust
// persistent_artrie/nodes/persistent_node.rs
pub use crate::persistent_artrie_core::overlay::{flags, OverlayNode};
use crate::persistent_artrie_core::key_encoding::ByteKey;
pub type PersistentNode<V = ()> = OverlayNode<ByteKey, V>;
pub type Child<V = ()> = crate::persistent_artrie_core::overlay::Child<ByteKey, V>;
pub const MAX_PREFIX_LEN: usize = <ByteKey as ...::KeyEncoding>::MAX_PREFIX_LEN; // 12
#[cfg(test)] mod tests { /* byte node tests move here unchanged — they alias <()>/`<u64>` (persistent_node.rs:942-943) and pass verbatim */ }
```
```rust
// persistent_artrie/nodes/atomic_ptr.rs
pub type AtomicNodePtr<V = ()> = crate::persistent_artrie_core::overlay::AtomicNodePtr<ByteKey, V>;
```

Byte `dict_impl.rs` fields: today `lockfree_root: Option<super::nodes::AtomicNodePtr>` (non-generic, `dict_impl.rs:321`) and `lockfree_cache: Option<DashMap<Vec<u8>, bool>>`. Since the byte overlay is exercised at `V=i64`/`V=()` (counter domain `i64` per the constraints, membership `()`), set `lockfree_root: Option<AtomicNodePtr<u64>>` to match char's counter value domain used by `try_increment_cas` — **decision point:** the byte counter accumulates in `u64` (`LOCKFREE_COUNTER_MAX = i64::MAX as u64`, byte:26; node value read as `Option<u64>`). So byte's overlay value type is `u64` (the in-overlay counter), persisted as `i64`. Set `lockfree_root: Option<AtomicNodePtr<u64>>` and `LockfreeInsertResult<u64>` for the counter path, `<()>` for membership — **but** byte currently has ONE non-generic `lockfree_root` shared by both membership (`insert_cas`) and counter (`try_increment_cas`). char solved this by making `lockfree_root: AtomicNodePtr<V>` on the generic `PersistentARTrieChar<V,S>` and putting counter methods in the `<u64>` impl. **Mirror char exactly:** make byte `lockfree_root: Option<AtomicNodePtr<V>>` on `PersistentARTrie<V,S>`, membership in `<V: DictionaryValue, S>`, counter in `<u64, S>`. This is the cleanest parity and is what G4 ("genericize byte over V mirroring char") intends.

This means byte sub-step A and B are really one coherent change: **genericize byte's `lockfree_root` over `V` and port the increment** — after which the node alias drops in.

### 4.4 Unify the `TrieRoot` impls?

Currently two near-identical impls: byte (`persistent_artrie/mvcc.rs:19`, `Key=u8, Value=u64`) and char (`persistent_artrie_char/mvcc.rs:13`, `Key=u32, Value=V`). With `OverlayNode<K,V>` you *can* write one blanket:

```rust
// Could live in persistent_artrie_core/overlay/mod.rs (downstream of core::mvcc — same crate, OK):
impl<K: KeyEncoding, V: Clone + Send + Sync + 'static> TrieRoot for OverlayNode<K, V> {
    type Key = K::Unit;
    type Value = V;
    fn is_final(&self) -> bool { OverlayNode::is_final(self) }
    fn find_child(&self, key: K::Unit) -> Option<Arc<Self>> {
        OverlayNode::find_child(self, key).and_then(|c| c.as_in_mem().map(Arc::clone))
    }
    fn get_value(&self) -> Option<V> { OverlayNode::get_value(self) }
}
```

**Recommendation: yes, unify, but mind the byte `Value` change.** The blanket gives byte `Value = V` (was hardcoded `u64`). Byte's `ReadTransaction::get` returns `T::Value` (`core/mvcc.rs:220`); today byte callers expect `u64`. Once byte's overlay is generic, `OverlayNode<ByteKey, u64>` yields `Value=u64` (same) and `<ByteKey,()>` yields `Value=()`. The blanket is strictly more correct. **Place the blanket `impl TrieRoot for OverlayNode<K,V>` in `core::overlay`**, then byte `mvcc.rs` and char `mvcc.rs` shrink to just the `pub use` re-exports they already carry (char `mvcc.rs` becomes empty of its own impl; byte `mvcc.rs:13-15` already re-exports the generics). This removes the byte/char `TrieRoot` duplication too — a bonus DRY win. **Caution:** verify no orphan-rule issue — `TrieRoot` (core) + `OverlayNode` (core) → blanket in core is fine; the `K::Unit: Copy` bound satisfies `type Key: Copy`.

---

## 5. Phased, reversible migration sequence

Each phase ships green (`cargo nextest run --features persistent-artrie` ≥ 2474) and is independently revertible. **Every build/test wrapped** `systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp <cmd>`.

> Ordering principle: introduce the shared types *additively* first (no variant touches them), then migrate char (pure alias, lowest risk, proves the generic), then bring byte to char's proven model, then migrate byte, then optionally unify `TrieRoot`. Vocab is validated as a no-op after char.

### Phase 0 — Extend `KeyEncoding` (additive, no behavior)
- **Files:** `persistent_artrie_core/key_encoding.rs` (add `MAX_PREFIX_LEN`, `UNIT_ZERO` to trait + both impls).
- **Green gate:** full suite (the two new consts are unused yet → no behavior change). Unsafe inventory unchanged.
- **Rollback:** delete the two consts.

### Phase 1 — Land `OverlayNode<K,V>` / `Child` / `ChildStore` / `AtomicNodePtr` in `core::overlay` (additive, unconsumed)
- **Files:** new `persistent_artrie_core/overlay/{mod.rs,node.rs,atomic_ptr.rs}`; register `pub mod overlay;` in `core/mod.rs`. Port the char node verbatim with the 4 key-typed substitutions; **move the char node's unit tests into `overlay/node.rs`** parameterized for both `ByteKey` and `CharKey` (the existing char tests + the existing byte tests become `<CharKey>`/`<ByteKey>` smoke tests — this is where the byte-instantiation coverage comes from "for free", see §6).
- **Green gate:** suite passes (new module compiled + its tests run; nothing else references it). `verify-formal-correspondence.sh` exit 0. Unsafe inventory: **must stay set-equal** — the new module has zero `unsafe` (Send/Sync auto-derive), so no inventory rows added.
- **Rollback:** delete `overlay/`, unregister.

### Phase 2 — Migrate CHAR to alias (pure re-export)
- **Files:** `persistent_artrie_char/nodes/persistent_node.rs` → alias + moved tests; `…/nodes/atomic_ptr.rs` → alias; `…/nodes/mod.rs` re-exports unchanged. char `lockfree_cas.rs`, `dict_impl_char.rs` (`LockfreeInsertResult<V>` now holds `OverlayNode<CharKey,V>`), `persist.rs`, `mvcc.rs` — **no edits** (names resolve identically).
- **Green gate:** **the entire char + vocab + overlay loom/proptest suite is the proof of behavioral identity.** Specifically these must stay green (they exercise the now-aliased node): `persistent_lockfree_overlay_loom`, `persistent_lockfree_overlay_proptest` (BTreeSet oracle), `persistent_lockfree_durable_loom`, `persistent_artrie_loom_correspondence`, `persistent_lockfree_merge_correspondence`, `persistent_char_node_layout_correspondence`, `persistent_char_ebr_correspondence`, `persistent_char_eviction_*`, plus the in-crate `reclaim_tests`/`eviction_primitive_tests`/`durable_write_tests` in char `lockfree_cas.rs`. `verify-formal-correspondence.sh` exit 0; unsafe inventory set-equal.
- **Rollback:** restore the two char node files from git (alias → inline). Self-contained.

### Phase 3 — Validate VOCAB is a no-op
- **Files:** none expected. Build `persistent_vocab_artrie`; confirm `lockfree.rs`/`lockfree_cas.rs`/`dict_impl.rs` aliases resolve to `OverlayNode<CharKey,u64>`.
- **Green gate:** `persistent_vocab_*` correspondence + `concurrent` tests green. If any import path needs a tweak (none anticipated), it is a one-line `use`.
- **Rollback:** trivial (revert any stray `use`).

### Phase 4 — Bring BYTE to char's proven model (genericize `lockfree_root` over `V`, port path-copy increment)
- **Files:** `persistent_artrie/lockfree_cas.rs` (split membership `<V:DictionaryValue,S>` vs counter `<u64,S>`; add `build_value_path_recursive` ported from char; delete in-place `try_increment_value` usage), `persistent_artrie/dict_impl.rs` (`lockfree_root: Option<AtomicNodePtr<V>>`, `lockfree_cache` keyed appropriately; `LockfreeInsertResult<V>`), byte `mvcc.rs` (`Value` may stay `u64` until Phase 6).
- **This is the behavior-changing phase for byte** (increment becomes path-copy CAS). New proof obligation = **none new** beyond what char already discharged: it is the *same* generic code path char proved via `char_create_vs_increment_race_…` loom. Add a byte mirror of that loom test (see §6).
- **Green gate:** byte concurrent/loom/proptest green: `persistent_artrie_loom_correspondence`, `persistent_artrie_concurrent`, `persistent_artrie_proptest`, `persistent_lockfree_merge_correspondence`, `persistent_transaction_increment_correspondence`, byte `reclaim_tests`. Plus the new byte increment-race loom test. `verify-formal-correspondence.sh` exit 0; unsafe inventory set-equal (byte node still inline here, so its commented-out mutators remain — no inventory delta).
- **Rollback:** revert byte `lockfree_cas.rs`/`dict_impl.rs` to the in-place increment (git). This is the riskiest phase; keeping it separate from the node alias makes rollback clean.

### Phase 5 — Migrate BYTE node + atomic_ptr to alias
- **Files:** `persistent_artrie/nodes/persistent_node.rs` → alias `OverlayNode<ByteKey,V>` + moved tests; `…/nodes/atomic_ptr.rs` → alias; `…/nodes/mod.rs` unchanged. byte `lockfree_cas.rs`/`mvcc.rs` — no edits (names resolve).
- **Green gate:** full byte suite + the whole persistent-artrie suite green. `verify-formal-correspondence.sh` exit 0. **Unsafe inventory:** byte node's inline file (with its commented mutators and the `// unsafe impl … deleted` provenance) is *removed*; confirm the inventory has **no rows** for byte `persistent_node.rs`/`atomic_ptr.rs` to drop (they have zero live `unsafe`), so set-equality holds. If the ledger had stale rows for those paths, update the ledger in the same commit and re-run the gate.
- **Rollback:** restore the two byte node files from git.

### Phase 6 (optional, DRY bonus) — Unify `TrieRoot` blanket
- **Files:** add blanket `impl TrieRoot for OverlayNode<K,V>` in `core::overlay/mod.rs`; delete the per-variant impls in byte `mvcc.rs` (lines 19-39) and char `mvcc.rs` (lines 13-31), leaving their re-exports. Update byte `mvcc.rs` `Value` expectation if any caller pinned `u64`.
- **Green gate:** `persistent_read_snapshot_correspondence`, `persistent_shared_concurrency_correspondence`, MVCC tests green. Suite green.
- **Rollback:** restore per-variant impls; remove blanket.

> Phases 0–5 are the core G4. Phase 6 is a clean follow-on. The §3b algorithm-sharing refactor is explicitly **not** part of G4.

---

## 6. Verification plan

### 6.1 Tests that transfer FOR FREE (cover the shared node, no new derivation)

Because char migrates by alias (Phase 2), **every char/overlay/vocab test below already exercises `OverlayNode<CharKey,…>`** and is the proof that unification preserves behavior:

- **Membership two-phase + single-arbiter (loom):** `tests/persistent_lockfree_overlay_loom.rs` — `concurrent_disjoint_inserts_never_lose_an_update`, `concurrent_prefix_finalize_has_exactly_one_winner` (the prefix single-arbiter fix), `reader_holding_owned_arc_snapshot_never_faults` (the owned-`Arc` reclamation). Its `ModelNode`/`ModelRootSlot` mirror `try_set_final` (`fetch_or`) + `compare_and_swap`+`ptr_eq`, which the alias preserves byte-for-byte.
- **BTreeSet oracle (proptest):** `tests/persistent_lockfree_overlay_proptest.rs` — `overlay_insert_contains_match_btreeset_oracle` (random insert/contains incl. the "is new?" bool, forcing inline↔heap transitions), `concurrent_contended_inserts_finalize_each_term_exactly_once`.
- **Order-A durability + watermark (loom + TLA):** `tests/persistent_lockfree_durable_loom.rs` (`NoLostWriteUnderLockFreeCommit` positive tests + the `#[should_panic]` appended-frontier negative control) ↔ `formal-verification/tla+/LockFreeDurableCheckpoint.tla` + `_Unsafe.cfg`. Also `PublicDurabilityPolicy.tla`, `ConcurrentCheckpointPublication.tla`, `LockFreeCounterMergeAtomicity.tla`, `LockFreeIndexedOverlay*.tla`, `LockFreeARTrieLinearizability.tla`.
- **Path-copy increment race (loom):** the char `char_create_vs_increment_race_has_one_leaf_and_total_value` (referenced in `lockfree_cas.rs:724`).
- **Reclaim / eviction (in-crate, `Arc::strong_count` witnesses):** char `lockfree_cas.rs` `reclaim_tests` + `eviction_primitive_tests` + `durable_write_tests`; `tests/persistent_char_eviction_*`, `persistent_char_ebr_correspondence`.
- **Node layout / snapshot / merge:** `persistent_char_node_layout_correspondence`, `persistent_read_snapshot_correspondence`, `persistent_lockfree_merge_correspondence`, `persistent_vocab_checkpoint_correspondence`, `persistent_vocab_wal_atomicity_correspondence`.
- **The node's own unit tests** (24 char + 22 byte tests in the two `persistent_node.rs` files) move into `core::overlay/node.rs`, now parameterized `<CharKey>`/`<ByteKey>` — so they directly test both instantiations of the shared code.

**Why char proofs stay valid at `K=CharKey` (behavioral identity argument):** the alias `PersistentCharNode<V> = OverlayNode<CharKey, V>` produces a type whose every method has the *same signature and same body* as before (the bodies are the char file verbatim; the only edits are `MAX_PREFIX_LEN`→`CharKey::MAX_PREFIX_LEN` = 6, the same value, and `[0u32;4]`→`[CharKey::UNIT_ZERO;4]` = `[0u32;4]`, the same value). Monomorphization of `OverlayNode<CharKey,V>` yields byte-identical machine code to today's `PersistentCharNode<V>`. Hence the loom/proptest/TLA correspondence — which is over observable behavior (CAS, `try_set_final`, `find_child`) — is unchanged by construction.

### 6.2 New test needed for the BYTE instantiation

**Exactly one new proof obligation, and it is for Phase 4 (the increment rework), not the node alias:** byte's increment changes from in-place `fetch_add` to path-copy CAS. Add a **byte mirror of the char increment-race loom test** — `byte_create_vs_increment_race_has_one_leaf_and_total_value` — in `tests/persistent_artrie_loom_correspondence.rs` (which already has the byte `ModelValueLeaf`/`try_increment_checked` scaffolding at lines 113-204). This asserts: two threads, one creating the key + one incrementing, converge to one leaf with the summed value (no lost update; root-CAS is the linearization point). It is a transcription of the char test with `u8` keys.

For the **node-alias phases (2, 5)** specifically: **no new test is needed** — the byte node's existing 22 unit tests (moved to `core::overlay` as `<ByteKey>` tests) plus the full byte suite cover it, and behavioral identity holds by the same monomorphization argument as char.

### 6.3 Green gate at each phase (the checklist)

At **every** phase boundary, all three must hold:
1. `systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp cargo nextest run --features persistent-artrie` → **≥ 2474 passing, 0 failing.**
2. `systemd-run … scripts/verify-formal-correspondence.sh` → **exit 0** (compiles both feature profiles + all correspondence tests, incl. `persistent_lockfree_overlay_proptest`, `persistent_char_ebr_correspondence`, `persistent_public_durability_policy_correspondence`).
3. `systemd-run … scripts/verify-unsafe-boundary-inventory.sh` (invoked by #2) → **set-equality** between `formal-verification/UNSAFE_INVENTORY.tsv` and the live `rg`-scanned `unsafe` sites. Since G4 adds **zero** `unsafe` (Send/Sync auto-derive throughout; the only `unsafe impl` touched are the already-deleted-and-commented ones in the node files, which vanish entirely when those files become aliases), the inventory should need **no edits** — but if removing the byte/char node files drops any ledger rows that referenced their paths, update the ledger in the same commit (Constraint 2).

---

## 7. Risk register (honest, with mitigations)

| Risk | Likelihood | Impact | Evidence / Mitigation |
|------|-----------|--------|----------------------|
| **R1 — SIMD specialization forces a per-key `find_child`** | **None** | — | Verified: `grep` for `_mm/avx/sse/target_feature` over both overlay `persistent_node.rs` = 0 hits. SIMD is only in owned `node16_char.rs` (out of scope). The overlay `find_child` is linear/binary, identical on both sides. No fallback needed. |
| **R2 — `KeyEncoding::Unit` lacks `Default` for the `[0;4]` filler** | Certain (it lacks it) | Low | Resolved by adding `UNIT_ZERO` const (§1.2). Alternative: bound `Unit: Default`. The filler is dead padding; either works. |
| **R3 — C3/C4/C5 used `CharUnit`/`char`; choosing `KeyEncoding`/`u32` diverges from precedent** | Certain | Low | Deliberate and justified: the overlay's key is genuinely `u32` (on-disk format stores `u32` code points; `CharUnit` isn't even impl'd for `u32`). `KeyEncoding` already has `CharKey{Unit=u32}`/`ByteKey{Unit=u8}` and is the *purpose-built* core seam. Documented in the design header so future readers don't "fix" it to `CharUnit`. |
| **R4 — byte/char node files are mutually inconsistent as committed (node post-G1, increment pre-G1)** | Certain (found it) | **High if missed** | This is *the* substantive work. `byte/lockfree_cas.rs:540-577` uses `try_increment_value` which the byte node already commented out — byte may not currently build against its own node at `<u64>` increment. Phase 4 explicitly ports char's path-copy increment to byte *before* the node alias. Sequencing it as its own reversible phase isolates the risk. |
| **R5 — Manual `Clone`/`Debug` bounds: a `#[derive]` would leak `K: Clone`/`K: Debug` into call-sites** | Medium | Low | Hand-write `impl<K: KeyEncoding, V: Clone> Clone` and `impl<K, V> Debug` (manual, K-free), exactly as the existing files hand-write `Debug` for the `V`-only case. Marker types are `Copy+Clone+Debug` anyway, so even an accidental derive compiles — but manual keeps bounds minimal. |
| **R6 — `AtomicNodePtr` char today impls `Clone`/`Default` only for `V=()` (atomic_ptr.rs:255,264), an inconsistency** | Certain | Low | Generic version impls `Clone`/`Default` uniformly for `<K, V: Clone>` (strictly more general). Verified no call-site depends on the `()`-only restriction (both just call `Self::new/null`). Widening is safe. |
| **R7 — `compare_exchange` None-arm constructs `OverlayNode::new()` (needs `V: Clone`)** | Certain | Low | The whole `impl AtomicNodePtr` block is already `<V: Clone>`, so `OverlayNode::<K,V>::new()` (also `<V:Clone>`) is callable. No new bound. (This arm is the unreachable "slot went null" case, preserved verbatim from char:204.) |
| **R8 — Monomorphization bloat (two full instantiations of one generic vs two hand-written types)** | Low | Negligible | Code size is identical to status quo: today there are already two concrete node types compiled; after G4 the compiler monomorphizes `OverlayNode<ByteKey,_>` and `OverlayNode<CharKey,_>` to the same two. Vocab's `<CharKey,u64>` is a third instantiation that *already exists today* (`PersistentCharNode<u64>`). Net: no new monomorphs beyond what exists. |
| **R9 — Vocab coupling: a hidden vocab dependence on a char-only node detail** | Low | Medium | Audited both vocab consumers (`lockfree.rs`, `lockfree_cas.rs`): they use only `new/as_final/with_value/find_child/as_in_mem/is_null/get_value/with_child` + `AtomicNodePtr` ops — all in the shared API. The `unreachable!` and reverse-index are outside the node. Phase 3 is a build-only validation gate to catch any surprise. |
| **R10 — Unsafe-inventory set-equality breaks when node files are deleted** | Medium | Medium (gate fails) | The deleted node files carry only *commented* `unsafe impl` (provenance) and zero live `unsafe`. Confirm `UNSAFE_INVENTORY.tsv` has no rows for those paths (it shouldn't, since the gate scans live `unsafe` only — `rg` pattern excludes `^//`). If stale rows exist, update ledger in the same commit (Constraint 2). |
| **R11 — `TrieRoot` blanket changes byte `Value` from `u64` to `V`** | Certain (Phase 6) | Low | Phase 6 is optional/isolated. Byte callers expecting `u64` get `OverlayNode<ByteKey,u64>` (same). Defer Phase 6 if any byte MVCC caller pins `u64` awkwardly; node unification (Phases 0–5) does not require it. |
| **R12 — On-disk format drift** | None | — | `overlay_to_inner<V>` (`persist.rs:966`) reads only preserved public methods; format is `[len][bincode(V)]`, untouched. Constraint 3 ✓. No serialization test changes. |
| **R13 — Proof invalidation by refactoring algorithms (if §3b attempted in G4)** | — | High *if attempted* | Mitigation: **do not** attempt §3b in G4. Keep algorithms per-variant (§3a); the loom/proptest models the concrete shapes. §3b is a separate, later, reversible change. |

---

## Appendix A — The four token-level substitutions (the entire mechanical diff for the node body)

When porting the char node into `core::overlay/node.rs` as `OverlayNode<K,V>`, these are the **only** non-mechanical edits (everything else is `s/PersistentCharNode/OverlayNode<K, …>/` and `s/u32/K::Unit/` in types):

1. `keys: [0u32; INLINE_CAPACITY]` / `[0u8; …]` → `keys: [K::UNIT_ZERO; INLINE_CAPACITY]` (4 sites: `ChildStore::new`, the demote arm in `without_child`).
2. `new_keys[n - 1] = 0;` (clear last slot in `without_child` Inline arm) → `new_keys[n - 1] = K::UNIT_ZERO;`.
3. `.min(MAX_PREFIX_LEN)` → `.min(K::MAX_PREFIX_LEN)` (2 sites: `with_prefix`, `with_prefix_replaced`).
4. `std::mem::size_of::<u32>()` / `<u8>()` → `std::mem::size_of::<K::Unit>()` (memory_usage, 2 sites).

All method *bodies* (recursion, CAS, `try_set_final`, sorted insert/shift, promotion/demotion) are copied verbatim.

## Appendix B — Decision summary (for the impl engineer)

- **Trait bound:** `K: KeyEncoding` (extended with `MAX_PREFIX_LEN` + `UNIT_ZERO`); unit is `K::Unit` (`u8`/`u32`). Not `CharUnit` (wrong unit type, surplus methods), not bespoke (redundant).
- **Home:** `persistent_artrie_core::overlay` (`node.rs`, `atomic_ptr.rs`, `mod.rs`).
- **Types:** `OverlayNode<K,V>`, `Child<K,V>`, `ChildStore<K,V>`, `AtomicNodePtr<K,V>` — all with hand-written `Clone`(bound `V:Clone`)/`Debug`(K-free), auto-`Send`/`Sync`, zero `unsafe`.
- **`LockfreeInsertResult`:** stays per-variant (over the variant's concrete `K`).
- **Algorithms:** stay per-variant in G4 (DRY at the node; §3b algorithm-sharing is a later follow-up).
- **Aliases:** byte `PersistentNode<V>=OverlayNode<ByteKey,V>`; char `PersistentCharNode<V>=OverlayNode<CharKey,V>`; vocab unchanged (`<CharKey,u64>`).
- **`TrieRoot`:** optionally unify as a blanket in `core::overlay` (Phase 6).
- **Critical precursor:** Phase 4 ports char's path-copy increment to byte (byte is pre-G1 on increment) — the one piece of real behavioral work; everything else is alias + verbatim port.

---

### Critical Files for Implementation
- `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_core/key_encoding.rs` — extend `KeyEncoding` with `MAX_PREFIX_LEN` + `UNIT_ZERO`; the chosen bound for the unified node.
- `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_char/nodes/persistent_node.rs` — the verbatim source for `OverlayNode<K,V>` (port into new `persistent_artrie_core/overlay/node.rs`); becomes a `CharKey` alias.
- `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie/nodes/persistent_node.rs` — byte node; becomes a `ByteKey` alias (its tests move to `core::overlay` as `<ByteKey>` coverage).
- `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie/lockfree_cas.rs` — Phase 4 hotspot: port char's path-copy increment (`build_value_path_recursive`) here, genericize `lockfree_root` over `V` (byte is pre-G1).
- `/home/dylon/Workspace/f1r3fly.io/libdictenstein/src/persistent_artrie_char/lockfree_cas.rs` — the proven reference implementation (`build_value_path_recursive`, two-phase `try_set_final`, Order-A durable paths) to mirror for byte and to keep green as the behavioral-identity oracle.
