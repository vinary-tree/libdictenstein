# Design: PRINCIPLED fix for the Order-A durable-write replay-order bug

**Repo:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein` · 2026-06-02 · implementation-ready. Fixes a real
data-loss bug blocking R-B (overlay delete) and the lock-free flip. ZERO new unsafe. Phased, reversible,
green-gated. Persisted from the Plan-agent design (full prose in the session transcript).

## (1) Root cause + the invariant violated
Order-A writes in two UNLINKED steps: **append (LSN order `≺_LSN`)** — `append_to_wal_returning_lsn`
(`wal_helpers.rs:73`)→`WalWriter::append` (`writer.rs:169`, `lsn=next_lsn.fetch_add(1)` then write under file
lock) — then **visibility CAS (commit order `≺_CAS`)** — the separate `compare_exchange` on `lockfree_root`
(`insert_cas_durable:288`/`remove_cas_durable:438`). Nothing forces `≺_LSN == ≺_CAS`. Recovery replays in physical/
LSN order into the OWNED tree (`mmap_ctor.rs:400-423`, every ctor sets `lockfree_root:None`; `enable_lockfree`
rebuilds overlay FROM the owned root, so REC-A inherits the owned replay), strict last-writer-by-position. The
committed VISIBLE overlay is LWW under `≺_CAS`.
**Violated invariant `ReplayEqualsCommittedVisible`:** LSN-ordered-replay membership must equal CAS-order committed
visible membership. **s019 trace:** WAL `…Insert@352, Remove@356`; CAS last-writer = Insert (PRESENT) but
highest-LSN record = Remove → replay ends absent → acknowledged net-present key LOST = data loss. REAL bug (the
quiesced overlay is a well-defined committed history), not a test-oracle bug.
**Why insert-only is accidentally safe:** inserts are idempotent+monotone (any replay order → same set); the
existing `concurrent_durable_writers_all_survive_reopen` passes. Remove makes `insert∪remove` order-sensitive +
non-monotone → divergence becomes loss. **General Order-A hazard for ANY non-monotone op (remove, future
upsert/decrement-increment).** Existing specs miss it: `LockFreeOverlayRemoveCas` has no WAL/replay;
`LockFreeDurableCheckpoint!NoLostWriteUnderLockFreeCommit` tracks LSNs as opaque tokens, never collapses two LSNs
of one term through replay. **Secondary hazard H2:** the direct Immediate append does `fetch_add` then separately
takes the file lock (`writer.rs:170` vs `:207`) → WAL physical order can differ from LSN order; the explicit
`(gen,lsn)` sort (below) fixes this too (group-commit already orders via `submit_order`).

## (2) Alternatives — verdict
- **A (linearize append with CAS):** A1 CAS-then-log = visible-before-durable, REJECTED (breaks Order-A/DurablePrefix);
  A2 append-then-CAS-loser-noop: premise FALSE — the appended record's LSN is fixed pre-CAS, and a losing-then-
  winning interleave still inverts; a losing Remove is NOT a replay no-op (it's the s019-erasing record). REJECT.
- **B (per-key lock around append+CAS):** correct + zero-format-change BUT gives up lock-free publication +
  serializes the contended same-key workload. **FALLBACK only.**
- **D (content-only replay reconciliation):** CONFIRMED IMPOSSIBLE — `≺_CAS` is not recorded anywhere durable; the
  natural "highest-LSN per term" rule gives the WRONG answer (picks Remove@356). Any real fix must add ≥1 ordering
  datum to the durable record ⇒ that IS approach C.
- **E (defer remove to owned path):** blocks R-B/flip. REJECT.
- **C′ (RECOMMENDED): per-term commit generation in a versioned record** — lock-free-preserving, general,
  additive/back-compat. Below.

## (3) RECOMMENDED FIX — C′
**Idea:** durably record enough of `≺_CAS` to recover per-term LWW. The minimal statistic is a **per-term commit
generation** assigned at CAS-success; replay picks, per term, the record with **max generation** (ties by lsn) and
applies only its effect. Generation in `≺_CAS` order ⇒ "max gen per term" == CAS-order last-writer == committed
visible ⇒ `ReplayEqualsCommittedVisible` by construction, for ALL mixed histories (future-proofs upsert/increment).

**Realization (single data append + a conditional rank marker; Order-A preserved):**
1. Step 1 (unchanged): append+sync the data record `Insert{term,value}`/`Remove{term}` → `lsn` (durable before
   visible).
2. Step 2 (unchanged): CAS loop. On the WINNING CAS, read the published leaf's node version `g = node.version`
   (`overlay/node.rs`: `as_final`/`as_non_final` do `version = loaded_child.version + 1`, so successive winners on
   a term's leaf are strictly increasing — the monotonicity theorem §3.6).
3. Step 2.5 (NEW, before ack, Order-A-preserving): append+sync `CommitRank{data_lsn:lsn, term, generation:g}`
   binding the durable data record to its commit generation.
4. Step 3 (unchanged): `mark_committed(lsn)` AND `mark_committed(rank_lsn)`; cache update; return.
**Replay (NEW reconcile, both `mmap_ctor.rs` sites → ONE shared `replay_records_lww`):** pass 1 build
`rank: HashMap<Lsn,u64>` from CommitRank records; pass 2 per-term keep the data record with max
`generation_of(lsn)=rank.get(lsn).unwrap_or(lsn)` (ties by lsn), apply its effect via `*_impl_no_wal`. `CommitRank`
is a membership no-op (like `Checkpoint`). Honor the `checkpoint_lsn` skip by `data_lsn`. The explicit `(gen,lsn)`
sort also closes H2 (no longer trusts physical order). Key the term map on raw `Vec<u8>` (avoid lossy collisions).

**Why a separate marker, not a field in the data record:** `g` is unknown at step 1 (Order-A forces the data record
out before the CAS); back-patching a synced frame violates append-only durability. The node-version + post-CAS
rank-marker needs no back-patch. (Rejected: a global commit counter patched into a reserved slot — needs back-patch.)

**§3.6 LOAD-BEARING THEOREM (gen monotone in `≺_CAS` per term):** read `g` from the EXACT leaf Arc the op published
(returned by `try_*_lockfree_path`; nodes immutable, only the root pointer swaps) — NOT by re-walking the live root
(stale-read hazard). Idempotent `AlreadyExists`/`AlreadyAbsent` arms publish nothing → record `g = current published
leaf.version` under the same `enter_read()` pin; if it races a later op, the later op has a strictly higher version
and wins replay — the no-op being out-ordered is harmless. Loom-checked + TLA `CommitCas` action.

**Signatures/sites:** (a) `codec.rs` — add `WalRecordType::CommitRank=15` + variant `CommitRank{data_lsn:Lsn,
term:Vec<u8>, generation:u64}` + serialize/deserialize/`serialized_size`/`TryFrom` arms + `WalHeader` version bump.
Existing variants byte-identical ⇒ existing WALs read unchanged. (b) `wal_helpers.rs` — reuse
`append_to_wal_returning_lsn` (optional thin `append_commit_rank` wrapper). (c) `insert_cas_durable:289`/(d)
`remove_cas_durable:439` — emit CommitRank after CAS (read published-leaf version), mark both LSNs; extend
`LockfreeRemoveResult::Removed(Arc<OverlayNode<V>>)` to carry the leaf. (e) `mmap_ctor.rs:400-423`+`:688-709` →
shared `replay_records_lww` (removes the duplication = no-drift). (f) `recovery.rs:182` — `CommitRank ⇒ vec![]`
no-op arm (preserves REC-A replay-completeness). (g) `mark_committed` UNCHANGED (keys on data LSN; watermark/
`NoLostWriteUnderLockFreeCommit` untouched).

**§3.4 WAL-format change is REAL + REQUIRED** (§2D proves visibility order isn't otherwise durable). Additive +
back-compat: **backward** (new code, old WAL) — no CommitRank ⇒ `generation_of=lsn` fallback = today's behavior;
existing logs are insert-only/pre-remove so the fallback is safe for every log that can exist; NO migration.
**forward** (old code, new WAL) — bump `WalHeader` version so an old binary refuses the file fail-closed (not
silent truncation). Opt-in (enable_lockfree + durable policy), pre-flip; document the one-way header bump in
GAP_LEDGER/UNSAFE_BOUNDARY. Matches the additive Version* records 12-14.

**§3.5 Order-A/watermark preserved:** data durable-before-visible (steps 1→2 unchanged); ack now also waits for the
rank sync (STRENGTHENS Order-A); `checkpoint_lsn`=committed watermark unchanged; IoError window — data durable, no
rank, watermark not advanced → recovery replays under `gen=lsn` fallback (correct for the single uncommitted op).
**§3.7 General** (records `≺_CAS` for every op; reconcile is content-agnostic). **§3.8** REC-A auto-fixed (overlay
rebuilds from corrected owned replay); fix touches ONLY the owned replay; REC-B must reuse the reconcile.

## (4) Formal re-proof
**NEW spec `LockFreeOverlayDurableReplay.tla` (+`.cfg`+`_Unsafe.cfg`)** — bounded (Terms={a,b}, MaxOps≈4, lsns/gens
1..6). Vars: `nextLsn`,`nextGen`,`wal`(seq of Insert/Remove/Rank), `present`/`removed` (visible, same abstraction as
`LockFreeOverlayRemoveCas`), `committed`, `replayed`. Actions: `Append(t,kind)` (step1, no visibility change);
`CommitCas(t)` (step2, update present/removed LWW + assign `gen'=nextGen[t]+1`); `AppendRank(t)` (step2.5, then
`committed'∪={lsn,rankLsn}`); `CrashRecover` (replayed[t]=effect of max-gen data record, gen=rank else lsn, ties by
lsn, over committed ≤ frontier). **`USE_COMMIT_RANK=TRUE`** (design, replay by gen) / **`=FALSE`** (`_Unsafe.cfg`
negative control, replay by lsn = the broken scheme). Invariants: **`ReplayEqualsCommittedVisible`** (∀t:
(t∈replayed)<=>(t∈present)) [headline]; `NoLostNetWrite` (present⇒replayed, the s019 direction); 
`NoResurrectionOnReplay` (¬present⇒¬replayed); reuse `DurablePrefix`. **The `_Unsafe.cfg` MUST violate
`ReplayEqualsCommittedVisible` via the s019 trace** (Append Insert@a, Append Remove@b>a, CommitCas(Remove) then
CommitCas(Insert) ⇒ present={s} but lsn-replay ends Remove ⇒ replayed={} ⇒ FALSE<=>TRUE). If the unsafe cfg passes,
the control is broken ⇒ fail the gate. Register in `verify-formal-correspondence.sh` SANY(`:252`)+TLC(`:303`)+
negative-control(`:335`). Do NOT weaken the existing specs (compose, don't replace).
**Deterministic Rust regression** `replay_orders_by_commit_rank_not_lsn` (+ resurrection-polarity twin): force the
s019 interleaving deterministically (two `Barrier`s / a controlled scheduler so WAL=Insert@a,Remove@b while CASes
land Remove-then-Insert), drop-no-checkpoint, reopen, assert `contains("s019")==true`. FAILS pre-fix (OD2 reverted),
PASSES post-fix — the differential proves the test has teeth. Seeded + `#[serial]`.
**Empirical soak gate:** run `concurrent_durable_mixed_insert_remove_reopen_equals_live_set` (`lockfree_cas.rs:2213`)
**≥50× green** (Immediate + a GroupCommit twin). Pre-fix fails within ~48 attempts; post-fix ≥50× clean.
**Stay green:** `concurrent_durable_writers_all_survive_reopen` (insert-only), `recovery_replay_completeness_
correspondence`, `persistent_{artrie,wal,vocab_wal}_*` atomicity/recovery; full gate exit 0; 0 new unsafe.

## (5) Phased migration (each: nextest ≥2504 + verify-formal-correspondence exit 0 + unsafe-inventory exit 0; systemd 32G + real-disk; RUN_TLC=1 for the gate)
- **OD0** — WAL record (codec only): `CommitRank=15` type+variant+ser/de/size+TryFrom + `WalHeader` version bump +
  `recovery.rs` no-op arm + codec round-trip unit test (old-record bytes unchanged). No producer/replay change.
  Rollback: delete variant.
- **OD1** — Replay reconcile (consumer) behind fallback: both `mmap_ctor` sites → shared `replay_records_lww` with
  `(gen,lsn)` ordering, `gen=lsn` when no rank ⇒ behavior IDENTICAL for existing WALs. Unit tests on hand-built
  record vectors (incl. a synthetic rank). Gate: all recovery/atomicity correspondence green (proves behavior-
  preserving). Rollback: revert to in-order loop.
- **OD2** — Producers emit rank: wire insert/remove durable to append CommitRank after CAS (read published-leaf
  version), mark both LSNs, extend `Removed(Arc)`. Rollback: drop rank append (consumer falls back to LSN order).
- **OD3 — FORMAL re-proof (HARD GATE):** add `LockFreeOverlayDurableReplay.tla`+cfgs; `ReplayEqualsCommittedVisible`
  passes; `USE_COMMIT_RANK=FALSE` negative control FIRES on s019. If not → fix is wrong, STOP.
- **OD4 — Deterministic regression** (+resurrection twin): confirm it fails with OD2 reverted, passes with OD2.
- **OD5 — Soak gate:** the mixed soak ≥50× green (Immediate + GroupCommit). Unblocks R-B + the flip.
OD0-OD2 reversible by deletion; OD3-OD5 verification-only. **No irreversible step** except the `WalHeader` version
bump (fail-closed downgrade, gates an opt-in pre-flip feature — no released format broken).

## (6) Honest risks
1. **WAL-format change real+required** (§2D); additive versioned record + header bump; fail-closed downgrade is the
   residual edge (document in UNSAFE_BOUNDARY/GAP_LEDGER). If ANY format change forbidden → fallback B (accept
   same-key write serialization).
2. **Extra fsync per state-changing op** (Immediate): mitigate via group-commit batching the rank (production-
   recommended policy), or coalesce rank into the same group-commit batch (durable before ack); measure with
   `benches/lockfree_flip_benchmark.rs`, gate on acceptable regression. (B trades this for lock contention.)
3. **Gen-monotonicity is load-bearing (§3.6):** read `g` from the exact published LEAF Arc (not a re-walk/ancestor);
   loom schedule ("rank monotonic under concurrent same-key insert‖remove") + TLA `CommitCas` check it.
4. **Checkpoint/watermark/REC-A interaction:** watermark also marks rank LSNs (partial rank append ⇒ watermark stalls
   at data LSN, safe); checkpoint subsumes sub-watermark ranks (reclaim with data records); REC-A auto-fixed; REC-B
   must reuse the reconcile. `LockFreeDurableCheckpoint[Eviction]` specs stay green unchanged.
5. **Increment/upsert share the hazard + the fix** (reconcile is content-agnostic; wire their durable paths to emit
   CommitRank when they land — out of scope here, unblocked).
6. **Term keying:** reconcile keys winners on raw `Vec<u8>` (not lossy-UTF8) to avoid collisions.
7. **Two replay sites must not drift:** factor into ONE `replay_records_lww` (the TLA-corresponding impl); if the
   refactor is declined, patch both identically (a hazard the design recommends against).

### Critical files
- `src/persistent_artrie_core/wal/codec.rs` (CommitRank=15 type+variant+ser/de/size/TryFrom; `WalRecordType:15-86`,
  `WalRecord:90-204`, `serialize_payload:230`; `WalHeader` version)
- `src/persistent_artrie_char/lockfree_cas.rs` (emit CommitRank: `insert_cas_durable:289-302`,
  `remove_cas_durable:439-454`; `LockfreeRemoveResult::Removed(Arc):496`; soak `:2213`)
- `src/persistent_artrie_char/mmap_ctor.rs` (replay sites `:400-423`+`:688-709` → shared `replay_records_lww`;
  checkpoint skip `:402`/`:690`)
- NEW `formal-verification/tla+/LockFreeOverlayDurableReplay.tla`+`.cfg`+`_Unsafe.cfg` (`ReplayEqualsCommittedVisible`
  + s019 negative control; register in `scripts/verify-formal-correspondence.sh:252/303/335`)
- `src/persistent_artrie_core/recovery.rs` (`recovered_operations_from_record:182` — `CommitRank ⇒ vec![]`)
