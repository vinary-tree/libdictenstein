# Final red-team of the DG-RECON irreversible gate (D2.6) — BLOCKING holes → D2.7

**2026-06-02.** The user requested "one more red-team on the irreversible DG-RECON gate" before implementing.
**It caught a catastrophic CROSS-CODEBASE break (F1) that all 8 prior char-focused red-team agents missed.**
Verdict: **NOT SAFE TO IMPLEMENT as specified.** The (1a) write-path spine remains PROVEN; the holes are in the
regime mechanism (cross-codebase) and the DG phase ordering — fixable in a focused D2.7.

## Blocking findings (agent afcfc23f)

**F1 [CRITICAL] — the R5 `WalWriter::open` guard bricks BASE and VOCAB on reopen.** `WalHeader`/`WalWriter`/
`AsyncWalWriter` live in SHARED `persistent_artrie_core/wal/` with ONE global `VERSION` (`header.rs:38`). Base
(`persistent_artrie/mmap_ctor.rs:378`), vocab (`persistent_vocab_artrie/mmap_ctor.rs:238`), and char ALL funnel
through `AsyncWalWriter::open_or_create` → `WalWriter::open` (`writer.rs:101`) — exactly where the proposed guard
`header.version < VERSION ⇒ UnsafeVersionMixing` lands. The moment DG-RECON sets `VERSION=3`, every pre-existing
base/vocab WAL (legitimately v2) is REFUSED at open. **regime≡version assumed the WAL version is char-specific; it
is GLOBAL.**

**F1b [CRITICAL] — fresh vocab/base WALs become v3-unranked after the bump.** `WalWriter::create` → `WalHeader::new()`
→ `version: VERSION`. After the bump a fresh base/vocab WAL is v3, but they write UNRANKED records (vocab's overlay
`insert_cas` is a pure in-memory cache emitting NO WAL/CommitRank; vocab `merge_lockfree_to_persistent` writes
unranked `Insert`; base writes plain inserts). So base/vocab violate `V3ImpliesRanked` wholesale; if ever routed
through the unified reconcile, all their data DROPS. `V3WalImpliesOverlayLive` is globally false.

**F2 [CRITICAL] — DG-RECON→DG-PATHS data-loss/resurrection window (C5a one level up).** The version-gated
`reconcile_lww` is wired ONLY into the clean-open `replay_records_lww` (`mmap_ctor.rs:403,597`). The
corruption/torn-tail path `rebuild_from_wal` (`recovery.rs:503-571`, reached on a NORMAL crash with a torn tail,
`:458`), `recover_from_archives` (`mmap_ctor.rs:1138`), and `IncrementalRecovery` (`:856`) apply RAW with no gating
— and D2.6 deferred fixing them to DG-PATHS. A crash+corruption-recovery between DG-RECON and DG-PATHS replays an
unranked orphan in-order ⇒ resurrects a removed term. **The archive/rebuild/incremental gating MUST be IN DG-RECON.**

**F3 [HIGH — rollback] — overlay-era pruning deletes the pre-flip v2 archives the rollback depends on.**
`prune_segments_if_needed` (`writer.rs:510`, called in `rotate_to_archive:469` on EVERY rotation) blindly
`fs::remove_file`s the oldest segments by `max_segments`/`max_archive_bytes`, version-unaware. During v3 overlay
operation it prunes exactly the pre-flip v2 segments the §5 rollback ("re-open the pre-flip v2 archives") needs.

**F4 [MEDIUM] — idempotent-arm (`lockfree_cas.rs:383`) commit_seq claim-point unspecified.** Repurposing it to a
"freshly-claimed commit_seq" without a specified claim-POINT lets a no-op idempotent insert claim AFTER a concurrent
real Remove on the same term and out-order it (A.5-class inversion under the global `fetch_add`) ⇒ resurrection.
Needs an explicit claim-point + ordering proof for this arm (the §3.1 rule covers only the 4 real producers).

**Verified SOUND:** F5 (crash mid fresh-v3 ctor / mid-rotate is safe — the rotate is the trusted owned-checkpoint
primitive). F6 (within char, §7.1's append-site enumeration IS complete — eviction emits no WAL, `apply_*_no_wal`
doesn't re-append, fault-in doesn't append; the holes are cross-codebase + cross-phase, NOT char-internal).

## → D2.7 fix direction (focused; carries the proven (1a) spine + reconcile-apply-all + floor + migrate + sentinel)

**The convergent regime answer (3rd revision): a DEDICATED, durable, per-FILE `rank_regime: u8` header field —
NOT the shared VERSION, NOT inferred-per-window.** This satisfies every constraint the red-teams found:
- durable (closes A#6) + per-file-not-per-window (closes A#1/A#3) + DEDICATED-not-shared-version (closes F1/F1b) +
  defaults `Owned=0` for all legacy/base/vocab files (they have 0 in the reserved bytes → automatically Owned →
  never dropped).
- **Do NOT bump the global `VERSION`** (avoids F1 entirely — base/vocab keep opening their files). Put `rank_regime`
  in the header's reserved bytes (like `commit_seq_floor`); old readers ignore reserved bytes. The
  CommitRank.generation semantic (root.version → commit_seq) is disambiguated by `rank_regime == Overlay`, not by
  VERSION. The flip sets `rank_regime = Overlay` on the FRESH char overlay WAL at creation; base/vocab/owned files
  stay `Owned`.
- The R5 guard + the §2 drop rule key on `rank_regime`: refuse appending an Overlay record to an Owned file (and
  vice versa); drop unranked IFF `rank_regime == Overlay`. Base/vocab (`Owned`) keep all unranked — never bricked,
  never dropped.

**F2:** fold the archive (per-segment `rank_regime` + filtered reconcile), `rebuild_from_wal` (torn-tail), and
`IncrementalRecovery` gating INTO the atomic gate (DG-RECON), not DG-PATHS — a v3/Overlay record must never be
recoverable by an ungated path.

**F3:** version/regime-aware pruning that PINS the pre-flip Owned archives (or a pre-flip-archive retention flag),
so the rollback's v2 archives survive overlay-era rotation. Re-validate the §5 rollback story against pruning.

**F4:** specify the idempotent-arm claim-point (claim at the same CAS-retry-loop-top as the real producers, before
the present-check) + an ordering proof that a no-op cannot out-order a concurrent real op on the same term.

**Cross-codebase mandate for D2.7:** explicitly validate that base + vocab are UNAFFECTED — they default `Owned`,
never write `rank_regime=Overlay`, their recovery keeps unranked records, and the shared `WalWriter::open` guard
permits them. Every prior cycle was char-only; F1 proves the cross-codebase dimension is load-bearing.

## ADDITIONAL KINK (orchestrator independent recon, 2026-06-02) — the VERSION-bump vs rank_regime vs compat needle

Confirmed by grep: vocab + base emit ZERO `CommitRank` (inherently Owned); base calls `AsyncWalWriter::create`
DIRECTLY (`persistent_artrie/mmap_ctor.rs:121,208`, bypassing `wal_managed`), so the regime guard MUST be in
`WalWriter::{open,create}`. Header: `VERSION=2`, `MIN_SUPPORTED=1`, `reserved[44]` at bytes 20..64;
`commit_seq_floor` (D2.6) would take 20..28, so `rank_regime` fits at byte 28.

**The needle D2.7 MUST thread explicitly (a real kink, not hand-waveable):** the ONLY fail-closed signal a
PRE-EXISTING (current v2) binary checks is `version > VERSION` (`header.rs:82`). `rank_regime` in reserved is
INVISIBLE to old binaries (they ignore reserved). Therefore:
- **If D2.7 does NOT bump the global VERSION** (keeps 2 + `rank_regime` in reserved): base/vocab are fully safe
  (forward AND rollback — their v2 files always open). BUT a char-Overlay file is NOT fail-closed against an old
  binary: an old binary opening it reads version 2, ignores `rank_regime`, and char-reconciles it as Owned ⇒ KEEPS
  orphans ⇒ silent mis-recovery / possible resurrection.
- **If D2.7 bumps the global VERSION to 3**: char-Overlay files ARE fail-closed against old binaries (`version 3 >
  2` ⇒ refused), BUT base/vocab's NEW files are also v3 ⇒ an old binary (rollback) opening them is refused ⇒
  base/vocab bricked on rollback (F1 redux on the rollback path).
You CANNOT make a current-v2 binary check a field it predates ⇒ the tension is fundamental for current binaries.

**Candidate resolutions D2.7 must evaluate + PICK (with a proof + a cross-codebase impact statement):**
(1) NO bump + `rank_regime` in reserved + the rollback procedure NEVER opens the live Overlay WAL with an old binary
    (it opens the pre-flip Owned archives, §5) — the residual (operator opens the Overlay WAL with an old binary) is
    a documented operational constraint, NOT a silent normal-operation bug. Add a tripwire if possible.
(2) A distinct HEADER MAGIC for Overlay files (bytes 0..8): if `from_bytes` validates `magic`, an old binary refuses
    an Overlay file on magic-mismatch (fail-closed) WITHOUT a global VERSION bump (base/vocab keep the standard
    magic, unaffected). VALIDATE whether `from_bytes` checks magic and whether a per-regime magic is clean.
(3) Bump VERSION but make base/vocab create files at a PINNED v2 (per-codebase create-version), so only char-Overlay
    files are v3 — decoupling the create-version from the global max-readable VERSION. Assess feasibility.
D2.7's red-team must attack the chosen resolution's forward AND rollback compat across all three codebases.

## D2.7 RED-TEAM VERDICT (agent a1a708b3) — needs D2.8. §A idempotent-no-rank is CORRECT; two structural gaps remain.

**VALIDATED SOUND:** §A read-before-append race (Ok(false) asserts "present at the read's linearization point" —
not a lost insert; same as the existing `remove_cas_durable:498` absent-fast-path); §A mark-but-no-rank
floor/shared-lsn (each append gets its own lsn; a marked-but-unranked lsn has no commit_seq ⇒ excluded from the
reclaimed-set floor max — must become a TLA invariant); multi-gen-Overlay prune (binary Owned-vs-Overlay partition);
base never calls the shared `rebuild_from_wal_segments` (only char `:1138` + re-export, not a call). The (1a) spine
+ CommitSeqMonotone remain untouched.

**P0 BLOCKING #1 — the recovery-gating ENUMERATION is STILL incomplete (3rd red-team to find "another ungated
path").** `open_with_recovery_config` (char `mmap_ctor.rs:729`, PRODUCTION-reachable via `:665`) has its OWN inline
rebuild loop (`:794-920`) applying Insert/Remove/Increment RAW (`insert_impl_no_wal`) — NOT in D2.7 §2's list. Plus
the PUBLIC `RecoveryManager` raw paths (char `recovery.rs:464` + `:503`, both `record_to_operations`-raw). On a
corrupt Overlay file each replays unranked orphans in-order ⇒ resurrection; `NoUngatedV3Recovery` as scoped would
PASS while these resurrect. **→ D2.8 STRUCTURAL FIX: stop enumerating. Route EVERY char recovery record-application
through ONE choke-point `apply_recovered_records(records, regime, tx_states)` that runs the regime-gated reconcile.
A single gated funnel ends the whack-a-mole — there is then ONE place to gate and no enumeration gap.** Cite
`mmap_ctor.rs:729/794-920/978/1106`, `recovery.rs:412/464/503`.

**P0 BLOCKING #2 — the magic needle is a CLOSEABLE kink, NOT an acceptable constraint (per "no deferrals").**
`WalHeader::from_bytes:73` DOES validate magic. A DUAL-MAGIC tripwire: the Overlay flip writes a distinct magic
(e.g. `PARTWALO`); OLD binaries fail-close on it (magic mismatch) WITHOUT a VERSION bump (base/vocab/char-owned keep
`PARTWAL\0`, unaffected); NEW binaries accept a magic SET `{PARTWAL\0, PARTWALO}` so same-binary recovery/migration
reads (`WalReader::new`/`read_header`, which route through the same `from_bytes`) still read Overlay files freely.
This closes the silent-mis-recovery gap (old binary reads an Overlay file as Owned ⇒ keeps orphans ⇒ resurrection)
that a backup/monitoring/mixed-deploy reader hits in NORMAL ops (not just operator error). **→ D2.8 adopts dual-magic.**

**P0-residual — §A read-before-append AMBIGUITY.** The current `insert_cas_durable` appends the data record at
`:329` BEFORE the CAS loop, so "read-before-append" requires HOISTING the membership check ABOVE `:329`. **→ D2.8
states the hoist explicitly (preferred) OR mandates the mark-but-no-rank fallback** (which applies to today's
append-then-loop shape). Cite `:329/:375/:520/:556`.

**P2 (specify in D2.8) —** migrate of a source-Owned file with LEGACY root-version CommitRanks must RE-RANK them
into commit_seq space (or they collide) — §6 omits this. The flip primitive (`ensure_overlay_wal_regime`,
`open_with_regime`, `RankRegime`) is entirely UNBUILT — crash-mid-flip idempotency (§1.3's "Owned-tail-after-half-
flip ⇒ rotate again") MUST be in the TLA flip model + a real-disk crash-mid-flip soak. Owned-producer-opens-Overlay
→Err: document which tools must use `WalReader` (LOW).

**D2.8 = D2.7 + {single gated recovery choke-point (closes the enumeration gap structurally); dual-magic tripwire;
§A hoist explicit; migrate legacy-rank re-ranking; flip-primitive crash-mid-flip TLA+soak}. The (1a) spine carries.**


Then: red-team D2.7 (cross-codebase focus + this needle) → commit good F0 parts → foreground DG implementation,
owner-gated at the irreversible flip. D2.6 NOT implemented. `db7cb2d`+`cf1f80c` safe; F0 hacks untouched (captured
to `f0-working-tree-recovery.patch`); nothing reverted.
