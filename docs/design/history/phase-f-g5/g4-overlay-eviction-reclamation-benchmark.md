# Design: REVERSIBLE Overlay-Eviction Driver — make eviction-ON TREATMENT do REAL reclamation

**Repo:** `/home/dylon/Workspace/f1r3fly.io/libdictenstein` · 2026-06-02 · **Scope:** a REVERSIBLE,
`bench-internals`-gated overlay-eviction DRIVER + correspondence tests + TLA extension + append-only ledger
**§F**, so the eviction-ON benchmark's TREATMENT arm performs REAL in-memory reclamation of overlay nodes
instead of the §E structural no-op. **NOT the production flip** (`checkpoint()` + production eviction untouched;
owner-gated). **ZERO new `unsafe`** (reuses the proven Phase-D safe Arc/arc-swap primitive). Persisted from the
Plan-agent design.

## (1) Feasibility + the mechanism gap (code-cited)
**FEASIBLE reversibly, BUT only under explicit COLD-ONLY, NO-FAULT-IN scoping (must be benchmark-enforced).**
**Two independent trees:** OWNED `self.root` (raw-ptr SwizzledPtr, in-place `&mut`, EBR) vs OVERLAY
`lockfree_root: AtomicNodePtr<V>` (`Child=InMem(Arc)|OnDisk(SwizzledPtr)`, path-copy+root-CAS, arc-swap refcount).
`force_eviction`→`force_eviction_char` (`coordinator.rs:327`)→`evict_char_nodes` (`mod.rs:1392`)→
`evict_node_at_path` (`mod.rs:1722`) which short-circuits `CharTrieRoot::Empty => return false` (`:1736-1739`) —
in the overlay arm `self.root` is Empty (data in `lockfree_root`) ⇒ **no memory reclaimed** (the §E no-op,
stated at `mod.rs:1653-1663` + ledger:577-581). **Proven foundation reused:** `eviction_primitive_tests`
(`lockfree_cas.rs:1202-1259`) proves on the overlay node `with_child(k, Child::OnDisk(ptr))` + root-CAS (a)
publishes OnDisk, (b) no UAF (pre-evict reader snapshot still sees the subtree), (c) no leak (strong_count==1
after roots drop). The missing pieces per the primitive's own note (`:1186-1189`): "real per-node disk
locations" (the registry now supplies) + "fault-in-on-read" (scoped out, §3). ⇒ new component = ONE driver +
ONE gated accessor + tests + TLA + §F. No production path, no new unsafe, one-edit rollback.

## (2) The overlay-eviction DRIVER
Beside `evict_node_at_path`/`evict_char_nodes` in `mod.rs`, gated `cfg(any(test, feature="bench-internals"))`:
```rust
fn evict_overlay_node_at_path(&self, char_path: &[u32], disk_ptr: SwizzledPtr) -> OverlayEvictOutcome
//   { Evicted | RootCasLost(retry) | NotEvictable(skip) }; path-copy spine from overlay root, build
//   parent' = parent.with_child(edge, Child::OnDisk(disk_ptr)), rebuild ancestors InMem, root compare_exchange.
//   false/retry when: root-CAS lost (rebase), any spine slot already OnDisk, path missing, ptr not a disk loc.
//   NEVER a write lock, NEVER unsafe.
fn evict_overlay_nodes<V,S>(trie, nodes: Vec<(path_hash, Vec<char>, SwizzledPtr)>, max_rebase_retries)
//   -> (evicted, bytes_freed). Overlay analogue of evict_char_nodes; LEAF-FIRST (descending depth) bounded retry.
```
**Disk-location sourcing:** the registry already carries it — `serialize_char_node_to_disk` calls
`reg.register_char(path, ptr, len, depth, node_type)` (`persist.rs:1047-1055`), so each `EvictableCharNode`
(`disk_registry.rs:46-58`) has the full `Vec<char>` path + on-disk `SwizzledPtr`. Driver READS them; does not
compute. **Ordering (load-bearing):** evict runs AFTER a checkpoint published the registry (only checkpointed
nodes have a SwizzledPtr). **Reuse the coordinator selection (also the fairness control):** driver calls
`coordinator.force_eviction_char(budget, overlay_callback)` which refuses an invalidated registry (`is_valid()`
`:332` — a concurrent durable write that bumped `invalidate_eviction_registry` yields zero evictions =
liveness, not safety), runs `select_char_for_eviction` (coldest-first LRU, min-depth, batch-bounded), hands the
callback `Vec<(hash, path, ptr)>`. ONLY change vs owned = the callback body (overlay vs owned). **Leaf-first +
closed-subtree invariant:** sort candidates by DESCENDING depth so a node is evicted before any ancestor (parent
spine stays InMem when we evict; a later shallower candidate through an already-OnDisk slot → NotEvictable,
skipped — overlay analogue of `evict_node_at_path`'s on-disk-parent guard `:1759-1761`).
**Concurrent-writer safety (§2c):** per attempt pin `_epoch = enter_read()` (parity), `old_root =
lockfree_root.load()` (load_full, hazard-protected), walk cloning Arc per InMem hop (OnDisk/missing →
NotEvictable), rebuild spine bottom-up, `compare_exchange(&old_root, new_root)`; Ok→Evicted, Err→rebase+retry.
**No UAF** (Phase-D witness: reader holding old_root still sees the subtree; freed only when last version drops;
no raw ptrs; overlay needs no EBR). **No lost write** (loser-safe CAS: if a writer landed between load+CAS, our
CAS fails+rebases → can never overwrite a concurrent insert; bounded `max_rebase_retries≈4`; on exhaustion SKIP
= a missed eviction is liveness-only). **Reclaim accounting:** successful root-CAS drops superseded old_root →
evicted subtree Arc frees when unpinned; `bytes_freed` = registry `size_bytes` sum (nominal); peak-RSS pass =
physical witness.

## (3) Fault-in-on-read: ABSENT → COLD-ONLY scoping (the honest core)
**Finding (code-cited): fault-in is absent.** `find_in_lockfree_trie` (`lockfree_cas.rs:461-468`) and
`find_leaf_recursive` (`:534-538`) treat an OnDisk child via `as_in_mem()→None` ⇒ term reported ABSENT;
`build_path_recursive` (`:334-346`) treats OnDisk as `AlreadyExists`. So evicting a node later re-read/written
makes that term unreachable (silent correctness violation). **Decision: cold-only, no-fault-in — and SAY SO.**
A correct fault-in (read bytes via buffer manager → deserialize OverlayNode → CAS-install InMem racing
writers/re-evict, on the read path) is precisely what the flip rewrites + would add unsafe/disk-read on the hot
path — unacceptable for a reversible measurement. **Instead the driver evicts ONLY nodes never re-touched**,
enforced by a cold-prefix filter: writers insert a LIVE range; the evictor is fed only the spine of a disjoint
COLD range (inserted, checkpointed, never re-touched). Honest: measures real reclamation of cold subtrees under
concurrent write load (the production eviction scenario) WITHOUT claiming fault-in (the flip owns it). SF5 gates:
`faultin_count` MUST be 0 (any non-zero = a hot node wrongly evicted ⇒ ABORT). Cold selection = inside the
overlay callback, skip any candidate whose path isn't in the cold prefix (`path.starts_with(COLD)`), no
coordinator change.

## (4) Reversible bench accessor + rollback
```rust
// mod.rs beside bench_enable_eviction (~:1671)
#[cfg(feature = "bench-internals")]
pub fn bench_evict_overlay_cold_nodes(&self, budget_bytes: usize, cold_filter: impl Fn(&[char]) -> bool) -> usize {
    let coordinator = self.eviction_coordinator.as_ref()?; // -> 0 if None
    coordinator.force_eviction_char(budget_bytes, |cands| {
        let filtered = cands.into_iter().filter(|(_,p,_)| cold_filter(p)).collect();
        evict_overlay_nodes(self, filtered, 4)
    }).0
}
```
Needs only `&self` (overlay path is all `&self`) ⇒ callable from the checkpointer thread; coordinator already
installed by the §E `bench_enable_eviction`. Reclamation driven SYNCHRONOUSLY from the checkpointer (deterministic,
off the writer path; `bench_enable_eviction`'s background no-op callback unchanged). **Rollback (one edit each):**
delete `bench_evict_overlay_cold_nodes`; delete `evict_overlay_nodes`+`evict_overlay_node_at_path`; remove the §F
driver call+cold-filter from the bench; delete §F tests + TLA + its verify-script lines; (ledger §F append-only).
`checkpoint()`+production untouched; only safe APIs ⇒ unsafe-inventory gate stays exit-0.

## (5) Safety: no-lost-write + no-UAF + tests + TLA
**No-lost-write:** eviction converts an in-mem subtree to an OnDisk ref whose bytes were written by the PRIOR
checkpoint (registry SwizzledPtr → `serialize_char_node_to_disk` output) + WAL RETAINED (TREATMENT publisher
records `checkpoint_lsn=watermark`, no truncate). ℓ≤watermark → in checkpoint image (durable OnDisk target);
ℓ>watermark → replayed from retained WAL; evicts only registry-resident (checkpointed/durable) nodes; coordinator
refuses invalidated registry; loser-safe CAS. **The dangerous line (destructive truncate) is NOT introduced.**
⇒ NoLostWriteUnderLockFreeCommit + Order-A + watermark + registry-invalidation + EBR-no-UAF preserved.
**No-UAF:** pure Arc/arc-swap; readers pin `load_full()`; superseded root keeps subtree alive until last reader
drops; evicted subtree frees by refcount (Phase-D witness). Zero raw ptrs, zero new unsafe.
**Correspondence tests** (`#[cfg(test)] mod overlay_eviction_driver_correspondence`, real-disk `target/test-tmp`):
- **OE1 `cold_eviction_under_concurrent_writers_reopens_losing_nothing`** (headline): insert COLD `c-*` + LIVE
  `w-*`; checkpoint-with-eviction; N `insert_cas_durable` writers on fresh `w2-*` ‖ repeated
  `bench_evict_overlay_cold_nodes(budget,|p|p.starts_with('c'))`; assert evicted>0 (REAL — 0 with old no-op),
  cold terms not re-read (cold contract), reopen ⇒ EVERY acked term (`c-*`,`w-*`,`w2-*`) present.
- **OE2 `reader_concurrent_with_overlay_eviction_sees_consistent_snapshot`** (no-UAF): reader loops
  contains_lockfree on LIVE ‖ evictor reclaims COLD; no panic/UAF (run under sanitizers); LIVE monotone-present.
  + a loom variant in `tests/persistent_lockfree_overlay_loom.rs` (2 writers+1 evictor+1 reader, 2-level tree).
- **OE3 `evict_then_reload_returns_exact_values`** (SE5 unit analogue): checkpoint→evict cold→reopen→cold values
  byte-identical (registry SwizzledPtr→durable bytes correspondence).
- **OE4 `evictor_root_cas_loser_never_clobbers_insert`** (proptest, extend overlay proptest): random insert+evict
  interleavings; post-run acked set == inserted set.
**TLA:** `EvictionWalkEBR.tla` is owned-tree EBR; the NEW interaction is evictor-root-CAS ‖ writer-root-CAS (CAS
arbitration). NEW spec **`OverlayEvictionCas.tla`** (+`.cfg`/`_Unsafe.cfg`): vars `root`,
`linkedInMem`,`onDisk`,`live`,`cold`,`acked`; `WriterCas` (path-copy+root-CAS, +acked), `EvictCas(n∈cold∩linkedInMem)`
(succeed XOR lose-to-writer/rebase), `Reclaim`. Invariants: **`NoLostAck == \A l∈acked: reachable(l)`**,
**`EvictTouchesOnlyCold == onDisk ⊆ cold`**, **`ReachableNotFreed`** (no-UAF). `_Unsafe.cfg` lets EvictCas fire on
`live` → violates EvictTouchesOnlyCold/NoLostAck (cold-only gate necessary). CONSTANTS `Nodes={n1,n2,n3}`,
`Lsns={1,2}`, `live={n1}`, `cold={n2,n3}`, CHECK_DEADLOCK FALSE. Register in verify-script SANY/RUN_TLC/_Unsafe lists.

## (6) Benchmark §F (append-only, frozen at its own persist time; do NOT edit §1-§11/§E)
Both arms do REAL eviction under matched pressure: CONTROL owned-tree `force_eviction`; TREATMENT overlay driver.
**peak RSS finally becomes a meaningful TREATMENT metric.**
- **HF1 (throughput):** Immediate+eviction-ON+real-reclaim, disjoint, T vs C, two-sided Welch + CI + d.
  **Expectation (anti-hindsight): the +533% NARROWS** (T now pays real reclaim cost C already paid). A narrowing
  is EXPECTED, not a regression — the question is whether T STILL wins + frees memory.
- **HF2 (the now-meaningful metric):** (a) TREATMENT §F peak RSS < TREATMENT §E peak RSS (§E was 1,318,888 KiB,
  NO reclaim — §F must beat it) by > noise; (b) TREATMENT RSS ≤ CONTROL RSS. Supported iff both.
- **HF3 (reclamation effectiveness):** `overlay_reclaimed_nodes` > 0 AND ≈ matched to CONTROL reclaimed count
  under same budget/cadence (fairness witness).
- **Secondary vetoes (gated FIRST by SF5):** SF1 pause T≤C; SF2 p99/p999 ≤1.10×; SF3 RSS ≤1.25×; SF4 contended
  not sig worse. **SF5 CORRECTNESS (ABORT on fail):** (i) reopen-exact both arms; (ii) `faultin_count==0` T
  (cold-only held); (iii) `overlay_reclaimed_nodes>0` T (no silent no-op). Enforced by `--evict-real` smoke +
  OE1-OE4.
- **§F.2 fairness:** matched budget+cadence (one evict call per checkpoint round AFTER registry publish, both
  arms); same `EvictionConfig::without_memory_monitor()`, min_depth, batch_size, coldness (both via
  `force_eviction_char`/`select_char_for_eviction`); only the callback differs. Per-write fsync unchanged+equal.
  **The evictor does ZERO disk I/O on BOTH arms** (cold-only → no fault-in read-back; just an in-mem slot swap)
  ⇒ NO new fsync/read asymmetry (cleaner than §E); only the §2.2/C3 truncate-vs-retain remains.
- **§F.3 metrics:** append TRAILING cols (analyze script maps leading 18 by name, ignores extras): **col20
  `overlay_reclaimed_nodes`**, **col21 `evict_bytes_nominal`**, **col22 `faultin_count`(==0)**. peak_rss_kib (col16)
  = physical witness via single-arm RSS pass. cas_retries now includes evictor rebases.
- **§F.4 rigor:** K=30 + 2 warmup; Welch+CI+d(≥0.8 floor)+MWU; interleave+randomize (C9); single-arm RSS (C10);
  real-disk `target/bench-scratch` never tmpfs; 5 GiB ceiling; systemd 32G + `taskset -c 0-15`; both Immediate +
  without_memory_monitor. SF5 first.
- **§F.5 decision rule:** SF5-gated, then: **1 PROCEED** (T throughput ≥C ∧ HF2 ∧ HF3 ∧ no SF1-4 veto); **2
  PROCEED-WITH-CAVEAT** (throughput narrows but ≥0 ∧ HF2 holds); **3 DON'T-FLIP no-benefit** (T ties/loses ∧/∨ HF2
  fails — overlay doesn't free memory ⇒ §E +533% was the no-op artifact); **4 DON'T-FLIP regression** (SF1/2/4 veto).
- **§F.6 runbook:** build release benches `--features persistent-artrie,bench-internals`; `--evict-real` smoke
  (SF5); `--measure --evict-real --variant {disjoint,contended}` ONCE each; `--arm {control,treatment}` RSS pass;
  `analyze_lockfree_flip.py` once; compare T §F-RSS vs §E-RSS (HF2a). All wrapped systemd + taskset + tee.
- **§F.7 bench changes:** `--evict-real` flag: both rounds, after each checkpoint publishes the registry, call
  matched eviction (CONTROL `force_eviction(budget)`; TREATMENT `bench_evict_overlay_cold_nodes(budget,
  cold_filter)`); insert a fixed COLD prefix set once at round start, checkpoint, never touch; cols 20-22 +
  `sf5_correctness_check`. No Cargo change.

## (7) Phased migration (each GREEN: nextest ≥2476 + verify-formal-correspondence exit 0)
1. **TLA first:** `OverlayEvictionCas.tla`+`.cfg`+`_Unsafe.cfg`; register SANY/RUN_TLC/_Unsafe. Gate: SANY ok,
   RUN_TLC holds NoLostAck/EvictTouchesOnlyCold/ReachableNotFreed, `_Unsafe` FAILS, exit 0. Rollback: del 3 files + 3 lines.
2. **Driver (in-crate cfg(test)):** `evict_overlay_node_at_path`+`evict_overlay_nodes` + OE1-OE4. Gate: nextest
   ≥2480 (+4); verify exit 0; OE1 asserts evicted>0 + reopen-loses-nothing. Rollback: del 2 fns + test mod.
3. **Bench accessor (bench-internals):** `bench_evict_overlay_cold_nodes`. Gate: default nextest ≥2476 (out of
   default build) + `cargo build --release --benches --features persistent-artrie,bench-internals` ok + verify exit
   0. Rollback: del accessor.
4. **Bench arm + §F:** `--evict-real` (matched eviction + cold partition + cols20-22 + sf5_correctness_check);
   append §F. Gate: `--evict-real` smoke 1 round/arm SF5 PASS (reopen-exact, faultin==0, reclaim>0); default nextest
   unaffected; verify exit 0. Rollback: revert arm + strike §F.
5. **Run + append §F RESULTS** (§F.6). Not part of merge gate.

## (8) Honest risks
1. **Evictor‖writer root-CAS races (highest):** both contend on `lockfree_root` CAS; contended variant compounds
   retries → eviction liveness may drop (skipped on exhaustion). Mitigation: bounded retries, cold-only paths
   (disjoint variant rarely collides), OE2/OE4+loom. Residual: a missed evict is liveness-only (loser-safe) —
   reported as lower reclaimed-count, doesn't gate the disjoint primary.
2. **Fault-in (designed AROUND):** absent (§3a); cold-only + SF5(ii) faultin==0 + SF5(i) reopen-exact ABORT rather
   than emit a verdict. Real fault-in stays owner-gated (the flip).
3. **Disk-read asymmetry ELIMINATED by scoping:** cold nodes never faulted ⇒ evictor zero disk I/O both arms.
4. **Real reclamation likely NARROWS the +533% — that IS the point.** HF1 pre-registers narrowing as EXPECTED; the
   deliverable is whether T still wins + frees memory (HF2/HF3), not preserving the inflated number. HF2 fail ⇒
   Branch 3 DON'T-FLIP (measurement doing its job).
5. **RSS-as-truth caveat:** VmHWM + deferred Arc drop under readers + glibc arena retention can mask reclamation.
   Mitigation: HF2a compares T-§F vs T-§E (same allocator/workload, isolates the delta) AND HF3 reports the
   logical reclaimed count (allocator-independent).
6. **Maintenance coupling:** `evict_overlay_nodes`/`evict_overlay_node_at_path` mirror the owned shapes (same crate;
   flagged).
7. **If even cold-only is infeasible** (doesn't appear so): fall back to a single `Arc::strong_count` correspondence
   test proving the driver reclaims one cold subtree under one writer (no benchmark), documenting throughput-under-
   real-reclaim deferred with the flip.

### Critical files
- `src/persistent_artrie_char/mod.rs` (`evict_overlay_node_at_path`+`evict_overlay_nodes` ~:1722/:1392;
  `bench_evict_overlay_cold_nodes` ~:1671)
- `src/persistent_artrie_char/lockfree_cas.rs` (Phase-D primitive ~:1202-1259; fault-in gap :461-468/:534-538;
  OE1-OE4 mod)
- `src/persistent_artrie_core/eviction/coordinator.rs` (`force_eviction_char` :327 reused; registry :78/:379)
- `benches/lockfree_flip_benchmark.rs` (`--evict-real` arm; matched eviction :292/:441; cold partition; cols20-22 +
  sf5_correctness_check)
- `docs/experiments/lockfree-flip-benchmark-ledger.md` (append-only §F) + `formal-verification/tla+/OverlayEvictionCas.tla`
  (+`.cfg`/`_Unsafe.cfg`; register in `scripts/verify-formal-correspondence.sh` :250/:299/:315)
