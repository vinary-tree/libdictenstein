# Shared lock-free durable-overlay architecture (char + byte + future variants)

**Crate `libdictenstein`. Baseline HEAD `1f120e8` (char on `LockFreeOverlay`). Pattern-driven, DRY,
non-blocking, scalable.** This is the architecture for sharing the WHOLE overlay subsystem — not just the
flip-layer (done) but the durable-write + checkpoint + watermark + recovery machinery — across the byte
and char variants (and future ones), so there is ONE copy of the data-loss-critical control flow.

Design owner: parent. Pattern grounding: pgmcp software-pattern catalog (`recommend_design_patterns` +
`software_pattern_search`, queried 2026-06-03). Code grounding: §0 facts below, all verified.

## Patterns applied (pgmcp catalog, cited)
- **Template Method** (`template_method`, score 0.70) + **Form Template Method** (`form_template_method`,
  0.65): the governing structure. Each durable operation is an INVARIANT skeleton (the Order-A control
  flow: durability-gate → present-hoist → durable WAL append → publish via overlay CAS → mark committed)
  expressed as a trait DEFAULT method; the per-variant steps are deferred to abstract SEAM hooks. The
  char→shared extraction is exactly "Form Template Method": push the common skeleton up, defer the
  differing steps.
- **Strategy** (`strategy`, 0.64): the per-variant seam hooks are injected behavior (WAL-record builder,
  serializer, value-domain bound) — interchangeable policy behind one interface.
- **RCU** (`rcu`, 0.63) + **CAS Loop** (`cas_loop`, 0.63) + **Wait-Free/lock-free** (`wait_free`): the
  substrate the architecture MUST PRESERVE — readers lock-free over the arc-swap root, writers publish
  immutable new versions via root CAS, EBR reclaims after a grace period. The shared traits take `&self`
  on reads and the publish path; NO seam introduces a hot-path lock.
- **Parallel Change / Branch by Abstraction** (`parallel_change`, 0.62): the migration — extract char onto
  the traits as a BEHAVIOR-IDENTICAL refactor (char correspondence suite is the oracle), byte impls the
  same traits, both coexist; expand → migrate → (eventually) contract.
- **Protected Variations** (`protected_variations_grasp`) / **Encapsulate What Varies**
  (`encapsulate_what_varies`) / **Open–Closed** (`open_closed_principle`) / **DRY** (`dry`): wrap the
  predicted variation (key encoding, WAL record shape, on-disk format, value domain) behind the stable
  trait; a NEW variant = a new impl (open for extension), the shared control flow is unedited (closed for
  modification); each data-loss-critical rule has ONE representation.

## Anti-patterns explicitly avoided (pgmcp catalog, the guardrails)
- **God Object** (`god_object`, 0.61): do NOT pile read + write + checkpoint + recovery into one mega-
  trait. SPLIT into cohesive traits (one responsibility each — below). `LockFreeOverlay` (read+flip) is
  already its own; `DurableOverlayWrite` and `OverlayCheckpoint` are separate, composable traits.
- **Speculative Generality** (`speculative_generality_ap`, 0.58): the "remain sensible" limit. Design for
  the TWO real variants (char/byte) + clean extensibility; do NOT add hooks for hypothetical variants that
  may never exist. The vocab variant is DELIBERATELY excluded (its allocator-index overlay doesn't fit —
  proven). Seams exist only where char and byte ACTUALLY diverge.
- **Lock Convoy / Priority Inversion** (`lock_convoy` 0.59 / `priority_inversion` 0.58): the non-blocking
  mandate. No seam may take a mutex on the read or write hot path; the only synchronization is the
  arc-swap root CAS + EBR + the existing per-WAL append lock (already there, off the read path).
- Code smells watched: **Long Parameter List** (seam methods stay narrow — pass `&[K::Unit]` + the value,
  not a bag of fields), **Primitive Obsession** (use `RankRegime`/`Lsn`/typed records, not bare ints),
  **Feature Envy** (the owned-read seams live on the variant that owns `self.root`).

## §0 Verified code facts (the generic-vs-per-variant boundary)
ALREADY SHARED in `persistent_artrie_core` (byte just reuses — DRY already won here):
- `wal::header::RankRegime` (Owned/Overlay) — the regime enum.
- `wal::codec::WalRecord` — the WAL record types (Insert/Increment/BatchIncrement/Checkpoint/…).
- `recovery::{reconcile_lww, reconcile_lww_with_regime, rebuild_from_wal_segments_regime_aware}` — the A2
  regime-aware LWW recovery (char's hardest-won machinery, already generic).
- `overlay::{OverlayNode<K,V>, AtomicNodePtr<K,V>}` + `overlay::flip::LockFreeOverlay<K,V,S>` (read engine
  + flip/route + reestablish folds — Step 1, done).
- `committed_watermark::CommittedWatermark` — **STILL IN CHAR** (`persistent_artrie_char/committed_watermark.rs`);
  K-agnostic (a contiguous-prefix LSN tracker) ⇒ MOVE to core (pure DRY win).

CHAR-CONCRETE (the extraction targets — push the skeleton up, keep the seam):
- The 5 Order-A durable writes `insert_cas_durable`/`remove_cas_durable`/`try_increment_cas_durable`/
  `insert_cas_with_value_durable`/`upsert_cas_durable` (char/lockfree_cas.rs:370/556/1654/1747/1862).
- The checkpoint route-split + `capture_snapshot_immutable` + the retaining publisher (char/persist.rs:109/383).
- `CheckpointSnapshot` (char/persist.rs) + the on-disk serializer — GENUINELY per-variant (char arena
  format vs byte arena format) ⇒ stays a seam.

BYTE GAPS (red-team `aec7447`): byte has NONE of the durable-write/checkpoint/watermark/regime machinery;
it has only Phase A/B (overlay node + enable_lockfree + NO-WAL CAS). So byte both REUSES the shared traits
AND must provide the per-variant seams (WAL builder, serializer, value bound) + consume the already-shared
A2 recovery.

## The trait family (cohesive split — avoid God Object)
1. **`LockFreeOverlay<K,V,S>`** (DONE, `overlay/flip.rs`): route predicate, the non-faulting RCU read
   engine, flip/kill-switch, the reestablish folds. Read + flip responsibility.
2. **`DurableOverlayWrite<K,V,S>: LockFreeOverlay<K,V,S>`** (NEW): the Order-A `*_cas_durable` skeletons as
   Template-Method defaults. SEAM hooks (Strategy): `durability_policy()`, `append_durable_wal(record) ->
   Lsn` (constructs+appends the variant's WalRecord, durable), `mark_committed(lsn)`, `value_bound(v) ->
   Result<CounterValue>` (the per-variant value-domain check — char u64 vs byte i64-non-negative, the C4
   guard), plus the already-present `overlay_publish_*` from trait 1. The skeleton owns: the durability-
   policy gate, the NON-FAULTING present-hoist (RCU read, never faulting — the 75-min-deadlock rule), the
   append-then-publish ORDER (Order-A: durable before visible), `mark_committed`. ONE copy.
3. **`OverlayCheckpoint<K,V,S>: LockFreeOverlay<K,V,S>`** (NEW): the checkpoint route-split skeleton —
   `if route_overlay() { capture_immutable + publish_retaining } else { assert!(!route_overlay()); owned }`
   — as a default. SEAM hooks: `capture_overlay_snapshot() -> Snapshot` (walk the overlay root → the
   variant's `CheckpointSnapshot`), `publish_retaining(snapshot, watermark)` (serialize + WAL-retain via
   the committed watermark), `capture_owned_snapshot()` (the owned arm). The watermark/retention LOGIC is
   shared (via the moved-to-core `CommittedWatermark`); only the serialize is a seam.
4. **Recovery**: NO new trait — byte THREADS the already-shared core `reconcile_lww_with_regime` +
   `rebuild_from_wal_segments_regime_aware` through its 3 replay sinks (read `header.regime()`, gate
   reestablish on `Overlay`). Reuse, not re-abstract (avoid Speculative Generality).

## Migration sequence (Parallel Change; each step green; reversible until byte's irreversible flip)
- **M0:** move `CommittedWatermark` char→core (pure relocation; char re-exports; suite green).
- **M1:** define `DurableOverlayWrite` + `OverlayCheckpoint` in core; extract char's 5 durable writes +
  checkpoint route-split into the trait defaults (Form Template Method, BYTE-IDENTICAL — char
  correspondence suite + full suite the oracle); char seam impl supplies the hooks. Reversible.
- **M2 (byte durable subsystem):** byte impls `DurableOverlayWrite` (CounterValue=i64, the i64 value-bound
  + negative-reject = C4) + `OverlayCheckpoint` (byte serializer seam = C2) + the watermark wiring (C3) +
  thread A2 regime recovery through byte's 3 sinks (H3). Each its own gate + a byte durability-witness
  test. Reversible (overlay stays opt-in; no create-flip yet).
- **M3 (byte read/write routing + guards):** byte public read routes (C6) + write routes/reject checklist
  (C5/H4) + the char-absent rejects (compact C7, iter_with_values C2-read) + uncapped owned enumerators
  (H1/H2) + byte correspondence tests. Reversible.
- **M4 (byte IRREVERSIBLE flip — owner per-flip GO):** create-flip on byte ctors + the reestablish sink
  after ALL recovery paths (the byte EDIT-3) + byte reestablish-survival test (incl. negative-count + >100k
  partition) + create-flip gate tests. Surfaced with the diff for GO.
- **Future variant:** implement the 3 traits' seams (WAL builder, serializer, value bound, owned readers)
  — zero control-flow copy. That is the scalability payoff (Open–Closed).

## Verification (each step)
Full `cargo nextest --features persistent-artrie` green + the char/byte correspondence suites + the D1
owned-seam grep gate (applied per-variant) + `verify-formal-correspondence.sh` exit 0 + 0 new unsafe. The
reestablish Rocq spec `OverlayReestablishSpec.v` is variant-agnostic (covers both). The #41/A2 TLA/loom
models are shared. M2's byte durable writes get a durability-witness test (durable write survives
reopen-without-checkpoint = the #41-closed witness, byte twin).

## Why this is excellent + reusable + scalable + DRY (the directive scorecard)
- **DRY:** one copy of Order-A, the checkpoint route-split, the watermark, the read engine, the reestablish
  fold, A2 recovery. Per-variant code = only the genuinely divergent seams (WAL builder, serializer, value
  bound, owned readers) ≈ the irreducible ~30%.
- **Reusable + scalable:** a new variant implements the seams; the data-loss-critical control flow is
  inherited + already proven. Open–Closed.
- **Non-blocking / max parallelism:** RCU substrate preserved end-to-end — lock-free reads, CAS-publish
  writes, EBR reclaim; no seam takes a hot-path lock (Lock-Convoy avoided).
- **Sensible (not over-abstracted):** seams only where char/byte actually diverge; vocab excluded; no
  hooks for imaginary variants (Speculative-Generality avoided); cohesive trait split (God-Object avoided).

## Design-review addendum (pgmcp `review_design_patterns`, 2026-06-03 — the flagged review questions, answered)
The catalog review (paradigms: concurrent/OO/parallel) returned NO blocker; it surfaced review questions,
each already handled by the existing RCU/immutable-node substrate (confirm-in-code, not change):
- **ABA Problem (0.56):** the root CAS is ABA-safe by Arc IDENTITY + EBR — a retired node's address cannot
  be reused while any reader is epoch-pinned, so a CAS cannot see A→B→A. The traits operate on the SAME
  `AtomicNodePtr`/EBR and inherit this; no new ABA surface.
- **Shared Mutable State (0.56):** avoided by design — overlay nodes are IMMUTABLE (RCU: writers publish
  new versions via root CAS, never mutate in place); owned state is `&mut self`-gated. No seam shares
  in-place-mutable state across threads.
- **Send/Sync Bounds (0.56, Rust):** `OverlayNode<K,V>`/`AtomicNodePtr<K,V>` are auto-`Send`/`Sync` (after
  the G4 unsafe removal); the traits add no raw pointers, so the bounds are preserved. The seam value-route
  uses SAFE `Any` (no `unsafe`).
- **Iterator Invalidation (0.54):** the reestablish folds SNAPSHOT owned terms to `Vec`s via the `owned_*`
  seams, THEN `clear_owned()` — no live iterator is held across the `&mut` clear (the clear-owned-LAST
  ordering already enforces this; the Rocq spec proves it).
- **Speculative Generality / Overabstraction (0.54):** reaffirmed — the 3 traits serve TWO REAL consumers
  (char already needs the durable layer; byte is the owner-chosen second). No trait/seam exists for a
  hypothetical variant; vocab is excluded by proof. If a seam ever has exactly one impl, delete it.
- **Switch Statements / Any-dispatch (0.54):** the `TypeId`/`Any` u64-vs-i64-vs-`()` dispatch is LOCALIZED
  to the value-route + reestablish-dispatch seams (~2–10 LOC each), not scattered — the Rust-idiomatic
  stand-in for the missing specialization, contained behind the seam (Encapsulate What Varies).
- **Let It Crash / fail-fast (0.54):** the data-loss-critical seams FAIL LOUD (value-bound reject, regime-
  stamp check, the D1 grep gate) rather than silently degrade — aligned.
