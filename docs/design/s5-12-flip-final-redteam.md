# S5-12 Flip — Final Pre-Flip Red-Team (VERDICT + fix list)

**Crate `libdictenstein`, char ARTrie. Read-only red-team (Plan agent + parent). Baseline HEAD `38c0f4e`
(S5-1…S5-11 done/committed/verified: 2534 tests green, formal gate exit 0, 0 new unsafe).**

## (a) VERDICT: **NO-GO as designed → GO-WITH-FIXES**

The reversible machinery (S5-1…S5-11) is correctly built + the recovery semantics are sound. **But the
load-bearing safety guard both design docs + the impl plan ASSUME — "the ctors gate the flip on
`TypeId::of::<V>() ∈ {u64,()}`" and "`enable_lockfree` refuses to stamp Overlay for V ∉ {(),u64}" — DOES
NOT EXIST** (exhaustive grep: zero `TypeId`/`Any` in `persistent_artrie_char/`). The flip MUST NOT ship
until the gate + V-3 dispatch are implemented + Test A / RES-3 close.

Confirmed-correct (do NOT re-touch): the dual-magic tripwire (#3), Overlay-drops-unranked on every live
path (#5), both checkpoint route-splits incl. RES-4 `SharedCharARTrie::checkpoint`, all producer guards
(RES-1), the empty-WAL regime-stamp guards, the no-WAL reestablish + clear-owned-LAST ordering.

## (d) Vectors found

- **V-1 (CRITICAL, corruption + lock-up): the TypeId gate does not exist.** Wiring `flip_to_overlay()` into
  the generic ctor without it ⇒ arbitrary V gets `lockfree_root=Some`+`LockFreeOverlay` ⇒ `route_overlay()`
  true ⇒ write-broken trie (increment/fetch_add/merge/begin_document reject; batch routes to undefined
  arbitrary-V overlay; `build_value_path_recursive` is u64-only). **FIX = EDIT 0.**
- **V-2 (HIGH, silent mis-flip): `enable_lockfree` swallows a failed Overlay stamp** (lockfree_cas.rs:186-188
  only `log::warn!`s, still enables the overlay). If ever reached on a `current_lsn()!=1` WAL, the overlay is
  enabled with an Owned-regime WAL ⇒ recovery KEEPS unranked orphans ⇒ resurrection. **FIX:** create-flip
  hard-errors if the stamp didn't take; flip path makes the warn caller-visible; open CASE (a) only runs when
  the on-disk regime is ALREADY Overlay (stamp = verified-idempotent no-op).
- **V-3 (MEDIUM, value loss): `reestablish_overlay_after_recovery` (u64) is `impl<u64,S>`-only**, NOT
  name-resolvable from the generic `impl<V>` ctor; the membership twin IS callable for u64 but DROPS values.
  A naive `if TypeId==u64 { reestablish_overlay_after_recovery() }` won't compile, tempting the value-dropping
  fallback. **FIX:** compile-selected V-specialized ctor extensions (`…<u64>` + `…<()>`), never route u64
  through the membership twin.
- **V-4 (MEDIUM, silent value loss on wrong-V reopen): no on-disk V-type discriminator.** bincode
  trailing-byte tolerance (bincode_compat.rs:57 drops `_consumed`) ⇒ `deserialize::<()>([8 u64 bytes])`=Ok(())
  (value dropped), `deserialize::<u64>([])`=DecodeError (term dropped). No file corruption, but mis-loads.
  **FIX:** operational invariant (reopen with the same V — same constraint the on-disk node blob already has)
  + GAP_LEDGER + a gate test asserting no panic/corruption. NOT a flip blocker.
- **V-5 (LOW): faulting-first remove** — addressed by `218b3d7` (non-faulting-first); re-run the N-S4-3 soak.

## (b) Edit list

**EDIT 0 (NEW, MUST-FIX) — TypeId gate, expressed ONCE:** add `pub(crate) fn overlay_eligible_v() -> bool`
(`overlay_write_mode.rs` impl<V,S>): `TypeId::of::<V>()==TypeId::of::<u64>() || ==TypeId::of::<()>()`
(`DictionaryValue: 'static`, so callable). Harden `flip_to_overlay` (:97): `if !Self::overlay_eligible_v()
{ return false; }` BEFORE enable_lockfree — arbitrary V never gets the overlay, stays OwnedTree.

**EDIT 1 — CREATE flip** (mmap `create`:79 / `create_with_slot_tracking`:143 / `create_with_config`:204;
io_uring `create_with_io_uring`:42): before the `Ok(Self{…})`, if `overlay_eligible_v()` call
`flip_to_overlay()` + hard-error if `!took` (fresh WAL ⇒ stamp must take). First checkpoint route-splits to
the overlay; reopens intact. NO-OP for arbitrary V (EDIT 0). [IRREVERSIBLE — this is the flip.]

**EDIT 2 — OPEN three cases** (mmap `open`:282 after replay@439 / `open_with_depth`:510 after :659;
io_uring `open_with_io_uring`:107 after :252): the `rank_regime` local is in scope.
- **(a) Overlay-regime + eligible V:** `enable_lockfree()` (idempotent, regime already Overlay ⇒ stamp no-op)
  + **V-specialized** reestablish (u64 value-carrying via `…<u64>` extension; `()` membership) + set
  LockFreeOverlay. The `?` ABORTS open on reestablish Err (owned cleared LAST ⇒ owned intact). [IRREVERSIBLE]
- **(b) Owned-regime + NON-EMPTY** (old never-flipped trie, new binary): SKIP ⇒ stays Owned. Backward-compat:
  no flip, no rotation, no loss (`set_overlay_regime` would reject the non-empty WAL anyway).
- **(c) Owned-regime + EMPTY:** **STAY OWNED** (decided). An empty Owned WAL is a deliberately-Owned file
  (arbitrary V / pre-flip binary / kill-switch-after-truncate); flipping on open would make `open` silently
  irreversible. The ONLY ways to Overlay: create-flip, or an already-Overlay file.

**EDIT 3 — corruption rebuild** (`open_with_recovery_config` arm:791 + `recover_from_archives`:1205): after
the rebuild populates owned, apply CASE (a) if the segments are Overlay-regime (`any_overlay` already at :862)
— else a corruption-rebuild of a flipped trie returns OwnedTree mode + the next checkpoint route-splits to the
EMPTY overlay (total loss). [IRREVERSIBLE-adjacent]

## (c) Gate (new flip tests — `target/test-tmp`)
1. **TypeId gate (PRIMARY):** create<u64>/create<()> ⇒ route_overlay()==true + MAGIC_OVERLAY; create<String>
   ⇒ route_overlay()==false + MAGIC + a subsequent increment/insert_batch succeeds via owned.
2. create→write→reopen (u64 + ()): checkpoint entry_count==N (not 0), reopen all N + correct counts.
3. open-old-Owned-stays-Owned (backward-compat): Owned trie reopen ⇒ route_overlay()==false, intact, MAGIC.
4. flip→crash-at-each-point→reopen (OD4 `set_commit_rendezvous` seam): post-stamp-pre-write; AfterAppend
   (orphan DROPPED); mid-reestablish (owned intact, re-runnable); post-clear-pre-checkpoint (pre-clear WAL
   replays full set).
5. old-binary-fail-closed: MAGIC_OVERLAY file fails an Owned-only open.
6. mixed-monomorph: u64 file reopened as <()> — no panic, no file corruption (document value-loss, V-4).
7. Test A end-to-end (residual, see (e)).
8. Cross-cutting: unsafe-inventory (0 new), --no-default-features, formal-correspondence, loom, N-S4-3 soak,
   full 2534 suite.

## (e) MUST-CLOSE-before-flip residuals
- **V-1 TypeId gate** (EDIT 0) — non-negotiable.
- **V-3 reestablish dispatch** (compile-selected) — else u64 reopen silently loses every counter value.
- **Test A end-to-end:** rendezvous-manufacture a durable UNRANKED orphan: `set_commit_rendezvous(AfterAppend
  ⇒ skip the CommitRank)` on a flipped u64 trie, `insert_cas_with_value_durable("orphan", v)` (data Insert
  durable, CommitRank never appended) + a normal "survivor"; checkpoint→rotate to an Overlay archive;
  `recover_from_archives` ⇒ orphan DROPPED, survivor KEPT. (Core drop already unit-covered by
  `reconcile_regime_aware_drops_overlay_orphans_keeps_owned_and_ranked`; the end-to-end real-archive path is
  NOT yet tested.)
- **RES-3 base-image self-completeness:** `recover_from_archives` deletes the base + passes checkpoint_lsn=0;
  `collect_retained_wal_segments_for_rebuild` renames the active WAL into the archive set (first-LSN-sorted,
  monotone across rotate). Gate-assert the archive set's first-LSN==1 (no interior gap) + that `prune_segments`
  (writer.rs:629) never strips below the rebuildable prefix.
- COVERED (no new work): reconcile core drop, dual-magic accept-set, regime-stamp guards, rotate floor/regime
  carry, reestablish u64 equivalence, producer rejects.

## Irreversibility boundary (confirmed ARMED)
`set_overlay_regime` writes MAGIC_OVERLAY+Overlay in ONE 64-byte header `write_all`+`sync_all` (writer.rs:394)
⇒ no torn magic-without-regime window. `from_bytes` accepts both magics, rejects unknown ⇒ an old binary
(MAGIC-only) FAIL-CLOSES on an Overlay WAL; NO silent-Owned-misread window.
