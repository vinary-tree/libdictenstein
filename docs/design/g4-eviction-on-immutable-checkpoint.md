# Design: EVICTION-ON Immutable-Snapshot Checkpoint (reversible, bench-gated)

**Repo:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein` · **Scope:** a REVERSIBLE, `bench-internals`-gated
extension making the immutable-overlay checkpoint support eviction-ON, + a pre-registered benchmark section
appended to the frozen ledger. **NOT the production flip** (`checkpoint()` untouched; owner-gated; out of
scope). **ZERO new `unsafe`.** Persisted from the Plan-agent design (2026-06-02).

## (1) Feasibility + the exact gap (code-cited)
**FEASIBLE, reversibly, small surface.** The hard part (watermark-bounded WAL reclaim under lock-free
out-of-order commit) is ALREADY implemented + proven for the retain-WAL variant; the new work is purely
additive (publish the eviction registry that capture already builds).
1. **`capture_snapshot_immutable` ALREADY populates the registry.** `persist.rs:346-349` builds
   `eviction_registry = self.eviction_coordinator.as_ref().map(|_| DiskLocationRegistry::new())`; `:426-430`
   threads `eviction_registry.as_mut()` into the SAME `serialize_char_node_to_disk` the owned path uses, which
   registers every node via `reg.register_char(path, ptr, len, depth, node_type)` (`:919-927`). Registry-over-
   immutable-snapshot is **already correct by construction**; carried in `CheckpointSnapshot.eviction_registry`
   (`:500`). Requirement #1 already satisfied.
2. **The publish side is the gap.** `publish_immutable_snapshot_retaining_wal` REFUSES a registry:
   `debug_assert!(snapshot.eviction_registry.is_none(), …)` (`persist.rs:560-565`) + deliberately doesn't
   publish it (`:554-559`). The bench shim `bench_immutable_checkpoint` (`:622-626`) calls it → benchmark runs
   eviction-OFF (ledger §3). **This `debug_assert!` is the one-line block.**
3. **Owned reference does the missing step:** `publish_durable_and_reclaim` (`:108-179`) publishes via
   `coordinator.update_disk_registry(registry)` (`:123-127`). But its reclaim is lock-free-incompatible: reclaims
   by `next_lsn` (`:153`) which the lock-free path never advances (`:405-411`), and `debug_assert_eq!`s next_lsn
   unchanged (`:140-146`) which a concurrent `insert_cas_durable` violates.
4. **Reclaim-correct publisher already exists:** `publish_immutable_snapshot_retaining_wal` writes
   `Checkpoint{checkpoint_lsn = committed_watermark_at_capture}` (`:567-598`), no truncate (retain-WAL); no-lost-
   write proven (`:524-536` doc + the multi-writer soak `:1407-1494`).
**⇒ the new component is ONE new publisher = retain-WAL publisher + registry publication, via a sibling bench
shim. Destructive truncation stays owner-gated.** Stale-comment note: `:557-558` claims the registry "is not
`Clone`" — FALSE (`disk_registry.rs` derives `Clone`); design moves the registry, doesn't need `Clone`; don't
repeat the claim.

## (2) Registry-over-immutable-snapshot
No new build code (already correct). New publisher beside `publish_immutable_snapshot_retaining_wal`:
```rust
#[cfg(any(test, feature = "bench-internals"))]
pub(crate) fn publish_immutable_snapshot_retaining_wal_with_eviction(
    &self, snapshot: CheckpointSnapshot,   // BY VALUE — moves the registry out
) -> Result<()> {
    let checkpoint_lsn = snapshot.committed_watermark_at_capture.ok_or_else(|| /* internal err */)?;
    self.publish_snapshot(&snapshot)?;     // (1) durable descriptor publish (lin. point) + verify
    self.verify_checkpoint()?;
    if let Some(registry) = snapshot.eviction_registry {   // (2) publish ONLY AFTER verify proves durable
        if let Some(ref coordinator) = self.eviction_coordinator {
            coordinator.update_disk_registry(registry);    // coordinator.rs:379 (in-memory swap, no fsync)
        }
    }
    if let Some(ref wal_writer) = self.wal_writer {        // (3) record checkpoint_lsn = watermark; sync;
        wal_writer.append(WalRecord::Checkpoint { checkpoint_lsn, timestamp })?;  //     RETAIN WAL (no rotate)
        wal_writer.sync()?;
    }
    Ok(())
}
```
By-value because `update_disk_registry` consumes the registry; mirrors owned `publish_durable_and_reclaim(snapshot)`;
only caller is the new shim. `publish_snapshot(&snapshot)` borrows before the move.

**Invalidation contract PRESERVED (the subtlety):** every durable mutation flows through `append_to_wal_inner`
(`wal_helpers.rs:78`) whose first act is `invalidate_eviction_registry()` (`:86`). The lock-free durable writer
ALREADY honors this: `insert_cas_durable` step 1 = `append_to_wal_returning_lsn` → `append_to_wal_inner` →
invalidate, BEFORE its visibility CAS (`lockfree_cas.rs:214,232`); likewise `try_increment_cas_durable` (`:784`).
So a concurrent writer invalidates the published registry before its write is visible; `select/perform/force_
eviction_char` gate on `is_valid()` (`disk_registry.rs:325,370`; `coordinator.rs:332,592,636`) → a dirtied
registry yields ZERO evictions, never a stale-pointer eviction. Risk = eviction *liveness*, not safety.

## (3) Watermark-bounded reclaim + no-lost-write proof sketch (data-loss-critical)
**Load-bearing decision: RECORD `checkpoint_lsn = committed watermark` + RETAIN WAL — NO destructive truncate**
(truncation = owner-gated flip, out of scope per `persist.rs:512-514`). Identical to the proven eviction-OFF
treatment. **The single most dangerous line is the SAME line already shipped + proven; eviction-ON does not move
it.**

Proof sketch: let `w = committed_watermark_at_capture` (captured `Acquire` STRICTLY before the root load —
`persist.rs:403`<`:420`, "DO NOT REORDER" `:351-402`); `S` = terms in the captured snapshot; recovery yields
`image(S) ⊕ replay{lsn > w}` (the `Checkpoint{checkpoint_lsn=w}` gates replay to tail >w; TLA `RecoveredSet`
`LockFreeDurableCheckpoint.tla:164-165`). For any visible write LSN `ℓ`:
- **ℓ ≤ w:** watermark contract ⇒ ℓ committed ⇒ (Order A) WAL-synced-durable before its visibility CAS, which
  linearized ≤ the snapshot root load (watermark read first ⇒ loaded root ⊇ all ℓ≤w) ⇒ `ℓ ∈ S ⊆ image(S)`. Preserved in image.
- **ℓ > w:** `ℓ > checkpoint_lsn` ⇒ recovery replays its (durable, retained) WAL record. Preserved via replay.
Exhaustive on `ℓ ⪋ w`; no double-count (membership idempotent; counter deltas: `Checkpoint{=w}` makes recovery
SKIP image-folded ≤w, SUM only retained tail >w — the c0=115-vs-60 bug `:524-530`). **Registry is invisible to
recovery** (`EvictionRegistryPublication.tla` `JustRecoveredMatchesDurable`; `recovery_independent_of_registry`
test `persistent_char_eviction_registry_correspondence.rs:133-159`) ⇒ eviction-ON cannot change the conclusion.
Capture-ordering assert `debug_assert!(watermark ≤ synced_frontier)` (`:464-471`) inherited verbatim.

## (4) Reversible bench-internals exposure + rollback
```rust
#[cfg(feature = "bench-internals")]
pub fn bench_immutable_checkpoint_with_eviction(&self) -> Result<()> {
    let snapshot = self.capture_snapshot_immutable()?;             // builds the registry when eviction on
    self.publish_immutable_snapshot_retaining_wal_with_eviction(snapshot)
}
```
**Reachability:** `eviction_coordinator` is `pub(crate)` (`mod.rs:438`), so the bench binary needs a gated
enabler `#[cfg(feature="bench-internals")] pub fn bench_enable_eviction(&mut self, config: EvictionConfig)` on
`PersistentARTrieChar` that constructs the coordinator exactly as `SharedCharARTrie::enable_eviction`
(`mod.rs:1452-1507`). (TREATMENT can't run over `SharedCharARTrie` because `bench_immutable_checkpoint*` are
`PersistentARTrieChar` methods.) **Rollback (one edit each):** delete the 2 shims; remove the `bench-internals`
disjunct from the publisher (→ `cfg(test)`-only); revert ledger §E + bench arm. `checkpoint()` + production
untouched. ZERO new `unsafe` (only safe APIs) ⇒ `verify-unsafe-boundary-inventory.sh` stays exit-0.

## (5) Formal-verification plan
**Reclaim under lock-free commit ALREADY covered:** `LockFreeDurableCheckpoint.tla` has `ReclaimWal` (`:152-160`)
+ `CrashRecover` (`:167-171`), proves `NoLostWriteUnderLockFreeCommit` (`:200-201`) + `CaptureEqualsPublishFrontier`
(`:195-196`) under `USE_WATERMARK=TRUE`; `_Unsafe.cfg` = losing negative control. The bench reclaim (record-
watermark+retain) is a SUBSET of `ReclaimWal`. **No new TLA for the reclaim.** NOT captured: the registry
interaction. **Minimal NEW spec `LockFreeDurableCheckpointEviction.tla`** (do NOT mutate the frozen base spec),
reusing the base + adding: `registryDurableUpTo: Nat`, `registryValid: BOOLEAN`; `PublishCheckpoint` also sets
`registryDurableUpTo'=ckptTarget`, `registryValid'=TRUE` (after Verified→Publish); `Commit(w)` sets
`registryValid'=FALSE` (invalidation under lock-free writers); `EvictUnderRegistry` enabled only when
`registryValid`, evicts only entries `≤ registryDurableUpTo`. **Invariants:** `NoLostWriteUnderLockFreeCommit`
(re-derived under reclaim+eviction — headline); `RegistryPointsAtDurableWatermark == registryValid =>
registryDurableUpTo <= Watermark`; `EvictionTouchesOnlyDurable` (evicted ⊆ durableCkpt); keep
`CaptureEqualsPublishFrontier`/`RecoveredNeverInventsState`/`ImmutableSnapshotIsClosed`/`DurablePrefix`.
**CONSTANTS:** `Writers={w1,w2}`, `Lsns={1,2,3}`, `NoLsn=0`, `USE_WATERMARK=TRUE`, `CHECK_DEADLOCK FALSE`; a
`_Unsafe.cfg` (`USE_WATERMARK=FALSE`) re-confirms the losing trace. Register both in
`verify-formal-correspondence.sh` SANY + RUN_TLC lists (beside `:235-236,283-284`); script stays exit-0.

**Rust correspondence tests** (`#[cfg(test)] mod immutable_eviction_checkpoint_correspondence` in persist.rs beside
`:1061`):
- **T1 `immutable_eviction_checkpoint_reopens_losing_nothing`:** eviction-enabled overlay trie, Immediate,
  enable_lockfree; insert_cas_durable a tier-spanning set; `capture_snapshot_immutable` (assert
  `snapshot.eviction_registry.char_len() > 0` — GAP closed); `publish_*_with_eviction` (assert
  `evictable_node_count() > 0`); force an eviction (every term still resolves); drop WITHOUT destructive reclaim;
  reopen; assert EVERY acknowledged term present.
- **T2 `writers_concurrent_with_eviction_checkpointer_all_survive_reopen`:** N insert_cas_durable writers ‖ a
  checkpointer looping capture + `publish_*_with_eviction` (retain) + a racing force_eviction; reopen ⇒ exact
  acknowledged set survives (counters CAPTURE-only like `:1516`).

## (6) Benchmark §E (NEW appended ledger section, frozen at its own persist time; do NOT edit §1-§11/RESULTS)
Both arms eviction ENABLED. CONTROL = owned tree + `publish_durable_and_reclaim` (publishes registry `:123-127`);
TREATMENT = overlay + `bench_immutable_checkpoint_with_eviction`.
- **HE1:** Immediate + eviction-ON, TREATMENT throughput > CONTROL (disjoint); two-sided Welch; supported iff
  positive ∧ significant ∧ d≥0.8. **Expectation:** eviction publication is OFF the timed writer path (checkpointer's
  `update_disk_registry` = one `RwLock::write` swap), so track the eviction-OFF result (+312%); registry build cost
  is in BOTH arms (same serializer). **Secondaries (vetoes):** SE1 pause T≤C; SE2 tails ≤1.10×; SE3 RSS ≤1.25×;
  SE4 contended not sig worse; **SE5 (NEW correctness veto): post-checkpoint force_eviction + reload returns exact
  values in BOTH arms — fail ⇒ ABORT (bug, not perf).**
- **§E.2 durability-parity-under-eviction:** per-write fsync unchanged+equal (ledger §2; invalidate is a flag bump,
  no fsync). Per-checkpoint: CONTROL = 1 data sync + 1 WAL sync + rotate(TRUNCATE); TREATMENT = 1 data sync + 1 WAL
  sync + NO rotate(RETAIN). `update_disk_registry` adds ZERO fsync to either ⇒ **per-checkpoint fsync count
  identical; no NEW asymmetry** (only the truncate-vs-retain already logged §2.2/C3). Record `round_dir_bytes` +
  `evictable_node_count()` per round.
- **§E.3 rigor inherited:** K=30/arm/variant, 2 warmup, Welch+CI+d(≥0.8)+Mann-Whitney, interleave+randomize (C9),
  single-arm-per-process RSS (C10), real-disk `target/bench-scratch` never tmpfs, 5 GiB ceiling,
  systemd 32G + `taskset -c 0-15`, both arms `EvictionConfig::without_memory_monitor()` (deterministic). §8 rule
  1-4 verbatim, gated first by SE5.
- **Bench changes:** `--eviction` flag enabling eviction on both arms + routing TREATMENT to
  `bench_immutable_checkpoint_with_eviction`; emit `evictable_node_count`. No Cargo change (`[[bench]]` already has
  the features).

## (7) Phased reversible migration (each ends GREEN: nextest ≥2474 + verify-formal-correspondence exit 0)
- **Phase 1 — TLA first:** add `LockFreeDurableCheckpointEviction.tla`+`.cfg`+`_Unsafe.cfg`; register in the verify
  script. Gate: SANY passes, RUN_TLC holds invariants, exit 0. Rollback: delete 3 files + 4 script lines.
- **Phase 2 — publisher (in-crate `cfg(test)`):** add `publish_immutable_snapshot_retaining_wal_with_eviction`
  (`cfg(any(test, bench-internals))`) + T1/T2 in a `#[cfg(test)] mod`. Gate: nextest ≥2476; verify exit 0; T1
  asserts registry char_len>0 + reopen-loses-nothing. Rollback: delete method + test mod.
- **Phase 3 — bench shims (`bench-internals`):** add `bench_immutable_checkpoint_with_eviction` +
  `bench_enable_eviction`; add the `bench-internals` disjunct to Phase-2 publisher. Gate: default nextest ≥2474
  (shims compiled out) + `cargo build --benches --features persistent-artrie,bench-internals` OK + verify exit 0.
  Rollback: delete 2 shims + the disjunct.
- **Phase 4 — bench arm + ledger §E:** extend `benches/lockfree_flip_benchmark.rs` (`--eviction` arm); append
  frozen §E. Gate: bench smoke (`run_smoke`) 1 round/arm; default nextest unaffected. Rollback: revert arm + §E.
- **Phase 5 — run + append RESULTS-E** (the measurement, per §10 runbook with `--eviction`). Not part of the merge gate.

## (8) Honest risks
1. **Reclaim correctness:** mitigated by NOT truncating — exact proven retain-WAL semantics; registry never in
   `RecoveredSet`; the watermark/synced_lsn-domain bug is caught by the inherited `debug_assert!` (`:464-471`).
   Lowest residual (dangerous line unmoved).
2. **Registry staleness vs CAS writers:** BY DESIGN — every durable write invalidates before its visibility CAS;
   eviction gates on `is_valid()` ⇒ a dirtied registry yields zero evictions (liveness, not safety).
3. **Eviction-vs-CAS-writer race:** overlay eviction primitive proven leak/UAF-free (`eviction_primitive_tests`
   `lockfree_cas.rs:1168-1260`, `EvictionWalkEBR.tla`); the new publisher only publishes the registry. NEW combo
   (force_eviction ‖ live insert_cas_durable) ⇒ **T2 is the runtime witness**; if it flakes, surface it.
4. **fsync-count asymmetry:** §E.2 — `update_disk_registry` adds zero fsync; per-checkpoint count identical;
   only truncate-vs-retain (already logged). Neutralized.
5. **`bench_enable_eviction` coupling:** duplicates `SharedCharARTrie::enable_eviction` construction — maintenance
   coupling (same crate), flagged.
6. **Stale doc comment** (`:557` "not Clone") — false; don't propagate.

### Critical files
- `src/persistent_artrie_char/persist.rs` (publisher beside `:548`; shim beside `:622`; T1/T2 beside `:1061`)
- `src/persistent_artrie_char/mod.rs` (gated `bench_enable_eviction`; invalidation contract `:1581-1619`)
- `formal-verification/tla+/LockFreeDurableCheckpointEviction.tla` (+ `.cfg`/`_Unsafe.cfg`)
- `benches/lockfree_flip_benchmark.rs` (eviction-ON arm; `--eviction`; `evictable_node_count` column)
- `docs/experiments/lockfree-flip-benchmark-ledger.md` (append-only frozen §E)
