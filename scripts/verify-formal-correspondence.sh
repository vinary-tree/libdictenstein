#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

FORMAL_RSS_LIMIT_BYTES="${FORMAL_RSS_LIMIT_BYTES:-8589934592}"

run_capped() {
  if [ "${FORMAL_RSS_LIMIT_BYTES}" != "0" ] && command -v prlimit >/dev/null 2>&1; then
    prlimit --rss="${FORMAL_RSS_LIMIT_BYTES}" -- "$@"
  else
    "$@"
  fi
}

echo "== Unsafe boundary inventory =="
run_capped bash scripts/verify-unsafe-boundary-inventory.sh

echo "== Rust correspondence tests =="
run_capped cargo test --test dictionary_law_correspondence
run_capped cargo test --test dynamic_dawg_mutation_correspondence
run_capped cargo test --test dynamic_dawg_u64_correspondence
run_capped cargo test --test bloom_filter_correspondence
run_capped cargo test --test double_array_trie_correspondence
run_capped cargo test --test unsafe_boundary_contracts
run_capped cargo test --test zipper_language_correspondence
run_capped cargo test --test valued_set_combinator_correspondence
run_capped cargo test --features lling-llang --test valued_set_combinator_correspondence
run_capped cargo test --features pathmap-backend --test pathmap_factory_correspondence
run_capped cargo test --test substring_candidate_correspondence
run_capped cargo test --test scdawg_occurrence_correspondence
run_capped cargo test --test fuzzy_candidate_coverage_correspondence
run_capped cargo test --features serialization --test serialization_correspondence
run_capped cargo test --features serialization --test serialization_value_roundtrip
run_capped cargo test \
  --features "serialization protobuf compression" \
  --test protobuf_compression_correspondence
run_capped cargo test --features persistent-artrie --test dictionary_law_correspondence
run_capped cargo test --features persistent-artrie --test unsafe_boundary_contracts
run_capped cargo test --features persistent-artrie --test zipper_language_correspondence
run_capped cargo test --features persistent-artrie --test persistent_artrie_formal_correspondence
run_capped cargo test --features persistent-artrie --test persistent_prefix_correspondence
run_capped cargo test --features persistent-artrie --test relative_encoding_correspondence
run_capped cargo test --features persistent-artrie --test arena_manager_correspondence
run_capped cargo test --features persistent-artrie --test dedup_arena_correspondence
run_capped cargo test --features persistent-artrie --test root_descriptor_reopen_correspondence
run_capped cargo test --features persistent-artrie --test persistent_lazy_mutation_correspondence
run_capped cargo test --features persistent-artrie --test persistent_wal_atomicity_correspondence
run_capped cargo test --features persistent-artrie --test checkpoint_retention_correspondence
run_capped cargo test --features persistent-artrie --test dirty_checkpoint_correspondence
run_capped cargo test --features persistent-artrie --test wal_segment_lifecycle_correspondence
run_capped cargo test --features persistent-artrie --test recovery_planner_correspondence
run_capped cargo test --features persistent-artrie --test recovery_replay_completeness_correspondence
run_capped cargo test --features persistent-artrie --test persistent_compaction_correspondence
run_capped cargo test --features persistent-artrie --test persistent_rewrite_compaction_correspondence
run_capped cargo test --features persistent-artrie --test persistent_vocab_wal_atomicity_correspondence
run_capped cargo test --features persistent-artrie --test persistent_vocab_checkpoint_correspondence
run_capped cargo test --features persistent-artrie --test concurrent_checkpoint_publication_correspondence
(
  export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
  run_capped cargo test --features persistent-artrie --test persistent_merge_correspondence
)
(
  export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
  run_capped cargo test \
    --features "persistent-artrie parallel-merge" \
    --test persistent_merge_correspondence
)
run_capped cargo test --features persistent-artrie --test persistent_artrie_storage_correspondence
run_capped cargo test --features persistent-artrie --test persistent_artrie_loom_correspondence
run_capped cargo test \
  --features persistent-artrie \
  --lib \
  persistent_vocab_artrie::tests::vocab_
run_capped cargo test \
  --features "persistent-artrie group-commit" \
  --test persistent_artrie_formal_correspondence \
  group_commit_writes_returned_lsns_in_wal_order

if [ "${RUN_MIRI:-0}" = "1" ]; then
  echo "== Miri unsafe-boundary checks =="
  miri_cargo=(cargo)
  if [ -n "${FORMAL_MIRI_TOOLCHAIN:-}" ]; then
    miri_cargo=(cargo "+${FORMAL_MIRI_TOOLCHAIN}")
  fi

  if ! "${miri_cargo[@]}" miri --version >/dev/null 2>&1; then
    echo "RUN_MIRI=1 was set, but cargo miri is not available" >&2
    if [ -z "${FORMAL_MIRI_TOOLCHAIN:-}" ]; then
      echo "Set FORMAL_MIRI_TOOLCHAIN=nightly to use an installed nightly toolchain" >&2
    fi
    exit 1
  fi

  if [ "${FORMAL_MIRI_STRICT_PROVENANCE:-1}" = "1" ]; then
    export MIRIFLAGS="-Zmiri-strict-provenance ${MIRIFLAGS:-}"
  fi

  if [ "${FORMAL_MIRI_DISABLE_ISOLATION:-1}" = "1" ]; then
    export MIRIFLAGS="-Zmiri-disable-isolation ${MIRIFLAGS:-}"
  fi

  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    vocab_child_remove_transfers_box_ownership_once
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    vocab_insert_child_replaces_without_aliasing_old_box
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    vocab_clone_deep_copies_child_boxes
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    vocab_get_or_create_child_mutation_keeps_unique_raw_borrow
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    char_child_remove_transfers_box_ownership_once
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    char_insert_child_replaces_without_aliasing_old_box
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    char_clone_deep_copies_child_boxes
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    char_get_or_create_child_mutation_keeps_unique_raw_borrow
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    swizzled_pointer_raw_extraction_is_gated_by_in_memory_state
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    swizzled_pointer_losing_lazy_load_candidate_can_be_reclaimed_once
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --lib \
    persistent_artrie_core::swizzled_ptr::tests
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --lib \
    persistent_vocab_artrie::tests::vocab_leaf_eviction_invalidates_node_map_entry_before_drop
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --lib \
    persistent_vocab_artrie::tests::vocab_leaf_eviction_keeps_sibling_queries_on_live_nodes
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --lib \
    persistent_vocab_artrie::tests::vocab_heap_node_map_parent_chain_tracks_live_nodes
  run_capped "${miri_cargo[@]}" miri test \
    --features persistent-artrie \
    --lib \
    persistent_artrie_core::buffer_manager::tests::fixed_buffer_registration_covers_write_guard_mutation_and_flush
else
  echo "Skipping Miri unsafe-boundary checks; set RUN_MIRI=1 to enable them"
fi

if [ "${RUN_IO_URING:-0}" = "1" ]; then
  echo "== io_uring storage correspondence tests =="
  run_capped cargo test \
    --features "persistent-artrie io-uring-backend" \
    --lib \
    io_uring_completion_
  run_capped cargo test \
    --features "persistent-artrie io-uring-backend" \
    --test persistent_artrie_storage_correspondence
else
  echo "Skipping io_uring storage correspondence checks; set RUN_IO_URING=1 to enable them"
fi

echo "== Rocq proofs =="
run_capped make -C formal-verification/rocq -j1

echo "== TLA+ syntax checks =="
if command -v tla2sany >/dev/null 2>&1; then
  (
    cd formal-verification/tla+
    for module in \
      DocumentTransactions \
      AsyncWalGroupCommit \
      VersionLifecycle \
      DurabilityFrontier \
      PointerOwnership \
      VocabPersistenceOwnership \
      MmapBlockStorage \
      StorageSyscallOutcome \
      IoUringFixedBufferOwnership \
      IoUringSqeCqeLifecycle \
      LockFreeARTrieLinearizability \
      LockFreeIndexedOverlay \
      ConcurrentCheckpointPublication \
      ConcurrentVocabLinearizability \
      ByzantineStorage \
      HotStuffConsensus
    do
      run_capped tla2sany "${module}.tla"
    done
  )
else
  echo "Skipping SANY checks: tla2sany is not on PATH"
fi

if [ "${RUN_TLC:-0}" = "1" ]; then
  echo "== TLC bounded model checks =="
  if ! command -v tlc >/dev/null 2>&1; then
    echo "RUN_TLC=1 was set, but tlc is not on PATH" >&2
    exit 1
  fi

  (
    cd formal-verification/tla+
    for module in \
      DocumentTransactions \
      AsyncWalGroupCommit \
      VersionLifecycle \
      DurabilityFrontier \
      PointerOwnership \
      VocabPersistenceOwnership \
      MmapBlockStorage \
      StorageSyscallOutcome \
      IoUringFixedBufferOwnership \
      IoUringSqeCqeLifecycle \
      LockFreeARTrieLinearizability \
      ConcurrentCheckpointPublication \
      ConcurrentVocabLinearizability \
      ByzantineStorage \
      HotStuffConsensus
    do
      run_capped tlc -workers 1 -config "${module}.cfg" "${module}.tla"
    done
    run_capped tlc -workers 1 -config LockFreeIndexedOverlayCounter.cfg LockFreeIndexedOverlay.tla
    run_capped tlc -workers 1 -config LockFreeIndexedOverlayVocabulary.cfg LockFreeIndexedOverlay.tla
  )
else
  echo "Skipping TLC model checking; set RUN_TLC=1 to enable bounded TLC runs"
fi
