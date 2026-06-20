# Slice 3 / Level 3 — L0.2 rollback retrospective (2026-06-08)

> **Status: L0.2 (delete owned READ tails) ROLLED BACK.** The read-tail deletion is
> **mis-ordered** in the converged plan and has been **moved to L3.2** (fused with the
> `::new()`→overlay install + struct-zipper re-point). **Level 0 is now L0.1-only**
> (eviction owned-arm deletion, already committed at `5aa6fc3`; `#46` at `1637aed`).
> Next actionable slice = **L1**. This file records *why*, so the failed ordering is
> not re-attempted.

## What L0.2 tried
Per the converged plan (`slice3-level3-converged-plan-2026-06-08.md` lines 81–86), L0.2
deleted the `route_overlay()==false` owned READ tails from the public read methods,
collapsing `if route_overlay() { overlay } else { owned }` → `overlay`:
- char `query_api.rs`: `try_contains` / `get_value` / `try_get`
- char `prefix_api.rs` / `prefix_helpers.rs`: `iter_prefix*` + the merge-read
- byte `cursor_iter.rs`: `iter_prefix_from_cursor` + delete `collect_terms_from_cursor`

Premise (stated in my L0.2 comments): *"the owned read arm was reachable only via
`kill_switch_to_owned`"* — i.e. assumed dead in production.

## Why it was unsound — the `::new()` route_overlay()==false wall
**The premise is empirically FALSE.** `route_overlay()` (`overlay/flip.rs:348`) is
`overlay_write_mode().uses_overlay() && lockfree_root().is_some()`. In-memory
constructors — `PersistentARTrieChar::new()` (`mmap_ctor.rs:43`) and
`PersistentARTrie::new()` (`mmap_ctor.rs:71`) — build the struct with
`lockfree_root: None` + `overlay_write_mode: OwnedTree` (default). Only the **disk-backed
`create*` ctors** call `apply_create_flip`. So **every `::new()` trie has
`route_overlay()==false` permanently, by construction** and uses the OWNED read path.

`::new()` is a **public, documented API** (the `zipper.rs:50` rustdoc example and the
**formal gate** `tests/zipper_language_correspondence.rs:538,553` both drive
`::new()`), not a test artifact.

Empirical proof: the L0.2 collapse made char `contains()`/`get_value()` overlay-only;
`contains_lockfree` (`lockfree_cas.rs:1219`) returns `false` unconditionally when
`lockfree_root == None`. Full feature-on suite went **2712-green → 76 failures**, of
which **44 were basic `persistent_artrie_char_integration` unicode/emoji/zipper tests
with ZERO `kill_switch` usage** — they just `::new()` + insert + read.

## Why the read-tail deletion belongs in L3 (the plan's own precondition)
The converged plan **already schedules** the `::new()`→overlay flip at **L3.2** (lines
160–161: *"`::new()` must install an empty overlay … Update zipper_language_correspondence
to not rely on owned"*) and ground-truth correction #3 (line 47) notes the struct zipper
"passes the formal gate today only because zipper_language_correspondence exercises it on
a `::new()` in-memory trie (`route_overlay()==false`)". The owned read tails are the
**only** read path for `::new()` tries **until** that flip lands. So the read-tail
deletion is **causally downstream of L3.2** — it cannot precede it.

## Why "flip `::new()` to overlay early" (Strategy B) is non-viable (not just risky)
Investigated via Explore + Plan agents (read-only). Three independent code-level walls:
1. **`flip_to_overlay()` hard-requires a WAL.** `flip.rs:564`:
   `wal_current_lsn().is_some() && wal_is_overlay_regime()`; `::new()` has no
   `wal_writer` → the flip returns false and `apply_create_flip` converts that into a
   **hard `internal()` error** (`mmap_ctor.rs:50`). Naively flipping `::new()` would make
   it *panic/error* — strictly worse.
2. **The durable write path is WAL-coupled.** Public `insert` under `route_overlay()`
   routes to `insert_cas_durable` → `durable_policy_gate` (errors unless Immediate/
   GroupCommit) → `append_to_wal_returning_lsn`, which returns `Ok(0)` with no WAL; the
   Order-A contract (`wal_helpers.rs:73`) **forbids acknowledging a `0`-LSN as durable**.
   A WAL-less overlay write is a *new representation* with its own recovery semantics.
3. **The struct zipper has no overlay implementation** — `has_path`/`is_final_at_path`/
   `get_children_at_path` (`zipper.rs:190/234/258`) walk `inner.root.read()` (owned-only);
   re-pointing them IS the ⚠️RT L3.2 work ("wrong overlay-zipper = silent wrong query
   results").
Strategy B would invent a WAL-less overlay write mode + fork `apply_create_flip` + pull
the most data-loss-critical step (zipper) to the front onto the least-proven foundation —
the opposite of "reversibility preferred until the keystone." **Rejected.**

## Decision: Strategy A (immediate) refined to Strategy C (campaign re-scope)
- **A (done today):** roll back the uncommitted L0.2 read collapses → restore the
  2712-green baseline (`1637aed`). Pure revert of uncommitted work; **no red-team needed**
  for the rollback (returns to a verified commit).
- **C (campaign re-scope):** the read-tail deletion is **not a standalone step**. Fold it
  into **L3.2** as a single atomic commit that *also* (i) makes `::new()` install an empty
  overlay and (ii) re-points the struct zipper — so there is never a window where reads
  are overlay-only while any constructor still yields a `route_overlay()==false` trie.
  Decide the `::new()`-overlay **durability** question (walls #1/#2 above) as its own
  gated, reversible design spike **before** L3.2.

## Corrected ordering (top-level backbone UNCHANGED)
`L0 → L1 → CX → L2 → L3`, but:
- **L0 = L0.1 only** (eviction owned-arm deletion, committed `5aa6fc3`; `#46` at
  `1637aed`). L0.2 removed from L0; L0.3 (OR-lock) already retracted → L3.3 (refinement R1).
- **Next slice = L1** (recovery redirect, `mmap_ctor.rs` recover-family — task #40).
- **L3.2 absorbs the former L0.2.** The read tails L0.2 targeted (char `try_contains`/
  `get_value`/`try_get`, `iter_prefix*`; byte `iter_prefix_from_cursor` +
  `collect_terms_from_cursor`) are exactly the owned readers L3.3 already lists for
  deletion — they die WITH the owned root + zipper re-point, not before.

## Lesson (do not re-attempt)
"Delete the `route_overlay()==false` owned arm" is only sound for surfaces where **no
public constructor yields `route_overlay()==false`**. Because `::new()` is permanently
`route_overlay()==false`, the owned **read** path is load-bearing until `::new()` itself
is overlay-backed. Any future "collapse the owned arm" step must first verify
`::new()`-overlay backing exists, or be fused into the same commit that adds it (L3.2).
