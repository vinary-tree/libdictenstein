# Durable Global Commit-Sequence Redesign — D2.6

**2026-06-02. Read-only design.** Corrects ONLY the recovery/reconcile/sequencing/regime layer of D2.5
(`durable-global-commit-sequence-redesign-d2.5.md`), closing the RT-D2.5 cluster (A#1–A#11, B#1c-residual,
B#2a/b/c, B#3a, B#4-residuals, B#5). **D2's (1a) single-LP insert write-path, `CommitSeqMonotone` (D2 §1.4), and
C1′ (D2 §1.7) are PROVEN SOUND and carried UNCHANGED.** D2.5's R3 ack-after-rank, R5 chokepoint, R6 tx, R7 errata
are carried UNCHANGED. D2.6 **REPLACES** D2.5's R1c inferred-per-window regime, R1a per-op-merge coder, and the
split DG sequencing — the fix is a simplification: **regime ≡ WAL VERSION**.

The six decisions D1–D6 are pre-made (synthesis in `redteam-d2.5-findings.md`). This document validates each
against code, fills in the mechanics + DG phases, and self-red-teams (§7) the load-bearing claim.

---

## §1. D1 — regime ≡ WAL VERSION

**Decision.** A v3 WAL is PURE overlay (every confirmed op is ranked). v1/v2 is owned/legacy (no ranks ever). No
mixed v3 WAL: the production flip creates a FRESH v3 WAL; the owned path NEVER writes v3 (enforced by the R5
version guard in `WalWriter::open`). Therefore **v3 ⟺ overlay-regime**, and the drop is purely version-gated:
`v3-unranked ⇒ DROP`, `v1/v2-unranked ⇒ KEEP@lsn`. **No regime field, no inference, no per-Checkpoint regime
stamp** (D2.5 S#1 dropped — superseded).

### 1.1 Validation against code

**C1 (regime constant within an open) — CONFIRMED.** `route_overlay()` (`overlay_write_mode.rs:71-73`) =
`uses_overlay() && lockfree_root.is_some()`. `overlay_write_mode` set once at construction (default `OwnedTree`,
`mmap_ctor.rs:349`), mutated only by restart-time `set_overlay_write_mode` (`:83-85`); `lockfree_root` set once in
`enable_lockfree` (`lockfree_cas.rs:160`). Every durable mutation in an open routes identically. ∎

**(a) does the flip create a fresh WAL? VALIDATED: today it does NOT — the gap D2.6 closes.**
`set_overlay_write_mode` (`:82-85`) is a trivial setter (no rotate/checkpoint/fresh-WAL); `overlay_write_mode`
defaults to `OwnedTree` each open (`mmap_ctor.rs:349`), never persisted (A#6). So flipping + continuing to write
the existing v2 `wal_writer` would append v3 CommitRanks into a v2 file.

> **D2.6 mechanism — "flip ⇒ fresh v3 WAL" (the load-bearing constructor step).** The flip is a CONSTRUCTOR-time
> decision: (1) `set_overlay_write_mode(LockFreeOverlay)` on the OLD binary, (2) clean shutdown runs the OWNED
> checkpoint (`persist.rs:147-175`, rotating the v2 active WAL into a v2 archive and leaving a fresh v2 active),
> (3) reopen with the new binary whose ctor, seeing `route_overlay()` true, opens the active WAL through a new
> private helper `ensure_overlay_wal_version(&wal_writer, path, config)` called immediately after
> `open_or_create_async_wal` (`mmap_ctor.rs:327`) and BEFORE any overlay producer: reads the header version; if
> `route_overlay() ∧ version < 3` → `rotate_to_archive` the v2 tail and re-create the active file with
> `WalHeader { version: 3, .. }`; if `route_overlay() ∧ version == 3` → no-op. The owned path never hits this
> (route_overlay false), keeps writing v2. **Makes "v3 WAL created only at/after the overlay flip" a structural
> invariant, not a discipline.** Cite `mmap_ctor.rs:326-332`, `io_uring_ctor.rs:66` analog, rotate `writer.rs:424-472`.

Honest note: production `enable_lockfree` wiring does not exist yet — the only callers are `#[cfg(test)]`
(`dict_impl_char.rs:2996-3126`, `lockfree_cas.rs:2062+`). This ctor step is GREENFIELD; no live v3 producer to
break. D2.6 specifies it as the flip's first irreversible action.

**(b) per-segment-version archive — VALIDATED feasible.** `WalReader::read_header(path)` exists
(`reader.rs:103-108`) → the `WalHeader` (version) for any segment. Today `rebuild_from_wal_segments`
(`recovery.rs:1443-1487`) never reads segment headers (it seeks past them, `reader.rs:28`) and applies raw, NO
version awareness. Across a flip the archive holds v2 segments (KEEP all unranked) AND v3 segments (DROP unranked
orphans) — A#3 real.

> **D2.6 mechanism — per-segment version threaded into ONE global reconcile.** Re-author `recover_from_archives`
> (`mmap_ctor.rs:1106-1160`) + `rebuild_from_wal_segments` (`recovery.rs:1443`): (1) sort segments by first-LSN
> (`sort_segments_by_lsn:1417`; archive LSN space globally monotone via the rotate `next_lsn` carry,
> `writer.rs:463`); (2) collect ALL segment records into one `Vec<(Lsn, WalRecord, u32 seg_version)>`, tagging each
> with its segment's header version via `WalReader::read_header` (unreadable header ⇒ refuse); (3) build ONE global
> rank map across all records (cross-segment rank visibility; ranks never cross the flip — v2 segments have none);
> (4) run the filtered reconcile (§2) ONCE with `checkpoint_lsn = 0` (no base image — `recover_from_archives`
> deleted it, `mmap_ctor.rs:1130-1132`), the per-record `seg_version` as the drop discriminant (v2 unranked KEEP,
> v3 unranked DROP); (5) self-completeness precondition (S#3): first-LSN==1 AND each segment's last+1==next.first
> (no interior gap), else `RecoveryError("archive non-contiguous; pruned ⇒ need base image")`
> (`prune_segments_if_needed:510` can delete old segments). Thread `max_commit_seq` out for the seed. Cite
> `recovery.rs:1443-1487`, `mmap_ctor.rs:1106-1160`, `reader.rs:103`.

**(c) base recovers v2 in-order; the guard prevents cross-opening — VALIDATED.** The base owned codebase recovers
via `RecoveryManager::recover:645` → `analysis_phase:678` + `redo_phase:756` (raw LSN order) + base archive
`mmap_ctor.rs:649` — NEVER calls `reconcile_lww`/`replay_records_lww` (grep empty in `src/persistent_artrie/`).
Correct for v2 (no ranks ⇒ in-order = the total `&mut self` append order). Under D1 the base never write-opens a
v3 file: the R5 guard in `WalWriter::open` returns `UnsafeVersionMixing` when `header.version < VERSION`. The
read-only `WalReader`/`read_header` is permissive `[MIN_SUPPORTED..=VERSION]` so `migrate_v2_to_v3` reads v2. ∎

### 1.2 Why regime≡version closes the A-cluster
A#1 (first window): no window — the header version IS the regime. A#2 (base never reconciles): by design (base = v2
in-order; guard blocks base opening v3). A#3 (multi-regime archive): per-segment version. A#4 (no regime stamp):
version IS the regime. A#5/A#6 (wrong-domain assert / not persisted): flip forces a fresh v3 file, regime durable
in the header forever; the D2.5 REGIME-CHECKPOINT assert DELETED. B#2b (switch-back domain mismatch): owned=v2
file / overlay=v3 file, floor lives only in v3 (§4).

---

## §2. D2 — reconcile = apply-ALL-in-(commit_seq,lsn)-order + version-gated DROP filter

**Decision.** `reconcile_lww` ALREADY applies all in-scope records in `(generation, lsn)` order with NO collapse —
CONFIRMED `recovery.rs:253-298` (Pass 1 rank map `:259-271`; Pass 2 expands EVERY record, stamps
`generation_of(lsn)=rank.unwrap_or(lsn)` `:273,286`, pushes ALL `:287-289`, stable-sorts `(generation,lsn)` `:296`,
returns ALL `:297`). Increments all emitted; the applier accumulates. **D2.5 §1.1's "reset-point applier" is a
fiction — DELETED.** R1a collapses to: apply all KEPT records in order; the drop rule only FILTERS which are kept.

### 2.1 The filter
New signature (replacing `recovery.rs:253-257`):
```rust
pub fn reconcile_lww(
    recovered_ops: Vec<(Lsn, WalRecord)>,
    loaded_from_disk: bool,
    checkpoint_lsn: Lsn,
    wal_version: u32,                  // file header version (homogeneous per file — R5)
    tx_states: &HashMap<u64, TxState>, // R6 tx-gating (carried)
) -> Vec<RecoveredOperation>
```
Archive path threads per-record version (§1.1b) via an internal `reconcile_core` taking a closure
`version_of: Fn(Lsn) -> u32` (file path passes `|_| wal_version`; archive passes the per-segment lookup) — one body.
The per-record decision (replacing `let g = generation_of(lsn);` at `:286`):
```rust
let cseq = match rank.get(&lsn).copied() {
    Some(s) => s,                          // ranked → confirmed-visible overlay op → KEEP @ commit_seq
    None => {
        if version_of(lsn) >= 3 { continue; }  // v3 overlay + unranked ⇒ two-append orphan ⇒ DROP
        else { lsn }                            // v1/v2 owned/legacy + unranked ⇒ KEEP @ lsn (in-order)
    }
};
```
Everything else (checkpoint-skip `:283-285`, expansion `:287`, stable sort `:296`) UNCHANGED. v1+v2 → KEEP@lsn
(closes C#11/C#10; v2 root-version is just the `rank.unwrap_or(lsn)` sort key; v2 never dropped, drop needs
version≥3). **No watermark, no reconstructed-overlay-watermark, no per-checkpoint regime stamp** (D2.5's two-pass
watermark + `committed_watermark_floor` header field DROPPED). Rationale: drop is purely version-gated, and R3
ack-after-rank guarantees `acked ⟹ ranked ⟹ never the drop arm`; the only unranked v3 records are un-acked
two-append orphans which MUST drop. A torn-hole = a missing data record; a CommitRank with a missing `data_lsn` is
a dangling rank (keyed by data_lsn; the expansion only emits ops for records that EXIST) — harmlessly ignored.
Strictly simpler; removes the fragile rank-interleaving walk (B#3).

### 2.2 Route ALL recovery paths through the filtered reconcile (closes B#4)
Three char consumers call `replay_records_lww` (`mutation_core.rs:252`) → `reconcile_lww`: `mmap_ctor.rs:403,597`,
`io_uring_ctor.rs:227` — each threads `wal_version` (from the header at the seed-scan `mmap_ctor.rs:292`). Plus:
- **`IncrementalRecovery`** (`recovery.rs:856-969`, fully ungated today) → filtered reconcile PER CHECKPOINT WINDOW
  (records between two Checkpoints); never-checkpoint-v3 (one unbounded window) is FAIL-CLOSED (refuses, caller
  falls back to whole-file `replay_records_lww`). `BeginTx`'s `pending_ops.clear()` (`:940`) → tx-state tracking (R6).
- **Archive** per §1.1b.
Satisfies `AllPathsAgree`.

---

## §3. D3 — ONE ATOMIC GATE + re-authored DG phases (honest reversibility)

**Decision.** Land TOGETHER in ONE irreversible gate (DG-RECON): {header 2→3; (1a) single-LP insert (D2.5 §1.2);
commit_seq stamp at all 8 producer sites (replacing `root.version()`); increment-rank + generation-threading
(B#3a); the version-gated reconcile reader (§2); the R5 `WalWriter::open` guard; the flip-creates-fresh-v3-WAL
ctor step (§1.1a)}. A v3 record must NEVER exist before its gating reader. Phases before the gate write NO v3
record and NO header bump (code-reversible); after, forward-only.

### 3.1 Why inseparable (validated)
- **B#5:** if a v3 CommitRank is written while the OLD ungated reconcile is deployed, an unranked Insert-orphan
  (`data@lockfree_cas.rs:329`, crash before `append_commit_rank@:367`) sorts at `generation_of=lsn` (LARGE) ⇒ WINS
  ⇒ resurrects a removed term. Producer + gated reader + bump are atomic.
- **B#3a:** the commit_seq stamp is an 8-site producer rewrite. Today 4 producers stamp `generation =
  new_root.version()`: insert `:363` (via `insert_lockfree_recursive:928→933`), remove `:540`, insert-value `:1659`,
  upsert `:1757`; plus idempotent insert `:383`. Increment `try_increment_cas_durable:1492` emits NO CommitRank
  (`:1547` calls `try_increment_cas` returning only `new_val`; generation captured internally at the CAS `:1441`
  and discarded). **Generation-threading fix:** extract `try_increment_cas_inner(key, delta) -> Result<(u64
  new_val, u64 generation)>` capturing `new_root.version()` before the CAS at `:1441` (mirroring `:1659/:1757`);
  keep public `try_increment_cas` a thin `inner().map(|(v,_)| v)` wrapper (preserves the formally-checked
  no-lost-update reuse `:1473-1477`). Then in `try_increment_cas_durable` after `:1547`:
  ```rust
  let (new_val, generation) = self.try_increment_cas_inner(key, delta)?;
  let rank_lsn = self.append_commit_rank(lsn, key.as_bytes(), generation)?;  // R3/A#8
  self.committed_watermark.mark_committed(lsn);
  self.committed_watermark.mark_committed(rank_lsn);                          // ack-after-rank
  Ok(new_val)
  ```
  The commit_seq: the 8 sites currently stamp `new_root.version()`. DG-RECON repurposes `CommitRank.generation`
  (`codec.rs:227-235`, `u64`, type 15 — NO codec change) to carry the durable GLOBAL `commit_seq`. The producer
  claims `commit_seq` at the CAS-retry loop top (NOT beside the `:329` data append — R7/A#7, which would break
  `CommitSeqMonotone` adjacency), stamps it into the CommitRank after the winning CAS. The 8 sites become `let
  generation = self.commit_seq.fetch_add(1, ...)` per-iteration; `CommitSeqMonotone` holds (value-path is
  single-LP; the claim brackets the sole CAS).
- **R5 guard** in `WalWriter::open` after `from_bytes` (`writer.rs:116`):
  ```rust
  if header.version < WalHeader::VERSION {
      return Err(WalError::UnsafeVersionMixing { found: header.version, current: WalHeader::VERSION });
  }
  ```
  Refuses appending v3 to a v<3 file at the single write-open chokepoint (both `AsyncWalWriter::open` and `::create`
  funnel through `WalWriter::{open,create}`; every ctor reaches an `AsyncWalWriter` ctor — `mmap_ctor.rs:378`,
  `io_uring_ctor.rs:66`, char `:327`). `WalReader`/`read_header` bypass it (migration reads v2). The same chokepoint
  enforces "no mixed v3 WAL" for §1. MUST land with the bump (before it, `VERSION=2` and the guard would refuse
  legitimate v2 opens). Cite `writer.rs:101-116`, `header.rs:38,82`.

### 3.2 Re-authored DG phases (each gate: `nextest` ≥ current + formal-correspondence exit 0 + unsafe-inventory
exit 0; systemd real-disk; `RUN_TLC=1` at the formal gate)
- **DG0 — `commit_seq` field + floor plumbing (NO behavior change, NO bump, VERSION 2).** Add `commit_seq:
  AtomicU64` + the bounded `commit_seq_by_data_lsn` map (§4) to the char inner struct; `commit_seq_floor` at header
  bytes 20..28 (`header.rs:22`), `set/get_commit_seq_floor` on `WalWriter`; make `rotate_to_archive` (`writer.rs:458`)
  and `truncate` (`:338-364`) CARRY the floor (§4). Seed `commit_seq = max(header.commit_seq_floor, scan_max)` in
  char ctors. **Rollback:** delete fields (v2 reads floor=0). **Gate:** floor round-trips across rotate+truncate.
- **DG1 — single-LP insert (1a) + builder split + increment-rank, behind the still-v2 reader (NO bump, NO drop).**
  Carry D2 §1.2: `build_final_path_recursive`, route `insert_cas_durable` through it, delete `try_set_final` from
  the durable arm, keep non-durable `insert_cas` on the old builder. Add increment generation-threading + CommitRank
  (B#3a). The 8 sites still stamp `new_root.version()` (NOT yet commit_seq). VERSION still 2 — a v2 reader reads
  `CommitRank.generation` as a root-version, OLD reconcile works (root-version monotone within one session).
  **Rollback:** revert. **Gate:** prefix-split regression PASSES; `CommitSeqMonotone` holds for the increment rank.
- **DG-DECODE — D6 sentinel fix** (§6) — NO codec change, lands at ≤DG1. **Gate:** absolute-0 vs delta-0 distinct.
- **DG2 — durable floor from the reclaimed set, both checkpoint paths (NO bump).** Wire
  `set_commit_seq_floor(reclaimed_range_max(map, checkpoint_lsn))` into owned `persist.rs:147` AND overlay retain
  `persist.rs:579`; seed-from-floor active (§4). **Rollback:** stop setting floors. **Gate:** post-checkpoint-reseed
  PASSES.
- **DG-RECON — THE ATOMIC GATE (ONE-WAY).** Together: (a) bump `header.rs:38` `VERSION 2→3`; (b) repurpose the 8
  stamp sites to claim `commit_seq`; (c) the new `reconcile_lww` + version-gated drop wired into all three char
  consumers (`mmap_ctor.rs:403,597`, `io_uring_ctor.rs:227`); (d) the R5 guard in `WalWriter::open:116`; (e) the
  flip-creates-fresh-v3-WAL ctor step (§1.1a); plus the §7.1 guards (merge-bridge overlay-reject;
  `V3WalImpliesOverlayLive` debug_asserts; idempotent-arm commit_seq claim). **One-way** (`from_bytes:82` refuses
  `>VERSION`). `migrate_v2_to_v3` (§5) the sole cross-version path. **Rollback:** NOT code-reversible past here.
  **Gate:** v3-orphan-drop + v2-keep + mixed-file-refusal PASS; `NoUnconfirmedWins`, `AckImpliesRanked`, negative
  controls FIRE.
- **DG-PATHS — unify recovery paths.** Char `rebuild_from_wal_segments` (union-collect + per-segment-version +
  filtered reconcile + `checkpoint_lsn=0` + contiguity) + `IncrementalRecovery` (per-window + fail-closed) +
  `migrate_v2_to_v3` (§5); thread `max_commit_seq` out. Base `redo_phase`/archive stay raw (v2 only). **Gate:**
  archive-orphan-drop + never-checkpoint + mixed-segment-archive PASS; `AllPathsAgree`, `ArchiveNoResurrection`.
- **DG-TX — tx gating (R6).** `reconcile_lww` tx-gates in the expansion cursor (drop data of tx ∉ Committed, via
  `tx_states` from `analysis_phase:678`); BatchInsert one commit_seq (v2 Owned, KEEP). **Gate:** torn-tx-batch +
  aborted-tx PASS.
- **DG-FORMAL (HARD GATE).** TLA `DurableGlobalOrderD26` + all controls. **Any `_Unsafe*.cfg` PASS ⇒ STOP.**
- **DG-SOAK.** All D2 §7.2 scenarios ≥50× (Immediate+GroupCommit, real-disk) PLUS flip-creates-fresh-v3-WAL across
  restart; archive-orphan-drop with a v2-then-v3 segment mix; increment-summed-across-crash; absolute-0 vs delta-0;
  the A#4-residual non-durable/durable-mix loom. Reversible until the flip flag flips.

---

## §4. D4 — floor: header-carry MANDATORY across rotate/truncate + map bound

**Decision.** `commit_seq_floor` carried across BOTH `rotate_to_archive` and `truncate`; lives ONLY in v3 files
(one domain); source = reclaimed set; the map is bounded.

### 4.1 Header-carry is load-bearing (B#2a) — VALIDATED
`rotate_to_archive` (`writer.rs:458`) writes a FRESH `WalHeader::new()` (zeroes `reserved` ⇒ any floor there;
resets `checkpoint_lsn=0`; carries `next_lsn`/`synced_lsn` only via runtime atomics `:463-466`, NOT the header).
`truncate` (`:338-364`) reuses the in-memory header but resets `checkpoint_lsn=0` (`:353`). The owned checkpoint
TRUNCATES/rotates (`persist.rs:170`), so "recompute from scan" UNDER-computes post-truncate (reclaimed ranks now
in an archive). ⇒ the header-floor-carry is the ONLY thing preventing floor regression — load-bearing.
> **Mechanism.** Add `commit_seq_floor: u64` at header bytes 20..28 (`to_bytes:66`/`from_bytes:91` — carve from
> `reserved[20..64]`, leaving `reserved[28..64]`). In `rotate_to_archive` (`writer.rs:458`) construct the new header
> `WalHeader { version: <preserved>, commit_seq_floor: <carried>, .. }` (NOT `WalHeader::new()`) — carry version AND
> floor. In `truncate` (`:352-356`) set `header.commit_seq_floor = <reclaimed floor>` alongside `checkpoint_lsn=0`.
> Monotone `set_commit_seq_floor` (raise-only, mirroring `set_min_lsn:269`). Floor lives only in v3 files ⇒ B#2b
> switch-back mismatch vanishes (owned checkpoint never reads/writes an overlay floor).

### 4.2 Source = reclaimed set; map bound (B#2c)
`floor = max { commit_seq(r) : CommitRank with data_lsn ≤ checkpoint_lsn }` (0 if none), from
`commit_seq_by_data_lsn: BTreeMap<Lsn,u64>` (updated in `append_commit_rank`, `wal_helpers.rs:87`): at checkpoint
`floor = map.range(..=checkpoint_lsn).map(|(_,s)| *s).max().unwrap_or(0)`; prune `range(..=checkpoint_lsn)` after
reclaim. Owned path: map empty ⇒ floor 0. Wire `persist.rs:147` (owned, `checkpoint_lsn=next_lsn`) + `:579`
(overlay, `checkpoint_lsn=watermark`).
> **Map bound (B#2c — overlay retains its WAL `persist.rs:599`; never-checkpoint soak ⇒ unbounded).** Cap at
> `MAX_COMMIT_SEQ_INDEX = 1<<20` (~16 MB). On cap, drop the lowest-`data_lsn` half (oldest). On a checkpoint whose
> `checkpoint_lsn` exceeds the smallest retained key, FALL BACK to a bounded active-WAL scan
> `range(prev_floor_lsn..=checkpoint_lsn)` to recompute the reclaimed max (those ranks are in the active WAL, not
> yet rotated). O(1) memory + scan-fallback correctness. (Never-checkpoint overlay: the floor is simply never
> advanced — safe, the WAL replays in full.)

---

## §5. D5 — implement `migrate_v2_to_v3` (was vaporware — B#4)
> **Tool (a `pub fn` on the char trie + CLI entry).** `migrate_v2_to_v3(v2_path, v3_path) -> Result<MigrationStats>`:
> (1) open v2 read-only via `WalReader`/`read_header` (read path permissive `[MIN_SUPPORTED..=VERSION]`,
> `header.rs:82`; the R5 write-guard does NOT apply to reads); assert `version ∈ {1,2}`. (2) recover under the v2
> comparator = the filtered reconcile (§2) with `wal_version=2` (every unranked KEPT@lsn — legacy in-order; reuses
> `replay_records_lww`; v2 inherits its pre-existing semantics, S#6). (3) create a FRESH v3 trie at `v3_path`
> (`create_with_config` writes `WalHeader::new()` at `VERSION=3`). (4) checkpoint the recovered image
> (`persist_to_disk`, `persist.rs:217`) so the v3 active WAL starts empty above `checkpoint_lsn` — no v2 records in
> the v3 WAL. (5) return stats.
> **Rollback (the ONLY bridge).** The header 2→3 is one-way (`from_bytes:82`). `migrate_v2_to_v3` is the FORWARD
> bridge; reverse is NOT supported (a v3 file may hold overlay ranks with no v2 ordering). Documented rollback: keep
> the pre-flip v2 archive segments (`rotate_to_archive` preserved them) and revert by re-opening the pre-flip v2
> image+archives with the OLD binary. Document in GAP_LEDGER / UNSAFE_BOUNDARY. Cite `header.rs:82`, `persist.rs:217`,
> `mmap_ctor.rs:1106`.

---

## §6. D6 — fix the `result==0` increment-sentinel collision (B#1c-residual, pre-existing)
VALIDATED: `recovered_operations_from_record:347-354` maps `BatchIncrement → Increment{result:0}` (delta) while a
single-op `WalRecord::Increment` (`codec.rs:145-152`) → `Increment{result:new_value}` (absolute). A signed delta
landing a counter at 0 emits `result:0` ⇒ both appliers (`mutation_core.rs:326` and the owned twin
`persistent_artrie/mutation_core.rs:405`) take the DELTA arm for an absolute-0 ⇒ divergence.
> **Encoding fix.** Replace `RecoveredOperation::Increment{result:i64}` with an explicit discriminant: `outcome:
> IncrementOutcome` where `enum IncrementOutcome { Delta, Absolute(i64) }` (or `result: Option<i64>`: None=delta,
> Some(v)=absolute incl. 0). `recovered_operations_from_record`: `WalRecord::Increment{delta,result}` →
> `Increment{delta, outcome: Absolute(result)}`; `BatchIncrement` (`:347-354`) → `Increment{delta, outcome: Delta}`.
> Appliers: `mutation_core.rs:319-337` → `match outcome { Delta => try_increment_impl_no_wal(term, delta),
> Absolute(v) => insert_impl_no_wal_with_value(term, value_from(v)) }`; owned twin `:399-418` similarly.
> **WAL-format/codec implication: NONE** — the two on-disk record types already encode the distinction (`Increment`
> has `result:i64`; `BatchIncrement` has only `(term,delta)`; codec `:341-348`/`:292+`). The collision is purely in
> the recovery-side `RecoveredOperation` mapping. NOT gated by the bump → lands at DG-DECODE (≤DG1). (Overlay durable
> increments log `BatchIncrement` deltas `lockfree_cas.rs:1536` — always Delta; only the OWNED `increment` path
> `atomic_ops.rs:83` logs a single-op `Increment{result}` — always Absolute. Post-fix each is correct.)

---

## §7. SELF-RED-TEAM

### 7.1 THE load-bearing claim: "v3 WAL ⟺ every record is from the ranked overlay path"
Attacked by enumerating EVERY WAL-append site in char (`rg append_to_wal*`):

**Gated/safe:** insert/insert_with_value/remove (`mutation_api.rs:28,66,106` → ranking overlay producers);
increment/upsert/get_or_insert (`atomic_ops.rs:40,151,260` → ranking routes); compare_and_swap (`:197` REJECTS — no
WAL write); batch_insert (`batch_insert.rs:28` → routed inserts); commit_document (`document_tx.rs:321` REJECTS
under overlay). Begin/Commit/BatchInsert/AbortTx only on the owned (non-overlay) path.

**Arbitrary-V fallthrough** (`insert_with_value:72`, `increment:44`, `upsert:155` fall to the OWNED unranked body
when the route returns `None` for arbitrary V): `route_overlay()` is FALSE for arbitrary V (the flip
`enable_lockfree()`s only for `V ∈ {(), u64}`; `enable_lockfree` is the only `lockfree_root` setter `:160`) ⇒
arbitrary-V keeps `lockfree_root=None` ⇒ never enters §1.1a's fresh-v3 ctor step ⇒ its file stays v2 ⇒ an
arbitrary-V unranked record never lands in a v3 file. **Load-bearing coupling — D2.6 mandates invariant
`V3WalImpliesOverlayLive`** (a v3 active WAL ⟹ `route_overlay()` true): `debug_assert!(self.route_overlay())` at
every overlay producer's WAL-append + a ctor assert that the fresh-v3 step ran iff `route_overlay()`.

**THREE genuine hazards (guarded, not hand-waved):**
1. **`merge_lockfree_values_to_persistent:1897` + `merge_lockfree_to_persistent` write UNRANKED records, NOT
   route_overlay-gated.** They drain the overlay into the OWNED tree via `append_to_wal(BatchInsert/BatchIncrement)`
   (no rank). Callers today: tests only (`dict_impl_char.rs:3134`, vocab `:1539`). HAZARD: a merge while the active
   WAL is v3 writes an unranked v3 record ⇒ §2 DROPS it ⇒ silent loss. **Guard:** fail-closed at the top of both —
   `if self.route_overlay() { return Err(InvalidOperation("merge_lockfree_*_to_persistent is owned-regime only")) }`.
   Cite `lockfree_cas.rs:1872-1914`.
2. **Idempotent insert/remove arms emit a CommitRank** (`:383` `generation = lockfree_root.load().version()`,
   ranked via `:388`) — so NOT an unranked-in-v3 orphan; §2 KEEPS it. **This CONTRADICTS the D2/D2.5
   "idempotent-no-rank" prose — errata.** Resolution: the idempotent op is a confirmed no-op on an already-present
   term; it IS acked, so by R3 it must be ranked. D2.6 keeps the rank but stamps a freshly-claimed `commit_seq`
   (monotone), NOT `root.version()`. Not a hazard; corrected here to match `:383-391`.
3. **Per-segment archive — a v3 data record's rank in a v2 segment:** impossible (ranks only in v3 files; v2 segments
   have none; the flip forces a file boundary so a segment is version-homogeneous by C1). ✓

**Other paths:** Checkpoint records are markers → `vec![]` (`:359`), unranked irrelevant. **Eviction emits NO WAL
record** (grep `*evict*` empty) — mutates the buffer/registry, not the WAL ⇒ cannot inject an unranked v3 record. ✓
Group-commit (S#7): a rank batched into an un-synced batch ⇒ `append_commit_rank` Err ⇒ NOT acked ⇒ orphan ⇒
dropped. ✓

**Verdict:** "v3 ⟺ ranked overlay" HOLDS conditional on three guards: (i) the fresh-v3 ctor step gates on
`route_overlay()`; (ii) `merge_lockfree_*` reject under `route_overlay()`; (iii) cas/commit_document already reject.
With these, every v3 record is a ranked overlay op or a marker. Unranked-in-v3 ⟺ a two-append orphan ⟺ correctly
dropped.

### 7.2 Other attacks
- **S-A — flip's fresh-v3 ctor step crashes mid-rotate.** Crash between rename and new-header-write ⇒ no active WAL
  ⇒ reopen `open_or_create` creates a fresh file; the ctor re-runs §1.1a (`route_overlay()` true ⇒ v3); the v2
  archive recovers per-segment (KEEP). ✓ (the rotate is the primitive the owned checkpoint already trusts).
- **S-B — floor carry across truncate writes a STALE floor.** D2.6 sets `header.commit_seq_floor = reclaimed_max`
  IN the checkpoint path before truncate/rotate (§4.1); monotone `set_commit_seq_floor` guards (floor-too-high is
  safe — drops more, never resurrects). ✓
- **S-C — map-bound scan fallback misses rotated ranks.** Only ranks already reclaimed (covered by a prior monotone
  floor) get rotated; the active-WAL scan covers the un-rotated tail. ✓
- **S-D — IncrementalRecovery per-window reorders across windows.** Windows are LSN-contiguous and commit_seq
  monotone ⇒ order preserved; never-checkpoint-v3 fail-closed avoids OOM. ✓
- **S-E — D6 changes `RecoveredOperation` (pub type).** Source-compat break for external consumers; internal
  recovery type, no documented external use, gated by DG-DECODE + codec round-trip tests. Acceptable (pre-flip).

---

## §8. TLA invariants + `_Unsafe*.cfg` controls
Extend → `DurableGlobalOrderD26.tla`. SIMPLIFY vs D2.5 (drop per-window `regime` + reconstructed watermark; use
per-file/segment `version`). Model: `version: [Files -> {1,2,3}]` (immutable per file, flip creates a new file at
3); `value: [Terms -> Int]`; drop = `ranked ⇒ keep@commit_seq ; (¬ranked ∧ version≥3) ⇒ drop ; (¬ranked ∧
version≤2) ⇒ keep@lsn`; `floor' = Max({commit_seq(r): r ∈ committedOps, r.data_lsn ≤ checkpoint_lsn})`; `Restart`
seeds `nextCommitSeq' = Max(floor, scanMax)`; `Archive` unions segments tagged by file version, reconciles with
`checkpoint_lsn=0`; `IncrementOutcome ∈ {Delta, Absolute}`.
**Invariants:** carried `ReplayEqualsCommittedVisible`, `NoLostNetWrite`, `NoResurrectionOnReplay`, `DurablePrefix`,
`CommitSeqMonotone` (UNCHANGED), `ReplayEqualsCommittedValue`, `FloorDominatesSubsumed`, `SeedAboveDurable`,
`NoUnconfirmedWins`, `ArchiveNoResurrection`, `AckImpliesRanked`, `NoUncommittedTxReplay`, `NoVersionMix`. NEW:
`V3ImpliesRanked` (every committed record in a v3 file carries a rank), `V2KeepsUnranked`, `IncrementOutcomeDistinct`
(D6), `FlipCreatesFreshV3`.
**Negative controls (each MUST fire; register in `scripts/verify-formal-correspondence.sh`):**
`_UnsafeKeepV3Orphan`, `_UnsafeDropV2Unranked`, `_UnsafeMixedVersionFile`, `_UnsafeArchiveOneVersion`,
`_UnsafeIncrementSentinel`, `_UnsafeUnrankedIncrement`, `_UnsafeGlobalFloor`, `_UnsafeNoFloorCarry`; carried
`_UnsafeAckBeforeRank`, `_UnsafeTxIgnored`, `_UnsafeVersionMix`, `_UnsafeSplitLP`, `_UnsafeRawLsnPaths`.

---

## §9. DG phase order (summary)
```
DG0       floor field + map + rotate/truncate carry         [V2, reversible]  → floor round-trips
DG1       (1a) single-LP insert + increment-rank+gen-thread [V2, reversible]  → prefix-split PASS, CommitSeqMonotone
DG-DECODE D6 IncrementOutcome encoding fix                  [no codec, rev]   → absolute-0 vs delta-0 distinct
DG2       reclaimed-set floor, both checkpoint paths        [V2, reversible]  → post-checkpoint reseed PASS
────────────────────────  ONE-WAY GATE  ────────────────────────
DG-RECON  header 2→3 ∧ commit_seq stamp (8) ∧ version-gated [V3, FORWARD-ONLY]
          reconcile ∧ R5 guard ∧ fresh-v3 ctor ∧ §7.1 guards                  → v3-orphan-drop, v2-keep, mixed-refusal;
                                                                                NoUnconfirmedWins/AckImpliesRanked + controls FIRE
DG-PATHS  per-segment-version archive ∧ Incremental         [forward]         → archive-orphan-drop, never-checkpoint; AllPathsAgree
          per-window ∧ migrate_v2_to_v3
DG-TX     tx-gating in reconcile (R6)                        [forward]         → torn-tx-batch, aborted-tx PASS
DG-FORMAL TLA DurableGlobalOrderD26 + all controls          [HARD GATE — any _Unsafe PASS ⇒ STOP]
DG-SOAK   ≥50× real-disk + 3 new scenarios                  [reversible until the flip flag flips]
```
Green-gate guards in DG-RECON (§7.1): merge-bridge overlay-reject; `V3WalImpliesOverlayLive` debug_asserts;
idempotent-arm commit_seq claim.

---

### Critical Files
- `src/persistent_artrie_core/recovery.rs` — filtered reconcile (`reconcile_lww:253-298` + version-gated drop at
  `:286`); D6 (`recovered_operations_from_record:347-354`); archive (`rebuild_from_wal_segments:1443-1487`
  union-collect + per-segment `read_header`); `IncrementalRecovery:856-969` per-window+fail-closed; base
  `redo_phase:756` stays raw.
- `src/persistent_artrie_char/lockfree_cas.rs` — 8-site commit_seq stamp (`:363,383,540,1659,1757`,
  `insert_lockfree_recursive:928`); increment gen-threading (`try_increment_cas:1353/:1441` → `_inner`;
  `try_increment_cas_durable:1547`); §7.1 hazard-1 guards on `merge_lockfree_*:1872/:1897`.
- `src/persistent_artrie_core/wal/writer.rs` — R5 guard `open:116`; D4 floor-carry `rotate_to_archive:458`
  (preserve version+floor) + `truncate:352-356`; `set/get_commit_seq_floor` (mirror `set_min_lsn:269`); §1.1a
  fresh-v3 create.
- `src/persistent_artrie_core/wal/header.rs` — `VERSION 2→3` `:38` (DG-RECON, one-way); `commit_seq_floor` bytes
  20..28 (`to_bytes:66`/`from_bytes:91`); NO Checkpoint regime stamp.
- `src/persistent_artrie_char/mmap_ctor.rs` — §1.1a fresh-v3 ctor step (after `open_or_create_async_wal:327`);
  thread `wal_version` into `replay_records_lww:403,597`; `recover_from_archives:1106-1160` per-segment +
  `max_commit_seq`. Twins: `io_uring_ctor.rs:227`; appliers `mutation_core.rs:326` + `persistent_artrie/mutation_core.rs:405`;
  flip wiring `overlay_write_mode.rs:83`; floor source `persist.rs:147/:579`.
```
