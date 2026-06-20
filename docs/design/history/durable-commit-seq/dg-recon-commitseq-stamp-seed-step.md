# Implementation Design — DG-RECON Step "commit_seq stamp + seed-on-reopen" (D2.8 §1.4)

**Read-only design. 2026-06-02. Crate `libdictenstein`, char ARTrie.** Specifies the
producer-stamp + seed step, validates the uncommitted working-tree draft, and is deliberately
adversarial because this is data-loss-and-deadlock-critical. (Produced by a Plan agent; pending a
red-team pass — see §11.)

## 0. TL;DR verdicts

| # | Question | Verdict |
|---|---|---|
| A | CommitSeqMonotone for the winning iteration | **HOLDS** — single-root-CAS LP + re-claim-on-Conflict. Draft's per-iteration `fetch_add` placement is correct. |
| B | Idempotent arms ranking under loop-top commit_seq | **WRONG / DATA-LOSS in this step.** Ranking `AlreadyExists`/`AlreadyAbsent` reintroduces the D2.7 §A resurrection because the regime-gated Overlay-drop is NOT yet active (files are Owned). The draft's two idempotent arms must NOT rank here. |
| C | Seed formula + placement | Formula `max(header.commit_seq_floor, scan-max CommitRank.generation)` is correct. Draft has NOT implemented the seed (all 8 ctors still `commit_seq=0`). `commit_seq_floor` is currently always 0 (DG2 not landed), so seed reduces to scan-max — adequate only for the no-checkpoint soaks. |
| D | Increment cross-domain sort hazard | **UNSAFE to land "stamp the 4 producers" alone while increment stays unranked AND a file can mix insert+increment on one key.** Bring increment into the commit_seq domain in this step, or land the whole DG-RECON gate. |
| E | Inertness vs existing replay results | Stamp change is observably different (generation values change) but outcome-preserving for the real-writer arms of the s019 integration test — PROVIDED idempotent arms don't rank (B). The bugs in B/D slip past the current suite (coverage gap). |
| F | Deadlock / lock ordering | Draft is CLEAN: `fetch_add` lock-free, no faulting read added to the hot path, seed single-threaded at open. Draft correctly did NOT add the §A faulting present-hoist (the 75-min hang). |
| G | What's missing | The seed (3 char ctors), the increment decision, and scope contamination (the pre-existing F0 flip-routing files are in the working tree). |
| H | Gates | loom no-lost-write + appended-frontier control; reconcile unit tests; the two s019 integration tests; membership+counter soaks; full recovery+char suites; PLUS a NEW cross-restart reopen-then-write-same-key collision test + a NEW idempotent-resurrection control test. All `timeout`-wrapped, tee'd. |

**Bottom line:** the four real-writer arms' stamp is sound and monotonicity is proven. But the draft
**ranks the idempotent arms** (a re-introduced resurrection given the still-Owned/ungated reconcile)
and **omits the seed**, and the step as scoped collides with the unranked increment path. The
commit_seq stamp, idempotent-NO-RANK, increment generation-threading, and the regime-gated
Overlay-drop **must land together in DG-RECON** (the master memo's SEQUENCING REFINEMENT) — pulling
the stamp out alone, with idempotent arms ranking, is the sequencing the red-team already rejected.

## 1. Ground truth (code-read, current working tree)

Producers all CAS the single `lockfree_root` arc-swap:
- `insert_cas_durable` `lockfree_cas.rs:292`; append `Insert@:329`; loop `:344`; LP root CAS in
  `insert_lockfree_recursive` `:976` ((1a), 291cc12). Draft claims `commit_seq` at `:346`, stamps in
  `Inserted` `:367` and `AlreadyExists` `:391`.
- remove durable `:454`; hoisted faulting `present_before` `:498`; append `Remove@:520`; loop `:540`;
  LP `:653`. Stamps `Removed` `:551`, `AlreadyAbsent` `:580`.
- `insert_cas_with_value_durable` `:1622` (V=u64); append `:1668`; loop `:1679`; LP `:1710`.
- `upsert_cas_durable` `:1736` (V=u64); append `:1755`; loop `:1789`; LP `:1812`.

Increment UNRANKED: `try_increment_cas_durable :1547` appends `BatchIncrement@:1581`, publishes via
`try_increment_cas :1473` (CAS `:1487`), `mark_committed :1599`. **No `append_commit_rank`, no
commit_seq claim.**

`append_commit_rank` `wal_helpers.rs:88` appends `WalRecord::CommitRank{data_lsn,term,generation}`;
does NOT update `commit_seq_by_data_lsn`.

`reconcile_lww` `recovery.rs:257` takes `rank_regime`; Pass-1 builds `rank: HashMap<Lsn,u64>`; Pass-2
`g = rank.get(lsn)` else `Owned => lsn / Overlay => continue`; sorts `(generation,lsn)` stable.
Char caller `replay_records_lww` `mutation_core.rs:286` hard-codes `RankRegime::Owned` (drop inert).

Ctors (commit_seq=0, unseeded): `mmap_ctor.rs::open` (loop `:299–322`, replay `:403`),
`open_with_depth` (replay `:608`), `io_uring_ctor.rs::open` (loop `:124–150`, replay `:227`).
`WalReader::read_header` `reader.rs:103` is the cheap floor read.

Default `OverlayWriteMode::default()==OwnedTree`; `route_overlay()` false by default ⇒ durable
producers run only via `enable_lockfree()` opt-in / tests. "No production flip" holds in default config.

## 2. Question A — CommitSeqMonotone proof

Claim: if X's winning iteration claims `cseq_X`, Y's claims `cseq_Y`, and X's root CAS precedes Y's in
the single-root-CAS order `≺_CAS`, then `cseq_X < cseq_Y`.

Key fact (verified): every durable write — insert, remove, valued-insert, upsert, AND increment —
path-copies from the current root and publishes via `compare_exchange` on the ONE `self.lockfree_root`
(LPs `:976`,`:653`,`:1710`,`:1812`,`:1487`). Disjoint keys still contend on that pointer; the loser
sees a changed `current` and returns `Conflict`→`continue`. So `≺_CAS` is a single total order.

The claim is `fetch_add` on a DIFFERENT atomic, placed at the loop-top, re-run every iteration. The
only dangerous interleaving (claim-order vs CAS-order inversion):
- A claims `cseq_A=k`; B claims `cseq_B=k+1` (after A).
- B's CAS wins first (B ≺_CAS A): B publishes generation `k+1`.
- A's CAS sees B's root ≠ A's `current` ⇒ `Conflict` ⇒ `continue` (A's stale `k` is DISCARDED — Conflict
  skips to continue without stamping/appending).
- A re-loops: claims `cseq_A'=k+2` (fresh, higher), rebuilds off B's root, wins ⇒ generation `k+2`.

Only the claim of the iteration whose CAS WON survives into a CommitRank. Because `fetch_add` is
monotone and a winning CAS strictly follows every claim made in iterations that lost to an earlier
winner, winning claims are strictly increasing in `≺_CAS`. ∎ Claiming once before the loop WOULD break
this (a stale low claim carried into a later win, inverting vs an earlier winner who claimed higher) —
the draft's per-iteration re-claim is load-bearing and present in all four producers. `AcqRel`
sufficient (it's a ticket, not the LP). commit_seq counts attempts not commits (gaps irrelevant; u64
non-overflow). **PROVEN; draft correct for the four real-writer arms.**

## 3. Question B — idempotent arms must NOT rank here (DATA-LOSS bug)

Draft stamps+ranks both idempotent arms (`insert AlreadyExists :391`, `remove AlreadyAbsent :580`).
The `AlreadyExists` arm is reached when our root CAS was NOT taken (term already final in the loaded
root `:987`); this op published no root and has NO position in `≺_CAS`, yet it already appended an
`Insert@lsn` `:329` and now stamps it with a loop-top commit_seq.

Resurrection trace (s019 polarity):
1. `t` present. Thread I `insert_cas_durable(t)`: appends `Insert@lsn_I`, loop reads root, finds `t`
   final ⇒ `AlreadyExists`, claims `cseq_I`.
2. Thread R `remove_cas_durable(t)`: appends `Remove@lsn_R`, claims `cseq_R`, wins clear CAS ⇒
   ranks `Remove@lsn_R` with `cseq_R`.
3. R claims+commits BEFORE I claims ⇒ `cseq_R < cseq_I`. Acked truth = ABSENT (R is the last real
   writer; I changed nothing).
4. Replay sorts Remove(`cseq_R`) then Insert(`cseq_I`) LAST ⇒ final PRESENT. **Removed term
   RESURRECTED; acked remove LOST.**

`root.version()` (pre-draft) did NOT have this: read AFTER observing `t` present, causally bounded by
the publication that made `t` present, never exceeding a later remove. The global loop-top claim breaks
that causality. (D2.7 red-team, master memo 97–102, 234–242.)

§A makes the idempotent record harmless two ways that BOTH need later DG-RECON pieces: (a) present-hoist
⇒ no append; (b) mark-but-NO-RANK ⇒ dropped as Overlay orphan. (b) needs the file Overlay + reconcile
threading that regime. In THIS step files are Owned + `replay_records_lww` hard-codes Owned, so an
unranked record is KEPT@lsn ⇒ a high-lsn idempotent Insert still resurrects. Hence in the still-Owned
world there are only two self-consistent options, and the draft picks neither:
- Option 1 (pre-draft / DG1 shipped): idempotent arms rank with **root.version()** (causally safe).
- Option 2 (DG-RECON end-state): idempotent arms NO-RANK + present-hoist + file Overlay + reconcile
  threads Overlay ⇒ orphan dropped.

Draft is an unsafe hybrid (rank with global commit_seq while Owned). **Fix: land the full DG-RECON gate
(B-fix-A)** — commit_seq stamp for the four real arms TOGETHER with idempotent-NO-RANK + present-hoist +
regime-gated Overlay-drop + MAGIC_OVERLAY. (B-fix-B "keep root.version() in idempotent arms" mixes two
unrelated numeric domains in one file's ranks and is NOT actually safe — same cross-domain hazard as D.)

## 4. Question D — increment cross-domain hazard (stamp can't land alone)

`try_increment_cas_durable` never ranks. After the stamp, inserts/upserts carry commit_seq generations
(small: 1,2,3,…/attempt); increments are unranked ⇒ keyed by **lsn** (e.g. 300,304). Two unrelated
domains. For a V=u64 key both set and incremented (the n-gram counter overlay):
- seeded `commit_seq=5000` after a long prior session; fresh `insert_with_value("x",9)` ⇒
  `commit_seq=5001, lsn=12`; an earlier `increment("x",1)` unranked `lsn=10`. Sort `(10,10)` increment
  THEN `(5001,12)` set-to-9 ⇒ final `9`, **dropping the increment**. The domains have no consistent
  order ⇒ wrong last-writer for any key touched by both.

Membership (V=()) never increments ⇒ safe; but DG-RECON is per-file regime and the V=u64 counter
overlay is a first-class target. **Resolution (master memo "increment generation-threading → DG-RECON"):
bring increment into the commit_seq domain in THIS step** — extract `try_increment_cas_inner` from
`try_increment_cas :1473` returning the winning commit_seq (claimed at its loop-top), and have
`try_increment_cas_durable` append a CommitRank with it. Then `(generation,lsn)` is single-domain.
BatchIncrement still replays as accumulating delta (D6); the rank only fixes order vs same-key overwrites.

## 5. Question C — the seed

Formula: `seed = max(header.commit_seq_floor, max{CommitRank.generation scanned during replay})`;
`self.commit_seq = AtomicU64::new(seed)` so first claim = `fetch_add(seed)+1 = seed+1`.
- floor currently always 0 (DG2 unlanded) ⇒ seed == scan-max ⇒ protects no-checkpoint reopen (the
  soaks) but NOT post-checkpoint (ranks ≤ checkpoint_lsn pruned). State explicitly; pair with DG2.
- scan-max: one accumulator in the existing `reader.next_record()` loop
  (`if let WalRecord::CommitRank{generation,..} = &record { max_gen = max_gen.max(*generation); }`).

Placement (before `replay_records_lww`): `mmap_ctor::open` store before `:403`; `open_with_depth`
before `:608`; `io_uring_ctor::open` before `:227`. Single-threaded open ⇒ store anywhere between scan
and ctor return is safe (producers need `enable_lockfree()` after ctor). Read floor via
`WalReader::read_header(&wal_path)` (also validates magic) when `wal_path.exists()`, else seed 0.

Proof (no cross-restart collision): every generation that existed is in `G` (surviving WAL ranks) or
`≤ F` (subsumed by checkpoint). `seed = max(F, max G) ≥ g` ∀g. First claim `seed+1 > seed ≥ g`. ∎
Edge cases: empty/no-WAL ⇒ seed 0, first claim 1. No CommitRank ⇒ seed 0 (legacy unranked replay in
lsn order — note a fresh ranked op with small commit_seq vs legacy unranked op at large lsn is the same
cross-domain hazard; safe envelope: producers run only on FRESH rank-homogeneous overlay tries, as the
soaks do). Version-1 WAL ⇒ seed 0, back-compat. Floor set, all checkpointed ⇒ seed F, claim F+1 (DG2).

## 6. Question E — inertness

reconcile unit tests pass hand-built generations ⇒ untouched. s019 integration drives real
Inserted/Removed arms under `commit_rendezvous`; relative order of the two real ops is identical under
root.version() and commit_seq (both monotone in `≺_CAS` for winning arms) ⇒ present/absent preserved —
BUT these tests exercise only the real arms, so they would stay green even with the draft's buggy
idempotent ranking (COVERAGE GAP). Soaks insert/increment disjoint keys ⇒ generation values don't
change the set/sum; the §4 hazard (same-key ranked+unranked) isn't exercised (COVERAGE GAP). Hence the
new tests in §9.

## 7. Question F — deadlock / lock-ordering (CRITICAL)

`commit_seq.fetch_add(1, AcqRel)` lock-free/wait-free, no lock, no fault. Seed scan single-threaded at
open; `read_header` reads 64 bytes, no trie/buffer lock, no node fault. **Draft did NOT add the §A
faulting present-hoist to `insert_cas_durable`** — clean (the 75-min hang was a faulting read before the
append racing a checkpoint/eviction holding the buffer lock). When the §A insert-hoist lands (DG-RECON
co-req) it MUST use non-faulting `find_leaf_lockfree`, never `find_leaf_faulting` (remove's `:498`
faulting hoist is pre-existing on the remove path; do NOT copy to the insert hot path).
`append_commit_rank` unchanged call (only the value changes). Do NOT add the `commit_seq_by_data_lsn`
mutex to the append path in this step (defer to DG2; prefer lock-free/try-lock-with-scan there).
**Deadlock-clean as drafted.**

## 8. Question G — missing / wrong (file:line)

WRONG: (1) idempotent-arm ranking `:391`,`:580` (resurrection, §3) — fix via full gate. (2) scope:
the working tree also has the pre-existing F0 flip-routing (`mutation_api.rs`, `atomic_ops.rs`,
`batch_insert.rs`, `document_tx.rs`, new `lockfree_value_route.rs`) — dormant by default but should be
segregated out of the stamp+seed commit.

MISSING: (3) seed in the 3 char ctors (§5). (4) increment generation-threading (§4). (5) §A present-hoist
(non-faulting) + idempotent NO-RANK. (6) regime-gated Overlay-drop wiring (thread `header.regime()` into
`replay_records_lww :286`; stamp fresh overlay WAL MAGIC_OVERLAY/rank_regime=Overlay). #1+#5 unsafe
without #6.

ACCEPTABLE (note): (7) `_root_generation`/`_node` ignored — dead-but-harmless; non-durable `insert_cas`
still consumes it; defer enum-signature cleanup (`// TODO(DG-FORMAL)`). (8) NO byte/vocab parity
(they emit zero CommitRank, own recovery; seed char-only). (9) no external `replay_records_lww` test
callers.

## 9. Question H — gate sequence

Under `systemd-run --user --scope -p MemoryMax=32G --quiet env TMPDIR=$PWD/target/test-tmp`, soaks
`timeout 150`, tee `target/test-logs/<gate>.log`, single crate (`--features persistent-artrie --lib`):
1. `cargo check` + unsafe-inventory exit 0.
2. `reconcile_lww` unit suite (s019/resurrection/tie/back-compat/checkpoint-skip) — byte-green.
3. s019 integration (present + resurrection polarity) `timeout 60`.
4. **NEW idempotent-resurrection control**: no-op `insert_cas_durable(t)` rendezvous-ordered with a real
   `remove_cas_durable(t)` committing AFTER the no-op claims; drop-no-checkpoint; reopen; assert ABSENT.
   FAILS against the draft's idempotent ranking; passes once #1/#5 fixed.
5. **NEW cross-restart monotonicity**: ranks up to gen g; drop-no-checkpoint; reopen; write SAME key;
   reopen; assert final = last write; white-box assert `commit_seq ≥ max WAL rank` after first reopen.
6. **NEW increment cross-domain** (if #4 landed): V=u64 key both set + incremented interleaved; reopen;
   assert value = set-then-accumulated in commit order.
7. Durable soaks (concurrent_durable_writers, membership+counter checkpointer soaks,
   try_increment_cas_durable_survives_reopen) `timeout 150`.
8. loom durable model: no-lost-write passes; appended-frontier negative control still `#[should_panic]`.
   (loom abstracts the root CAS with a Mutex, doesn't model commit_seq ⇒ gate 5 is the monotonicity proof.)
9. Full recovery + char suites ≥ baseline.
10. `scripts/verify-formal-correspondence.sh` exit 0 (+ `_Unsafe` controls if the gate includes the drop).

## 10. Recommendation

1. Do NOT ship the draft as a standalone stamp step (idempotent ranking resurrects).
2. Fold into the full DG-RECON atomic gate {4-producer stamp + increment threading + §A non-faulting
   hoist + idempotent-NO-RANK + regime-gated Overlay-drop + MAGIC_OVERLAY + choke-point}.
3. Add the seed (3 char ctors).
4. Segregate the five F0 flip-routing files out of this commit.
5. Keep the hot path lock-free (no faulting read in insert; no append-path mutex).
6. Add the new tests (gates 4–6) — the only ones exercising the resurrection, the seed, and the cross-domain hazard.

## 11. Red-team synthesis (3 adversarial passes, CONVERGED 2026-06-02)

Three independent red-team agents attacked the verdicts. They CONVERGED. Net result: the draft is
unsafe for **two independent** reasons, HEAD has a build bug, and the step shape is a **gated
sequence S0→S5**, not one atomic gate.

### Confirmed (all three agents agree)
- **B (idempotent resurrection): CONFIRMED, worse than stated.** RT-1 showed the `AlreadyExists` arm is
  **deterministically reachable cache-cold** (post-recovery/eviction the positive cache is empty but the
  term is final), not a narrow race. Exact replay trace ⇒ PRESENT (acked remove lost). **The regime gate
  is IRRELEVANT** because the idempotent record is *ranked* (the Owned/Overlay drop only fires for
  *unranked* records) — the bug is "an op that took NO position in ≺_CAS is given a global commit rank as
  if it had." 
- **B-fix-B (keep root.version in idempotent arms): CONFIRMED unsafe** (three-domain sort: commit_seq /
  root.version / lsn on one key ⇒ arbitrary order). RT-1 **bonus finding**: pre-draft `root.version()`
  idempotent stamp is *also* latently unsound (a second-`load()` leapfrog past a third publication
  resurrects) — so reverting to root.version is a *mitigation, not a fix*. **The only robust fix: the
  idempotent arms append NO rank (and no data record) via a NON-FAULTING present-hoist.**
- **A (monotonicity): PROVEN for the 4 real-writer arms.** RT-2 verified the single-root-CAS total order,
  per-iteration re-claim (no shadowing), and that no Conflict/error arm leaks a stamped rank.
- **Increment publishes via the ROOT CAS** (`try_increment_cas:1487`), NOT a per-leaf atomic (the
  fast-path was removed in G1/G4). ⇒ Verdicts A and D are well-posed; the increment-rank fix is
  straightforward (claim at `try_increment_cas` loop-top `:1422`, append_commit_rank in the durable wrapper).
- **D (increment cross-domain): real but UNEXERCISED.** Bites ONLY a `V=u64` file mixing a ranked
  overwrite (insert_with_value/upsert) AND an unranked increment on the SAME key. No test/example does
  this today; unreachable in committed code (routing uncommitted). Pure-counter (lsn-only) and
  pure-membership (no increment) files are safe. ⇒ increment-rank must ship WITH the stamp, but this is
  one sub-step, not "the whole gate."

### NEW findings beyond the Plan
- **N1 (RT-2/RT-3) — the draft reopens A.2 (a SECOND, independent data-loss).** Stamping commit_seq from
  the unseeded `AtomicU64::new(0)` ⇒ on reopen the counter resets to 0 and **collides with replayed
  generations** (reopen-then-write-same-key picks the wrong winner). The seed is the missing half; the
  draft is a premature stamp without it.
- **N2 (RT-3) — HEAD does not compile on a clean checkout.** `mod.rs:187 pub(crate) mod
  lockfree_value_route;` is committed (a7f114a) but `lockfree_value_route.rs` is UNTRACKED ⇒ E0583 on a
  fresh clone / CI / rollback. The working tree only builds because the untracked file is physically
  present. **MUST FIX (S0).**
- **N3 (RT-2) — floor=0 is a latent data-loss for the checkpoint-prune case** (not just unoptimized): a
  checkpoint cannot *lower* the live counter (intra-session safe), but once overlay checkpoint-pruning is
  enabled, pruned high generations vanish from scan-max and only `commit_seq_floor` carries them — and
  `set_commit_seq_floor` has ZERO non-test callers. ⇒ the stamp+seed step MUST NOT be paired with overlay
  checkpoint pruning until DG2 wires the floor. The WAL-only boundary is load-bearing.

### REFUTED: "must be one big atomic gate"
DG-RECON is **already** landing incrementally (955e3ab dual-magic + 4d203b8 regime-drop, both inert,
green). RT-3's decomposition (each independently inert/reversible until the final owner-gated flip):

| Step | Change | Reversible | Gate |
|---|---|---|---|
| **S0** | Fix the dangling `mod lockfree_value_route` (commit the file or revert the line) so HEAD builds clean. | Yes | `cargo check` on a clean tree |
| **S1** | Seed infra: `max_gen` accumulator in the WAL read loop + `commit_seq = max(read_header().commit_seq_floor, scan-max)` in the 3 char ctors (inert — nothing reads it yet). | Yes (→0) | header + ctor unit tests |
| **S2** | §A present-hoist in `insert_cas_durable` using **NON-FAULTING `find_leaf_lockfree`** + idempotent arms `mark_committed` but **NO `append_commit_rank`**. Regime-INDEPENDENT-safe (a hoisted no-op appends nothing ⇒ no record to resurrect). **ISOLATED — its own `timeout 150` soak (the 75-min-hang tripwire).** | Yes | isolated soak + NEW idempotent-vs-remove control |
| **S3** | (optional) choke-point `apply_recovered_records` threaded with `Owned` (byte-identical, inert). | Yes | recovery+char suites = baseline |
| **S4** | commit_seq stamp for the **4 real arms** + **increment-rank** (rank-homogeneous file) + activate the S1 seed. Idempotent arms stay NO-RANK (S2). | Yes (→root.version) | NEW cross-restart monotonicity + cross-domain set+increment tests |
| **S5** | **[OWNER GO/NO-GO — IRREVERSIBLE per file]** write MAGIC_OVERLAY/rank_regime=Overlay on a fresh WAL + thread `header.regime()` into the reconcile caller + route production mutators. | **NO** | full DG-FORMAL + DG-SOAK |

**The irreversible line is S5 ONLY.** The draft + S0–S4 are all reversible; the entire machinery is
built+tested with **test-only** Overlay files (scratch under `target/test-tmp`, never tmpfs), leaving
the production flip as the single owner-gated step.

### S2 RE-VALIDATION (2026-06-03): RT-3's "idempotent NO-RANK is regime-independent-safe" is REFUTED

A focused adversarial re-verification (mandate: *prove NO-RANK safe*) could NOT — it is a confirmed
resurrection bug under the **deployed** `Owned` reconcile, via the insert-insert race:
- Threads I and J both `insert_cas_durable(t)` with t ABSENT ⇒ both pass the non-faulting hoist ⇒ both
  append durable `Insert` records (Order-A; the hoist CANNOT prevent the loser's append — both saw t
  absent). J wins the root CAS (ranked `g_J`); **I loses ⇒ `AlreadyExists` ⇒ NO-RANK ⇒ `Insert@lsn_I`
  stays durable + UNRANKED**.
- Later K removes t (ranked `g_K`). Acked truth: ABSENT.
- Reopen: deployed `reconcile_lww` (Owned) keys I's unranked record by **raw `lsn_I`**; `lsn`
  (`next_lsn.fetch_add`) and `g` (`commit_seq`/`root.version`) are **unrelated domains**, and `lsn_I >
  g_K` is the COMMON case (commit_seq starts tiny; every op burns ≥2 LSNs). ⇒ I's Insert sorts AFTER
  K's Remove ⇒ final **PRESENT** ⇒ acked remove RESURRECTED.
- Also deterministically reachable **cache-cold single-threaded** (post-recovery/eviction: t final but
  uncached ⇒ `insert_cas_durable(t)` appends then hits `AlreadyExists`).

**Corroborated by `durable-global-commit-sequence-redesign-d2.md` §3.1/§3.2/§3.4:** NO-RANK is safe
ONLY when paired with BOTH (a) the idempotent arms append NO data record (hoist short-circuits BEFORE
the append) AND do NOT `mark_committed` on the residual race; AND (b) a **watermark-gated reconcile**
(reconstruct the contiguous-ranked watermark during the scan and DROP unranked-above-watermark) — OR
the file is `Overlay` and reconcile threads `header.regime()` so unranked orphans hit `=> continue`.
The DEPLOYED `reconcile_lww` (`recovery.rs:257`) is the PRE-D2 form (no watermark param; Owned keeps
unranked @ lsn). ⇒ **NO-RANK cannot land as an inert Owned step (RT-3's S2 has a hole).**

CONSEQUENCE — VALIDATED CORRECTED DECOMPOSITION (2026-06-03, second validator):

**FIX-A is SOUND and is the corrected S2** (replaces RT-3's unsafe NO-RANK-under-Owned). The idempotent
arms rank the **OBSERVED-root version** — `current_root.version()` captured at `try_*_lockfree_path`
ENTRY (the same load that decided already-final/already-absent), NOT a second `lockfree_root.load()`.
Proof: `OverlayNode::version()` is a per-node-lineage `+1` (every path-copy sets
`new.version = self.version+1`), globally monotone along the SINGLE CAS-serialized root chain. The
idempotent op witnessed `t` present at `r_obs`; any later same-key remove publishes `r_R` with
`r_obs ≺ r_R` ⇒ `version(r_obs) < version(r_R)` ⇒ the idempotent Insert sorts BEFORE the later Remove ⇒
NO resurrection — robust to an intervening third publication P (it only widens the gap). This closes
RT-1's leapfrog (no second load) AND the global-claim resurrection (causal bounding), and stays in the
SAME `root.version` domain as the (pre-stamp) real arms ⇒ no cross-domain ⇒ Owned-safe + inert.
Char-only (byte/vocab emit zero CommitRank). Needs `LockfreeInsertResult::AlreadyExists(u64)` /
`LockfreeRemoveResult::AlreadyAbsent(u64)` payloads (observed version already materialized at
`try_*` entry — no new walk, no second load; empty-overlay `None=>AlreadyAbsent(0)`).

**FIX-B CONFIRMED — S4 coupling is NECESSARY.** Once real arms carry `commit_seq`, the idempotent arm
has NO correct rank: claim-fresh-commit_seq resurrects; read-publishing-commit_seq is unavailable (it
didn't observe that root, and the value isn't durably attached); rank-root.version is cross-domain. ⇒
it MUST NO-RANK, which under the deployed reconcile is safe ONLY under `RankRegime::Overlay`
(unranked⇒drop). "Store commit_seq on the root node" is a strictly-dominated TRAP (degenerates to
FIX-A, gives no durability the CommitRank doesn't, widens the hot-path node). FIX-A is therefore
**mutually exclusive** with commit_seq-stamped real arms: it is the PRE-stamp regime; at S4 the
idempotent arms convert from "rank observed-version" to "NO-RANK + non-faulting present-hoist", the same
code region edited forward.

**Test seam (Q5) exists + clean:** `WalWriter` header lock sets `rank_regime=1` (writer.rs:619 test);
`reconcile_lww` already takes `RankRegime`; the only hard-code is `replay_records_lww`
(mutation_core.rs:286). Tests build an Overlay-regime trie on real-disk scratch (`target/test-tmp`) or
call `reconcile_lww(…, Overlay)` directly. **Reversibility (Q6):** writing Overlay IN A TEST is
reversible; only PRODUCTION writing Overlay by default (S5) is irreversible (dual-magic tripwire).

CORRECTED S2 row: idempotent arms → observed-root version (NOT NO-RANK). Real arms stay `root.version`.
NOW SAFE TO IMPLEMENT S2 (FIX-A); S4 follows (coupled stamp+NO-RANK+hoist+increment+Overlay).

### Disposition of the working-tree draft
The draft's **4 real-arm stamps are correct** (keep). Its **2 idempotent-arm stamps are the resurrection
bug** (must become S2's NO-RANK-via-non-faulting-hoist — an EDIT-FORWARD, not a git revert). The **seed
is missing** (S1). So the draft EVOLVES into the correct S4 by adding S1 (seed) + S2 (fix idempotent) +
increment-rank — no git-revert/stash needed; the bad parts are overwritten by forward edits.

## 12. S3 + S4 red-team (2026-06-03, 2 adversarial passes) — VALIDATED + sharpened

After S0–S2 landed, two questions were re-attacked: (S3) can increment-rank land NOW on the `root.version`
domain, decoupled from the commit_seq/Overlay gate? and (S4) the Overlay-seam + non-faulting-hoist mechanics.

**S3 = increment-rank at `root.version`: SAFE + CORRECT + STANDALONE-COMMITTABLE (do it next).**
- The rank is ORDER-ONLY: a `BatchIncrement` is a commutative accumulate-delta, so ranking it can NEVER
  change a recovered sum (pure-counter soaks unaffected); it only fixes the mixed set+increment same-key
  case (hazard D), by putting the increment in the SAME monotone root-CAS `root.version` chain as the 4 real
  arms (all 5 producers CAS the one `lockfree_root`; the per-leaf counter was removed in G1/G4).
- Cross-restart: HARMLESS for increments (deltas commute; lsn tiebreaker is cross-restart-monotone for the
  set-vs-increment case). Same intra-session limitation as FIX-A — NO new hole; closed uniformly by the
  commit_seq seed at S4.
- **Guards (mandatory):** **G-OVF** — append the CommitRank ONLY when the inner returns `Ok`; the overflow
  check errors BEFORE any CAS, so `let (val,gen)=self.try_increment_cas_inner(..)?;` then unconditional rank
  leaves the overflow-window `BatchIncrement` unranked (benign: accumulate-delta replays in lsn order under
  Owned; unacked-drop under Overlay). NEVER pre-append the rank. **Capture site:** `new_root.version()` after
  `build_value_path_recursive`, returned ONLY from the winning `Ok(_)` CAS arm (never `continue`/`None`/
  overflow) — guarantees the winning iteration's generation, no losing-iteration leak. **G-MERGE** (document,
  don't fix): `merge_lockfree_values_to_persistent` stays an unranked `BatchIncrement` — a non-Order-A
  `&mut self` drain, Owned-only-safe, the one remaining unranked durable record; flag it for the S4 Overlay
  drop so a legit drain is not silently dropped.
- **Ship the mixed set+increment same-key reopen test (both polarities) WITH S3** — the existing increment
  soaks are all commutative/single-delta and give S3 no teeth on hazard D.

**S4 corrections:**
- **N-S4-1 (HIGH): the Overlay seam is a WAL ROTATION, not an in-place magic restamp.** `enable_lockfree()`
  runs on a non-empty WAL; an in-place `MAGIC→MAGIC_OVERLAY`+`rank_regime=Overlay` overwrite has (a) a
  torn-write hazard (magic persists but regime doesn't ⇒ Overlay-magic-Owned-regime ⇒ orphans KEPT ⇒
  resurrection) and (b) drops the pre-existing Owned unranked records already in the file. SAFE seam: rotate
  the Owned WAL to archive + open a FRESH active file whose 64-byte header is written ONCE with
  `MAGIC_OVERLAY`/`rank_regime=Overlay` (reuse `rotate_to_archive`'s regime+floor carry; pre-existing records
  stay in the Owned archive segment; `next_lsn` continuity preserved). In-place is safe ONLY on a
  provably-EMPTY WAL. `from_bytes` already dual-accepts both magics; no VERSION bump.
- **`enable_lockfree→Overlay` is REVERSIBLE-S4** — every caller is `#[cfg(test)]`/opt-in; default ctors set
  `OwnedTree`; vocab `enable_lockfree` emits zero CommitRank. Guard with a comment/assert at the stamp site:
  "reversible ONLY while every caller is opt-in/test; a production caller makes this S5."
- The non-faulting hoist (`find_leaf_lockfree`, NEVER `find_leaf_faulting`) is deadlock-free (no buffer/trie
  lock, never faults) AND correct under Overlay: a hoist-MISS (term present-under-evicted-prefix) falls
  through to either a ranked CAS arm or a NO-RANK orphan that the Overlay drop removes ⇒ no resurrection; the
  fall-through is an optimization-decline, never a wrong result. The NO-RANK arm MUST still
  `mark_committed(data_lsn)` (liveness; the drop is replay-time + orthogonal to the LSN-contiguous watermark,
  so it punches no hole).
- **N-S4-2 (MEDIUM):** `find_leaf_faulting`'s "REVERSIBLE BENCH GATE" doc is STALE (it is un-gated
  `pub(crate)`, live on production read/remove/value/increment paths) — fix the doc + add a "NEVER call from
  the insert hoist; use find_leaf_lockfree" marker (deadlock memo). **N-S4-3 (LOW):** re-discharge the
  lock-order proof for S4's faulting producers (remove/increment/value fault-in) via an isolated
  `timeout 150` insert‖remove‖increment‖checkpoint‖eviction soak.
- commit_seq monotonicity holds (NO-RANK idempotent arms emit zero ranks ⇒ strengthen the order); S4 MUST
  convert all 4 real arms + increment to commit_seq + idempotent→NO-RANK + activate the seed in ONE change
  (no mixed root.version/commit_seq domain).
