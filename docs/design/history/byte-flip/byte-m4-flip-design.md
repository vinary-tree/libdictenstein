# Byte M4 — the IRREVERSIBLE create-flip + reestablish sink

> ## ⛔ RED-TEAM VERDICT: NO-GO as designed → GO-WITH-FIXES (agent `a893dc6b`, code-grounded vs `b5ff744`)
> The D1 reestablish-sink ordering (this design's §1 claim) **PASSES** — byte genuinely inherits char's fix
> (the reestablish folds read the unrouted `owned_*`/`unrouted_*` seams; M3 routed the public API, NOT the
> `_impl` methods reestablish/recovery use; the publish path never reads `self.root`). BUT two independent
> IRREVERSIBLE total-loss defects survive:
> - **D-VAL (P0, BLOCKER, irreversible total VALUE loss):** the overlay i64 CHECKPOINT drops every counter
>   value on reopen. `overlay_checkpoint.rs:580` forces every overlay final to `ChildNode::ArtNode` (never a
>   bucket); `serialize_impl.rs:265` does `let _ = value;` (ArtNode value never written); `disk_load.rs:310/
>   599/874` reload `value: None`; the retaining WAL does NOT save it (recovery SKIPS deltas ≤ checkpoint_lsn,
>   recovery.rs:318). §2's "not a hard blocker" was WRONG — the equivalence is VACUOUS (equals the broken
>   owned-ArtNode path; byte's WORKING value path is the BUCKET, serialize_impl.rs:46, which the overlay
>   capture never produces). The flip MOVES i64 values from the round-tripping bucket rep to the
>   non-round-tripping ArtNode rep = a NEW M4 regression. **FIX (one of): (A)** implement byte ArtNode value
>   serialization (serialize_impl.rs:265 + disk_load.rs:310/599/874) — lifts byte's "future work" the flip now
>   depends on; **(B)** make `overlay_root_to_owned`/`overlay_node_to_child` emit valued finals as BUCKETS (the
>   value-preserving path), not ArtNodes; **(C)** ship M4 for `V=()` membership ONLY this release (no value to
>   lose), defer the `V=i64` flip until A/B. The valued-checkpoint-reopen gate test currently FAILS.
> - **D-SINK (P0, BLOCKER):** the corruption-rebuild arm (`mmap_ctor.rs:714 Self::create` → replays into owned
>   at :738/:774 → `:798 Ok((trie,report))`) returns with NO reestablish sink → recovered owned data never
>   reaches the overlay → first checkpoint persists the empty overlay = total loss. The sink MUST be at ~:798
>   (covering BOTH replay arms), NOT in create() (which runs on the empty tree before the replay). The design
>   §0.2 "recover_from_archives" wording is CHAR-borrowed and WRONG — byte has NO recover_from_archives; the
>   byte equivalent is this corruption arm (mmap_ctor.rs:682-798).
> - **D-SINK-2 (HIGH, must-add):** EDIT-2 open-flip + reestablish at `mmap_ctor.rs:~536` + `io_uring_ctor.rs:
>   ~324`, gated `rank_regime==Overlay && overlay_eligible_v()`. (Delegation covers open_with_slot_tracking/
>   _recovery/_and_slot_tracking + all SharedARTrie ctors.)
> - **D-FLIP-COVERAGE (MED):** `apply_create_flip` on ALL THREE create bodies (mmap:96 create, mmap:190
>   create_with_slot_tracking, io_uring:40 create_with_io_uring), not just create().
> - **D-NEW-FOOTGUN (LOW):** do NOT apply the flip to `mmap_ctor::new()` (WAL-less, deprecated — flip returns
>   false there, the hard-error would break the in-memory path).
> - **Test reframes (~30–45 byte eligible-V tests):** doc-tx/merge/CAS/compact tests → `kill_switch_to_owned()`
>   after create (M3 rejects them under overlay); checkpoint-survival tests (Class E) gated on D-VAL; the
>   flip-gate inertness tests (overlay_write_mode.rs:788/817, overlay_routing_tests.rs:79/194/484,
>   overlay_correspondence_tests.rs:90/186 — `!route_overlay()` fresh-is-owned assumption now false) →
>   precondition updates; the VACUOUS-PASS `.get()` class (swap to `.get_value()` so they don't mask D-VAL).
> - **DO-NOT:** trust §2's value dismissal; sink-only-in-open(); flip new(); unconditional open-flip; leave
>   `.get()` value checks (vacuous-pass masks D-VAL); tidy recompute_recovered_increment/`_impl` to routed
>   reads (reborns char's 2nd D1 bug); rely on the WAL to recover D-VAL-dropped counts.
>
> **The §2 below is SUPERSEDED by D-VAL.** Full verdict in the session transcript.
>
> ## OWNER DECISION + EXECUTION PLAN (post-red-team)
> Owner chose **"Implement ArtNode value serialization, full i64 flip"** (option A). M4 is decomposed:
> - **M4a (the D-VAL fix — a committable, reversible durable-format extension; NOT the flip):** make byte's
>   ArtNode on-disk record carry an OPTIONAL value (serialize_impl.rs:265 + serialization.rs v2 node codec +
>   disk_load.rs:310/599/874), BACK-COMPAT-SAFE (value-less records byte-identical; a HAS_VALUE flag/version
>   gates the new value bytes; the WAL MAGIC_OVERLAY tripwire bounds old binaries from valued-node files). Gate
>   = the valued-checkpoint-reopen for V=i64 (the currently-FAILING D-VAL gate) GREEN + a value-less back-compat
>   round-trip + the full suite. Commit M4a on its own.
> - **M4b (the IRREVERSIBLE flip — surfaced for the owner's final GO with the diff):** fold ALL the red-team
>   fixes — apply_create_flip on ALL 3 create bodies (mmap:96/190, io_uring:40; NOT new()); the reestablish
>   SINK at mmap_ctor.rs:798 (corruption arm, both replay arms — D-SINK) + the EDIT-2 open-flip+reestablish at
>   mmap:~536 + io_uring:~324 gated `rank_regime==Overlay && overlay_eligible_v()` (D-SINK-2); byte
>   reestablish_overlay_dispatch (the V-3 twin); the ~30–45 test reframes (doc-tx/merge/CAS/compact →
>   kill_switch; flip-gate inertness preconditions; the vacuous-pass .get()→.get_value()). Gate = full suite +
>   the M4b gate tests (reestablish-survival incl. >100k partition, create→write→reopen, old-owned-stays-owned,
>   compact-rejects, valued-checkpoint-reopen) + formal exit 0 + 0 unsafe + D1 grep empty + the byte red-team's
>   DO-NOT list honored. Surface the irreversible diff → explicit owner GO → commit.

---


**Crate `libdictenstein`, byte `src/persistent_artrie/`. Baseline HEAD `b5ff744` (M0–M3 done: byte has the
complete durable-overlay subsystem + routing/rejects, all opt-in behind the inert `route_overlay()`).
DATA-LOSS-CRITICAL, IRREVERSIBLE.** M4 makes the lock-free overlay byte's production default for
`V ∈ {(), i64}` — the byte twin of char's S5-12 EDIT 1/2/3 + the E1 reestablish-sink. Owner gave the
directional GO ("full byte flip"); this lands AFTER a red-team + the full gate + the diff surfaced for an
explicit final GO.

## 0. What M4 adds (mirrors char EDIT 1/2/3)
1. **Create-flip (EDIT 1):** byte `apply_create_flip` on the create ctors — `if overlay_eligible_v() &&
   !flip_to_overlay() { return Err(internal) }`. A fresh `create::<i64|()>()` becomes overlay-routed
   (`route_overlay()==true`). `flip_to_overlay` is the shared `LockFreeOverlay` default (M2a) — it
   `enable_lockfree()`s (which now stamps the Overlay regime on the empty WAL, M2d) + sets the mode + the
   V-2 stamp check. Arbitrary V (not in {(),i64}) → no-op, stays owned.
2. **Reestablish sink (EDIT 3 — byte has ZERO reestablish today):** after EVERY recovery path that can
   produce an Overlay-regime reopen, add `if route_overlay() { reestablish_overlay_dispatch()? }`. The
   sites (the M2d sinks + open): mmap_ctor `open` (after replay), `open_with_recovery_config` corruption
   arm + `recover_from_archives`, io_uring `open`. Byte `reestablish_overlay_dispatch` (the byte twin of
   char's V-3) = SAFE Any-downcast: `i64` → `reestablish_overlay_counter` (the trait default), `()` →
   `reestablish_overlay_membership` (the trait default).
3. **EDIT 2 (open mode):** an Overlay-regime reopen must end with `route_overlay()==true`. The create-flip
   ctors flip on create; the open path: after recovery + reestablish, set `LockFreeOverlay` mode for an
   eligible V on an Overlay-regime WAL (the M2d `enable_lockfree` stamp made the regime durable).

## 1. D1 — THE CRITICAL INVARIANT (byte inherits char's fix via the shared trait)
The reestablish folds (`reestablish_overlay_counter`/`_membership`, the `DurableOverlayWrite`/
`LockFreeOverlay` trait DEFAULTS from M1/M2a) run with `route_overlay()` ALREADY TRUE (the ctor/open flips
before dispatching reestablish). They read the recovered owned tree via the `owned_*` seam readers, which
byte implements (M2a) over the UN-ROUTED `unrouted_*` walks of `self.root` (verified D1-safe: every
`unrouted_*` does `match &self.root`, zero routed `self.{get,get_value,contains,iter_prefix}` calls). So
byte's reestablish reads the OWNED tree (not the empty overlay), publishes to the overlay, then
`clear_owned()` LAST. **Byte inherits char's D1 guard for free through the shared trait + the M2a unrouted
seams.** The shared Rocq `OverlayReestablishSpec.v` (variant-agnostic) is the formal guard. The M4 red-team
MUST re-verify this ordering holds at byte's ctor/open call sites (the create-flip happens BEFORE the
reestablish dispatch, and nothing between them reads through a routed path that the reestablish depends on).

## 2. The ART-node-value prerequisite (INVESTIGATED — not a hard blocker)
Byte stores real values in buckets/ChildNodes (serialize + round-trip — proven by the 2580 green incl.
value-roundtrip tests); the overlay capture (M2b `overlay_root_to_owned`) maps overlay finals' values →
ChildNode values → the SAME serializer (equivalent-by-construction). The vestigial `ArtNode.value` field is
never populated by byte's insert path (values go to buckets) and is ignored identically in owned + overlay
capture. M4's gate adds a definitive **valued-checkpoint-reopen** test (byte i64 overlay write → checkpoint
→ reopen-from-checkpoint → value preserved) to confirm the overlay checkpoint round-trips values.

## 3. Gate (all green before the diff is surfaced)
- Full `cargo nextest --features persistent-artrie` green (the create-flip changes the DEFAULT for
  `<i64>`/`<()>` byte tries, so tests that build `create::<i64|()>()` + assert owned-path behavior get the
  `kill_switch_to_owned()` reframe — the char precedent; the INERT-pre-flip property no longer holds for
  eligible-V create, so this is the one place the baseline legitimately shifts).
- NEW: byte reestablish-survival test (build owned, checkpoint, reopen → reestablish, assert EVERY term +
  value survives) INCLUDING a negative-count term (C4 — must reject at write, so the survival test uses
  non-negative) and a >100k-term first-byte partition (H2 — the uncapped enumerator).
- NEW: create-flip gate tests — create→write→reopen (overlay survives), old-owned-file-stays-owned
  (ineligible V / a pre-existing Owned-regime file reopens Owned, no flip), compact-rejects-under-overlay,
  the valued-checkpoint-reopen (§2).
- `verify-formal-correspondence.sh` exit 0; 0 new unsafe; the D1 owned-seam grep stays empty.
- The byte red-team cleared.

## 4. Top red-team targets (the irreversible-flip risks)
1. **The reestablish sink D1 ordering** (§1) — the create-flip MUST precede the reestablish dispatch, and
   the reestablish MUST read unrouted owned; verify at every byte ctor/open site. Total-loss if wrong.
2. **Every recovery path reaches a reestablish sink** — char had open + corruption-arm + recover_from_archives
   (3+ sites). Find any byte recovery/open path that produces an Overlay-regime reopen WITHOUT a reestablish
   → the first post-recovery checkpoint persists the empty overlay = total loss (the M2b sink-gap, now the
   M4 fix — must cover ALL sites).
3. **The checkpoint route-split is reached post-flip** — under `route_overlay()`, byte's `checkpoint()` must
   take the OverlayCheckpoint overlay arm (M2b), capturing the live overlay (not the cleared owned tree).
   Verify the create-flip makes `route_overlay()` true BEFORE any checkpoint.
4. **old-owned-file back-compat** — a pre-M4 Owned-regime byte file MUST reopen Owned (no flip), its data
   intact. The create-flip is for fresh `create()`; `open()` of an Owned-regime file stays Owned.
5. **The valued-checkpoint-reopen** (§2) — confirm overlay values round-trip through the checkpoint
   serializer (the ART-node-value concern).
6. **Irreversibility boundary** — the first Overlay-regime WAL archive segment (old binaries fail-closed on
   the Overlay regime stamp). Confirm the dual-magic / regime tripwire applies to byte (shared WAL header).
