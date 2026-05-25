(** Persistent dirty-checkpoint publication model.

    This specification captures the publication protocol that connects
    incremental dirty-slot flushing, root-descriptor trust, checkpoint records,
    and WAL truncation:

    - dirty arena/slot evidence covers every modified persisted payload;
    - dirty evidence is cleared only after successful write and sync;
    - failed write/sync outcomes preserve dirty evidence for retry;
    - enabling slot tracking after existing dirty arenas preserves coverage;
    - a checkpoint is trusted only after dirty data, root descriptor, and
      checkpoint record are all valid and synced;
    - WAL truncation/replay skipping is safe only for records covered by a
      trusted checkpoint.

    Filesystem/kernel ordering below a successful storage sync and certified
    Rust compilation are outside this proof boundary.
 *)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Import ListNotations.

Definition Key := nat.
Definition Value := nat.
Definition ArenaId := nat.
Definition SlotId := nat.
Definition Lsn := nat.
Definition RefMap := Key -> option Value.

Definition empty_map : RefMap := fun _ => None.

Definition lookup (map : RefMap) (key : Key) : option Value :=
  map key.

Definition map_put (map : RefMap) (key : Key) (value : Value) : RefMap :=
  fun query => if Nat.eq_dec query key then Some value else map query.

Definition map_remove (map : RefMap) (key : Key) : RefMap :=
  fun query => if Nat.eq_dec query key then None else map query.

Theorem map_put_lookup_same :
  forall map key value,
    lookup (map_put map key value) key = Some value.
Proof.
  intros map key value.
  unfold lookup, map_put.
  destruct (Nat.eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - contradiction Hneq. reflexivity.
Qed.

Theorem map_remove_lookup_same :
  forall map key,
    lookup (map_remove map key) key = None.
Proof.
  intros map key.
  unfold lookup, map_remove.
  destruct (Nat.eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - contradiction Hneq. reflexivity.
Qed.

Theorem map_put_lookup_other :
  forall map key other value,
    key <> other ->
    lookup (map_put map key value) other = lookup map other.
Proof.
  intros map key other value Hneq.
  unfold lookup, map_put.
  destruct (Nat.eq_dec other key) as [Heq | _].
  - contradiction Hneq. symmetry. exact Heq.
  - reflexivity.
Qed.

Definition ArenaDirty := ArenaId -> bool.
Definition SlotDirty := ArenaId -> SlotId -> bool.

Definition empty_arena_dirty : ArenaDirty := fun _ => false.
Definition empty_slot_dirty : SlotDirty := fun _ _ => false.

Definition mark_arena (dirty : ArenaDirty) (arena : ArenaId) : ArenaDirty :=
  fun query => if Nat.eq_dec query arena then true else dirty query.

Definition mark_slot (dirty : SlotDirty) (arena : ArenaId) (slot : SlotId)
  : SlotDirty :=
  fun query_arena query_slot =>
    if Nat.eq_dec query_arena arena then
      if Nat.eq_dec query_slot slot then true else dirty query_arena query_slot
    else dirty query_arena query_slot.

Record DirtyState : Type := mkDirtyState {
  dirty_arenas : ArenaDirty;
  dirty_slots : SlotDirty;
  slot_tracking_enabled : bool;
  dirty_data_synced : bool
}.

Definition clean_dirty_state : DirtyState :=
  mkDirtyState empty_arena_dirty empty_slot_dirty false true.

Definition arena_is_dirty (state : DirtyState) (arena : ArenaId) : bool :=
  dirty_arenas state arena.

Definition slot_is_dirty
  (state : DirtyState)
  (arena : ArenaId)
  (slot : SlotId)
  : bool :=
  dirty_slots state arena slot.

Definition modify_slot
  (state : DirtyState)
  (arena : ArenaId)
  (slot : SlotId)
  : DirtyState :=
  mkDirtyState
    (mark_arena (dirty_arenas state) arena)
    (mark_slot (dirty_slots state) arena slot)
    (slot_tracking_enabled state)
    false.

Definition enable_slot_tracking (state : DirtyState) : DirtyState :=
  mkDirtyState
    (dirty_arenas state)
    (dirty_slots state)
    true
    (dirty_data_synced state).

Inductive FlushOutcome : Type :=
| FlushOk
| FlushWriteFailed
| FlushSyncFailed.

Definition flush_dirty_state
  (state : DirtyState)
  (outcome : FlushOutcome)
  : DirtyState :=
  match outcome with
  | FlushOk => mkDirtyState empty_arena_dirty empty_slot_dirty
                  (slot_tracking_enabled state) true
  | FlushWriteFailed => state
  | FlushSyncFailed => state
  end.

Theorem modified_slot_marks_arena_dirty :
  forall state arena slot,
    arena_is_dirty (modify_slot state arena slot) arena = true.
Proof.
  intros state arena slot.
  unfold arena_is_dirty, modify_slot. simpl. unfold mark_arena.
  destruct (Nat.eq_dec arena arena) as [_ | Hneq].
  - reflexivity.
  - contradiction Hneq. reflexivity.
Qed.

Theorem modified_slot_marks_slot_dirty :
  forall state arena slot,
    slot_is_dirty (modify_slot state arena slot) arena slot = true.
Proof.
  intros state arena slot.
  unfold slot_is_dirty, modify_slot. simpl. unfold mark_slot.
  destruct (Nat.eq_dec arena arena) as [_ | Harena].
  - destruct (Nat.eq_dec slot slot) as [_ | Hslot].
    + reflexivity.
    + contradiction Hslot. reflexivity.
  - contradiction Harena. reflexivity.
Qed.

Theorem modified_slot_marks_data_unsynced :
  forall state arena slot,
    dirty_data_synced (modify_slot state arena slot) = false.
Proof.
  reflexivity.
Qed.

Theorem modifying_one_slot_preserves_other_arena_evidence :
  forall state arena slot other,
    other <> arena ->
    arena_is_dirty (modify_slot state arena slot) other =
    arena_is_dirty state other.
Proof.
  intros state arena slot other Hneq.
  unfold arena_is_dirty, modify_slot. simpl. unfold mark_arena.
  destruct (Nat.eq_dec other arena) as [Heq | _].
  - exfalso. apply Hneq. exact Heq.
  - reflexivity.
Qed.

Theorem enable_slot_tracking_preserves_dirty_arena_evidence :
  forall state arena,
    arena_is_dirty state arena = true ->
    arena_is_dirty (enable_slot_tracking state) arena = true.
Proof.
  intros state arena Hdirty.
  unfold arena_is_dirty, enable_slot_tracking.
  exact Hdirty.
Qed.

Theorem enable_slot_tracking_enables_tracking :
  forall state,
    slot_tracking_enabled (enable_slot_tracking state) = true.
Proof.
  reflexivity.
Qed.

Theorem failed_write_preserves_dirty_arena :
  forall state arena,
    arena_is_dirty state arena = true ->
    arena_is_dirty (flush_dirty_state state FlushWriteFailed) arena = true.
Proof.
  intros state arena Hdirty.
  exact Hdirty.
Qed.

Theorem failed_write_preserves_dirty_slot :
  forall state arena slot,
    slot_is_dirty state arena slot = true ->
    slot_is_dirty (flush_dirty_state state FlushWriteFailed) arena slot = true.
Proof.
  intros state arena slot Hdirty.
  exact Hdirty.
Qed.

Theorem failed_sync_preserves_dirty_arena :
  forall state arena,
    arena_is_dirty state arena = true ->
    arena_is_dirty (flush_dirty_state state FlushSyncFailed) arena = true.
Proof.
  intros state arena Hdirty.
  exact Hdirty.
Qed.

Theorem failed_sync_preserves_dirty_slot :
  forall state arena slot,
    slot_is_dirty state arena slot = true ->
    slot_is_dirty (flush_dirty_state state FlushSyncFailed) arena slot = true.
Proof.
  intros state arena slot Hdirty.
  exact Hdirty.
Qed.

Theorem successful_flush_clears_arena_evidence :
  forall state arena,
    arena_is_dirty (flush_dirty_state state FlushOk) arena = false.
Proof.
  reflexivity.
Qed.

Theorem successful_flush_clears_slot_evidence :
  forall state arena slot,
    slot_is_dirty (flush_dirty_state state FlushOk) arena slot = false.
Proof.
  reflexivity.
Qed.

Theorem successful_flush_marks_dirty_data_synced :
  forall state,
    dirty_data_synced (flush_dirty_state state FlushOk) = true.
Proof.
  reflexivity.
Qed.

Theorem successful_flush_preserves_tracking_mode :
  forall state,
    slot_tracking_enabled (flush_dirty_state state FlushOk) =
    slot_tracking_enabled state.
Proof.
  reflexivity.
Qed.

Record CheckpointPublication : Type := mkCheckpointPublication {
  publication_dirty_clean : bool;
  publication_data_synced : bool;
  publication_root_descriptor_valid : bool;
  publication_root_descriptor_synced : bool;
  publication_checkpoint_record_synced : bool;
  publication_checkpoint_lsn : Lsn;
  publication_map : RefMap
}.

Definition trusted_publication (publication : CheckpointPublication) : bool :=
  publication_dirty_clean publication &&
  publication_data_synced publication &&
  publication_root_descriptor_valid publication &&
  publication_root_descriptor_synced publication &&
  publication_checkpoint_record_synced publication.

Definition recovery_base (publication : CheckpointPublication) : RefMap :=
  if trusted_publication publication
  then publication_map publication
  else empty_map.

Definition recovery_threshold
  (publication : CheckpointPublication)
  : option Lsn :=
  if trusted_publication publication
  then Some (publication_checkpoint_lsn publication)
  else None.

Definition can_truncate_record
  (publication : CheckpointPublication)
  (record_lsn : Lsn)
  : bool :=
  trusted_publication publication &&
  (record_lsn <=? publication_checkpoint_lsn publication).

Theorem trusted_publication_requires_clean_dirty_state :
  forall publication,
    trusted_publication publication = true ->
    publication_dirty_clean publication = true.
Proof.
  intros [dirty_clean data_synced root_valid root_synced checkpoint_synced lsn map].
  simpl.
  destruct dirty_clean; simpl; intros H; try discriminate.
  reflexivity.
Qed.

Theorem trusted_publication_requires_data_sync :
  forall publication,
    trusted_publication publication = true ->
    publication_data_synced publication = true.
Proof.
  intros [dirty_clean data_synced root_valid root_synced checkpoint_synced lsn map].
  simpl.
  destruct dirty_clean; simpl; try discriminate.
  destruct data_synced; simpl; intros H; try discriminate.
  reflexivity.
Qed.

Theorem trusted_publication_requires_valid_root_descriptor :
  forall publication,
    trusted_publication publication = true ->
    publication_root_descriptor_valid publication = true.
Proof.
  intros [dirty_clean data_synced root_valid root_synced checkpoint_synced lsn map].
  simpl.
  destruct dirty_clean; simpl; try discriminate.
  destruct data_synced; simpl; try discriminate.
  destruct root_valid; simpl; intros H; try discriminate.
  reflexivity.
Qed.

Theorem trusted_publication_requires_root_sync :
  forall publication,
    trusted_publication publication = true ->
    publication_root_descriptor_synced publication = true.
Proof.
  intros [dirty_clean data_synced root_valid root_synced checkpoint_synced lsn map].
  simpl.
  destruct dirty_clean; simpl; try discriminate.
  destruct data_synced; simpl; try discriminate.
  destruct root_valid; simpl; try discriminate.
  destruct root_synced; simpl; intros H; try discriminate.
  reflexivity.
Qed.

Theorem trusted_publication_requires_checkpoint_record_sync :
  forall publication,
    trusted_publication publication = true ->
    publication_checkpoint_record_synced publication = true.
Proof.
  intros [dirty_clean data_synced root_valid root_synced checkpoint_synced lsn map].
  simpl.
  destruct dirty_clean; simpl; try discriminate.
  destruct data_synced; simpl; try discriminate.
  destruct root_valid; simpl; try discriminate.
  destruct root_synced; simpl; try discriminate.
  destruct checkpoint_synced; simpl; intros H; try discriminate.
  reflexivity.
Qed.

Theorem unclean_dirty_state_is_not_trusted :
  forall publication,
    publication_dirty_clean publication = false ->
    trusted_publication publication = false.
Proof.
  intros [dirty_clean data_synced root_valid root_synced checkpoint_synced lsn map] H.
  simpl in *.
  rewrite H.
  reflexivity.
Qed.

Theorem unsynced_dirty_data_is_not_trusted :
  forall publication,
    publication_data_synced publication = false ->
    trusted_publication publication = false.
Proof.
  intros [dirty_clean data_synced root_valid root_synced checkpoint_synced lsn map] H.
  simpl in *.
  rewrite H.
  destruct dirty_clean; reflexivity.
Qed.

Theorem invalid_root_descriptor_is_not_trusted :
  forall publication,
    publication_root_descriptor_valid publication = false ->
    trusted_publication publication = false.
Proof.
  intros [dirty_clean data_synced root_valid root_synced checkpoint_synced lsn map] H.
  simpl in *.
  rewrite H.
  destruct dirty_clean, data_synced; reflexivity.
Qed.

Theorem unsynced_root_descriptor_is_not_trusted :
  forall publication,
    publication_root_descriptor_synced publication = false ->
    trusted_publication publication = false.
Proof.
  intros [dirty_clean data_synced root_valid root_synced checkpoint_synced lsn map] H.
  simpl in *.
  rewrite H.
  destruct dirty_clean, data_synced, root_valid; reflexivity.
Qed.

Theorem unsynced_checkpoint_record_is_not_trusted :
  forall publication,
    publication_checkpoint_record_synced publication = false ->
    trusted_publication publication = false.
Proof.
  intros [dirty_clean data_synced root_valid root_synced checkpoint_synced lsn map] H.
  simpl in *.
  rewrite H.
  destruct dirty_clean, data_synced, root_valid, root_synced; reflexivity.
Qed.

Theorem trusted_publication_has_recovery_threshold :
  forall publication,
    trusted_publication publication = true ->
    recovery_threshold publication =
    Some (publication_checkpoint_lsn publication).
Proof.
  intros publication Htrusted.
  unfold recovery_threshold.
  rewrite Htrusted.
  reflexivity.
Qed.

Theorem untrusted_publication_has_no_recovery_threshold :
  forall publication,
    trusted_publication publication = false ->
    recovery_threshold publication = None.
Proof.
  intros publication Htrusted.
  unfold recovery_threshold.
  rewrite Htrusted.
  reflexivity.
Qed.

Theorem trusted_publication_uses_checkpoint_map :
  forall publication,
    trusted_publication publication = true ->
    recovery_base publication = publication_map publication.
Proof.
  intros publication Htrusted.
  unfold recovery_base.
  rewrite Htrusted.
  reflexivity.
Qed.

Theorem untrusted_publication_replays_from_empty :
  forall publication,
    trusted_publication publication = false ->
    recovery_base publication = empty_map.
Proof.
  intros publication Htrusted.
  unfold recovery_base.
  rewrite Htrusted.
  reflexivity.
Qed.

Theorem truncation_requires_trusted_publication :
  forall publication record_lsn,
    can_truncate_record publication record_lsn = true ->
    trusted_publication publication = true.
Proof.
  intros publication record_lsn Htruncate.
  unfold can_truncate_record in Htruncate.
  destruct (trusted_publication publication) eqn:Htrusted.
  - reflexivity.
  - simpl in Htruncate. discriminate.
Qed.

Theorem truncation_requires_checkpoint_coverage :
  forall publication record_lsn,
    can_truncate_record publication record_lsn = true ->
    record_lsn <= publication_checkpoint_lsn publication.
Proof.
  intros publication record_lsn Htruncate.
  unfold can_truncate_record in Htruncate.
  destruct (trusted_publication publication); simpl in Htruncate; try discriminate.
  apply Nat.leb_le. exact Htruncate.
Qed.

Theorem untrusted_publication_cannot_truncate :
  forall publication record_lsn,
    trusted_publication publication = false ->
    can_truncate_record publication record_lsn = false.
Proof.
  intros publication record_lsn Htrusted.
  unfold can_truncate_record.
  rewrite Htrusted.
  reflexivity.
Qed.

Inductive WalOp : Type :=
| WalPut : Key -> Value -> WalOp
| WalDelete : Key -> WalOp
| WalCheckpoint : Lsn -> WalOp.

Record WalEntry : Type := mkWalEntry {
  entry_lsn : Lsn;
  entry_op : WalOp
}.

Definition apply_wal_entry (map : RefMap) (entry : WalEntry) : RefMap :=
  match entry_op entry with
  | WalPut key value => map_put map key value
  | WalDelete key => map_remove map key
  | WalCheckpoint _ => map
  end.

Fixpoint replay_entries (entries : list WalEntry) (map : RefMap) : RefMap :=
  match entries with
  | [] => map
  | entry :: rest => replay_entries rest (apply_wal_entry map entry)
  end.

Definition should_skip_entry
  (publication : CheckpointPublication)
  (entry : WalEntry)
  : bool :=
  can_truncate_record publication (entry_lsn entry).

Fixpoint replay_entries_after_publication
  (publication : CheckpointPublication)
  (entries : list WalEntry)
  (map : RefMap)
  : RefMap :=
  match entries with
  | [] => map
  | entry :: rest =>
      if should_skip_entry publication entry then
        replay_entries_after_publication publication rest map
      else
        replay_entries_after_publication publication rest
          (apply_wal_entry map entry)
  end.

Definition recover_from_publication
  (publication : CheckpointPublication)
  (entries : list WalEntry)
  : RefMap :=
  replay_entries_after_publication publication entries (recovery_base publication).

Theorem replay_entries_app :
  forall left right map,
    replay_entries (left ++ right) map =
    replay_entries right (replay_entries left map).
Proof.
  induction left as [| entry rest IH]; intros right map.
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Theorem untrusted_publication_replays_old_put :
  forall publication lsn key value,
    trusted_publication publication = false ->
    lookup
      (recover_from_publication
         publication
         [mkWalEntry lsn (WalPut key value)])
      key = Some value.
Proof.
  intros publication lsn key value Htrusted.
  unfold recover_from_publication.
  rewrite (untrusted_publication_replays_from_empty publication Htrusted).
  simpl.
  unfold should_skip_entry, can_truncate_record.
  simpl.
  rewrite Htrusted.
  simpl.
  apply map_put_lookup_same.
Qed.

Theorem trusted_checkpoint_skips_covered_put :
  forall publication lsn key value map,
    trusted_publication publication = true ->
    lsn <= publication_checkpoint_lsn publication ->
    replay_entries_after_publication
      publication
      [mkWalEntry lsn (WalPut key value)]
      map = map.
Proof.
  intros publication lsn key value map Htrusted Hle.
  simpl.
  unfold should_skip_entry, can_truncate_record.
  simpl.
  rewrite Htrusted.
  assert (Hskip : (lsn <=? publication_checkpoint_lsn publication) = true).
  { apply Nat.leb_le. exact Hle. }
  rewrite Hskip.
  reflexivity.
Qed.

Theorem trusted_checkpoint_replays_tail_put :
  forall publication lsn key value map,
    trusted_publication publication = true ->
    publication_checkpoint_lsn publication < lsn ->
    lookup
      (replay_entries_after_publication
         publication
         [mkWalEntry lsn (WalPut key value)]
         map)
      key = Some value.
Proof.
  intros publication lsn key value map Htrusted Hlt.
  simpl.
  unfold should_skip_entry, can_truncate_record.
  simpl.
  rewrite Htrusted.
  assert (Hnoskip : (lsn <=? publication_checkpoint_lsn publication) = false).
  { apply Nat.leb_gt. exact Hlt. }
  rewrite Hnoskip.
  apply map_put_lookup_same.
Qed.

Theorem trusted_checkpoint_replays_tail_delete :
  forall publication lsn key map,
    trusted_publication publication = true ->
    publication_checkpoint_lsn publication < lsn ->
    lookup
      (replay_entries_after_publication
         publication
         [mkWalEntry lsn (WalDelete key)]
         map)
      key = None.
Proof.
  intros publication lsn key map Htrusted Hlt.
  simpl.
  unfold should_skip_entry, can_truncate_record.
  simpl.
  rewrite Htrusted.
  assert (Hnoskip : (lsn <=? publication_checkpoint_lsn publication) = false).
  { apply Nat.leb_gt. exact Hlt. }
  rewrite Hnoskip.
  apply map_remove_lookup_same.
Qed.

Theorem checkpoint_records_do_not_change_reference_map :
  forall publication lsn map,
    replay_entries_after_publication
      publication
      [mkWalEntry lsn (WalCheckpoint lsn)]
      map = map.
Proof.
  intros publication lsn map.
  simpl.
  unfold should_skip_entry, can_truncate_record.
  simpl.
  destruct (trusted_publication publication); simpl.
  - destruct (lsn <=? publication_checkpoint_lsn publication); reflexivity.
  - reflexivity.
Qed.

Theorem failed_flush_publication_cannot_skip :
  forall publication record_lsn,
    publication_data_synced publication = false ->
    can_truncate_record publication record_lsn = false.
Proof.
  intros publication record_lsn Hunsynced.
  apply untrusted_publication_cannot_truncate.
  apply unsynced_dirty_data_is_not_trusted.
  exact Hunsynced.
Qed.

Theorem invalid_root_publication_cannot_skip :
  forall publication record_lsn,
    publication_root_descriptor_valid publication = false ->
    can_truncate_record publication record_lsn = false.
Proof.
  intros publication record_lsn Hinvalid.
  apply untrusted_publication_cannot_truncate.
  apply invalid_root_descriptor_is_not_trusted.
  exact Hinvalid.
Qed.

Theorem unsynced_checkpoint_record_cannot_skip :
  forall publication record_lsn,
    publication_checkpoint_record_synced publication = false ->
    can_truncate_record publication record_lsn = false.
Proof.
  intros publication record_lsn Hunsynced.
  apply untrusted_publication_cannot_truncate.
  apply unsynced_checkpoint_record_is_not_trusted.
  exact Hunsynced.
Qed.

Theorem dirty_slot_retry_after_failed_sync_then_success :
  forall state arena slot,
    slot_is_dirty state arena slot = true ->
    slot_is_dirty
      (flush_dirty_state
         (flush_dirty_state state FlushSyncFailed)
         FlushOk)
      arena slot = false /\
    dirty_data_synced
      (flush_dirty_state
         (flush_dirty_state state FlushSyncFailed)
         FlushOk) = true.
Proof.
  intros state arena slot _.
  split; reflexivity.
Qed.

Theorem checkpoint_publication_exact_conditions :
  forall dirty_clean data_synced root_valid root_synced checkpoint_synced lsn map,
    trusted_publication
      (mkCheckpointPublication dirty_clean data_synced root_valid root_synced
         checkpoint_synced lsn map) = true ->
    dirty_clean = true /\
    data_synced = true /\
    root_valid = true /\
    root_synced = true /\
    checkpoint_synced = true.
Proof.
  intros dirty_clean data_synced root_valid root_synced checkpoint_synced lsn map Htrusted.
  simpl in Htrusted.
  destruct dirty_clean; simpl in Htrusted; try discriminate.
  destruct data_synced; simpl in Htrusted; try discriminate.
  destruct root_valid; simpl in Htrusted; try discriminate.
  destruct root_synced; simpl in Htrusted; try discriminate.
  destruct checkpoint_synced; simpl in Htrusted; try discriminate.
  repeat split; reflexivity.
Qed.
