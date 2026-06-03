# S5 v2 — The Production Flip, Re-Hardened (READ-ONLY; pending re-red-team)

**Crate `libdictenstein`, char ARTrie. 2026-06-03. Design only — NO code edited.** Baseline: committed
reversible core S0–S4 (HEAD `26b08ba`, all green). Supersedes `s5-production-flip-design.md`; closes
every confirmed v1 hole (V1–V5, H1–H5, A2, A6, A7) **plus two the v1 red-team missed (N1, N2)**.
Thesis: the flip is safe ONLY after a body of REVERSIBLE hardening lands green; the single IRREVERSIBLE
act (the ctor flip, S5-12) changes ~6 lines and is last.

## 0. Closing summary
| Hole | v2 status | Mechanism |
|---|---|---|
| V1 `open()` never re-establishes overlay | CLOSED | §1 recovery-into-overlay rebuild from the owned tree |
| H1/V5 owned-fallback u64 de-route (`increment(-)`) | CLOSED | §2 reject under `route_overlay()` |
| **N1 (NEW)** `insert_batch_bytes` has NO route guard | CLOSED | §2.3 route guard |
| **N2 (NEW)** `fetch_add(-n)` inherits increment de-route | CLOSED | §2.2 (inherits reject) |
| V2 merge drain appends unranked BatchIncrement | CLOSED | §3 hard-reject under Overlay |
| H2/A2 corruption-rebuild regime+gen blind; no data-header checkpoint_lsn | CLOSED | §4 single global reconcile pass + floor + write checkpoint_lsn |
| H3 flip emptiness predicate = current_lsn()==1 | CLOSED | §5 gate on FILE LENGTH==WalHeader::SIZE + post-assert |
| V4 checkpoint-before-flip not enforceable | CLOSED | §6 flip PERFORMS the checkpoint |
| A6 kill-switch asymmetric | CLOSED | §7 set_owned_regime + symmetric kill-switch |
| H4/A7 debug_assert! compiled out | CLOSED | §8 promote to assert! |
| H5 remove/incr fault-before-append stall | MITIGATED | §9 non-faulting-first pre-flight + mandatory soak |

## 1. V1 — open()-time overlay re-establishment (HIGH)
Both ctors construct `lockfree_root:None`, mode `OwnedTree`, replay into the OWNED tree, NEVER re-enable
the overlay for an Overlay file ⇒ post-reopen production writes go owned/unranked on an Overlay WAL ⇒
DROPPED next reopen; reads also miss recovered data. FIX: new `reestablish_overlay_after_recovery(&mut
self)` called from BOTH ctors' `open` IFF active-header regime == Overlay: (1) the owned tree already
holds the fully-reconciled state Σ (load image + `replay_records_lww(…,Overlay)`); (2) `enable_lockfree`
(stamp is a no-op on the already-Overlay non-empty file — §5 predicate prevents a restamp); (3) drain Σ
into the overlay NON-DURABLY: `iter_with_values()` → `insert_cas` (membership) / a NEW
`insert_cas_with_value_nodurable` (u64, build_value_path + CAS, NO WAL append); (4) clear owned
(`root=Empty`); (5) `set_overlay_write_mode(LockFreeOverlay)`; (6) asserts. Proof: the rebuild writes
NOTHING durable (RA-1) ⇒ crash mid-rebuild = disk byte-identical to pre-open ⇒ re-derives Σ. Reopen→write
W (ranked Order-A overlay producer)→reopen keeps W (no drop). Reads return Σ (overlay). The reopen path is
the SUFFIX of the flip path (§6) — one mechanism, two entry points.

## 2. H1/V5/N1/N2 — owned-fallback u64 de-route (HIGH)
The ONLY residual de-routes on a u64+Overlay trie: `increment(t, δ<0)` (route_increment returns None for
negative ⇒ owned body appends unranked Increment; also via `fetch_add` = N2) and `insert_batch_bytes`
(NO route guard at all = N1). (`upsert`/`get_or_insert`/`insert_with_value` u64 always route; CAS/doc-tx
already `Err` under overlay.) FIX: (2.2) `increment` under `route_overlay()`: if route returns None ⇒
`return Err("negative-delta increment unsupported under the add-only overlay")` (NOT fall through);
`fetch_add` inherits. (2.3) `insert_batch_bytes`+`_sorted`/`_grouped` get the overlay prologue
(delegate to routed single-op). (2.4) STAMP INVARIANT: `enable_lockfree` refuses to stamp Overlay for
`V ∉ {(),u64}` (TypeId check) — so no arbitrary-V file is ever MAGIC_OVERLAY. Closing: every
`append_to_wal` caller on a u64 Overlay trie either routes-to-ranked or `Err`s before the append. Gate:
a grep test asserts each `append_to_wal(` in the 4 routing files is route-guarded (RA-3 lexical).

## 3. V2 — merge drains (HIGH)
`merge_lockfree_values_to_persistent` (char + byte + vocab) appends unranked BatchIncrement + drains to
owned ⇒ dropped + invisible on Overlay. FIX: hard-REJECT under Overlay (`route_overlay()` or
`rank_regime()==Overlay`) — NOT emit-ranked (post-flip every increment is ALREADY durable per-op via
`try_increment_cas_durable`; a drain would double-count). Data-availability cost ZERO (data already
durable+visible in overlay). Owned/un-flipped: guard false ⇒ unchanged.

## 4. H2/A2 — corruption-rebuild regime+generation awareness (HIGH, conditional)
`rebuild_from_wal_segments` (core) + `RecoveryManager::rebuild_from_wal` (char) bypass `reconcile_lww`
(raw order, no regime/gen/drop) ⇒ mixed-segment archive resurrects/double-applies. `publish_snapshot`
never writes `checkpoint_lsn` to the data header ⇒ corruption path double-applies the folded prefix.
FIX: (4.2) ONE global `reconcile_lww` over all segments, each record tagged with ITS segment's header
regime (segments are single-regime; generalize `reconcile_lww` to per-record regime), generation-ordered
globally (LSNs are globally monotone across segments — `rotate_to_archive` carries next_lsn HIGH, RA-7),
using the data-header `checkpoint_lsn` skip. (4.3) `publish_snapshot` writes `checkpoint_lsn` (bytes
24–32, same fsync as the descriptor, RA-14). (4.4) populate `commit_seq_floor` at checkpoint
(`set_commit_seq_floor(commit_seq@capture)`, monotone, carried across rotate) so post-checkpoint ops
out-rank survivors. (4.5) break-glass fail-closed-on-Overlay-segment feature flag. **RA-6 RESOLVED:**
real removes ARE ranked (`remove_cas_durable` Removed arm emits `append_commit_rank` — verified) ⇒ never
dropped under Overlay; only idempotent no-op removes are unranked (safe to drop). v2 ALSO exempts
`WalRecord::Remove` from the Overlay unranked-drop as harmless defense-in-depth (a spurious remove is a
no-op; a dropped remove resurrects).

## 5. H3 — flip emptiness predicate (MEDIUM-HIGH)
The stamp gates on `current_lsn()==1`, FALSE after checkpoint+`rotate_to_archive` (carries next_lsn HIGH;
file is empty length-64) ⇒ stamp SILENTLY SKIPPED ⇒ Overlay-intent trie on an Owned WAL ⇒ NO-RANK orphans
KEPT ⇒ resurrection. FIX: gate on FILE LENGTH == `WalHeader::SIZE` (`is_empty_after_header()`);
`set_overlay_regime` internally length-guards; post-stamp `assert!(rank_regime()==Overlay)` (release).
RA-8: the fresh active is exactly a 64-byte header write+fsync (length-64 ⟺ truly empty).

## 6. V4 — flip PERFORMS the checkpoint (MEDIUM)
Emptiness ≠ folded. `flip_to_overlay(&mut self)` (construction): `checkpoint()` [owned, folds WAL into
data file, writes checkpoint_lsn §4.3, rotates spent WAL→archive, fresh active empty] → assert empty →
`enable_lockfree`+stamp (§5) → assert Overlay → **rebuild overlay from the just-folded owned tree (§1)**
→ clear owned → `LockFreeOverlay`. RA-9: post-flip same-process reads of pre-flip data work because the
flip ENDS with the §1 rebuild (overlay=Σ), NOT because an empty overlay faults the data image (a fresh
enable_lockfree root has no OnDisk children). Reopen path = the §1 SUFFIX of the flip path.

## 7. A6 — kill-switch symmetry (HIGH for completeness)
Reverting mode alone leaves MAGIC_OVERLAY ⇒ owned writes dropped. FIX: `WalWriter::set_owned_regime()`
(inverse, length-guarded, post-assert Owned). `kill_switch_to_owned(&mut self)`: overlay-checkpoint (fold
overlay→data) → rotate → set_owned_regime on the empty active → drop lockfree_root → mode OwnedTree.
Crash-safe at each step (table in §10). Archived Overlay segments stay Overlay (recovered per-segment by
§4); irreversibility boundary = existence of any Overlay archive segment (RA-10).

## 8. H4/A7 — promote asserts (MEDIUM)
persist.rs:464 (watermark≤synced_frontier, #41 guard), persist.rs:140 (next_lsn-unchanged), mod.rs:1312
(lockfree_root.is_none, → owned arm of the route-split): `debug_assert*` → `assert*` (unconditional).
RA-11: the watermark advances strictly AFTER WAL append+sync (Order-A) ⇒ no spurious release panic.

## 9. H5 — fault-in-before-append stall (MEDIUM, not deadlock)
`remove_cas_durable` (lockfree_cas.rs:554) faults BEFORE the append (buffer lock) — the 75-min-hang CLASS
(no cycle found, but stalls vs a checkpoint's buffer.write). FIX: non-faulting-FIRST pre-flight
(`find_leaf_lockfree`): present-in-memory ⇒ append (no fault); absent-via-non-OnDisk-edge ⇒ skip; hit an
OnDisk edge ⇒ THEN fault. Shrinks the faulting window to cold-prefix removes only. The N-S4-3 isolated
soak stays MANDATORY (empirical gate; RA-12: mitigated not eliminated).

## 11. Ordered edit list (reversible S5-1..S5-11 land before owner GO; only S5-12 irreversible)
- S5-1 (H2): `publish_snapshot` writes data-header `checkpoint_lsn`. Inert.
- S5-2 (A3): populate `commit_seq_floor` at checkpoint. Inert.
- S5-3 (A2): per-record-regime `reconcile_lww`; rewrite both rebuild sites to one global pass; Remove
  never-dropped; +fail-closed feature flag. Inert (corruption path).
- S5-4 (H3): `is_empty_after_header` + length-predicate + post-assert. Reversible.
- S5-5 (A6): `set_owned_regime`. Reversible.
- S5-6 (H1/N1/N2): `increment` reject negative under overlay; `insert_batch_bytes` route guard;
  `enable_lockfree` refuse non-{(),u64}. Reversible.
- S5-7 (V2): merge-drain reject under overlay (char+byte+vocab). Reversible.
- S5-8 (H4): promote the 3 asserts. Reversible.
- S5-9 (route-split): inner overlay checkpoint = capture_immutable + retain-WAL (un-gate); owned arm
  keeps the moved is_none assert. Reversible.
- S5-10 (V1+V4): `reestablish_overlay_after_recovery` + `flip_to_overlay` + `kill_switch_to_owned` +
  `insert_cas_with_value_nodurable`; **wire reestablish into BOTH ctors gated on Overlay regime** (the
  V1 close — byte-identical for Owned files). Reversible (no construction flip yet).
- S5-11: the new gate tests. Reversible.
- **S5-12 — THE FLIP (IRREVERSIBLE, ~6 lines):** the V∈{(),u64} ctors call `flip_to_overlay` (create) /
  reestablish handles open. Arbitrary-V UNCHANGED. Owner GO + full gate.

## 12. Gate sequence (timeout-wrapped, tee'd, REAL-disk scratch)
check+unsafe-inventory; reconcile per-record-regime unit (Remove-never-dropped); **V1 reopen→write→
reopen**; **H1 negative-increment + N1 batch_bytes**; **V2 merge-on-overlay**; **H3 flip-after-checkpoint**
(post-assert fires green); kill-switch round-trip + crash-injection; **A2 mixed-segment rebuild**;
flip-then-crash-at-each-step soak; **N-S4-3 lock-order soak (MANDATORY)**; durable soaks on flipped
default; loom + FULL TLA (NoLostWrite holds + _Unsafe controls FAIL); full recovery+char suites;
verify-formal-correspondence.sh. Owner GO consumed BETWEEN gate-pass and committing S5-12.

## 15. Residual assumptions for re-red-team (load-bearing)
- RA-1 (HIGH): the §1 rebuild writes NOTHING durable (verify `insert_cas`/`insert_cas_with_value_nodurable`
  never append/fsync).
- RA-2 (MEDIUM perf): eager §1 rebuild doubles peak memory + O(N) open latency (multi-GB vocab OOM/stall
  risk); lazy-fault reopen is Phase-F.
- RA-3 (MEDIUM): §2 no-unguarded-append is a LEXICAL grep gate, not type-enforced.
- RA-4: no production path calls the merge on a flipped trie expecting success.
- RA-5: the dual `CharTrieFileHeader` defs agree on bytes 24–32.
- RA-6 (was HIGHEST): RESOLVED — real removes ARE ranked (verified); v2 also exempts Remove from drop.
- RA-7: `rotate_to_archive` carries next_lsn HIGH (LSNs globally monotone; only `truncate` resets to 1).
- RA-8: the fresh active is exactly a 64-byte header write+fsync (length-64 ⟺ empty).
- RA-9: post-flip same-process reads work via the §1 rebuild ending the flip (not empty-overlay-faults).
- RA-10: archived Overlay segments are permanently Overlay; boundary = first Overlay archive segment.
- RA-11: watermark advances AFTER sync (Order-A) ⇒ no spurious release panic from the promoted assert.
- RA-12 (MEDIUM stall): H5 mitigated not eliminated; cold-prefix remove can still stall vs checkpoint.
- RA-13: the IRREVERSIBLE flip is CHAR-ONLY; byte/vocab stay Owned (holes latent-safe); a future
  byte/vocab flip must repeat this design.
- RA-14: §4.3 writes checkpoint_lsn + descriptor in the SAME fsync (verify no reorder).
- RA-15: §2.4 TypeId stamp-refusal + the §1/lockfree_value_route `Any` downcasts are sound for V=u64.

## 16. Re-red-team findings
PENDING.
