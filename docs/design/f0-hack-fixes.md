# Design: Principled replacements for the four F0 hacks/gaps

**Repo:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein` · 2026-06-02 · spec for FOREGROUND implementation.
Resolves: `batch_insert.rs` (single-op fan-out), `document_tx.rs` (`commit_document` errors),
`atomic_ops.rs::compare_and_swap` (errors), `atomic_ops.rs::get_or_insert` (invented route). Targets:
CommitRank-correct, Order-A, `NoLostWriteUnderLockFreeCommit`, registry-invalidation, ZERO new unsafe, WAL
back-compat (additive/versioned only), reversible/green-gated. Persisted from the Plan-agent design.

## (0) Load-bearing code facts
- **F-1:** `reconcile_lww` keys ONLY on `data_lsn` and stamps every op fanned from one record with the SAME
  `(generation, lsn)` (`recovery.rs:259-298`, `rank.insert(*data_lsn,*generation)` :269, `generation_of` :273,
  fan at :287-288, sort :296). ⇒ **one `BatchInsert` at one LSN CANNOT carry N distinct per-entry generations.**
- **F-2:** the commit generation is the published-ROOT version (per-CAS), NOT the leaf version
  (`lockfree_cas.rs:346,363` `Inserted(node, root_generation)`; remove `:532,540`; valued `:1657,1735`
  `new_root.version()`). OD4 comment `:353-362`: leaf `try_set_final` is in-place + does NOT bump leaf version
  ⇒ root version is the strictly-monotone-per-publication source. **The order-a doc §3.6 prose says "leaf
  version"; the CODE uses root version and is correct — follow the CODE.**
- **F-3:** increments are CommitRank-FREE by design (`try_increment_cas_durable:1535-1553` logs single-entry
  `BatchIncrement`, NO `append_commit_rank`; deltas commutative, recovery SUMS them, `recovery.rs:347-355`).
- **F-4:** `reconcile_lww` applies ALL ops in `(gen,lsn)` order (not a per-term collapse) — each op needs its
  own generation.

## (1) Q1 — batched / document-tx overlay writes under CommitRank
### 1.1 Verdict (Q1-A, RECOMMENDED): overlay batch = N per-op `*_cas_durable` records (NOT one BatchInsert)
§2.6's "one BatchInsert append + N CAS" was written BEFORE CommitRank and is now **incorrect for the overlay**
(F-1: one data_lsn → one generation; replaying a batch would stamp all N entries with one generation → the s019
class the CommitRank fix defeats). The principled overlay batch is **N independent Order-A per-op records, each
`Insert | Insert{value} | BatchIncrement-delta` + its own CommitRank** = exactly N `insert_cas_durable` /
`insert_cas_with_value_durable` calls. This is the CommitRank-correct realization of §2.6, not a regression. The
single-`BatchInsert` append stays ONLY on the owned arm. **Zero codec / reconcile / TLA change** (N more of the
same Insert+rank ops, in the existing `LockFreeOverlayDurableReplay` envelope).
**Order-A is per-op** (each entry durable-before-visible, `mark_committed` after its CAS); there is NO batch-level
atomicity claim (the owned `insert_batch` is also non-atomic across entries on crash — replays the durable prefix).
**Partial-batch/error (replaces the `log::warn!` swallow):** count `Ok(true)`; `Ok(false)` no-ops; on the FIRST
`Err` (fault-in `IoError` window) STOP and return the count committed so far (the failed entry's data record is
durable, replays on reopen). Document it.
**LATENT GAP (not in the original list): `insert_batch_bytes` (`batch_insert.rs:148`) has NO overlay routing** —
under the F5 default it silently writes the owned tree (the bug `commit_document` correctly rejects). MUST add the
route.
**Variants:** `_chars`/`_sorted`/`_grouped`/`_arena_grouped` delegate to `insert_batch`/`insert_batch_bytes` (the
sort is owned-arena locality, semantically inert; harmless to keep) ⇒ routing `insert_batch` + `insert_batch_bytes`
covers all. Q1-B (one BatchInsert + `(data_lsn,term)`-keyed reconcile) is the documented escape hatch only if
huge-batch WAL append-count is ever measured as a bottleneck (costs a reconcile re-proof; overlay gets no arena-
locality benefit so not worth it now).

### 1.2 document-tx: ship tx-ii in F0; tx-i as gated follow-on
**Recovery hazard (traced, the subtle part):** `reconcile_lww` (the PRODUCTION open path, `mmap_ctor` →
`recovery.rs:253-298`) has NO tx bracketing — it expands BeginTx/CommitTx/AbortTx → `vec![]` and applies all data
ops in `(gen,lsn)` order. So a partial (uncommitted) tx's already-durable per-op records WOULD be replayed.
Therefore **tx-iii (keep BeginTx/CommitTx on the overlay arm WITHOUT fixing the reconcile) is UNSOUND** (resurrects
partial state) — do NOT ship it (it's the trap a naive "same WAL records, overlay apply" read of §2.7 produces).
- **tx-ii (F0 landing, RECOMMENDED):** overlay `commit_document` applies SETs via `insert_cas*_durable` (each
  Insert+CommitRank, Order-A) and increments via `try_increment_cas_durable` (BatchIncrement-delta, NO rank, F-3,
  commutative); **DROP BeginTx/CommitTx/sync_wal on the overlay arm** (skip the orphan BeginTx in `begin_document`
  under `route_overlay()`). Semantics = `insert_batch` + increments: **per-op durable + per-op visible, NOT
  all-or-nothing crash-atomic.** This MATCHES the owned path's actual semantics (the owned tx is crash-atomic but
  NOT isolation-atomic; and the reconcile ignores its bracket anyway). Document as a named residual: "under the
  overlay, `commit_document` is per-op durable, not all-or-nothing; callers needing all-or-nothing use
  `OverlayWriteMode::OwnedTree`." If any aggregated increment delta `<0` → reject the whole commit (documented
  negative-delta gap), NOT a silent owned write.
- **tx-i (follow-on, gated):** add a `pending_tx_ops` bracket to `reconcile_lww` (match the `redo_phase` that
  already drops uncommitted ops — removes a latent inconsistency between the two recovery paths) + extend
  `LockFreeOverlayDurableReplay.tla` with BeginTx/CommitTx + a `NoUncommittedTxReplay` negative control. Then the
  overlay tx can be all-or-nothing crash-atomic. Touches the proven reconcile ⇒ its own re-proof.

## (2) Q2 — compare_and_swap + get_or_insert
### 2.1 compare_and_swap → R-A (documented gap) for F0; RC-B (proven primitive) gated follow-on
A value-level CAS IS feasible on the root-CAS arbiter (read leaf value under the pin, match `expected`, build
new-value leaf, root-CAS, re-read on retry) — BUT **the append-before-failed-CAS hole** makes it expensive: Order-A
forces the data record durable BEFORE the publish CAS; if the CAS then loses and the re-evaluated match fails, we
return `Ok(false)` but the durable WAL already holds a `CompareAndSwap{success:true}` record that recovery replays
as an overwrite → **phantom write / lost update** (an s019-class, value-dependent, non-monotone hazard). Resolving
it needs its OWN TLA negative control (`LockFreeOverlayValueCas.tla`, `NoPhantomCasWrite`, the append-before-failed-
CAS trace) + loom (CAS‖{CAS,upsert,remove}) + proptest `Op::Cas` BTreeMap oracle. **So `compare_and_swap` is NOT a
thin upsert variant.** F0: keep the `route_overlay()` error, align wording with the documented gaps + PS3-guard +
GAP_LEDGER entry; add `compare_and_swap_cas_durable`+re-proof as RC-B, gating F5 ONLY if a u64 caller needs overlay
CAS.

### 2.2 get_or_insert → replace the racy route with atomic `get_or_insert_cas_durable` (CHEAP, fix in F0)
**The current route (`lockfree_value_route.rs:70-95`) has a REAL TOCTOU:** `insert_cas_with_value_durable` (no-op
if present) then a SEPARATE `get_lockfree` read-back. (1) a concurrent `remove_cas_durable` between the two clears
the term → `get_lockfree` returns None → `.unwrap_or(default)` returns the default while the term is ABSENT (the
return value lies); (2) `get_lockfree`→`find_leaf_lockfree` does NOT fault-in (`:1091` `as_in_mem()?`), so a term
under an evicted prefix read-backs as None → wrong value. Two linearization points with a window = not atomic.
**Fix: `get_or_insert_cas_durable(&self, term, default: u64) -> Result<u64>`** — ONE CAS-retry loop under ONE epoch
pin: load root; `find_leaf_faulting` (fault-in!); if final → return `leaf.get_value().unwrap_or(default)` (RESIDENT,
NO WAL); else append `Insert{default}` (Order-A), `build_value_path_recursive`, `generation=new_root.version()`,
root-CAS → on Ok: cache + `append_commit_rank` + `mark_committed`(both) + return default; on Err: retry (re-read,
may now be resident). Single linearization point (root-CAS insert arm OR leaf-load resident arm, both under the
pin). **Cheap re-proof (NO new TLA negative control — in the Insert+rank / published-read envelope):** proptest
`Op::GetOrInsert(default)` vs `BTreeMap` oracle (`entry().or_insert`) + a `V=u64` value test (resident arm returns
resident value, not default); loom `get_or_insert‖remove` (no phantom — value consistent with root-CAS LWW) +
`get_or_insert‖get_or_insert` (same value, exactly one inserts). Rewrite `route_get_or_insert` to call it
(SAFE-`Any` u64 dispatch + the existing u64→V re-wrap); delete the two-step.

## (3) Per-fix implementation spec
- **Fix A (batch):** new `insert_batch_cas_durable(&self, terms:&[&str])->Result<usize>` (membership, loops
  `insert_cas_durable`, count+first-Err-stops); new `route_insert_batch<V,S>(trie, entries)->Option<Result<usize>>`
  in `lockfree_value_route.rs` (all-None→membership; else u64-downcast loop `insert_cas_with_value_durable`/
  `insert_cas_durable`; None if V≠u64 & any Some). Wire `insert_batch:18` (replace `:28-42`) + **add routing to
  `insert_batch_bytes:148`** (currently missing). `_chars/_sorted/_grouped` inherit. Update the module doc.
- **Fix B (document-tx tx-ii):** replace the `:321-329` blanket error; validate state; reject negative aggregated
  increment; apply SETs via `route_insert_with_value`/`insert_cas_durable`, increments via
  `try_increment_cas_durable`; NO BeginTx/CommitTx on the overlay arm; skip orphan BeginTx in `begin_document`
  under `route_overlay()`; `abort_document` unchanged. Reuse the existing preflight/aggregate/overflow logic
  (`:362-425`), route the APPLY to overlay primitives. Document the per-op (not atomic) semantics.
- **Fix C (compare_and_swap R-A):** keep `:197-203` error; align wording + PS3 + GAP_LEDGER; reference RC-B.
- **Fix D (get_or_insert):** new `get_or_insert_cas_durable` (§2.2); rewrite `route_get_or_insert:70-95`; owned
  `false`-arm verbatim.

## (4) Verification (per fix)
- **A:** deterministic `batch_overlay_replay_orders_by_commit_rank` regression (FAILS if reverted to one
  BatchInsert); extend the mixed soak with batch inserts (≥50× green, Immediate+GroupCommit); gate-by
  `persistent_bulk_mutation_correspondence`, `persistent_lockfree_overlay_proptest`,
  `recovery_replay_completeness_correspondence`, the existing `LockFreeOverlayDurableReplay` TLA (+ `_Unsafe` still
  fires). NO new TLA.
- **B (tx-ii):** reuse `persistent_transaction_increment_correspondence` + `PersistentTransactionIncrementRecovery.tla`;
  new `overlay_commit_document_per_op_durable_reopen` + a crash-mid-tx test asserting the DOCUMENTED per-op (not
  all-or-nothing) semantics; PS3: commit_document APPLIES (was errors), negative-delta still errors. (tx-i
  follow-on: the reconcile-bracket TLA negative control.)
- **C:** PS3 asserts the documented error. (RC-B follow-on: `LockFreeOverlayValueCas.tla` + loom + proptest.)
- **D:** proptest `Op::GetOrInsert` + loom (`‖remove`, `‖get_or_insert`); gate-by `LockFreeARTrieLinearizability` +
  `LockFreeOverlayDurableReplay` stay green. NO new TLA negative control.
- **Global per phase:** nextest ≥2534 + `verify-formal-correspondence.sh` (RUN_TLC=1) exit 0 +
  `verify-unsafe-boundary-inventory.sh` exit 0 + 0 new unsafe; systemd 32G + real-disk for soaks.

## (5) Phased reversible migration (no phase flips the default; that's F5)
- **H0 — Fix D** (`get_or_insert_cas_durable` + rewrite route + loom + proptest). FIRST (fixes a live TOCTOU race).
  Rollback: revert route to the two-step; delete the primitive.
- **H1 — Fix A** (`insert_batch_cas_durable` + `route_insert_batch`; wire `insert_batch` + `insert_batch_bytes`;
  regression + batch soak). Rollback: revert routing blocks; delete primitive. No codec/reconcile/TLA change.
- **H2 — Fix B tx-ii** (overlay commit_document per-op; drop BeginTx/CommitTx overlay arm; PS3 + regression).
  Rollback: restore the blanket error.
- **H3 — Fix C alignment** (keep error; wording + PS3 + GAP_LEDGER). Rollback: trivial (text).
- **Follow-ons (separately gated, NOT F0):** RC-B (compare_and_swap_cas_durable + LockFreeOverlayValueCas.tla),
  tx-i (reconcile tx-bracket + TLA), Q1-B (one-BatchInsert + (data_lsn,term) reconcile).
Each phase independently revertible; H0 first (live race), H1/H2 independent (reuse proven primitives), H3 text.

## (6) Honest risks
1. **document-tx atomicity downgrade (tx-ii):** overlay commit_document loses all-or-nothing crash-atomicity vs
   owned (becomes per-op). Sound because the reconcile already ignores the bracket (tx-iii would be unsound); a
   named residual; tx-i gates callers needing overlay all-or-nothing.
2. **`insert_batch_bytes` had NO overlay routing (latent gap):** under F5 it silently writes owned. Fix A closes
   it; if A descoped, it must AT LEAST get a guard.
3. **compare_and_swap is genuinely expensive (RC-B), not thin:** the append-before-failed-CAS phantom-write hole
   needs its own TLA negative control. R-A (error) in F0 is correct; don't wave it through as an upsert variant.
4. **get_or_insert resident arm depends on `find_leaf_faulting` (F0 un-gated fault-in):** gated by g5 fault-in
   tests + the new get_or_insert loom.
5. **GENERATION = ROOT version, NOT leaf version** (doc §3.6 prose vs code): use `new_root.version()`/
   `root_generation`, or the s019 re-finalize tie reappears. **The single most likely implementation slip — call
   out in review.**
6. **Q1-A spends more WAL append count than one BatchInsert:** overlay gets no arena-locality benefit; GroupCommit
   coalesces the ranks; measure with `benches/batch_wal_benchmarks.rs`; Q1-B is the escape hatch.
7. **Two recovery paths disagree on tx semantics today** (`reconcile_lww` ignores brackets, `redo_phase` honors
   them): pre-existing; tx-i converges them.
8. **ZERO new unsafe** (SAFE-`Any` dispatch, immutable path-copy, arc-swap, Arc) — inventory stays exit-0.

### Critical files
- `src/persistent_artrie_char/lockfree_cas.rs` (add `insert_batch_cas_durable`, `get_or_insert_cas_durable`; mirror
  `insert_cas_durable:291`/`insert_cas_with_value_durable:1576`/`upsert_cas_durable:1700`/
  `try_increment_cas_durable:1535`; root-generation `:346/:1657`; regression home `:2213+`)
- `src/persistent_artrie_char/lockfree_value_route.rs` (add `route_insert_batch`; rewrite `route_get_or_insert:70-95`)
- `src/persistent_artrie_char/batch_insert.rs` (`insert_batch:18` block `:28-42`; **add routing to
  `insert_batch_bytes:148`**); `document_tx.rs` (`commit_document:309` arm `:321-329`; `begin_document:40`);
  `atomic_ops.rs` (`compare_and_swap:184`, `get_or_insert:256`)
- `src/persistent_artrie_core/recovery.rs` (`reconcile_lww:253-298` keys on data_lsn `:269`; tx-i bracket point;
  `recovered_operations_from_record:305` arms `:343/:370`); `wal/codec.rs` (confirm NO change for Q1-A)
- `tests/persistent_lockfree_overlay_loom.rs` + `tests/persistent_lockfree_overlay_proptest.rs` +
  `scripts/verify-formal-correspondence.sh` (SANY :252 / TLC :305 / negative-control :346; `LockFreeOverlayDurableReplay.tla`
  is the home for tx-i/RC-B extensions)
