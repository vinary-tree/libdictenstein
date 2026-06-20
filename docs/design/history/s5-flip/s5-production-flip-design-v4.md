# S5 v4 — Corrected Minimal Recovery Plan (live-paths-only) + Reachability Red-Team

**Crate `libdictenstein`, char ARTrie. 2026-06-03. DESIGN ONLY — NO code edited.** Baseline: committed
reversible core S0–S4 (HEAD `04f6a46`). Supersedes the §4 thread of
`s5-production-flip-design{,-v2,-v3}.md`. The entire v1→v2→v3 §4 thread rested on the premise "the char
`RecoveryManager` is on the live recovery path." That premise is **REFUTED** (v3 §9, re-verified here).
This doc re-derives the recovery plan against **only call-graph-proven-reachable code**, with `file:line`
for every reachability claim, and red-teams every remaining S5 item for the same class of error.

> Produced by a background Plan agent (read-only) and persisted by the parent. The A2 headline below was
> subsequently re-verified against source by the parent (see the verification appendix / commit log).

## 0. Verdict summary

| # | Question | Verdict |
|---|---|---|
| **A2 hole** | Is the LIVE archive-rebuild regime-aware? | **NO — REGIME-BLIND on BOTH live rebuild paths.** The real remaining §4 work (§2). Inert while Owned; bites only post-flip. |
| **S5-1** | char `RecoveryManager` / `get_checkpoint_lsn` fix | **MOOT** (dormant, test-only). Drop from the flip gate. |
| **S5-3 (old)** | "rewrite char `RecoveryManager::rebuild_from_wal`" | **MOOT target**; re-point at the LIVE rebuilds (§1). |
| **S5-6 producers** | increment/fetch_add/insert_batch_bytes guards | **LIVE** — all reach `append_to_wal` unranked. Keep. |
| **S5-7 merge reject** | `merge_lockfree_values_to_persistent` | **LIVE (public surface)** — appends unranked BatchIncrement. Keep. |
| **begin_document reject** | `begin_document` | **LIVE** — appends unranked BeginTx, no route guard. Keep (severity downgraded, §RES-2). |
| **S5-9 cfg un-gate** | overlay-checkpoint fns | **DORMANT-today but a genuine flip PREREQUISITE** (production `checkpoint()` reads the OWNED tree only). Keep, re-scoped. |
| **S5-12 flip gate** | owner-GO-only irreversible flip | **PRESERVED** unchanged. |

## 1. The A2-hole verdict (REGIME-BLIND, confirmed live)

When the WAL header magic is `MAGIC_OVERLAY`, both live rebuilds RESURRECT unranked records (they never
read the header regime, never call `reconcile_lww`, replay every record in raw `(segment, lsn)` order).

### 1.1 The regime-AWARE baseline (normal reopen) — for contrast
- `mmap_ctor.rs:435-439` (`open`): `rank_regime = WalReader::read_header(&wal_path).map(|h| h.rank_regime()).unwrap_or(Owned)` → `inner.replay_records_lww(recovered_ops, loaded_from_disk, checkpoint_lsn, rank_regime)`.
- `mmap_ctor.rs:655-658` (`open_with_depth`), `io_uring_ctor.rs:248-252` (`open`): same.
- Flow → `replay_records_lww` (mutation_core.rs:252) → `reconcile_lww(…, rank_regime)` (recovery.rs:257), recovery.rs:299-305: `None => match rank_regime { Owned => lsn, Overlay => continue }` (Overlay DROPS unranked).

### 1.2 LIVE regime-blind path #1 — `recover_from_archives`
`recover_from_archives` (mmap_ctor.rs:1167, `pub`) at :1198-1208 calls
`crate::persistent_artrie::recovery::rebuild_from_wal_segments(&segments, |op| …apply_core_recovered_operation_no_wal(op)…)`.
Core `rebuild_from_wal_segments` (recovery.rs:1469-1513) is `(segments: &[PathBuf], apply_fn: F)` — **NO
`rank_regime` param**; body applies every `recovered_operations_from_record(lsn, record)` in raw order.
No header read, no rank map, no Overlay drop ⟹ post-flip Overlay archive rebuild RESURRECTS every
dropped-at-merge / two-append-window orphan.

### 1.3 LIVE regime-blind path #2 — `open_with_recovery_config` corruption branch
`open_with_recovery_config` (mmap_ctor.rs:790, `pub`) corruption arm (:816-996) has its OWN inline
`'segments:` replay loop (:855-982) matching `WalRecord::{Insert,Remove,Increment,Upsert,CompareAndSwap,
BatchInsert,BatchIncrement}` → `insert_impl_no_wal_with_value` / `try_increment_impl_no_wal` etc. Also
regime-blind. Same resurrection bug.

### 1.4 Reachability (both production-live)
- `recover_from_archives` is `pub` on `PersistentARTrieChar` (in-tree callers tests only, but public surface).
- `open_with_recovery_config` ← `open_with_recovery` (mmap_ctor.rs:726, `ArtrieDict`/`SharedCharARTrie`
  trait method mod.rs:1224-1231) and `open_with_full_recovery` (mmap_ctor.rs:1039, `pub`, :1087).
- Corruption DETECTION uses the byte module's `detect_corruption` (mmap_ctor.rs:795/:1044) — correct
  "PART" read; only the subsequent REBUILD is regime-blind.

**∴ A2 is a REAL, post-flip, LIVE data-loss bug (resurrection after a corruption-triggered rebuild).**
Inert while every file is Owned (Owned-arm == in-order replay). It is the real remaining §4 work and is
NOT the dormant char `RecoveryManager`.

### 1.5 The fix (minimal, reuses existing machinery)
Per-segment regime is readable: `WalReader::read_header(seg).rank_regime()` → `RankRegime::from_u8`
(header.rs:165). `reconcile_lww` already drops unranked under Overlay. So:
- Generalize `reconcile_lww` to a **per-record/per-segment regime** (`regime_of: impl Fn(Lsn)->RankRegime`,
  or collect all segment records tagged with each segment's regime into one global `(generation,lsn)`-sorted
  pass; LSNs globally monotone across rotate — `rotate_to_archive` carries `next_lsn` HIGH + floor + regime,
  writer.rs:524-528; only `truncate` resets to 1).
- `recover_from_archives`: replace the raw `rebuild_from_wal_segments` with read-each-segment-regime →
  tagged record vec → `reconcile_lww(records, loaded_from_disk=false, checkpoint_lsn=0, regime_of)` → apply
  winners via `apply_core_recovered_operation_no_wal`. (`checkpoint_lsn=0` because it deletes the base image
  first, mmap_ctor.rs:1191-1193.)
- `open_with_recovery_config` corruption branch: replace the inline `'segments:` loop (:855-982) with the
  SAME regime-aware reconcile + applier (also DRYs two hand-rolled loops into the shared path).
- **Remove NEVER dropped under Overlay** (defense-in-depth).
- **Invariant preserved:** Overlay-drops-unranked now holds on the rebuild paths identically to normal open.
  No on-disk format touch; no Order-A / committed-watermark relaxation.

## 2. MOOT items to DROP — dormant-targeting
Char `RecoveryManager::new` constructed ONLY at recovery.rs:814 inside `#[cfg(test)]`. Live char ctors use
the **byte** `detect_corruption`. "ARTC" `CharTrieFileHeader` never written in production.

| Item | Old target | Reachability | Disposition |
|---|---|---|---|
| **S5-1** | char `get_checkpoint_lsn` byte-misread + `latest_checkpoint_lsn_from_wal` helper | DORMANT | **DROP from flip gate.** Live checkpoint_lsn source (WAL `Checkpoint` record, mmap_ctor.rs:318-323) already correct. |
| **S5-3 (char half)** | char `RecoveryManager::rebuild_from_wal` (recovery.rs:503) | DORMANT | **DROP**; re-point intent at the LIVE rebuilds (§1.5). The core `rebuild_from_wal_segments` half is LIVE = the real work. |
| `replay_wal_after_checkpoint` (char recovery.rs:464) | — | DORMANT | **DROP.** |
| char `detect_corruption` (recovery.rs:210) | — | DORMANT (byte twin is live) | **No action.** |

**Net:** the entire v3 §0–§1 `FileHeader`/`CharTrieFileHeader`/bytes-24..32/bytes-56..64 analysis is moot;
RB-1/RB-2/RB-6/RB-7 evaporate.

## 3. Corrected minimal reversible plan S5-1…S5-11 (live-only)
Each names its production entry point. Land all GREEN before owner GO; only S5-12 is irreversible.

- **S5-1 (A2, the new headline).** Entry: `recover_from_archives` (mmap_ctor.rs:1167) + `open_with_recovery`/
  `open_with_full_recovery`→`open_with_recovery_config` corruption arm (mmap_ctor.rs:790). Implement Fix-A2
  (§1.5): per-record/per-segment-regime `reconcile_lww`; route BOTH live rebuilds through it; Remove-never-
  dropped; break-glass `--feature` fail-closed on any Overlay segment. **Inert while Owned.** REPLACES old
  S5-1 + the live half of old S5-3.
- **S5-2 (A3 floor).** Entry: `checkpoint()`→`publish_durable_and_reclaim` (persist.rs:108). Populate
  `commit_seq_floor` at checkpoint (`set_commit_seq_floor(commit_seq@capture)`, monotone, carried across
  rotate writer.rs:524). Inert (floor read at open mmap_ctor.rs:339-342, currently 0).
- **S5-3 (flip emptiness predicate, was S5-4).** Entry: `WalWriter::set_overlay_regime` (writer.rs:373).
  Add `is_empty_after_header()` (len == `WalHeader::SIZE`); `set_overlay_regime` RETURNS `Err` if not empty
  (today it unconditionally restamps — contract is doc-only, writer.rs:366-372); flip caller asserts; post-
  stamp `assert!(rank_regime()==Overlay)`.
- **S5-4 (`set_owned_regime`, was S5-5).** Length-guarded inverse for the kill-switch (post-assert Owned).
- **S5-5 (producer guards, was S5-6).** Three VERIFIED-LIVE u64+Overlay holes:
  - `increment` (atomic_ops.rs:39): `route_increment` returns `None` for `delta<0` (lockfree_value_route.rs:114-118)
    ⟹ falls through to owned `append_to_wal(Increment)` UNRANKED (atomic_ops.rs:88). FIX: under `route_overlay()`,
    a `None` route ⟹ `Err`, not fall-through. `fetch_add` (atomic_ops.rs:247-250) delegates ⟹ inherits.
  - `insert_batch_bytes` (batch_insert.rs:148): `append_to_wal(BatchInsert)` (:185-186) with NO route guard;
    `_sorted` (:289)/`_grouped` (:388) delegate (:302/:405) ⟹ covered. FIX: overlay prologue (route or reject).
  - `enable_lockfree` refuses Overlay stamp for `V ∉ {(),u64}` (TypeId). See §RES-1 for the full producer set.
- **S5-6 (merge reject, was S5-7).** Entry: `merge_lockfree_values_to_persistent` (lockfree_cas.rs:2002)
  appends UNRANKED `BatchIncrement` (:2029) then drains to owned; `merge_lockfree_to_persistent`
  (lockfree_cas.rs:1115) analogous. FIX: hard-`Err` under Overlay (post-flip every increment is already
  per-op durable+visible; a drain double-counts).
- **S5-7 (begin_document reject).** Entry: `begin_document` (document_tx.rs:28) appends `BeginTx` (:40) with
  NO route guard. FIX: `Err` under `route_overlay()` (symmetry with `commit_document`). Severity downgraded
  (§RES-2).
- **S5-8 (promote asserts).** `debug_assert*`→`assert*` at persist.rs:140 (next_lsn-unchanged #41 guard), the
  watermark≤synced_frontier guard, the moved `lockfree_root.is_none` owned-arm assert. Entry:
  `publish_durable_and_reclaim` (persist.rs:108). Order-A ⟹ no spurious panic.
- **S5-9 (cfg un-gate + checkpoint route-split).** Entry: `checkpoint()` (persist.rs:86). Production
  `checkpoint()`→`capture_snapshot()` (persist.rs:255) serializes from `self.root` (OWNED tree) ONLY — no
  overlay branch. Post-flip the owned tree is cleared ⟹ would checkpoint an EMPTY tree. The overlay-checkpoint
  fns (`capture_snapshot_immutable` persist.rs:343, `publish_immutable_snapshot_retaining_wal[_with_eviction]`
  :548/:655, `overlay_to_inner` :1143, `count_overlay_finals` :1251) are `#[cfg(any(test, feature="bench-internals"))]`,
  reached only from bench/tests ⟹ DORMANT today. S5-9 = (a) remove the cfg gate; (b) route-split `checkpoint()`
  so Overlay captures from the immutable overlay, Owned keeps `capture_snapshot()`. A genuine flip
  PREREQUISITE (the flip is incorrect without it), NOT a live-bug-fix, labeled dormant-until-wired. Re-run
  unsafe-inventory gate (no new `unsafe`).
- **S5-10 (V1+V4 overlay reestablish + flip plumbing).** Entry: BOTH ctors' `open` (gated on Overlay regime)
  + `flip_to_overlay`/`kill_switch_to_owned`. Streaming-fallible `reestablish_overlay_after_recovery` (v3 §3:
  `iter_prefix_with_values(prefix)?` per first-code-point partition, insert→drop, abort-on-`Err`, clear-owned-LAST),
  `insert_cas_with_value_nodurable` (build_value_path + root CAS, zero `append_*`/fsync). **§9 non-faulting-first
  remove pre-flight** (`remove_cas_durable` lockfree_cas.rs:553 tries `find_leaf_lockfree` first; N-S4-3 soak)
  folds in here.
- **S5-11 (gate tests).** §4 below.

### 3.1 S5-12 — THE FLIP (IRREVERSIBLE, ~6 lines) — PRESERVED
The `V ∈ {(),u64}` ctors call `flip_to_overlay` (create) / `reestablish` handles open. Arbitrary-V
UNCHANGED. **Owner GO only**, consumed BETWEEN a full green gate and committing S5-12. Irreversibility
boundary = existence of any Overlay archive segment (v2 RA-10).

## 4. Gate additions (S5-11)
- **A2 regime-aware rebuild (NEW PRIMARY GATE):** build an Overlay trie with a known two-append-window
  orphan (unranked tail) + a ranked survivor; force EACH live rebuild (`recover_from_archives` AND
  `open_with_recovery_config` corruption arm); assert orphan DROPPED, survivor KEPT (== normal Overlay
  reopen). Mixed-regime archive (Owned seg + Overlay seg): Owned-unranked KEPT, Overlay-unranked DROPPED,
  Remove never dropped.
- **Inertness:** on an Owned file the new reconcile path is byte-for-byte the old raw replay.
- Plus prior gates: V1 reopen→write→reopen; H1 negative-increment + N1 batch_bytes (+_sorted/_grouped/
  _arena_grouped); V2 merge-on-overlay; H3 flip-after-checkpoint; kill-switch round-trip + crash-injection;
  flip-then-crash-at-each-step; N-S4-3 lock-order soak (MANDATORY); streaming-rebuild equivalence + memory
  bound; I/O-error-abort (owned NOT cleared); loom + FULL TLA (NoLostWrite holds, _Unsafe controls FAIL);
  recovery+char suites; `verify-formal-correspondence.sh`.

## 5. Residual unverified-reachability assumptions for the FINAL red-team (§RES)
Each is a reachability/dormancy claim NOT yet exhaustively call-graph-proven — the exact error class that
wasted §4. Red-team MUST close each.
- **RES-1 (producer enumeration completeness).** S5-5 covers `increment`/`fetch_add`/`insert_batch_bytes
  (+_sorted/_grouped)`. NOT yet proven: `insert_batch_arena_grouped` (batch_insert.rs:412), the `String`-keyed
  `insert_batch`/`_sorted`/`_grouped` (:18/:231/:325), `insert_batch_chars*` (:119/:258/:353), and ANY other
  `append_to_wal(`/`append_batch(` caller in the 4 routing files route-or-reject under Overlay. ACTION: re-run
  the v2 lexical grep gate + enumerate ALL public batch entry points.
- **RES-2 (begin_document watermark-stall is real).** Prove `BeginTx` advances `next_lsn` without advancing
  the committed-watermark, AND that checkpoint reclaim keys on the watermark (persist.rs:140-153 uses
  `next_lsn` for `checkpoint_lsn`). If reclaim keys on `next_lsn`, the stall claim is wrong and S5-7 is
  hygiene not correctness. (LOW either way — it's a reject.)
- **RES-3 (recover_from_archives base-image semantics).** Fix-A2 passes `checkpoint_lsn=0` because it deletes
  the base image (:1191-1193). Confirm NO caller relies on a retained base image, and "delete base, rebuild
  from archives only" is self-complete (first-LSN==1, no interior gap) — else a pruned-archive rebuild drops
  the pre-archive prefix. Verify vs `collect_retained_wal_segments_for_rebuild` (recovery.rs:1378) +
  `find_wal_archive_segments` (:1250).
- **RES-4 (S5-9 flip actually routes checkpoint to the immutable path).** Prove the route-split predicate is
  consulted INSIDE `checkpoint()` post-flip, and `capture_snapshot_immutable`'s on-disk image is
  equivalent-by-construction to owned `capture_snapshot` at production scale. (LIVE risk: post-flip checkpoint
  of an empty owned tree ⟹ total loss on next reopen.)
- **RES-5 (io_uring archive rebuild).** Only the **mmap** ctor's two rebuilds were traced. Grep
  `io_uring_ctor.rs` for `rebuild_from_wal_segments`/`recover_from_archives`/a corruption arm; if present,
  apply Fix-A2 there too. (LIVE risk: same A2 resurrection on the io_uring backend.)
- **RES-6 (streaming first-unit partition is a disjoint cover).** v3 RB-3 — prove no missed/double term at
  chunk boundaries; empty term handled.
- **RES-7 (mid-rebuild abort leaves owned-consistent trie).** v3 RB-4 — ctor propagates `Err`, publishes no
  partial overlay, owned NOT cleared.
- **RES-8 (un-gating compiles in `--no-default-features`).** v3 RB-5 — the 5 overlay-checkpoint fns add zero
  new `unsafe` and compile with no features.

**Explicitly NOT relaxed (data-loss invariants):** Order-A WAL-before-CAS; committed-watermark /
WAL-`Checkpoint`-record `checkpoint_lsn` (mmap_ctor.rs:318-323); Overlay-drops-unranked (now enforced on the
rebuild paths too, §1.5). None of S5-1…S5-12 weakens any of these.

## 6. Process lesson
For any recovery/format concern: FIRST prove the suspect code is reachable from a production entry point via
call-graph, THEN design the fix. v4 attaches a `file:line` reachability proof to every retained item and an
explicit RES-row to every still-unproven assumption.

## 7. Critical files for implementation
- `src/persistent_artrie_core/recovery.rs` — A2 fix: `reconcile_lww`:257 per-record regime;
  `rebuild_from_wal_segments`:1469 regime-aware; `recovered_operations_from_record`:324.
- `src/persistent_artrie_char/mmap_ctor.rs` — both live regime-blind rebuilds: `recover_from_archives`:1167,
  `open_with_recovery_config`:790 inline loop :855-982; normal-path regime threading :435 to mirror.
- `src/persistent_artrie_char/mutation_core.rs` — `replay_records_lww`:252 + `apply_core_recovered_operation_no_wal`
  (the shared applier the fixed rebuilds reuse).
- `src/persistent_artrie_char/{atomic_ops,batch_insert,lockfree_cas,document_tx}.rs` — S5-5/6/7 live producer
  guards: `increment`:39, `fetch_add`:247, `insert_batch_bytes`:148, `merge_lockfree_values_to_persistent`:2002,
  `begin_document`:28.
- `src/persistent_artrie_char/persist.rs` (S5-9 route-split + cfg un-gate: `checkpoint`:86,
  `capture_snapshot`:255 owned-only, `capture_snapshot_immutable`:343 dormant) +
  `src/persistent_artrie_core/wal/writer.rs` (S5-3/4 length guard: `set_overlay_regime`:373, add
  `is_empty_after_header`/`set_owned_regime`).
