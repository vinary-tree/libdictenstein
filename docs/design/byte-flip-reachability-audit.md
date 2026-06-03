# Byte ARTrie overlay-flip — reachability audit (predictive, the byte twin of the char data-loss audit)

**Crate `libdictenstein`, byte variant `src/persistent_artrie/`. Baseline HEAD `1f120e8` (char on the
shared `LockFreeOverlay` trait). READ-ONLY audit by agent `a9132d79` + parent.** This is the data-loss
prerequisite for Step 2 (the byte flip): when byte's `create()` create-flips (V ∈ {(), i64}) and an
Overlay-regime reopen clears the owned tree, which INTERNAL byte reads become reachable under
`route_overlay()` and would silently read the empty owned tree? The char twin of this audit found two
data-loss bugs; the byte audit finds the byte set + THREE char-absent hazards.

## Ground truth
- Byte has NO flip today (`route_overlay`/`OverlayWriteMode`/`LockFreeOverlay` appear 0× in
  `src/persistent_artrie/`). `enable_lockfree` (lockfree_cas.rs:93) does NOT stamp the WAL regime (the
  generic `flip_to_overlay` `current_lsn()==1` restamp must cover byte). Predictive audit.
- Byte's un-routed owned read layer = the `_impl` methods: `get_value_impl` (query_impl.rs:70),
  `contains_impl` (query_impl.rs:34) walk `self.root` directly — byte's structural equivalent of char's
  `owned_*`. Byte internal code reads through `_impl`, so byte's recovery RMW is ALREADY structurally
  owned-safe where char's pre-fix code was not.
- Byte counter overlay = `impl<S> PersistentARTrie<i64,S>`; `get_lockfree(&[u8])->Option<u64>`,
  `increment_cas(&[u8],u64)->u64` (no-WAL publisher, lockfree_cas.rs:595). ⇒ seam `CounterValue = i64`
  with an i64↔u64 conversion at the overlay boundary (the leaf stores i64, bounded ≥0 by
  LOCKFREE_COUNTER_MAX, widened to u64 losslessly).

## A. NEEDS-OWNED-READER — already satisfied, must PROTECT (not change)
- **Site #1 — `recompute_recovered_increment` (mutation_core.rs:468)**, the BatchIncrement recovery-replay
  RMW reached via `apply_recovered_operation_no_wal` (mutation_core.rs:376/410) from all 3 recovery sites
  (mmap_ctor.rs:436/650, io_uring_ctor.rs:256). It ALREADY reads `self.get_value_impl` (the un-routed
  owned reader) — byte is structurally pre-fixed (the char bug #1 twin, already safe). **Action:** keep it
  on `_impl`; NEVER tidy it to a routed `get_value`/`contains`; the §6(a) CI grep gate forbids
  recovery/owned readers referencing `route_overlay`/`iter_prefix(`/`self.get(`/`get_value(`. Correct ONLY
  if paired with the recovery reestablish sink (§C.3).

## B. BROKEN-BY-DESIGN — 8 reject guards (the byte twins of char's 8)
Each is reachable under the flip and incoherent with overlay-is-durable; reject with
`InvalidOperation` under `route_overlay()`:
- `merge_from` (merge_api.rs:54) — #2 (covers `merge_replace`).
- `merge_from_batched_with_options` (merge_api.rs) — #3 (covers `merge_from_batched`/`_grouped`).
- `merge_from_parallel` (parallel_merge.rs, feature `parallel-merge`, on SharedARTrie) — #4 (byte has ONE
  parallel merge; char had two).
- `merge_lockfree_values_to_persistent` (lockfree_cas.rs:605) — #5 (overlay→owned drain + clears overlay
  = destroys durable state).
- `merge_lockfree_to_persistent` (lockfree_cas.rs:343) — #6 (cache→owned drain).
- `begin_document` + `commit_document` (document_tx.rs:27/171) — #8 (owned absolute write via
  `insert_impl_core`); reject at BOTH entry points; this also closes `try_tx_increment_bytes`
  (document_tx.rs:140) reachability.
- `remove_prefix_batched` (mutation_api.rs:206) — #15 (routed `iter_prefix` read + owned `remove_impl`
  write = silent no-op delete); reject OR reimplement over an overlay remove-CAS (char implemented it).

## C. CHAR-ABSENT byte-specific hazards (the audit's highest value)
1. **`compact()` (compaction_impl.rs:110) — P0, char has no trie-level compact.** Public + FILE-REPLACING:
   `compaction_snapshot` enumerates overlay terms (routed `iter_prefix_with_arena`) but reads values from
   the empty owned tree (`get_value_impl`→None), builds a value-stripped trie, and ATOMICALLY RENAMES it
   over the original — clobbering the durable WAL/overlay with a counters-lost image. **Reject under
   `route_overlay()`** (or make it a route-split-aware overlay snapshot). The single most dangerous
   char-absent surface.
2. **`iter_with_values` (public_iter.rs:35) — byte's MIXED-read iterator.** It enumerates via the arena
   iter (routed→overlay) then re-reads each value via `get_value_impl` (owned→None). The flip must give it
   a VALUE-CARRYING overlay route (the trait's `overlay_collect_units_with_values`), NOT
   enumerate-overlay-then-value-owned. (Char's `iter_prefix_with_values` routes as a unit, so char never
   hit this.)
3. **The recovery REESTABLISH SINK GAP (byte has zero `reestablish`).** Char ends recovery with
   `if route_overlay() { reestablish_overlay_dispatch()? }` (mmap_ctor.rs:1085/1330). Byte's 3 recovery
   sites rebuild owned-only and stop; byte's checkpoint reads `self.root` (serialize_impl.rs:112), so the
   first post-recovery checkpoint under the flip would persist the EMPTY overlay = the rebuilt terms lost.
   The byte flip MUST add the reestablish step after all 3 replay loops (the byte EDIT-3). Site #1's owned
   read is correct ONLY when paired with this sink.

## D. Write-flip checklist — SAFE-DEAD *iff routed* (each owned read is a missed-route tripwire)
The flip MUST add `if route_overlay() { <overlay CAS> } else { <owned body> }` to each, above the owned
read/write — a miss = silent under-count/lost write:
- `increment_bytes` (atomic_ops.rs:40) → byte `increment_cas`/`try_increment_cas`.
- `upsert_bytes` (atomic_ops.rs:125) → byte overlay upsert.
- `compare_and_swap_bytes` (atomic_ops.rs:162) → **NO overlay CAS-with-expected exists on byte today** ⇒
  REJECT under overlay (or build the primitive).
- `get_or_insert_bytes` (atomic_ops.rs:224) → overlay get-or-insert.
- `insert`/`insert_with_value` (mutation_api.rs:27/32) → `insert_cas` membership/value-CAS.
- `remove` (mutation_api.rs:190) → overlay remove-CAS, else reject.
- The ITER FAMILY (public_iter.rs:56/70/87, cursor_iter.rs:151): route the ARENA/CURSOR iters at the
  public top (char precedent: `iter_prefix_with_arena` is routed), not merely the thin `iter`/`iter_prefix`
  wrappers — else `len`/`iter` read owned (empty) under the overlay.

## Bottom line for the byte flip design
- 1 recovery RMW (#1) already owned-safe → protect + wire the reestablish sink (§C.3).
- 8 reject guards (§B) + the 2 char-absent rejects/routes (`compact` §C.1, `iter_with_values` §C.2).
- ~7 write entry points + the iter family (§D) = the write-flip route checklist; any miss = silent loss.
- Highest char-absent risk: `compact()`'s atomic file replacement (§C.1) + the recovery reestablish gap
  (§C.3). Both MUST be in the byte flip design + gate.
