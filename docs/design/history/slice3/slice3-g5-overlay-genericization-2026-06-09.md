# G5 — Genericize + Share the Lock-Free Overlay Logic (design, 2026-06-09)

Owner directive: "genericize and share as much of the overlay logic as possible — use traits."
Completes the G-series. Precedes the vocab flip so vocab *instantiates* the shared machinery
rather than hand-rolling a third copy (trait-first; see [[vocab-overlay-flip-campaign]]).

## Why now
L3.3c-C2 (commit b9570ca) deleted the owned trees, so byte (`persistent_artrie/`) and char
(`persistent_artrie_char/`) are OVERLAY-ONLY — their overlay code is now near-identical modulo the
key type `K`. G1–G4 already shared the core (`persistent_artrie_core/overlay/`: `OverlayNode<K,V>`,
`LockFreeOverlay`/`DurableOverlayWrite`/`OverlayCheckpoint`/`OverlayEvictable`/`OverlayFaulter` traits,
the non-faulting read engine, the Order-A durable-write skeleton, `build_overlay_root_from_terms`).
G5 unifies the *remaining* per-variant duplication.

## Verified load-bearing claims (checked against source before implementing)
- `OverlayFaulter<K,V>: Send + Sync` (faulter.rs:52) ⇒ a `{overlay: Option<Arc<OverlayNode<K,V>>>,
  overlay_faulter: Option<Arc<dyn OverlayFaulter<K,V>>>}` node AUTO-derives Send/Sync → char's
  `unsafe impl Send/Sync for PersistentARTrieCharNode` is **redundant** (a clean −2 UNSAFE-row reduction).
- char `overlay_value_get` FAULTS (BUG #46 fix); byte's is non-faulting → the unify must adopt FAULTING.
- `KeyEncoding` lacked the public-token surface → G5.0 added it.

## G5.0 — KeyEncoding token contract (DONE)
Added `type Token` + `token_to_unit(Token)->Unit` + `unit_to_token(Unit)->Option<Token>` to
`KeyEncoding`. ByteKey `Token=u8` (identity); CharKey `Token=char` (`as u32` / `char::from_u32`, so a
surrogate `u32` yields `None` and is SKIPPED — preserving the old `edges()` filter). Additive,
zero-behavior. Tests: `{byte,char}_key_token_unit_roundtrip`.

## G5.1 — unify the `DictionaryNode` handle
`OverlayDictionaryNode<K,V>` in core (`overlay/dict_node.rs`) with `type Unit = K::Token` + the
`DictionaryNode`/`MappedDictionaryNode` impls + `from_overlay_root`/`from_overlay_node`/`overlay_child_node`.
Re-alias `PersistentARTrieNode = OverlayDictionaryNode<ByteKey,V>` and
`PersistentARTrieCharNode = OverlayDictionaryNode<CharKey,V>` (public names preserved for downstream).
Delete byte's one-arm `NodeInner::Overlay` wrapper + char's hand-written handle + char's `unsafe impl
Send/Sync` (auto-derives now) → UNSAFE_INVENTORY −2 rows + UNSAFE_CONTRACTS −`char-public-node-thread-contract`
tag (set-equality, lock-step).

## G5.2 — unify the `overlay_write_mode.rs` seam bodies
Collapse the line-identical `LockFreeOverlay`/`DurableOverlayWrite`/`OverlayCheckpoint` delegating bodies
to trait defaults; route str↔units via `K::units_to_term`/`units_from_bytes`. Adopt the FAULTING
`overlay_value_get` (RT-1). KEEP the counter `Any`-downcast seams per-variant (they name `<u64,S>`).

## STATUS (2026-06-09): G5.0+G5.1+G5.2 COMMITTED 906b4cf
G5.0 (KeyEncoding `Token`), G5.1 (`OverlayDictionaryNode<K,V>` + byte/char aliases, char `unsafe impl
Send/Sync` removed → −2 inventory/−1 contract), G5.2 (`overlay_value_get` unified to one FAULTING default;
byte latent BUG#46 fix). nextest 2708/0/3, loom (overlay 12/f4 3/durable 3), unsafe set-eq 0, format untouched.

## G5.3' — generic CAS-WALK SKELETON + per-variant specialization hooks (REVISED per OWNER DECISION)
The original G5.3 premise ("lift token-identical, adopt char's finalize form for byte") was REFUTED by code:
byte uses LSN-order recovery (NO CommitRank) + a DUAL-method (non-durable two-phase `try_set_final` + a
separate durable single-phase) + no-generation result types; char uses D2.8 generation-ranked recovery + a
single `finalize`-flag method + generation-bearing results. Forcing one form onto the other re-architects a
loom-verified data-loss-critical path. OWNER DECISION (3 clarifications): "unify where they share OR SHOULD
share logic; allow optimized, specialized operations per data type." → the design:

- NEW `pub(crate) mod cas_walk` (core): free generic-over-`<K,V>` COMMON fns — `find_leaf_recursive`,
  `find_in_lockfree_trie`, `create_spine` (bottom-up build w/ a leaf-maker closure), `build_value_spine`,
  `resolve_or_fault` (the OnDisk write-path fault-in, copy-pasted ~7× today).
- `trait OverlayCasWalk<K,V,S>: LockFreeOverlay<K,V,S>` with SPECIALIZATION HOOKS: assoc
  `InsertResult`/`RemoveResult`/`BuildErr`; `make_insert_*`/`make_remove_*` (build the per-variant result —
  WITH char's generation / WITHOUT byte's); `claim_generation` (char=`claim_commit_seq`, byte=default,
  vocab=`next_index.fetch_add`); `insert_terminal_leaf(node, WalkCtx)` (byte-dual vs char-finalize, via a
  `Copy WalkCtx::{Membership{finalize},Value,IndexAlloc}`); `fault_in`; + DEFAULT skeleton
  `build_insert_spine`/`try_insert_path`/`build_remove_spine`/`try_remove_path` (shared descent + the SINGLE
  root-CAS + retry structure). Result/error enums STAY per-variant.
- Each variant impls the hooks in its `overlay_write_mode.rs`; NEITHER adopts the other's form. byte keeps
  no-generation/dual-method (I/O→Conflict); char keeps generation-ranked/finalize-flag.
- MUST stay specialized: the key-decode boundary, char generation-ranking, byte dual-method, error
  cardinality, remove terminal semantics, vocab index-alloc + bloom side-effect.
- "Should-share" wins: `resolve_or_fault` (×7 dedup), a `drive_cas` retry-loop driver, `create_spine`.

### Phasing (each: build + nextest + --no-default + doctests + formal + unsafe + fmt + 3 loom [overlay/f4/durable] + soak; reversible until Phase 6; ZERO unsafe/format/build-order delta)
P0 dormant scaffold · P1 lift pure find/spine helpers (delegation) · P2 char remove · P3 char insert ·
P4 byte remove + non-durable insert · P5 byte durable single-phase · P6 shared `drive_cas` (LAND).
P7 (separate — the payoff): vocab plugs in `impl OverlayCasWalk<CharKey,u64,S>` (`WalkCtx::IndexAlloc`) — zero new shared code.

### Red-team focus (data-loss-critical)
generation captured at the EXACT linearization point + handed to `make_insert_inserted` (no stale gen →
correct `reconcile_lww` order) · proper-prefix `finalize:false`→`Arc::clone(node)` (the `try_set_final`
arbiter) un-cross-wireable · byte dual-method preserved as 2 `WalkCtx` behaviors · `resolve_or_fault`
preserves byte's I/O→Conflict mapping · exactly ONE root-CAS/attempt · hooks monomorphized (no `dyn`) ·
`create_spine` build-order unchanged.

## G5.4 — STOPPED (byte's Shared-root `faulter=None` is a DELIBERATE specialization — retain; changing it alters byte's read behavior). Share only the faulter NEWTYPE if cleanly common.

## DEFER G5.5 — sharing the serialize/enumerate codec SPINE (on-disk-format drift risk; out of scope)

## Per-variant (MUST stay): the `KeyEncoding` impl, the on-disk leaf codec
(`serialize_overlay_to_disk_iterative` leaf + `enumerate_terms_from_disk`), the block-storage loaders
behind `fault_overlay_slot`, the batch evict driver + LRU path conversion, the counter `Any`-downcast.

## Invariants / gates (every phase)
On-disk format byte-identical (G5.1-4 are in-memory/control-flow only). Public API + `DictionaryNode::Unit`
(`u8`/`char`) unchanged. UNSAFE delta = the G5.1 −2 only. Gate each phase: build --all-features (0 err,
warnings ≤ pre-existing 20) + full nextest (≥2706) + --no-default-features + doctests + formal (0) +
unsafe set-equality (0) + fmt; G5.3 also loom (`--cfg loom` overlay + f4-hierarchy) + a soak run.

## Red-team focus (RT-1..7)
RT-1 adopt faulting `overlay_value_get` (else BUG #46 reintroduced for char) · RT-2 `unit_to_token`
surrogate-skip + no panic on astral Token · RT-3 DictionaryNode coherence (`Unit=u8`/`char` monomorphs) ·
RT-4 node-build order = serializer input (no format drift) · RT-5 `finalize`/`BuildPathError` preserves
byte conflict-retry · RT-6 dropping char unsafe is sound (auto-derive) + set-equality −2 · RT-7 keep the
counter `Any`-downcast per-variant (don't bake a 2-monomorph assumption — vocab is coming).

## Vocab readiness
After G5, the vocab flip reduces to: `impl KeyEncoding for VocabKey` (+`Token`) + ~5 thin seam impls
(`LockFreeOverlay`/`DurableOverlayWrite`/`OverlayCheckpoint`/`OverlayEvictable`/`OverlayFaulter`) + 2 codec
fns (`serialize`/`enumerate`) — the node handle, read engine, CAS primitives, Order-A skeleton, checkpoint
route-split, evict primitives, and `build_overlay_root_from_terms` are all INHERITED.
