# Byte ARTrie overlay-flip — design (Step 2 of the genericization)

> ## ⛔ RED-TEAM VERDICT: NO-GO as written (agent `aec7447`, code-grounded against HEAD `1f120e8`)
>
> The design below treats the byte flip as "the char pattern via the trait + 3 hazards." **That is wrong.**
> Byte is missing the ENTIRE durable-overlay subsystem the char flip is built on. The flip is NOT a thin
> Step-2 impl — it requires first BUILDING byte's equivalent of char's Phases C/D/E (the durable-write
> layer, the checkpoint route-split, the watermark/retention, regime-aware recovery + A2). Byte today has
> only Phase A/B (the overlay node + `enable_lockfree` + NO-WAL CAS). Blocking defects:
> - **C1 (CRITICAL):** byte's `insert_cas`/`increment_cas`/`try_increment_cas` (lockfree_cas.rs:112/461/595)
>   are NO-WAL; byte has ZERO `*_cas_durable`. Routing production writes to them = total loss on reopen.
>   ⇒ must build a byte Order-A durable-overlay-write layer first.
> - **C2 (CRITICAL):** `persist_to_disk` reads `self.root` (serialize_impl.rs:112) + `term_count`; NO
>   overlay-capture route-split (char S5-9). First post-reestablish checkpoint persists the EMPTY owned tree
>   + authorizes WAL truncation = total loss. ⇒ must build a byte `capture_snapshot_immutable` + route-split.
> - **C3 (CRITICAL):** no WAL-retention/commit_seq floor/watermark (0 byte matches). Checkpoint truncates
>   below the overlay frontier. ⇒ must port the retaining publisher + floor.
> - **H3 (HIGH):** no regime-aware recovery / `reconcile_lww` / A2 (0 byte matches); dumb in-order replay.
>   Once durable writers emit CommitRank, recovery resurrects orphans. ⇒ must thread `rank_regime` +
>   `reconcile_lww` through byte's 3 sinks BEFORE the durable writers land.
> - **C4 (CRITICAL):** byte i64 counters CAN be negative (`increment_bytes`/`fetch_add`/recovery take
>   `delta: i64`); the seam's `v as u64` wraps a negative → `increment_cas` PANICS. ⇒ reject negatives on
>   every overlay-write + reestablish path; use `try_increment_cas` (never the panicking `increment_cas`).
> - **C5/C6 (CRITICAL):** missing write routes (6 `insert_batch*`, bare `remove_prefix`, `fetch_add`, ALL
>   `SharedARTrie`/`Dictionary`/`MappedDictionary` writers) + missing read routes (the trait impls +
>   `get_value_bytes`/`contains_bytes` + `Dictionary::len`→`term_count` + `root()`/zipper DEFER+warn). The
>   real byte public surface is the trait impls + `*_bytes` wrappers, NOT char-shaped inherent methods.
> - **H1/H2 (HIGH):** the owned_* seam needs UN-ROUTED, UNCAPPED enumerators; byte's only enumerators
>   (`arena_iter.rs:354/497`) get routed in B1 AND cap at 100k (silent reestablish truncation).
> - **H4 (HIGH):** byte has NO overlay remove primitive ⇒ `remove`/`remove_prefix*` MUST reject (not route).
>
> Confirmed NON-defects: the `overlay_write_mode` field is serialization-benign (the struct isn't
> serialized, only `self.root`); the i64=CounterValue TypeId identity is sound (read-back lossless); the 3
> recovery sinks are correctly enumerated.
>
> **CONSEQUENCE:** the byte flip is a major, multi-phase, IRREVERSIBLE effort comparable to the char
> Phases C–E (durable subsystem + watermark + A2 + formal verification), NOT the thin impl this doc
> assumed. The §1–§7 below are SUPERSEDED as a standalone plan — they are the *flip-layer* half (the ~45%
> the trait already provides); the *durable-subsystem* half (C1/C2/C3/H3) must be designed + built +
> red-teamed + formally verified first, per-variant. Full verdict + defect list: the red-team result in
> the session transcript; corrected-items list reproduced at the end of this doc.
>
> **The trait EXTRACTION (Step 1, `1f120e8`) is unaffected + complete** — char is on the shared trait, and
> the generic flip-layer (read engine, route, flip/kill, reestablish fold, value-route) is ready for byte
> to reuse ONCE byte's durable subsystem exists.

---


**Crate `libdictenstein`, byte `src/persistent_artrie/`. Baseline HEAD `1f120e8` (char on the shared
`LockFreeOverlay<K,V,S>` trait). DATA-LOSS-CRITICAL, IRREVERSIBLE at the write/regime layer.** Inputs:
`docs/design/byte-flip-reachability-audit.md` (the hazard set), `docs/design/overlay-flip-genericization.md`
(§5 Step 2), and the char precedent (`docs/design/s5-12-e1-readflip-design.md`). The byte flip = the char
flip applied via the trait, V ∈ {(), i64}, plus 3 char-absent hazards.

## 0. Sequence — reversible foundation, THEN the gated irreversible flip (char's proven path)
- **Phase B1 (REVERSIBLE):** the byte seam impl + `overlay_write_mode` field + owned readers + public
  read routing + the 10 reject/route guards + byte correspondence tests driven by explicit
  `enable_lockfree`+`set_overlay_write_mode(LockFreeOverlay)` (like char's pre-flip tests). NO ctor change.
  Gateable, reversible.
- **Phase B2 (IRREVERSIBLE — owner per-flip GO, like char's E2):** the create-flip on byte's ctors +
  the open-reestablish sink (the byte EDIT-3) + the byte reestablish-survival test. Surfaced with the diff
  for GO before landing.

## 1. The seam impl `impl LockFreeOverlay<ByteKey, V, S> for PersistentARTrie<V, S>`
`type CounterValue = i64` (the byte counter trie is `PersistentARTrie<i64>`). The i64↔u64 conversion lives
in the publisher/getter seams (byte's overlay primitives speak u64; the counter is ≥0, bounded by
LOCKFREE_COUNTER_MAX = i64::MAX, so both directions are lossless):
- `overlay_publish_counter(units, v: i64)` → `self.increment_cas(units, v as u64)` (no-WAL publisher).
- `overlay_counter_get(units) -> Option<i64>` → `self.get_lockfree(units).map(|u| u as i64)`.
- `overlay_publish_membership(units)` → `self.insert_cas(units)`.
- `overlay_contains(units)` → `self.contains_lockfree(units)`.
- `overlay_eligible_v()` → `TypeId::<V>()==()` || `==i64`.
- `lockfree_root`/`overlay_write_mode`/`set_overlay_write_mode`/`enable_lockfree` → field accessors (+ the
  new field).
- `wal_current_lsn`/`wal_is_overlay_regime`/`wal_stamp_overlay_regime`/`wal_stamp_owned_regime` → byte's
  `wal_writer: Option<Arc<AsyncWalWriter>>` (same AsyncWalWriter API char uses).
- **UN-ROUTED owned readers** (D1 — read `self.root` via byte's `_impl`, NEVER the routed public reads):
  `owned_first_units`/`owned_units_under`/`owned_units_with_values_under`/`owned_has_empty_term_value`
  built over byte's owned enumerators (`iter_prefix_with_values_and_arena`'s OWNED body / `get_value_impl`);
  `clear_owned` → `self.root = TrieRoot::empty(); self.term_count.store(0)`. Convert `Vec<u8>`↔`Vec<u8>`
  units via `ByteKey::units_from_str`/`units_to_term` (identity for byte). **Because byte's public
  `iter_prefix`/`get` will be routed in B1, the owned readers must call byte's `_impl`/owned-body
  enumerators directly — extract `owned_iter_prefix`-equivalents if byte lacks un-routed enumerator entry
  points** (byte's `arena_iter`/`cursor_iter` bodies that `match &self.root` are the un-routed source).

## 2. The `overlay_write_mode` field + ctors (B1, reversible)
Add `pub(crate) overlay_write_mode: OverlayWriteMode` to `PersistentARTrie` (dict_impl.rs:264), default
`OwnedTree` (inert). Initialize it in EVERY byte ctor (mmap_ctor + io_uring_ctor + any `Default`/test
ctor) — `OverlayWriteMode::default()`. This changes NO behavior (inert default), so the suite stays green.

## 3. Public read routing (B1, reversible — the E1 equivalent for byte)
Mirror char's E1, adapted to byte's API shapes:
- `contains`/`try_contains` → `if route_overlay() { overlay_contains via units } else { owned _impl }`.
- `get`/`try_get` (&V) → return None under overlay (no borrowable overlay value); callers use `get_value`.
- `get_value` (owned Option<V>) → `if route_overlay() { overlay_route_get_value (trait) } else { owned }`.
- `len`/`is_empty`/`term_count` → `if route_overlay() { overlay_len/overlay_is_empty (trait) } else { owned }`.
- The ITER FAMILY — route at the public TOP (audit §D): `iter`/`iter_prefix`/`iter_prefix_with_arena`/
  `iter_prefix_from_cursor` → `overlay_collect_units` (trait) mapped via `ByteKey::units_to_term`
  (`arena_id: None` for the arena variants, like char). `iter_with_values`/`iter_prefix_with_values`/
  `iter_prefix_with_values_and_arena` → `overlay_collect_units_with_values` (trait) — the VALUE-CARRYING
  route (audit §C.2: NOT enumerate-overlay-then-value-owned).
- Extract un-routed `owned_*` bodies for each so reestablish + the false-arms read owned.

## 4. The 10 reject/route guards (B1, reversible — audit §B + §C)
`if self.route_overlay() { return Err(InvalidOperation("... not valid under the lock-free overlay ...")) }`
at the top of: `merge_from`, `merge_from_batched_with_options`, `merge_from_parallel`,
`merge_lockfree_values_to_persistent`, `merge_lockfree_to_persistent`, `begin_document`+`commit_document`,
`remove_prefix_batched`, **`compact()` (the P0 char-absent file-replacer)**. Plus the write-flip routes
(audit §D): `increment_bytes`/`upsert_bytes`/`get_or_insert_bytes`/`insert`/`remove` get
`if route_overlay() { <overlay CAS> } else { owned }`; `compare_and_swap_bytes` REJECTS under overlay
(no byte overlay CAS-with-expected exists). The recovery RMW `recompute_recovered_increment` stays on
`get_value_impl` (protect, do not change).

## 5. Phase B2 (IRREVERSIBLE) — create-flip + the reestablish sink
- Create-flip: each byte create ctor calls `apply_create_flip` (the byte twin: `if overlay_eligible_v() &&
  !flip_to_overlay() { return Err(internal) }`).
- Open-reestablish (the byte EDIT-3, audit §C.3 — byte has NO reestablish today): after all 3 recovery
  replay loops (mmap_ctor.rs:436/650, io_uring_ctor.rs:256) and the normal open replay, add
  `if rank_regime==Overlay && overlay_eligible_v() { flip_to_overlay(); reestablish_overlay_dispatch()? }`.
  Byte needs a `reestablish_overlay_dispatch` (the byte twin: TypeId i64→`reestablish_overlay_counter`,
  ()→`reestablish_overlay_membership` — both are now TRAIT DEFAULTS; the dispatch is the ~10-LOC seam that
  Any-downcasts to `<ByteKey,i64,S>`).
- Open-3-cases + corruption-arm + recover_from_archives: same regime-gated reestablish (the byte EDIT-2/3).

## 6. Gate (each phase) + the data-loss red-team
- B1 gate: full `cargo nextest --features persistent-artrie` green + NEW byte correspondence tests
  (overlay==owned for len/contains/get_value/iter_prefix*, None-vs-Some(empty), deep-key) + the D1 seam
  grep on the byte impl + `verify-formal-correspondence.sh` exit 0 + 0 new unsafe + the 10 reject-guard
  tests.
- B2 gate: + a byte reestablish-survival test (build owned, checkpoint, reopen→reestablish, assert EVERY
  term+value survives) + the create-flip gate tests (create→write→reopen, old-owned-file-stays-owned,
  compact-rejects-under-overlay) + the byte red-team cleared. Then the irreversible commit (diff shown).
- The reestablish Rocq spec `OverlayReestablishSpec.v` is variant-agnostic ⇒ byte inherits the D1 guard
  (a one-line doc note, no new proof).

## 7. Top risks (for the red-team)
1. The reestablish SINK GAP (§C.3) — if B2 forgets the post-recovery reestablish, the first checkpoint
   persists the empty overlay = total loss. The reestablish-survival test is the guard.
2. `compact()` file-replacement (§C.1) — if not rejected, atomic-renames a values-lost image over the
   durable file. The compact-rejects test is the guard.
3. `iter_with_values` mixed-read (§C.2) — must use the value-carrying overlay route, not
   enumerate-then-owned-value. The correspondence test pins it.
4. The i64↔u64 CounterValue conversion — verify the counter domain is ≥0 (LOCKFREE_COUNTER_MAX) so both
   directions are lossless; the correspondence test reads back i64 counters.
5. The write-flip route checklist (§D) — any missed route = silent under-count. Each owned read in a
   write method is a tripwire; the reject-guard/route tests + the correspondence suite catch a miss.

## 8. CORRECTED build order (post-red-team — the durable subsystem MUST come first)

The flip-layer (§1–§7, the trait-provided ~45%) is the LAST step. Before it, build byte's durable
subsystem (the char Phases C–E equivalents), each its own gated + red-teamed + (where char was) formally
verified phase:
1. **Byte Order-A durable-overlay-write layer** — `try_increment_cas_durable`/`insert_cas_durable`/
   `upsert_cas_durable`/`remove_cas_durable` (durable WAL append + CommitRank, bound-before-log, THEN
   publish). The i64-domain bound + negative-reject live here (C4/M3).
2. **Byte checkpoint overlay-capture route-split** (S5-9 twin) — a byte `capture_snapshot_immutable` over
   `lockfree_root.load()`; under `route_overlay()` serialize the overlay, owned arm asserts
   `!route_overlay()` (C2).
3. **WAL-retaining publisher + commit_seq floor/watermark** (C3) — or a proof the byte overlay checkpoint
   image is self-sufficient (char needed the floor; default to porting it).
4. **Regime-aware recovery + A2** — read `header.regime()` at the 3 sinks, replay via `reconcile_lww`,
   gate reestablish on `rank_regime == Overlay` (H3). MUST precede #1's CommitRank-emitting writers.
5. **The complete write-route + reject checklist** — the 6 batch inserts, bare `remove_prefix`,
   `fetch_add`, all `SharedARTrie`/`Dictionary`/`MappedDictionary` writers; remove paths REJECT (no byte
   overlay remove primitive); `compact()` rejects (M1, confirmed safe — no internal caller) (C5/H4).
6. **The complete read-route checklist** — trait impls + `get_value_bytes`/`contains_bytes` +
   `Dictionary::len`→`term_count` + `iter_with_values` value-carrying route + `root()`/zipper DEFER+warn
   (C6/M2).
7. **Uncapped un-routed owned enumerators** for the owned_* seams (no 100k truncation) under the §6(a)
   grep gate applied to the byte impl (H1/H2).
8. **THEN** the flip-layer (§1–§5 here) + the create-flip/reestablish-sink (B2, irreversible, owner GO).
9. **Tests:** a byte reestablish-survival test WITH a negative-count term + a >100k-term first-byte
   partition + a checkpoint→reopen→checkpoint→reopen cycle (catches C2/C3); the byte twin of the char gate.

Each of 1–4 is a data-loss-critical, mostly-irreversible subsystem of its own; this is the char Phase C–E
effort replicated for byte (the trait only saved the flip-layer half). Scope accordingly.
