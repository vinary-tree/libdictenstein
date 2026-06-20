# S5 Implementation Plan — items 2, 9, 10, 11 (flip-plumbing edit list)

**Crate `libdictenstein`, char ARTrie. Plan-agent design, parent-persisted. Baseline HEAD `ac8f188`.**
Authoritative: `s5-production-flip-design-v4.md` (§3 list, §4 gates, §5 RES). S5-1/3/4/5/6/7/8 are
DONE+green+committed (NOT redesigned). **S5-12 (the irreversible flip) is OUT — owner-GO-only.**

Three invariants every edit preserves: **Order-A** (WAL append+sync DURABLE before the visibility CAS) ·
**committed-watermark = checkpoint_lsn** (overlay records `checkpoint_lsn = committed_watermark_at_capture`,
captured Acquire BEFORE the root load) · **Overlay-drops-unranked** (`reconcile_lww_with_regime`
recovery.rs:330-332).

## Verified ground truth (file:line)
- `route_overlay()` = `uses_overlay() && lockfree_root.is_some()` (overlay_write_mode.rs:71-73);
  `set_overlay_write_mode` (:83, pub(crate)).
- `checkpoint()` defined ONCE, generic `<V,S>` (persist.rs:86) → `capture_snapshot()` (owned, :260-323) →
  `publish_durable_and_reclaim()` (:108). **One route-split edit covers mmap+io_uring.**
- Overlay capture/publish (all `#[cfg(any(test, feature="bench-internals"))]`): `capture_snapshot_immutable`
  (:347, sets `committed_watermark_at_capture=Some`, watermark Acquire @408 BEFORE root load @425),
  `publish_immutable_snapshot_retaining_wal` (:557, records `checkpoint_lsn=watermark`, NO rotate),
  `_with_eviction` (:664), `overlay_to_inner` (:1152), `count_overlay_finals` (:1262). `inner_to_overlay`
  already un-gated (:1224). bench shims (:748/:770) stay gated.
- `CheckpointSnapshot.committed_watermark_at_capture: Option<u64>` (:38-70; owned=None @320, immutable=Some @509).
- ONLY `unsafe` in persist.rs = line 944 (`serialize_char_node_to_disk`, already un-gated/in-ledger). Un-gating
  the 5 fns adds 0 unsafe rows (RES-8). bench-internals=["io-uring-backend"], default=["parking_lot"].
- `commit_seq: AtomicU64`; open seeds `floor.max(scan-max-gen)` (mmap_ctor.rs:342). `WalWriter::set_commit_seq_floor`
  raise-only+fsync (writer.rs:343); `commit_seq_floor()` (:360); `rotate_to_archive` carries floor+regime (:571-577).
- Order-A durable template: `insert_cas_with_value_durable` (lockfree_cas.rs:1696), `upsert_cas_durable` (1811).
  `build_value_path_recursive` (1915, u64-only); `build_path_recursive` (826, generic V). Two impl blocks:
  `impl<V,S>` (138) vs `impl<u64,S>` (1371). `remove_cas_durable` (509) present-check is **faulting-FIRST** (553-562).
  `merge_lockfree_values_to_persistent` already rejects under overlay (2002, S5-6 done).
- A2 DONE: `reconcile_lww_with_regime` (recovery.rs:290), `rebuild_from_wal_segments_regime_aware` (1556, inert
  fast-path 1563-1570); `recover_from_archives` routed (mmap_ctor.rs:~1237). io_uring has NO rebuild/recover_from_archives.
- `iter_prefix_with_values(prefix) -> Result<Option<Vec<(String,V)>>>` where `V: Clone`, reads OWNED tree
  (prefix_api.rs:37; navigate_to_prefix prefix_helpers.rs:25). ⚠️ v3 §3's `iter_with_values()`/`mod.rs:625`
  swallow site DO NOT EXIST at HEAD — the real fallible source is `iter_prefix_with_values` (materializes a Vec
  per call; memory bound rests on first-unit partitioning, not laziness).

## S5-2 — A3 commit_seq floor at the overlay checkpoint publish
- **2.1** `CheckpointSnapshot`: add `commit_seq_at_capture: Option<u64>` after `committed_watermark_at_capture` (:66).
- **2.2** owned `capture_snapshot` (:320): set `commit_seq_at_capture: None` (INERT — owned never advances commit_seq).
- **2.3** `capture_snapshot_immutable`: AFTER the watermark load (@408)/synced_frontier (@419-423), BEFORE root load
  (@425): `let commit_seq_at_capture = self.commit_seq.load(Acquire);` and in the returned struct (@509):
  `commit_seq_at_capture: Some(commit_seq_at_capture)`. (Same window as watermark; claims monotone in CAS order
  ⇒ floor ≤ every in-snapshot survivor generation.)
- **2.4** BOTH `publish_immutable_snapshot_retaining_wal` (after Checkpoint append+sync, in the wal_writer block)
  AND `_with_eviction`: `if let Some(floor)=snapshot.commit_seq_at_capture { wal_writer.set_commit_seq_floor(floor)?; }`.
  Monotone (raise-only), carried across rotate. Owned path unaffected (None guard).
- **R-2a (MEDIUM):** v4 §3 names `publish_durable_and_reclaim` (owned) as the floor entry; this plan places it in
  the OVERLAY retaining publishers (where the watermark checkpoint_lsn lives) — correct for the reversible subset.
  Confirm with owner if S5-12 later unifies the publisher.

## S5-9 — cfg un-gate + checkpoint route-split
- **(a)** Delete the `#[cfg(...)]` above the 5 fns (:347,:557,:664,:1152,:1262). Callees all production (no
  transitively-gated). Leave bench shims (:748,:770) gated. Reword "test-only" doc-comments. RES-8: 0 new unsafe
  (verify gate), compiles `--no-default-features` (verify).
- **(b)** route-split `checkpoint()` (:86, the body @94-95): `if self.route_overlay() { let s = self.capture_snapshot_immutable()?;
  if eviction_coordinator.is_some() { publish_immutable_..._with_eviction(s) } else { publish_immutable_...retaining_wal(&s) } }
  else { assert!(!route_overlay()); let s = self.capture_snapshot()?; self.publish_durable_and_reclaim(s) }`.
- **S5-8 third assert:** use `assert!(!(uses_overlay() && lockfree_root.is_some()))` (≡ `!route_overlay()`), NOT the
  literal `assert!(lockfree_root.is_none())` — the latter would PANIC the legit kill-switch owned checkpoint
  (overlay root present + OwnedTree mode). **Confirm intent with owner.**
- **RES-4 (HIGHEST):** `SharedCharARTrie::checkpoint` is a SEPARATE capture site (captures under a write guard then
  calls `publish_durable_and_reclaim` directly — persist.rs:88-93 doc; caller in mod.rs). The `checkpoint()`
  route-split does NOT cover it ⇒ identical post-flip total-loss bug. **MUST find + route-split it too (or prove
  production never uses it post-flip) BEFORE coding S5-9.**

## S5-10 — overlay reestablish + flip plumbing (NOT wired into prod ctors)
- **(a)** `insert_cas_with_value_nodurable(&self, term, value:u64) -> Result<bool>` in `impl<u64,S>` (@1371): reuse
  `build_value_path_recursive` + root CAS, ZERO append_*/sync/CommitRank/mark_committed (asserted by absence; gate
  test checks synced_lsn + watermark UNCHANGED). OnDisk child during reestablish ⇒ Err (overlay is in-memory).
  **Membership twin REQUIRED:** `insert_cas_nodurable(&self, term)` in the generic `impl<V,S>` (@138) via
  `build_path_recursive(..., finalize=true)` + root CAS.
- **(b)** `reestablish_overlay_after_recovery` (streaming-fallible, v3 §3): per first-code-point partition (disjoint
  cover: owned-root child keys + the empty term), `iter_prefix_with_values(prefix)?` (ABORT on Err), publish each
  chunk via the no-WAL fn, DROP chunk; clear the owned tree LAST (so mid-stream abort leaves owned-consistent —
  RES-7). Two monomorphic wrappers (`()`/`u64`) over a generic `insert_chunk` closure driver. NEW helpers:
  `owned_root_first_code_points()` + `clear_owned_tree_after_overlay_reestablish()`. Needs `&mut self` (root is a
  plain field); sequence `&self` iter/publish borrows BEFORE the final `&mut` clear (collect chunk to local, drop
  iter borrow, publish via `&self` CAS, then clear).
- **(c)** `flip_to_overlay`/`kill_switch_to_owned` construction helpers (overlay_write_mode.rs) — NOT wired
  (`#[allow(dead_code)]` or referenced only from a #[cfg(test)] round-trip). flip = enable_lockfree + set
  LockFreeOverlay; kill-switch = set OwnedTree (do NOT force set_owned_regime on a non-empty WAL).
- **(d) §9 remove non-faulting-first:** `remove_cas_durable` (553) is CURRENTLY faulting-FIRST — INVERT to
  `find_leaf_lockfree` first, `find_leaf_faulting` only on a non-OnDisk-absent. **Verify it didn't regress.** Same
  faulting-first in insert/upsert durable (1729/1846) — out of scope unless soak flags.
- **ctor wiring:** add a COMMENTED/owner-GO-gated block after `replay_records_lww` in mmap_ctor.rs `open` (@449),
  `open_with_depth` (@659), io_uring_ctor `open` (@252): `if Overlay regime && V∈{(),u64} { flip_to_overlay()?;
  reestablish_overlay_after_recovery()?; }`. Leave commented (no live #[cfg] that could flip in CI).

## S5-11 — gate tests
Helper: `overlay_trie(path)` = create + `set_durability_policy(Immediate)` + `enable_lockfree()` (stamps Overlay) +
`set_overlay_write_mode(LockFreeOverlay)`. Real-disk scratch `target/test-tmp`, never /tmp.
- **A (PRIMARY):** Overlay archive with a known UNRANKED orphan (via OD4 `set_commit_rendezvous` test seam @56, or
  the idempotent NO-RANK arm) + RANKED survivor → force `recover_from_archives` → orphan DROPPED, survivor KEPT.
  Mixed Owned+Overlay segments. Inertness sub-assert on all-Owned. (Grep tests/ first — A2 is committed, may exist.)
- **B:** B1 negative-increment Err; B2 merge Err; B3 begin_document Err; B4 insert_batch_bytes routes (visible via
  overlay). (May have landed with 1e91c0a — ADD gaps.)
- **C:** S5-2 floor round-trip (checkpoint → `commit_seq_floor()` == captured; owned-mode checkpoint leaves floor 0).
- **D:** S5-9 route-split — overlay checkpoint writes entry_count==N (NOT 0 from empty owned tree); reopen sees all
  N + correct counter values (no double-count). Shared-checkpoint variant once RES-4 fixed.
- **E:** S5-10 reestablish equivalence (overlay == owned set; owned cleared; synced_lsn+watermark unchanged); memory
  bound (structural); I/O-error abort leaves owned intact (RES-7); disjoint-cover boundary (""/"a"/"ab"/"b"/multibyte).
- Cross-cutting: unsafe-inventory gate, `--no-default-features` build, formal-correspondence, loom, N-S4-3 soak, full suites.

## Residual risks (verify before/while coding)
1. **RES-4 shared-checkpoint gap (HIGHEST, total-loss):** route-split `SharedCharARTrie::checkpoint` too.
2. **S5-8 third assert form (MEDIUM):** `!route_overlay()`, not `lockfree_root.is_none()` (kill-switch).
3. **§9 remove direction inverted today (MEDIUM):** currently faulting-first; verify before inverting.
4. **Reestablish membership twin + TypeId dispatch (MEDIUM).**
5. **RES-8 (LOW):** 0 new unsafe — verify gate.
6. **iter naming (LOW):** `iter_prefix_with_values` (not `iter_with_values`); Vec-per-call.
7. **`&mut self` for reestablish clear (LOW-MED):** sequence borrows.
8. **A2/producer tests may already exist (LOW):** grep, add only gaps.
9. **io_uring mostly moot (LOW):** no rebuild; checkpoint shared; add reestablish ctor-comment for symmetry.
10. **Orphan manufacture in Test A (LOW):** confirm the orphan LSN truly lacks a CommitRank.
