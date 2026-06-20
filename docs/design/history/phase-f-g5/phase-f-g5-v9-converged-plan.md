# Phase F + G5 — V9 converged plan (post red-team round 1)

**Repo:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein` · 2026-06-06 · supersedes V8
(`tool-results/toolu_01GKFczksRv4QueaqK35mXKS.json`) per the owner's
`plan → red-team → refine → repeat-until-converge → final-red-team` instruction.

This document records **red-team round 1** (4 adversarial agents) and the **V8→V9 refinements** that
answer every finding. The headline change: V8's carve-out **category 2 ("multi-key fold into ONE
`BatchInsert` + ONE generation")** is **REFUTED** and replaced by the project's own already-converged
design (`docs/design/f0-hack-fixes.md`, 2026-06-02) — **N per-op Order-A records reusing proven
primitives** — plus a minimal new piece for `merge_*` (**a per-key CAS-retry-loop over the existing,
already-phantom-safe `compare_and_swap_cas_durable` primitive**).

---

## 0. Red-team round 1 — verdicts (verified against live code)

| # | Target | Verdict | Severity |
|---|---|---|---|
| A1 | increment trait removal (cat 1) | **SAFE** — zero generic/`dyn`/Shared/downstream callers of `.increment()`; `ARTrie` is intentionally not object-safe (`artrie_trait.rs:717`); `fetch_add` is inherent-only; sibling uses only `ARTrie::create` | framing fixes only |
| A2 | multi-key overlay fold (cat 2) | **REFUTED — 3 BLOCKERS** — `BatchInsert` can't encode removals/increments (resurrects deleted terms on reopen); one generation for N terms is outside the TLA envelope (GAP_LEDGER #88; `CommitRank` carries ONE term); merge-via-Upsert drops concurrent writes. **Contradicts `f0-hack-fixes.md` §1.1.** | BLOCKER |
| A3 | F4 lock collapse (deadlock + audit) | **SOUND** — graph `CK→OR`, `CK→EC`, `OR→EC`, EC leaf = DAG, acyclic; CK at exactly 2 sites, no reentrancy; 8-field audit COMPLETE (per-variant **union**: byte=5, char=7) | 2 gaps (below) |
| A4 | F2 activation + triage | **PARTLY REFUTED** — real feature-on failures = **124, not 246**; **≥24 orphans** outside the 5 buckets, incl. a real semantic bug (`insert_with_value` insert-once vs upsert); `get()` confirmed trait-free | BLOCKER (orphans) |

Full agent transcripts: this session's tool results (agents `a4b54fb06…`, `a6b9ac561…`, `a8bab96b4…`,
`a1aa89e89…`). Empirical artifacts under `docs/benchmarks/redteam-f2-*.txt`.

### 0.1 The load-bearing discovery
The working tree (indexed 2026-06-06) is **far ahead of `f0-hack-fixes.md` (2026-06-02)**. Already
present and proven:
- `compare_and_swap_cas_durable_default` (`overlay/durable_write.rs:461-529`) — **and its
  append-before-failed-CAS phantom hole is ALREADY closed**: mismatch ⇒ `Ok(false)` no WAL
  (`:495-498`); match ⇒ append `Upsert{new}` durable ⇒ publish with per-iteration `expected`-recheck
  ⇒ **on recheck-miss `mark_committed_burned(lsn)` (NEVER ranks)** (`:524-527`). Unranked records are
  **dropped on Overlay reopen** (`recovery.rs:332` `RankRegime::Overlay => continue`, per-segment at
  `:265-284`). **"burn = unranked = dropped" is the mechanism that makes conditional/recomputed value
  writes phantom-safe.**
- `get_or_insert_durable_default` (`durable_write.rs:537`) — atomic read-your-write.
- `remove_cas_durable`, `upsert_cas_durable_default`, `insert_cas_durable`,
  `insert_cas_with_value_durable_default`, `try_increment_cas_durable` — all proven primitives.
- TLA models: `LockFreeOverlayDurableReplay.tla`(+`_Unsafe`), `LockFreeOverlayRemoveCas.tla`(+`_Unsafe`),
  `OverlayEvictionCas.tla`(+`_Unsafe`), `LockFreeCounterMergeAtomicity.tla`, `LockFreeIndexedOverlay*`.

⇒ **C2 is mostly "wire the already-built primitives into the rejecting call sites," not "design a new
batch protocol."**

---

## 1. Carve-out category 1 — `increment`/`fetch_add` compile-time specialization (A1: SAFE)

**Refinements from A1:**
- The inherent `increment`/`fetch_add` ALREADY exist on the generic `impl<V: DictionaryValue + Serialize
  + DeserializeOwned, S>` blocks (char `atomic_ops.rs:41`/`:274`; byte `:35`/`increment_bytes:53`/`:321`).
- To achieve the owner's **compile-time** specialization (so `PersistentARTrieChar::<String>::increment`
  is a compile error, not a runtime reject), bound the inherent methods to a **sealed `Counter` marker
  trait** (impl'd only for the counter value types), i.e. move them to `impl<V: Counter, S> …`. This is
  cleaner than a single monomorph because **callers use multiple counter types** (A1: char counter tests
  use `PersistentARTrieChar<i64>`; byte uses `PersistentARTrie<i64>`; the lock-free seam is `u64`
  internally). `Counter` must cover **both `i64` and `u64`** (verify the exact set during impl).
- Edits: comment-out `fn increment` in the `ARTrie` trait (`artrie_trait.rs:532`, per "never delete to
  disable"); comment-out the 3 trait-impl delegations (byte `shared_trait_impl.rs:232`, char
  `mod.rs:1463`, vocab reject `mod.rs:708`); add the `Counter` bound to the inherent blocks. **Keep the
  `ARTrie` trait itself** (sibling depends on `ARTrie::create`).
- Honesty: this **drops the (verified-unused) `.increment()` from the `Shared*` handles** — state that,
  don't claim "no loss."
- New test: `compile_fail` doc-test that `PersistentARTrieChar::<String>::increment` does not exist.

**Reversibility:** signature-reversible; coverage-lossy (retyped increment tests). MINOR.

---

## 2. Carve-out category 2 — doc-tx / batch / merge / CAS / get_or_insert (A2 REFUTED → f0-hack-fixes)

**Replace V8's fold entirely.** The converged design = `f0-hack-fixes.md` (N per-op Order-A records,
reusing proven primitives) + the `insert_with_value` upsert fix + a minimal merge primitive. **No
`BatchInsert` fold, no batch-rank codec variant, no "one generation for N", zero on-disk/codec change.**

### 2.0 `insert_with_value` upsert bug (A4 BLOCKER) — fix FIRST
Owned `insert_with_value` **overwrites** on duplicate (`mutation_core.rs:151-154`: already-final ⇒
`node.value = Some(value); Ok(false)`), matching `upsert`, the map laws (`dictionary_law_correspondence`),
and `test_value_update_persistence`. The overlay routes to `insert_cas_with_value_durable_default`
(**insert-once**) in BOTH char (`mutation_api.rs:72`) and byte (`mutation_api.rs:62`) ⇒ stale values via
`get_value()` ⇒ owned↔overlay divergence. **Fix: route overlay `insert_with_value` →
`upsert_cas_durable_default`** (overwrite), both variants. Correct the dedicated test
(`persistent_arbitrary_v_overlay.rs:110-119`) which currently asserts the buggy insert-once contract.

### 2.1 batch (f0-hack-fixes Fix A) — N per-op records
`insert_batch` + **`insert_batch_bytes` (currently has NO overlay route — latent F5 data-loss gap)** →
loop per entry: membership ⇒ `insert_cas_durable`; valued ⇒ **`upsert_cas_durable`** (match §2.0). Count
`Ok(true)`; first `Err` stops and returns the count so far (the failed entry's record is durable, replays).
Per-op durable, **not batch-atomic** (matches owned `insert_batch`). `_chars/_sorted/_grouped` inherit.

### 2.2 document-tx (f0-hack-fixes Fix B, tx-ii) — per-op overlay arm
Replace the `route_overlay()` reject (byte `document_tx.rs:189/193`, char `:326/339`): apply SETs via
`upsert_cas_durable`/`insert_cas_durable`, increments via `try_increment_cas_durable` (counter-monomorph
only, §1); **DROP BeginTx/CommitTx/sync on the overlay arm** (skip the orphan BeginTx in `begin_document`
under `route_overlay()`); reject a negative aggregated increment delta (don't silently owned-write).
Per-op durable, **not all-or-nothing** — **matches the owned path's actual recovery semantics**
(`reconcile_lww` ignores tx brackets, `f0-hack-fixes.md` §1.2). Document as a named residual. (tx-i =
all-or-nothing via a reconcile tx-bracket = a **separate, pre-existing recovery-path-convergence task**,
not Phase-F scope — surface to owner, do not silently defer.)

### 2.3 get_or_insert (f0-hack-fixes Fix D)
The atomic primitive `get_or_insert_durable_default` already exists. Verify the route
(`lockfree_value_route.rs`) calls it (not the racy 2-step `insert + get_lockfree`); rewrite if stale.

### 2.4 compare_and_swap — ALREADY DONE; verify + formalize
Implemented + phantom-safe (§0.1). Action: **confirm** the burned-record drop is covered by
`LockFreeOverlayDurableReplay.tla`; add an explicit `LockFreeOverlayValueCas.tla` +
`NoPhantomConditionalWrite` + `_Unsafe.cfg` (negative control: NOT burning ⇒ phantom write appears) so
CAS+merge are formally pinned (owner: "formal verification of Phase F + G5"; no-deferral).

### 2.5 merge_from / merge_replace / parallel_merge (A2 BLOCKER-3) — per-key CAS-retry-loop
**The minimal new piece.** A merge value is **state-dependent** (`merge_fn(self_val, other_val)`), the
same hazard class as CAS. Resolve by **reusing the proven CAS primitive**:
```
fn merge_value_cas_durable(&self, key, other_val, merge_fn) -> Result<()> {
    loop {                                            // obstruction-free; bounded-retry → brief lock fallback
        let self_val = self.value_read_faulting(key)?;       // re-read each iteration (overlay, NOT empty owned)
        let merged   = merge_fn(self_val.as_ref(), &other_val);
        match self.compare_and_swap_cas_durable_default(key, /*expected=*/self_val, /*new=*/merged)? {
            true  => return Ok(()),                   // won: ranked + marked by the CAS primitive
            false => continue,                        // concurrent change: CAS burned an unranked record → retry
        }
    }
}
```
- Self-read via `value_read_faulting` (overlay) fixes the "reads empty owned tree" bug
  (`merge_api.rs:34`). **No new `ValueWriteMode`** — the re-resolve lives in the OUTER loop; the inner
  CAS already re-checks `expected` against the fresh root and burns on miss.
- `merge_replace` = `merge_fn = |_self, other| other.clone()` (last-writer); `merge_from` = the custom
  fn; **parallel** variants resolve `merge_fn` in parallel (rayon, disjoint key partitions — A2 TASK H:
  resolve-only parallelism confirmed race-free) then funnel each key through `merge_value_cas_durable`
  (per-key atomic; **not batch-atomic**, matches owned).
- **Crash-safety:** every lost attempt's `Upsert` is unranked ⇒ dropped on Overlay reopen ⇒ no phantom
  merge. The winning attempt is ranked ⇒ survives. Same envelope as CAS (§2.4 TLA covers it).
- **Signature stays `&mut self`** for now (works: `&mut self` can call the `&self`-CAS internally) — A4's
  "C2 depends on F4" is **over-stated**; routing the body needs no `&self` conversion. F4 later collapses
  the signature.

### 2.6 The per-op semantics decision (explicit, not a deferral)
doc-tx/batch/merge on the overlay are **per-op durable, not all-or-nothing crash-atomic** — because
**that is exactly what the owned path delivers** (`reconcile_lww` ignores tx brackets). This achieves the
Phase-F goal (overlay ≡ owned). All-or-nothing (tx-i) would make the overlay *better than* owned and is a
separate task. **Surface to owner; do not bury.**

---

## 3. Carve-out category 4 — borrow-returning `get()`/`try_get()` (A4: trait-free, confirmed)
`get`/`try_get` are inherent-only (NOT on `ARTrie`; the trait has `get_value`, `artrie_trait.rs:325`).
They already return `None` under `route_overlay()`. Final contract: `#[deprecated(note=…use get_value())]`,
keep returning `None` (graceful). `get_value()` (owned clone, overlay-routed) is the canonical reader. No
trait-surface change.

---

## 4. F2 activation — the REAL 124 failures, 6 remediation buckets (A4)
Real: `2618 run, 2494 passed, 124 failed, 3 skipped` (build compiles clean feature-on). The 5 V8 buckets
miss ≥24 orphans. **Six** remediation categories:
1. **eligibility/flip asserts** (8) → cfg-gate / invert assertion.
2. **increment carve-out** (13) → retype to `Counter` handles (§1).
3. **doc-tx + merge carve-out** (16+9) → succeed via §2.2/§2.5 (flip assert `InvalidOperation`→success).
4. **compact carve-out** (16) → succeed via F6 (gate compact tests on F6).
5. **read via `get()`→None** (part of 38) → use `get_value()`.
6. **NEW — owned-representation white-box** (~18 orphans: eviction-registry, owned `walk_map`,
   reopen-`InvalidMagic` corruption-injection, walk-under-eviction, dirty/epoch) → these inspect the
   OWNED rep, which is empty post-flip. **Pin to `OverlayWriteMode::OwnedTree`** (construct un-flipped) —
   they are owned-tree white-box tests and must say so.
   Plus the **two real bugs** (NOT contract-flips): §2.0 `insert_with_value`→upsert (fixes
   `dictionary_law_correspondence`, `test_value_update_persistence`); and **root-cause
   `test_mixed_value_recovery`** (membership `insert()` + valued `insert_with_value()` on a non-`()`
   `V` — likely the same insert-once vs upsert divergence; verify).

**Honesty (A4 TASK F):** most of the 124 are **contract-flip rewrites** (asserting the carved-out
behavior), not "new functionality proven." State as "N tests migrated to the F2 contract," not "N now
pass." Genuinely-new arbitrary-V coverage = the dedicated `persistent_arbitrary_v_overlay.rs` suite.

**Default-flip (A4 TASK E):** one line, BUT adding `overlay-arbitrary-v` to `default` also pulls
`persistent-artrie` (→ memmap2/dashmap/lru/sysinfo/…) into every default build — a bigger blast radius
than "feature into default set." This is the **F2-default-on irreversible flip (owner GO #1)**.

---

## 5. F4 — lock collapse (A3: SOUND; 2 gaps to close)
Graph `CK→OR`, `CK→EC`, `OR→EC`, EC leaf — **acyclic**, CK at exactly 2 sites, no reentrancy. Field audit
COMPLETE as a **per-variant union**: **byte = 5** (`root`(OR), `eviction_coordinator`(EC),
`overlay_write_mode`(AtomicU8), `durability_policy`, `dirty_prefixes`); **char = 7** (`root`(OR),
`eviction_coordinator`(EC), `overlay_write_mode`, `durability_policy`, `memory_monitor`,
`checkpoint_manager`, `group_commit`(cfg)). **Document per-variant** (wrapping `dirty_prefixes` on char or
`memory_monitor` on byte = compile error).

**GAP 1 (BLOCKER-class impl constraint):** the eviction `disable` rewrite **MUST** use a statement-
temporary so the EC guard drops BEFORE `shutdown().join()`:
`let coord = self.eviction_coordinator.lock().take();  /* guard dropped */  if let Some(c)=coord { c.shutdown(); }`.
Binding the guard across the join reintroduces the production deadlock (disable holds EC + joins; worker
holds OR + waits EC). The current code is safe only by the outer-RwLock temporary; the `Mutex<Option<Arc>>`
wrap does NOT auto-preserve it.

**GAP 2 (MAJOR):** the 3 sister subsystems also `join()` a thread in `Drop`; their `disable_*`
(`memory_monitor` `observability.rs:224`, `group_commit` `:154`, `epoch_checkpointing` `:103`) need the
**same drop-before-join temporary**. Critical for `memory_monitor`: its **user-supplied callback can
re-enter the trie** (`force_eviction` → OR/EC) ⇒ holding the field mutex across the join is a real
cross-subsystem deadlock.

Plus the existing F4 mechanics (V8 §6): `&mut self`→`&self` ripple, ~266 `.read()/.write()` shared-handle
sites (mechanical; check the liblevenshtein sibling), delete `downgrade`, ctor `&mut self.root` fixes, C2
debug-assert fix. **IRREVERSIBLE — owner GO #2.** No new unsafe.

---

## 6. F5 / F6 / F7 (largely unchanged from V8; refinements)
- **F5** `load_root_immutable` (arena→`OverlayNode`) — generic over `V` from the start; both-loaders
  correspondence proptest over every on-disk format BEFORE F7 switches reopen to it. Reversible (flag).
- **F6** overlay-snapshot compaction (B2) = the `compact` carve-out (byte-only); **CK-gate `compact`**
  (closes R15: compact renames the file but isn't CK-gated today); watermark-bounded WAL retain +
  `synced_frontier ≤ watermark` assert + post-rename-retain test. Reversible until wired. Resolves F2
  bucket 4.
- **F7** delete owned tree + kill-switch; §2 made doc-tx/merge/CAS overlay-native so their owned bodies
  are now genuinely deletable; reopen→`load_root_immutable`; lock graph `CK>OR>EC`→`CK>EC`.
  **IRREVERSIBLE — owner GO #3 FINAL.** Per-sub-step green.

---

## 7. Formal-verification obligations (owner: "formal verification of Phase F + G5")
| Obligation | Model | New? | Phase |
|---|---|---|---|
| conditional/recomputed value write (CAS + merge) phantom-safety | `LockFreeOverlayValueCas.tla` + `NoPhantomConditionalWrite` + `_Unsafe` (don't-burn ⇒ phantom) | **NEW** | §2.4/2.5 |
| per-op batch/doc-tx replay ordering | `LockFreeOverlayDurableReplay.tla` (existing) + a deterministic `batch_overlay_replay_orders_by_commit_rank` regression | existing | §2.1/2.2 |
| lock-collapse no-lost-write (no writer-exclusion) | `LockFreeDurableCheckpoint.tla` (existing; already no-writer-exclusion) — re-run as regression | existing | F4 |
| concurrent-checkpoint serialization (CK) | `ConcurrentCheckpointSerialization.tla` (committed F3) — re-run + real-disk 2-checkpoint+reopen test | existing | F4 |
| eviction CK>OR>EC deadlock-freedom | loom `checkpoint(+eviction) ‖ disable_eviction ‖ writer` + the drop-before-join discipline (§5) | **NEW loom** | F4 |
| compaction WAL bound | `synced_frontier ≤ watermark` assert + post-rename-retain disk test | existing reasoning | F6 |
Each phase gate: full suite green + `scripts/verify-formal-correspondence.sh` exit 0 (SANY + TLC +
`_Unsafe` negative controls MUST fire) + `verify-unsafe-boundary-inventory.sh` exit 0 + 0 new unsafe.
TLC under `systemd-run … MemoryMax`; loom ≤3 threads/2 keys; disk tests real-disk (never tmpfs).

---

## 8. Ordering / reversibility (A4 corrections)
1. **C0** `insert_with_value`→upsert bug fix (§2.0) — cheapest, unblocks F2 correspondence. Reversible.
2. **C1** increment `Counter`-bound specialization (§1). Reversible (signature).
3. **C2** wire f0-hack-fixes (batch/doc-tx/get_or_insert) + merge primitive + CAS formalization (§2.1-2.5).
   Reversible (restore rejects). Stays `&mut self` (NOT F4-dependent — A4 over-stated).
4. **C4** deprecate `get()`/`try_get()` (§3). Reversible.
5. **F2-migrate** the 124 → 6 buckets (§4). Reversible (feature off).
6. **F4** lock collapse (§5). **IRREVERSIBLE — owner GO #2.** After F2-migrate green + soak.
7. **F2-default-on** (§4). **IRREVERSIBLE — owner GO #1.** After soak.
8. **F5** loader (§6). Reversible.
9. **F6** overlay compaction (§6). Reversible until wired. (Gates F2 bucket 4.)
10. **F7** delete owned (§6). **IRREVERSIBLE — owner GO #3 FINAL.** After F5+F6 + soak.

Each irreversible flip = isolated commit behind its own owner GO, after a soak. **C1 before F4** is
defensible (specialize before the collapse moves methods) but the V8 "&mut→&self ripple" justification is
imprecise (increment is already `&self`; the ripple is F4's). The only true cross-dep is **F2 bucket 4 ⊢
F6**.

---

## 9. Open questions for the V9 red-team (round 2)
- Q1: Does `LockFreeOverlayDurableReplay.tla` actually prove "an unranked durable record is dropped on
  Overlay reopen" generally (so it subsumes CAS-burn + merge-burn), or is the new `LockFreeOverlayValueCas`
  strictly required? Verify against the .tla.
- Q2: Is `merge_value_cas_durable = loop{read;merge;cas}` genuinely lost-write-free AND livelock-bounded?
  Find a trace where a merge is lost or never terminates. Confirm `merge_replace`/parallel funnel reuse it
  safely; confirm self-read uses `value_read_faulting` not `get`.
- Q3: Is routing overlay `insert_with_value`→`upsert_cas_durable_default` correct for **empty-string `""`**
  and for the **counter** monomorph (does upsert vs insert-once matter for `try_increment` paths)? Verify
  no regression to the empty-string-value support or the increment path.
- Q4: Does the `Counter` sealed-trait bound cover **exactly** the value types the increment tests
  instantiate (i64 AND u64, char AND byte)? Enumerate every `.increment(`/`.fetch_add(` receiver type.
- Q5: F4 — re-confirm the per-variant field union (byte=5/char=7) and that the 3 sister `disable_*` +
  eviction use a statement-temporary; find any 4th `join()`-under-lock site.
- Q6: doc-tx tx-ii — does dropping BeginTx/CommitTx on the overlay arm leave any reader/recovery path
  expecting the bracket? Is the negative-aggregated-increment rejection complete (char increments only)?
- Q7: `insert_batch_bytes` missing overlay route — confirm it currently silently writes owned under the
  flip (data-loss), and that the §2.1 route closes it.

---

# V10 refinements (post red-team round 2)

Round 2 (agents `ae3431df2…` merge/CAS, `a6d9d87ee…` wiring, `a58059086…` Counter/F4) found NO
fundamental refutation — the V9 core designs are sound. The deltas below close the completeness/precision
gaps + one genuinely-new concern (merge termination). Convergence trajectory: round 1 = wrong approach
(C2 fold); round 2 = right approach, fix these specific sites.

## V10.1 — merge primitive (fixes R2-1 BLOCKER termination + MAJOR pseudocode/parallel)
- **Correct per-key body** (real API is `Fn(&V,&V)->V`; absent key inserts `other` WITHOUT calling
  `merge_fn`):
  ```
  let self_val = self.value_read_faulting(key)?;            // Option<V>
  let merged = match &self_val { Some(s) => merge_fn(s, &other_val), None => other_val.clone() };
  // compare_and_swap_cas_durable_default(key, expected = self_val, new = merged)
  ```
- **Termination/WAL-amplification fix:** (a) per-key CAS loop uses `crossbeam_utils::Backoff` (spin→yield)
  — the CAS primitive's OUTER gate (`durable_write.rs:495`) already catches most concurrent changes with
  NO fsync, so backoff bounds the expensive read-consistent-then-lose-root-CAS window; (b) the whole-trie
  `merge_from` driver takes a dedicated per-trie **`merge_lock: Mutex<()>`** (a NEW **leaf** lock,
  independent of CK/OR/EC — merge takes no other lock under it, so no cycle) serializing merge‖merge (kills
  merge-vs-merge livelock). merge‖{insert,increment,upsert,remove} stays obstruction-free but practically
  terminating (those ops are quick; the CAS wins between them) — document the residual: a bulk merge under
  *sustained single-key external writes* is obstruction-free (unrealistic workload; the system is making
  progress, just this merge is slow). WAL amplification ≤ backoff-bounded.
- **`merge_replace` = direct per-key `upsert_cas_durable`** (no read-compare needed; absent-key already
  inserts `other`, present-key overwrites) — cheaper than the CAS loop.
- **parallel_merge:** resolve `other`'s entries in parallel (read-only, race-free), collect, then apply via
  the serial driver (merge_lock + per-key CAS). The funnel-through-CAS made the parallel *write* illusory
  anyway (R2-1 TASK E); document parallel-merge applies serially under the overlay.
- **Lock-order note:** `merge_lock` is a leaf acquired ONLY by `merge_from`/`merge_replace`/`parallel_merge`;
  never held across CK/OR/EC; checkpoint (CK) snapshots the lock-free root concurrently as designed. No
  interaction.

## V10.2 — increment Counter bound (fixes R2-3 BLOCKER + precision)
- **`Counter = {i64, u64}`** (empirically sufficient: lib + all 5 inherent-increment test crates compile
  with the bound). byte counter = `i64`, char increment callers use BOTH `<i64>` and `<u64>` → `Counter`
  must cover both.
- **Bound the 3 methods INDIVIDUALLY** (`pub fn increment(&self,…) where V: Counter`, same for
  `fetch_add`, byte `increment_bytes`) — do NOT add `Counter` to the whole `impl` block:
  `try_increment_impl_no_wal` (char `atomic_ops.rs:120`) stays on `DictionaryValue` (arbitrary-V recovery
  caller `mmap_ctor.rs:1062`, BatchIncrement `mutation_core.rs:335`).
- **Migrate the 2 trait callers the removal breaks** (compiler-proven E0599/E0782):
  `tests/persistent_artrie_recovery_tests.rs:2537/2541/2545` (`SharedCharTrie<i64>` → retype to inherent
  `PersistentARTrieChar<i64>`); `tests/vocab_trait_honesty.rs:195-208` (asserts the trait-level
  increment-reject which ceases to exist → rewrite to assert vocab has no `.increment()` /
  inherent-only). Round-1 A1's "zero Shared callers" is REFUTED.

## V10.3 — wiring completeness (fixes R2-2 MAJORs)
- **§2.0 must ALSO fix byte `insert_batch_entry_overlay`'s valued arm** (`persistent_artrie/mutation_api.rs`)
  — it calls `insert_cas_with_value_durable_default` (insert-once) DIRECTLY, bypassing `insert_with_value`,
  so fixing only `insert_with_value` leaves byte batch insert-once while byte single becomes upsert
  (silent divergence). Change its valued arm → `upsert_cas_durable_default`. (char batch delegates to
  `self.insert_with_value` so it auto-inherits the fix.) The "insert_batch_bytes has no overlay route"
  premise is STALE (both already routed — tree ahead of f0-hack-fixes); the real issue is the valued-arm
  insert-once.
- **§2.2 doc-tx is VARIANT-SPECIFIC:** byte `DocumentTransaction` has NO `increments` field (increments
  are folded into `shadow_terms` as absolute SETs at buffer-time) → byte overlay arm = upsert(shadow_terms)
  ONLY (NEVER route through `try_increment_cas_durable` — would double-count). char = upsert(shadow_terms)
  + `try_increment_cas_durable(aggregated_increments)` with negative-aggregate reject. Reuse char's
  existing aggregate/overflow preflight (`document_tx.rs:412-423`).
- §2.3 get_or_insert: already atomic in BOTH variants (route_get_or_insert commented out, live calls
  `get_or_insert_durable_default`); only the stale byte `get_or_insert_bytes` docstring
  (`atomic_ops.rs:338-342`) needs correction.
- Only 1 test asserts the buggy insert-once: `persistent_arbitrary_v_overlay.rs:110-114` → flip to
  assert overwrite (value becomes the 2nd insert).

## V10.4 — F4 drop-before-join completeness (fixes R2-3 BLOCKER 5th site)
- Drop-before-join sites (post-F4, statement-temporary `let x = self.field.lock().take(); /*guard drops*/
  if let Some(c)=x { c.shutdown(); }`): (1) `disable_eviction` [byte+char], (2) `disable_memory_monitor`
  [char], (3) `disable_group_commit` [char], (4) `disable_epoch_checkpointing` [char], **(5) `close()` /
  `Drop` [byte `dict_impl.rs:536-557`, char `mod.rs:567-577`] — joins eviction thread by a bare `&self`
  field read, runs on EVERY teardown → MUST get the temporary** (the missed site). (6) `compact`
  `wal_writer=None` [byte `compaction_impl.rs:295/309`] — WAL-sync join, no trie re-entry → benign,
  hygiene note only (don't hold CK across it).
- `overlay_write_mode` is a plain `Copy` enum (NOT atomic) — wrap as an atomic-backed cell or `Mutex`;
  byte=5/char=7 field union CONFIRMED.

## V10.5 — formal model (fixes R2-1 REQUIRED)
- **NEW `LockFreeOverlayValueCas.tla`** (+`.cfg`+`_Unsafe.cfg`) is STRICTLY required — `LockFreeOverlayDurableReplay.tla`
  never models a durable-but-refused (burned) record. Must add: `RecomputeAndAppend` (read→merge→append+sync
  durable, per-iteration expected), `WinAndRank` (expected==fresh-current → publish+rank), `BurnOnLoss`
  (refused → durable-but-unranked), recovery ranging over durable-WAL-incl-burned with the regime-drop +
  checkpoint-skip, invariants `NoPhantomConditionalWrite` + `NoLostConditionalWrite`, and `_Unsafe.cfg`
  (don't-burn ⇒ phantom MUST fire). Include a `Checkpoint(watermark)` action so burn-drop is checked via
  BOTH regime AND checkpoint-skip. Covers CAS + merge. Wire into `verify-formal-correspondence.sh`.

## V10.6 — round-3 red-team targets (the NEW/uncertain pieces)
- T1 (merge_lock + backoff): deadlock/livelock/lock-ordering of the new `merge_lock` leaf vs CK/OR/EC and
  checkpoint; does serial-apply parallel_merge regress any correctness; is the obstruction-freedom residual
  truly acceptable.
- T2 (completeness sweeps — find what round 2 sampled): EVERY call site routing to
  `insert_cas_with_value_durable_default` that should be upsert (byte batch + any others); EVERY
  trait/`dyn`/UFCS `.increment()`/`ARTrie::increment` caller (the 2 found + any others); EVERY `join()`
  reachable from a `&self`/`Drop`/`disable_*` path (the 5th + any 7th).
- T3 (new TLA adequacy): does the proposed `LockFreeOverlayValueCas.tla` action set actually capture the
  merge bounded-retry + the byte-batch-upsert + the doc-tx per-op semantics, and does `_Unsafe` fire.

---

# V11 refinements (post red-team round 2... 3)

Round 3 (agents `a8ae65a79…` merge_lock, `a3338a3f0…` sweeps) found NO new data-loss; sweeps A/B CLOSED;
remaining = deadlock-discipline with known in-repo fix patterns. Trajectory: R1 wrong-approach → R2
right-approach-fix-sites → R3 fix-deadlock-discipline-sites (narrowing).

## V11.1 — cross-instance merge deadlock (R3-1 BLOCKER) — the load-bearing fix
char `union_with` (`mod.rs:1130-1132`) holds `other.read()` + `self.write()` SIMULTANEOUSLY (other-then-self)
⇒ `A.union_with(&B)` ‖ `B.union_with(&A)` = AB/BA deadlock. **Pre-existing in committed code** (the reject
is inside `merge_from`, AFTER both locks taken); merge wiring WIDENS the held-both window to O(terms).
`merge_lock` does NOT fix it. **FIX (the vocab pattern, already correct at `persistent_vocab_artrie/mod.rs:476-483`):
snapshot `other` fully into an owned `Vec` under `other.read()`, DROP `other`'s guard, THEN take self's
write/merge_lock and apply.** Mandate for ALL merge entry points (merge_from/merge_replace/merge_from_batched*/
parallel_merge/union_with/union_replace, byte+char). Rewrite char `union_with` accordingly. (byte
`SharedARTrie` has no `union_with` — char-only via union_with, plus any future byte Shared merge wrapper.)
Snapshot cost: O(other) memory — acceptable (merge is bulk/rare); for a huge `other`, snapshot in chunks
under repeated short read-locks (still never two OR locks at once).

## V11.2 — merge_lock discipline (R3-1 MAJORs)
- `merge_lock: Arc<parking_lot::Mutex<()>>` — mirror `checkpoint_lock` EXACTLY (ships F4-ready, not a Tier-2
  `Mutex<Option<Arc>>` wrap). **Add to the F4 audit: byte=6, char=8** (the V9 §5 "complete" union must grow).
- **Lock order: `CK > merge_lock > OR > EC`** (pre-F7) → **`CK > merge_lock > EC`** (post-F7, OR gone).
  merge_lock is acquired ONLY by the merge drivers, never by any CK/OR/EC holder.
- **Single acquisition site:** take `merge_lock` in exactly the innermost private driver
  (`merge_from`/`_with_options`); public wrappers (`merge_replace`, `merge_from_batched`,
  `merge_from_batched_grouped`) must NOT re-take it (parking_lot is non-reentrant → double-take = self-
  deadlock). Audit the delegation chains.
- Per-key body = obstruction-free CAS + backoff via `std::hint::spin_loop()` + `std::thread::yield_now()`
  (NO new `crossbeam-utils` dep). Obstruction-free residual vs sustained single-key external writers is
  ACCEPTABLE (matches the shipped lock-free writers; system makes progress; merge_lock kills only
  merge‖merge livelock). `merge_replace` = direct per-key `upsert_cas_durable` (no read-compare).
- parallel_merge: parallelize ONLY the read of `other`; apply serially (the parallel write was illusory).
  Drop the "4-6×" docstring (byte `parallel_merge.rs:50`).

## V11.3 — F4 drop-before-join: 2 ADDED sites/classes (R3-2 SWEEP C)
The complete drop-before-join set (statement-temporary: `let x=self.field.lock().take(); /*drop guard*/ if
let Some(c)=x { c.shutdown(); }`):
1-2. `disable_eviction` [byte+char]; 3-4. `close()`/`Drop` [byte+char] (bare-read 5th site, every teardown);
5-7. char `disable_{memory_monitor,group_commit,epoch_checkpointing}`.
**ADDED — 8. vocab `disable_eviction` (`persistent_vocab_artrie/mod.rs:795-803`)** — holds `self.write()`
LIVE across `shutdown()`/join; the vocab eviction callback re-enters via `trie.write()` ⇒ **latent deadlock
TODAY** (fix now regardless of F4; vocab is otherwise out of byte+char F4 scope).
**ADDED — 9 (a CLASS). the `enable_*` re-arm path** — `enable_{memory_monitor,group_commit,epoch_checkpointing,
eviction}` do `self.field = Some(new)`; if already enabled, the assignment drops the OLD `Arc` → its `Drop`
joins the old worker ⇒ post-F4 `*self.field.lock() = Some(new)` joins UNDER the held guard (re-entrant
callback ⇒ deadlock on re-arm). FIX: `let old = { let mut g=self.field.lock(); g.replace(new) }; drop(old);`
(take-old-then-drop-guard-then-let-old-drop). Apply to all enable_* (both variants where the field exists).
Benign (hygiene only): byte `compact` `wal_writer=None`; `close()` `wal stop_sync`.

## V11.4 — sweeps A/B FINAL (R3-2; no further edits)
- **A (insert-once→upsert):** EXACTLY 3 bug-sites → `upsert_cas_durable_default`: byte `mutation_api.rs:63`
  (insert_with_value), byte `mutation_api.rs:360` (insert_batch_entry_overlay valued arm), char
  `mutation_api.rs:77` (insert_with_value). LEGITIMATE insert-once (do NOT change): byte
  `lockfree_cas.rs:1314` + char `lockfree_cas.rs:1854` (the public insert-once primitive bodies), core
  `durable_write.rs:541` (get_or_insert). char `insert_batch`/`insert_batch_bytes` auto-inherit via
  `self.insert_with_value`; byte batch routes through site #360; vocab has NO value-write overlay path
  (warn-stubs). doc-tx forward-looking: SET arm uses upsert; byte has NO `increments` field.
- **B (trait-increment callers to migrate):** EXACTLY 2: `tests/persistent_artrie_recovery_tests.rs:2537/2541/2545`
  (`SharedCharTrie<i64>`→retype inherent), `tests/vocab_trait_honesty.rs:203` (rewrite). `Counter={i64,u64}`
  sufficient; all 3 `impl ARTrie` blocks drop `increment` together.

## V11.5 — round-4 = convergence check (holistic)
Round 4 verifies V11 has converged: (T1) any OTHER pre-existing cross-instance / two-trie deadlock the
focused sweeps missed (byte/vocab merge wrappers, any op taking two tries' locks); (T2) the
snapshot-other-then-release fix is correct + complete + memory-safe for large `other`; (T3) the enable_*
re-arm + vocab disable_eviction fixes are correctly specified; (T4) a final no-data-loss re-confirm that
V11's refinements didn't reintroduce a hole. If round 4 is clean (only confirmations) ⇒ CONVERGED ⇒ one
final confirming round-5 per the owner's "red-team once more."

---

# Round 4 (convergence check) + the F7 owner-fork

Round 4 (agents `a1862c213…` holistic-deadlock, `a89093da6…` fresh-skeptic) did NOT converge — it found 2
NEW blockers the merge-focused rounds missed, BOTH in F5/F7 scope. The EARLY phases (C0/C1/C2/C4/F2/F4) are
CONFIRMED converged + data-loss-sound (both agents failed to break the merge/CAS/upsert/lock-collapse core).

## Confirmed sound (round 4)
- burn=unranked=dropped phantom-safety; merge CAS-retry lost-write-free + terminating (obstruction-free +
  merge_lock kills merge‖merge); value_publish_inner generic over V (arbitrary-V merge/CAS reachable at the
  flip); C0 upsert bug + 3 sites accurate; F4 char 7-field audit matches code; increment→Counter loses
  nothing; LockFreeOverlayValueCas.tla correctly identified as required; the inner lock graph
  (CK→OR→am→bm, EC/watermark/cache leaves) is a DAG.

## NEW blockers (round 4) — all in F5/F7
- **B1 — F7 drops zipper/root()/DictionaryNode/transducer/fuzzy** (owned-tree-only; overlay returns empty+warn:
  `zipper.rs:99-118`, char/byte `root()` mod.rs:616-631). Exercised by the FORMAL GATE
  (`zipper_language_correspondence.rs:522-557` in `verify-formal-correspondence.sh:45`; GAP_LEDGER:63). NOT
  in any carve-out. F2-default-on → empty results; F7 → permanently broken + gate regression.
- **B2 — F5/F7 Option A (load_root_immutable) is XL + data-loss-critical, erased to a 1-liner.** Source doc
  `phase-f-g5-delete-owned-tree.md` §3.1/§3.4: Option A = new arena→OverlayNode parser reading EVERY legacy
  on-disk format = "weeks of parser + back-compat work", and RECOMMENDS Option B (keep owned dormant +
  retain kill-switch; "almost nothing owned is deletable"). F7 deletes the kill-switch in the same arc ⇒
  new single-soak parser becomes the ONLY reopen path with NO fallback ⇒ misread any legacy format = brick.

## Other round-4 findings (fold into the chosen path)
- pre-existing live deadlocks to fix NOW (independent of F-flips): char `union_with` mod.rs:1130-1132
  (AB/BA, both modes), vocab `disable_eviction` mod.rs:796-800 (guard across join). Out-of-scope but real:
  DynamicDawg/DynamicDawgChar `union_with` (same AB/BA).
- merge_lock is the POST-F4 serializer (pre-F4 the Shared OR-write already serializes ALL writers, even
  overlay ones) ⇒ introduce merge_lock AT F4 (it replaces OR-write's role); resolves the "merge_lock>OR
  impossible" inconsistency. merge_lock needs a loom schedule (merge‖checkpoint‖insert/remove‖disable_evict).
- doc-tx "matches owned": a 2nd recovery path `ln_phase` (recovery.rs:768-856) HONORS tx brackets; must
  prove it's unreachable on every production reopen (else overlay per-op is a real atomicity downgrade).
- MINOR: stale "F4 audit COMPLETE byte=5/char=7" → byte=6/char=8 (merge_lock); §0.1 framing; the new TLA
  wires into verify-formal-correspondence.sh at THREE sites (SANY :218, TLC :272, _Unsafe :348).

## CONVERGENCE STATUS
- **CONVERGED + ready to implement (reversible, F7-independent):** C0 (insert_with_value→upsert, 3 sites),
  C1 (increment Counter-bound + migrate the 2 trait callers), C2 (doc-tx tx-ii + batch + merge CAS-retry +
  the pre-existing deadlock fixes + LockFreeOverlayValueCas.tla), C4 (deprecate get()/try_get()),
  F2-migrate (124 failures → 6 buckets).
- **CONVERGED, IRREVERSIBLE (owner GO #2):** F4 (lock collapse + merge_lock + the 9-site drop-before-join +
  per-variant field union + snapshot-other) — first irreversible flip.
- **BLOCKED on OWNER DECISION:** F5/F7 end-state — Option A (full delete + build/carve overlay
  zipper+transducer+fuzzy + XL data-loss reopen parser, irreversible, brick-risk) vs Option B (keep owned
  dormant + retain kill-switch; delete only dead owned WRITE paths; reversible; no XL parser; preserves all
  capability). Red-teaming CANNOT resolve a scope/risk fork — surfaced to owner per the source doc.
