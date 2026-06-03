# Overlay-Flip Genericization — share the char flip with byte (+ why vocab is excluded)

**Crate `libdictenstein`. Baseline HEAD `7cc9984` (char flip shipped). Data-loss-critical.**
Design by a Plan agent (`af63b7a0`) + parent red-team (the 4 load-bearing claims below are
code-verified). Source spec: `docs/design/s5-12-e1-readflip-design.md`. This designs the extraction of
the char lock-free-overlay flip into a SHARED GENERIC layer over `K: KeyEncoding`, so byte reuses it
rather than copy-pasting; vocab is excluded by correctness.

## 0. Verified ground truth (the 4 load-bearing facts)
1. **Byte's counter overlay is `V = i64`** (`impl<S: BlockStorage> PersistentARTrie<i64, S>`,
   `src/persistent_artrie/lockfree_cas.rs:425`; `get_lockfree(&[u8]) -> Option<u64>` :434). Char's is
   `u64`. ⇒ the eligible counter monomorph is per-variant (char `{(),u64}`, byte `{(),i64}`).
2. **Vocab's overlay value is an allocator-assigned index** (`next_index.fetch_add(1, AcqRel)` INSIDE
   `insert_cas`, `src/persistent_vocab_artrie/lockfree_cas.rs:131`). Replaying inserts during a
   reestablish would allocate FRESH indices, corrupting the durable term↔index bijection + the `.idx`
   reverse index. ⇒ vocab CANNOT use the flip/reestablish; it is flip-safe by never flipping.
3. **Byte's `enable_lockfree` does NOT stamp the WAL regime** (`src/persistent_artrie/lockfree_cas.rs:93`
   sets `lockfree_root` but no `set_overlay_regime`); char's does (`persistent_artrie_char/lockfree_cas.rs:186`).
   ⇒ the generic `flip_to_overlay`'s `current_lsn()==1` Overlay-restamp must cover byte.
4. **Byte's public iter API is shaped differently** (`iter_prefix(&[u8]) -> Option<impl Iterator<Item=
   Vec<u8>>>`, `src/persistent_artrie/public_iter.rs:64`), vs char's `Result<Option<Vec<String>>>`. ⇒
   the public-method routing glue is irreducibly per-variant.

## 1. Extraction surface (LOC-weighted over the ~620-LOC char flip)
- **(a) FULLY GENERIC over `K` → move to core (~45%, ~280 LOC):** the overlay-read DFS walks
  (count_finals/navigate/collect in `K::Unit` space), `OverlayWriteMode` + `route_overlay`, the
  `flip_to_overlay`/`kill_switch_to_owned` bodies (incl. the WAL-regime restamp), the `reestablish`
  streaming-fold control flow (the data-loss-critical clear-owned-LAST), the value-route SHAPE, the
  merge-reject helper.
- **(b) GENERIC-WITH-SEAM (~25%, ~155 LOC):** the seam trait itself + the value-route/dispatch
  monomorph selection (generic body; the variant supplies the concrete counter monomorph).
- **(c) IRREDUCIBLE per-variant (~30%, ~185 LOC):** the owned-tree readers (node-format-specific:
  char `CharTrieRoot` vs byte `TrieRoot` Bucket/ArtNode), the public-method routing glue (signature
  divergence, fact #4), the Order-A durable writes (already per-variant; byte has them), the
  `Vec<K::Unit>`→public-term adapter.
- **Net:** byte gains **~120–160 LOC** vs **~620** copy-pasted (~75% reduction).

## 2. The seam trait `LockFreeOverlay<K: KeyEncoding, V, S>`
Decision: a **seam trait with default-provided generic methods + variant-supplied seam methods** — NOT a
blanket impl (three distinct trie structs, no single type to blanket), NOT a wrapper struct (reestablish
mutates `&mut self` trie state while reading the owned tree via `&self`; a wrapper re-creates the seam as
constructor args with a lifetime mess across the `&self`-iter-before-`&mut`-clear ordering). Lives in
`persistent_artrie_core::overlay::flip`; each variant writes one thin `impl` providing only the seam.
- **`type CounterValue: 'static + Copy`** — the per-variant counter monomorph (`u64`/`i64`). THE
  divergence that makes the value-route a seam, not a blanket.
- **Required seam (~18 small methods):** `lockfree_root()`, `overlay_write_mode()`/`set_…`,
  `enable_lockfree()`, `wal_current_lsn()`/`wal_is_overlay_regime()`/`wal_stamp_{overlay,owned}_regime()`;
  the **UN-ROUTED owned readers** `owned_first_units()`/`owned_units_under(&[Unit])`/
  `owned_units_with_values_under`/`owned_has_empty_term_value()`/`clear_owned()` (each carries a
  `# Safety (data-loss): MUST read the owned tree directly, never via route_overlay()` contract — D1);
  the overlay publishers `overlay_publish_membership/counter`, `overlay_counter_get`, `overlay_contains`;
  `overlay_eligible_v()`.
- **Default-provided generic (DO NOT override — they encode D1 + clear-owned-LAST):** `route_overlay`,
  `overlay_root_node`, `overlay_len`/`overlay_is_empty`, `overlay_navigate(&[Unit])`,
  `overlay_collect_units(_with_values)`, `flip_to_overlay`, `kill_switch_to_owned`,
  `reestablish_overlay_membership`/`_counter`, `reject_under_overlay`.
- The public-method routing (`if route_overlay() { adapt(overlay_collect_units) } else { owned_* }`)
  stays per-variant + thin (the adapter is the per-variant skin).
- Coherence: trait in core, impls in variant modules, **one crate** ⇒ no orphan-rule problem (same as
  the existing `TrieRoot for OverlayNode` blanket).

## 3. `KeyEncoding` additions (reverse conversion)
Add `type Term: Clone` + `fn units_to_term(&[Self::Unit]) -> Self::Term`. Char: code points →
`String` (`char::from_u32(_).unwrap_or('\u{FFFD}')`, the existing lossy map). Byte: identity
`Vec<u8>` — **byte terms are arbitrary byte strings; reconstruction is the raw-bytes copy, NO UTF-8
re-decode** (the byte API is byte-native, returning `Vec<u8>`; UTF-8 is the caller's concern). `type
Term` beats `units_to_string(->Result<String>)` because byte terms aren't always valid UTF-8. Property:
`units_from_str ∘ units_to_term == id` on each variant's valid domain (a proptest). The forward
direction (`units_from_str`) already exists.

## 4. Value-route `Any`/`TypeId` genericity across `K`
- **Composes cleanly:** `Any` needs `Self: 'static`; `K: 'static` is guaranteed by `KeyEncoding:
  'static`, `V`/`S` already `'static` ⇒ no new bound. `K` is never a separate downcast target — it's
  baked into the concrete monomorph (`PersistentARTrie{Char}<CounterValue, S>`), so `TypeId` can't
  collide across variants; zero `unsafe`.
- **The u64-vs-i64 divergence:** the generic `route_get_value` driver branches on
  `TypeId::of::<V>() == TypeId::of::<Self::CounterValue>()` (counter) / `== ()` (membership) and re-wraps
  via `Any` on `V`/`CounterValue` (both `'static`), never on `K`. The actual `downcast_ref::<concrete
  monomorph>()` lives in the ~2-LOC seam (`overlay_counter_get`), where the monomorph is nameable.
  `reestablish_overlay_dispatch` follows the same seam pattern (`downcast_mut::<<K,CounterValue,S>>()` in
  a ~10-LOC per-variant fn; bodies are the generic defaults).

## 5. Migration sequence (green at each step; reversible until the per-variant ctor create-flip)
- **Step 1 — extract char → core (BYTE-IDENTICAL REFACTOR):** add `KeyEncoding::{Term,units_to_term}`;
  move `OverlayWriteMode` to core; create `core::overlay::flip::LockFreeOverlay` with the default bodies
  ported TOKEN-FOR-TOKEN from char; write `impl LockFreeOverlay<CharKey,V,S> for PersistentARTrieChar`
  (the seam delegates to the EXISTING shipped `owned_*`/`insert_cas`/`increment_cas`/`<u64>`-downcast);
  re-point char's public methods at the trait. **Guarantee byte-identical:** the existing char E1
  correspondence suite + 2547-test nextest + `verify-formal-correspondence.sh` exit 0 + 0-new-unsafe +
  the `units_from_str∘units_to_term==id` proptest, all UNCHANGED. Reversible.
- **Step 2 — wire byte (the irreversible byte flip):** add byte `overlay_write_mode` field; `impl
  LockFreeOverlay<ByteKey,V,S>` (`CounterValue=i64`, `eligible={(),i64}`, byte owned readers, byte
  publishers, `<i64>`-downcast); byte create-flip + open-reestablish wiring (byte `enable_lockfree`
  gains the first-call Overlay stamp, or rely on the generic `current_lsn()==1` restamp — fact #3); byte
  public-routing glue; **the byte reachability audit** (the byte twin of char's `try_increment_impl_no_wal`
  owned_get fix + the merge-reject guards); byte E1 correspondence tests (`&[u8]`/`i64`); a byte
  reestablish-survival test. **Gate = char's gate.** Reversible up to the byte create-flip line.
- **Step 3 — vocab: NO CODE CHANGE.** Excluded by fact #2 (index-allocator overlay). Vocab gets at most
  a read-only `OverlayRead<K,V>` sub-trait IF a future overlay-enumeration need arises — not now.

## 6. Red-team (data-loss-critical)
- **(a) HEADLINE — the owned-fallback seam wired to a routed reader = D1 again, per variant.** The
  genericization INCREASES this risk: `owned_units_under` is less self-evidently "un-routed" than char's
  inline-commented body, and a byte author who already has a routed `iter_prefix` is tempted to delegate
  to it → reestablish reads the empty overlay, publishes nothing, clears owned = total loss.
  **NON-OPTIONAL 3-layer mitigation:** (1) a CI grep gate that FAILS if any `owned_*` seam body
  references `route_overlay`/`iter_prefix(`/`self.get(`/`get_value(`; (2) a reestablish-survival
  correspondence test (clone of char `s5_10b_*`) that goes red the instant the overlay is empty
  post-reestablish; (3) `OverlayReestablishSpec.v` is the formal statement that owned-source is the only
  correct read source (already proven, variant-agnostic — byte inherits it free).
- **(b) `Any`/`K` composition:** proven `'static`; `CounterValue` prevents i64/u64 confusion at the type
  level; caught by compile + the byte counter read-back test + an `overlay_eligible_v` unit test.
- **(c) vocab divergence:** excluded by design; forcing it in would re-allocate indices (silent
  corruption); caught by the vocab suite + a "vocab has no `overlay_write_mode` field" structural assert.
- **(d) silent char behavior change in the refactor:** token-for-token port (only the two boundary
  conversions change, defined to reproduce char behavior); the existing char correspondence suite is the
  oracle; the round-trip proptest is the formal "refactor cannot change terms" statement.

## 7. GO/NO-GO
**GO** for Step 1 (char→core refactor) + Step 2 (byte reuse). **NO-GO** for folding vocab into the flip
(fact #2). Single most important risk: §6(a) — the owned-seam wired to a routed reader; the grep gate +
reestablish-survival test + the Rocq guard are the non-optional mitigation.
