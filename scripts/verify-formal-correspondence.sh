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

echo "== Rust feature profile compile checks =="
run_capped cargo test --no-run
run_capped cargo test --all-features --no-run

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
run_capped cargo test --features persistent-artrie --test persistent_read_snapshot_correspondence
run_capped cargo test --features persistent-artrie --test persistent_char_node_layout_correspondence
# L3.3: the owned-machinery correspondence tests (persistent_char_ebr / persistent_lazy_mutation /
# persistent_bulk_mutation / persistent_lockfree_merge / persistent_char_eviction_proptest) were
# retired with the owned tree — their owned-walk EBR / owned lazy-load / owned-drain / owned-rep
# eviction behavior no longer exists; the overlay equivalents are covered by the surviving suites.
run_capped cargo test --features persistent-artrie --test dictionary_node_reopen_traversal_correspondence
run_capped cargo test --features persistent-artrie --test relative_encoding_correspondence
run_capped cargo test --features persistent-artrie --test arena_manager_correspondence
run_capped cargo test --features persistent-artrie --test dedup_arena_correspondence
run_capped cargo test --features persistent-artrie --test root_descriptor_reopen_correspondence
run_capped cargo test --features persistent-artrie --test persistent_wal_atomicity_correspondence
run_capped cargo test --features persistent-artrie --test persistent_transaction_increment_correspondence
run_capped cargo test --features persistent-artrie --test persistent_lockfree_overlay_proptest
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
run_capped cargo test --features persistent-artrie --test persistent_char_eviction_correspondence
run_capped cargo test --features persistent-artrie --test persistent_char_eviction_registry_correspondence
run_capped cargo test \
  --features persistent-artrie \
  --lib \
  persistent_artrie_char::eviction_registry_tests
run_capped cargo test --features persistent-artrie --test persistent_shared_concurrency_correspondence
run_capped cargo test --features persistent-artrie --test persistent_public_durability_policy_correspondence
run_capped cargo test --features persistent-artrie --test persistent_public_lifecycle_correspondence
run_capped cargo test --features persistent-artrie --test persistent_end_to_end_trace_correspondence
run_capped cargo test --features persistent-artrie --test epoch_checkpoint_recovery_correspondence
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
run_capped cargo test --features persistent-artrie --test persistent_lockfree_overlay_loom
run_capped cargo test --features persistent-artrie --test persistent_lockfree_durable_loom
run_capped cargo test \
  --features persistent-artrie \
  --lib \
  persistent_vocab_artrie::tests::vocab_
run_capped cargo test \
  --features "persistent-artrie group-commit" \
  --test persistent_artrie_formal_correspondence \
  group_commit_writes_returned_lsns_in_wal_order
run_capped cargo test \
  --features "persistent-artrie group-commit" \
  --test persistent_public_lifecycle_correspondence \
  group_commit_concurrent_writes_return_lsn_written_for_same_record

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
      BufferPageLease \
      ReverseIndexMmap \
      IoUringFixedBufferOwnership \
      IoUringSqeCqeLifecycle \
      LockFreeARTrieLinearizability \
      LockFreeIndexedOverlay \
      LockFreeCounterMergeAtomicity \
      ConcurrentCheckpointPublication \
      LockFreeDurableCheckpoint \
      LockFreeDurableCheckpointEviction \
      EvictionRegistryPublication \
      SharedPersistentConcurrency \
      PublicDurabilityPolicy \
      PersistentEndToEndTrace \
      PublicReadSnapshotTraversal \
      CharNodeV2Layout \
      ConcurrentVocabLinearizability \
      EpochCheckpointRecovery \
      PersistentCharBulkMutationRecovery \
      PersistentTransactionIncrementRecovery \
      ByzantineStorage \
      HotStuffConsensus \
      PublicDictionaryNodeTraversal \
      EvictionWalkEBR \
      OverlayEvictionCas \
      OverlayEvictionStale \
      LockFreeOverlayRemoveCas \
      LockFreeOverlayDurableReplay \
      LockFreeOverlayValueCas \
      ConcurrentCheckpointSerialization
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
      BufferPageLease \
      ReverseIndexMmap \
      IoUringFixedBufferOwnership \
      IoUringSqeCqeLifecycle \
      LockFreeARTrieLinearizability \
      LockFreeCounterMergeAtomicity \
      ConcurrentCheckpointPublication \
      LockFreeDurableCheckpoint \
      LockFreeDurableCheckpointEviction \
      EvictionRegistryPublication \
      SharedPersistentConcurrency \
      PublicDurabilityPolicy \
      PersistentEndToEndTrace \
      PublicReadSnapshotTraversal \
      CharNodeV2Layout \
      ConcurrentVocabLinearizability \
      EpochCheckpointRecovery \
      PersistentCharBulkMutationRecovery \
      PersistentTransactionIncrementRecovery \
      ByzantineStorage \
      HotStuffConsensus \
      PublicDictionaryNodeTraversal \
      EvictionWalkEBR \
      OverlayEvictionCas \
      OverlayEvictionStale \
      LockFreeOverlayRemoveCas \
      LockFreeOverlayDurableReplay \
      LockFreeOverlayValueCas \
      ConcurrentCheckpointSerialization
    do
      run_capped tlc -workers 1 -config "${module}.cfg" "${module}.tla"
    done
    run_capped tlc -workers 1 -config LockFreeIndexedOverlayCounter.cfg LockFreeIndexedOverlay.tla
    run_capped tlc -workers 1 -config LockFreeIndexedOverlayVocabulary.cfg LockFreeIndexedOverlay.tla

    # ── Negative controls (each `_Unsafe.cfg` MUST FAIL its model's safety) ──
    # Each `_Unsafe.cfg` deliberately relaxes the one design choice the model
    # exists to justify, and MUST FAIL a safety invariant:
    #   * LockFreeDurableCheckpoint / LockFreeDurableCheckpointEviction set
    #     USE_WATERMARK = FALSE and MUST violate `NoLostWriteUnderLockFreeCommit`
    #     (the GAP_LEDGER #41 appended-frontier losing trace) — proving the
    #     committed-watermark choice is REQUIRED (base retain-WAL reclaim AND with
    #     eviction-registry publication on).
    #   * OverlayEvictionCas sets USE_FAULT_IN = FALSE (lets the overlay evictor
    #     fire on a LIVE node with NO fault-in recovery) and MUST violate
    #     `ReadNeverMissesCommitted` — proving the read/write fault-in path is
    #     REQUIRED once eviction is unrestricted (an acked LIVE node evicted with
    #     no fault-in is permanently unreachable = silent data loss).
    #   * LockFreeOverlayRemoveCas sets USE_FRESH_COPY_CLEAR = FALSE (models the
    #     rejected in-place `fetch_and(!IS_FINAL)` clear that writes `present` and
    #     `removed` non-atomically with no root bump) and MUST violate
    #     `LastWriterWins` (resurrection / lost-remove) — proving the proven-DELETE
    #     fresh-copy-published-via-root-CAS choice (design §3.5) is REQUIRED for the
    #     composite {insert, remove} to stay last-writer-wins.
    #   * LockFreeOverlayDurableReplay sets USE_COMMIT_RANK = FALSE (recovery
    #     reconciles by LSN/physical order = the broken pre-fix scheme) and MUST
    #     violate `ReplayEqualsCommittedVisible` via the s019 interleaving (Append
    #     Insert@lsn1, Append Remove@lsn2>lsn1, then CommitAndRank(Remove) before
    #     CommitAndRank(Insert) ⇒ committed-visible PRESENT but lsn-order replay
    #     ends ABSENT = the acked-net-present-key loss) — proving the durable
    #     commit-generation reconcile (design C′, §3) is REQUIRED so replay order
    #     equals CAS/visibility order.
    # If TLC unexpectedly PASSES one of these, the model no longer exhibits the
    # bug it must catch → the negative control is broken → fail the whole gate.
    #   * ConcurrentCheckpointSerialization sets USE_LOCK = FALSE (no checkpoint_lock —
    #     the F3/NF-3 bug) and MUST violate `NoTornDescriptor`: two concurrent
    #     checkpoints interleave their block-0 descriptor writes, leaving fields from
    #     different generations (a torn descriptor → lost/corrupt terms on reopen) —
    #     proving the `checkpoint_lock` serialization (design §3.5 / R-NF3) is REQUIRED.
    #   * LockFreeOverlayValueCas sets USE_BURN_ON_LOSS = FALSE (the "forgot to burn"
    #     bug: a refused conditional write's already-durable Upsert record is RANKED
    #     instead of burned) and MUST violate `NoPhantomConditionalWrite`: a crash-
    #     recover resurrects a value the caller was told Ok(false) (the append-before-
    #     failed-CAS phantom behind compare_and_swap + the C2 merge CAS-retry loop) —
    #     proving the `mark_committed_burned` (UNRANKED, dropped on Overlay reopen)
    #     choice is REQUIRED.
    for unsafe_module in \
      LockFreeDurableCheckpoint \
      LockFreeDurableCheckpointEviction \
      OverlayEvictionCas \
      OverlayEvictionStale \
      LockFreeOverlayRemoveCas \
      LockFreeOverlayDurableReplay \
      LockFreeOverlayValueCas \
      ConcurrentCheckpointSerialization
    do
      echo "== Negative control: ${unsafe_module}_Unsafe.cfg (MUST violate a safety invariant) =="
      if run_capped tlc -workers 1 -config "${unsafe_module}_Unsafe.cfg" "${unsafe_module}.tla"; then
        echo "ERROR: ${unsafe_module}_Unsafe.cfg PASSED but MUST FAIL (negative control did not fire)" >&2
        exit 1
      else
        echo "OK: ${unsafe_module}_Unsafe.cfg failed as required (negative control fired)"
      fi
    done
  )
else
  echo "Skipping TLC model checking; set RUN_TLC=1 to enable bounded TLC runs"
fi
