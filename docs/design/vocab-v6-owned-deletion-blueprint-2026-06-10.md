# Vocab V6 — owned-tree deletion + single-lock-free completion (execution blueprint)

Red-teamed Plan-agent blueprint (2026-06-10) for deleting the now-DEAD owned tree from
`PersistentVocabARTrie` and finishing the single-lock-free transition. Mirrors C2 (byte/char,
commit `b9570ca`). **ONE irreversible commit** — the build is red until ALL constructors + trait
impls are reconciled together (owned fields are woven into every `Self {…}` builder + trait impl).

Pre-state (committed, 14 commits incl. 406c566): every production ctor flips at construction →
`route_overlay()` always true → every `else`-arm is dead. Override already removed.

## Execution checklist (dependency order — §G)

1. **RELOCATE `build_disk_char_node_static`** (disk_io.rs:386) → `overlay_serialize.rs` as `pub(super)` — overlay_serialize.rs:190 calls it (R1). DO THIS FIRST.
2. **Collapse route-split else-arms to overlay-only** (KEEP `&mut self` — §D, zero trait-signature changes): `mutation_api.rs` insert→`insert_overlay`, insert_batch→per-term `insert_overlay`, insert_with_index→`insert_with_index_overlay` (delete the owned helpers append_vocab_insert_wal/append_vocab_batch_wal/sync_vocab_wal_after_append/next_unassigned_index_from/validate_index_insert/insert_with_index_no_wal/wal_unavailable_error/map_wal_error); `query_api.rs` get_index→`get_index_lockfree`, get_term→reverse_term_map (delete reconstruct_term/reserve_node_map); `path_query.rs` iter_terms/iter_terms_with_prefix→overlay-only, **REWORK get_root_children/get_children_at_path/is_final_at_path over the overlay** (char overlay_read.rs template, R8); `persistence_api.rs` checkpoint→`checkpoint_overlay`, rework rotate_wal/sync_to_disk_async (drop reverse_index+bloom flush), delete cache_stats.
3. **Toggle removal** (`lockfree_cas.rs`): `enable_lockfree`→`pub(crate)` (flip_to_overlay still calls it); DELETE `insert_cas`, `is_lockfree_enabled`, `merge_lockfree_to_persistent`. KEEP try_insert_lockfree_path/find_in_lockfree_trie/insert_lockfree_recursive/create_lockfree_path/cas_retries (overlay-live).
4. **Struct fields + ctors** (`dict_impl.rs` + mmap_ctor + io_uring_ctor): DELETE fields `root`/`reverse_index`/`reverse_cache`/`node_map`/`next_slot`/`bloom_filter`. KEEP all else incl. `dirty`/`reverse_term_map`/`lockfree_*`/`commit_seq`/`committed_watermark`/`epoch_manager`. Remove those fields + the owned root construction (`Box::into_raw/from_raw`) + `.idx VocabReverseIndex::create` from all 4 builders (create mmap/io_uring + open_snapshot/open_with_io_uring); collapse `!is_overlay` reopen branch (R4). Rewrite Clone (overlay fields None). KEEP explicit `unsafe impl Send/Sync` (rows 39,40).
4b. **DELETE FILE `bloom_filter_api.rs`** + the `create_with_bloom`/`create_with_start_index_and_bloom` ctors + mod.rs decl. (Verify no external `might_contain`/`has_bloom_filter` caller.)
5. **DELETE FILES**: disk_io.rs (after relocating §1), reverse_index.rs, reverse_cache.rs, iterators.rs, serialization.rs. `types.rs`: DELETE `VocabTrieNode`(320-622)/`VocabTrieRoot`(625-656)/`FLAG_HAS_PARENT_POINTER`; KEEP VocabTrieFileHeader/VOCAB_TRIE_MAGIC/VOCAB_HEADER_VERSION_V1+V2/crc32_header/NodeRef-reexport. Drop mod.rs mod-decls + re-exports.
6. **mod.rs**: DELETE `evict_node_at_path`(875-976) + rework `EvictableARTrie` evict-callback to no-op (char template, overlay never evicts finals); update BijectiveDictionary doc (parent-pointer→reverse_term_map); DELETE the 4+helpers unsafe white-box tests (1324-1478) + the test ctor `heap_only_vocab_for_unsafe_tests`. KEEP public-API tests.
7. **DELETE lockfree.rs + concurrent.rs** + mod.rs decls (168,169) + re-exports (201,203).
8. **External tests (same commit)**: tests/persistent_artrie_formal_correspondence.rs (drop ConcurrentVocabARTrie/LockFreeVocab/VocabTrieNode imports + 2 Send/Sync asserts + 5 VocabTrieNode white-box tests); DELETE tests/concurrent_checkpoint_publication_correspondence.rs (whole file); tests/persistent_vocab_checkpoint_correspondence.rs (delete 5 ignored + 3 reverse-index tests); tests/persistent_rewrite_compaction_correspondence.rs (delete the 1 ignored sidecar test). NOTE: libgrammstein_support/persistent_artrie_concurrent/lockfree_flip_benchmark `insert_cas`/`enable_lockfree` hits are BYTE/CHAR — NO change (verify `rg PersistentVocabARTrie`).
9. **UNSAFE ledgers (LAST)**: see below.
10. **Delete the 11 `#[ignore]`'d tests + collateral** (2 insert_cas tests dict_impl 1525/1547, the 3 reverse-index correspondence tests).

## UNSAFE set-equality (the hard gate — scripts/verify-unsafe-boundary-inventory.sh)
- **REMOVE 31 inventory rows**: 37,38 (concurrent.rs Send/Sync), 41-46 (disk_io.rs), 47,48 (io_uring owned root), 49,50 (lockfree.rs Send/Sync), 51,52 (mmap owned root), 53-56 (mod.rs evict), 57,58 (query_api reconstruct_term), 59-61 (reverse_index.rs mmap), 62-69 (types.rs VocabTrieNode).
- **KEEP 2 inventory rows**: 39,40 (dict_impl.rs `unsafe impl Send/Sync for PersistentVocabARTrie<S>`).
- **REMOVE 20 contract rows**: concurrent-vocab-rwlock-thread-contract; vocab-child-{box-reclaim,box-replacement,raw-map-iteration,read-only-traversal,unique-mutation}; vocab-disk-{box-ownership,child-serialization,node-map-rebuild-contract,root-box-reclaim,root-serialization}; vocab-eviction-{child-drop,target-reference}; vocab-io-uring-root-box-reclaim; vocab-lockfree-thread-contract; vocab-mmap-root-box-reclaim; vocab-public-node-mutation; vocab-query-node-map-traversal; vocab-reverse-index-{map,remap}-lifetime.
- **KEEP 1 contract row**: persistent-vocab-storage-thread-contract.
- **Verify after edits**: `rg '(unsafe impl|unsafe fn|unsafe\s*\{)' src/persistent_vocab_artrie -g '*.rs'` = EXACTLY 2 lines (dict_impl Send+Sync). Any remaining Box::from_raw/&*/&mut* = a missed deletion.
- Do NOT delete any `.tla` (PointerOwnership.tla etc. may be shared with byte/char).

## Red-team (verify each)
- R1 build_disk_char_node_static relocated (else checkpoint serialize fails) → overlay_serialize round-trip test.
- R2 reestablish_overlay_from_image is owned-free (reads arenas, not self.root) → flip_checkpoint_reopen_roundtrip + flip_crash_recovery.
- R3 get_term reverse coverage: reverse_term_map populated by insert_overlay/insert_with_index_overlay/reestablish/replay incl. "" → empty-string + unicode reverse tests.
- R4 v1 (owned) files unreadable post-deletion — ACCEPTED (owner: legacy rebuilt; NO v1 files in production — all create* write V2). Enumeration fails loudly not silently.
- R5 mmap open_with_recovery: delete owned WAL-replay loop, keep replay_wal_into_overlay_rank_aware → wal-recovery tests.
- R6 entry_count: replay guards `get_index_lockfree.is_none()` (count only new) → crash-recovery len assert.
- R7 trait honesty (Bijective/Mapped) → bijective_trait_invariant/vocab_trait_honesty/dictionary_law_correspondence.
- R8 get_root_children/Dictionary::root/iter_prefix reworked to overlay (not deleted).
- R9 set-equality gate = THE constraint (kept rows 39,40 untouched).
- R10 `--no-default-features` + `--all-features` (io_uring_ctor cfg-gated).
- R11 doctests (lockfree/concurrent doc examples deleted with files); update mod.rs module-doc prose to overlay.
- R12 Clone sets overlay None (empty until rebuilt) — matches char; no production .clone().
- R13 reverse_term_map is Option but always Some post-ctor (enable_lockfree/reestablish); get_term guards gracefully.

## Gates (b9570ca parity): cargo build / --no-default-features / --all-features; nextest; cargo test --doc; cargo fmt --check; verify-unsafe-boundary-inventory.sh (set-equality); formal correspondence; loom overlay model; #41 soak; cross-repo liblevenshtein-rust + libgrammstein build.

Full verbatim blueprint (every file:line) is in the session transcript (Plan agent result, 2026-06-10).
