#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

FORMAL_RESOURCE_CONTROL="${FORMAL_RESOURCE_CONTROL:-systemd}"
FORMAL_MEMORY_HIGH="${FORMAL_MEMORY_HIGH:-6G}"
FORMAL_MEMORY_MAX="${FORMAL_MEMORY_MAX:-8G}"
FORMAL_TASKS_MAX="${FORMAL_TASKS_MAX:-384}"
FORMAL_CPU_QUOTA="${FORMAL_CPU_QUOTA:-400%}"
FORMAL_COMMAND_TIMEOUT_SECONDS="${FORMAL_COMMAND_TIMEOUT_SECONDS:-7200}"

run_capped() {
  local -a command=("$@")

  if [ "$FORMAL_COMMAND_TIMEOUT_SECONDS" != "0" ]; then
    command=(
      timeout --foreground --signal=TERM --kill-after=30s
      "$FORMAL_COMMAND_TIMEOUT_SECONDS"
      "${command[@]}"
    )
  fi

  case "$FORMAL_RESOURCE_CONTROL" in
    systemd)
      if ! command -v systemd-run >/dev/null 2>&1; then
        echo "ERROR: FORMAL_RESOURCE_CONTROL=systemd requires systemd-run" >&2
        return 1
      fi
      systemd-run --user --scope --quiet --collect \
        --property="MemoryHigh=$FORMAL_MEMORY_HIGH" \
        --property="MemoryMax=$FORMAL_MEMORY_MAX" \
        --property=MemorySwapMax=0 \
        --property="TasksMax=$FORMAL_TASKS_MAX" \
        --property="CPUQuota=$FORMAL_CPU_QUOTA" \
        "${command[@]}"
      ;;
    external)
      "${command[@]}"
      ;;
    *)
      echo "ERROR: FORMAL_RESOURCE_CONTROL must be 'systemd' or 'external'" >&2
      return 1
      ;;
  esac
}

run_tlc_isolated() {
  local label="$1"
  shift
  local state_parent="$repo_root/target/tlc-state-spaces"
  local state_directory
  local status=0

  mkdir -p "$state_parent"
  state_directory="$(mktemp -d "$state_parent/${label}.XXXXXX")"
  run_capped tlc -metadir "$state_directory" "$@" || status=$?
  if [ "$status" -eq 0 ]; then
    rm -rf -- "$state_directory"
  else
    echo "TLC state directory retained after nonzero exit: $state_directory" >&2
  fi
  return "$status"
}

tlc_invariant_violation_reported() {
  local assertion_name="$1"
  local log_file="$2"

  grep -Fxq \
    -e "Error: Invariant ${assertion_name} is violated." \
    -e "Error: Invariant ${assertion_name} is violated by the initial state:" \
    "$log_file"
}

verify_tlc_invariant_diagnostic_classifier() {
  local assertion_name="ExpectedInvariant"

  if ! tlc_invariant_violation_reported "$assertion_name" \
    <(printf '%s\n' "Error: Invariant ${assertion_name} is violated."); then
    echo "ERROR: TLC diagnostic classifier rejected a transition-state violation" >&2
    return 1
  fi
  if ! tlc_invariant_violation_reported "$assertion_name" \
    <(printf '%s\n' "Error: Invariant ${assertion_name} is violated by the initial state:"); then
    echo "ERROR: TLC diagnostic classifier rejected an initial-state violation" >&2
    return 1
  fi
  if tlc_invariant_violation_reported "$assertion_name" \
    <(printf '%s\n' 'Error: Invariant WrongInvariant is violated.'); then
    echo "ERROR: TLC diagnostic classifier accepted the wrong invariant name" >&2
    return 1
  fi
  if tlc_invariant_violation_reported "$assertion_name" \
    <(printf '%s\n' "prefix Error: Invariant ${assertion_name} is violated. suffix"); then
    echo "ERROR: TLC diagnostic classifier accepted a non-exact diagnostic line" >&2
    return 1
  fi
}

run_tlc_negative_control() {
  local module="$1"
  local assertion_kind="$2"
  local assertion_name="$3"
  local config_base="${4:-${module}_Unsafe}"
  local log_parent="$repo_root/target/tlc-negative-control-logs"
  local state_parent="$repo_root/target/tlc-state-spaces"
  local state_directory
  local log_file
  local expected_status
  local status=0

  case "$assertion_kind" in
    invariant)
      expected_status=12
      ;;
    temporal)
      expected_status=13
      ;;
    *)
      echo "ERROR: unknown TLC assertion kind: $assertion_kind" >&2
      return 1
      ;;
  esac

  if ! grep -Fq "$assertion_name" "${config_base}.cfg"; then
    echo "ERROR: ${config_base}.cfg does not declare required ${assertion_kind} ${assertion_name}" >&2
    return 1
  fi

  mkdir -p "$log_parent" "$state_parent"
  log_file="$(mktemp "$log_parent/${module}.XXXXXX.log")"
  state_directory="$(mktemp -d "$state_parent/${config_base}.XXXXXX")"
  if run_capped tlc \
    -metadir "$state_directory" \
    -workers 1 \
    -config "${config_base}.cfg" \
    "${module}.tla" 2>&1 | tee "$log_file"; then
    status=0
  else
    status="${PIPESTATUS[0]}"
  fi

  if [ "$status" -eq 0 ]; then
    echo "ERROR: ${config_base}.cfg PASSED but MUST violate ${assertion_name}" >&2
    echo "TLC output retained at $log_file" >&2
    echo "TLC state directory retained at $state_directory" >&2
    return 1
  fi
  if [ "$status" -ne "$expected_status" ]; then
    echo "ERROR: ${config_base}.cfg exited with TLC status $status instead of required ${assertion_kind}-violation status $expected_status" >&2
    echo "TLC output retained at $log_file" >&2
    echo "TLC state directory retained at $state_directory" >&2
    return 1
  fi

  case "$assertion_kind" in
    invariant)
      if ! tlc_invariant_violation_reported "$assertion_name" "$log_file"; then
        echo "ERROR: ${config_base}.cfg did not violate required invariant ${assertion_name}" >&2
        echo "TLC output retained at $log_file" >&2
        echo "TLC state directory retained at $state_directory" >&2
        return 1
      fi
      ;;
    temporal)
      if ! grep -Fq "Error: Temporal properties were violated." "$log_file"; then
        echo "ERROR: ${config_base}.cfg did not violate required temporal property ${assertion_name}" >&2
        echo "TLC output retained at $log_file" >&2
        echo "TLC state directory retained at $state_directory" >&2
        return 1
      fi
      ;;
  esac

  rm -rf -- "$state_directory"
  rm -f -- "$log_file"
  echo "OK: ${config_base}.cfg violated required ${assertion_kind} ${assertion_name}"
}

verify_tlc_invariant_diagnostic_classifier

assert_nonzero_cargo_filter() {
  local output
  output="$(run_capped cargo test "$@" -- --list)"
  printf '%s\n' "$output"
  if ! printf '%s\n' "$output" | grep -qE '^[^[:space:]].*: test$'; then
    echo "ERROR: filtered cargo test target discovered zero tests: cargo test $*" >&2
    exit 1
  fi
}

run_filtered_cargo_test() {
  assert_nonzero_cargo_filter "$@"
  run_capped cargo test "$@"
}

echo "== Unsafe boundary inventory =="
run_capped bash scripts/verify-unsafe-boundary-inventory.sh

echo "== Snapshot cursor strict-provenance source gate =="
if cursor_provenance_drift="$(rg -n \
  'Arc::as_ptr.*as usize|from_provenance|\.provenance::<|DENSE_TAG|without_provenance|expose_provenance|with_exposed_provenance' \
  src/lib.rs src/dynamic_dawg/lockfree.rs src/dynamic_dawg/mod.rs)"; then
  printf '%s\n' "$cursor_provenance_drift" >&2
  echo "Snapshot cursor code must not tag pointers, expose addresses, or recover provenance from integers" >&2
  exit 1
fi

echo "== ABI invariant registry =="
run_capped python3 scripts/check-abi-invariants.py

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
# The old `lling-llang` feature was retired when the lattice dependency was
# extracted into `llattice`; keep the harness compatible with both layouts.
if grep -q '^lling-llang[[:space:]]*=' Cargo.toml; then
  run_capped cargo test --features lling-llang --test valued_set_combinator_correspondence
else
  echo "Skipping lling-llang feature correspondence; feature not declared"
fi
run_capped cargo test --features pathmap-backend --test pathmap_factory_correspondence
run_capped cargo test --features pathmap-backend --test pathmap_snapshot_tests
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
run_capped cargo test --features persistent-artrie --test persistent_suffix_automaton_correspondence
run_capped cargo test --features persistent-artrie --test persistent_suffix_automaton_proptest
run_capped cargo test --features persistent-artrie --test persistent_suffix_automaton_concurrency
run_capped cargo test --features persistent-artrie --test persistent_suffix_tree_correspondence
run_capped cargo test --features persistent-artrie --test persistent_scdawg_correspondence
run_capped cargo test --features persistent-artrie --test persistent_artrie_u64_correspondence
run_capped cargo test --features persistent-artrie --test persistent_char_node_layout_correspondence
run_capped cargo test --features persistent-artrie --test char_node_format_compatibility
run_capped cargo test --features persistent-artrie --test char_v3_crash_reopen_correspondence
run_capped bash scripts/verify-char-node-format-compatibility.sh
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
run_capped cargo test --features persistent-artrie --test persistent_char_eviction_correspondence
run_capped cargo test --features persistent-artrie --test persistent_char_eviction_registry_correspondence
run_filtered_cargo_test \
  --features persistent-artrie \
  --lib \
  persistent_artrie::char::eviction_registry_tests
run_capped cargo test --features persistent-artrie --test persistent_shared_concurrency_correspondence
run_capped cargo test --features persistent-artrie --test persistent_public_durability_policy_correspondence
run_capped cargo test --features persistent-artrie --test persistent_public_lifecycle_correspondence
run_capped cargo test --features persistent-artrie --test persistent_end_to_end_trace_correspondence
run_capped cargo test --features persistent-artrie --test epoch_checkpoint_recovery_correspondence
run_capped env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" \
  cargo test --features persistent-artrie --test persistent_merge_correspondence
run_capped env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" \
  cargo test \
  --features "persistent-artrie parallel-merge" \
  --test persistent_merge_correspondence
run_capped cargo test --features persistent-artrie --test persistent_artrie_storage_correspondence
run_capped cargo test --features persistent-artrie --test persistent_artrie_loom_correspondence
run_capped cargo test --features persistent-artrie --test persistent_eviction_publication_gate_loom
run_capped cargo test --features persistent-artrie --test persistent_lockfree_overlay_loom
run_capped cargo test --features persistent-artrie --test persistent_lockfree_durable_loom
run_filtered_cargo_test \
  --features persistent-artrie \
  --lib \
  persistent_artrie::vocab
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

  run_miri_filtered() {
    assert_nonzero_cargo_filter "$@"
    run_capped "${miri_cargo[@]}" miri test "$@"
  }

  run_miri_filtered \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    vocab_checkpoint_reopen_preserves_unicode_bijection
  run_miri_filtered \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    vocab_duplicate_insert_keeps_stable_index_after_reopen
  run_miri_filtered \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    char_child_remove_transfers_box_ownership_once
  run_miri_filtered \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    char_insert_child_replaces_without_aliasing_old_box
  run_miri_filtered \
    --features persistent-artrie \
    --lib \
    nonresident_child_insertion_rejects_resident_pointer_without_publication
  run_miri_filtered \
    --features persistent-artrie \
    --lib \
    nonresident_child_type_rejects_transitional_and_malformed_disk_states
  run_miri_filtered \
    --features persistent-artrie \
    --lib \
    nonresident_to_resident_replacement_preserves_every_adaptive_variant
  run_miri_filtered \
    --features persistent-artrie \
    --lib \
    pending_owned_child_reclaims_or_returns_exactly_once
  run_miri_filtered \
    --features persistent-artrie \
    --lib \
    resident_publication_preserves_provenance_across_growth_boundaries
  run_miri_filtered \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    char_clone_deep_copies_child_boxes
  run_miri_filtered \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    char_get_or_create_child_mutation_keeps_unique_raw_borrow
  run_miri_filtered \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    swizzled_pointer_raw_extraction_is_gated_by_in_memory_state
  run_miri_filtered \
    --features persistent-artrie \
    --test persistent_artrie_formal_correspondence \
    swizzled_pointer_losing_lazy_load_candidate_can_be_reclaimed_once
  run_miri_filtered \
    --features persistent-artrie \
    --lib \
    persistent_artrie::core::swizzled_ptr::tests
  run_miri_filtered \
    --features persistent-artrie \
    --lib \
    persistent_artrie::core::buffer_manager::tests::fixed_buffer_registration_covers_write_guard_mutation_and_flush
  run_miri_filtered \
    --lib \
    concurrent_slots::tests::raw_box_ownership_paths_reclaim_exactly_once
  run_miri_filtered \
    --lib \
    snapshot_traversal_cursor_tests::dense_cursor_is_one_word_and_round_trips_both_index_forms
  run_miri_filtered \
    --lib \
    dynamic_dawg::lockfree::tests::provenance_cursor_is_one_word_opaque_and_revision_retained
  run_miri_filtered \
    --lib \
    dynamic_dawg::lockfree::tests::native_edge_ranges_are_exact_across_inline_and_spilled_storage
  run_miri_filtered \
    --lib \
    dynamic_dawg::lockfree::tests::retained_old_edge_range_survives_new_root_publication
  run_miri_filtered \
    --features protobuf \
    --lib \
    serialization::protobuf_impl::binary_dat_payload_tests::v2_sink_emits_exact_events_across_spare_capacity_boundaries
  run_miri_filtered \
    --features protobuf \
    --lib \
    serialization::protobuf_impl::binary_dat_payload_tests::v2_sink_second_growth_failure_preserves_both_logical_lengths
  run_miri_filtered \
    --features protobuf \
    --lib \
    serialization::protobuf_impl::binary_dat_payload_tests::v2_sink_consults_allocator_only_at_true_exhaustion
  run_miri_filtered \
    --features protobuf \
    --lib \
    serialization::protobuf_impl::binary_dat_payload_tests::v2_sink_records_exactly_one_atomic_commit_per_successful_event
  run_miri_filtered \
    --features ffi \
    --lib \
    double_array_trie::core::shared::tests::trusted_edge_projection_matches_checked_projection
  run_miri_filtered \
    --features ffi \
    --lib \
    bindings::tests::byte_dat_snapshot_uses_validated_native_cursor_tokens_end_to_end
  run_miri_filtered \
    --features ffi \
    --lib \
    bindings::tests::dynamic_snapshot_graph_is_stable_and_live_resources_do_not_advertise_it
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
      IoUringFixedBufferOwnership \
      IoUringSqeCqeLifecycle \
      LockFreeARTrieLinearizability \
      LockFreeIndexedOverlay \
      LockFreeCounterMergeAtomicity \
      ConcurrentCheckpointPublication \
      LockFreeDurableCheckpoint \
      LockFreeDurableCheckpointEviction \
      CapturedCheckpointEvictionRoute \
      DetachedCallbackSeparation \
      DetachedCompatibilityInstall \
      CachelessOwnedRegistry \
      EvictionExactRootPublication \
      HelpedRootResidency \
      HelpedResidencyScan \
      HelpedCheckpointStamps \
      PackedResidencyRollover \
      ResidencyRevisionOrdinalABA \
      RootOwnerFence \
      SparseResidencyWinnerAuthority \
      ResidentRankingDepth \
      PackedResidencyFreshCatalog \
      OverlayTreeWitness \
      ResidentBudgetEviction \
      SharedPersistentConcurrency \
      PublicDurabilityPolicy \
      PersistentEndToEndTrace \
      PublicReadSnapshotTraversal \
      PersistentSuffixAutomaton \
      PersistentSuffixTree \
      PersistentScdawg \
      PersistentARTrieU64 \
      PersistentARTrieU64Iteration \
      PersistentARTrieU64WorkMachines \
      CharNodeV2Layout \
      CharV3ArenaPublication \
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
      ConcurrentCheckpointSerialization \
      RetainedEdgeRangeTraversal \
      AbiProducerSnapshot \
      AbiSnapshotInitializerTakeover \
      DictionaryEntryBatchLease \
      AbiSnapshotQuiescence
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
      IoUringFixedBufferOwnership \
      IoUringSqeCqeLifecycle \
      LockFreeARTrieLinearizability \
      LockFreeCounterMergeAtomicity \
      ConcurrentCheckpointPublication \
      LockFreeDurableCheckpoint \
      LockFreeDurableCheckpointEviction \
      CapturedCheckpointEvictionRoute \
      DetachedCallbackSeparation \
      DetachedCompatibilityInstall \
      CachelessOwnedRegistry \
      EvictionExactRootPublication \
      HelpedRootResidency \
      HelpedResidencyScan \
      HelpedCheckpointStamps \
      PackedResidencyRollover \
      ResidencyRevisionOrdinalABA \
      RootOwnerFence \
      SparseResidencyWinnerAuthority \
      ResidentRankingDepth \
      PackedResidencyFreshCatalog \
      OverlayTreeWitness \
      ResidentBudgetEviction \
      SharedPersistentConcurrency \
      PublicDurabilityPolicy \
      PersistentEndToEndTrace \
      PublicReadSnapshotTraversal \
      PersistentSuffixAutomaton \
      PersistentSuffixTree \
      PersistentScdawg \
      PersistentARTrieU64 \
      PersistentARTrieU64Iteration \
      PersistentARTrieU64WorkMachines \
      CharNodeV2Layout \
      CharV3ArenaPublication \
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
      ConcurrentCheckpointSerialization \
      RetainedEdgeRangeTraversal \
      AbiProducerSnapshot \
      AbiSnapshotInitializerTakeover \
      DictionaryEntryBatchLease \
      AbiSnapshotQuiescence
    do
      tlc_workers=1
      run_tlc_isolated "$module" \
        -workers "$tlc_workers" \
        -config "${module}.cfg" \
        "${module}.tla"
    done
    run_tlc_isolated PersistentARTrieU64WorkMachines_Cycle \
      -workers 1 \
      -config PersistentARTrieU64WorkMachines_Cycle.cfg \
      PersistentARTrieU64WorkMachines.tla
    run_tlc_isolated PersistentARTrieU64Iteration_Chain \
      -workers 1 \
      -config PersistentARTrieU64Iteration_Chain.cfg \
      PersistentARTrieU64Iteration.tla
    run_tlc_isolated PersistentARTrieU64Iteration_Prefix \
      -workers 1 \
      -config PersistentARTrieU64Iteration_Prefix.cfg \
      PersistentARTrieU64Iteration.tla
    run_tlc_isolated PersistentARTrieU64Iteration_OnDisk \
      -workers 1 \
      -config PersistentARTrieU64Iteration_OnDisk.cfg \
      PersistentARTrieU64Iteration.tla
    run_tlc_isolated LockFreeIndexedOverlayCounter \
      -workers 1 \
      -config LockFreeIndexedOverlayCounter.cfg \
      LockFreeIndexedOverlay.tla
    run_tlc_isolated LockFreeIndexedOverlayVocabulary \
      -workers 1 \
      -config LockFreeIndexedOverlayVocabulary.cfg \
      LockFreeIndexedOverlay.tla
    run_tlc_isolated HelpedResidencyScan_Liveness \
      -workers 1 \
      -config HelpedResidencyScan_Liveness.cfg \
      HelpedResidencyScan.tla

    # ── Negative controls (each `_Unsafe.cfg` MUST FAIL its model's safety) ──
    # Each `_Unsafe.cfg` deliberately relaxes the one design choice the model
    # exists to justify, and MUST FAIL a safety invariant:
    #   * LockFreeDurableCheckpoint / LockFreeDurableCheckpointEviction set
    #     USE_WATERMARK = FALSE and MUST violate `NoLostWriteUnderLockFreeCommit`.
    #     The composite model additionally proves that semantic publication clears
    #     exact authority, exact publication checks the captured root and catalog
    #     stamp, exact operations revalidate the current pair, and recovery ignores
    #     detached advisory state. Each obligation has its own unsafe control.
    #   * CapturedCheckpointEvictionRoute re-probes the live coordinator slot at
    #     publish time and MUST violate `PublicationUsesCapturedRoute`, proving
    #     capture-off cannot reroute on and generation A cannot be replaced by B.
    #   * DetachedCompatibilityInstall mutates either total-wrapper behavior or
    #     rejection atomicity. The controls MUST respectively violate
    #     `LegacyWrapperNeverPanics` and `RejectedInstallPreservesCatalog`.
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
    #   * AbiProducerSnapshot sets USE_IMMUTABLE_CAPTURE = FALSE (the rejected
    #     design where snapshot() aliases the live head instead of pinning the
    #     captured revision) and MUST violate `CapturedRevisionImmutable`: any
    #     post-capture publish rewrites what an ABI consumer already captured —
    #     proving the pinned-immutable-revision capture (vt.dictionary.v1, family
    #     FV obligation #10) is REQUIRED.
    #   * AbiSnapshotInitializerTakeover adds SingleConstruction to the safe
    #     invariants and MUST violate it: a boundedly stalled cold initializer
    #     may be superseded by a fresh generation, while both immutable
    #     same-revision snapshots remain valid. This prevents tests from
    #     mistaking usual-path pointer identity for a safety requirement.
    #   * AbiSnapshotQuiescence sets USE_ADMISSION_GATE = FALSE (the rejected
    #     fallback that only waits for a transient zero-writer observation) and
    #     MUST violate `SnapshotEventuallyCompletes`: writers can continuously
    #     re-enter between a departure and capture. This proves that atomically
    #     closing admission before draining existing writers is REQUIRED for
    #     starvation-free capture under weak scheduler fairness.
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
    #   * PersistentARTrieU64WorkMachines selects the LateCycle graph and sets
    #     RejectBackEdge = FALSE, modeling a
    #     disk materializer that accepts a gray/Visiting back-edge. It MUST
    #     violate `NoCyclicSnapshotAccepted`, proving rejection occurs before an
    #     Arc edge can be spliced into the reconstructed overlay.
    #   * PersistentARTrieU64Iteration enables global node-identity suppression
    #     on the Diamond DAG. It MUST violate `CompletionIsExact`, proving trie
    #     language enumeration remains path-sensitive across shared nodes.
    while IFS='|' read -r unsafe_module assertion_kind assertion_name unsafe_config; do
      negative_config="${unsafe_config:-${unsafe_module}_Unsafe}"
      echo "== Negative control: ${negative_config}.cfg (MUST violate ${assertion_name}) =="
      run_tlc_negative_control "$unsafe_module" "$assertion_kind" "$assertion_name" "$negative_config"
    done <<'NEGATIVE_CONTROLS'
LockFreeDurableCheckpoint|invariant|NoLostWriteUnderLockFreeCommit
LockFreeDurableCheckpointEviction|invariant|NoLostWriteUnderLockFreeCommit
LockFreeDurableCheckpointEviction|invariant|ExactRootRegistryAgreement|LockFreeDurableCheckpointEviction_SemanticBindingUnsafe
LockFreeDurableCheckpointEviction|invariant|ExactRootRegistryAgreement|LockFreeDurableCheckpointEviction_StaleRootUnsafe
LockFreeDurableCheckpointEviction|invariant|PublishedCatalogIsStamped|LockFreeDurableCheckpointEviction_PreStampUnsafe
LockFreeDurableCheckpointEviction|invariant|NoInexactUse|LockFreeDurableCheckpointEviction_InexactUseUnsafe
LockFreeDurableCheckpointEviction|invariant|RecoveryIndependentOfDetached|LockFreeDurableCheckpointEviction_RecoveryDetachedUnsafe
CapturedCheckpointEvictionRoute|invariant|PublicationUsesCapturedRoute|CapturedCheckpointEvictionRoute_LiveReprobeUnsafe
DetachedCallbackSeparation|invariant|DetachedCallbackHasOnlyDetachedCapability|DetachedCallbackSeparation_LegacyReadsExactUnsafe
DetachedCallbackSeparation|invariant|DetachedNeverAuthorizesExactCommit|DetachedCallbackSeparation_DetachedAuthorizesUnsafe
DetachedCallbackSeparation|invariant|DetachedCatalogContainsOnlyDetached|DetachedCallbackSeparation_CheckpointPopulatesDetachedUnsafe
DetachedCallbackSeparation|invariant|SemanticClearsExactAuthority|DetachedCallbackSeparation_SemanticPreservesBindingUnsafe
DetachedCallbackSeparation|invariant|CatalogNeverAuthorizesExactCommit|DetachedCallbackSeparation_CatalogAuthorizesUnsafe
DetachedCompatibilityInstall|invariant|LegacyWrapperNeverPanics|DetachedCompatibilityInstall_PanicUnsafe
DetachedCompatibilityInstall|invariant|RejectedInstallPreservesCatalog|DetachedCompatibilityInstall_OverwriteUnsafe
CachelessOwnedRegistry|invariant|LastCollisionOccurrenceEquivalent|CachelessOwnedRegistry_FirstCollisionUnsafe
CachelessOwnedRegistry|invariant|FailedRemovePreservesProjection|CachelessOwnedRegistry_MutateBeforeMaterializeUnsafe
EvictionExactRootPublication|invariant|ExactRootRegistryAgreement|EvictionExactRootPublication_SemanticBindingUnsafe
EvictionExactRootPublication|invariant|NoInexactCommit|EvictionExactRootPublication_InexactCommitUnsafe
EvictionExactRootPublication|invariant|ExactRootRegistryAgreement|EvictionExactRootPublication_PreStampUnsafe
EvictionExactRootPublication|invariant|ExactRootRegistryAgreement|EvictionExactRootPublication_RetirementUnsafe
EvictionExactRootPublication|invariant|NoRetainedGenerationABA|EvictionExactRootPublication_GenerationReuseUnsafe
EvictionExactRootPublication|invariant|FailedPublicationPreservesRegistry|EvictionExactRootPublication_RollbackUnsafe
HelpedRootResidency|invariant|MaterializedResidencyMatchesPublishedRoot|HelpedRootResidency_EarlyFrontierUnsafe
HelpedRootResidency|invariant|RootIsSoleLogicalAuthority|HelpedRootResidency_RetirementFenceUnsafe
HelpedRootResidency|invariant|MaterializedResidencyMatchesPublishedRoot|HelpedRootResidency_UnfencedWordUnsafe
HelpedRootResidency|invariant|NoUnstampedPublication|HelpedRootResidency_UnstampedUnsafe
HelpedRootResidency|invariant|CatalogNeverAuthorizes|HelpedRootResidency_CatalogUnsafe
HelpedResidencyScan|invariant|NoAcceptedTornScan|HelpedResidencyScan_Unsafe
HelpedCheckpointStamps|invariant|NoEarlyActivation|HelpedCheckpointStamps_EarlyActivationUnsafe
HelpedCheckpointStamps|invariant|NoStampBeforePublication|HelpedCheckpointStamps_FailedCandidateUnsafe
PackedResidencyRollover|invariant|CurrentCellMatchesRoot|PackedResidencyRollover_OrdinalReuseUnsafe
PackedResidencyRollover|invariant|CurrentCellMatchesRoot|PackedResidencyRollover_WrongGenerationUnsafe
ResidencyRevisionOrdinalABA|invariant|NoDelayedHelperABA|ResidencyRevisionOrdinalABA_Unsafe
RootOwnerFence|invariant|RootNeverNamesRetiredOwner|RootOwnerFence_StalePublishUnsafe
SparseResidencyWinnerAuthority|invariant|SettledMaterializationMatchesRoot|SparseResidencyWinnerAuthority_LoserHelpUnsafe
ResidentRankingDepth|invariant|ConcreteChildStrictDepth|ResidentRankingDepth_Unsafe
CharV3ArenaPublication|invariant|NoRootToUncommittedV3Arena|CharV3ArenaPublication_UncommittedUnsafe
CharV3ArenaPublication|invariant|ChecksumPrecedesV3Root|CharV3ArenaPublication_ChecksumUnsafe
CharV3ArenaPublication|invariant|OldReaderRejectsV3|CharV3ArenaPublication_OldReaderUnsafe
CharV3ArenaPublication|invariant|CurrentReaderAcceptsSupportedRoots|CharV3ArenaPublication_CurrentReaderV2Unsafe
CharV3ArenaPublication|invariant|CommittedV3ReopensAfterCrash|CharV3ArenaPublication_CurrentReaderV3ReopenUnsafe
CharV3ArenaPublication|invariant|PublishedRootHasExactGeneration|CharV3ArenaPublication_StaleRootUnsafe
CharV3ArenaPublication|invariant|V2MigrationUsesTargetHeaders|CharV3ArenaPublication_LossyMigrationUnsafe
PackedResidencyFreshCatalog|invariant|FreshAddressDomainIsDistinct|PackedResidencyFreshCatalog_ReuseUnsafe
PackedResidencyFreshCatalog|invariant|PublishedCellsMatchLogicalRoot|PackedResidencyFreshCatalog_WrongHelperUnsafe
PackedResidencyFreshCatalog|invariant|PublishedCellsMatchLogicalRoot|PackedResidencyFreshCatalog_PartialUnsafe
PackedResidencyFreshCatalog|invariant|WinnerOwnsPublishedRoot|PackedResidencyFreshCatalog_NonExactUnsafe
OverlayTreeWitness|invariant|WitnessImpliesTree|OverlayTreeWitness_ForgeDagUnsafe
OverlayTreeWitness|invariant|WitnessNamesCurrentRevision|OverlayTreeWitness_StaleWitnessUnsafe
OverlayTreeWitness|invariant|FastSerializationRequiresCurrentTreeWitness|OverlayTreeWitness_FastWithoutWitnessUnsafe
OverlayTreeWitness|invariant|CycleIsNeverPublished|OverlayTreeWitness_AdmitCycleUnsafe
ResidentBudgetEviction|invariant|PlannedReclamationIsReal|ResidentBudgetEviction_LocalDescendantUnsafe
ResidentBudgetEviction|invariant|AcceptedSnapshotsWereRevalidated|ResidentBudgetEviction_SnapshotUnsafe
OverlayEvictionCas|invariant|ReadNeverMissesCommitted
OverlayEvictionStale|invariant|NoStaleEvict
OverlayEvictionStale|invariant|FaultInstalledCarriesExactStamp|OverlayEvictionStale_FaultStampUnsafe
LockFreeOverlayRemoveCas|invariant|LastWriterWins
LockFreeOverlayDurableReplay|invariant|ReplayEqualsCommittedVisible
LockFreeOverlayValueCas|invariant|NoPhantomConditionalWrite
ConcurrentCheckpointSerialization|invariant|NoTornDescriptor
RetainedEdgeRangeTraversal|invariant|RetainedReaderRevisionIsAllocated|RetainedEdgeRangeTraversal_UnsafeReclaim
RetainedEdgeRangeTraversal|invariant|NoPartialExternalPublication|RetainedEdgeRangeTraversal_UnsafePartialPublish
RetainedEdgeRangeTraversal|invariant|RangeBounds|RetainedEdgeRangeTraversal_UnsafeAdvance
AbiProducerSnapshot|invariant|CapturedRevisionImmutable
AbiSnapshotInitializerTakeover|invariant|SingleConstruction|AbiSnapshotInitializerTakeover_SingleConstructionUnsafe
AbiSnapshotQuiescence|temporal|SnapshotEventuallyCompletes
PersistentARTrieU64WorkMachines|invariant|NoCyclicSnapshotAccepted
PersistentARTrieU64Iteration|invariant|CompletionIsExact
NEGATIVE_CONTROLS
  )
else
  echo "Skipping TLC model checking; set RUN_TLC=1 to enable bounded TLC runs"
fi
