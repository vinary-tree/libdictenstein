# Arbitrary-`V` Support for the Lock-Free Overlay — Design & Roadmap

> Status: **roadmap** (not yet implemented). Answers "what would full arbitrary-`V`
> support entail?" The lock-free overlay node currently carries only a `u64` value
> (`value: AtomicU64`), so the immutable/lock-free architecture can be the *default*
> checkpoint/read/write source only for `V ∈ {(), u64-counter}`. This document is
> the plan to lift that to **all** `V`.

## Executive summary

Arbitrary-`V` means swapping the overlay node's value field from `AtomicU64` to an
immutable, construction-time **`Option<V>`** baked into each path-copied node, then
threading a `<V>` type parameter through `PersistentCharNode`, `Child`, `ChildStore`,
`AtomicNodePtr`, the char/byte `lockfree_cas.rs`, the `TrieRoot` MVCC trait, and the
vocab overlay. The hard part is **not** the typing — it is that *an arbitrary `V`
cannot live in an atomic*, so the char overlay must drop its two-phase
(`CAS-then-try_set_final`) finalization and adopt the **vocab overlay's single-phase
model** (finalize+value baked into the node; the root CAS is the sole arbiter; a
losing CAS re-finds the winner). The prefix-insert fix survives this flip (root CAS +
re-find-on-conflict is the arbiter), but **must be re-proven** with the loom/proptest
harness — that re-proof is the riskiest single obligation.

**The decisive enabler:** the on-disk format **already** stores `[node][value_len:u32]
[bincode(V)]` (`persist.rs` ≈L529-555; read back at `disk_io.rs:242`). The `u64` limit
lives *only* in the overlay's `AtomicU64`. So genericizing the overlay's in-memory value
unblocks Phase B/C for all `V` with **zero on-disk format change** and full backward
compatibility.

**Worth doing? Yes, but staged.** Effort **L–XL**. For `V=()` it is a net *win* (node
shrinks: 8-byte `AtomicU64` → ~0-byte `Option<()>` niche). The biggest risk is the G1
single-phase flip + prefix re-proof, not the eventual default-flip plumbing.

## Recommended value representation: immutable `Option<V>`, single-phase

Evaluated four candidates:

| # | Representation | Verdict |
|---|---|---|
| **1** | **immutable `Option<V>`, root-CAS-arbitrated (vocab single-phase)** | **RECOMMENDED** |
| 2 | `arc_swap::ArcSwapOption<V>` (atomic `Arc<V>` publish, keeps two-phase) | not zero-cost for `()`; `Arc<V>` alloc per valued node; preserves prefix fix unchanged |
| 3 | `AtomicPtr<V>` / hand-rolled boxed-V | **REJECTED** — re-adds `unsafe` (the leak-fix removed it) |
| 4 | hybrid (`AtomicU64` fast path + `Option<V>` general) | complexity not worth a *permanent* hybrid; prefer a narrow `u64`-only `increment_cas` specialization if benchmarks demand |

**Why #1:** allocation-lean (inline `V`, niche for `()`); already proven in-tree (the
vocab overlay runs single-phase correctly); unifies the char + vocab overlays onto one
model (dissolving the "do NOT apply the prefix fix to vocab" caveat); and the
`Option<V>` serializes directly with no lossy `u64` bridge.

**Same-key value race (v1 ≠ v2):** #1 is *first-committer-wins* at the overlay (the root
CAS is a unique total order); production `insert_with_value` is *last-writer-wins*. This
is a **semantic gap for arbitrary `V`** (invisible for `()`/counters). Resolution: add an
explicit last-writer-wins `upsert_cas` (a value-update path-copy) for production parity,
codified by a `BTreeMap<String,V>`-oracle proptest.

**Prefix-insert fix under single-phase (the crux, re-derived):** inserting a proper
prefix ("d" after "da") finalizes the *existing* non-final `n_d` by path-copying it AS
final + value and CAS-ing the root. Two racers each build their own finalized copy; the
**root CAS serializes them** — the first installs, the loser's CAS fails, re-loads, re-
runs, finds the node final, returns `AlreadyExists`. No double-finalize. The fix's "return
shared node un-finalized" trick is two-phase-specific; single-phase replaces it with
"build finalized copy + root-CAS-arbitrate + re-find-on-conflict" (exactly the vocab
model). **Must be re-proven in loom + proptest** (the rewritten "prefix single-arbiter"
loom model).

## Genericization ripple
- `PersistentCharNode<V>`, `Child<V> = InMem(Arc<PersistentCharNode<V>>) | OnDisk(SwizzledPtr)`,
  `ChildStore<V>`, `AtomicNodePtr<V> = ArcSwapOption<PersistentCharNode<V>>`,
  `LockfreeInsertResult<V>`. Bound: the **existing `DictionaryValue`**
  (`Clone+Default+Send+Sync+Unpin+'static+Serialize+DeserializeOwned`) — same as
  `CharTrieNodeInner<V>`, so no new trait. `Send+Sync` still auto-derives (Phase-A win
  preserved; no `unsafe impl`).
- `impl TrieRoot for PersistentCharNode<V>` + `TrieRoot::get_value -> Option<u64>`: add an
  associated `type Value` (ripples to `ReadTransaction::get/get_str` + byte/char/TestNode
  impls) — OR defer (keep `u64` MVCC snapshot reads) for the scoped `V∈{(),u64}` work.
- The **vocab** overlay becomes a `PersistentCharNode<u64>` consumer; reverse-index
  (`index_term_storage`) is outside the node, untouched; its `unreachable!` on-disk branch
  stays. Genericization makes char *match* vocab's single-phase model.
- The **byte** overlay (`PersistentNode`) genericizes symmetrically to `PersistentNode<V>`
  — mechanical mirror, deferred to G4.

## How it unblocks Phase B/C for all `V`
With `Option<V>` in the node, `capture_snapshot_immutable`'s `overlay_to_inner` converter
copies `Option<V> → Option<V>` directly into `CharTrieNodeInner<V>` and feeds the existing
`serialize_char_node_to_disk` — byte-equivalent (neither rep uses path compression).
Recovery deserializes `bincode::<V>` (bound already in `DictionaryValue`). The generic-`V`
checkpoint limitation is *removed*; no on-disk format change.

## Forward-compatibility with the scoped (`V∈{(),u64}`) work
**Introduce `PersistentCharNode<V = ()>` (default type param) so bare `PersistentCharNode`
keeps compiling as `<()>` and arbitrary `V` slots in additively.** The value-field swap
(`AtomicU64`→`Option<V>`) is NOT purely additive (it flips finalization to single-phase),
so: write the Phase-B converter generic from day one (`overlay_to_inner<V>`), and avoid a
`u64→Option<V>` bridge that becomes dead code. **Trade-off:** doing the `Option<V>` swap +
single-phase flip as G1 *now* avoids rework but pays the single-phase re-proof cost
upfront; keeping the proven two-phase `AtomicU64` overlay for the `V∈{(),u64}` scoped work
defers that risk at the cost of a small converter bridge later. For `V∈{(),u64}` the
CURRENT two-phase overlay already works perfectly (membership needs no value; counters use
the wait-free `fetch_add`), so G1 is only strictly required when arbitrary `V` is pursued.

## Sub-phases (effort / reversibility)
| Sub-phase | Scope | Effort | Reversible |
|---|---|---|---|
| **G1** | `<V=()>` type param + `value: Option<V>` + flip char overlay to single-phase + re-prove prefix/no-double-count (loom+proptest) | **L** | yes (gate/comment two-phase) |
| **G2** | `TrieRoot` associated `type Value`; `ReadTransaction::get/get_str -> Option<V>` | **M** | yes |
| **G3** | serialize/recover `V` (generic `overlay_to_inner` + watermark-correct `capture_snapshot_immutable` + recovery `bincode::<V>`) | **M** | yes |
| **G4** | vocab reconciliation (`PersistentCharNode<u64>`) + byte overlay mirror (`PersistentNode<V>`) | **M** | yes |
| **G5** | irreversible DEFAULT flip: owned `Arc<RwLock<…>>` → arc-swap root for all `V`; comment-out owned tree; keep watermark durability + `u64`-increment specialization kill-switched | **L–XL** | **no** |

**Riskiest: G1** (single-phase flip + prefix re-proof), gated by the loom/proptest rewrite.

## Risk register (abridged)
- **R1 (high):** same-key value semantics — overlay first-wins vs production last-wins; add `upsert_cas` + BTreeMap-oracle proptest. Invisible for `()`/counters.
- **R2 (high):** prefix-fix interaction — re-prove single-phase arbitration in loom+proptest.
- **R3 (low, win):** `V=()` node shrinks (`Option<()>` niche) vs current `AtomicU64`.
- **R4 (med):** large-`V` node bloat (inline `Option<V>` on every node) — document `Box`/`Arc` wrapping for large `V`.
- **R5 (med):** `increment_cas` loses wait-free `fetch_add` → narrow `u64`-only specialization if benchmarks regress.
- **R7 (low):** #1/#2 add **zero `unsafe`** (avoid #3).
- **R9 (none):** on-disk format already carries `bincode(V)` → full back-compat.

See [[lockfree-overlay-leak-fix-phase-a]], [[lockfree-overlay-prefix-insert-bug]],
[[overlay-node-cannot-carry-generic-v]], [[lockfree-checkpoint-watermark]].
