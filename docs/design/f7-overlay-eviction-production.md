# Production overlay-heap reclamation (byte+char, DRY) — design v1 (to be red-teamed)

Problem (confirmed @ e909eab): under route_overlay() (F2 default) every write accumulates in the
resident overlay (lockfree_root Arc<OverlayNode>); checkpoint() serializes the image but does NOT
reclaim. BYTE: eviction_coordinator always None (mmap_ctor:86/186/290/603, io_uring:110/347) →
checkpoint_route_split (core/overlay/checkpoint.rs:135) takes the no-eviction branch; the
"_with_eviction" variant (overlay_checkpoint.rs:360) is a no-op stub. CHAR: wires a coordinator +
registry, BUT the production callback evict_char_nodes→evict_node_at_path acts on the OWNED tree
(mod.rs:2527, empty under overlay); the REAL overlay reclaimer evict_overlay_node_at_path
(mod.rs:1892) is #[cfg(test/bench-internals)]. → BOTH variants are production no-ops; the fix is a
SHARED byte+char concern. Resident heap grows unbounded → libgrammstein byte n-gram shards OOM.

## Existing infra (reuse; proven)
- Primitive: OverlayNode::with_child(k, Child::OnDisk(ptr)) + root-CAS (core/overlay/node.rs:814;
  Child :118; iterative leak-free Drop :1002). Witness: byte reclaim_tests strong_count==1 (lockfree_cas:1573).
- Char overlay evict driver: evict_overlay_node_at_path (mod.rs:1892, loser-safe CAS, zero-unsafe,
  leaf-first batch evict_overlay_nodes :1996) — CORRECT, just #[cfg]-gated.
- Registry→SwizzledPtr: EvictableCharNode{path,disk_ptr,depth,size} (disk_registry.rs:46), populated
  at checkpoint persist.rs:1167 (every serialized node → its exact on-disk ptr).
- Fault-in: char load_overlay_node_from_disk/load_char_node_from_disk_lazy (disk_io.rs:296) +
  find_leaf_faulting (lockfree_cas.rs:1385) + build_path_recursive OnDisk arm (:1445). Byte
  load_overlay_node_from_disk (overlay_fault.rs:48) EXISTS but the hot lock-free read/write paths
  do NOT call it (the BYTE GAP).
- Formal: OverlayEvictionCas.tla ALREADY models the production design (USE_FAULT_IN=TRUE proves
  NoLostAck/ReadNeverMissesCommitted/FaultEqualsDurable/ReachableNotFreed; _Unsafe.cfg negative ctrl).

## The fix (narrow; DRY)
1. **Coordinator wiring (opt-in enable_eviction(budget), byte+char, DRY).** NOT default-on (controlled
   blast radius; reversible). Add a shared force_eviction_overlay<U>(target, callback) to
   EvictionCoordinator (coordinator.rs:291/327 — byte's force_eviction only selects+counts; char's
   force_eviction_char invokes the callback; unify). Both enable_eviction impls (byte
   shared_trait_impl.rs:244, char mod.rs:2158) install the coordinator with the OVERLAY callback (NOT
   the owned evict_node_at_path) + share self.epoch_manager.
2. **Un-gate + call the overlay evict driver.** Lift evict_overlay_node_at_path/evict_overlay_nodes
   (char mod.rs:1842-2058) to production pub(crate) + DROP the cold-only filter (production has
   fault-in → may evict ANY durable node). LIFT to a K-generic in persistent_artrie_core (byte+char
   share, DRY) — also unifies with the fault-walk (they're "keep-in-lockstep" coupled). Byte gets the
   twin. CALL it from the checkpoint TAIL (publish_..._with_eviction, char persist.rs:720 / byte
   overlay_checkpoint.rs:360) AFTER publish+verify+update_disk_registry+WAL-Checkpoint-sync — so every
   victim's SwizzledPtr is durable. Byte must ALSO register nodes in serialize_overlay_node_to_disk
   (overlay_checkpoint.rs:743 — currently doesn't).
3. **Close byte fault-in gap** (mirror char): byte find_leaf_faulting + route contains/get/get_value/
   prefix/find_leaf_lockfree through it; add the OnDisk fault arm to build_final_path_recursive
   (lockfree_cas.rs:476/559/899 — currently as_in_mem()-bail / Conflict). DRY: lift the fault-walk to
   the same K-generic core helper.
4. **Budget policy: evict-subsumed-down-to-budget, leaf-first, LRU.** Add resident_budget:
   Option<ResidentBudget{Nodes(usize)|Bytes(usize)}> to EvictionConfig (config.rs:24). target_bytes =
   max(0, resident - budget); select_*_for_eviction (disk_registry.rs:318) already LRU-orders +
   stops at target. min_eviction_depth keeps root+L1 resident.
5. **Resident counter:** add AtomicUsize resident_overlay_nodes (primary unit = node count;
   deterministic), inc at overlay publish points, dec on Evicted + reader-snapshot reclaim. Input to
   target_bytes + the heap assertion. **LOAD-BEARING: inc/dec at EXACTLY the publish/evict/reclaim
   points or the budget drifts** (derive from the CAS publish points, not ad-hoc).

## Invariants (the #41 guard is structural)
- **No lost write / #41:** victims sourced ONLY from the post-checkpoint registry = nodes durable ≤
  committed_watermark_at_capture (publish-AFTER-verify, persist.rs:739-754). An un-synced write (LSN >
  watermark) is NOT serialized → NOT in the registry → can NEVER be selected. A concurrent durable
  write invalidates the registry (coordinator.rs:424 invalidate_registry; is_valid gate) → evictor
  reclaims nothing from a dirtied registry (liveness loss, not safety). NO truncate (WAL>w retained).
- **No UAF:** pure Arc/arc-swap; pre-evict reader holds its load_full snapshot Arc; superseded version
  reclaims when the last reader drops (iterative Drop). evictor pins enter_read. (tla ReachableNotFreed)
- **No double-count:** faulted node reads EXACT value from image (load_overlay_node_from_disk writes
  nothing, advances no watermark; tla FaultEqualsDurable); the i128 codec (20e8e27) covers u64 >
  i64::MAX. On reopen, counter read from image, never re-added.
- **#41 capture-ordering UNTOUCHED:** evict strictly AFTER publish, never during capture (the
  watermark-Acquire-before-root-load is unmodified).

## F7 interaction (CONFIRMED no conflict)
Evicted OnDisk overlay child re-serializes verbatim at the next checkpoint (serialize reuses the
OnDisk location, overlay_checkpoint.rs:581); reopen faults from the image via the SAME decoder F5
uses; eviction touches ONLY lockfree_root (overlay), never self.root (owned) — does not resurrect
the owned tree.

## Verification
- Tests (extend OE1-OE4 + char eviction suites): round-trip exact (byte+char × {(),u64,String}, fault
  AND reopen); DATA-LOSS-PROOF (budget=0 evict-all, reopen, all terms incl. u64>i64::MAX recover);
  resident-heap ≈ budget (the counter + massif); evict‖reader/writer/faulter no-UAF (loom +
  sanitizers); no-double-count-on-reopen; loser-safe proptest (byte OE4).
- Formal: gate the EXISTING OverlayEvictionCas.tla (+ _Unsafe negative) in verify-formal-correspondence.sh;
  no-unsafe-boundary stays exit-0 NO delta (driver+fault add ZERO unsafe).
- Heap bench: scaled N-million byte keys, enable_eviction ON, post-checkpoint resident ≈ budget vs
  unbounded; libgrammstein billion-ngram proxy ≤ <16GB. Real disk target/test-tmp, NEVER tmpfs.

## Residual risks (red-team surface)
R1 byte fault-in is NEW code (char's proven) — needs byte OE1/OE2/OE4 + loom before flip.
R2 evictor‖writer‖faulter CAS contention = liveness-only (missed evict, never loss); async loop catches up.
R3 resident_overlay_nodes accounting must be exact at publish/evict/reclaim or budget drifts.
R4 DRY K-generic lift (driver+fault-walk to core) touches both variants — stage behind per-variant green.

## STATUS: v1 — RED-TEAM before implementing.

# === v2 (red-team round-1 resolutions — 2 byte BLOCKERs + the accounting MAJOR) ===
Round 1: architecture VERIFIED SOUND (publish-after-verify registry, char invalidate-before-
visibility, no-double-count [fault is pure read; counter is absolute RMW in the overlay; the
"sum both layers" comment is LEGACY/INERT — impl must NOT reintroduce an owned-layer counter
read under overlay mode], root protection via min_eviction_depth=1 + empty-path hard-reject,
DRY K-generic lift OK, TLA 3-way coverage, F7 no-conflict). Folds:

## v2 FIX 1 (BLOCKER) — byte registry-invalidation chokepoint
Char invalidates the eviction registry in append_to_wal_inner (wal_helpers.rs:112) BEFORE the
WAL append/visibility-CAS, so a concurrent durable write dirties the registry and the evictor
no-ops (RwLock mutual-exclusion). BYTE has NO such chokepoint (append_mutation_wal_record,
persistence_api.rs:138, never invalidates) → with byte eviction on, a post-checkpoint byte
write could let the evictor unswizzle a freshly-OVERWRITTEN live node onto its STALE pre-write
disk ptr = LOST UPDATE. FIX: add invalidate_eviction_registry() at the head of byte
append_mutation_wal_record (mirror char), BEFORE the WAL append.

## v2 FIX 2 (BLOCKER) — byte fault-in is READ **AND WRITE** (liveness)
Byte reads bail on OnDisk (find_in_lockfree_trie:490, find_leaf_recursive:576 as_in_mem()?) =
silent read-loss; byte WRITES return DurableBuildError::Conflict on OnDisk (build_path_recursive
:390, build_final_path_recursive:918) → a write under an evicted prefix INFINITE-LOOPS on
Conflict (the retry re-finds the same OnDisk child; no faulter installs it). Char faults on BOTH
(find_leaf_faulting :1385; build_value_path_recursive OnDisk arm :1566). FIX: byte must fault-in
on read AND write (mirror char: load_overlay_node_from_disk + splice Child::InMem + loser-safe
install CAS). LIFT both fault-walks (read + write) to the K-generic core helper (DRY, alongside
the evict driver).

## v2 FIX 3 (MAJOR resolution) — RSS-based budget, NOT a per-node live-counter
The design's "resident_overlay_nodes inc at publish points" is WRONG units (path-copy allocs D
ancestors per insert; losing CAS attempts alloc-then-drop; superseded versions free LAZILY in
OverlayNode::Drop — no trie-level hook) AND a global node-counter CONFLATES multiple tries.
RESOLUTION: use the EXISTING memory-pressure monitor (EvictionConfig.target_memory_fraction /
the memory_monitor RSS) as the production BUDGET — it is the OS-truth heap measure, matches the
actual "<16 GB total process heap" goal, and sidesteps the node-counter drift entirely. At
checkpoint (and the async pressure loop), evict the COLDEST registry entries (LRU,
select_*_for_eviction) until RSS <= target (re-check via the monitor), sourcing victims ONLY
from the post-checkpoint registry (the #41 guard). The HEAP TEST asserts the bound via
`valgrind --tool=massif` (physical RSS truth) — no production node-counter to drift. (If a
per-trie precise measure is later needed, a node-Arc-back-ref counter is the fallback, but it
bloats every node — counterproductive for a memory-reclamation feature — so RSS is preferred.)

## v2 FIX 4 (MINOR) — evict-all test asserts root-resident
The evict-all (budget=0 / aggressive preset min_eviction_depth=0) data-loss-proof MUST assert
the root stays resident (relies on the empty-path hard-reject mod.rs:1899; root has no parent
slot so it is never a victim). Also assert the "" root value survives evict-all.

## STATUS: v2 — RE-RED-TEAM (the 2 byte BLOCKERs + the RSS-budget resolution).

# === v2.1 (REVISES v2 FIX 3 + adds the 3-variant scope — evidence from the code audit) ===

## v2.1 THREE-VARIANT SCOPE (audit, 2026-06-07) — overlay-eviction is CHAR+BYTE only; VOCAB already works
| Variant | Representation (default) | Eviction wiring | Production status |
|---|---|---|---|
| CHAR  | overlay-default (`route_overlay()`; owned tree Empty) | `enable_eviction`→`evict_char_nodes`→`evict_node_at_path` (OWNED, **no-op** under overlay). The REAL overlay reclaimer `evict_overlay_node_at_path` (mod.rs:1892) / `evict_overlay_nodes` (:1996) is `#[cfg(any(test, feature="bench-internals"))]`. | **NO-OP** (reclaims the empty owned tree) |
| BYTE  | overlay-default (`route_overlay()`) | `EvictableARTrie for SharedARTrie` (shared_trait_impl.rs:243)→`enable_eviction`→`evict_node_at_path` (OWNED, **no-op**). NO overlay reclaimer exists at all; NO overlay fault-in on read OR write. | **NO-OP** + missing primitive + missing fault-in + missing registry-invalidation (v2 FIX 1/2) |
| VOCAB | **OWNED-tree** (`lockfree_root: None`, query reads `self.root`; NO `route_overlay`) | `EvictableARTrie for SharedVocabARTrie` (mod.rs:723)→`enable_eviction`→`evict_node_at_path` (OWNED, parent-pointer-integrity) acts on the REAL tree vocab uses. | **WORKS** — reclaims the owned tree it actually reads from. |

⇒ The overlay-eviction work targets **CHAR + BYTE** (the overlay-default variants, both production no-ops),
DRY via the shared `OverlayNode<K,V>` core helper. **VOCAB needs NO change** — it is owned-tree by
default and its `evict_node_at_path` already reclaims its real representation. (If a vocab lock-free
overlay production mode is ever enabled it would inherit the char fix through the shared helper, but the
default + the libgrammstein use are owned ⇒ out of scope here.)

## v2.1 FIX 3 (REVISED — registry-sized budget, NOT process-RSS, NOT a per-node live counter)
The audit kills BOTH earlier budget proposals: the MemoryPressureMonitor measures SYSTEM-available
(`mem_available/mem_total`, sysinfo+PSI), NOT process RSS — it cannot bound one trie's heap to "<16 GB"
(a 100 GB trie on a 256 GB box still reads >30% available → never fires). And NO process-RSS reader
exists. The red-team's per-node alloc↔Drop counter is ALSO unnecessary (it drifts + conflates tries).
RESOLUTION — the checkpoint-built DiskLocationRegistry IS the per-trie, drift-free node accounting:
  - `register_char` (persist.rs:1168) records every committed node at checkpoint as
    `(path, disk_ptr, size_bytes, depth, node_type)`; `total_size_bytes()` sums them. Because at
    checkpoint the overlay == the durable image (every resident committed node is registered), the
    registry sum is a faithful per-trie resident estimate — and it is EXACTLY the #41-safe set
    (durable ≤ committed watermark), so budget-sourcing inherits the no-lost-write guard for free.
  - `select_char_for_eviction(target_bytes, lru, min_depth, max_count)` ALREADY selects the COLDEST
    (LRU coldness, desc), filtered by `min_eviction_depth`, up to `target_bytes`/`batch_size`;
    `force_eviction_char(target_bytes, callback)` ALREADY drives it. The amount mechanism EXISTS.
  - NEW production work is then narrow: (i) add `resident_budget_bytes: Option<usize>` to
    EvictionConfig (None = today's unbounded behavior = back-compat); (ii) at the checkpoint tail
    (char persist + byte persist), after publish+update_disk_registry, if `total_size_bytes() > budget`
    call `force_eviction_{char}(total - budget, OVERLAY_evict_callback)`; (iii) the callback is the
    UN-GATED overlay evictor (evict_overlay_nodes), NOT the owned-tree no-op (evict_char_nodes).
  - SIZE UNIT: `register_char` records the ON-DISK serialized `data.len()` — a proxy for resident
    (in-memory tiers are larger but proportional). v1 expresses the budget in on-disk-equivalent bytes
    + documents the proportionality; the massif test calibrates the real-RSS factor (data-driven, the
    ground-truth heap bound). PRECISION UPGRADE (if the factor is unstable): weight selection by an
    in-memory tier size derived from the registry's `node_type` (no new field, no drift) — deferred
    behind the massif evidence.
  ⇒ The red-team's #4 (resident-accounting drift) is DISSOLVED: there is no per-node live counter; the
    registry is the accounting, updated at register (checkpoint) and rebuilt fresh each checkpoint.

## STATUS: v2.1 — RE-RED-TEAM (byte invalidation + byte read/write fault-in + registry-sized budget + 3-variant scope).

# === v3 (red-team round-2 resolutions — 4 must-fix + 4 should-fix; architecture still SOUND) ===
Round 2 re-VERIFIED: convergence FOUNDATION sound (production iterative serializer descends only
in-mem children; OnDisk subtrees never re-walked/re-registered → drop from next registry), byte
invalidation chokepoint COMPLETE for the overlay path (all byte overlay writers funnel through
append_mutation_wal_record), checkpoint-tail safety SOUND (validity guard + loser-safe CAS re-derived
for the new call site), root protection SOUND (empty-path reject even at min_depth=0), TLA safety
coverage live (163 states clean + _Unsafe negative control). Folds:

## v3 MUST-FIX 1 (was 1a, MAJOR) — convergence: the budget path must be UNCAPPED, not batch_size
`force_eviction_char` passes `max_count = config.batch_size` (default 256, max 4096) to
`select_char_for_eviction`, whose loop breaks at `result.len() >= max_count` OR `total_bytes >=
target_bytes` — WHICHEVER FIRST. So one call evicts ≤ batch_size nodes regardless of how far over
budget; there is NO multi-pass loop. Under bulk load (a checkpoint adding ≫ batch_size resident nodes
— libgrammstein's exact load) the heap grows faster than one batch/checkpoint reclaims → resident
stays > budget forever. FIX: the CHECKPOINT-TAIL budget path selects UNCAPPED (`max_count =
usize::MAX`) so one pass selects all the COLDEST nodes summing to `target_bytes = resident − budget`
and evicts down to budget in a single pass (the eviction is non-blocking loser-safe root-CAS, not one
long lock-hold; the large pass happens only on the first over-budget checkpoint, then steady-state
evicts only the small per-checkpoint delta). The ASYNC memory-pressure loop KEEPS batch_size
(incremental). If the budget is UNREACHABLE because nodes below `min_eviction_depth` alone exceed it,
evict all eligible and `log::warn!` (NO silent cap — the no-silent-caps rule). Add a `force_eviction*`
arity / param carrying the uncapped `max_count` for the budget path (do not change the async caller).

## v3 MUST-FIX 2 (was 1b, MAJOR) — budget on IN-MEMORY tier size, not on-disk bytes
REFUTED the "term-length-varying factor": the overlay serialize is 1:1 (each overlay node = one
`Frame` = one `serialize_one_char_node_to_disk` = one `register_char`; NO path compression on the
overlay serialize — Phase B "no path compression ⇒ identical structure"), so the registry holds ONE
entry per (un-path-compressed) overlay node. Therefore `Σ in_mem_tier_size(node_type)` over the
registry is an EXACT resident estimate (per-tier in-memory size is a known constant: CharNodeN flat
array + Arc + OverlayNode enum overhead). The current `EvictableCharNode.size_bytes` is doc'd
"Estimated MEMORY size in bytes" (disk_registry.rs:52) but POPULATED with on-disk `data.len()`
(persist.rs:1170) — a latent doc/value mismatch. FIX: register the IN-MEMORY tier size. Concretely add
`in_mem_size_bytes` to EvictableCharNode/EvictableNode (computed at register from the disk_node tier /
node_type), and have the budget + `select_*_for_eviction` weight on it (keep on-disk `size_bytes` for
I/O/bytes_freed stats). massif still calibrates the residual constant (Arc/allocator overhead) as the
ground-truth witness, but the estimate is now per-node-exact, not a workload-varying factor.

## v3 MUST-FIX 3 (was 3d, MAJOR) — byte fault-in covers ALL 7 OnDisk-bail sites (grep-confirmed)
The 4 the design named were incomplete. The COMPLETE byte fault-in surface (src/persistent_artrie/
lockfree_cas.rs), each currently bailing on OnDisk and each must mirror char's fault arm:
  READS  — find_in_lockfree_trie:490 (membership), find_leaf_recursive:576 (value/leaf),
           collect_lockfree_entries_recursive:1504 (prefix enumeration/merge — silently drops OnDisk).
  WRITES — build_path_recursive:390 (non-durable insert), build_final_path_recursive:925 (durable
           insert → Conflict→spin), build_value_path_recursive:1070 (durable VALUE/COUNTER write →
           None→spin — **libgrammstein's exact n-gram counter workload; the most important arm**),
           build_remove_path_recursive:1021 (durable remove → AlreadyExists==already-absent → LOST
           REMOVE, a data-CORRECTNESS bug, not just liveness).
byte already HAS the loader (overlay_fault.rs:48 load_overlay_node_from_disk + OverlayFaulter<ByteKey,V>
impl :119, zero unsafe) — only the hot paths don't CALL it. The PRESENT-HOIST stays NON-faulting (a
faulting read before the WAL append is char's documented "75-minute hang"; byte already hoists
non-faulting at insert_cas_durable:677-686 — preserve that). PersistentNode<V> ≡ OverlayNode<ByteKey,V>
so char's templates port with no UAF; the read-fault does its own loser-safe install-CAS + bounded
rebase (never spins — final no-fault walk reads absent after the budget), the write-fault splices the
faulted child InMem into the fresh path-copy and lets the writer's single root-CAS arbitrate.

## v3 MUST-FIX 4 (was 6, MAJOR) — correct the VOCAB scope characterization (it does NOT "work")
"VOCAB … WORKS" is WRONG in the data-loss sense. Vocab IS owned-tree (lockfree_root:None, reads
self.root) and its evict_node_at_path reclaims the REAL tree (not a no-op) — BUT vocab's owned read
get_child (types.rs:452) uses `as_ptr()` which returns None for an OnDisk SwizzledPtr, and there is NO
vocab production read-path fault-in (deserialize_vocab_node, serialization.rs:404, is used only by
construction/recovery, NEVER by get_child/get_index). So once evict_node_at_path unswizzles a vocab
node to OnDisk, reads of any term under it return None = SILENT READ-LOSS (the SAME class as char/byte,
reached differently). CORRECTED scope: vocab is OUT of scope because libgrammstein uses vocab in OWNED
mode with eviction OFF — NOT because it "works". DO NOT enable vocab eviction without first adding an
owned read-path fault-in. (LockFreeVocab is a distinct opt-in type with no OnDisk children + no eviction
wiring — no hidden no-op there.)

## v3 SHOULD-FIX (named so the implementer doesn't trip on them)
- (was 5, DRY) The lift's load-bearing abstraction is the `OverlayFaulter<K,V>` trait
  (`fault_overlay_slot`), present for BOTH byte (overlay_fault.rs:119) and char. The fault-WALK +
  evict primitive lift K-generic over OverlayNode<K,V> ONLY IF node-loading routes through
  `self.fault_overlay_slot(ptr)` (trait dispatch) — the LOADERS stay variant-specific (char:
  buffer_manager + load_char_node_from_disk_lazy + inner_to_overlay; byte: arena_manager +
  deserialize_node_v2; same signature, different bodies — do NOT try to unify them). The registry
  plumbing (register/register_char, select/select_char, path u8 vs u32) ALSO stays variant-specific;
  the lift covers the spine-walk, not the registry.
- (was 4, epoch + callback) byte enable_eviction (shared_trait_impl.rs:263) + vocab create a SEPARATE
  EpochManager for the coordinator; for byte's OVERLAY evictor to drain real overlay readers honestly,
  byte must share self.epoch_manager like char (mod.rs:2188) — reclamation is Arc-refcount (not EBR) so
  this is reader-ACCOUNTING honesty, not correctness. And the production force_eviction caller
  (mod.rs:2290) currently passes evict_char_nodes (the OWNED no-op) — swap to evict_overlay_nodes.
- (was 2, byte 2nd appender) append_batch_mutation_wal_record (persistence_api.rs:197) does NOT
  delegate to append_mutation_wal_record and is used ONLY by the OWNED arm of insert_batch — off the
  overlay-eviction path, so the single invalidate suffices. Add a comment it is owned-only (or
  defensively invalidate there too) so a future owned+eviction byte config can't silently regress.
- (was 7, formal scope) OverlayEvictionCas.tla covers the abstract 3-way evictor‖faulter‖writer
  root-CAS + evict⊆durable precondition + no-UAF — but does NOT cover: (i) the checkpoint-tail
  composition (publish→concurrent-writer-invalidate→tail-evictor is_valid() as one system — close with
  a loom test: checkpoint-tail-eviction ‖ writer), (ii) convergence/budget (a LIVENESS property the
  safety specs don't model — close with a massif heap soak under write-heavy load), (iii) byte's NEW
  fault arms build_value/remove/collect (variant-agnostic spec doesn't exercise them — byte OE1/OE2/OE4
  instantiations + loom). The design must SAY these are loom/heap-test obligations, not TLA-covered.

## STATUS: v3 — RE-RED-TEAM (round 3): convergence-uncap, in-mem-size budget, 7-site byte fault-in,
## vocab "do-not-enable", + the 4 should-fix. Architecture unchanged & sound across all 3 rounds.

# === ROUND-3 FINDINGS (v3 NEEDS-REVISION) — feed into the v4 Plan-agent design ===
Architecture still sound; but v3 had a WRONG safety claim + an over-correction. Round-3 verdict:
- **BLOCKER 1c (SAFETY) — concurrent overwrite during the uncapped evict = lost update.** The evict
  CAS keys on the ROOT (re-loaded fresh inside evict_overlay_node_at_path, mod.rs:1921/1976), NOT the
  child Arc → it SUCCEEDS against a node a writer overwrote between selection (force_eviction_char
  drops the registry lock, coordinator.rs:332/354) and the per-node evict → NEW value unswizzled onto
  the stale ptr; acked write invisible until reopen (violates ReadNeverMissesCommitted). v3's "CAS
  keyed on child Arc identity → skips" is FALSE. AND naive Arc-identity can't fix it: the eviction's
  own LEAF-FIRST path-copies bump every ancestor Arc → identity would reject the internal nodes the
  eviction just rebuilt. CORRECT guard = "live node structurally EQUALS its durable image" — candidates
  for v4: (A) serial_disk_ptr atomic on OverlayNode (set Release at serialize, NULL on write-copy,
  PRESERVED on evict-copy; evict iff node.serial_disk_ptr == registered disk_ptr), or (B) batch-build
  ONE new root with all evicted slots→OnDisk then a single CAS against the loaded root (no intermediate
  ancestor-bump; identity vs the loaded root is valid; bounded-retry; defer to next checkpoint under
  contention). Also extend OverlayEvictionCas.tla: model the registry disk_ptr as a per-node VERSION,
  WriterCas STALES the entry, negative control fires ReadNeverMissesCommitted (the current model treats
  durable as version-free set membership → cannot catch this).
- **BLOCKER — byte serialize-time registration MISSING.** byte serialize_overlay_node_to_disk /
  publish_overlay_snapshot_retaining_with_eviction (overlay_checkpoint.rs:743/360, the latter a
  passthrough stub "registry … not yet wired for byte") never register → byte registry permanently
  empty → byte eviction a no-op regardless of fault-in/budget. Mirror char's register at byte serialize.
- **BLOCKER — byte invalidation ABSENT (not "complete").** zero invalidate_eviction_registry in byte;
  append_mutation_wal_record (persistence_api.rs:138) only appends. v2 FIX 1 must be IMPLEMENTED. (NOTE:
  the 1c per-node durable-validation SUBSUMES invalidation as a CORRECTNESS mechanism — invalidation
  becomes a coarse early-out; checked-once-then-evict-list means invalidation alone can't catch mid-list
  overwrites. Decide in v4: rely on 1c guard for correctness, keep invalidation as optional early-out.)
- **MAJOR — budget estimate NOT exact (v3 over-corrected).** node-type tier size DROPS the value
  payload (String/Vec heap), the Arc<[Unit]> prefix, and transient superseded versions; in-mem
  ChildStore is 2-tier Inline(0-4)/Heap(5+) count-dependent (node.rs:197-219), NOT a 4-tier NodeType
  constant. On-disk data.len() (v2.1) at least INCLUDES the serialized value. v4: estimate = on-disk
  data.len() + per-node structural-overhead constant, massif-calibrated, APPROXIMATE + safety margin;
  drop "per-node-exact". Add char_total_size_bytes() accessor (total_size_bytes mixes byte+char maps,
  disk_registry.rs:108).
- **MAJOR — byte counter fault needs BOTH halves.** route try_increment_cas_inner step-2 cur read
  through a NEW byte find_leaf_faulting (lockfree_cas.rs:1225 currently find_leaf_recursive→unwrap_or(0))
  AND the step-4 build_value_path_recursive fault arm; else an evicted counter resets to 0+delta. Mirror
  char lockfree_cas.rs:1739.
- **MINOR — unit consistency + uncap isolation.** budget math + bytes_freed/force_eviction reporting
  (coordinator.rs:305) MUST use the same size field; add the uncapped max_count as a NEW arity used ONLY
  by the checkpoint tail (don't change shared select_char_for_eviction — async perform_eviction_char
  coordinator.rs:642 + force_eviction:291 share it). Keep the min_eviction_depth-unreachable log::warn.
VERIFIED-SOUND (no change): 7-site byte fault enumeration + remove-arm semantics, vocab do-not-enable,
OverlayFaulter<K,V> K-generic lift (loaders variant-specific), append_batch_mutation_wal_record
owned-only, registry-not-persisted (field add is format-safe), uncapped single-pass mechanics +
non-blocking O(N·depth) cost.

## STATUS: v3 NEEDS-REVISION → v4 via PLAN AGENT (centerpiece = the 1c overwrite-race-safe evict driver
## A-vs-B + TLA extension; budget revert; byte registration/invalidation/counter; then RE-RED-TEAM v4).
