# Redesign: Durable Global Commit-Sequence (D1) — fixes the CommitRank residual data-loss holes

**2026-06-02.** Replaces the `cf1f80c` "generation = per-lifetime root version" ordering key, which the 3-agent
red-team found has residual data-loss holes (`docs/design/redteam-synthesis-commitrank-and-flip.md` A.1-A.6). This
redesign WILL be red-teamed before implementation. Persisted from the redesign Plan agent.

## (1) Mechanism: D1 (durable global commit-sequence). Why
**D3 (durable root version) REJECTED:** fixes only A.2, not A.1/A.3/A.4/A.5; root version is bumped per-publication
but idempotent arms publish nothing + increments bypass it. **D2 (visibility-LSN token) REJECTED:** assigning a
token at the CAS from the `next_lsn` counter (also advanced at append, out-of-CAS-order) just moves the skew; a
reserved LSN punches watermark holes. *But D2 surfaces the right invariant: the key must live in ONE durable
monotone domain shared by ranked AND unranked records.* **D1 RECOMMENDED:** a purpose-built `AtomicU64 commit_seq`,
advanced once per state-changing commit, stamped on EVERY state-changing record (incl. increments), ordered by
`(commit_seq, lsn)`, seeded on open from the durable max → globally monotone across restarts, decoupled from the
volatile root version. Strictly stronger than cf1f80c (monotone per-term AND across terms AND across restarts);
generalizes the single-session `reconcile_lww` path, doesn't regress it.

## (2) The durable key
**`commit_seq: AtomicU64`** on `PersistentARTrieChar` (beside `committed_watermark`).
**Seeding (A.2 fix):** in every open/recovery WAL scan (`mmap_ctor.rs:300`, io_uring twin, `RecoveryManager`),
track `max_commit_seq = max over ALL recovered CommitRank.generation` (over the FULL scan, BEFORE the
checkpoint-skip filter — a post-checkpoint op must out-rank a pre-checkpoint same-term op still in a retained
segment); seed `commit_seq = AtomicU64::new(max_commit_seq)`. `enable_lockfree` MUST NOT reset it (it's trie-owned,
seeded once at open, survives enable_lockfree — the precise A.2 fix).
**Claim-before-CAS, discard-on-loss (the monotonicity rule — load-bearing):** `next_commit_seq()` =
`commit_seq.fetch_add(1, AcqRel)+1`, claimed BEFORE the visibility CAS each loop iteration; on CAS-loss the claim is
DISCARDED (leaves a harmless gap — we only ever COMPARE commit_seq, never require contiguity). Theorem
(`CommitSeqMonotone`): if `X ≺_CAS Y` on the same term, `commit_seq(X) < commit_seq(Y)` — because the only way Y
gets a lower winning claim than X is if Y claimed-and-won before X claimed (⇒ Y ≺_CAS X, contradiction). Claiming
AFTER the CAS (no gaps) reopens a CAS-vs-fetch_add inversion — rejected; gaps are the correct price.
**A.6 (crash between data-append and rank-append) — made BENIGN, NOT eliminated (honest):** under Order-A you
CANNOT carry the CAS-assigned key in a single durable-before-visible append (the key is unknown until the CAS = the
visibility point; a single append after the CAS is Order-B = visible-before-durable, rejected). So KEEP the post-CAS
`CommitRank` append; the window yields an UN-ACKED op (watermark stalls at the data LSN — `mark_committed` runs only
after BOTH appends). A v3 data record with no rank at the frontier uses `generation_of=lsn` — safe because it has no
same-term out-orderer (it's the last append). **This is the single weakest seam (risk §8.1); the red-team must
scrutinize the "no same-term competitor above the frontier" argument under a torn multi-op batch.**

## (3) Stamp EVERY state-changing op, at CAS-success, from the OBSERVED state (A.3 + A.5)
`next_commit_seq()` wired into EVERY durable producer (replacing root-version reads), claim-before-CAS:
- `insert_cas_durable` winning `:363` + **idempotent `AlreadyExists` `:383`** (A.5 fix: real commit_seq, not live
  `lockfree_root.load().version()` re-walk).
- `remove_cas_durable` winning `:540` + **idempotent `AlreadyAbsent` `:567`** (A.5 fix).
- `insert_cas_with_value_durable:1659`, `upsert_cas_durable:1757`.
- **`try_increment_cas_durable:1547` (A.3 HEADLINE): now appends a CommitRank** (was rank-free) — increments ranked
  exactly like every other op; no more cross-domain LSN-vs-root-version mis-sort.

## (4) ALL FOUR recovery paths consume it — UNIFY on one reconcile (A.1)
Paths: (1)+(2) ctors `mmap_ctor.rs:403/597` + `io_uring_ctor.rs:227` → `replay_records_lww`→`reconcile_lww`
(already rank-honoring — auto-fixed once §3 stamps commit_seq). (3) `recover_from_archives:1138`→
`rebuild_from_wal_segments:1443` (raw LSN — A.1): collect segment records into one `Vec<(Lsn,WalRecord)>` (already
LSN-sorted) → route through `replay_records_lww`. (4) `RecoveryManager::redo_phase:756` + `IncrementalRecovery:932`
(raw LSN — A.1): extract tx-gating into a shared `committed_records_after_tx_filter` → route through `reconcile_lww`.
`IncrementalRecovery` STREAMS (can't hold all) → buffer per-checkpoint-window (a checkpoint bounds the reorder
window) OR fail-closed on v3 lock-free WALs (documented restriction, risk §8.3). New reconcile signature:
`reconcile_lww(records, loaded_from_disk, checkpoint_lsn, wal_version, tx_states)`.

## (5) reconcile honors the tx state machine (A.4)
Shared `analysis_phase`-style scan builds `tx_states: HashMap<u64,{Committed|Aborted|Incomplete}>`; `reconcile_lww`
SKIPS data records of non-Committed tx (a `current_tx` cursor like `redo_phase:769`). Closes A.4 (aborted/incomplete
document-tx replayed on the ctor path). A committed document-tx batch gets ONE `CommitRank` at `CommitTx` (batch
entries touch distinct terms; intra-batch order irrelevant). Removes the fragile "masked because overlay rejects
commit_document" dependence.

## (6) value-CAS C1′ + cf1f80c + WAL back-compat
**C1′:** its R-2 bail-rank `g_read` becomes a `commit_seq` claimed at the read-snapshot (same counter) → strictly <
any superseder's commit_seq in the DURABLE domain (fixes the synthesis note "C1′ inherits A.2"). One counter, one
rank record, one reconcile; the `NoPhantomCasWrite` + `DurableGlobalOrder` TLAs compose.
**cf1f80c:** single-session insert/remove → commit_seq order ≡ root-version order, so `reconcile_lww` + the OD4 s019
regression PASS unchanged (strengthens, doesn't regress).
**WAL:** REUSE `CommitRank=15` (rename `generation→commit_seq` in the v3 path; wire layout byte-identical). Header
**VERSION 2→3** (`header.rs:38`), MIN_SUPPORTED 1. Backward: v1→lsn fallback, v2→root-version legacy comparator
(selected by the `wal_version` param — a v2 WAL's ranks are root-versions, must NOT be reinterpreted as commit_seq);
no migration. Forward: v2 binary refuses v3 fail-closed (GAP_LEDGER/UNSAFE_BOUNDARY). No new record type (auditable
zero structural delta).

## (7) Re-proof
**TLA — extend `LockFreeOverlayDurableReplay.tla` → `DurableGlobalOrder`** with FIVE negative controls (each MUST
fire its invariant; register in `verify-formal-correspondence.sh:341`):
- W1/A.2: add `Restart` (resets `rootVersion`, NOT `nextGen=commit_seq`); `_UnsafeRestart.cfg` orders by rootVersion
  → MUST violate `ReplayEqualsCommittedVisible` on the cross-restart collision.
- W2/A.3: add `Increment` kind + per-term value; `_UnsafeRankFreeIncrement.cfg` models rank-free increment → MUST
  violate new `ReplayEqualsCommittedValue`.
- W3/A.1: `CONSTANT RECOVERY_PATH`; `AllPathsAgree`; `_UnsafeRawLsnPaths.cfg` (archive/redo/incremental order by raw
  LSN) → MUST violate on s019.
- W4/A.4: tx records + abort; `NoUncommittedTxReplay`; `_UnsafeTxIgnored.cfg` (reconcile ignores tx) → MUST violate.
- `CommitSeqMonotone` (the §2 theorem, gaps modeled).
**Extended soak (the blind spots cf1f80c's 50/50 missed) — each ≥50× green, Immediate+GroupCommit, real-disk:**
(1) archive-rebuild (`recover_from_archives`), (2) cross-restart (session1 insert+remove → reopen → session2 insert
→ crash → reopen → PRESENT), (3) increment-mixed-with-insert/upsert/remove on one key (value equality), (4)
aborted document-tx (OwnedTree), (5) idempotent-arm race. Plus a deterministic cross-restart regression
(fails-pre/passes-post). Stay-green: `concurrent_durable_writers_all_survive_reopen`, the existing soak, OD4,
`recovery_replay_completeness_correspondence`, atomicity specs, full gate exit 0, 0 unsafe.

## (8) Phased migration (foreground, green-gated: nextest ≥2534 + verify-formal-correspondence exit 0 + unsafe exit 0)
- **DG0:** `commit_seq` field + seeding (no behavior change; key still root-version). Rollback: delete field.
- **DG1:** producers source key from `commit_seq`, claim-before-CAS-discard-on-loss, INCL. increment-ranked (A.3) +
  idempotent-arm fix (A.5); header 2→3. Rollback: revert key source + header. Gate: single-session soak+OD4 green;
  cross-restart/increment soaks now PASS.
- **DG2:** reconcile `wal_version`+`tx_states` (A.4) + unify paths 3+4 (A.1). Gate: archive+aborted-tx soaks PASS,
  `AllPathsAgree`.
- **DG3:** value-CAS C1′ on the shared counter. Gate: NoPhantomCasWrite + DurableGlobalOrder compose.
- **DG4 (HARD GATE):** the extended TLA + FIVE controls fire. If any passes → STOP.
- **DG5:** the 5 extended soaks ≥50× + deterministic cross-restart regression. Unblocks the flip.
DG0-DG3 revert by code; DG4-DG5 verification-only; the one one-way step = header 2→3 (fail-closed, opt-in pre-flip).

## (9) Honest risks + the irreducible hole
1. **A.6 two-append window BENIGN not eliminated** (the weakest seam — see §2; red-team the frontier argument under
   a torn multi-op batch). **The one hole I cannot close: under append-only Order-A you cannot make key+data atomic
   in one fsync without back-patching (forbidden) or Order-B (loses on crash). The two-append benign-window is the
   irreducible residue.**
2. commit_seq GAPS from discard-on-loss (harmless for ordering; a future contiguity-assuming feature breaks).
3. `IncrementalRecovery` ⟂ global reordering (bound by checkpoint or fail-closed on never-checkpoint+v3).
4. document-tx batch records owned-path-logged+unranked → the §5 single-CommitRank-at-CommitTx couples commit_document
   to the new key (untested-in-production if overlay-tx never enabled).
5. increments now incur a second fsync (rank append) — mitigate via group-commit coalescing; measure.
6. the byte trie shares the codec but is out of scope — carries the latent defect if it gains lock-free durable writes.

### Critical files
- `src/persistent_artrie_char/lockfree_cas.rs` (`commit_seq`+`next_commit_seq`; rewrite key source in all producers
  `:363,383,540,567,1659,1757` + A.3 `:1547`; claim-before-CAS; 5 soaks)
- `src/persistent_artrie_core/recovery.rs` (`reconcile_lww:253` +`wal_version`+`tx_states`, v3-vs-legacy, A.4
  tx-gating; unify `redo_phase:756`+`IncrementalRecovery:932`+`rebuild_from_wal_segments:1443`)
- `src/persistent_artrie_char/mutation_core.rs` (`replay_records_lww:252` params; `apply_core_recovered_operation_no_wal:286` shared applier)
- `src/persistent_artrie_char/mmap_ctor.rs` (seed `commit_seq:300-321`; route `recover_from_archives:1138`;
  `io_uring_ctor.rs:227` twin)
- `formal-verification/tla+/LockFreeOverlayDurableReplay.tla` (DurableGlobalOrder + 5 `_Unsafe*.cfg`;
  `scripts/verify-formal-correspondence.sh:341`); `wal/header.rs:38` (2→3); `wal/codec.rs:227` (rename, layout same)
