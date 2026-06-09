# L3.3c Execution Plan — Final Atomic Owned-Tree Deletion (2026-06-09)

Code-verified Plan-agent design (baseline `master` @ `128374e`, the doc-tx ACCUMULATE fix — the
L3.3c owner-decision gate is RESOLVED; `get_value_impl`'s last byte caller is gone). Implement BY
HAND. This is the FINAL, IRREVERSIBLE step of the owned-tree deletion campaign: delete the OWNED
in-memory trie entirely in favor of the lock-free OVERLAY (`OverlayNode<K,V>`).

## 0. Ground-truth corrections to the converged-plan/FINDING one-liners (verified vs actual code)

1. **There is no `fn convert_bucket_to_art`.** The FINDING named `bucket.rs:434/441` as
   `convert_bucket_to_art` calling `insert_impl` — FALSE. `bucket.rs:434/441` is
   `StringBucket::insert_key`/`insert` (a method on the on-disk bucket RECORD, not the trie cluster).
   The real owned bucket→ART promotion is `convert_root_bucket_to_art` (`mutation_core.rs:380`),
   private, called only from `insert_into_root`. **The self-referential cluster is entirely inside
   `mutation_core.rs` + `transitions.rs`, NOT `bucket.rs`.**
2. **The OR-lock is ALREADY collapsed at the wrapper level** (F4, task #29): `SharedARTrie = Arc<…>`
   (`mod.rs:394`), `SharedCharARTrie = Arc<…>` (`char/mod.rs:370`) — bare `Arc`, no outer `RwLock`.
   The stale comment at `dict_impl_char.rs:199` ("Arc<RwLock<…>>") is wrong. The `.read()/.write()/
   .get_mut()` shims are CONCRETE lock-free transparent guards (the public API — KEEP). **The only
   remaining owned lock is the INNER `self.root: RwLock<TrieRoot<V>>` FIELD — deleting the field
   deletes the lock. There is NO separate wrapper sweep.**
3. `serialize_root`/`capture_owned_snapshot` already gone; only `serialize_root_value_bytes`
   (`overlay_checkpoint.rs:1297`) survives — a KEEP.
4. `OverlayWriteMode`/`kill_switch_to_owned`/`wal_stamp_owned_regime` already gone (L3.3a).
5. `serialize_node_to_disk` (no-value wrapper, `serialize_impl.rs:57`) IS dead — DELETE it; KEEP
   `serialize_node_to_disk_with_value_len` (live: `overlay_checkpoint.rs:956/997`). (B2's "pinned
   serialize_node_to_disk" claim was about the value-len variant.)
6. **char field is `mod.rs:425`** (429 is `dirty`). Byte field `dict_impl.rs:280` is correct.

## 1. The owned-write CLUSTER boundary (the FINDING's prerequisite) — DELETE as a WHOLE

### 1a. BYTE
- `mutation_core.rs`: `insert_impl`(44) → `insert_impl_core`(88) → `insert_into_root`(111) →
  {`StringBucket::insert/insert_key`, `ChildNode::insert_with_value`, node `add_child`,
  `resolve_child_for_mutation_with_bm`, recursive `convert_root_bucket_to_art`(380) →
  `bucket_to_art_node`}; `remove_impl`(259) → `contains_impl` + `remove_impl_core`(280) →
  `remove_from_root`(299); `insert_impl_no_wal`(420), `remove_impl_no_wal`(430),
  `upsert_impl_no_wal`(441).
- `transitions.rs` owned-only: `ChildNode::insert_key`(503)/`insert_with_value`(586)/`remove_key`(679)/
  `contains_key`(735); `bucket_to_art_node`(125), `art_node_to_bucket`(252),
  `should_convert_bucket_to_art`(109), `should_merge_art_to_bucket`(243) + result structs.
- `dirty_tracking.rs`: `propagate_dirty_to_root`(74)/`clear_dirty_flags_recursive`(133)/
  `clear_child_dirty_flags_recursive`(146)/`record_dirty_path`(42) + `dirty_prefixes` field
  (`dict_impl.rs:345`) + its owned-checkpoint readers (`dict_impl.rs:1874-1902`).
- `node_impl.rs`: KEEP `PersistentARTrieNode` + `NodeInner::Overlay` + `new_overlay`(221) +
  `overlay_child_node`(236) + the overlay branches. DELETE owned `NodeInner::{ArtNode,Bucket,Root,Empty}`
  + `new_root`(146)/`new_root_with_children`(159)/`new_art_node`(179)/.../`empty`(261) +
  `BucketPosition` + owned match-arms. (Sole owned constructor `get_root_node` (`dict_impl.rs:505`)
  is now "never used".)
- `bucket.rs`: **KEEP the DECODE surface** (`from_bytes`/`len`/`get_entry`/`get_suffix`/`get_value`/
  `search`/`contains`/`iter`/`as_bytes`/`header` + `StringEntry`/`BucketHeader` — used by
  `enumerate_terms_from_disk`). DELETE the WRITE/SPLIT surface ONLY when the compiler confirms orphan
  (do NOT pre-emptively delete).
- `path_compression.rs`: VERIFY via compiler; KEEP `common_prefix_len`/`make_common_prefix` IF the CX
  codec reuses them, else DELETE-whole.

### 1b. CHAR (`mutation_core.rs` holds UNSAFE rows 19-20)
- `mutation_core.rs`: `preflight_*`(43/73/80/85), `try_insert_impl_no_wal`(98, row 20 `&mut *current`
  ×6), `try_insert_impl_no_wal_with_value`(134, **LIVE caller atomic_ops.rs:159 — see §8.2**),
  `try_remove_impl_no_wal`(176, row 19 `&*current`), `insert_impl_no_wal`(213),
  `insert_impl_no_wal_with_value`(224), `remove_impl_no_wal`(236).
- `types.rs`: KEEP `CharTrieNodeInner`(468) + `get_child`/`get_child_mut`/`iter_children`/child-map
  raw ops (UNSAFE 22-30, the fault-in decode). DELETE only `get_or_create_child`(680)/`remove_child`(717)
  when orphaned.

## 2. Fail-closed DELETION ORDER (author C2 in this internal order; the FIELD dies LAST)
Principle: collapse LIVE arms first, delete orphans inward, delete the field/type LAST — so the
compiler flags any missed LIVE caller as a clear error instead of a buried borrow storm.
1. Collapse the LIVE deferred owned arms (their early `return <overlay>` makes the owned tail dead):
   byte `shared_trait_impl.rs` insert/insert_with_value/remove/remove_prefix tails;
   `parallel_merge.rs::merge_from_parallel`; `atomic_ops.rs::get_value_bytes` + `unrouted_*` shims;
   `lockfree_cas.rs:1476`; char `query_api::get_value`/`owned_try_get`; char `prefix_api` 3 arena
   methods; char `mod.rs::SharedCharARTrie::root` owned tail + the checkpoint owned tail
   (`persist.rs:303 capture_snapshot`); byte `arena_iter.rs` inline `self.root` tails.
2. Delete orphaned owned READERS: byte `contains_impl`/`get_value_impl`/`query_impl`/`*_in_child`/
   `unrouted_*`/cursor collectors; char `owned_try_contains`/`owned_try_get`/`owned_get`/
   `owned_iter_prefix*`/`owned_root_guard`/`navigate_to_prefix_*`/`collect_terms_*`.
3. Delete the owned-write CLUSTER (§1).
4. Delete owned LOADERS: byte `load_root_from_disk`(disk_load.rs:89)/`load_root_from_disk_with_arena`(333)/
   `load_art_node_with_children_from_arena`(711)/`load_child_from_disk_with_arena`(759); char
   `load_root_from_disk`(disk_io.rs:36) + owned `resolve_swizzled_ptr`/`_mut`(1123/1213) + owned
   `load_char_node_from_disk*`(401/744/852). **KEEP `load_char_node_from_disk_lazy`(508) +
   `load_overlay_node_from_disk`(625).** This forces BLOCKER#4 (§4).
5. Delete holder TYPES + FIELD LAST: `TrieRoot`(dict_impl.rs:464), `CharTrieRoot`(char/types.rs:753);
   then `self.root` (byte dict_impl.rs:280, char mod.rs:425) — deletes the inner RwLock. **KEEP
   `CharTrieNodeInner`.**
6. Delete `overlay_to_inner`(char persist.rs:1884) + its tests (~3207-3251, refs 2286/2300).
7. Migrate/retire owned white-box tests + RETIRE/re-point `persistent_lockfree_f4_lock_hierarchy_loom.rs`
   (OR-lock premise gone).
8. PRUNE UNSAFE rows 4-16,19-20 (§6).

## 3. KEEP boundary (production-caller proof)
`CharTrieNodeInner`(types.rs:468) ← `disk_io.rs:633` fault-in; `inner_to_overlay`(persist.rs:1960) ←
`disk_io.rs:633`; `load_char_node_from_disk_lazy`(508); `enumerate_terms_from_disk`(disk_load.rs:507)
← `f5_loader.rs:122`; `load_overlay_root_compressed` ← `load_root_immutable`(f5_loader.rs:65);
`load_overlay_node_from_disk` (byte overlay_fault.rs:157 / char disk_io.rs:625) ← lockfree_cas
fault-in + checkpoint; `SingleChildData` + `load_single_art_node_data`(disk_load.rs:847)/
`load_single_child_data`(893) ← the enumerator (uses these, NOT `ChildNode`);
`serialize_node_to_disk_with_value_len`; `serialize_root_value_bytes`(1297); `StringBucket` decode
surface; `PersistentARTrieNode`+`NodeInner::Overlay`+`new_overlay` ← dictionary_traits.rs:40;
`PersistentARTrieCharNode`+Send/Sync (UNSAFE 17-18) ← char root() overlay-only.
**Orphan oracle:** after C2, `cargo check` dead-code must show NONE of these. A surviving
`use …transitions::ChildNode` means that file is owned residue → delete it in C2.

## 4. BLOCKER#4 — byte in-loader Err→empty fallback (port char's pattern)
Do NOT use a `read_root_descriptor().is_ok()` eager probe (a valid descriptor over a corrupt NODE
reads true → wrong checkpoint-skip → fails `byte_invalid_root_descriptor_replays_wal_without_checkpoint_skip`
+ silent WAL-tail loss).
- byte `load_root_immutable`(f5_loader.rs:60) `Result<usize>` → **`Result<(usize,bool)>`** (bool =
  `image_loaded`); a corrupt/Err load returns `(0,false)` with an EMPTY overlay (mirror char
  f5_loader.rs:65); `root_ptr==0` ⇒ `(0,false)`.
- byte `mmap_ctor.rs`: DELETE the eager pre-load (`load_root_from_disk_with_arena`, ~421-432) + its
  `match` (~569-576); replace `was_loaded_from_disk=loaded_root.is_some()` with `root_ptr!=0`; compute
  `effective_loaded=(root_ptr!=0)&&image_loaded`; thread into `effective_root_ptr`/
  `effective_checkpoint_lsn`/drain-skip; drop the `root: RwLock::new(initial_root)` struct line.
- identical in byte `io_uring_ctor.rs` (~196/313/318/375).
- Keep green: `root_descriptor_reopen_correspondence.rs::byte_invalid_root_descriptor_replays_wal_without_checkpoint_skip`
  + `byte_*_owned_regime_reopen` + reopen-correspondence + arbitrary-V suites.

## 5. OR-lock collapse (CORRECTED) — field-only
No wrapper-type change/sweep (done in F4). Delete the field `self.root` (byte dict_impl.rs:280, char
mod.rs:425) ⇒ inner RwLock gone. All `self.root.read()/.write()/.get_mut()` sites live inside code
deleted in §2. KEEP the `SharedTrieAccess::read()/write()` transparent guards (public API). `sync_compat::RwLock`
stays (wraps BufferManager/ArenaManager). **#41 soak:** multi-writer (insert/upsert/increment CAS) +
checkpointer + evictor under `timeout` — confirm `CK > merge_lock > EC` has no cycle without the OR rung.

## 6. UNSAFE prune — EXACT rows + the 2-file edit (set-equality gate)
PRUNE `UNSAFE_INVENTORY.tsv` rows: 4-5 (`char-disk-swizzled-pointer-resolution`), 6-8
(`char-disk-node-map-resolution`), 9-10 (`char-disk-box-ownership`), 11-12
(`char-walk-guard-faulter-traversal`), 13-16 (`char-public-node-traversal`), 19
(`char-mutation-core-traversal`), 20 (`char-mutation-core-unique-borrow`). DELETE the 7 matching
`UNSAFE_CONTRACTS.tsv` tag rows. **KEEP rows 1-3, 17-18 (Send/Sync), 21, 22-30 (fault-in/inner_to_overlay
child-map), ≥31.** Byte unsafe delta = ZERO. Rows 11-16 retire only if the char public-node OWNED arm
(node/faulter/pin fields + from_trie/from_ptr + owned else-branches + CharWalkGuard) is fully deleted →
`PersistentARTrieCharNode` overlay-only (any surviving owned `unsafe { &*ptr }` → set-equality fails =
the desired fail-closed signal).

## 7. Commit slicing — TWO commits
- **C1 (REVERSIBLE, FIRST): BLOCKER#4 fallback-port** (§4) while the owned loader + `TrieRoot` field
  still exist (drop only the eager pre-load). Behavior-preserving, fully gate-able, de-risks the
  keystone by proving the codec-only reopen path works BEFORE the owned rep is gone.
- **C2 (IRREVERSIBLE, atomic): the owned deletion** — §2 steps 1-8 as ONE commit (the cluster is
  self-referential + entangled with the field/TrieRoot; the UNSAFE set-equality requires rows+code to
  move together). Mirrors the campaign's reversible-prep-then-irreversible-keystone discipline.

## 8. Red-team focus (data-loss-critical)
1. **Reopen after loader deletion (brick-risk):** `enumerate_terms_from_disk` becomes the SOLE reopen
   path. Re-RUN the CX back-compat proof; verify all 3 formats (overlay/CX-compressed/legacy bucket
   incl. "") — B2 pins ROOT_TYPE_BUCKET; corrupt-node-under-valid-descriptor replays WAL (BLOCKER#4).
2. **⚠️ #1 TRAP — char recovery applier (`atomic_ops.rs:138/159`):** `try_increment_impl_no_wal` calls
   `owned_get` + `try_insert_impl_no_wal_with_value` and "rebuilds the OWNED tree during crash recovery"
   (`apply_core_recovered_operation_no_wal` → BatchIncrement). Deleting the char cluster compile-forces:
   (a) if `apply_core_recovered_operation_no_wal` is ALREADY DEAD (L1 redirected char recovery to the
   overlay drain — **VERIFY against task #40 / the L1 state BEFORE C2**), delete it too; (b) if LIVE,
   REWRITE onto the overlay counter (`route_increment`/`value_read_faulting`) BEFORE deleting — else a
   `<i64>`/`<u64>` counter reopen silently UNDER-COUNTS. Gate with a recovery→reopen counter-accumulate test.
3. **Field deletion vs concurrent eviction:** prove no surviving `&self` path touches the deleted owned
   root; run the #41 soak; RETIRE/re-point the f4-lock-hierarchy loom test.
4. **Accidentally-orphaned KEEP symbols:** after C2, dead-code warnings must NOT name StringBucket
   decode / SingleChildData / serialize_node_to_disk_with_value_len / inner_to_overlay /
   CharTrieNodeInner child-map / NodeInner::Overlay.
5. **bucket.rs / path_compression.rs over-deletion:** let the compiler name orphans; KEEP
   common_prefix_len/make_common_prefix + StringBucket decode unless dead-code confirms orphan.

## 9. Gate (per commit)
full nextest (C1≈2636; C2 shifts after test migration — record) + `--no-default-features` +
`--all-features` + `--doc` + `verify-formal-correspondence.sh` (0) + `verify-unsafe-boundary-inventory.sh`
(C1: ZERO delta; **C2: delta = −rows{4-16,19-20} + 7 contract tags**, fail-closed if any owned `unsafe`
survives) + `fmt --check` + cross-repo READ-ONLY `cargo check` (liblevenshtein-rust + libgrammstein;
confirm no downstream names TrieRoot/CharTrieRoot/load_root_from_disk/get_root_node/the owned readers).

## Critical files
byte: `mutation_core.rs`, `transitions.rs`, `dict_impl.rs` (TrieRoot:464/field:280/get_root_node:505),
`mmap_ctor.rs`+`io_uring_ctor.rs`+`f5_loader.rs` (BLOCKER#4), `disk_load.rs` (DELETE loaders
89/333/711/759; KEEP enumerate:507 + SingleChildData 847/893), `node_impl.rs`, `dirty_tracking.rs`,
`bucket.rs` (split), `path_compression.rs` (verify).
char: `mutation_core.rs` (rows 19-20), `mod.rs` (field:425, owned node arm + CharWalkGuard + from_trie,
rows 11-16), `types.rs` (CharTrieRoot:753 DELETE; **CharTrieNodeInner:468 KEEP**), `disk_io.rs` (DELETE
owned load_root_from_disk:36 + resolve_swizzled_ptr*:1123/1213, rows 4-10; **KEEP
load_char_node_from_disk_lazy:508 + load_overlay_node_from_disk:625**), `atomic_ops.rs:138/159` (§8.2
recovery applier — THE decision point), `query_api.rs`/`prefix_api.rs`/`prefix_helpers.rs` (readers/arms).
`formal-verification/UNSAFE_INVENTORY.tsv` + `UNSAFE_CONTRACTS.tsv` (prune rows 4-16,19-20 + 7 tags).
