# F2 test-migration classification (subagent)

Feature-on baseline: 91 failed (90 unique). Feature-off baseline: **2 failed** (NOT 100% green — pre-existing C2 unconditional reject-removal):
- `s5_567_overlay_producer_guards_reject` (Bucket D, unconditional)
- `m3_reject_guards_fire_under_overlay` (Bucket D, unconditional)

Contract facts (verified in src):
- `get()`/`try_get()` return None under `route_overlay()`; read owned tree when NOT routed.
- `get_value()` routes to overlay when flipped (NEVER falls through to owned for arbitrary V), else reads owned tree. Works in BOTH owned & overlay mode → safe migration target.
- Eligibility: feature-OFF `{(),u64}` (char) / `{(),i64}` (byte) eligible → flip. Arbitrary V (i32, String, u32 for char, u64 for byte) stays OWNED feature-off. Feature-ON: ALL V eligible → flip.
- `merge_from`/`merge_replace`/`merge_from_batched(_grouped)` route to overlay (succeed) UNCONDITIONALLY when routed.
- `begin_document` succeeds when routed (UNCONDITIONAL). `tx_increment` on overlay requires u64 counter.
- `merge_lockfree_to_persistent`/`merge_lockfree_values_to_persistent`/`compact` STILL reject under overlay.

## BUCKET A — get()→get_value() (value reads return None under overlay)
Value type ineligible feature-off (i32/i64/MaybeFailValue) → stays owned feature-off → get_value falls to owned. Safe both ways.

tests/checkpoint_retention_correspondence.rs :: char_corruption_rebuild_replays_archived_checkpoint_and_active_tail (L96-97 char helper get→get_value)
tests/dirty_checkpoint_correspondence.rs :: char_descriptor_publication_before_wal_truncation_reopens_with_wal_tail (L458-459)
tests/dictionary_node_reopen_traversal_correspondence.rs :: (NONE here — these are Bucket B walk_map)
tests/persistent_lazy_mutation_correspondence.rs :: char_lazy_duplicate_insert_is_noop_without_wal_append, char_successful_lazy_mutations_replay_after_reopen (assert_char_value helper L64)
tests/persistent_prefix_correspondence.rs :: disk_backed_prefix_semantics_survive_sync_reopen_and_deletion (assert_full_map helper L126)
tests/recovery_planner_correspondence.rs :: char_corruption_rebuild_uses_only_durable_wal_prefix (L202); byte_corruption_rebuild_uses_only_durable_wal_prefix (L176 ALREADY get_value — VERIFY: may be Bucket-B-like reopen-into-owned)
tests/root_descriptor_reopen_correspondence.rs :: char_descriptor_reopen_roundtrip_preserves_reference_map_across_depths (assert_char_value helper L94)
tests/persistent_artrie_recovery_tests.rs :: test_char_trie_merge_from_* (8 tests, get→get_value Some(&v)→Some(v)); test_parallel_merge_with_overlaps (L2716/2726)
tests/persistent_end_to_end_trace_correspondence.rs :: char_trace_survives_checkpoint_and_wal_tail_reopen (L141,148)
tests/persistent_public_lifecycle_correspondence.rs :: char_public_open_replays_unicode_checkpoint_plus_synced_tail (L74-76)
tests/persistent_shared_concurrency_correspondence.rs :: char_shared_checkpoint_racing_insert_reopens (L110)
tests/persistent_nonblocking_checkpoint_correspondence.rs :: nonblocking_checkpoint_preserves_data_under_concurrent_reads_writes (L144)
tests/persistent_wal_atomicity_correspondence.rs :: char_atomic_serialization_failures_preserve_memory_and_wal (L236 get→get_value .map(value)); char_atomic_writes_replay_after_reopen (L276-277)
tests/persistent_bulk_mutation_correspondence.rs :: char_remove_prefix_batched_survives_reopen_without_checkpoint_for_batch_sizes (assert_char_map helper L72); byte_and_char_checked_increment_overflow_preserves_wal_and_memory (L393-394)
tests/recovery_replay_completeness_correspondence.rs :: char_archive_recovery_replays_every_mutating_variant_without_relogging (L278,281), char_archive_recovery_stops_at_first_corrupt_record (L323)
tests/persistent_transaction_increment_correspondence.rs :: char_archive_recovery_stops_before_overflowed_batch_increment_suffix (L204)
tests/persistent_rewrite_compaction_correspondence.rs :: char_checkpoint_rewrite_keeps_post_checkpoint_wal_tail_replayable, char_rewrite_checkpoint_preserves_unicode_values_lazy_and_eager_reopen, char_persist_to_disk_alone_does_not_clear_checkpoint_dirty_state?, char_failed_wal_archive_after_rewrite_keeps_dirty_until_retry? (VERIFY: some may be Bucket B dirty-state)
src persistent_artrie_char/dict_impl_char.rs :: test_shared_char_trie_upsert (L3000 get→get_value)

## BUCKET B — owned-rep white-box (owned-pin via kill_switch_to_owned)
tests/dictionary_node_reopen_traversal_correspondence.rs :: resident_walk_equals_reopened_walk, walk_after_reopen_matches_inserted_utf8_values, walk_after_reopen_equals_snapshot (walk_map over self.root)
tests/persistent_char_eviction_correspondence.rs :: force_eviction_char_reclaims_and_key_reloads, post_checkpoint_write_invalidates_registry, reopen_identical_with_and_without_eviction, value_at_reloads_after_eviction, value_survives_eviction_via_get, empty_trie_checkpoint_registers_nothing (eviction registry/force_eviction)
tests/persistent_char_eviction_registry_correspondence.rs :: evicted_entries_reference_durable_data, recovery_independent_of_registry, registry_empty_until_verified_checkpoint, write_invalidates_published_registry
tests/persistent_char_eviction_proptest.rs :: prop_reclaim_is_lossless, prop_post_checkpoint_write_invalidates, prop_eviction_preserves_recovery
tests/persistent_char_ebr_correspondence.rs :: walk_concurrent_with_eviction_is_safe_and_complete (walk under eviction)
src persistent_artrie_char/eviction_registry_tests.rs :: force_eviction_unswizzles_a_live_slot_to_disk, async_request_eviction_reclaims_registered_nodes
src persistent_artrie_char/mod.rs (tests) :: dictionary_node_traversal_descends_after_reopen, dictionary_node_traversal_descends_after_forced_eviction
tests/epoch_checkpoint_recovery_correspondence.rs :: public_mutations_record_epoch_wal_bytes_without_manual_calls (op accounting), corrupt_epoch_metadata_fails_closed_while_trie_checkpoint_recovers (VERIFY: read part may be A)
InvalidMagic corruption-injection (owned-tree disk offsets) → owned-pin:
tests/persistent_lazy_mutation_correspondence.rs :: char_lazy_insert_error_returns_err_before_wal_append, char_lazy_value_insert_and_remove_errors_do_not_append_wal
tests/persistent_read_snapshot_correspondence.rs :: char_lazy_traversal_failure_is_error_and_does_not_append_wal
tests/root_descriptor_reopen_correspondence.rs :: char_lazy_load_errors_are_result_errors_and_public_reads_fail_closed
tests/persistent_bulk_mutation_correspondence.rs :: char_remove_prefix_lazy_collection_error_preserves_wal_and_unaffected_terms (InvalidMagic)
VERIFY (WAL record shape): char_remove_prefix_batched_replays_every_durable_wal_prefix (CommitRank vs Remove)
VERIFY (rebuild-into-owned reads): recovery_planner byte/char, recovery_replay, end_to_end, public_lifecycle, wal_atomicity replay, rewrite_compaction, checkpoint_retention, transaction_increment archive, shared_concurrency, nonblocking — these REOPEN; if reopen re-flips overlay get_value works (A), if reopen leaves data in owned get_value fails (need owned-pin source). DETERMINE EMPIRICALLY.

## BUCKET C — compaction (owned-pin)
tests/compaction_tests.rs :: ALL 11 failing test_compact_* (InvalidOperation compact-under-overlay)
tests/persistent_compaction_correspondence.rs :: compaction_rejects_wal_sidecar_collision_without_losing_recovery_wal, in_place_compaction_preserves_unsynced_wal_values_after_reopen, non_utf8_byte_keys_survive_compaction, output_file_compaction_preserves_key_value_snapshot, successful_in_place_compaction_does_not_replay_stale_original_wal
tests/persistent_artrie_acid_tests.rs :: test_durability_policy_none_for_testing (upsert_cas_durable requires Immediate/GroupCommit — VERIFY bucket)

## BUCKET D — eligibility/reject contract changed
UNCONDITIONAL (fix in place, no cfg — contract changed for all configs; these fail feature-off too):
src persistent_artrie/overlay_routing_tests.rs :: m3_reject_guards_fire_under_overlay (merge*/begin_document now succeed; merge_lockfree*/compact still reject)
src persistent_artrie_char/persist.rs :: s5_567_overlay_producer_guards_reject (begin_document/merge now succeed under u64 overlay)
CFG-SPLIT (String/u64-for-byte ineligible feature-off, eligible feature-on):
src persistent_artrie/overlay_write_mode.rs :: byte_eligible_v_gate (String+u64 elig), byte_flip_is_noop_for_ineligible_v (String flips), byte_create_flip_eligible_v_routes_ineligible_v_owned (String routes)
src persistent_artrie_char/overlay_write_mode.rs :: v1_typeid_gate_flip_is_noop_for_arbitrary_v (String elig+flips)
src persistent_artrie/lockfree_cas.rs :: m4b_old_owned_file_stays_owned_on_reopen (String flips feature-on)
src persistent_artrie_char/mmap_ctor.rs :: s5_12_create_flip_eligible_v_overlay_arbitrary_v_owned, s5_12_old_owned_file_stays_owned_on_reopen (String flips feature-on)

## FINAL RESULTS (verified)
- FEATURE-ON: 90→0 deterministic failures. 2628 tests, 2628 passed (run 2). One intermittent flake (`char_union_with_no_ab_ba_deadlock`, NOT modified, NOT in scope) — passes 3/3 isolated + run-2 full; fails ~1/3 full runs on `assert_eq!(n2, 2)` getting 3 (a genuine cross-instance union race in the test's exact-count assertion).
- FEATURE-OFF: 2617 passed, 0 failed (×2 runs). Both pre-existing failures (m3, s5_567) fixed. No regression.
- Bucket A (get→get_value): ~28 tests across 16 files (several via shared assert_char_value/assert_char_map/assert_full_map helpers).
- Bucket B (owned-pin): ~24 tests (eviction registry/proptest/ebr, DictionaryNode walk, InvalidMagic corruption-injection, epoch op-accounting, dirty-state, recovery-source-owned-pin).
- Bucket C (compaction owned-pin): 16 compaction tests + acid None-policy + char tx-increment overflow.
- Bucket D (eligibility/reject): 2 UNCONDITIONAL (m3, s5_567) + 7 cfg-split (byte/char eligibility + ctor-gates) + e1_inert cfg-split.
- Recovery-family KEY FINDING: corruption-rebuild/archive-recovery for arbitrary-V (i32 both variants; i64 char) initially looked product-limited (empty trie after rebuild) but is FIXED by owned-pinning the SOURCE (so the WAL/archive holds Owned-format records the rebuild replays) + get_value on the flipped rebuild trie. Raw-archive tests (recover_from_archives with manually-written Owned-format records) needed only get_value.
- FLAGGED product limitations (pre-existing, NOT migrated-away — real overlay gaps): see report.
