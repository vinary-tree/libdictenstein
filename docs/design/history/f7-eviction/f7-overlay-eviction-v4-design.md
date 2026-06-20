# f7 Overlay-Eviction v4 — converged design (Plan agent, post round-3)

> Companion to `f7-overlay-eviction-production.md` (v1→v3 history + round-1/2/3 findings).
> v4 is the Plan-agent design folding all round-3 must-fixes. Centerpiece = the 1c
> overwrite-race-safe evict driver via mechanism (A). Awaiting round-4 red-team convergence.

## 1. CENTERPIECE — 1c overwrite-race-safe evict driver: MECHANISM (A) `serial_disk_ptr`

### The race (verified against code)
`force_eviction_char` (coordinator.rs:336-356) and the async twins do `select → drop(disk_registry)
→ callback`. Between selecting victim X (registry disk_ptr = X's durable image) and the per-node evict,
the registry lock is dropped. `evict_overlay_node_at_path` (mod.rs:1892-1980) loads `old_root` FRESH
(:1921), walks `as_in_mem()` slots, swaps the victim's parent slot → OnDisk(disk_ptr), CAS keyed on
`old_root` (:1976). If a writer overwrote X (path-copy new InMem X′ w/ NEW value + publish R′) between
selection and load, the evictor unswizzles X′ to X's STALE disk_ptr and the CAS against R′ SUCCEEDS →
acked new value lost until WAL replay. Naive Arc-identity FAILS: leaf-first eviction's own path-copies
(mod.rs:2010) bump every ancestor Arc → identity rejects nodes the eviction itself rebuilt.

### The correct invariant + why a single per-node stamp suffices
Evict X to disk_ptr ONLY IF the live node at X's path STRUCTURALLY EQUALS the durable image disk_ptr
names. By the immutable path-copy invariant, overwriting ANY descendant D of X path-copies ALL of D's
ancestors INCLUDING X → a fresh X Arc. So "neither X nor any descendant overwritten since the
checkpoint that registered X" ⟺ "X is the exact Arc stamped at that checkpoint" ⟺ "X.serial_disk_ptr
== registry.disk_ptr". The eviction's own ancestor rebuilds get stamp 0 (write/evict copy clears it) →
correctly non-evictable until re-serialized. This is why A keys on the STAMP, not on Arc identity, and
why it avoids the false-reject that motivated the (rejected) batch-CAS mechanism B.

### Why (A) over (B) batch-CAS and (C) dirty-bit
- (B) one-CAS-for-the-whole-batch: one writer anywhere ABORTS the whole pass → STARVES under
  continuous writes (libgrammstein's load) = opposite of an OOM fix. Also needs a registry-side
  Weak<node> (keeps checkpointed allocations alive = counterproductive on the reclaim path) + ABA guard.
- (C) 1-bit dirty (IS_DIRTY exists, node.rs:34): catches the overwrite race but a 1 bit can't name WHICH
  durable image → misses the stale-registry-generation race (evict to an older registry's disk_ptr). A's
  64-bit `== disk_ptr.to_raw()` catches both. C is strictly weaker. Reject.
- (A): correct, DRY (one field on OverlayNode<K,V> → byte+char free), minimal (one `if` before the CAS),
  non-blocking (keeps the per-node loser-safe root CAS → localized contention, steady-state reclaim
  under write pressure), live (stale victim SKIPPED not retried-forever; next checkpoint re-stamps),
  formal-checkable. Cost = +8B/node (folded into the §2 budget). CHOSEN.

### Exact algorithm
FIELD on `OverlayNode<K,V>` (node.rs, after `version`): `serial_disk_ptr: AtomicU64` (to_raw() of the
durable SwizzledPtr; 0 = none). `new`/`with_prefix`/ALL path-copy ctors (`with_child`/`without_child`/
`with_prefix_replaced`/`as_final`/`as_non_final`/`with_value`)/`Clone` init it to 0. Accessors
`durable_stamp()->u64` (load Acquire) / `set_durable_stamp(raw)` (store Release).
STAMP-AT-SERIALIZE (the single registration site each variant): at the `register_*` call —
char serialize_one_char_node_to_disk (persist.rs:1167) on the live `frame.node` Arc (persist.rs:1401);
byte serialize_overlay_node_to_disk (overlay_checkpoint.rs:743) on `node` — also call
`node.set_durable_stamp(result_ptr.to_raw())`. InMem-only (OnDisk children reused verbatim, not
re-registered → convergence preserved).
GUARD in the K-generic primitive, BEFORE the bottom-up rebuild + root CAS:
    let want = disk_ptr.to_raw();
    if current.durable_stamp() != want { return OverlayEvictOutcome::NotEvictable; }
The guard closes "raced before load"; the existing root CAS closes "raced after guard" (that writer
path-copies the victim → old_root stale → CAS fails → bounded rebase → re-walk sees stamp 0 →
NotEvictable). ABA-safe: arena slots fresh each checkpoint, registry rebuilt fresh, write-copy clears
the stamp regardless.

### TLA extension (OverlayEvictionCas.tla) — the 1c safety witness
Add per-node `durableVersion`/`liveVersion`/`evictedToVersion`/`ackedVersion` (the stamp = "liveVersion
node carries durableVersion iff not overwritten since the checkpoint"). `WriterCas(n)`:
liveVersion+1, acked. New `Checkpoint(n)`: durableVersion := liveVersion (re-stamp). Guarded
`EvictCasSucceed(n)` requires `durableVersion[n] = liveVersion[n]`. New invariant `NoStaleEvict` (an
acked node is reachable-at-acked-version OR onDisk-evictedToVersion==acked OR durable-version==acked).
Negative control `USE_1C_GUARD=FALSE` drops the conjunct → WriterCas then unguarded evict →
evictedToVersion(1) ≠ acked(2) → NoStaleEvict VIOLATED (the lost update). Keep USE_FAULT_IN=FALSE
control. MaxRoot≈8. Gate the new `_StaleEvict_Unsafe.cfg` in verify-formal-correspondence.sh (251/305/360).
loom (NOT TLA): the stamp Release/Acquire vs with_child reconstruction + checkpoint-tail ‖ writer.

## 2. BUDGET (revert v3 over-correction) — APPROXIMATE on-disk + overhead
resident_estimate(node) ≈ on_disk_data_len (INCLUDES serialized Option<V>) + STRUCT_OVERHEAD
(massif-calibrated per-variant const: Arc cb + node struct + serial_disk_ptr + ChildStore inline arrays
+ Arc<[Unit]> prefix + amortized superseded slack). Keep registry `size_bytes` = on-disk data.len()
(do NOT add v3's in_mem_size tier field). Add variant-split `char_resident_estimate_bytes` /
`byte_resident_estimate_bytes` (total_size_bytes mixes both maps, disk_registry.rs:108/243). Config:
`resident_budget_bytes: Option<usize>` (None = back-compat, no eviction). Tail: if resident>budget,
force_eviction_*_uncapped(resident-budget) [NEW arity, max_count=usize::MAX, ONLY the tail; async keeps
batch_size]; min_eviction_depth-floor-unreachable → log::warn (no silent cap). Unit-consistency: budget
math + bytes_freed + force_eviction reporting all read size_bytes.

## 3. BYTE COMPLETENESS
3.1 Serialize-time REGISTRATION (BLOCKER): byte CheckpointSnapshot (overlay_checkpoint.rs:277-286) has
NO eviction_registry field → add it; build in capture_overlay_snapshot (:210, mirror char :289-294);
thread reg + path:Vec<u8> through serialize_overlay_root/subtree/node_iterative; register InMem-only +
set_durable_stamp in serialize_overlay_node_to_disk (:743); replace publish_*_with_eviction passthrough
stub (:360) with char-tail logic (publish→verify→update_disk_registry→WAL Checkpoint retain→commit_seq)
+ the §2 budget tail.
3.2 FAULT-IN for all 7 OnDisk-bail sites (overlay_fault.rs:48 loader via OverlayFaulter<ByteKey,V>):
reads find_in_lockfree_trie:490 + find_leaf_recursive:576 (→ new byte find_leaf_faulting, install-CAS
+ bounded rebase, present-hoist stays NON-faulting) + collect_lockfree_entries_recursive:1504; writes
build_path_recursive:390 + build_final_path_recursive:925 + build_value_path_recursive:1070(counter) +
build_remove_path_recursive:1021(lost-remove) (splice faulted child InMem into the fresh path-copy).
3.3 COUNTER both halves: try_increment_cas_inner step-2 cur read (lockfree_cas.rs:1225) → byte
find_leaf_faulting (mirror char :1739); step-4 build_value_path_recursive fault arm (mirror char :1773).
3.4 INVALIDATION (BLOCKER): add byte invalidate_eviction_registry at append_mutation_wal_record head
(persistence_api.rs:138). DECISION: the 1c serial_disk_ptr guard is the CORRECTNESS mechanism;
invalidation is a coarse early-out only (checked-once can't catch mid-list overwrites; the stamp does).

## 4. DRY + epoch + callback
Lift fault-walk (read+write) + evict primitive K-generic over OverlayNode<K,V> via OverlayFaulter<K,V>
seam (node-loading through self.fault_overlay_slot; LOADERS stay variant-specific — char buffer_manager+
load_char_node_from_disk_lazy, byte arena_manager+deserialize_node_v2; registry plumbing stays
variant-specific). byte enable_eviction shares self.epoch_manager (shared_trait_impl.rs:262, was
separate). Swap callbacks evict_char_nodes(owned no-op)→evict_overlay_nodes (char mod.rs:2201/2290 +
byte shared_trait_impl.rs:267).

## 5. VERIFICATION
Data-loss-proof (byte+char × {(),u64,String}): THE 1c overwrite-during-evict centerpiece test (writer
overwrites cold X v1→v2; evict(X.path,disk_ptr_v1) → NotEvictable; read=v2; reopen=v2; + loom racing
variant); evict-all root-survives + "" root; counter both-halves (no 0+delta reset); remove-arm (no
lost remove); no-double-count-on-reopen; fault==durable round-trip. massif heap-bound bench (real disk
target/, NEVER tmpfs; calibrates STRUCT_OVERHEAD; libgrammstein proxy ≤16GB). loom checkpoint-tail ‖
writer + evictor‖writer‖faulter. TLA extension + _StaleEvict_Unsafe + USE_FAULT_IN=FALSE controls.
unsafe-inventory set-equality (ZERO new unsafe — all Arc/atomic/CAS). vocab stays OUT (do-not-enable).

## 6. PHASES (each keeps cargo nextest --features persistent-artrie green)
0 serial_disk_ptr field+accessors (inert, +8B, reversible). 1 stamp-at-serialize (char then byte snapshot
field+threading+register+stamp). 2 1c guard in primitive + un-gate evict_overlay_nodes + drop cold-filter
+ centerpiece test. 3 TLA extension + negative controls + gate. 4 K-generic lift via OverlayFaulter
(char re-targets, behavior-identical). 5 byte 7-site fault-in + counter both-halves + byte OE tests.
6 byte invalidation + epoch share + callback swaps + byte publish real body. 7 budget revert + uncapped
tail arity + resident_budget_bytes config + tail wiring + warn. 8 massif + loom + unsafe-inventory +
full formal gate. Reversible 0-4 (additions/refactors); resident_budget_bytes=None default = opt-in.

## 7. RESIDUAL RISKS (round-4 red-team surface)
R1 (PRIMARY): set_durable_stamp(Release) writes into the LIVE shared overlay node during serialize.
Benign argument: (i) changes no membership/value/child observable, only records durable location; (ii)
any concurrent overwrite path-copies (new Arc, stamp 0), root CAS orders overwrite vs stamp; (iii)
evictor reads CURRENT root's node stamp under epoch pin. PRIMARY loom target (stamp-write ‖ overwrite ‖
guard-read). Fallback if loom finds a window: mechanism B (batch CAS), accepting its liveness cost.
R2 STRUCT_OVERHEAD instability (budget-accuracy not correctness; massif calibrates + margin).
R3 byte registration is NEW code (largest byte delta; gate behind byte OE + reopen before callback flip).
R4 liveness under continuous writes (stamp skips overwritten nodes → only cold reclaimed = correct, but
convergence bench must confirm steady-state reclaim ≥ growth under libgrammstein write mix).
R5 DRY lift blast radius (touches both variants' hot paths; stage Phase 4 before 5; loaders variant-specific).

## 8. ROUND-4 RESULT — NO BLOCKERs (mechanism A verified sound); 5 MAJORs = implementation obligations
Round-4 (convergence gate) verified mechanism A sound against real code: R1 ordering holds (arc-swap
root happens-before + stamp→register→registry-publish→registry-read→guard chain); stamp-clearing
COMPLETE across all 9 ctors (grep: zero struct-literals outside node.rs; compiler-enforced); fault-in
(overlay_fault.rs:101 + char inner_to_overlay) + recovery route through zero-stamp ctors → stamp 0
(conservative-safe, fault-in→next-checkpoint re-stamp → convergence holds); serial_disk_ptr is
structurally runtime-only (NOT in SerializedCharNodeHeader; deserialize sets version=0 "runtime-only")
→ cannot leak to disk; guard+root-CAS closes both windows; +8B no format impact. The 5 MAJORs are
implementation obligations folded into the phases:

- **M-2a (Phase 0/1):** make the stamp-0 invariant EXPLICIT + TESTED — every non-serialize ctor (the 9
  + BOTH fault-in loaders + ALL recovery builders) yields serial_disk_ptr==0; ONLY set_durable_stamp at
  the register_* site writes non-zero. Test: fault a node, assert durable_stamp()==0 before next
  checkpoint. (NEVER add a "stamp the faulted node with the ptr it loaded from" optimization — it would
  falsely mark a node evictable to a ptr NOT in the current registry.)
- **M-3a (Phase 2):** pin the guard's `current` to the victim reached by the in-mem spine walk from the
  FRESHLY-loaded old_root (mod.rs:1948), NOT a selection-time node captured before the
  coordinator.rs:354 lock-drop. The guard lives INSIDE the per-attempt evict fn so each rebase re-reads
  the stamp.
- **M-4a/b/c (Phase 3, highest gate-risk — only DOING discharges it):** WRITE the extended
  OverlayEvictionCas.tla (per-node durableVersion/liveVersion/evictedToVersion/ackedVersion + a
  Checkpoint(n) action [durableVersion:=liveVersion] + RELAX WriterCas to allow re-write/overwrite of an
  already-linked node [bump liveVersion, leave durableVersion — the current `n \notin linkedInMem`
  precondition makes the violating trace UNREACHABLE → would vacuously pass] + guarded EvictCasSucceed
  recording evictedToVersion:=durableVersion + NoStaleEvict). RUN TLC: confirm (a) safe cfg passes AND
  (b) _StaleEvict_Unsafe (USE_1C_GUARD=FALSE) ACTUALLY FIRES NoStaleEvict within MaxRoot (verify the
  bound by running, don't assert ≈8). Extend the EXISTING spec (keep USE_FAULT_IN=FALSE control too);
  gate both cfgs in verify-formal-correspondence.sh.
- **M-5a (Phase 5/6):** byte serialize_overlay_subtree_iterative needs char's FULL path push/pop
  machinery (persist.rs:1351/1376/1411), not just "thread path"; byte CheckpointSnapshot gains
  eviction_registry: Option (shared struct — CONFIRM it stays None on the owned + eviction-off arms with
  the EXISTING byte opt-in durable tests as the named regression gate).
- **M-7a (Phase 7/8, documented limitation):** the budget is enforceable ONLY over the cold/quiescent
  set — the 1c guard correctly REFUSES to evict hot (overwritten-since-checkpoint) nodes, so a hot
  working set continuously rewritten and LARGER than budget cannot be bounded (inherent: you can't
  safely evict a node being concurrently overwritten). DOCUMENT this; the convergence bench must measure
  libgrammstein's n-gram counter hot-set size vs budget (NOT just steady-state reclaim≥growth). NOTE:
  checkpoint-tail eviction just-stamped ~all nodes → cold set evictable there; the limit bites the async
  pressure loop / pathological all-hot workloads. libgrammstein's STREAMING import has a bounded recent
  hot window << cold imported set → eviction works; the limit is for hot-set>budget steady-state.

Minor (fold in): R1-a state registry-gen==stamp-gen coupling; massif margin must cover the write-burst
TRANSIENT peak (off-registry superseded versions); char/byte_resident_estimate_bytes accessors are NEW;
place serial_disk_ptr off the hot read cache line; vocab inert-field note.

## STATUS: DESIGN CONVERGED (mechanism A sound, 4 adversarial rounds, no BLOCKERs). Remaining = the 5
## implementation obligations above, discharged empirically by the 8 phases + their verification
## (TLC negative control = the rigorous "once more"). IMPLEMENTING.
