# Design "R-B": PROVEN Overlay DELETE for the Lock-Free char-ARTrie Overlay

**Repo:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein` · 2026-06-02 · implementation-ready. Prerequisite the
owner chose before the F0-F5 lock-free flip. Reversible, gated, **ZERO new unsafe**. The decisive deliverable is
the **LOOM/proptest/TLA RE-PROOF** that the composite (insert ∪ remove) stays linearizable once finality is no
longer monotone. Persisted from the Plan-agent design (full prose in the session transcript).

## (1) Feasibility + the monotone-final break (code-cited)
**FEASIBLE with the single-root-CAS arbiter (`atomic_ptr.rs:131`, loser-safe per `Arc::ptr_eq:143`) — but ONLY
after a re-proof.** The arbiter is operation-agnostic (serializes published root versions); a remove that
path-copies the spine with a clear-finality leaf then root-CAS-publishes is structurally identical to
`insert_cas_durable`. The catch: the *content invariant* the proofs rely on — finality monotone (0→1 only) — is
broken by delete (1→0). Where the proofs rely on it:
- **Node API:** `try_set_final`=`fetch_or(IS_FINAL)` (`overlay/node.rs:724`), `as_final` ORs (`:809`); no
  `as_non_final` exists.
- **Prefix-insert fix:** `build_path_recursive` `depth==len` returns the SHARED existing Arc so `try_set_final`'s
  `fetch_or` is the single arbiter (`lockfree_cas.rs:345-365`, comment "membership only ever goes 0→1"). A
  concurrent remove clearing that node breaks the "0→1 only" reasoning.
- **Loom** (`tests/persistent_lockfree_overlay_loom.rs`): no clear-finality action; assertions are insert-only
  theorems.
- **Proptest** (`tests/persistent_lockfree_overlay_proptest.rs`): `BTreeSet` insert-only oracle; `Op` = Insert/Contains.
- **⚠ POSITIVE CACHE (DATA-CORRECTNESS BUG SURFACE):** `contains_lockfree` checks `lockfree_cache.contains_key`
  FIRST and short-circuits `true` (`lockfree_cas.rs:516-520`); `insert_cas_durable` does
  `lockfree_cache.insert(term,true)` (`:259`). It's insert-only-positive → **a remove that clears the trie but
  leaves the cache entry makes the term read present FOREVER.** Remove MUST invalidate the cache (§3.4).
- **TLA `OverlayEvictionCas`:** `acked` only grows; no removed-set. Needs a `RemoveCas` action + `removed` set.
**Conclusion:** R-B is feasible; the code is small (one node method + one durable method + router branch + cache
invalidation). **The real work — and the GATE — is the re-proof** (§4). Delete IS linearizable under the
single-root-CAS arbiter; the models simply don't yet witness it, and two (NoLostAck, the positive cache) would be
UNSOUND if delete were wired without §3.4/§4.

## (2) Node primitive `as_non_final`
Add to `impl<K: KeyEncoding, V: Clone> OverlayNode<K,V>` (mirror of `as_final`, `overlay/node.rs:809`):
```rust
pub fn as_non_final(&self) -> Self {
    Self {
        version: AtomicU64::new(self.version.load(Acquire) + 1),
        store: self.store.clone(),                                   // SUBTREE RETAINED (remove "cat" keeps "cats")
        flags: AtomicU8::new(self.flags.load(Acquire) & !(flags::IS_FINAL | flags::HAS_VALUE)),
        value: None,                                                 // drop the value (mirror owned remove)
        prefix: self.prefix.clone(), prefix_len: self.prefix_len,
    }
}
```
Clears `IS_FINAL`+`HAS_VALUE` on a COPY (immutability preserved), retains children/prefix (compaction = future opt,
out of scope — matches owned remove which also leaves the node). No `without_value` (folded in). ZERO unsafe
(same shape as `as_final`; Send/Sync unaffected). Node unit tests: clear→not-final/no-value/children-preserved/
original-unchanged; as_non_final∘as_final round-trip; deep-child retention ("cat" cleared keeps "cats" final);
both ByteKey+CharKey.

## (3) `remove_cas_durable` — Order-A mirror of insert_cas_durable
Add to `impl<V: DictionaryValue, S: BlockStorage> PersistentARTrieChar<V,S>` (after `insert_cas_durable:207`):
`pub fn remove_cas_durable(&self, term: &str) -> Result<bool>` — Ok(true) iff it cleared a present term, Ok(false)
if absent. Steps:
1. Policy guard (reject Periodic/None, verbatim from insert).
2. enable_lockfree guard.
3. **Absent fast-path + WAL avoidance (key divergence):** walk the overlay (via `find_leaf_faulting` for OnDisk
   prefixes); if not present → `Ok(false)` with **NO WAL** (matches owned `preflight_remove_no_wal`; a no-op remove
   mustn't burn an LSN/punch a watermark hole). NOTE: the positive cache is NOT a sufficient presence oracle for
   remove (cache-miss ≠ trie-absent after a recovery rebuild) → consult the TRIE, not just the cache.
4. **ORDER-A step 1:** `append_to_wal_returning_lsn(WalRecord::Remove{term})` append+sync DURABLE (same chokepoint
   → `invalidate_eviction_registry`); one append, never re-logged on retry.
5. **Step 2: visibility CAS loop** (`enter_read()` pinned): `try_remove_lockfree_path`:
   - `Removed` → **invalidate cache (`lockfree_cache.remove(term)`)** → `mark_committed(lsn)` → Ok(true).
   - `AlreadyAbsent` (raced/cleared) → invalidate cache → mark_committed(lsn) → Ok(false) (LSN durable, watermark
     mustn't stall — same as insert's AlreadyExists arm).
   - `Conflict` → `cas_retries+=1`, re-find on retry.
   - `IoError(e)` (gated) → Remove already durable; return Err, do NOT advance watermark (contiguous prefix stalls;
     recovery replays). Order-A "durable-but-visible-after-reopen" window, identical to insert.
Helpers: `try_remove_lockfree_path` + `build_remove_path_recursive` (dual of build_path_recursive): at
`depth==len`, if `!is_final` → `Err(AlreadyAbsent)` (don't publish a no-op spine); if final → `node.as_non_final()`
as a **FRESH** cleared leaf in a NEW spine (root CAS = sole arbiter; NOT shared-Arc like insert). Descend: InMem→
recurse+splice; OnDisk (gated)→fault-in first (`load_overlay_node_from_disk`) then recurse; None/null→AlreadyAbsent.
Reuse `BuildPathError` + add `AlreadyAbsent`.

**§3.4 CACHE INVALIDATION (DATA-CORRECTNESS — do not omit):** `remove_cas_durable` MUST `lockfree_cache.remove(term)`
on every state-changing arm (Removed + AlreadyAbsent), BEFORE `mark_committed`, else removed terms read present
forever via the positive cache. Asserted by the proptest `Contains` check + a remove‖contains loom schedule.

**§3.5 Why FRESH-COPY-via-root-CAS, NOT in-place `fetch_and`:** insert's `fetch_or` is in-place-safe BECAUSE
finality is monotone (an early observer of 0→1 is benign). An in-place `fetch_and(!IS_FINAL)` could race an in-place
`fetch_or` on the same shared node with no serialization → resurrection/lost-update. By publishing a fresh cleared
node version ONLY via the root CAS, the clear is atomic with a specific published root and the CAS arbiter
linearizes it. **The node's `flags` is only ever flipped in-place 0→1 by try_set_final (still monotone in-place);
the 1→0 happens only on a fresh copy via as_non_final, arbitrated by the root CAS.** The §4.4 TLA negative control
proves this choice is required.

## (4) THE RE-PROOF — the decisive deliverable (the GATE)
**R-B is NOT done until loom + remove-aware proptest + TLA (with a firing negative control) are all green** (inside
`verify-formal-correspondence.sh` + nextest ≥2489). Reviewer must NOT approve on the code diff alone.
**Theorem (composite linearizability, no-lost-op):** for any concurrent history over {insert,remove,contains},
published-root membership = a last-writer-wins linearization respecting the root-CAS real-time order; no op lost;
no resurrection (a removed term reappears only via a later insert); no double-clear UAF. Monotonicity is DROPPED,
replaced by **last-writer-wins under the root-CAS total order**.
- **§4.2 Loom** (`tests/persistent_lockfree_overlay_loom.rs`): add `ModelNode::as_non_final_clone` + `remove_one_char`
  (fresh cleared copy + root-CAS, NOT in-place) + 5 schedules: (1) remove‖insert same key → one wins, consistent
  last-writer, no lost op; (2) **remove‖prefix-finalize → "ab" always preserved, prefix-insert fix STILL holds**
  (the decisive schedule); (3) remove‖remove → one true/one false, idempotent, no double-clear UAF; (4) remove
  through faulted-in prefix (reuse OE9 machinery); (5) reader-snapshot survives concurrent remove (no-UAF).
- **§4.3 Proptest:** add `Op::Remove`; keep `BTreeSet` oracle for `V=()` (set insert/remove = LWW) mutated by both;
  `Contains` assertion catches the stale-cache bug; multi-thread insert/remove convergence via a deterministic
  quiescent settling phase (remove-all-then-insert-known-subset, assert exact final membership); ADD a `V=u64`
  `BTreeMap` oracle test (remove drops value → `get_value`==None not Some(0)).
- **§4.4 TLA:** add `LockFreeOverlayRemoveCas.tla` (+`.cfg`+`_Unsafe.cfg`) — vars `root`(MaxRoot-capped),`present`,
  `acked`,`removed`; actions `InsertCas`/`RemoveCas`/`RemoveAbsentNoop`; invariant `LastWriterWins == \A t:
  (t∈present) <=> (t∉removed)` + `NoResurrection` + `NoLostOp`. **Negative control `_Unsafe.cfg`:**
  `USE_FRESH_COPY_CLEAR=FALSE` models the rejected in-place clear (clear `present` WITHOUT bumping root) → TLC MUST
  violate `LastWriterWins` (resurrection/lost-remove). Register in `verify-formal-correspondence.sh` SANY/RUN_TLC/
  negative-control lists (`:251`/`:269`/`:323`). Bounded: 2 Terms, MaxRoot≈6.
- **§4.5 Why the prefix-insert fix survives:** insert's path is UNCHANGED (still shares the node + `fetch_or`);
  remove never flips an existing node's bit, it swaps in a fresh node version via root-CAS. The two never write the
  same atomic; the root-CAS total order resolves them. Loom schedule #2 is the machine-checked witness.

## (5) Recovery — UNCHANGED under REC-A
Owned WAL-replay already handles `Remove` (`recovery.rs:187`, `mmap_ctor.rs:420`) — same `WalRecord::Remove`, no
codec change. Under REC-A (rebuild overlay from `iter()` live set), a removed term is non-final in the recovered
owned tree → **naturally absent** from the rebuilt overlay → recovery correct with ZERO new recovery code (strengthens
the flip's §6 assertion). Overlay-as-source recovery (REC-B follow-on): tail `Remove` replay calls a
`remove_cas_no_wal` (CAS loop minus the WAL append) — specified, lands with REC-B. Order-A: a durably-acked remove
survives crash (replay clears it); the IoError window is durable-but-not-yet-visible → replay honors it (correct).

## (6) Production routing (supersedes R-A)
`remove`/`SharedCharARTrie::remove` (`mutation_api.rs:66`) gains `if self.route_overlay() { return
self.remove_cas_durable(term); }` — replaces R-A's error/fallback. `&self`-compatible (Phase-F-ready). PS3 flips
from "remove errors" to "remove WORKS + absent after reopen"; PS1 production soak gains mixed insert/remove.
Negative-delta `increment` STAYS the separate gap (R-B solves remove only). **Gate ordering dependency: RB6
(production remove routing) lands only AFTER the flip's "un-gate fault-in to production" (flip F0), because
remove-under-evicted-prefix needs fault-in** — flag to owner.

## (7) Phased migration (each green-gated: nextest ≥2489 + verify-formal-correspondence exit 0 + unsafe-inventory exit 0; systemd 32G + real-disk)
- **RB0** — `as_non_final` + node unit tests (both keys). No callers. Rollback: delete.
- **RB1** — `remove_cas_durable` + path helpers + `BuildPathError::AlreadyAbsent` + cache invalidation + reject test
  + single-thread durable round-trip. Behind enable_lockfree, not routed. Rollback: delete.
- **RB2 — RE-PROOF loom** (5 schedules). HARD GATE — if any fails, the design is wrong, stop.
- **RB3 — RE-PROOF proptest** (remove-aware oracle + multi-thread convergence + `V=u64` BTreeMap value test). The
  stale-cache `Contains` assertion is load-bearing.
- **RB4 — RE-PROOF TLA** (`LockFreeOverlayRemoveCas.tla` + negative control fires). **R-B's re-proof complete only
  when RB2+RB3+RB4 all green.**
- **RB5** — durable mixed insert/remove soak (analogue of `concurrent_durable_writers_all_survive_reopen`): reopen →
  live set == acknowledged net membership.
- **RB6** — production routing (supersedes R-A); merges into the flip's F0; requires fault-in un-gated first.
RB0-RB5 pure additions (reversible by deletion); RB6 reversible via the kill-switch until the flip's F6. NO new
irreversible step, NO on-disk format change (reuses `WalRecord::Remove`).

## (8) Honest risks
1. **Lost-remove/lost-insert race (DATA-CORRECTNESS — the re-proof's core):** LWW under the root-CAS total order;
   loom #1 + TLA `LastWriterWins` check it. Re-proof IS the work.
2. **Resurrection via stale positive cache (DATA-CORRECTNESS, concrete):** §3.4 mandates `lockfree_cache.remove`;
   proptest `Contains` + remove‖contains loom witness it. The single most likely implementation slip.
3. **Cache‖CAS ordering window:** clear the cache as the FIRST action in the Removed/AlreadyAbsent arms (before
   mark_committed); linearization point is the root CAS; oracle checks against root-CAS membership.
4. **Fault-in‖remove:** remove-under-evicted-prefix faults in first (gated until flip F0); IoError → durable-not-
   visible, recovered on reopen; loom #4 witnesses; **RB6 depends on flip-F0 fault-in un-gate.**
5. **V=u64 remove vs zero:** as_non_final drops the value → `get_value`==None not Some(0); BTreeMap proptest guards.
6. **Double-clear UAF:** loser drops its Arc (refcount); loom #3/#5 + ReachableNotFreed; low residual.
7. **AlreadyAbsent vs Conflict retry (liveness):** bounded retry-to-absent, terminates; mirrors insert.
8. **The re-proof is the real work (process):** ~150 LOC mirror code; the loom/proptest/TLA + negative control are
   the effort + risk. Gate is explicit (§4).
**Final:** delete IS linearizable with the single-root-CAS arbiter; the fresh-copy-published-via-root-CAS choice
(NOT in-place fetch_and) is load-bearing and justified by the §4.4 negative control. R-B feasible + recommended,
gated on the re-proof.

### Critical files
- `src/persistent_artrie_core/overlay/node.rs` (add `as_non_final` after `:809`; node tests `:895+`)
- `src/persistent_artrie_char/lockfree_cas.rs` (add `remove_cas_durable` after `:207`; `build_remove_path_recursive`
  mirror of `:331`; reuse `find_leaf_faulting:681` + `load_overlay_node_from_disk`; cache invalidation; reject test)
- `tests/persistent_lockfree_overlay_loom.rs` (5 remove schedules + `as_non_final_clone`; reuse prefix fixture
  `:200` + OE9 machinery `:262`)
- `tests/persistent_lockfree_overlay_proptest.rs` (remove-aware `BTreeSet` oracle + `Op::Remove` + multi-thread +
  `V=u64` BTreeMap)
- `src/persistent_artrie_char/mutation_api.rs` (`remove:66` gains `route_overlay()`→`remove_cas_durable`)
- NEW `formal-verification/tla+/LockFreeOverlayRemoveCas.{tla,cfg}` + `_Unsafe.cfg`; register in
  `scripts/verify-formal-correspondence.sh:251/269/323`. Recovery (`recovery.rs:187`, `mmap_ctor.rs:420`) NO change
  under REC-A.
