# Durable Global Commit-Sequence Redesign — D2

**2026-06-02. Read-only design. Supersedes D1 (`docs/design/durable-global-commit-sequence-redesign.md`).** This closes every finding in `docs/design/redteam-d1-synthesis.md` (RT#1 F1–F6, RT#2 F1–F6) and the PRE-D1 holes A.1–A.6. Principled-only: no benign-window deferrals, no "irreducible hole" hand-waving. Where D1 said "BENIGN not eliminated," D2 *eliminates*. Cite-anchored to verified code. This document will be red-teamed; §6 attacks it first.

The repo root is `/home/dylon/Workspace/f1r3fly.io/libdictenstein`. All paths below are relative to that root.

---

## §0. The two D1 root-cause errors, and D2's two structural corrections

D1 made exactly two unstated wrong assumptions; both red-teams falsified them:

1. **(RT#1) D1 assumed every op has a single linearization point.** False for membership-insert: it stamps the generation at the root CAS (`lockfree_cas.rs:928`) but becomes *visible* at the later in-place `try_set_final` (`:347`→`overlay/node.rs:725`), which is a `fetch_or` that does **not** bump `version` (contrast `as_final:809`/`with_child:766`/`with_value:866`/`without_child:782`/`as_non_final:850`, all of which DO bump). The reader gates on `is_final()` (`:1011`). So for inserts, generation-order ≠ visibility-order.

2. **(RT#2) D1 assumed a scan can reconstruct the commit_seq domain.** False across checkpoint/truncation: checkpoint archives/truncates the active WAL (`persist.rs:147-175`→`writer.rs:rotate_to_archive:424`/`truncate:338`), deleting the CommitRank records the scan reads. The LSN domain survives via a durable FLOOR (`set_min_lsn`/header `checkpoint_lsn`); commit_seq stored none.

**D2's two corrections:**

- **C-A (Axis 1):** Make membership-insert single-LP by publishing a **fresh FINAL leaf inside the root CAS** — the proven shape the value path (`build_value_path_recursive:1785`) and the remove path (`build_remove_path_recursive:678`) already use. Delete the deferred `try_set_final`. Stamp `commit_seq` (a durable global counter) at that one root CAS, claim-before-CAS. Now ALL ops (insert/remove/upsert/insert-with-value/increment) linearize at one root CAS each, and commit_seq order == visibility order universally.

- **C-B (Axis 2):** Give commit_seq a **durable floor** in the header, mirroring `checkpoint_lsn`, persisted at checkpoint, carried across rotate/truncate, and seeded as `max(header floor, full-scan max)` on EVERY open path including archive-rebuild.

The ordering key is renamed end-to-end from "generation" to **`commit_seq`** to kill the "root version" mental model that produced both bugs.

---

## §1. AXIS 1 — the architectural crux: stamp the seq at the TRUE linearization point

### 1.1 Decision: adopt **(1a)** — unify insert to single-LP atomic-final. Reject (1b).

**Why (1b) is rejected (rigorously, not by preference).** (1b) keeps the deferred `try_set_final` and stamps commit_seq when `newly==true`. This fails three ways, none patchable without re-deriving (1a) anyway:

- **Phantom-rank (the synthesis's own hazard).** Between `try_set_final(node_A)` succeeding and the post-CAS rank append, a concurrent remove can publish a new root whose spine drops the parent edge to `node_A` (remove path-copies via `with_child`/`without_child`, CAS-swaps the root — `:622`). `node_A`'s final bit is set on a node no longer reachable from the live root, yet its data record + a `commit_seq` higher than the remove's is durable. Replay then resurrects a key that quiesced absent. To suppress this you must prove `node_A` is still root-reachable at the instant you stamp — but reachability in a lock-free path-copy tree is only decidable by *being the root CAS that publishes it*. That is (1a).

- **The split-LP is intrinsic, not incidental.** `try_set_final` is a `fetch_or` on a **shared** Arc (`build_path_recursive` returns `Arc::clone(node)` at `:801` for the proper-prefix case). Two racers converge on one allocation by design (`:786-800`). The "winner" of `fetch_or` is therefore *not* the winner of any root CAS — there is no root CAS that corresponds 1:1 to the finalization. commit_seq stamped at `fetch_or` cannot be made monotone in *publication* order because finalization is decoupled from publication. This is F1 restated: you cannot stamp visibility order at a point that is not the visibility-publication point.

- **Idempotent-arm inversion (F6) persists.** With a deferred finalize, the `AlreadyExists` arm (`:375`) still has to guess a commit_seq from a live re-walk (A.5), reintroducing the stale-read hazard.

**(1a) eliminates all three** because finalization *is* the root CAS: there is exactly one linearization point per insert, identical in kind to remove/upsert/value-insert/increment.

### 1.2 The (1a) mechanism (exact code-site changes)

**Change 1 — a NEW finalizing builder for the durable path (do NOT mutate the shared `build_path_recursive`; see §6.2).** Add `build_final_path_recursive`, identical to `build_path_recursive` (`lockfree_cas.rs:781`) except the base case at `depth == chars.len()`:
- If `node.is_final()` → `Err(BuildPathError::AlreadyExists)` (unchanged, `:783-785`).
- Else, **replace** `return Ok((Arc::clone(node), Arc::clone(node)))` (`:801`) with:
  ```
  let final_leaf = Arc::new(node.as_final());     // fresh FINAL copy, version-bumped
  return Ok((Arc::clone(&final_leaf), final_leaf));
  ```
  This is byte-for-byte the shape of `build_remove_path_recursive:678` (`Arc::new(node.as_non_final())`) and `build_value_path_recursive:1796` (`Arc::new(node.as_final().with_value(value))`). The subtree (`store`) is retained by `as_final` (`:809-818` clones `store`), so a proper-prefix node keeps its children.

**Change 2 — the brand-new-path (`None`) arm of the finalizing builder.** The `None` arm (`:860-865`) builds a fresh spine via `create_lockfree_path`; its leaf is created non-final today (`:890`, "caller will try_set_final"). For 1a, give the finalizing builder a `create_final_lockfree_path` that bakes `as_final()` into the leaf at construction (mirroring `build_value_path_recursive:1838-1842`, which constructs `PersistentCharNode::new().as_final().with_value(value)` directly). The non-durable `insert_cas` path keeps the old `create_lockfree_path`+`try_set_final` (see 1.6).

**Change 3 — `insert_lockfree_recursive` (`:910-958`).** Structurally unchanged: it already captures the claim before the CAS (`:928`) and returns `Inserted(leaf, …)` on CAS success (`:933`). Under 1a, `leaf` is now final, so the result means "published-and-visible." The caller stamps `commit_seq` (§1.4).

**Change 4 — `insert_cas_durable` (`:344-413`).** Route through the finalizing builder. **Delete** `let newly = node.try_set_final();` (`:347`). The `Inserted` arm no longer finalizes (already final); `newly` is derived from the build result (1.5). The `AlreadyExists` arm (`:375`) produces no publication ⇒ it does NOT consume a commit_seq and does NOT append a rank and does NOT `mark_committed` (§3.4).

This is a **structural simplification**: the durable insert collapses to the exact two-method shape (`build_*_path_recursive` + root-CAS in `*_lockfree_recursive`) that remove and value-insert already have. The asymmetry RT#1 F1 names is deleted, not papered over.

### 1.3 PROOF OBLIGATION: (1a) does NOT reintroduce the Phase-A proper-prefix data-loss bug

**The Phase-A bug (why `try_set_final`-as-arbiter was introduced, `:786-800`).** The *old* code called `node.as_final()` at the leaf and then a *separate* `try_set_final` re-checked the bit and used the result as the duplicate signal; inserting a NEW prefix term (e.g. "d" after "da") observed an already-final node and reported a duplicate — returning `false` AND skipping the cache, so merge dropped it. The fix made `try_set_final` the single arbiter by sharing the non-final node.

**Why (1a) cannot reintroduce it.** The Phase-A bug had two ingredients: (i) `as_final()` at the leaf, AND (ii) a *separate* `try_set_final` re-check used as the duplicate signal. (1a) keeps (i) but **deletes (ii).** Under (1a):
- Duplicate detection is `node.is_final()` on the *snapshot* inside the builder (`:783`), BEFORE any new node is built. "d after da" reaches `depth==len` at the "d" node, finds it **non-final** (only "da" was finalized; "d" was a path intermediary), takes the else-branch, publishes a fresh final "d" leaf. Correctly NEW.
- No post-CAS bit re-check exists, so the "observe already-final → wrongly report duplicate" mode is gone.
- The cache insert (`:364`) happens on the `Inserted` arm exactly as before.

**The three-way prefix race (`cat`/`cats`/`catnip`).** Setup: `"cats"` present ⇒ `"cat"` node exists non-final. Concurrent: `I1 = insert("cat")`, `I2 = insert("catnip")`. Let R = `lockfree_root`.

- `I1` descends c→a→t, at `depth==3` finds "cat" non-final, builds `new_root1` = path-copy of c→a→t with a **fresh final "cat" leaf** (children `{s→…}` retained by `as_final`'s `store.clone()`), claims `seq1` before CAS.
- `I2` descends c→a→t→n→i→p; t→n absent, so the `None` arm builds a fresh spine n→i→p with a final "catnip" leaf under "cat", producing `new_root2` (path-copy including the existing "cat" node, still non-final, with its "s" child), claims `seq2`.

Case A — `I1` CAS wins (`R: root0→new_root1`): `I2`'s CAS (`expected=root0`) fails (`Conflict`, `:937`), retries: re-reads `R=new_root1`, descends c→a→t (now the FINAL "cat" leaf with child `{s}`), t→n absent, builds fresh n→i→p final "catnip" under the now-final "cat" (its final bit + "s" child preserved by `with_child`), CAS `new_root1→new_root3` wins. Final: cat,cats,catnip all final. No loss.

Case B — `I2` CAS wins (`R: root0→new_root2`): `I1`'s CAS fails, retries: re-reads `R=new_root2`, descends c→a→t ("cat" still non-final, now children `{s, n→…}`), finds it non-final, builds a fresh final "cat" leaf whose `store` is cloned from the current node (children `{s, n→catnip}` retained), CAS `new_root2→new_root4` wins. Final: cat,cats,catnip all final. Same outcome.

Case C — both CAS `expected=root0`, one wins, loser retries → degenerates to A or B. The root CAS is a total order on publications, so no third outcome.

**RT#1's F1/F2 data-loss trace is closed.** Their trace required "publish spine to non-final node_A, then later finalize." Under (1a) this **cannot be expressed**: `I`'s publication *is* the finalization (fresh final leaf in the root CAS). When `R=remove("cat")` runs, either (a) it sees "cat" final (I already CAS-won) and does a real remove with seq > seq(I) — replay orders remove last, matching live; or (b) it sees "cat" non-final (I hasn't CAS-won) and is a genuine `AlreadyAbsent` no-op that does NOT rank and does NOT consume a seq (§3.4), while I's later CAS makes "cat" present — replay sees only I's ranked Insert, present, matching live. No interleaving has a finalized-but-unpublished node. **QED.**

### 1.4 Claim-before-CAS, discard-on-loss — the monotonicity rule (now universally valid)

`next_commit_seq() = commit_seq.fetch_add(1, AcqRel) + 1`, claimed at the **top of each CAS-retry iteration** (immediately before the `build_*` + the root-read used as the CAS `expected`), used as the publication's seq on CAS-win, **discarded** on CAS-loss (a harmless gap; we only ever COMPARE commit_seq, never require contiguity).

**Theorem `CommitSeqMonotone`.** If `X ≺_CAS Y` on the same term, then `commit_seq(X) < commit_seq(Y)`. *Proof.* Under (1a), every op's sole linearization point is its winning root CAS, whose `expected` is the root read at the same iteration's top (adjacent to the claim). A winning CAS implies no other publication landed between that root-read and the CAS (else `expected` is stale and the CAS fails → re-claim). Given `X ≺_CAS Y`: X's winning CAS precedes Y's winning CAS. X's CAS cannot lie in `(Y.root_read, Y.CAS)` (else Y fails), and X's CAS produces a root ≠ Y's expected, so X's CAS cannot precede Y's root-read without Y reading X's root — hence Y's winning iteration's root-read observes X's effect, i.e. Y's winning claim was taken AFTER X's winning CAS, which was after X's winning claim. `fetch_add` monotonicity ⇒ `commit_seq(Y) > commit_seq(X)`. ∎ (D1's argument was correct but *vacuously inapplicable to insert* because insert had no single LP. (1a) supplies it.)

The `commit_seq` field lives on the inner trie beside `lockfree_cache`/`cas_retries` (`mod.rs:470-476`) as `pub(crate) commit_seq: AtomicU64`, **trie-owned, seeded once at open (§2), and MUST survive `enable_lockfree`** (`:149` sets only `lockfree_root`/`lockfree_cache`, so no change needed — the precise A.2 fix).

### 1.5 Insert return-value semantics without `try_set_final`

`insert_cas_durable` returns `Ok(true)` iff this call newly inserted. Today `newly = node.try_set_final()`. Under (1a): `Err(AlreadyExists)` ⇒ already present on the snapshot ⇒ `Ok(false)`; `Inserted(...)` on CAS-win ⇒ this op published the absent→present transition ⇒ `Ok(true)`. Racing the same new term: one CAS wins → `Inserted` → true; the loser gets `Conflict`, retries, sees `is_final()` → `AlreadyExists` → false. Exactly one true — the single arbiter is now the **root CAS**, not `fetch_or`.

### 1.6 The non-durable `insert_cas` path (`:193-251`)

`insert_cas` (non-durable, no WAL/rank) keeps `try_set_final` (`:225`) and the old non-finalizing `build_path_recursive` **as-is**. Its correctness is membership-monotone and loom-checked; it has no replay key to mis-order, so the split-LP is not a bug there. Keeping it avoids re-looming the non-durable path. The two paths are already separate methods (`insert_cas` vs `insert_cas_durable`), so no drift is introduced. (§6.2/§6.7 attack this.)

### 1.7 C1′ (RT#1 F5) — move the value-CAS bail-claim to the READ instant

The value-CAS `compare_and_swap`/`get_or_insert` phantom-bail ranks a bailed orphan with a read-snapshot seq `g_read`. RT#1 F5: D1 took the claim at APPEND (after the READ), so a superseder can claim a LOWER seq mid-CAS while `g_read` ties/exceeds it. **Fix:** call `next_commit_seq()` at the **read-snapshot instant** (the `enter_read()` pin that loads the root for the resident/match decision), BEFORE the data append. Any op that supersedes (publishes after our read) `fetch_add`s after us ⇒ strictly higher seq ⇒ `g_read < commit_seq(superseder)` strictly in the durable domain. The bail record carries `g_read`; reconcile orders it strictly below the superseder. (These value-CAS methods slot in at DG3; they share `next_commit_seq` + the rank record.)

---

## §2. AXIS 2 — durable commit_seq FLOOR (RT#2 F1/F2)

### 2.1 Header layout — exact byte offsets

`WalHeader` (`wal/header.rs:14-23`) is 64 bytes: `magic[0..8]`, `version[8..12]`, `checkpoint_lsn[12..20]`, `reserved[20..64]` (44 bytes, `reserved: [u8;44]` indexed 0..44). D2 carves a typed field:

- **`commit_seq_floor: u64` at byte offset 20..28** (= `reserved[0..8]`). `reserved[8..44]` (bytes 28..64) stays reserved/zero.

```
pub struct WalHeader {
    pub magic: [u8; 8],
    pub version: u32,
    pub checkpoint_lsn: Lsn,
    pub commit_seq_floor: u64,   // NEW: byte offset 20..28
    pub reserved: [u8; 36],      // was [u8;44]; now 28..64
}
```
`to_bytes` (`:61-68`): write `commit_seq_floor.to_le_bytes()` into `buf[20..28]`, `reserved` into `buf[28..64]`. `from_bytes` (`:71-99`): read `buf[20..28]`, `buf[28..64]`. `new()` (`:51-58`): `commit_seq_floor: 0`.

**Layout back-compat:** a v2 file has `reserved[0..8] == 0`, so reading yields `commit_seq_floor = 0`, which the seed formula (§2.4) treats as "no floor" and falls back to the scan max — exactly v2 behavior. The *comparator* refusal is Axis 5 (§5), enforced independently.

### 2.2 Writer API — `set_commit_seq_floor`, mirroring `checkpoint()`

```
pub fn set_commit_seq_floor(&self, floor: u64) -> Result<(), WalError> {
    let mut header = self.header.lock()...;
    if floor <= header.commit_seq_floor { return Ok(()); }   // monotone, like set_min_lsn
    header.commit_seq_floor = floor;
    let mut file = self.file.lock()...;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header.to_bytes())?;
    file.flush()?; file.get_ref().sync_all()?;
    file.seek(SeekFrom::End(0))?;
    Ok(())
}
pub fn commit_seq_floor(&self) -> u64 { self.header.lock()....commit_seq_floor }
```
Monotone, durable (fsync), in the header (survives reopen) — the exact contract `checkpoint_lsn` has.

### 2.3 The rotate/truncate carry — the trap RT#2 didn't name but D2 must close

**`rotate_to_archive` (`writer.rs:424-472`) writes a FRESH `WalHeader::new()` (`:458`)** with `commit_seq_floor=0` AND `checkpoint_lsn=0`. It carries `next_lsn`/`synced_lsn` (`:463-466`) but zeroes both floors. Today the LSN floor is re-established by `set_min_lsn` at the call site. D2 does the same for commit_seq:
1. In `rotate_to_archive`, before writing the new header (`:458`): `let carried = self.header.lock()....commit_seq_floor; let mut header = WalHeader::new(); header.commit_seq_floor = carried;`.
2. `truncate` (`:338-364`) sets `checkpoint_lsn=0` (`:353`) but must **preserve** `commit_seq_floor` (don't touch it in the header rewrite at `:352-356`).

**Why carry (vs re-floor-at-callsite):** the invariant "the floor is monotone and never lost across rotate/truncate" is enforced in the writer; the checkpoint path then advances it (§2.5). Robust even if a rotate happens without an immediate floor-update.

### 2.4 Seed formula — ALL open paths

Add `WalReader::read_header(path) -> Result<WalHeader>` (reads the first 64 bytes → `from_bytes`), or use `WalWriter::commit_seq_floor()` where a writer is in hand.

**Seed = `max(header.commit_seq_floor, max over the FULL active-WAL scan of CommitRank.commit_seq)`** (scan max taken BEFORE the checkpoint-skip filter — a post-checkpoint op must out-rank a pre-checkpoint same-term op still in a retained segment).
- **mmap ctor (`mmap_ctor.rs:292-324`):** the scan already tracks `max_lsn`/`checkpoint_lsn`; add `max_commit_seq` from `CommitRank.commit_seq`, read `header.commit_seq_floor`, seed `inner.commit_seq = AtomicU64::new(max(floor, max_commit_seq))` (struct init `:338-369`, add beside `cas_retries:368`).
- **io_uring ctor (`io_uring_ctor.rs:124-199`):** identical (twin).
- **`RecoveryManager`:** add `max_commit_seq: Lsn` to `RecoveredState`, compute in `redo_phase`; callers seed from it.
- **`recover_from_archives` (`mmap_ctor.rs:1110-1160`) → `rebuild_from_wal_segments` (`recovery.rs:1443`):** thread out `max_commit_seq` (change return to include it); seed after `create_with_config` (`:1135`). (This path ALSO needs reconcile, §3.3.)

**Floor on seed:** `commit_seq = AtomicU64::new(seed)`; first `next_commit_seq()` → `seed+1`, strictly above every subsumed/durable seq. Globally monotone across restarts AND checkpoints — the A.2/post-checkpoint-reseed class (RT#2 F1) closed: after a checkpoint the floor = max subsumed seq (§2.5), so a new op gets `floor+1`, never `1`.

### 2.5 Checkpoint-time floor update — "max commit_seq subsumed"

Maintain a trie `max_durable_commit_seq: AtomicU64`: every durable producer, after appending its CommitRank and marking the watermark, does `max_durable_commit_seq.fetch_max(commit_seq, AcqRel)`. At checkpoint (`persist.rs:147`, where the `Checkpoint` WAL record is written), call `wal_writer.set_commit_seq_floor(max_durable_commit_seq.load(Acquire))`.

Because the checkpoint reclaims only records ≤ watermark (the #41 invariant, `committed_watermark.rs:6-12`, asserted `persist.rs:140-146`), every reclaimed record's commit_seq ≤ `max_durable_commit_seq`, so the floor dominates every subsumed seq. After rotate/truncate the floor is carried (§2.3); the seed of the now-empty active WAL is `max(floor, 0) = floor`. **The LSN floor's exact discipline, applied to commit_seq.**

`max_durable_commit_seq` is itself seeded on open from the same computed `seed`, so a second checkpoint after reopen never lowers the floor. **Coupling (see §6.3):** under overlay writes the checkpoint must capture the overlay (flip-F3) for the floor to dominate overlay-write seqs; pre-flip (owned-tree checkpoint, overlay durability via WAL replay) the floor = scan max anyway. DG2 sequences this.

---

## §3. AXIS 3 — gate the two-append window in REPLAY (RT#1 F3/F4, RT#2 F4/F5)

### 3.1 The defect, precisely

`reconcile_lww` (`recovery.rs:253`) assigns unranked records `generation_of(lsn) = lsn` (`:273`), LARGE, so an **unranked-but-durable** record sorts ABOVE ranked records and WINS. The two-append window (data appended+synced, CAS, then CommitRank appended+synced) means a crash between data and rank leaves a durable data record with no rank. D1 argued "no same-term competitor above the frontier" — RT#1 F3 / RT#2 F4 falsify it (two concurrent same-term ops both mid-window; or a torn batch where a later same-term op's rank crashed).

### 3.2 The fix — watermark-gated reconcile; unranked-above-watermark records LOSE

**The committed watermark is the durable boundary of "confirmed visible."** An LSN is confirmed-visible iff it has a CommitRank (the rank is appended only after the CAS wins; the watermark advances only after BOTH appends). Therefore:
- **Ranked record** → confirmed-visible; use its `commit_seq`.
- **Unranked, data-LSN ≤ watermark** → impossible under Order-A (both appends completed); if observed, conservative LOSE.
- **Unranked, data-LSN > watermark** → genuinely ambiguous (CAS may/may-not have landed; rank certainly didn't). **Never confirmed visible. Must NOT win.**

**Decision: DROP unranked-above-watermark records** (do not replay them). An unconfirmed write was never acked (ack waits for the watermark, which waits for the rank). Dropping loses nothing promised and cannot resurrect/erase a confirmed key. (A losing `commit_seq=0` is wrong for *increments* — a dropped-vs-zero-ranked unconfirmed delta would still be summed; DROP is correct for membership AND counters.)

**Signature:** `reconcile_lww(recovered_ops, loaded_from_disk, checkpoint_lsn, committed_watermark: Lsn, wal_version: u32, tx_states: &HashMap<u64,TxState>)`.
```
let generation_of = |lsn| rank.get(&lsn).copied();   // Option<u64> now
// expansion loop:
let cseq = match generation_of(lsn) {
    Some(s) => s,                                    // ranked → confirmed
    None if lsn <= committed_watermark => /* anomaly: LOSE/drop */ continue,
    None => continue,                                // unranked above watermark → DROP
};
```
Sort by `(cseq, lsn)`. No record without a confirmed `commit_seq` can sort above a ranked one.

**CRITICAL (self-found, §6.4): the `committed_watermark` here is NOT the runtime `next_lsn-1` field — it is RECONSTRUCTED from the ranks during the scan** as the largest `L` such that every data-LSN in `(checkpoint_lsn, L]` is ranked (the contiguous-ranked prefix). Unranked data-LSNs are holes. The runtime watermark field is seeded `next_lsn-1` for liveness, but reconcile must use the rank-reconstructed boundary, else nothing is "above watermark" and the drop rule is a no-op. (For in-order v1/v2 core WALs, pass `Lsn::MAX` — every record is confirmed, nothing drops.)

### 3.3 Route the secondary paths through reconcile (RT#2 F2, A.1)

- **`rebuild_from_wal_segments` (`recovery.rs:1443-1487`):** today raw via `recovered_operations_from_record` in LSN order (`:1473`), CommitRank → `vec![]` (`:370`). **Fix:** collect all segment records into one `Vec<(Lsn, WalRecord)>` (segments LSN-ordered), route through `reconcile_lww`, thread out `max_commit_seq`. Archive segments hold only checkpoint-subsumed = confirmed records, so pass `Lsn::MAX` as the watermark.
- **`redo_phase` (`recovery.rs:756-834`) + `RecoveryManager::recover`:** route committed ops through `reconcile_lww` with `tx_states` from `analysis_phase` (`:658`). The core path's WAL is in-order (only the lock-free overlay produces out-of-order) ⇒ pass `Lsn::MAX`; the lock-free char path passes its reconstructed watermark.
- **`IncrementalRecovery::process_record` (`recovery.rs:932`):** §4.4.

**Unification target:** all four funnel into `reconcile_lww`. `replay_records_lww` (`mutation_core.rs:252`, the char wrapper) gains the new params; the ctors already call it (`mmap_ctor.rs:403`, `io_uring_ctor.rs:227`).

### 3.4 Two concurrent same-term ops both in their windows

With (1a) each op's data record is ranked only after its root CAS wins (a total order). Crash scenarios:
- Both data durable, neither ranked, both above watermark ⇒ **both dropped** ⇒ term reverts to pre-both state (neither was acked). Correct.
- One ranked (≤watermark), the other unranked-above ⇒ ranked wins, other dropped. Correct.
- The idempotent `AlreadyExists`/`AlreadyAbsent` arms (today they append a CommitRank `:388`/`:572` AND `mark_committed` `:389`) **must NOT under D2**: they publish nothing (no root CAS), so no rank, no `mark_committed`, no commit_seq. The `Insert` data record the durable path appended at `:329` before the loop, if the loop yields `AlreadyExists`, is an unranked no-op duplicate; with no `mark_committed` the watermark never confirms it ⇒ reconcile drops it (the real writer's ranked record wins). **This is the F6 idempotent-inversion fix.**

Net: F3/F4 closed — no unconfirmed record wins; two-window races resolve to the confirmed op or a clean drop.

---

## §4. AXIS 4 — transactions (RT#1 F4, RT#2 F5/F6)

### 4.1 `reconcile_lww` honors the tx state machine

`reconcile_lww` takes `tx_states` (built by `analysis_phase`, `recovery.rs:678-742`, which already produces `transactions: HashMap<u64, TransactionState>`). During expansion, maintain a `current_tx` cursor (like `redo_phase:769`): inside a `BeginTx{tx_id}`..`CommitTx`/`AbortTx` span, **drop** every data record whose tx is not `Committed`. `recovered_operations_from_record` already maps Begin/Commit/AbortTx → `vec![]` (`:356-358`); the gating is in the *reconcile expansion*. Closes A.4 — both the ctor path and `redo_phase` now consult `tx_states` and agree.

### 4.2 Per-op commit_seq inside a committed tx

D1 gave one CommitRank at `CommitTx` (RT#2 F5: a single rank-crash unranks the whole batch). **D2: per-op commit_seq.** Each data record in a committed tx carries its own commit_seq so an in-tx increment orders against a non-tx same-term op. The lock-free overlay has no tx batching — `commit_document`/Begin/Commit-Tx is **OwnedTree-mode** (lock-free producers are single-op; the only batch record is single-entry `BatchIncrement` `:1536`). The OwnedTree path is `&mut self`-serialized, so assign commit_seq sequentially as each batched op is logged (application order), from the same `commit_seq` counter. Reconcile orders by `(commit_seq, lsn)` and tx-gates.

### 4.3 Torn-mid-batch durability under group-commit

RT#1 F4 / RT#2 F5: Order-A + group-commit doesn't guarantee atomic {per-op data appends, CommitTx append}; a partial fsync could leave op-k's data missing while CommitTx is present ⇒ replay applies a strict subset. **D2 closes this:**
- The WAL is append-only; recovery stops at the first corrupt/torn record (`analysis_phase:695-705`, `redo_phase:776-781`, `rebuild:1462-1467` break at the durable prefix). A torn op terminates the prefix BEFORE a later `CommitTx` is reached ⇒ you cannot have op-k missing while a later CommitTx is durable.
- The remaining hazard is a *non-contiguous* fsync (op-k's bytes lost, op-(k+1)+CommitTx present). Defense: the committed watermark. CommitTx is acked only when the watermark covers its LSN, requiring the contiguous prefix `1..=CommitTx_lsn` all committed. A hole at op-k stalls the watermark below CommitTx ⇒ the whole batch is above-watermark ⇒ **dropped (§3.2).** Per-record **CRC** (`reader.rs:71-82`) detects an interior torn op as corruption, ending the prefix.
- ⇒ either fully durable (prefix intact through CommitTx, watermark covers it, all applied) or dropped. **No strict-subset application.**

### 4.4 IncrementalRecovery v3 — fail-closed for never-checkpoint, per-window otherwise

`IncrementalRecovery` (`recovery.rs:856-975`) STREAMS and uses a single `pending_ops` with `BeginTx` doing `pending_ops.clear()` (`:940`) — tx/order-unsafe across windows (RT#2 F6). **D2:**
- **A checkpoint bounds the reorder window** (records below it are reconciled into the on-disk image). Buffer **per-checkpoint-window**: accumulate since the last checkpoint, run `reconcile_lww` on that window at the next checkpoint/EOF, emit, reset. Window size bounded by the checkpoint interval (documented operational bound).
- **Never-checkpoint + v3 ⇒ one unbounded window ⇒ FAIL-CLOSED.** Return an error ("streaming recovery of an un-checkpointed v3 WAL is unavailable; use `RecoveryManager::recover` or checkpoint first"). An **availability constraint, not a safety hole** — the data recovers fully via the non-streaming path (§3.3). Detection: peek the header version in `IncrementalRecovery::new`; if v3 and analysis finds no checkpoint, make `next_batch` return the error. Documented in GAP_LEDGER.
- **Tx-safety within a window:** replace single `pending_ops`+`clear()` with per-tx pending maps (like `redo_phase:770`).

---

## §5. AXIS 5 — back-compat (RT#2 F3): refuse in-place v2→v3 mixing

### 5.1 The defect
A v3 binary opens & APPENDS to a cf1f80c **v2** file (`from_bytes` accepts `[MIN_SUPPORTED=1 ..= VERSION]`, `header.rs:82`; no refuse-v2-for-append guard). The "v2 root-version vs v3 commit_seq comparator" is a distinction without a difference — both are u64-ascending `(generation,lsn)` sorts (`recovery.rs:296`). A small v2 root-version (2) and a small early v3 commit_seq (2) on one term tie-break by lsn ⇒ mis-sort.

### 5.2 Decision: REFUSE to APPEND to a sub-current-version file
Header version **2 → 3** (`header.rs:38`, `VERSION = 3`). Two enforcement points:
1. **READ stays permissive within `[MIN_SUPPORTED ..= VERSION]`** — a v2 file is *readable* for recovery/migration, reconciled under the **v2 comparator** (§5.3). `from_bytes` keeps accepting `1..=3`.
2. **APPEND (open-for-write) REFUSES `version < VERSION`.** Enforcement at the **writer open path** — `open_or_create_async_wal` (called `mmap_ctor.rs:327`, `io_uring_ctor.rs:157`) and `WalWriter` open. Opening an existing WAL for writing reads its header; if `version < WalHeader::VERSION`, return `Err(WalError::UnsafeVersionMixing { found, current })`. A freshly-created WAL is `VERSION=3`. The v2→v3 mix is **impossible**.

### 5.3 Per-record version-tagged comparator (defense in depth for READ)
`reconcile_lww` takes `wal_version`:
- `>= 3`: CommitRank carries **commit_seq** (durable global); order by `(commit_seq, lsn)`, watermark+tx-gated.
- `== 2`: CommitRank carries **root-version** (per-lifetime); order by `(root_version, lsn)` — the cf1f80c single-session comparator, valid because a pure-v2 file has one root lifetime/session and no floor (`reserved[0..8]=0`). The legacy comparator, explicitly version-selected so a v2 rank is never read as a commit_seq.
- `== 1`: no CommitRank → `generation_of=lsn` fallback (in-order replay).
Append-mixing refused ⇒ a single file is homogeneous ⇒ the version tag fully disambiguates.

### 5.4 Migration story
A pure-v2 file = **recover-and-rewrite**: open read-only, `RecoveryManager::recover` under the v2 comparator, checkpoint to a fresh v3 file (v3 header + `commit_seq_floor` = max seq assigned during the rewrite). An explicit `migrate_v2_to_v3(path)` tool (off the hot path, opt-in, pre-flip). No silent in-place upgrade. Documented in GAP_LEDGER/UNSAFE_BOUNDARY (a v2 binary already refuses v3 via `version > VERSION` at `:82`).

---

## §6. SELF-RED-TEAM — attacking D2 before the red-team does

### 6.1 (Axis 1) `as_final()` clone cost + stale-snapshot prefix race
**Attack:** every insert hitting an existing non-final prefix node now allocates a fresh `as_final()` + path-copies the spine each retry (vs in-place `fetch_or`). Retry storm livelock? Stale snapshot drops a concurrently-added child?
**Defense:** the value/remove paths already allocate-per-retry and are loom/proptest/TLA-checked; insert now matches them — no new livelock class. `as_final` clones `store` from the node on the *current* snapshot (`overlay/node.rs:813`); the CAS `expected` is that snapshot's root — a concurrent child-add fails the CAS → retry re-clones the larger `store` (§1.3 Case B). **Residual:** retry amplification under pathological same-prefix contention is *performance*, bounded by `cas_retries` telemetry (`:395`). Bench at DG1.

### 6.2 (Axis 1) Does deleting `try_set_final` break the non-durable `insert_cas`? — THE sink-the-design seam
**Attack:** if `build_path_recursive`'s base case returns a *final* leaf, `insert_cas`'s subsequent `try_set_final` (`:225`) sees an already-final node → returns `false` → every new prefix term reported duplicate = **the Phase-A bug reintroduced on the non-durable path.**
**Defense — the builder MUST be split.** Add `build_final_path_recursive` (final leaf, durable path only); leave `build_path_recursive` (shared non-final leaf, `insert_cas`+`try_set_final`). ~40 duplicated lines, but the principled split: durable=single-LP, non-durable=arbiter. **§1.2 Change 1 targets a NEW method, not the shared one.** (Verified: `insert_cas:221` and `insert_cas_durable:345` both reach `build_path_recursive` via `insert_lockfree_recursive`; the durable caller must call the finalizing variant.) This is the most important self-correction.

### 6.3 (Axis 2) Floor over-advances under the pre-flip owned-tree checkpoint
**Attack:** could the floor be set from overlay-write commit_seqs whose effects are NOT in the owned-tree image (the pre-flip safety boundary, `:272-283`)?
**Defense:** `checkpoint_lsn = next_lsn` at capture (`:153`) with the #41 no-racing-writer assert (`:140-146`) ⇒ `checkpoint_lsn ≥ watermark`, so the floor ≤ max subsumed seq ≤ records ≤ checkpoint_lsn. **Residual:** under overlay writes, the floor logic is sound only under the *flipped* (overlay-capturing) checkpoint; pre-flip, overlay durability rests on WAL replay (no checkpoint between write and recovery), where the floor = scan max. **The floor and the flip are coupled; DG2 lands with/after F3.**

### 6.4 (Axis 3) The reconstructed watermark — THE second sink-the-design seam
**Attack:** the ctor seeds the runtime watermark to `next_lsn-1` (`mmap_ctor.rs:347`). If reconcile used that, EVERYTHING is "≤ watermark" ⇒ nothing dropped ⇒ F4 NOT closed.
**Defense — reconcile must RECONSTRUCT the watermark from the ranks** (the largest `L` with every data-LSN in `(checkpoint_lsn,L]` ranked — the contiguous-ranked prefix), computed in the same scan that builds the `rank` map. Unranked data-LSNs are holes. §3.2 is corrected to compute this, NOT read the runtime field. **A `_UnsafeRuntimeWatermark.cfg` negative control proves the seam is real.**

### 6.5 (Axis 3) Increment + drop interaction
**Attack:** increments log a *delta* `BatchIncrement` (`:1536`); dropping a confirmed delta whose rank crashed corrupts the sum.
**Defense:** under D2 increments are RANKED (A.3 — `try_increment_cas_durable` appends a CommitRank after its CAS, which it currently does NOT, `:1547-1553`). A confirmed increment has a rank (≤ reconstructed watermark); an unconfirmed one (rank crashed, above watermark) was never acked → dropping its delta is correct. **Residual:** REQUIRES the increment-rank wiring (DG1) to land WITH the drop rule (DG3) — sequenced in §8.

### 6.6 (Axis 4) Per-op tx commit_seq vs OwnedTree serialization
**Attack:** per-op commit_seq assigned at log time; a non-tx op on the same term interleaving between batch-logging and CommitTx?
**Defense:** OwnedTree mode is `&mut self`-serialized — no concurrent op on the instance during `commit_document`. The lock-free overlay has no tx batching. **Residual:** if a future design runs overlay+OwnedTree concurrently on one instance, this breaks — out of scope, flagged.

### 6.7 (Axis 5) v3 binary opens a v2 file READ-ONLY then enables writes
**Attack:** open read-only (recovery) passes, then a durable write appends v3 to the v2 file.
**Defense:** refusal is at the **writer open** (`open_or_create_async_wal`), which every ctor calls (`:327`/`:157`); a v2 file fails writer-open ⇒ no writable trie from a v2 file. No "open-RO-then-upgrade" API bypasses it; `WalReader` never appends. Closed at construction.

### 6.8 (Axis 1/3) Discard-on-loss gaps vs the reconstructed watermark
**Attack:** commit_seq gaps (discarded claims) — could a gap make `max_durable_commit_seq` skip / the floor too low, or create LSN holes?
**Defense:** the reconcile watermark walks data-LSNs (not commit_seqs), so commit_seq gaps create no LSN holes. The floor is the MAX observed commit_seq, not a count; gaps below the max are irrelevant; a discarded claim's value appears in no record (neither floor candidate nor replay key). No effect. ∎

### 6.9 Residual seams summary (red-team probe these first)
1. **§6.2 (builder-split) and §6.4 (reconstructed-watermark)** are the two corrections that, if missed, sink D2 — now baked into §1.2/§3.2.
2. **§6.3/§6.5 sequencing coupling** — floor↔flip, drop-rule↔increment-rank. §8 sequences so no intermediate state is unsafe.
3. **§6.6 OwnedTree-overlay concurrency** assumed-absent (true today); flagged.

---

## §7. Formal re-proof — TLA invariants, negative controls, extended soak

### 7.1 TLA — extend `formal-verification/tla+/LockFreeOverlayDurableReplay.tla` → `DurableGlobalOrder`
Model: `commitSeq` (durable counter + floor), `walVersion`, `wal` (Insert/Remove/Increment/CommitRank/BeginTx/CommitTx/AbortTx), `present`/`removed`/`value`, `committed` (= contiguous ranked prefix), `floor`, `replayed`. Actions: `Append`; `RootCas(t)` (SINGLE LP — updates visible state AND claims `commitSeq'+1`, models (1a) atomic-final); `AppendRank(t)` (binds data-LSN→commit_seq, advances `committed'`); `Checkpoint` (`floor'=max subsumed`, truncate ≤ cp); `Rotate` (carries floor); `Restart` (resets root/version-domain, seeds `commitSeq'=max(floor,scan)`, reconstructs `committed'` from ranks); `CrashRecover` (reconcile: per-term max commit_seq among RANKED records with data-LSN ≤ committed; DROP unranked-above; tx-gate non-Committed).

**Invariants:** `ReplayEqualsCommittedVisible` (Axis 1 headline), `ReplayEqualsCommittedValue` (Axis 1/4 counters), `CommitSeqMonotone` (§1.4), `FloorDominatesSubsumed` (Axis 2), `SeedAboveDurable` (Axis 2), `NoUnconfirmedWins` (Axis 3), `NoUncommittedTxReplay` (Axis 4), `AllPathsAgree` (CONSTANT RECOVERY_PATH ∈ {Ctor,Archive,RecoveryMgr,Incremental}), `NoVersionMix` (Axis 5).

**Negative controls (`_Unsafe*.cfg`, each MUST fire its named invariant; register in `scripts/verify-formal-correspondence.sh`):**
- `_UnsafeSplitLP.cfg`: `RootCas` doesn't finalize; a separate deferred `Finalize(t)` (no seq bump) is visibility ⇒ violates `ReplayEqualsCommittedVisible` (F1 cat/cats/catnip).
- `_UnsafeNoFloor.cfg`: Restart seeds `0` ⇒ violates `SeedAboveDurable`/`ReplayEqualsCommittedVisible` (RT#2 F1).
- `_UnsafeRawLsnPaths.cfg`: Archive/RecoveryMgr/Incremental order by raw LSN ⇒ violates `AllPathsAgree` (s019).
- `_UnsafeUnrankedWins.cfg`: unranked `generation_of=lsn`, no watermark gate ⇒ violates `NoUnconfirmedWins` (RT#1 F3 / RT#2 F4).
- `_UnsafeTxIgnored.cfg`: reconcile ignores tx_states ⇒ violates `NoUncommittedTxReplay` (RT#2 F5).
- `_UnsafeRankFreeIncrement.cfg`: increments unranked ⇒ violates `ReplayEqualsCommittedValue` (A.3).
- `_UnsafeVersionMix.cfg`: Append permits sub-VERSION ⇒ violates `NoVersionMix` (RT#2 F3).
- `_UnsafeRuntimeWatermark.cfg`: reconcile uses full-frontier `next_lsn-1` not the reconstructed ranked-prefix ⇒ violates `NoUnconfirmedWins` (the §6.4 self-found seam).

### 7.2 Extended soak (each ≥50× green, Immediate + GroupCommit, real-disk; the blind spots both red-teams exploited)
The current soak keys (`d{t}_{i:04}`, `s{:03}`) **share no proper prefixes** — why RT#1 F1 survived. Add:
1. **Prefix-reuse insert‖remove/increment** (Axis 1, F1/F2/F6): `{c, ca, cat, cats, catnip, …}` concurrent across the prefix chain; reopen == quiesced live. Fails pre-(1a).
2. **Post-checkpoint reseed** (Axis 2, F1): S1 insert+remove+increment, checkpoint, S2 same-term insert, crash (no 2nd checkpoint), reopen ⇒ S2 wins (commit_seq > floor).
3. **Archive-rebuild** (Axis 3/A.1, F2): rotate segments, `recover_from_archives`, == live; same-term last-writer picked by commit_seq across segments.
4. **Mixed-file refusal** (Axis 5, F3): create a v2 WAL, open-for-write with v3 ⇒ `Err(UnsafeVersionMixing)`; read-only v2-comparator recovery still correct.
5. **Torn-tx-batch** (Axis 4, F4): commit_document, truncate mid-batch before CommitTx, reopen ⇒ whole batch absent (dropped, not subset); twin where fully durable ⇒ all-present.
6. **Never-checkpoint IncrementalRecovery** (Axis 4, F6): stream v3 un-checkpointed ⇒ fail-closed error; same WAL recovers via `RecoveryManager::recover`.
7. **Two-concurrent-same-term-windows** (Axis 3, F3): two threads `insert_cas_durable` the same new term, rendezvous at `AfterCommit` (`RendezvousPhase:39-62`), crash one before its rank, reopen ⇒ exactly the confirmed one.

**Deterministic regressions (fail-pre/pass-post):** `stage_prefix_split` (cat/cats/catnip, §1.3) + `stage_post_checkpoint_reseed`, extending the `stage_s019` harness (`:2804`).

**Stay-green:** `concurrent_durable_writers_all_survive_reopen` (`:2511`), `concurrent_durable_mixed_insert_remove_reopen_equals_live_set` (+group-commit twin), `insert_cas_durable_survives_reopen_without_checkpoint` (`:2288`), `try_increment_cas_durable_survives_reopen_without_checkpoint` (`:2443`), `recovery_replay_completeness_correspondence`, OD4 determinism, all atomicity specs, full gate exit 0, 0 new unsafe.

---

## §8. Phased implementation — DG0–DG7, green-gated, reversible until the flip

Each gate: `nextest` (≥ current) + `scripts/verify-formal-correspondence.sh` exit 0 + unsafe-inventory exit 0; systemd real-disk; `RUN_TLC=1` at the formal gate. DG0–DG5 revert by code; DG6–DG7 verification-only; the one one-way step is the header `2→3` bump (fail-closed, opt-in, pre-flip).

- **DG0 — `commit_seq` field + floor plumbing (no behavior change).** Add `commit_seq`+`max_durable_commit_seq` to the inner struct (`mod.rs:476`), seed both `max(header floor, scan)` in both ctors + `recover_from_archives`. Add `commit_seq_floor` to `WalHeader`, `set/get` to `WalWriter`, rotate/truncate carry. Key still root-version. **Rollback:** delete fields. **Gate:** existing green; floor round-trips (header-bytes + reopen unit test).
- **DG1 — single-LP insert (1a) + builder split (§6.2).** Add `build_final_path_recursive`; route `insert_cas_durable` through it; **delete `try_set_final` from the durable `Inserted` arm**; keep `insert_cas` on the old builder. Source the key from `commit_seq` (claim-before-CAS, discard-on-loss) in ALL durable producers (`:344,532,1659,1757`). **Rank increments** (`:1547`, §6.5). Idempotent arms: no rank, no `mark_committed` (§3.4). Header 2→3. **Rollback:** revert builder+key+header. **Gate:** prefix-split regression PASSES; soaks green; `CommitSeqMonotone` holds.
- **DG2 — durable floor at checkpoint + seed-from-floor active.** Wire `set_commit_seq_floor(max_durable_commit_seq)` into `publish_durable_and_reclaim` (`:147`). **Coupling (§6.3):** land with/after the overlay-capturing checkpoint (flip-F3) OR document pre-flip floor==scan. **Rollback:** stop setting floor. **Gate:** post-checkpoint-reseed PASSES; `FloorDominatesSubsumed`/`SeedAboveDurable`.
- **DG3 — reconcile watermark-gate + reconstructed ranked-prefix watermark (§3.2/§6.4) + C1′ (§1.7).** `reconcile_lww` gains `committed_watermark`/`wal_version`/`tx_states`; reconstruct the ranked-prefix watermark; DROP unranked-above; version-select the comparator. C1′ bail-claim at the read instant. **Rollback:** revert to ungated. **Gate:** two-window + torn-window PASS; `NoUnconfirmedWins`/`_UnsafeRuntimeWatermark` fire.
- **DG4 — unify all four recovery paths (A.1).** Route `rebuild_from_wal_segments` (thread `max_commit_seq`), `redo_phase`/`RecoveryManager`, `IncrementalRecovery` (per-window + fail-closed) through `reconcile_lww`. **Rollback:** per-path. **Gate:** archive-rebuild + never-checkpoint-Incremental + mixed-file PASS; `AllPathsAgree`.
- **DG5 — tx gating + per-op tx commit_seq (A.4).** `reconcile_lww` tx-gates; OwnedTree per-op commit_seq. **Rollback:** revert gating. **Gate:** torn-tx-batch + aborted-tx PASS; `NoUncommittedTxReplay`.
- **DG6 — FORMAL re-proof (HARD GATE).** Extend TLA to `DurableGlobalOrder` with all §7.1 invariants + controls. **If ANY `_Unsafe*.cfg` PASSES → STOP.**
- **DG7 — extended soak gate.** All §7.2 scenarios ≥50× (Immediate+GroupCommit) + deterministic regressions. Unblocks the flip. Verification-only; reversible until the flip flag flips.

**Sequencing invariants honored:** drop rule (DG3) AFTER increments ranked (DG1) — §6.5; floor (DG2) coupled to F3 — §6.3; builder split (DG1) prevents the non-durable Phase-A regression — §6.2; append-refusal (DG1) prevents v2/v3 mixing before any v3 record is written — §6.7.

---

## Critical Files
- `src/persistent_artrie_char/lockfree_cas.rs` — Axis 1: NEW `build_final_path_recursive` for the durable path, delete `try_set_final` from `insert_cas_durable:347`, claim-before-CAS `commit_seq` in all durable producers (`:344,532,1659,1757`), rank `try_increment_cas_durable:1547`, idempotent-arm no-rank (`:383,567`); soak/regression harness (`:2609,2804`).
- `src/persistent_artrie_core/recovery.rs` — Axis 3/4: `reconcile_lww:253` (watermark + reconstructed ranked-prefix + version-select + tx-gate), unify `redo_phase:756`/`IncrementalRecovery:932`/`rebuild_from_wal_segments:1443`.
- `src/persistent_artrie_core/wal/header.rs` — Axis 2/5: `commit_seq_floor` at byte 20..28 (`reserved`→`[u8;36]`), `to/from_bytes:61-99`, `VERSION 2→3` (`:38`).
- `src/persistent_artrie_core/wal/writer.rs` — Axis 2: `set/get_commit_seq_floor` (mirror `checkpoint:303`), carry floor across `rotate_to_archive:458` + `truncate:353`.
- `src/persistent_artrie_char/mmap_ctor.rs` — Axis 2: seed from `max(floor, scan)` (`:300-321`, struct init `:338-369`), route+seed `recover_from_archives:1137`; io_uring twin (`io_uring_ctor.rs:124-199`) + checkpoint floor-set (`persist.rs:147`) change identically.
