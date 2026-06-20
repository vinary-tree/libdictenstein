# f7 Overlay-Eviction — COMPLETION plan (prefix-fault + Phase 7 activation + Phase 8)

> Phases 0–6 DONE+verified (2705 green, formal gate exit 0, zero new unsafe). This is the
> remaining work: Phase A (prefix-fault fix), Phase 7 (budget + production activation),
> Phase 8 (verification). Plan-agent design; to be red-teamed to convergence then implemented BY HAND.

## Verified baseline (do not redo)
- Shared collect walks SKIP OnDisk: `overlay_navigate` (flip.rs:410, `as_in_mem()?`), `overlay_collect_finals`
  (flip.rs:440), `overlay_collect_with_values` (flip.rs:475). `LockFreeOverlay` (flip.rs:184) is `: Sized+'static`.
  Both byte (overlay_write_mode.rs:933/944) + char (overlay_read.rs:58/71 via prefix_api.rs:26/62/122/194) route here;
  arena-prefix variants reuse overlay_iter_prefix* → a fix in the 2 collect walks propagates to ALL prefix entry points.
- `OverlayFaulter::fault_overlay_slot` (faulter.rs:60): single-level lazy load, children stay OnDisk, writes nothing,
  advances no watermark, None on error. Both variants impl it. EXACTLY the read-only primitive Phase A needs.
- `OverlayEvictable: OverlayFaulter` (evict.rs:94); 1c guard in evict_overlay_node_at_path (evict.rs:219). Driver
  `evict_overlay_nodes` is #[cfg(test,bench-internals)] (char mod.rs:1922, byte overlay_fault.rs:188).
- Checkpoint tails: char publish_immutable_snapshot_retaining_wal_with_eviction (persist.rs:720), byte
  publish_overlay_snapshot_retaining_with_eviction (overlay_checkpoint.rs:405): publish→verify→update_disk_registry→
  WAL-Checkpoint+sync (no truncate). NEITHER calls force_eviction yet.
- Production force_eviction callers use OWNED no-op: char mod.rs:2218 (evict_char_nodes) + async mod.rs:2134; byte
  shared_trait_impl.rs:361 (select+count only) + async :298 (evict_node_at_path).
- Coordinator force_eviction_char/_bytes hardcode batch_size as max_count (coordinator.rs:340/385), drop registry
  lock before callback (:354/:398); F: Fn (NO 'static bound, :327-329 → can capture &self). select_*_for_eviction
  takes max_count, stops at target_bytes OR max_count (disk_registry.rs:347-356).
- Registry: only total_size_bytes() (sums BOTH maps); per-entry size_bytes = on-disk data.len(). Per-map accessors NEW.
- OverlayEvictionStale.tla models USE_GUARD/NoStaleEvict/_Unsafe; gated (verify script 252/307/363). Invalidation
  chokepoints exist (byte persistence_api.rs:175, char wal_helpers). No massif harness. evict_overlay_nodes byte+char only (no vocab) → un-gate ripple-free.

## Phase A — prefix-fault fix (Phase-7 PREREQUISITE; data-loss-in-observation)
Once eviction is live, iter_prefix*/arena variants under-report evicted subtrees (byte+char, shared core).
FIX: `LockFreeOverlay<K,V,S>: OverlayFaulter<K,V> + Sized + 'static` (both variants impl OverlayFaulter; Send+Sync ok).
Make the 3 walks fault OnDisk READ-ONLY (load via fault_overlay_slot + recurse into the TRANSIENT loaded Arc;
NEVER install/CAS — enumeration must not bloat the overlay or un-evict). overlay_collect_finals + _with_values become
`&self` methods (call self.fault_overlay_slot); callers in overlay_collect_units/_with_values switch Self::→self.;
recursive self-calls too. navigate: `Child::OnDisk(ptr) if !ptr.is_null() => self.fault_overlay_slot(ptr)?` else None.
collect: OnDisk child → fault_overlay_slot → Some(loaded) recurse / None continue (fail-closed = point-read parity).
KEY PROPERTY: no install, no CAS, no watermark — strictly weaker (safer) than find_leaf_faulting (which installs).
Considerations: (a) static→method mechanical, no external sig change; (b) recursion depth unchanged (same key-length
recursion, "depth-safe at production point-read bound"); (c) fail-closed None→skip (matches point read); (d) CONCURRENCY
pure-read (frozen Arc snapshot OR immutable durable image; no UAF via Arc refcount; vs writer = our snapshot frozen at
load; deadlock: NO trie lock held + NO WAL append — same safe side as the shipping find_leaf_faulting point read, NOT
the "75-min hang" write-path-present-hoist-under-lock side; UPDATE overlay_read.rs:22-30 "DO NOT fault" doc to scope it);
(e) navigate alone insufficient — subtree below prefix also faults in the collect walks (bounded: only nodes the query visits).
DISCRIMINATING TEST (char+byte, () + u64): insert terms under prefix P spanning ≥2 levels + a sibling outside P;
checkpoint-with-eviction (stamps); evict the subtree (min_eviction_depth=0 so interior nodes go OnDisk, assert evicted>0);
iter_prefix(P) MUST return ALL terms (faulted) not a subset + scoping preserved + returned set == inserted set exactly.
**A NEEDS a focused red-team** (concurrency/deadlock argument — wrong lock claims have shipped before; v3's "CAS keyed on
child Arc" was FALSE). NO TLA (read-only, no new CAS, NoStaleEvict unaffected). DOES need the C.2 loom (prefix-fault‖evict).

## Phase 7 — budget + PRODUCTION ACTIVATION (data-loss-critical)
- **7.1 config:** `resident_budget_bytes: Option<usize>` on EvictionConfig (None=back-compat=unbounded). Add to all
  struct-literal ctors (compiler-enforced). Tests use ..Default → unaffected. Additive/inert.
- **7.2 per-map accessors:** disk_registry byte_resident_estimate_bytes / char_resident_estimate_bytes =
  Σ (size_bytes + STRUCT_OVERHEAD_{BYTE,CHAR}) over the respective map ONLY (no cross-contamination). Keep
  total_size_bytes + per-entry size_bytes as-is. Coordinator thin pass-throughs. Additive.
- **7.3 uncapped arity:** force_eviction_char_uncapped / force_eviction_bytes_uncapped = copies of force_eviction_*
  EXCEPT max_count=usize::MAX + the min_eviction_depth-floor-unreachable log::warn (warn ONLY when selected_bytes <
  target_bytes AND selected_bytes < eligible_total — clamp to avoid spurious warn when budget > whole registry).
  Shared select_* sig UNCHANGED (async + public force_eviction keep batch_size). Additive/inert.
- **7.4 un-gate driver:** remove #[cfg(test,bench-internals)] from char evict_overlay_nodes (mod.rs:1922) +
  OverlayEvictOutcome re-export + byte evict_overlay_nodes (overlay_fault.rs:188). bench_* enablers STAY gated. Ripple:
  K-generic, byte+char only, no test-deps in body; move any gated `use` into body or #[allow(unused_imports)]. Green (widened availability only).
- **7.5 GO-LIVE wiring:** char tail (persist.rs:720) AFTER update_disk_registry + WAL Checkpoint sync (so victim disk_ptrs
  durable): if let Some(budget)=coordinator.resident_budget_bytes() { resident=coordinator.char_resident_estimate_bytes();
  if resident>budget { coordinator.force_eviction_char_uncapped(resident-budget, move |nodes| evict_overlay_nodes(self,
  nodes, MAX_REBASE_RETRIES)) } }. Byte tail (overlay_checkpoint.rs:405) twin (byte_resident_estimate + force_eviction_bytes_uncapped
  + byte evict_overlay_nodes, &[u8] paths). Callback captures &self (Fn, no 'static; runs inline synchronously — confirmed
  coordinator.rs:327). Add coordinator resident_budget_bytes() accessor (config is private; one source of truth via the cloned Arc).
  SWAP public force_eviction callers (char mod.rs:2218 + byte shared_trait_impl.rs:361) + BOTH async callbacks (char :2134,
  byte :298) from evict_char_nodes/evict_node_at_path (owned no-op) → evict_overlay_nodes (safe: owned no-ops under
  route_overlay; no owned+eviction prod config — vocab is owned but eviction-OFF). const MAX_REBASE_RETRIES=4.
  UPDATE the 2 force_eviction tests (char mod.rs:2685-2695 "no-op because owned empty" → now evicts overlay or 0). GO-LIVE.
- **7.6 STRUCT_OVERHEAD:** analytic floor (Arc ctrl block 16 + version 8 + serial_disk_ptr 8 + inline-array slack +
  prefix Arc alloc header + atomics — the in-MEM residual NOT in on-disk size_bytes), refined by C.1 massif. Per variant
  (K::Unit 1 vs 4, MAX_PREFIX_LEN 12 vs 6) + per value (() vs u64).

### Concurrency safety of the ACTIVATED tail (re-derived for live firing)
Tail runs under checkpoint_lock but writers DON'T take it → races writers. Safety = 3-part composition:
1. **1c durable_stamp guard (CORRECTNESS):** evict X to disk_ptr ONLY IF X.durable_stamp()==disk_ptr.to_raw(); a
   post-stamp overwrite path-copies X + all ancestors to fresh stamp-0 nodes → evictor reaches stamp-0 → NotEvictable.
2. **root CAS closes post-guard window:** writer after guard → root advances → our CAS fails → RootCasLost → rebase →
   re-walk reaches stamp-0 → NotEvictable. Loser-safe (publish nothing on lost CAS).
3. **registry invalidation (coarse early-out, NOT correctness):** durable write invalidates before its visibility CAS;
   force_eviction_*_uncapped returns (0,0) on dirtied registry. Liveness only (the stamp catches mid-list overwrites).
Ordering requirement: tail evict AFTER update_disk_registry (selects the just-published registry) AND after WAL Checkpoint
sync (victim ptrs durable) — both at step (4). Composition with prefix-fault (pure-read, frozen snapshot or immutable
durable image) + 3-way writer/faulter (all loser-safe root CAS). Un-gate ripple: build-availability only, no new unsafe.

## Phase 8 — verification
- **C.1 massif** (example under target/, NEVER tmpfs; assert path not tmpfs): N-million keys, resident_budget_bytes=B,
  periodic checkpoint (tail fires), byte+char × ()+u64; peak RSS ≈ B+margin vs unbounded control. CALIBRATE
  STRUCT_OVERHEAD = (measured_resident_RSS − on_disk_Σ)/Nc per variant+value (massif --pages-as-heap for true RSS).
  **margin MUST cover the write-burst TRANSIENT peak** (superseded path-copy versions free LAZILY on Drop, off-registry);
  calibrate margin from peak-during-burst not quiescence. Document command + numbers in docs/benchmarks/.
- **C.2 loom** (extend persistent_lockfree_overlay_loom.rs): (1) checkpoint-tail-evict‖writer (the R1 PRIMARY: stamp
  Release/Acquire vs with_child reconstruction; writer value never lost; evicted XOR overwritten never stale-evict);
  (2) prefix-fault‖evict (read-only walk reads frozen-inmem OR equal-durable, no UB/drop, no CAS from walk side);
  (3) 3-way evict‖write‖fault (no-UAF, no-lost-write, loser-safe). TLA=abstract safety; loom=ordering; massif=liveness.
- **C.3 M-7a doc:** budget enforceable ONLY over cold/quiescent set (1c guard refuses to evict hot/overwritten nodes —
  inherent). libgrammstein counter hot-set: streaming import has bounded hot window << cold imported set → CONVERGES
  (cold reclaim ≥ growth); the limit bites only hot-set>budget steady state. Bench must MEASURE hot-set vs budget.
- **C.4 formal+unsafe+parity:** RUN_TLC=1 verify-formal-correspondence.sh exit 0 (re-run, confirm _Unsafe still FIRES);
  unsafe-inventory set-equality zero-delta; byte OE twins for EVERY char OE test incl. the A.5 prefix test.

## Phase ordering (each keeps cargo nextest --features persistent-artrie green)
A (prefix fault + tests, inert) → 7.1 config → 7.2 accessors+overhead-placeholder → 7.3 uncapped arity → 7.4 un-gate →
7.5 wire tail + swap callers + update 2 force_eviction tests [GO-LIVE] → 8 (massif calibration bakes real overhead + loom + docs + formal/unsafe re-run + byte parity).
Reversibility: 7.1-7.4 additive/inert; 7.5 activation but resident_budget_bytes=None default keeps tail OFF (opt-in);
the always-on change is the force_eviction caller swap (only matters with a coordinator + populated registry).

## Residual risks
- R1 (PRIMARY): set_durable_stamp(Release) into the live node during serialize, now firing live via the tail. Benign
  arg holds; C.2 loom #1 is the witness. Fallback: mechanism B (batch-CAS) — no evidence needed yet.
- R2 (budget accuracy not correctness): STRUCT_OVERHEAD approximate (2-tier ChildStore count-dependent); margin+massif absorb. Transient-burst margin harder.
- R3 (force_eviction test-behavior change): the 2 "no-op because owned-empty" tests MUST update in 7.5 (green→red if missed = caught immediately).
- R4 (liveness): 1c guard skips overwritten → only cold reclaimed; bounded-hot-window converges, hot-set>budget cannot (M-7a inherent). Bench confirms hot window << budget.
- R5 (prefix-fault deadlock regression): read-only no-lock fault = same side as shipping point read; the claim most worth red-teaming (A.6).

## Red-team scope: SKIP (mechanical, compile+suite covers) = 7.1/7.2/7.3/7.4 + formal-rerun/unsafe/byte-twins.
## NEEDS red-team = Phase A (concurrency/deadlock), 7.5 (go-live concurrency = the heart), C.1/C.2 (R1 loom + transient margin).

# === v2 (completion-plan red-team round-1 resolutions) ===
Round-1 VERIFIED-SOUND (no change): Phase A deadlock (prefix fault = arena_manager.read() only, released
per node, no WAL append, no trie/checkpoint lock held across the walk, no cycle with checkpoint's AM→BM
nest; the 75-min hang [git 0fef74d] was a write-path present-hoist fault under the WAL/durability chain —
different chain); Phase A durable-image immutability (arenas append-only within a run — clear_for_loading
only at open; checkpoint allocates NEW slots; in-place update only fixes a slot THIS serialize just
allocated; ArenaManager::read returns &[u8] into the in-MEMORY arena copy not the mmap → no torn read);
7.5 composition (callback Fn no-'static runs inline → &self capture sound; tail after update_disk_registry
+ WAL sync selects the just-published gen; checkpoint_lock serializes N's tail vs N+1; 1c guard+root CAS
handle writers; evict_overlay_nodes = pure Arc-refcount ZERO unsafe); 7.4 un-gate ripple-free
(hash_char_path/LruRegistry/OverlayEvictOutcome all non-gated; uses are in-body). 5 must-fixes:

## v2 FIX 1 (MAJOR — budget unit mismatch). select_*_for_eviction accumulates ON-DISK size_bytes
(disk_registry.rs:351/393) and stops at target_bytes; but the tail target is RESIDENT (size_bytes+
STRUCT_OVERHEAD) → passing target=resident−budget systematically OVER-evicts by ≈resident/on_disk (safe
for OOM but churns). FIX: the uncapped budget path selects in RESIDENT units — add a resident-aware
selection (new select_char_for_eviction_resident / select_for_eviction_resident, OR an `overhead: usize`
param) that accumulates `size_bytes + STRUCT_OVERHEAD` per node, stopping at the resident target. Leave
the SHARED select_*_for_eviction (on-disk units, batch_size) UNCHANGED for the async/public-batch path.

## v2 FIX 2 (MAJOR — caller-swap is unconditional; owned+eviction IS reachable). Ineligible-V tries
(overlay_eligible_v false → lockfree_root==None) + kill_switch_to_owned tries run owned-mode with
eviction enable-able; the unconditional swap to evict_overlay_nodes silently STOPS owned-tree eviction
there (evict_overlay_node_at_path → overlay_root_slot()==None → NotEvictable → (0,0); graceful, no panic
— but a memory-bound regression). FIX: GATE the callback on route_overlay() — route_overlay() →
evict_overlay_nodes (overlay); else → evict_char_nodes/evict_node_at_path (owned, PRESERVE existing
owned-tree eviction). Apply at ALL swap sites (char force_eviction mod.rs:2218 + async :2134; byte
force_eviction shared_trait_impl.rs:361 + async :298; the checkpoint-tail callback). UPDATE the byte
force_eviction tests too (byte's public force_eviction return-semantics change from candidate-count to
evicted-count), NOT just the 2 char tests (R3).

## v2 FIX 3 (MAJOR — transient = correctness-of-PURPOSE, an OOM path, not just accuracy). Superseded
path-copy versions are off-registry, freed lazily on Arc drop; between checkpoints RSS = registry_total +
transient, and the tail only evicts to registry_total−budget → post-tail/inter-checkpoint RSS can exceed
budget by the inter-checkpoint transient → a sustained write burst can blow past budget+margin = the OOM
the feature must prevent. FIX (Phase 8/C.1 + doc): the massif bench MUST measure the INTER-CHECKPOINT
TRANSIENT PEAK (RSS during a write BURST, not at quiescence); the margin MUST cover it; calibrate
STRUCT_OVERHEAD on a HIGH-FAN-OUT (not average) workload (the Inline-tier const is exact for ≤4 children
but the Heap-tier residual grows with fan-out). DOCUMENT: checkpoint cadence bounds the transient (more
frequent checkpoints → smaller transient → tighter budget adherence); operators couple cadence to write
rate. The budget bounds the POST-CHECKPOINT resident; the live peak = budget + transient(cadence).

## v2 FIX 4 (MAJOR-liveness, N1 — uncapped tail latency under checkpoint_lock). max_count=usize::MAX +
a large overshoot (first checkpoint after a burst, worsened by FIX-1's over-eviction) does thousands of
O(depth) spine path-copies + root-CASes INLINE under checkpoint_lock → blocks the next checkpoint + storms
root-CAS vs live writers. FIX: add a CONFIGURABLE per-tail eviction cap (e.g.
`resident_budget_eviction_cap: Option<usize>` on EvictionConfig; None = uncapped/budget-precise with a
documented one-time initial-burst latency; Some(n) = evict ≤ n nodes/checkpoint, CONVERGING over
checkpoints). Co-tune with FIX 3: smaller cap → slower reclaim → larger transient → cap MUST be ≥
per-checkpoint growth or the budget never converges (the v3 batch_size-starvation lesson). Keep the
eviction INSIDE checkpoint_lock (round-1-verified safe); the cap bounds the lock-held latency. The
uncapped select (FIX 1) feeds this; the cap limits how many of the selected are evicted per pass.

## v2 FIX 5 (N3 compile-verify + N2 doc/test). N3: LockFreeOverlay: OverlayFaulter adds a Send+Sync
supertrait obligation (OverlayFaulter: Send+Sync, faulter.rs:52) onto every LockFreeOverlay impl — COMPILE-
VERIFY both variants (and any non-Send S block-storage param) satisfy it BEFORE relying on the bound; if
a non-Send S breaks it, scope the bound (e.g. where S: Send+Sync) or route the loader differently. N2:
DOCUMENT the transient-fault re-fault cost — Phase A faults OnDisk children TRANSIENTLY (never installs),
so iter_prefix over a heavily-evicted subtree re-faults O(evicted-nodes) arena reads PER CALL (no caching;
correct, not a bloat); the discriminating test (A.5) must assert TERMINATION (+ correctness), and the
massif/throughput note documents the enumerate-heavy+budget-tight cliff.

## STATUS: v2 — RE-RED-TEAM (the 5 fixes: resident-unit select, route_overlay-gated swap, transient-as-
## OOM-path + high-fan-out calibration, configurable tail cap, Send+Sync compile-verify + re-fault doc).

# === v3 (completion-plan red-team round-2 resolutions — clarifications; mechanism CONFIRMED sound) ===
Round-2 CONFIRMED SOUND (do NOT re-litigate): FIX-2 route_overlay gating + gate-staleness benign (wrong
evictor for one pass = loser-safe no-op, never data loss) + tail-gate placement; FIX-5 Send+Sync ALREADY
discharged (BlockStorage: Send+Sync+'static at block_storage.rs:141 → every S is Send+Sync; both LockFreeOverlay
impl types already impl OverlayFaulter → the supertrait bound is satisfiable, the "non-Send S" branch is dead
code); FIX-3 transient=OOM-path sound as scoped (Phase-8/doc); all non-regression (1c guard, no-lost-write,
registry-as-resident-set, owned-correspondence preserved). 4 must-fixes (clarifications):

## v3 FIX A (was BLOCKER) — collapse FIX-1+FIX-4+7.3 into ONE resident-aware CAPPED select (no _uncapped arity).
The evict driver (char mod.rs:1947, byte overlay_fault.rs:201) evicts the ENTIRE list it is handed — there is
NO post-select cap. So the cap can ONLY be `max_count` at select_*_for_eviction (the loop's two breaks:
`if result.len() >= max_count` BEFORE push, `if total_bytes >= target_bytes` AFTER push — disk_registry.rs:348/354).
FIX-1's max_count=usize::MAX (via 7.3 _uncapped) and FIX-4's cap are mutually exclusive. RESOLUTION: DELETE the
separate force_eviction_*_uncapped arity. ONE budget arity force_eviction_*_resident(target_bytes, overhead,
max_count, callback) that calls select with BOTH: `max_count = resident_budget_eviction_cap.unwrap_or(batch_size)`
AND the resident accumulation (FIX D). The two breaks COMPOSE (single forward pass): byte-target stops early when
the cold set < target (under-evict → converges next checkpoint, benign); count-cap stops early when overshoot >
cap (bounded latency → converges over checkpoints). Vec::with_capacity(max_count.min(candidates.len())) is
OOM-safe even at usize::MAX. Drop the "uncapped feeds, cap evicts" wording entirely.

## v3 FIX B (was MAJOR C) — cap in NODES, default None ⇒ batch_size (NOT usize::MAX).
The cap is a NODE count (= max_count), which bounds the per-checkpoint O(depth) spine-rebuild + root-CAS COUNT =
the checkpoint_lock-held latency FIX-4 targets. `resident_budget_eviction_cap: Option<usize>` on EvictionConfig;
None ⇒ fall back to the existing validated `batch_size` (default 256, validated [16,4096] at config.rs:202), NOT
usize::MAX — so the OUT-OF-THE-BOX tail does BOUNDED work per checkpoint and converges as long as cross-checkpoint
cold-reclaim ≥ growth (the M-7a inherent condition). Only an operator who MEASURED their burst raises it. C.1
massif must REPORT measured per-checkpoint growth-at-target-write-rate so the operator sets the cap from data, not
folklore. (Resolves the v3-starvation-redux: bounded default + documented derived floor, not an unguarded footgun.)

## v3 FIX C (was MAJOR D) — resident-select as an `overhead: usize` PARAM on the existing select, NOT a fork.
Add `overhead: usize` to select_char_for_eviction / select_for_eviction (one-line accumulator change
`total_bytes += node.size_bytes + overhead`); existing async/public callers pass `overhead = 0` (on-disk units,
behavior UNCHANGED); the budget path passes `overhead = STRUCT_OVERHEAD_{variant}`. A PARAM (not a forked
select_*_resident copy) so the LRU-coldness ordering + min_depth filter CANNOT drift (the evict↔find_leaf_faulting
lockstep-hazard class). SINGLE SOURCE OF TRUTH for the per-(variant,value) overhead const: the SAME const feeds
both the select accumulator (here) AND the *_resident_estimate_bytes accessor (7.2) — else target (resident−budget,
computed with the accessor's overhead) and accumulation (select's overhead) land in different units, re-creating
the FIX-1 mismatch. (Supersedes v2's "shared select sig UNCHANGED": adding a param changes the SIG but preserves
BEHAVIOR for the overhead=0 callers — the red-team explicitly prefers the param over the fork.)

## v3 FIX D (was MAJOR E.1) — citation corrections for the hand-implementation (accuracy before coding):
- byte public force_eviction (shared_trait_impl.rs:375) currently calls coordinator.force_eviction (NO callback,
  select+count only, coordinator.rs:303-306) → swap to coordinator.force_eviction_bytes (the callback arity,
  coordinator.rs:372) + the route_overlay-gated callback (return-semantics change candidate-count→evicted-count).
- byte async (shared_trait_impl.rs:294-303) is an INLINE owned `for` loop calling trie.evict_node_at_path per
  node, NOT a helper call → the swap is a loop-BODY rewrite (gate → evict_overlay_nodes(&trie,..) vs the owned loop).
- char INLINE force_eviction test is at mod.rs:2640-2660 (NOT 2685), asserts only re-fault correctness (no count
  assertion) → needs NO update. The tests whose green-ness DEPENDS on the route_overlay gate (the FIX-2 regression
  witnesses, V=i32 ineligible→owned path→must stay green unedited): tests/persistent_char_eviction_correspondence.rs,
  _registry_correspondence.rs (asserts force_eviction().0 >= 1), _ebr_correspondence.rs, _eviction_proptest.rs.
- 3 COMPLETE-LITERAL EvictionConfig ctors MUST gain the 2 new fields (compiler-enforced): Default (config.rs:114),
  memory_constrained (config.rs:143), read_optimized (config.rs:167). disabled()/without_memory_monitor() use
  ..Default → fine. (v2's "tests use ..Default → unaffected" was misleading: the breakage is in the config module's OWN ctors.)

## STATUS: v3 — RE-RED-TEAM (round 3): confirm the collapse (A), node-cap default (B), overhead-param single-source
## (C), and the corrected cites (D) are coherent + introduce nothing new. Mechanism confirmed sound by round-2.

# === v4 (completion-plan red-team round-3 resolution — 1 must-fix + 3 nits; A/C/D confirmed) ===
Round-3 CONFIRMED A, C, D coherent + accurately cited (no re-litigate). 1 must-fix + 3 nits:

## v4 FIX B-REVISED (the must-fix) — budget-tail cap DEFAULT = UNCAPPED, cap is OPT-IN.
v3 FIX-B (default cap = batch_size=256) RE-INTRODUCED the exact starvation the PRODUCTION design already
diagnosed + rejected (f7-overlay-eviction-production.md:195-206: with max_count=batch_size "under bulk
load … the heap grows faster than one batch/checkpoint reclaims → resident stays > budget FOREVER";
fixed there by max_count=usize::MAX). Checkpoint cadence is OPERATOR-DRIVEN (verified: no
checkpoint_interval/inserts_since_checkpoint mechanism) → per-checkpoint cold growth G is unbounded; a
bulk import (import millions, then checkpoint) has G ≫ 256 → a 256-cap leaves resident growing without
bound = the OOM the feature exists to prevent. This is a COLD-set starvation (those nodes ARE evictable,
just-stamped) — distinct from M-7a's inherent hot-set>budget limit. RESOLUTION (reconcile with the
already-converged production decision): `resident_budget_eviction_cap: Option<usize>` DEFAULT None ⇒
**uncapped** (`max_count = usize::MAX`, budget-precise: one pass evicts the coldest summing to
resident−budget; the large pass is ONE-TIME on the first over-budget checkpoint, then small steady-state
deltas; the eviction is non-blocking loser-safe root-CAS, NOT one long lock-hold — the documented
production trade). `Some(n)` ⇒ opt-in LATENCY limiter for operators who MEASURED their burst AND accept
slower convergence (n MUST be ≥ per-checkpoint growth or it starves — C.1 massif REPORTS measured growth
so n is set from data). Budget tail: `max_count = resident_budget_eviction_cap.unwrap_or(usize::MAX)`.
This keeps FIX-A's collapse (one resident select arity) + the cap FIELD (opt-in) but inverts the DEFAULT
to the production design's uncapped choice. (N1 latency is bounded by non-blocking CAS + one-time-ness;
operators needing a hard cap opt in.)

## v4 NIT 1 — overhead param goes to ALL 5 select_* callers (FIX C/D omitted 2).
select_*_for_eviction callers (coordinator.rs): 295 (force_eviction), 336 (force_eviction_char), 381
(force_eviction_bytes), AND 659 + 703 (the request/urgency MemoryPressure paths — OMITTED by FIX C/D).
All 5 + the 4 test-site callers (disk_registry.rs:553, tests.rs:82/249/391) pass `overhead = 0` to
preserve on-disk behavior; ONLY the new budget arity passes `overhead = STRUCT_OVERHEAD_{variant}`.
Compile-caught, but enumerate all 5 so no pass is missed.

## v4 NIT 2 — STRUCT_OVERHEAD a literal shared `const` (no compiler-enforced single-source).
Neither STRUCT_OVERHEAD nor *_resident_estimate_bytes exists yet → "same const in both" is a forward
DISCIPLINE, not a fact. Make STRUCT_OVERHEAD_BYTE / STRUCT_OVERHEAD_CHAR literal `const`s referenced by
BOTH the select accumulator (NIT-1 budget call) AND the *_resident_estimate_bytes accessor (7.2), so the
target (resident−budget, accessor's overhead) and the accumulation (select's overhead) can't drift into
different units (which would re-create the FIX-1 mismatch). No compiler check today — enforce by sharing the const.

## v4 NIT 3 (verified) — config ctors: only 3 are complete literals.
CONFIRMED: disabled() (config.rs:132) + without_memory_monitor() (config.rs:186) BOTH use ..Default::default()
(:136/:190) → no edit. Only Default (config.rs:114), memory_constrained (config.rs:143), read_optimized
(config.rs:167) are complete literals → MUST gain resident_budget_bytes + resident_budget_eviction_cap.

## STATUS: v4 — RE-RED-TEAM (round 4): confirm B-REVISED reconciles with the production uncapped decision +
## the 3 nits close cleanly. Expect CONVERGED (FIX B restores an already-red-team-converged choice; A/C/D done).
