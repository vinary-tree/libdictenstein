# S5-12 Formal-Verification Strategy + Red-Team

**Crate `libdictenstein` (char ARTrie). Plan-agent strategy + parent red-team. Baseline HEAD `92b81eb`
(S5-12 V-1/V-2/V-3 landed).** Prover policy: Rocq + Z3 + TLA+ primary; the upgrade adds **TLAPS**
(Z3/Zenon/Isabelle backends) + **Verus** (Z3). Everything else rejected (§e).

## Obligation → tool → artifact → effort → risk

| # | Obligation (failure = data loss) | Tool | Artifact | Evidence now | Upgrade | Effort | Risk |
|---|---|---|---|---|---|---|---|
| 1 | `NoLostWriteUnderLockFreeCommit` (#41) | **TLAPS** | `LockFreeDurableCheckpoint.tla` → `THEOREM Spec ⇒ []Inv` | TLC 2w/3lsn + loom MAX_LSN=2 + `_Unsafe` neg-control | bounded→unbounded | 4–8 d (worst 2 wk) | MED-HIGH (inductive `Inv` hard) |
| 2 | Overlay-drops-unranked / no-resurrection (A2) | **Verus** (opt) | `reconcile_lww_with_regime` (recovery.rs:290) `ensures` over multisets | TLC `LockFreeOverlayDurableReplay` + unit test | test→proof | 5–9 d | MED (WalRecord/HashMap/sort spec-models) |
| 3 | RES-3 base-image self-completeness | **TLA model** (+opt Verus on extracted pure fn) | new `WalSegmentRetention.tla`; or `prune_keep_set` extraction | indirect only | new model + gate test | 3–6 d | **HIGH (claim-correctness) — see red-team** |
| 4 | Reestablish term/value preservation + clear-last abort-safety | **Rocq** | extend MapRefinement-idiom; fold-of-publish = recovered_owned | `s5_10b_*` tests | test→model proof | 2–4 d | LOW-MED (model not real iterator code) |
| 5 | Dual-magic fail-closed + TypeId gate | **decline** | 1 branch each | armed + unit tests | stays test | 0.5 d | TRIVIAL (proof restates code) |

## Recommended PRE-FLIP minimal package (cheapest, highest-info)
1. **RES-3 — but see the red-team below: a code-grounded fail-loud fix largely closes it without a multi-day TLA model.**
2. **Reestablish Rocq (obligation 4)** — 2–4 d, in-idiom, covers the clear-last abort-safety (the real irreversibility hazard).
3. A2 (obligation 2): KEEP TLC + the unit test; Verus is a STRETCH (already double-covered).
4. #41 (obligation 1): STAYS TLC + loom for the flip; TLAPS is the top POST-flip hardening (4–8 d, high variance). Justification: the watermark is TLC-checked at the witnessing bound (2 writers = min to disagree on commit order; 3 LSNs covers the appended-before/committed-after window) WITH a failing negative control proving it necessary-and-sufficient.

## Rejected tools (§e, one line each)
Apalache (bounded, not the ∀-proof #41 needs) · Iris/coq-paco/coq-itree (months for what loom+TLA cover) ·
Spin/mCRL2/Uppaal/IVy (re-model = translation risk, not assurance; Uppaal=timed, irrelevant) ·
Maude/K/AProVE/CSI/TCT-TRS/TTT2 (term-rewriting/termination — category mismatch) ·
ProVerif/Tamarin (no crypto/secrecy) · CVC5/Z3/Yices/Vampire/E/MiniSAT (backends, not front-ends) ·
Creusot (viable but Verus fits better — Z3 policy + vstd collections; both = tool-soup) ·
Dafny/F*/Why3/Lean/Agda/Idris2 (port the fn out of Rust = lose "prove the actual code").

---

## RED-TEAM VERDICT (parent, code-grounded against HEAD `92b81eb`)

**The strategy is sound + honestly-scoped. Its highest-value finding (RES-3) is VERIFIED but REFINED, and
yields a small reversible fix that mostly closes it.**

### RES-3 (obligation 3) — VERIFIED + REFINED
- ✅ CONFIRMED: `prune_segments_if_needed` (writer.rs:647) sorts by `a.0.cmp(&b.0)` (PATH), removes the
  lowest over count/size, floored ONLY by `remaining_count<=1` — no explicit LSN/checkpoint floor.
- ⚠️ REFINEMENT (the strategy's concern (ii) is MOOT): the segment filename is
  `wal_{counter:020}_...` (writer.rs:482) with `counter` a monotonic global `AtomicU64`
  (`ARCHIVE_SEGMENT_COUNTER`). The zero-padded fixed-width counter ⇒ **lexical path order ≡ counter
  order ≡ this-trie's rotation order ≡ first-LSN order**. So prune removes the OLDEST/lowest-LSN
  segments FIRST — the order is correct, not arbitrary. "filename-time vs first-LSN" does not diverge.
- 🎯 THE GENUINE RESIDUAL: `recover_from_archives` DELETES the base image + rebuilds with
  `checkpoint_lsn=0`. If prune removed old segments (records subsumed by the now-deleted base), the
  rebuild SILENTLY reconstructs an incomplete set starting at the first surviving segment's first-LSN
  > 1 — it does not detect the gap. This is a PRE-EXISTING corruption-recovery limitation (prune +
  recover_from_archives predate S5-12; the flip does not change prune or the base-deletion). It bites
  only on the DOUBLE failure: base corrupt (the trigger for recover_from_archives) AND old segments
  pruned. NOT a flip blocker.
- ✅ ACTIONABLE FIX (reversible, doable now): make `recover_from_archives` /
  `rebuild_from_wal_segments_regime_aware` FAIL-LOUD when the retained segment set has a prefix gap
  (the lowest first-LSN > 1 — a pruned prefix with a deleted base), instead of silently losing it. This
  turns silent data loss into a loud, recoverable error and is the right RES-3 close. A full
  `WalSegmentRetention.tla` model is then optional (the property is now "no silent incomplete rebuild",
  enforced in code).
- NOTE for the flip path specifically: the OVERLAY checkpoint RETAINS the WAL (NO rotate —
  `publish_immutable_snapshot_retaining_wal`), so checkpoint-driven archiving/pruning does not occur
  for the overlay; only the async WAL writer's SIZE-based rotation can archive overlay segments. The
  fail-loud fix covers that path too.

### Tool assignments — VALIDATED
- TLAPS-for-#41 (1), Verus-for-A2 (2), Rocq-for-reestablish (4), decline-#5: all sound. The §c inductive
  strengthening for #41 (`checkpointLsn ≤ Watermark` + the retention floor) is the right shape; the
  `CHOOSE`-Watermark elimination is the honestly-flagged hard part.
- The §e rejections are correct (verified the category mismatches; SMT-as-backend-only is right).
- The §f residuals (vacuous-ensures risk, model-vs-real-code gap, "proofs don't retire Test A/RES-3
  gate tests") are well-taken — keep the empirical tests.

### Red-team conclusion
GO on the strategy. The pre-flip formal package reduces to: (i) the RES-3 **fail-loud fix** (code, now —
closes the silent-loss vector the strategy found) + a gate test; (ii) reestablish Rocq (2–4 d). #41 TLAPS
+ A2 Verus are valuable POST-flip hardening, not flip blockers (double-covered at the witnessing bound).
The strategy correctly did NOT gate the irreversible flip on the high-variance unbounded-concurrency proof.
