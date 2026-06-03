# Durable Global Commit-Sequence Redesign — D2.5

**2026-06-02. Read-only design. Corrects ONLY the recovery/reconcile/sequencing layer of D2**
(`docs/design/durable-global-commit-sequence-redesign-d2.md`). Closes R1–R7 from
`docs/design/redteam-d2-findings.md`. **The D2 (1a) single-LP insert write-path and the `CommitSeqMonotone`
theorem (D2 §1) are PROVEN SOUND by RT-D2-A and are carried UNCHANGED.** D2.5 touches only the supporting
machinery: reconcile semantics, the archive recovery path, the commit_seq floor source, the version-enforcement
chokepoint, the tx encoding, and the DG phase sequencing. Principled-only — no deferrals. This document will
itself be red-teamed; §8 attacks it first.

Repo root: `/home/dylon/Workspace/f1r3fly.io/libdictenstein`. All paths relative to it.

---

## §0. The terrain D2.5 inherits (verified against code)

Two write/recovery regimes share one WAL format and one recovery substrate:

- **Base owned-tree path** (`src/persistent_artrie/`): `document_tx.rs:commit_document:171` logs `BeginTx` then
  ONE `WalRecord::BatchInsert` carrying all N terms under ONE LSN (`document_tx.rs:225` →
  `wal/writer.rs:append_batch:226`, single `append`), then `CommitTx`. Recovery is `RecoveryManager::recover` →
  `redo_phase:756` (raw LSN order, ignores `_checkpoint_lsn`/`_transactions` for filtering) and the archive
  rebuild at `mmap_ctor.rs:649` (raw `recovered_operations_from_record`). **This path NEVER emits a `CommitRank`**
  (verified: `rg CommitRank src/persistent_artrie/` is empty). It is `&mut self`-serialized ⇒ append order ==
  apply order == a total in-order log.
- **Char/vocab overlay path** (`src/persistent_artrie_char/`): the Order-A lock-free producers
  `insert_cas_durable:291`, `remove_cas_durable:454`, `insert_cas_with_value_durable:1576`,
  `upsert_cas_durable:1686` each append a data record then, after the winning CAS, append a `CommitRank`
  (`lockfree_cas.rs:367,388,551,572,1665,1763` via `wal_helpers.rs:append_commit_rank:87`).
  **`try_increment_cas_durable:1492` does NOT append a CommitRank today** (`:1536-1553` logs only `BatchIncrement`
  + marks the watermark). Recovery is the char ctor (`mmap_ctor.rs:403`) → `replay_records_lww:252` →
  `reconcile_lww`, the archive rebuild `recover_from_archives:1106` → `rebuild_from_wal_segments:1443`, and
  `IncrementalRecovery:932`.
- **`reconcile_lww` (`recovery.rs:253`) is shared** by BOTH codebases (the base re-exports it). Today it
  LWW-collapses everything by `(generation_of(lsn), lsn)` with `generation_of = rank.get(&lsn).unwrap_or(lsn)`
  (`recovery.rs:273`) — unranked records get `lsn` (LARGE) and WIN.
- **`CommitRank.generation: u64`** (`codec.rs:227-235`) currently carries the per-lifetime published-root version.
  D2 repurposes this field to carry the durable global `commit_seq` (no codec change — same `u64`, record type
  15). The `committed_watermark` runtime field is seeded to the full frontier `next_lsn-1` on open
  (`mmap_ctor.rs:346-348`), so it is unusable as the reconcile drop boundary (confirms A#6).
- **The kill-switch `OverlayWriteMode` is RESTART-TIME, not a hot toggle** (`overlay_write_mode.rs:75-85`): under
  `LockFreeOverlay` the owned tree is not written; a hot flip back to `OwnedTree` would read a stale owned tree.
  Switching requires setting the mode then reopening. **Crucially (`:78`): "the WAL is the shared source of
  truth, both trees recoverable from it" — a mode switch at restart does NOT itself write a checkpoint.** This is
  the exact seam R1c must resolve.

The repurposed `commit_seq` durable floor (D2 §2), the `set_commit_seq_floor` writer API, and the rotate/truncate
carry (D2 §2.2/§2.3) are carried into D2.5 unchanged **except** the floor SOURCE (R2). The (1a) builder split,
claim-before-CAS, and idempotent-arm-no-rank (D2 §1, §3.4) are carried unchanged.

---

## §1. R1 — reconcile semantics + the mixed-WAL drop-distinction (THE crux)

### 1.1 R1a — per-op-TYPE merge: increments SUM, membership/value LWW

D2 LWW-collapsed every record to one `(commit_seq, lsn)` winner per term, silently dropping all but the max —
**wrong for increments**, whose deltas are commutative and must all be applied (RT C#3b). The fix partitions the
reconcile by op type, not by term alone.

**Mechanism (in `reconcile_lww`, `recovery.rs:253`, expansion + apply).** After the `rank` map (Pass 1) and the
per-record drop/keep decision (1.2/1.3 below), build the kept, stamped, sorted stream
`sorted: Vec<(commit_seq, lsn, RecoveredOperation)>` exactly as today (stable sort by `(commit_seq, lsn)`). Then
apply with op-type-aware semantics:
- **`Increment { term, delta, result }`**: a *commutative accumulator*. The kept increment records for a term are
  ALL applied; the sort order is irrelevant to the final sum but is preserved for the `result`-snapshot case.
  (`mutation_core.rs:319-336` already applies each `Increment` accumulating, OR sets an absolute value when
  `result != 0`. D2.5 keeps that applier; the fix is reconcile *emits every kept increment op* instead of
  collapsing to one.)
- **`Insert`/`Remove`/`Upsert`/`CompareAndSwap`/membership** (LWW types): per term, only the **single**
  `(commit_seq, lsn)`-max kept record's effect survives. Reconcile may collapse same-term LWW records to that max.

**increment ‖ membership on one term.** A term can carry BOTH increment deltas AND a membership/value LWW record.
Rule: **LWW selects the latest absolute-value/membership anchor by `(commit_seq, lsn)`; increments with
`commit_seq` strictly greater than that anchor are summed ON TOP of it; increments below the anchor are subsumed**
(the anchor overwrote them). Implementation: an absolute-value op (Insert-with-value / Upsert / remove) is a
*reset point* for that term; increments after the last reset accumulate onto the reset's base; increments before
are discarded. Because the stream is in `(commit_seq, lsn)` order and `commit_seq` is the proven total visibility
order (D2 `CommitSeqMonotone`), "after the last reset" == "visible after the last reset" — the live semantics. For
pure-membership `V=()` no increments exist; degenerates to plain LWW.

**Correctness.** Increments are commutative+associative over i64 (bounded by `LOCKFREE_COUNTER_MAX`,
`lockfree_cas.rs:1523`); applying all confirmed deltas in any order after the latest reset matches the live
accumulator. Membership/value is idempotent-LWW; the max survivor matches the live last-writer. No confirmed delta
dropped (closes C#3b); no sub-max LWW record resurrected.

### 1.2 R1b — version-gate the drop rule (subsumes B#1)

D2 §3.2's pseudocode dropped BOTH `None` arms (the B#1 total-loss bug). The principled rule is **version-gated**,
because the two regimes have categorically different "unranked" meanings:
- **v3 (overlay-produced), record in the overlay rank-regime** (1.3 decides regime): every confirmed overlay op
  IS ranked (Order-A appends the CommitRank before ack). An unranked v3-overlay record is a two-append-window
  orphan NEVER acked ⇒ **DROP**.
- **v1/v2 (legacy), and v3-OWNED-regime records**: NO ranks exist (owned path never emits CommitRank). Every such
  record is a confirmed in-order append ⇒ **KEEP**, ordered by LSN (the `generation_of=lsn` legacy behavior).
  Exactly today's `recovery.rs:273 .unwrap_or(lsn)` ⇒ no loss.

The drop is **version-and-regime gated, NOT watermark-gated** (resolves B#3). Signature:
```
reconcile_lww(
    recovered_ops:  Vec<(Lsn, WalRecord)>,
    loaded_from_disk: bool,
    checkpoint_lsn: Lsn,
    wal_version: u32,                 // from the header (R5 guarantees a file is homogeneous in version)
    regime: RankRegime,               // Owned | Overlay — see 1.3
    tx_states: &HashMap<u64, TxState>,
) -> Vec<RecoveredOperation>
```
Per-record decision (`recovery.rs:280-290`):
```
let cseq = match rank.get(&lsn).copied() {
    Some(s) => s,                                   // ranked → confirmed visible → keep
    None => match (wal_version, regime) {
        (v, RankRegime::Owned) if v >= 1 => lsn,    // owned/legacy in-order → KEEP @ lsn
        (1..=2, _)                       => lsn,    // v1/v2 has no ranks at all → KEEP @ lsn
        (3.., RankRegime::Overlay)       => continue, // v3 overlay orphan → DROP
        _ => lsn,
    },
};
```
v1 and v2 both fall in KEEP@lsn and NEVER hit the drop (closes C#11). The v2 root-version comparator is the
`wal_version == 2` selection: order by `(root_version, lsn)` where `root_version = rank.get(&lsn)` (a v2 file is
single-session/single-root-lifetime so root-version is a valid intra-file order — closes C#10). v2 records are
always KEEP (no drop).

### 1.3 R1c — THE MIXED-WAL RESOLUTION (the load-bearing decision)

**Problem.** Pre-flip, the base owned-tree path writes in-order UNRANKED records AND the opt-in overlay path
writes RANKED records — possibly to the SAME WAL (`overlay_write_mode.rs:78`). A v3 file can contain BOTH owned
records (unranked, must KEEP) AND overlay two-append orphans (unranked, must DROP). Both "unranked." The drop rule
needs a per-record `regime`. **How is `regime` determined, principally?**

**PICK candidate (i) — regime-homogeneity-per-checkpoint-window — with the watermark as a NON-load-bearing
liveness fallback. Proof:**

**Claim C1 — within one open, the rank regime is constant.** `route_overlay()` (`overlay_write_mode.rs:71`) =
`uses_overlay() && lockfree_root.is_some()`. `overlay_write_mode` is set ONCE at construction, mutated only by the
restart-time `set_overlay_write_mode`; `lockfree_root` is set at open. So for an open's lifetime, EVERY durable
mutation routes the same way: all-Owned (no CommitRank) or all-Overlay (CommitRank for every confirmed op). ∎(C1)

**Claim C2 — a regime switch is observable only across a restart, and on a restart the recovered image becomes a
checkpoint-equivalent base the new regime sits above.** A switch = set mode → reopen. On reopen the ctor recovers
the prior WAL into the image, then `enable_lockfree()` builds the overlay from it. The new-regime producer's first
append gets `next_lsn = max_lsn+1` (`mmap_ctor.rs:320`), a clean LSN cut. But the active WAL after a switch
contains both regimes' records by LSN-prefix. **The principled separator is the checkpoint, MANDATED:**

> **Invariant REGIME-CHECKPOINT: `set_overlay_write_mode` MUST be preceded, on the OLD binary before shutdown, by
> a successful checkpoint that reclaims the entire active WAL.** Enforced: `set_overlay_write_mode` asserts the
> active WAL is empty above `checkpoint_lsn` (`next_lsn-1 == checkpoint_lsn`), and the flip runbook checkpoints as
> step 1.

Reconcile only processes records ABOVE `checkpoint_lsn` (the `lsn <= checkpoint_lsn` skip at `recovery.rs:283`).
Under REGIME-CHECKPOINT, the pre-switch checkpoint reclaims all old-regime records into the image; the new-regime
records are the ONLY records above `checkpoint_lsn`. **So the records reconcile examines were produced by exactly
ONE regime.** `regime` is a single per-open value, derived at recovery as: `if wal_header.version >= 3 && any
CommitRank with DATA_LSN > checkpoint_lsn { Overlay } else { Owned }` — OR (preferred, S#1 hardening) read from a
per-Checkpoint regime stamp. With C1, regime is well-defined for the entire reconcile input. ∎(C2)

**Crash BEFORE the pre-switch checkpoint completes:** REGIME-CHECKPOINT's assert fails-closed (refuses the switch
until a clean checkpoint), so the unsafe interleaving is unconstructable through the supported API — a fail-closed
precondition, not a silent hazard.

**Watermark needed? — NO for the drop decision; retained as a defense-in-depth liveness floor.** The drop is
version-and-regime gated and regime is homogeneous-per-window, so the contiguous-ranked-prefix watermark is NOT
the drop boundary (resolves B#3). D2.5 RETAINS a *reconstructed* watermark as a **liveness/anomaly floor inside
the Overlay regime only**, for a non-contiguous fsync hole INSIDE the overlay tail (torn-batch): in Overlay
regime, after the version+regime gate keeps a ranked record, additionally require `data_lsn ≤
reconstructed_overlay_watermark`; a ranked record above a durability HOLE is treated as torn and dropped. The
reconstruction (two-pass, marker-LSN-precise — closes B#3/A#6):
1. **Pass A (classify):** partition the tail's LSNs into DATA-LSNs and MARKER-LSNs (`CommitRank`/`Checkpoint`/
   tx-control). Build `data_lsns`, `ranked_data = {data_lsn : ∃ CommitRank{data_lsn}}` (a dangling rank whose
   `data_lsn ∉ data_lsns` is ignored — closes the dangling-rank concern), `marker_lsns`.
2. **Pass B (contiguous closure over DATA only):** `watermark = checkpoint_lsn`; repeat: `next = smallest data_lsn
   > watermark`; if `next` exists ∧ `next ∈ ranked_data` ∧ all marker-LSNs in `(watermark, next)` decode cleanly,
   advance `watermark = next`; else stop. Marker-LSNs are SKIPPED (not data), so the walk never stalls at a
   rank-LSN.

This watermark is liveness-conservative (wrong ⇒ drops MORE, never resurrects); acked records are protected by R3
(acked ⇒ ranked ⇒ contiguous-covered). For robustness D2.5 ALSO persists the overlay watermark durably:
`committed_watermark_floor: u64` at header byte 28..36 (inside D2's `reserved[8..44]`), written by the overlay
checkpoint publisher (`persist.rs:579`, where `checkpoint_lsn == watermark`). Recovery seeds reconstruction from
`max(checkpoint_lsn, header.committed_watermark_floor)`. So neither reconstruction nor persistence is alone
load-bearing.

**Decision (R1c):** regime via REGIME-CHECKPOINT homogeneity (C1+C2), making the drop version+regime-gated and the
watermark non-load-bearing; the reconstructed-AND-persisted overlay watermark retained ONLY as an Overlay torn-hole
liveness floor. NO per-record "expects-rank" bit (redundant given homogeneity; rejected on no-unnecessary-mechanism).
NO forbidding shared WAL (homogeneity-per-window buys correctness without two WALs).

### 1.4 R1d — the archive path (closes A#5/C#4)

`recover_from_archives:1106` deletes the main image + active WAL (`mmap_ctor.rs:1130-1132`), creates a FRESH trie
(`:1135`), rebuilds purely from segments via `rebuild_from_wal_segments:1443` in raw per-segment LSN order with NO
reconcile/rank/drop. An orphan `Insert("k")@lsn11` archived after `Remove("k")@lsn9` resurrects "k" (A#5). Fix:
1. **Collect ALL segment records into ONE `Vec<(Lsn, WalRecord)>`** (segments sorted by first-LSN via
   `sort_segments_by_lsn:1417`; archive LSN space is globally monotone via the rotate `next_lsn` carry
   `writer.rs:463`), then `reconcile_lww` ONCE over the union (cross-segment rank visibility). Thread out
   `max_commit_seq` for the seed.
2. **`checkpoint_lsn = 0`** for the archive reconcile (no base image; `recover_from_archives` deleted it). Nothing
   skipped; segment set must be self-complete (segment 1 is the first-ever WAL, covering history from LSN 1).
3. **Apply the v3 drop** (1.2/1.3): regime determined the same way (segment header version + CommitRank above
   `checkpoint_lsn=0`, or per-Checkpoint stamp). Overlay-regime archive ⇒ orphans DROP; Owned-regime ⇒ KEEP@lsn.

**Self-completeness + base requirement.** Self-complete iff segment 1 present AND segments contiguous in LSN.
`prune_segments_if_needed` (`writer.rs:469`) can delete old segments. **If pruned, the set is NOT self-complete and
`recover_from_archives` MUST refuse.** Precondition (S#3-hardened): first segment's first LSN == 1 AND each
segment's last-LSN+1 == next segment's first-LSN (NO interior gap), else
`RecoveryError("archive set non-contiguous; pruned segments require a base image — use the main-file open path")`.
Fail-closed, not silent resurrection. Orphans don't resurrect: union-reconcile + Overlay-drop ⇒ the orphan
Insert@lsn11 is unranked-above-regime ⇒ dropped; only ranked Remove@lsn9 survives ⇒ absent. ∎

---

## §2. R2 — floor source: from the actually-reclaimed set

**Defect (B#4c/C#12).** D2 §2.5 set `floor = max_durable_commit_seq` (global, bumped by every producer incl.
overlay). The two checkpoint paths use different `checkpoint_lsn` domains: owned `next_lsn` (`persist.rs:153`),
overlay `committed_watermark` (`persist.rs:568`). If overlay writes bump the global max but the OWNED checkpoint
runs (image lacks those writes), the floor claims subsumed seqs not in the image ⇒ a replayed overlay write can
sort below a stale owned-image value ⇒ loss.

**Fix.** `floor = max { commit_seq(r) : r is a CommitRank with data_lsn ≤ checkpoint_lsn }` (0 if none) — tied to
the reclaim boundary, domain-correct for both:
- **Owned checkpoint** (`checkpoint_lsn = next_lsn`): owned records unranked ⇒ rank-max 0 unless overlay ranks ≤
  `next_lsn` exist; by regime homogeneity (§1.3 C1) the owned window has no overlay writes ⇒ floor=0=correct. Any
  overlay ranks present are from a PRIOR overlay window already in the image (`data_lsn ≤ checkpoint_lsn`) ⇒
  correctly subsumed.
- **Overlay checkpoint** (`checkpoint_lsn = committed_watermark`): reclaimed set = contiguous-confirmed prefix; its
  rank-max = the max commit_seq folded into the image. Un-reclaimed overlay writes (`data_lsn > checkpoint_lsn`)
  keep their WAL records, NOT falsely subsumed. ✔

**Computation.** Maintain `commit_seq_by_data_lsn: BTreeMap<Lsn, u64>` updated by `append_commit_rank`. At
checkpoint, `floor = commit_seq_by_data_lsn.range(..=checkpoint_lsn).map(|(_,s)| *s).max().unwrap_or(0)`. After the
checkpoint reclaims `≤ checkpoint_lsn`, prune that range. Owned path: map empty ⇒ floor 0, no scan. Replaces D2
§2.5's global `fetch_max` with a bounded range-max — eliminating the §6.3 deferral (the floor depends only on what
`data_lsn ≤ checkpoint_lsn` was reclaimed = what the image contains). Wire: `persist.rs:147` (owned) + `:579`
(overlay). Monotone `set_commit_seq_floor` guards against a lower-domain checkpoint lowering the floor.

---

## §3. R3 — ack-after-rank-sync invariant (B#2)

**Invariant ACK-AFTER-RANK:** *a durable producer ACKs the caller ONLY AFTER the CommitRank is appended AND
synced.* Then `acked ⟹ ranked-and-durable ⟹ never the trailing-rank case ⟹ never dropped`.

**Already-satisfied (verified):** insert returns at `lockfree_cas.rs:373` AFTER `append_commit_rank(:367)`; remove
`:551/572`; insert-value `:1665`; upsert `:1763`. `append_commit_rank` → `append_to_wal_inner` (`wal_helpers.rs:101`)
syncs per policy (`sync_wal_after_append:170`, verifying `synced_lsn ≥ appended_lsn`); durable producers reject
non-sync policies (`lockfree_cas.rs:1500`). ✔ (docstring `wal_helpers.rs:80-83` states it; D2.5 elevates to a named
invariant.)

**The ONE violator: `try_increment_cas_durable`** (`:1547-1553`) acks WITHOUT a CommitRank ⇒ an acked increment is
unranked ⇒ Overlay-regime DROP ⇒ delta lost ⇒ violates `ReplayEqualsCommittedValue`. **Fix (also R1a/DG1):** add
`let rank_lsn = self.append_commit_rank(lsn, key.as_bytes(), generation)?;` after the CAS (`:1547`) +
`mark_committed(rank_lsn)` before return, mirroring insert. commit_seq claimed before the CAS (value-path is
single-LP). Confirmed increments become ranked ⇒ summed ⇒ never dropped. Ack sites: insert `:373`, remove
`:551/572`, insert-value/upsert returns, increment (NEW) after `:1547`. Base owned acks at `commit_document` return
(`document_tx.rs:252`) — Owned regime (KEEP, no drop), so vacuous (durability = the BatchInsert+CommitTx sync).

---

## §4. R4 — DG resequencing: atomic header+reader gate, honest reversibility (B#5/C#5a/C#6)

**Defects.** (B#5/C#5a) D2's DG1 bumped header + stamped commit_seq, but the gated reader landed at DG3 — between
deploys a v3 WAL is recovered by the ungated reconcile (orphans WIN = the loss class). (C#6) the bump is one-way
(`from_bytes:82` refuses `version > VERSION`), so a reverted v2 binary refuses a v3 file ⇒ stranding; D2 mis-filed
it as reversible.

**Fix — the WRITER-side commit_seq stamp and the READER-side gate land in ONE atomic gate, header bump honestly
one-way.** The version `2→3` bump, the commit_seq stamp meaning-change, AND the version+regime-gated reader land in
the SAME phase (DG-RECON, §10). Before it, NO v3 record exists; after, the v3-gating reader is deployed. DG band
split: DG0–DG2 code-reversible (no v3 record, no bump); DG-RECON+ crosses the one-way bump; `migrate_v2_to_v3` is
the only cross-version path. Stop claiming DG0–DG5 reversible across the bump.

---

## §5. R5 — enforcement chokepoint INTO `AsyncWalWriter::open`/`open_or_create` (C#7)

**Defect.** D2's guard was at the char/vocab `wal_managed` wrapper; the base ctors call
`AsyncWalWriter::open_or_create` (`mmap_ctor.rs:378`) / `::create` (`io_uring_ctor.rs:66`) DIRECTLY, bypassing it.

**Fix — guard inside `WalWriter::open`** (`writer.rs`, after `from_bytes:116`, the write-open entry opening
`read+write` `:104`):
```
if header.version < WalHeader::VERSION {
    return Err(WalError::UnsafeVersionMixing { found: header.version, current: WalHeader::VERSION });
}
```
`WalReader` (recovery) never goes through `WalWriter::open`, so READ stays permissive `[MIN_SUPPORTED..=VERSION]`
(migration reads v2). Both `AsyncWalWriter::open` and `::create` funnel through `WalWriter::{open,create}`, and
every ctor in both codebases reaches an `AsyncWalWriter` ctor ⇒ the guard covers base (`mmap_ctor.rs:378`,
`io_uring_ctor.rs:66`), char (`:327`), io_uring twins. `create` writes a fresh `VERSION=3` header. v2→v3 in-place
mix impossible at the single chokepoint (closes C#7, subsumes D2 §6.7).

---

## §6. R6 — tx: ONE commit_seq per BatchInsert/tx (C#8)

**Defect.** D2 §4.2 posited per-op commit_seq in a tx, but `commit_document` logs the whole tx as ONE BatchInsert
with ONE LSN + one CommitTx, NO per-op record (verified: `append_batch` → single `append`, `writer.rs:226`).
Unimplementable without splitting BatchInsert.

**Fix — ONE commit_seq per BatchInsert/committed-tx (the atomic unit), increments summed (R1a).** A BatchInsert is
the Owned regime (KEEP, no drop), so commit_seq isn't strictly needed for ordering within the owned tail (in-order).
Cross-regime ordering (a later overlay op on a term a prior owned BatchInsert touched) is via the floor (R2: owned
checkpoint sets floor, post-reopen overlay gets `floor+1`). **Tx-gating (closes A.4):** `reconcile_lww` takes
`tx_states` (from `analysis_phase:678`). During expansion, a `current_tx` cursor (mirroring `redo_phase:769-819`)
DROPs every data record whose tx is not `Committed`. `recovered_operations_from_record` already maps
Begin/Commit/AbortTx → `vec![]` (`recovery.rs:356-358`); gating lives in the expansion so both the ctor path AND
`redo_phase` consult `tx_states` and agree. Torn-mid-batch (one BatchInsert is one record — atomic; a partial fsync
truncates at the boundary, CRC-detected `recovery.rs:776`) ⇒ no strict-subset (RT-D2 §4.3 confirmed). Abandon the
per-op-in-tx scheme.

---

## §7. R7 — local errata (A#7, C#10, C#11, A#4-residual)

- **A#7 — commit_seq lives ONLY in the CommitRank, never the data record.** The Insert/Remove/Increment DATA
  records carry NO commit_seq (verified: `codec.rs` data variants have no such field; only `CommitRank.generation`).
  DG-RECON must NOT hoist the claim beside the `:329` data append (breaks `CommitSeqMonotone` adjacency or forces
  per-retry re-append). Claim at the CAS-retry iteration top, stamp into the CommitRank after the winning CAS (D2
  §1.4). State as a DG-RECON precondition.
- **C#10 — v2 root-version branch:** `wal_version == 2` selects order-by-`(root_version, lsn)`,
  `root_version = rank.get(&lsn)`. Pure-v2 is single-root-lifetime (floor reads 0). v2 records always KEEP (drop
  never fires for v2; root-version is the sort key only).
- **C#11 — v1 `unwrap_or(lsn)` branch:** v1 has no CommitRank, `rank` empty, `generation_of=lsn`, KEEP@lsn — the
  pre-fix in-order replay. v1 MUST hit KEEP, NEVER the drop.
- **A#4-residual — guard/test for "non-durable insert_cas is replay-irrelevant".** Test obligation: no
  durable+non-durable mix lets a non-durable `try_set_final` (in-place 0→1) race a durable `as_non_final` (copy+CAS
  1→0) on a shared node. Safe today (remove uses copy+CAS, never in-place clear, `lockfree_cas.rs:431-438`); add a
  `debug_assert` in non-durable `insert_cas` that the trie isn't durable-producer-active + a loom schedule.
  Documented in GAP_LEDGER as a guarded premise.

---

## §8. SELF-RED-TEAM — attacking D2.5 (especially R1c and the archive path)

**S#1 (R1c, crux) — REGIME-CHECKPOINT relies on operator discipline; a future hot toggle breaks it.** The proof
hinges on `set_overlay_write_mode` being restart-time AND gated by a pre-switch checkpoint. A future hot toggle
interleaves regimes above one `checkpoint_lsn` ⇒ mis-classification (loss). **Hardening (ADOPT): record regime
per-CHECKPOINT** — add `regime: u8` to `WalRecord::Checkpoint` (or the header), so each window's regime is READ
from its bounding checkpoint, not inferred. Upgrades C2 from operator-gated to self-describing bytes. **The seam
most likely attacked; the per-checkpoint stamp is the airtight version.**

**S#2 (R1c) — the "any CommitRank above checkpoint_lsn ⇒ Overlay" inference is fooled by a dangling rank** whose
`data_lsn ≤ checkpoint_lsn` but whose MARKER LSN is `> checkpoint_lsn`. **Mitigation:** key the inference on a
CommitRank whose DATA_LSN > `checkpoint_lsn`, AND/OR adopt S#1's per-checkpoint stamp (moots the inference).

**S#3 (archive, R1d) — the contiguity check `first==1` is satisfiable by a pruned set that starts at 1 with an
interior gap.** **Fix:** verify each segment's last-LSN+1 == next's first-LSN (contiguous coverage); any gap ⇒
refuse. (Folded into §1.4.) Also: the corrupt-record `break 'segments` (`recovery.rs:1467`) truncates the union;
since we collect-then-reconcile, stop collection AT the corrupt point and reconcile the prefix; a rank in a later
segment for a truncated-tail data record is dangling-and-ignored (safe — beyond the durable prefix).

**S#4 (R2) — the incremental map vs the overlay watermark.** A ranked record with `data_lsn ≤ watermark` but rank
MARKER above it: its data IS in the image (`data_lsn ≤ watermark ⇒ committed ⇒ in snapshot`), and the map is keyed
by data_lsn ⇒ `range(..=watermark)` includes it ⇒ contributes to the floor. ✔ **Residual:** prune the map range
`≤ checkpoint_lsn` ONLY after rotate/truncate succeeds+syncs; a crash between floor-set and rotate ⇒ reopen
recomputes the map from the WAL scan (the map is a cache, the ranks are truth). Add a test.

**S#5 (R1a) — increment + remove on one term.** Within one overlay window, `Increment@cseq5` then `Remove@cseq7`:
sort ⇒ increment then remove ⇒ remove wins ⇒ absent (the increment delta "lost" but correctly superseded). The R1a
rule must be scoped "summed among increments NOT superseded by a later LWW reset (remove/insert-value)" — §1.1
states this (reset points). Load-bearing; confirmed consistent.

**S#6 (R4/R5) — `migrate_v2_to_v3` reads a v2 file that might itself be mixed-regime** (the kill-switch existed
pre-D2.5; v2 had no REGIME-CHECKPOINT). The v2 comparator is best-effort under the legacy comparator — which is
what v2 always did; D2.5 doesn't make it worse. Document that migration inherits v2's pre-existing recovery
semantics, no stronger.

**S#7 (R3) — group-commit batches the rank into a later un-synced batch.** Under GroupCommit, `append_commit_rank`
→ `gc.append_with_sync` (`wal_helpers.rs:117`) BLOCKS on the batch fsync + `verify_full_policy_sync_coverage`
checks `synced_lsn ≥ rank_lsn` (`:198-210`) ⇒ the rank IS synced before return. ✔ If the rank batch's fsync is
interrupted, `append_commit_rank` returns Err ⇒ NOT acked ⇒ the unranked data is a two-window orphan ⇒ dropped
(correct). GroupCommit twin must be in the soak.

**S#8 (R1c watermark) — byte-budget bookkeeping** for `committed_watermark_floor` at 28..36, leaving
`reserved[16..44]` (36..64). Document the offset + round-trip test. Low risk.

---

## §9. Updated TLA invariants + negative controls

Extend `formal-verification/tla+/LockFreeOverlayDurableReplay.tla` → `DurableGlobalOrderD25.tla`. Carry D2 §7.1's
model and ADD: per-op-type effect (increment accumulator vs LWW), a `regime ∈ {Owned, Overlay}` per checkpoint
window, the reclaimed-set floor, the archive union-reconcile. Model additions: `value: [Terms -> Int]` (counter
sum) alongside `present`; `ReplayValue(t)` sums all kept increment deltas after the last reset; `regime` stamped at
`Checkpoint`, `CrashRecover` reads it per window; drop = `Overlay ∧ ¬ranked ⇒ drop`, `Owned ∨ v≤2 ⇒ keep@lsn`;
`floor' = Max({gen : rec ∈ committedOps, rec.lsn ≤ checkpoint_lsn})`; `Restart` seeds `nextGen' = Max(floor,
scanMax)`; an `Archive` action (union all segments, reconcile with `checkpoint_lsn=0` + window regime).

**Invariants:** `ReplayEqualsCommittedVisible`, `NoLostNetWrite`, `NoResurrectionOnReplay`, `DurablePrefix`
(carried); `ReplayEqualsCommittedValue` (NEW, R1a counters); `CommitSeqMonotone` (carried, UNCHANGED);
`FloorDominatesSubsumed` (R2, RECLAIMED-SET def); `SeedAboveDurable` (R2); `NoUnconfirmedWins` (R1b/c);
`RegimeHomogeneousPerWindow` (NEW, R1c); `ArchiveNoResurrection` (NEW, R1d); `AckImpliesRanked` (NEW, R3);
`NoUncommittedTxReplay` (R6); `NoVersionMix` (R5).

**Negative controls (`_Unsafe*.cfg`, each MUST fire; register in `scripts/verify-formal-correspondence.sh`):**
- `_UnsafeLwwIncrement.cfg`: reconcile LWW-collapses increments ⇒ violates `ReplayEqualsCommittedValue`.
- `_UnsafeUngatedDrop.cfg`: both-None-arms drop (B#1) ⇒ violates `NoLostNetWrite` for Owned/v1/v2.
- `_UnsafeKeepOverlayOrphan.cfg`: Overlay unranked KEPT@lsn ⇒ violates `NoUnconfirmedWins`/`NoResurrectionOnReplay`.
- `_UnsafeMixedRegimeWindow.cfg`: a checkpoint window has BOTH regimes ⇒ violates `RegimeHomogeneousPerWindow`.
- `_UnsafeArchiveRawLsn.cfg`: archive raw per-segment replay, no union/drop ⇒ violates `ArchiveNoResurrection`.
- `_UnsafeGlobalFloor.cfg`: floor = global max_durable under owned-checkpoint-after-overlay-write ⇒ violates
  `FloorDominatesSubsumed`/`SeedAboveDurable`.
- `_UnsafeAckBeforeRank.cfg`: ack after data but before rank ⇒ violates `AckImpliesRanked` ∧
  `ReplayEqualsCommittedVisible`.
- `_UnsafeTxIgnored.cfg`, `_UnsafeVersionMix.cfg`, `_UnsafeNoFloor.cfg`, `_UnsafeSplitLP.cfg`,
  `_UnsafeRawLsnPaths.cfg` (carried for `NoUncommittedTxReplay`, `NoVersionMix`, `SeedAboveDurable`,
  `ReplayEqualsCommittedVisible`, `AllPathsAgree`).

---

## §10. Re-authored DG phases — atomic header+reader gate, honest reversibility

Each gate: `nextest` ≥ current + `scripts/verify-formal-correspondence.sh` exit 0 + unsafe-inventory exit 0;
systemd real-disk; `RUN_TLC=1` at the formal gate. **DG0–DG2 revert by code (NO v3 record, NO bump). DG-RECON
crosses the ONE-WAY `2→3` bump; everything from DG-RECON on is forward-only, `migrate_v2_to_v3` the sole
cross-version path.**

- **DG0 — `commit_seq` field + floor plumbing (no behavior change, NO bump).** Add `commit_seq` + the
  `commit_seq_by_data_lsn` map (R2) to the char inner struct; add `commit_seq_floor` (20..28) +
  `committed_watermark_floor` (28..36) to `WalHeader`, `set/get_commit_seq_floor` to `WalWriter`, rotate/truncate
  CARRY both floors (`writer.rs:458`/`:353`). **VERSION stays 2.** Seed `commit_seq = max(header.commit_seq_floor,
  scan-max)` in char + base ctors. **Rollback:** delete fields (v2 reads floor=0). **Gate:** green; floor
  round-trips.
- **DG1 — single-LP insert (1a) + builder split + increment-rank, behind the still-v2 reader (NO bump, NO drop).**
  Carry D2 §1.2: `build_final_path_recursive`, route `insert_cas_durable` through it, delete `try_set_final` from
  the durable arm, keep non-durable `insert_cas` on the old builder. Claim commit_seq in all durable producers.
  **Add CommitRank to `try_increment_cas_durable`** (R3/R1a). Idempotent arms: no rank, no `mark_committed`.
  **VERSION still 2** — CommitRank.generation now carries commit_seq but a v2 reader reads it as a root-version and
  the OLD reconcile works (commit_seq is monotone like root-version within one session). **Rollback:** revert.
  **Gate:** prefix-split regression PASSES; `CommitSeqMonotone` holds.
- **DG2 — durable floor from the reclaimed set, both checkpoint paths (NO bump).** Wire
  `set_commit_seq_floor(range_max_of(map, checkpoint_lsn))` into owned `publish_durable_and_reclaim` (`:147`) AND
  the overlay retain-publisher (`:579`); set `committed_watermark_floor = checkpoint_lsn` at the overlay path.
  Seed-from-floor active. **No §6.3 deferral** (reclaimed-set floor is domain-correct). **Rollback:** stop setting
  floors. **Gate:** post-checkpoint-reseed PASSES; `FloorDominatesSubsumed`/`SeedAboveDurable`.
- **DG-RECON — THE ATOMIC GATE: header `2→3` + commit_seq-stamp-meaning + version+regime-gated reader, together
  (ONE-WAY).** (a) bump `header.rs:38` `VERSION 2→3`; (b) add the per-checkpoint `regime` stamp (S#1); (c) the new
  `reconcile_lww` signature + version+regime drop (§1.2/1.3) + per-op-type merge (§1.1) + two-pass
  reconstructed-AND-persisted overlay watermark + C1′ bail-claim at the read instant (D2 §1.7); (d) the R5 guard in
  `WalWriter::open` (`:116`). (a)+(c) ship together ⇒ the moment a v3 record can exist, the gating reader is
  deployed (closes B#5/C#5a). **One-way.** **Rollback:** NOT code-reversible past here (documented, pre-flip,
  opt-in, fail-closed). **Gate:** two-window + torn-window + mixed-file-refusal PASS; `NoUnconfirmedWins`,
  `RegimeHomogeneousPerWindow`, `AckImpliesRanked`, `_UnsafeUngatedDrop`/`_UnsafeKeepOverlayOrphan`/
  `_UnsafeMixedRegimeWindow`/`_UnsafeAckBeforeRank` all FIRE.
- **DG-PATHS — unify all recovery paths (A.1 + R1d archive).** Route `redo_phase`/`RecoveryManager` (base owned),
  the base archive rebuild (`mmap_ctor.rs:649`), the char `rebuild_from_wal_segments` (union-collect + reconcile +
  `checkpoint_lsn=0` + Overlay-drop + contiguity precondition, §1.4 + S#3), and `IncrementalRecovery` (per-window +
  fail-closed for never-checkpoint-v3) through the gated reconcile. Thread `max_commit_seq` out of every path.
  REGIME-CHECKPOINT assert added to `set_overlay_write_mode`. **Rollback:** per-path. **Gate:** archive-rebuild
  (orphan-drop) + never-checkpoint-Incremental + mixed-file PASS; `AllPathsAgree`, `ArchiveNoResurrection`.
- **DG-TX — tx gating + one-commit_seq-per-BatchInsert (R6).** `reconcile_lww` tx-gates in the expansion cursor;
  BatchInsert carries one commit_seq (Owned, KEEP). **Rollback:** revert gating. **Gate:** torn-tx-batch +
  aborted-tx PASS; `NoUncommittedTxReplay`.
- **DG-FORMAL (HARD GATE).** Extend TLA to `DurableGlobalOrderD25` with ALL §9 invariants + controls. **If ANY
  `_Unsafe*.cfg` PASSES → STOP.**
- **DG-SOAK.** All D2 §7.2 scenarios ≥50× (Immediate+GroupCommit, real-disk) PLUS: regime-switch-across-restart
  (set Owned→checkpoint→reopen-Overlay, verify homogeneity), archive-orphan-drop (idempotent Insert after Remove,
  both archived, reopen ⇒ absent), increment-summed-across-crash, the A#4-residual non-durable/durable-mix loom.
  Plus deterministic regressions (`stage_prefix_split`, `stage_post_checkpoint_reseed`, `stage_archive_orphan_drop`,
  `stage_increment_sum`). Verification-only; reversible until the flip flag flips.

---

### Critical Files for Implementation
- `src/persistent_artrie_core/recovery.rs` — R1a/b/c/d/R6: `reconcile_lww:253` (per-op-type merge, version+regime
  gate, two-pass watermark, tx-gate), `recovered_operations_from_record:305`, `redo_phase:756`,
  `IncrementalRecovery:932`, `rebuild_from_wal_segments:1443` (union-collect).
- `src/persistent_artrie_char/lockfree_cas.rs` — R3/R1a/DG1: add CommitRank to `try_increment_cas_durable:1547`;
  ack-after-rank sites `:373,:551,:572,:1665,:1763`; (1a) builder split carried from D2.
- `src/persistent_artrie_core/wal/writer.rs` — R5/R2/DG0: version-refuse guard in `open:116`;
  `set/get_commit_seq_floor`; carry floors across `rotate_to_archive:458`/`truncate:353`.
- `src/persistent_artrie_core/wal/header.rs` — R4/R1c/DG-RECON: `VERSION 2→3` at `:38` (atomic gate);
  `commit_seq_floor` 20..28, `committed_watermark_floor` 28..36; `Checkpoint.regime` stamp.
- `src/persistent_artrie_char/persist.rs` — R2: reclaimed-set floor at owned `:147` + overlay `:579`; and
  `src/persistent_artrie_char/overlay_write_mode.rs:83` — REGIME-CHECKPOINT assert in `set_overlay_write_mode`.
