# Level 3 (full owned-tree elimination) — CONVERGED execution plan (2026-06-08)

Owner GO'd "Level 3: full no-residual". This is the multi-week, data-loss-critical, IRREVERSIBLE campaign
to eliminate the OWNED in-memory trie representation. Authoritative reference; supersedes
slice3-f5-f7-{execution-plan,revalidated-2026-06-08}.md for ordering. Baseline a46d9c1 / e44a877 (2713 green).

Process (owner directive): plan → red-team → refine until convergence → +1 confirming red-team → implement BY HAND,
each step green + committable. The 8 data-loss-critical steps (flagged ⚠️RT) each get a dedicated adversarial red-team
BEFORE implementation. Implementation is by hand (plan/explore/red-team agents allowed; no work delegation).

## REFINEMENT R1 (confirming red-team af13fe7 — 2026-06-08): L0.3 RETRACTED; OR-lock collapse → L3.3
The +1 confirming red-team on the converged L0 found a BLOCKER the prior passes missed: the owned `&self` WRITE
mutators were never enumerated. **L0.3 (collapse OR RwLock → bare field) CANNOT happen at Level 0** — RETRACTED:
- byte insert_impl_core/remove_impl_core + dirty_tracking clear_dirty_flags_recursive/propagate_dirty_to_root, and
  char try_insert_impl_no_wal/_with_value/try_remove_impl_no_wal/preflight_existing_terminal_is_final all take
  self.root.WRITE() through `&self` (the OR lock's raison d'être). They are LOAD-BEARING (compaction staging via
  insert_impl_no_wal until L2.1; reestablish/recovery/staging until L3.x); the plan deletes them only at L2.2/L3.3.
- A bare field cannot express `&self` writes (won't compile); removing the lock is a data race — commit_document is
  now `&self` (e44a877), so an Arc embedder can kill_switch_to_owned + concurrently commit_document[owned write] +
  contains[owned read] + enable_eviction[owned evict], serialized today ONLY by OR. persistent_lockfree_f4_lock_
  hierarchy_loom.rs EMPIRICALLY proves OR serializes a concurrent insert_impl_core.

**Resolution:** the OR RwLock + owned root field persist UNTIL **L3.3** deletes the field OUTRIGHT (after every `&self`
owned writer/reader is gone via L2.2 + L3.3). NO intermediate bare-field state — "collapse" == "delete at L3.3". L3.3
additionally deletes clear_dirty_flags_recursive/propagate_dirty_to_root and must RETIRE/REWRITE the loom suite (its
OR-lock premise evaporates). **Level 0 is now ONLY L0.1 + L0.2**, with these RT-confirmed corrections:
- L0.1 commit must ALSO, in the same commit, migrate the owned-eviction tests: src/persistent_artrie_char/
  eviction_registry_tests.rs; the persist.rs ~2725 region; the owned-arm assertions in tests/persistent_char_eviction_
  {correspondence,proptest,registry_correspondence}.rs (re-assert vs the overlay registry / drop the kill-switched-
  owned-evict scenario); confirm tests/persistent_artrie_loom_correspondence.rs:1748 is comment-only.
- L0.2 must UNWRAP each route_overlay() guard (not merely delete the tail — else missing return → won't compile),
  delete collect_terms_with_cursor_and_arena (cursor_iter.rs:195) alongside collect_terms_from_cursor, and route/delete
  the byte iter_prefix_with_arena/iter_prefix_with_values_and_arena owned tails (arena_iter.rs:392/561).
- L0.1/L0.2 are RT-CLEARED (mechanical, no further red-team). The OR-lock collapse (now folded into L3.3) keeps its ⚠️RT.

## 5 ground-truth corrections (each load-bearing for ordering)
1. **Normal reopen is ALREADY overlay-drained.** open_inner production arms use convert_owned_to_overlay_on_reopen
   (Owned regime) / reconcile_and_drain_overlay (Overlay regime) — neither touches self.root. The owned
   replay_records_lww + reestablish_overlay_from_owned survive in open_inner ONLY on the open_with_legacy_loader
   (force_f5==false) oracle branch. So L1 redirects the CORRUPTION/ARCHIVE-rebuild ctors, not "reopen".
2. **An overlay→dense serializer EXISTS but is NOT path-compressing.** serialize_overlay_node_to_disk (byte
   overlay_checkpoint.rs / char persist.rs serialize_char_node_to_disk) is the shipping production overlay-arm
   checkpoint — one owned Node/CharNode record per overlay node, UN-compressed. owned serialize_root emits
   path-COMPRESSED images. C-opt-1 (L2) is a NEW path-compressing serializer; density is compact()'s whole purpose.
3. **4th load-bearing owned consumer: the struct zipper.** PersistentARTrieZipper / …CharZipper
   (zipper.rs has_path/is_final_at_path/get_children_at_path walk inner.root.read(), log::warn! stub). Passes the
   formal gate today only because zipper_language_correspondence exercises it on a ::new() in-memory trie
   (route_overlay()==false). L3 must re-point it onto the overlay before deleting the owned root.
4. **kill_switch_to_owned has exactly ONE production caller (compaction_impl.rs:209)** + ~80 TEST callers
   (owned-white-box). So OverlayWriteMode deletion (L2) is entangled with retiring that owned-white-box test corpus.
5. **Owned NODE record types are NOT deletable, even at L3** — Node/Node4/16/48/256/StringBucket (nodes/mod.rs:576,
   bucket.rs:287) + CharNode/CharNode4/16/48 (nodes/mod.rs:237) ARE the on-disk record format
   (serialize_overlay_node_to_disk + load_overlay_node_from_disk consume them). Only the in-memory root HOLDERS —
   TrieRoot<V> (dict_impl.rs:484), CharTrieRoot<V> (types.rs:753), CharTrieNodeInner<V> (types.rs:468) — and the
   owned methods over them are deletable. L3's codec drives the Node-record format DIRECTLY from overlay nodes,
   never materializing a TrieRoot/CharTrieNodeInner.

## OPTIMAL ORDERING (the central decision): L0 → L1 → CX → L2 → L3
Reorder vs naive 0→1→2→3: build the codec primitives as a DORMANT sub-phase (CX) AFTER L1 and BEFORE the L2/L3
flips, because (a) C-opt-1/C-opt-2 are the real new format work + prerequisites for BOTH L2 (owned-staging removal)
and L3 (owned-root deletion); landing them dormant lets the byte-identity proof land in isolation before load-bearing;
(b) L1 is codec-independent (reuses the shipping apply_recovered_operation_overlay/drain). Throughout: #41
checkpoint_lsn=committed-watermark capture ordering (core/overlay/checkpoint.rs overlay arm) is UNTOUCHED.

Per-step gate (every commit): full suite (feature-on default) + `--no-default-features` (feature-off) + doctests +
scripts/verify-formal-correspondence.sh exit 0 + scripts/verify-unsafe-boundary-inventory.sh set-equality + fmt +
cross-repo READ-ONLY cargo check (liblevenshtein-rust; and at L3.3 libgrammstein/lling-llang/pgmcp). Keep verification
LOGS to summaries (don't commit 25K-line test logs).

---
## LEVEL 0 — runtime deletion (computed route_overlay; OR-lock collapse; eviction owned arms)
route_overlay() STAYS a computed fn (red-team #2: const true breaks compaction staging). A-decision: KEEP
OverlayWriteMode at L0 (deleted at L2). Hand-delete every route_overlay-false owned arm (the compiler will NOT flag
them — `if route_overlay(){return X} <tail>` borrow-checks the tail), then collapse the OR lock.

- **L0.1 — delete eviction owned else-arms** (mechanical, RT:no — already dead under production route_overlay==true).
  Byte shared_trait_impl.rs start_eviction async cb (~291-317) + force_eviction (~392-401) → keep only the overlay
  arm; char mod.rs start_char (~2141-2145) + force_eviction (~2231-2235) → drop the evict_char_nodes else + unused
  quiescence locals. Now-dead → delete: byte evict_node_at_path (+find_parent_in_root), char evict_char_nodes +
  evict_node_at_path (+inline relink). KEEP vocab evict_node_at_path (persistent_vocab_artrie/mod.rs:767/869 — distinct type).
- **L0.2 — delete route_overlay-false owned READ tails** (RT:no). Char: try_contains/get_value/try_get
  (query_api.rs) + iter_prefix*/merge-read (prefix_api.rs) owned tails. Byte: iter_prefix_from_cursor owned tail +
  DELETE collect_terms_from_cursor (cursor_iter.rs — gap b RESOLVED: it's the route_overlay-gated owned arm, a clean
  deletion). KEEP (D1 seams, switch to &self.root at L0.3): byte contains_impl/get_value_impl (query_impl.rs:34/71 —
  called by unrouted_contains_bytes/unrouted_get_value_bytes atomic_ops.rs:248/262 + compaction_snapshot fallback +
  recompute_recovered_increment); char owned_try_contains/owned_get/owned_try_get + owned_root_guard + navigate_to_prefix_from.
- **L0.3 — collapse OR RwLock→bare field** ⚠️RT (data-loss-critical: lock-collapse soundness). root: RwLock<TrieRoot<V>>
  → TrieRoot<V> (dict_impl.rs:280); char RwLock<CharTrieRoot<V>> → CharTrieRoot<V> (mod.rs:429). Surviving self.root
  accesses after L0.1/L0.2: (i) ctor/reopen scratch (f5_loader get_mut, &mut); (ii) D1 converter seam readers;
  (iii) byte compaction staging (insert_impl_no_wal/serialize_root/capture_owned_snapshot); (iv) recovery appliers;
  (v) struct zipper; (vi) reestablish_overlay_from_owned. Rewrite each: &mut self → &mut self.root; &self read (byte
  contains_impl/get_value_impl, char owned_root_guard) → direct &self.root borrow (owned_root_guard return type
  MappedRwLockReadGuard → Option<&CharTrieNodeInner<V>>). Soundness: every surviving access is &mut self (single-writer)
  OR a &self read on a rep only reached by single-threaded reopen/recovery/compaction (production never writes owned).
  Lock order after OR removed: CK > merge_lock > EC (no cycle); Send/Sync OK (SwizzledPtr = AtomicU64+AtomicPtr).
  Gate adds the loom suites. **Red-team focus: prove NO surviving &self self.root read can race a &mut self owned
  writer in ANY config (Shared* wrappers, kill-switched owned, in-memory zipper).**

---
## LEVEL 1 — recovery redirect (S5′) ⚠️RT
Point the corruption/archive-rebuild ctors at the overlay drain, eliminating the apply_*_recovered_operation_no_wal +
reestablish_overlay_from_owned dependency. Reuse the SHIPPING apply_recovered_operation_overlay (flip.rs:1031) —
red-team #1 proved it subsumes counter semantics incl. u64>i64::MAX (counter_leaf_to_i128) + delta ACCUMULATE.
- **L1.1 byte** open_with_recovery_config (mmap_ctor.rs:935-1027): apply closures 938/977 → apply_recovered_operation_overlay;
  DELETE reestablish_overlay_from_owned (1021-1027). Verify watermark base-seed covers drained LSNs.
- **L1.2 char** open_with_recovery_config (mmap_ctor.rs:1082-1343, apply :1166 + reestablish :1337), recover_from_archives
  (:1531-1601, :1573 + :1592), open_with_full_recovery (:1403). Same transform + delete reestablish calls.
- **L1.3** retire the legacy-loader oracle's owned dependency (migrate open_with_legacy_loader to overlay-built
  correspondence or delete + re-point the both-loaders/owned-to-overlay suites to production reopen + BTreeMap oracle),
  THEN delete owned replay_records_lww + apply_*_recovered_operation_no_wal + recompute_recovered_increment +
  value_from_recovered_i64 (prove unreachable first). KEEP *_impl_no_wal (staging/reestablish; die L2/L3). UNSAFE rows
  23-24 live in KEPT *_impl_no_wal → NO prune at L1 (re-run set-equality to confirm 0 delta).
  **Red-team focus:** recover-via-drain ≡ recover-via-owned-then-convert across V×archive-layout×crash-point; the
  recover-family uses rebuild_from_wal_segments_regime_aware (its own tx-resolution) — verify SAME tx-filter the owned
  path applied (else aborted-tx records resurrect). Perf: bulk overlay path-copy rebuild vs owned dense. New TLA
  RecoveryRebuildOverlay (archive-rebuild-into-overlay sink + tx-filter parity).

---
## SUB-PHASE CX — build + PROVE the path-compressing overlay↔dense codec (DORMANT, reversible) ⚠️RT
- **CX.1 byte serializer** serialize_overlay_snapshot_compressed: walk the immutable overlay root, emit the SAME dense
  Node-record format serialize_root produces (collapse single-child chains → compressed prefixes; leaf runs →
  StringBucket; ROOT_TYPE_BUCKET vs ART_NODE per the owned heuristics) via serialize_node_to_disk_with_value_len. Do
  NOT touch serialize_overlay_node_to_disk (the un-compressed production checkpoint — STAYS). Proof: byte-identity (or
  reopen-equivalence incl. compacted_bytes density bound) vs owned serialize_root over V×{valued,term-only,""}×deep key.
- **CX.2 byte loader** load_overlay_root_compressed: read the dense path-compressed format, EXPAND multi-unit prefixes +
  buckets directly into Arc<OverlayNode<ByteKey,V>>, never materializing TrieRoot. Reuse load_overlay_node_from_disk
  (single-node). Proof: load(serialize(overlay))≡overlay AND load_compressed(legacy_owned_image) ≡
  build_overlay_root_from_owned(load_root_from_disk(legacy_owned_image)) — back-compat vs the owned loader (the
  "B2 brick-risk" mitigation: prove BEFORE it's the only path).
- **CX.3 char twins** (serialize_char_*_compressed / load_overlay_char_root_compressed). Same proofs.
- Prefer ZERO new unsafe (build via Arc/OverlayNode::with_child); any new unsafe → new inventory row + contract.
  Optional TLA OverlayDenseCodecRoundTrip; correspondence + back-compat tests are the empirical gate. Fully reversible.
  **Red-team focus:** back-compat (every legacy on-disk format — bucket/ArtNode roots, all 4 node sizes, compressed
  prefixes, value blobs incl. "" — read byte-equivalently to the owned loader); deep-term iterative expand; byte-identity/density.

---
## LEVEL 2 — compaction onto the codec; delete OverlayWriteMode / owned checkpoint arm / owned staging
- **L2.1 flip compact()** (byte-only; char has no compact()) ⚠️RT. compaction_impl.rs:110-370: replace create-staging
  + kill_switch_to_owned (209) + insert_impl_no_wal loop (231) + checkpoint (264) with serialize_overlay_snapshot_compressed
  (CX.1) of the source overlay snapshot into the temp file. compaction_snapshot enumeration (already overlay) stays the
  verify oracle. No owned staging trie / kill_switch / owned insert; density preserved. **Red-team:** new staging image
  byte-equivalent + complete; atomic-rename + WAL-sidecar dance unchanged; &mut self exclusivity → no past-snapshot WAL loss.
- **L2.2 delete OverlayWriteMode + kill_switch_to_owned + field + owned checkpoint arm** ⚠️RT. route_overlay() body →
  self.lockfree_root().is_some() (overlay installed iff routing — true for all eligible V; ineligible V never installs
  overlay). Delete OverlayWriteMode enum/field/seams, kill_switch_to_owned, wal_stamp_owned_regime, the owned checkpoint
  else-arm in checkpoint_route_split (incl. RES-4 assert), capture_owned_snapshot/publish_owned_and_reclaim + trait
  decls + byte serialize_root (superseded by CX.1). KEEP overlay arm + capture_overlay_snapshot (#41 untouched). Delete
  byte staging mutators (insert_impl_no_wal/insert_impl_core/...); char twins survive to L3. Retire the ~80 owned-white-box
  kill_switch_to_owned test callers as a GROUP (this commit). Prune any byte owned-walk UNSAFE rows + UNSAFE_CONTRACTS
  entries (set-equality). **Red-team:** with the owned arm gone, prove by construction lockfree_root().is_some() at every
  reachable checkpoint (the only counterexample was the kill-switched staging trie, now deleted).

---
## LEVEL 3 — keystone: reopen-scratch onto codec; re-point zipper; delete owned root field + holder types
- **L3.1 flip F5 reopen scratch onto CX.2/CX.3** ⚠️RT. load_root_immutable_seam (flip.rs:1557) + the F5 Overlay arm →
  load_overlay_root_compressed (no TrieRoot scratch). Keep corrupt-image→empty+WAL fallback. **Red-team:** every normal
  reopen now reads via the new codec with NO owned fallback (the brick-risk; CX back-compat proof is the precondition).
- **L3.2 re-point the struct zipper onto the overlay** ⚠️RT (4th consumer). zipper.rs has_path/is_final_at_path/
  get_children_at_path (+ char twin): replace inner.root.read() navigation with the overlay-backed root()/DictionaryNode
  (NodeInner::Overlay; fault OnDisk via fault_overlay_slot). Delete the log::warn! stub. ::new() must install an empty
  overlay (or adjust the in-memory zipper tests). Update zipper_language_correspondence to not rely on owned.
  **Red-team:** the formal gate's PublicDictionaryNodeTraversal + zipper correspondence (wrong overlay-zipper = silent
  wrong query results).
- **L3.3 delete owned root field + holder TYPES + owned readers/converters** ⚠️RT (biggest risk). Delete: root field
  (dict_impl.rs:280, mod.rs:429); TrieRoot/CharTrieRoot/CharTrieNodeInner; owned loaders load_root_from_disk(_with_arena);
  D1 seams (owned_first_units/owned_units_under/owned_units_with_values_under/owned_has_empty_term_value/clear_owned);
  build_overlay_root_from_owned; reestablish_overlay_from_owned; unrouted_* + byte contains_impl/get_value_impl + char
  owned_try_*/owned_root_guard/navigate_to_prefix_from/collect_terms_*; char owned mutators (insert_impl_no_wal*/
  insert_impl_core/try_increment_impl_no_wal) + byte twins; inner_to_overlay/overlay_to_inner. KEEP Node/CharNode RECORD
  types + single-node serde + load_overlay_node_from_disk + CX codec. compaction_snapshot owned fallback → overlay
  unconditional. PRUNE UNSAFE rows 23-24 (+ any other owned-walk rows) + UNSAFE_CONTRACTS same-commit (inventory 100→fewer).
  **Red-team:** (a) every deleted owned reader/converter has ZERO surviving caller in any feature combo; (b) the codec +
  L3.1 reopen + L3.2 zipper fully subsume every owned capability across V types; (c) high-concurrency real-disk soak
  (owned GONE; #41 witness — every committed key survives reopen); (d) the feature-off build.

## Formal deltas: L1 RecoveryRebuildOverlay (new); CX OverlayDenseCodecRoundTrip (correspondence mandatory, TLA optional);
L2/L3 re-RUN (not modify) the existing ConcurrentCheckpointSerialization/LockFreeDurableCheckpoint(+Eviction)/
OverlayEvictionCas/OverlayEvictionStale/LockFreeOverlayValueCas (all _Unsafe negative controls must still fire);
PublicDictionaryNodeTraversal covers L3.2. Prune UNSAFE rows 23-24 + owned-walk rows at L3.3 (no inventory change before).

## ⚠️RT data-loss-critical steps needing a dedicated adversarial red-team before implementation:
L0.3, L1, CX, L2.1, L2.2, L3.1, L3.2, L3.3. Mechanical (no RT): L0.1, L0.2.

## Critical files
- core/overlay/flip.rs (route_overlay:348; apply_recovered_operation_overlay:1031; drain_segments_into_overlay:1281;
  build_overlay_root_from_owned:873; reestablish_overlay_from_owned:946; convert_owned_to_overlay_on_reopen:1482;
  load_root_immutable_seam:1557; owned_* D1 seams; kill_switch_to_owned:581)
- persistent_artrie/overlay_checkpoint.rs (capture_owned_snapshot:113; serialize_root:515; serialize_overlay_node_to_disk:961)
  + core/overlay/checkpoint.rs (checkpoint_route_split:134 RES-4 owned arm; #41 ordering)
- persistent_artrie/compaction_impl.rs (kill_switch_to_owned:209; insert_impl_no_wal:231; checkpoint:264; snapshot fallback:396)
  + persistent_artrie{,_char}/mmap_ctor.rs (recover-family ctors — L1)
- persistent_artrie/dict_impl.rs (root:280; TrieRoot:484) + persistent_artrie_char/types.rs (CharTrieRoot:753;
  CharTrieNodeInner:468) + persistent_artrie_char/mod.rs (root:429; eviction owned arms:2141/2231; root():1330)
- persistent_artrie/zipper.rs (has_path:190/…; L3.2) + persistent_artrie/overlay_fault.rs (load_overlay_node_from_disk:49)
  + persistent_artrie_char/disk_io.rs (load_overlay_node_from_disk:404)
