# L3.3c-C2 — Owned-Tree Deletion COMPLETE (as-built, 2026-06-09)

The FINAL, irreversible step of the owned-tree-deletion campaign. The **byte** (`PersistentARTrie`)
and **char** (`PersistentARTrieChar`) variants now use the lock-free `OverlayNode<K, V>` as their
**sole** representation; the owned ART/bucket trees, their loaders, the owned checkpoint/serialize
paths, and the inner root `RwLock` are gone. Design: `slice3-l33c-execution-plan-2026-06-09.md`.

## What was deleted

**Char** (`src/persistent_artrie_char/`):
- `mod.rs`: `SharedCharARTrie::root` + the `PersistentARTrieCharNode` handle collapsed to overlay-only
  (`{overlay: Option<Arc<OverlayNode<CharKey,V>>>, overlay_faulter}`); deleted `from_trie`/`from_ptr`/
  `CharWalkGuard`/`CharNodeFaulter` + `debug_check_no_concurrent_mutation` + owned method arms; the
  `PersistentARTrieCharZipper::value` owned read routed through `MappedDictionaryNode::value`.
- `types.rs`: `CharTrieRoot` enum + impls + test. `disk_io.rs`: owned loader cluster (1272→408 LOC).
- `persist.rs` (3590→3062): `publish_durable_and_reclaim`, recursive `serialize_char_node_to_disk`,
  `overlay_to_inner`, `next_lsn_at_capture` field, + 2 owned test modules.
- `lockfree_cas.rs` merge twins; `prefetch_api` `prefetch_disk_refs_bounded`; `wal_helpers`
  `append_to_wal`/`sync_wal`.

**Byte** (`src/persistent_artrie/`, ~5400 LOC; delegated then independently audited):
- git-rm: `mutation_core.rs`, `dirty_tracking.rs`, `query_impl.rs`.
- `dict_impl.rs`: `TrieRoot` enum + `root: RwLock<TrieRoot>` field + `dirty_prefixes` field +
  `get_root_node` + `bytes_le` (→`#[cfg(test)]`). `parallel_merge.rs` collapsed to the overlay funnel.
- `transitions.rs` owned write/transition surface; `node_impl.rs` owned `NodeInner` arms + ctors;
  `disk_load.rs` owned loaders; + the misc dead owned helpers (serialize_impl/persistence_api/cursor_iter/
  overlay_write_mode/overlay_checkpoint `next_lsn_at_capture`).

## KEEP boundary (verified present + referenced)
`CharTrieNodeInner` + `inner_to_overlay` + `load_char_node_from_disk_lazy` + `load_overlay_node_from_disk` +
`enumerate_char_terms_from_disk` (char); `enumerate_terms_from_disk` + `SingleChildData` + `load_single_*` +
the `StringBucket` decode surface + `NodeInner::Overlay`/`new_overlay` + `ChildNode` decode helpers +
`serialize_node_to_disk_with_value_len` + `serialize_root_value_bytes` + overlay_fault `bench_*`/
`load_overlay_node_from_disk`/`evict_overlay_nodes` (byte). The CX codec (`serialize_overlay_snapshot_compressed`
+ `peel_chain` + `overlay_inner_single_node_with_prefix`) — KEEP (test-exercised; the L2 compaction codec).

## Reopen
Codec-only: `enumerate_terms_from_disk`/`enumerate_char_terms_from_disk` → `build_overlay_root_from_terms`.
Handles all 3 on-disk formats (overlay / CX-compressed / legacy `ROOT_TYPE_BUCKET` incl. empty term `""`).
BLOCKER#4 (corrupt node under a valid descriptor ⇒ replay the WAL, never skip the checkpoint) preserved (C1).

## UNSAFE
Char `UNSAFE_INVENTORY.tsv` 93→77 rows (pruned rows 4-16, 19-20, 22 — the owned swizzled-ptr/node-map/
box-ownership/walk-guard/public-node/mutation-core traversal boundaries + `char-persist-child-serialization`
for the deleted `serialize_char_node_to_disk`) + `UNSAFE_CONTRACTS.tsv` 61→53 (8 tags). **Byte delta = ZERO**
(the byte owned cluster contained no `unsafe`). `verify-unsafe-boundary-inventory.sh` set-equality holds.

## Loom
`tests/persistent_lockfree_f4_lock_hierarchy_loom.rs` re-pointed from `CK > merge_lock > OR > EC` to
`CK > merge_lock > EC` (the owned-root "OR" rung deleted; eviction/merge/checkpoint now take only their
lock + lock-free overlay CAS). 3 tests pass exhaustively under `--cfg loom`.

## Verification (all green)
`cargo nextest --all-features` **2706 / 0 / 3 skipped** · `--no-default-features` 0 err · doctests 154/0 ·
`verify-formal-correspondence.sh` 0 · `verify-unsafe-boundary-inventory.sh` 0 · `fmt --check` clean ·
loom `--cfg loom` 3/0 · **#41 soak 15/15 iterations clean** (multi-writer + checkpointer + evictor, no
deadlock/lost-write under `CK > merge_lock > EC`). Red-team (reopen/data-loss + over-deletion/coverage):
reopen/recovery/empty-string CONFIRMED-SOUND; added a deterministic overlay proper-prefix-insert + reopen
regression (`proper_prefix_insert_survives_live_and_reopen`) replacing the two deleted L3.3a merge-witnesses.
Cross-repo `cargo check` (liblevenshtein-rust + libgrammstein) clean (public API unchanged).

## Owned white-box tests
29 byte owned-internal-only tests deleted (dirty-tracking / owned `NodeInner` / bucket↔ART conversion — no
public-behavior coverage lost); char `owned_try_contains` asserts removed (owned tree gone), `dict_impl_char`
`test_inner_new` → overlay-emptiness, `dirty_checkpoint_correspondence` `persist_to_disk` → `checkpoint`.

## Follow-ons (owner GO'd, scoped post-C2)
1. **Overlay genericization (G5+, trait-first)** — DRY byte+char overlay logic into shared generic-over-`K`
   code (the now-identical node handles + read engines + seams), *before* vocab so vocab reuses it.
2. **Vocab flip campaign** — vocab's owned tree is still LIVE (never flipped); build its durable overlay +
   checkpoint + regime-aware recovery + production flip, then delete its owned tree (mirror char/byte).
3. Fix the pre-existing `parallel_merge_benchmarks.rs` F4-era breakage (`--all-targets` cleanliness).
