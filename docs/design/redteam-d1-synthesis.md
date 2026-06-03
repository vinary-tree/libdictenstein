# Red-team synthesis: D1 durable-global-commit-sequence redesign — BROKEN (2 adversarial Plan agents + code verification)

**2026-06-02.** Two independent read-only red-team Plan agents attacked
`docs/design/durable-global-commit-sequence-redesign.md` (D1) from disjoint angles
(intra-session linearization vs cross-restart/recovery/back-compat). **Headline: BOTH broke it,
complementarily, at the ARCHITECTURAL level.** D1's *core idea* (a durable global commit-sequence as
the LWW replay key) survives — but D1 made two unstated wrong assumptions that both red-teams falsified.
I (the orchestrator) independently re-verified the single load-bearing finding (RT#1 F1) against the
actual code; it holds. The loop continues: **D1 → (broken) → D2 redesign → red-team D2**.

---

## RT#1 — intra-session linearization + A.6 frontier + concurrency (agent a8eaec6)

**F1 — [CRITICAL] `CommitSeqMonotone` is FALSE for the insert path. CODE-VERIFIED by orchestrator.**
Membership-insert has a **split linearization point**:
- seq/generation captured at the **root CAS** (`lockfree_cas.rs:928` `let root_generation = new_root.version()`; `:930` `compare_exchange`),
- but membership becomes visible only at the **later `try_set_final`** (`:347` → `overlay/node.rs:725`
  `flags.fetch_or(IS_FINAL, AcqRel)`), which **does NOT bump `version`** (contrast `as_final:811` /
  `with_child:766` / `with_value:866`, which DO bump).
- the reader gates membership on `is_final()` (`:1081`), so visibility = `try_set_final`, not the root CAS.
- `build_path_recursive` returns `Arc::clone(node)` of the **shared non-final** node (`:801`, the
  prefix-reuse arbiter — comment `:787` "`try_set_final` is the SINGLE atomic arbiter"), so the
  finalized node can be **reachable and shared**, not a fresh path-copy.

⇒ For inserts, **commit_seq order ≠ visibility order**. Value-ops (upsert `:1659`, increment `:1441`)
are SOUND — they stamp at an atomic-final root CAS (`build_value_path_recursive` bakes
`as_final().with_value`, `:1781`) that IS their visibility point. The asymmetry is the bug.

**F2 — [CRITICAL] Cross-op-type inversion: insert ‖ increment/upsert on one term.** Insert's deferred
`try_set_final` vs the value-op's atomic root CAS ⇒ same-term insert(seq=lo) ‖ increment(seq=hi) can
have visibility order = increment-then-insert while seq order = insert-then-increment. The
"claim-before-CAS ⇒ winner has lower claim" bracket assumes ONE linearization point per op; insert has two.

**Concrete data-loss trace (orchestrator-constructed, flows through the prefix-reuse path):**
`"cats"` present ⇒ `"cat"` node_A exists non-final. (1) `I=insert("cat")` claims seq=5, reuses node_A
(`:801`), root-CAS publishes spine to node_A (still non-final), **pauses before `try_set_final`**.
(2) `R=remove("cat")` claims seq=6, observes node_A **non-final ⇒ AlreadyAbsent no-op**; under the A.5
fix it appends a `Remove` record + rank(6). (3) `I` resumes `try_set_final(node_A)` ⇒ `"cat"` now
**visible present**. Replay sorts `I(5) Insert` before `R(6) Remove` ⇒ `"cat"` **absent ≠ visible
present = DATA LOSS**. (Note: pure-F1 where I's `try_set_final` lands on a *dead* node → no loss; the
exploit requires the **shared-reachable node_A** prefix path + a higher-seq competitor; for the remove
case it also requires the A.5 idempotent-rank. Narrow but real, and it falsifies the theorem.)

**F3 — [HIGH] A.6 frontier "no same-term out-orderer" fails with TWO concurrent same-term ops in their
two-append windows simultaneously** — both data-durable, both rank-pending ⇒ both fall back to
`generation_of=lsn` (LARGE) and can out-sort a third ranked op or each other.

**F4 — [HIGH] Torn multi-op document-tx batch defeats single-rank-at-CommitTx.** Order-A + group-commit
does not guarantee atomic all-or-nothing durability of the per-op data appends vs the CommitTx append;
a partial fsync can leave op-k's data missing while CommitTx is present ⇒ replay applies a strict
subset of the committed batch.

**F5 — [MED/HIGH] C1′ bail-rank strict-inequality not guaranteed: the claim is taken at APPEND, after
the READ** (R-1 "decide before append" pushes the claim too late). A superseder can claim a LOWER seq
and be mid-CAS while the bailer's `g_read` ties/exceeds it. Needs the claim at the read instant.

**F6 — [MED] Idempotent-arm A.5 inverse-deletion enabled by F1** (the F1 trace's step 2). A.5 fixes one
resurrection but, with F1's split-LP, introduces the inverse (a no-op `AlreadyAbsent` deletes a
concurrently-finalized present key). Unique claims fix a *tie* but not an *inversion*.

**RT#1 verified SOUND:** the seed/A.2 fix *within one process lifetime*; increment commutativity/A.3
*among increments*; all-paths unification/A.1 *structurally* (it inherits F1 uniformly — a
correctness-of-key bug, not a path-divergence bug).

**RT#1 root cause (one line):** *D1 assumes every op has a single linearization point at which both
visibility and commit_seq are fixed. Membership-insert violates this — seq at the root CAS (`:928`),
visibility at the later `try_set_final` (`node.rs:725`), which does not bump the version.*

---

## RT#2 — cross-restart seeding + recovery paths + WAL back-compat (agent acf4e26f)

**F1 — [CRITICAL] Post-checkpoint reseed → A.2 reborn.** The seed scan reads ONLY the active `wal_path`
(`mmap_ctor.rs:300-321`, `io_uring_ctor.rs:133`), never archives. Checkpoint
(`persist.rs:108-165` → `rotate_to_archive` → `wal.truncate()` then `set_min_lsn(cp+1)`) resets the
active WAL to empty. The **LSN domain survives via a durable FLOOR** (`set_min_lsn`/header
`checkpoint_lsn`); **commit_seq has NO floor** and is NOT in the header's unused `reserved[20..64]`
(`header.rs:22`). ⇒ After a checkpoint the seed restarts at 0 ⇒ a new op gets commit_seq=1 while a
pre-checkpoint same-term op survives on-disk at commit_seq=300 ⇒ later reconcile mis-sorts the NEW op
BELOW the old ⇒ acked write lost. **The exact A.2 class the redesign exists to kill, reborn.**

**F2 — [CRITICAL] `recover_from_archives` neither reconciles NOR seeds.** `mmap_ctor.rs:1137` →
`rebuild_from_wal_segments` (`recovery.rs:1443`) replays each segment in raw per-record order
(CommitRank → `vec![]` at `:370`), never `reconcile_lww` ⇒ s019 recurs on the archive path; and it
returns only `(records, terms)` with **no channel** to report a `max_commit_seq` ⇒ zero seed.

**F3 — [HIGH] Mixed v2/v3 file: one header byte, two key-domains, no enforced clean break.** A v3 binary
opens & APPENDS to a `cf1f80c` v2 file (`from_bytes` accepts `[MIN_SUPPORTED=1 ..= VERSION]`,
`header.rs:82`; no "refuse v2-for-append" guard). The "v2 root-version vs v3 commit_seq comparator" is a
**distinction without a difference** — both are `u64`-ascending `sort_by (generation,lsn)`
(`recovery.rs:273,296`); selecting by `wal_version` cannot disambiguate intra-file mixed ranks. A small
v2 root-version (2) and a small early v3 commit_seq (2) on one term tie-break by lsn, not commit order.

**F4 — [HIGH] reconcile replays the unranked frontier and it can WIN.** `reconcile_lww` replays EVERY
in-scope record and assigns unranked records `generation_of=lsn` (LARGE, `:273`); it ignores the
watermark (no watermark param). A torn batch where op_A(T) ranked at seq=10 and a later same-term op_B's
data is durable but its rank crashed ⇒ op_B replays at `lsn≫10` ⇒ **op_B wins** over op_A. The
watermark-stall argument is about ACK, not REPLAY.

**F5 — [HIGH] single-CommitRank-at-CommitTx widens A.6 to whole-batch granularity; tx-gating is
unimplemented.** `reconcile_lww` expands batches unconditionally and treats Begin/Commit/AbortTx as
no-ops (`:343-370`) — A.4 entirely unfixed. A committed tx whose single CommitRank append crashes ⇒ the
ENTIRE batch unranked ⇒ every entry mis-sorts.

**F6 — [MED] IncrementalRecovery per-window buffering can't preserve cross-window order.**
`process_record` (`:932`) uses a single `pending_ops` and `BeginTx` does `pending_ops.clear()` (`:940`)
— tx-unsafe today. Never-checkpoint+v3 ⇒ one unbounded window ⇒ unbounded buffer (OOM) or fail-closed
(streaming v3 recovery unavailable, not merely restricted).

**F7 — [SOUND] Codec sharing does not cross-contaminate the counter** — `commit_seq` is a per-instance
`AtomicU64`; byte/vocab are separate instances. Verified sound.

**RT#2 root structural defect (uniting F1+F2):** *the LSN domain is reseedable across truncation via a
persisted FLOOR (`set_min_lsn`/header `checkpoint_lsn`); D1's commit_seq is reconstructed by SCANNING
CommitRank records that checkpoint/truncate DELETES, and stores no durable floor. commit_seq needs the
same floor-persistence the LSN already has — scanning is insufficient.*

---

## THE COMBINED SYNTHESIS — what D2 must fix

D1's core idea (durable global commit-seq = LWW replay key) is RIGHT. It failed on two axes, both with a
**known principled fix direction**:

### Axis 1 — intra-session: the seq must be stamped at the TRUE linearization point (RT#1 F1/F2/F6).
The membership-insert split-LP is THE crux. Two candidate fixes (D2 must pick + PROVE no prefix-bug
regression — the prefix bug is why `try_set_final`-as-arbiter exists, fixed Phase-A; vocab is already
single-phase and correct):
- **(1a) Unify insert to single-LP atomic-final:** publish a fresh **final** leaf in the root CAS (like
  the value path), handle the prefix case via the **rebase-retry loop** (on CAS-failure, rebase onto the
  new root preserving the concurrent insert) instead of the shared-node `try_set_final` trick. Then ALL
  ops linearize at the root CAS and claim-before-CAS is correct universally. RISK: must not reintroduce
  the proper-prefix data-loss bug (the Phase-A fix). This is the cleanest if the rebase-merge is proven.
- **(1b) Stamp the seq AT `try_set_final`** (when `newly==true`, the unique finalizer), append the
  CommitRank after. RISK: a node finalized then made unreachable by a concurrent parent-edge remove ⇒
  phantom rank; needs a reachability/abort argument that is hard in a lock-free tree. Likely inferior.
- Increment ‖ insert (F2) is subsumed: once insert linearizes at a single point consistent with the
  value path, the cross-op inversion closes.

### Axis 2 — cross-restart: commit_seq needs a DURABLE FLOOR, like the LSN (RT#2 F1/F2 — their gift).
- Persist `commit_seq` floor in the header's unused `reserved[20..64]`; at checkpoint, set the floor =
  max commit_seq subsumed by the checkpoint (mirror `set_min_lsn`). Seed = `max(header floor, scan)`.
- `recover_from_archives` / `rebuild_from_wal_segments`: (a) route through `reconcile_lww`, (b) thread a
  `max_commit_seq` out so the rebuilt trie seeds correctly.

### Axis 3 — the two-append window must be GATED in REPLAY, not just ack (RT#1 F3/F4, RT#2 F4/F5).
- `reconcile_lww` must **exclude (or down-rank) unranked records above the committed watermark** — an
  unranked-but-durable data record is genuinely ambiguous (CAS may/may-not have landed); it must NOT win
  by `generation_of=lsn`-is-large. Give unranked-above-watermark records a LOSING rank, or drop them
  (they were never confirmed visible). Add the watermark as a reconcile parameter.

### Axis 4 — tx (RT#1 F4, RT#2 F5/F6).
- `reconcile_lww` honors the tx state machine (drop non-Committed). Per-op ranking inside a committed tx
  (not one batch rank) so increments order against non-tx same-term ops. IncrementalRecovery v3:
  fail-closed for never-checkpoint (documented availability constraint), else per-checkpoint-window.

### Axis 5 — back-compat (RT#2 F3).
- **Refuse to append to a v2 file** (force a clean break / explicit migration) OR make the comparator
  genuinely version-tagged per-record. The header bump alone does NOT prevent in-place v2→v3 mixing.

### C1′ value-CAS (RT#1 F5).
- Move the bail-claim to the READ instant (not the append), so `g_read <` any superseder strictly.

---

## STATUS
- D1 is **NOT correct** — do NOT implement. `db7cb2d`+`cf1f80c` remain committed (cf1f80c is a PARTIAL
  fix: closes the single-session `reconcile_lww` path only); F0 hacks remain uncommitted/untouched; the
  flip stays PAUSED.
- Next: a Plan agent produces **D2** (read-only) solving Axes 1–5 + C1′, with Axis-1 (insert split-LP)
  as the architectural crux and a no-prefix-regression proof obligation. Then **red-team D2**. Then
  foreground implementation, green-gated, with the soak EXTENDED to exercise the holes both red-teams
  found (prefix-reuse insert ‖ remove/increment, post-checkpoint reseed, archive-rebuild, mixed-file,
  torn-tx-batch, never-checkpoint IncrementalRecovery).
