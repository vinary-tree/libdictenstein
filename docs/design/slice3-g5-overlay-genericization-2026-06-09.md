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

## G5.3 — unify the `lockfree_cas.rs` private DFS CAS primitives (hot path, loom-gated)
Lift `build_path_recursive`/`create_lockfree_path`/`build_value_path_recursive`/`build_remove_path_recursive`/
`find_leaf_recursive`/`try_{insert,remove}_lockfree_path`/`insert_lockfree_recursive` into a `pub(crate)`
trait default-method family over `OverlayNode<K,V>`+`OverlayFaulter` (token-identical modulo `&[K::Unit]`).
Adopt char's `finalize: bool` + `BuildPathError{AlreadyExists,Io}` (promote to core), mapping byte's
bare-`Err(())` to preserve its conflict-RETRY semantics (RT-5). Public entry points stay per-variant
3-line `&str`/`&[u8]`→`&[K::Unit]` skins. Do NOT change node-build order (feeds the serializer — RT-4).

## G5.4 — unify the faulter newtype + `Shared*::root` wiring

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
