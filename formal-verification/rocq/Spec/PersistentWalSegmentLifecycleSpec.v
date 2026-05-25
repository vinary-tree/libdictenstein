(** Persistent WAL segment lifecycle model.

    This specification captures the segment-management obligations used by
    checkpointed persistent tries:

    - active WAL segments rotate to pending/archived segments without changing
      their record content or LSN range;
    - collection order is the logical first-LSN order, not filename order;
    - reopening after retained archive/pending segments must continue at
      max_lsn + 1 instead of resetting to 1;
    - archive pruning is safe only for segments fully covered by a valid
      checkpoint;
    - replay of archive, pending, and active-tail segments matches the
      reference map replay.
 *)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Import ListNotations.

Definition Key := nat.
Definition Value := nat.
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

Inductive WalOp : Type :=
| WalPut : Key -> Value -> WalOp
| WalDelete : Key -> WalOp
| WalBatchPut : list (Key * Value) -> WalOp
| WalCheckpoint : Lsn -> WalOp.

Record WalEntry : Type := mkWalEntry {
  entry_lsn : Lsn;
  entry_op : WalOp
}.

Definition apply_batch_put
  (entries : list (Key * Value))
  (map : RefMap)
  : RefMap :=
  fold_left
    (fun acc entry =>
      let '(key, value) := entry in map_put acc key value)
    entries
    map.

Definition apply_wal_entry (map : RefMap) (entry : WalEntry) : RefMap :=
  match entry_op entry with
  | WalPut key value => map_put map key value
  | WalDelete key => map_remove map key
  | WalBatchPut entries => apply_batch_put entries map
  | WalCheckpoint _ => map
  end.

Fixpoint replay_entries (entries : list WalEntry) (map : RefMap) : RefMap :=
  match entries with
  | [] => map
  | entry :: rest => replay_entries rest (apply_wal_entry map entry)
  end.

Theorem replay_entries_app :
  forall left right map,
    replay_entries (left ++ right) map =
    replay_entries right (replay_entries left map).
Proof.
  induction left as [| entry rest IH]; intros right map.
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Inductive SegmentState : Type :=
| Active
| Pending
| Archived.

Record Segment : Type := mkSegment {
  segment_state : SegmentState;
  segment_first_lsn : Lsn;
  segment_last_lsn : Lsn;
  segment_entries : list WalEntry
}.

Definition segment_has_records (segment : Segment) : bool :=
  match segment_entries segment with
  | [] => false
  | _ :: _ => true
  end.

Definition replay_segment (segment : Segment) (map : RefMap) : RefMap :=
  replay_entries (segment_entries segment) map.

Fixpoint replay_segments (segments : list Segment) (map : RefMap) : RefMap :=
  match segments with
  | [] => map
  | segment :: rest => replay_segments rest (replay_segment segment map)
  end.

Theorem replay_segments_app :
  forall left right map,
    replay_segments (left ++ right) map =
    replay_segments right (replay_segments left map).
Proof.
  induction left as [| segment rest IH]; intros right map.
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Definition rotate_active_to_pending (segment : Segment) : Segment :=
  mkSegment Pending
    (segment_first_lsn segment)
    (segment_last_lsn segment)
    (segment_entries segment).

Definition sync_pending_to_archive (segment : Segment) : Segment :=
  mkSegment Archived
    (segment_first_lsn segment)
    (segment_last_lsn segment)
    (segment_entries segment).

Theorem rotate_preserves_entries :
  forall segment,
    segment_entries (rotate_active_to_pending segment) =
    segment_entries segment.
Proof.
  reflexivity.
Qed.

Theorem rotate_preserves_lsn_range :
  forall segment,
    segment_first_lsn (rotate_active_to_pending segment) =
      segment_first_lsn segment /\
    segment_last_lsn (rotate_active_to_pending segment) =
      segment_last_lsn segment.
Proof.
  intros segment. split; reflexivity.
Qed.

Theorem sync_preserves_entries :
  forall segment,
    segment_entries (sync_pending_to_archive segment) =
    segment_entries segment.
Proof.
  reflexivity.
Qed.

Theorem sync_preserves_lsn_range :
  forall segment,
    segment_first_lsn (sync_pending_to_archive segment) =
      segment_first_lsn segment /\
    segment_last_lsn (sync_pending_to_archive segment) =
      segment_last_lsn segment.
Proof.
  intros segment. split; reflexivity.
Qed.

Theorem state_changes_do_not_change_replay :
  forall segment map,
    replay_segment (sync_pending_to_archive (rotate_active_to_pending segment)) map =
    replay_segment segment map.
Proof.
  reflexivity.
Qed.

Definition collect_retained_segments
  (archive pending : list Segment)
  (active : option Segment)
  : list Segment :=
  archive ++ pending ++
    match active with
    | Some segment =>
        if segment_has_records segment then [segment] else []
    | None => []
    end.

Theorem empty_active_tail_not_retained :
  forall archive pending first last,
    collect_retained_segments
      archive pending (Some (mkSegment Active first last [])) =
    archive ++ pending.
Proof.
  intros archive pending first last.
  unfold collect_retained_segments, segment_has_records.
  rewrite app_nil_r.
  reflexivity.
Qed.

Theorem nonempty_active_tail_retained :
  forall archive pending active,
    segment_entries active <> [] ->
    In active (collect_retained_segments archive pending (Some active)).
Proof.
  intros archive pending active Hnonempty.
  unfold collect_retained_segments.
  destruct (segment_entries active) as [| entry rest] eqn:Hentries.
  - contradiction Hnonempty. reflexivity.
  - unfold segment_has_records. rewrite Hentries. simpl.
    apply in_or_app. right.
    apply in_or_app. right.
    simpl. left. reflexivity.
Qed.

Theorem archive_pending_active_replay_order :
  forall archive pending active map,
    segment_entries active <> [] ->
    replay_segments
      (collect_retained_segments archive pending (Some active))
      map =
    replay_segment active (replay_segments pending (replay_segments archive map)).
Proof.
  intros archive pending active map Hnonempty.
  unfold collect_retained_segments.
  destruct (segment_entries active) as [| entry rest] eqn:Hentries.
  - contradiction Hnonempty. reflexivity.
  - unfold segment_has_records. rewrite Hentries. simpl.
    repeat rewrite replay_segments_app.
    reflexivity.
Qed.

Definition order_pair_by_first_lsn (left right : Segment) : list Segment :=
  if segment_first_lsn left <=? segment_first_lsn right
  then [left; right]
  else [right; left].

Theorem lsn_order_swaps_filename_order_when_needed :
  forall left right,
    segment_first_lsn right < segment_first_lsn left ->
    order_pair_by_first_lsn left right = [right; left].
Proof.
  intros left right Hlt.
  unfold order_pair_by_first_lsn.
  assert (Hcmp : (segment_first_lsn left <=? segment_first_lsn right) = false).
  { apply Nat.leb_gt. exact Hlt. }
  rewrite Hcmp.
  reflexivity.
Qed.

Theorem lsn_order_keeps_already_ordered_pair :
  forall left right,
    segment_first_lsn left <= segment_first_lsn right ->
    order_pair_by_first_lsn left right = [left; right].
Proof.
  intros left right Hle.
  unfold order_pair_by_first_lsn.
  assert (Hcmp : (segment_first_lsn left <=? segment_first_lsn right) = true).
  { apply Nat.leb_le. exact Hle. }
  rewrite Hcmp.
  reflexivity.
Qed.

Definition next_lsn_after_segments (segments : list Segment) : Lsn :=
  S (fold_left Nat.max (map segment_last_lsn segments) 0).

Definition synced_lsn_after_segments (segments : list Segment) : Lsn :=
  fold_left Nat.max (map segment_last_lsn segments) 0.

Theorem next_lsn_after_empty_segments :
  next_lsn_after_segments [] = 1.
Proof.
  reflexivity.
Qed.

Theorem synced_lsn_after_empty_segments :
  synced_lsn_after_segments [] = 0.
Proof.
  reflexivity.
Qed.

Theorem next_lsn_after_single_segment :
  forall segment,
    next_lsn_after_segments [segment] =
    S (segment_last_lsn segment).
Proof.
  intros segment.
  unfold next_lsn_after_segments.
  simpl.
  reflexivity.
Qed.

Theorem synced_lsn_after_single_segment :
  forall segment,
    synced_lsn_after_segments [segment] =
    segment_last_lsn segment.
Proof.
  intros segment.
  unfold synced_lsn_after_segments.
  simpl.
  reflexivity.
Qed.

Theorem reopen_after_archive_continues_after_retained_lsn :
  forall segment,
    next_lsn_after_segments [segment] =
    segment_last_lsn segment + 1.
Proof.
  intros segment.
  rewrite next_lsn_after_single_segment.
  lia.
Qed.

Theorem reopen_after_archive_restores_synced_frontier :
  forall segment,
    synced_lsn_after_segments [segment] =
    segment_last_lsn segment.
Proof.
  apply synced_lsn_after_single_segment.
Qed.

Theorem reset_to_one_after_nonempty_archive_breaks_continuity :
  forall segment,
    0 < segment_last_lsn segment ->
    1 <> next_lsn_after_segments [segment].
Proof.
  intros segment Hpositive Heq.
  rewrite next_lsn_after_single_segment in Heq.
  lia.
Qed.

Record CheckpointState : Type := mkCheckpointState {
  checkpoint_valid : bool;
  checkpoint_lsn : Lsn
}.

Definition segment_safe_to_prune
  (checkpoint : CheckpointState)
  (segment : Segment)
  : bool :=
  checkpoint_valid checkpoint &&
    (segment_last_lsn segment <=? checkpoint_lsn checkpoint).

Definition prune_segments
  (checkpoint : CheckpointState)
  (segments : list Segment)
  : list Segment :=
  filter
    (fun segment => negb (segment_safe_to_prune checkpoint segment))
    segments.

Theorem invalid_checkpoint_prunes_nothing :
  forall lsn segments,
    prune_segments (mkCheckpointState false lsn) segments = segments.
Proof.
  intros lsn segments.
  induction segments as [| segment rest IH].
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Theorem valid_checkpoint_prunes_covered_singleton :
  forall checkpoint_lsn' segment,
    segment_last_lsn segment <= checkpoint_lsn' ->
    prune_segments (mkCheckpointState true checkpoint_lsn') [segment] = [].
Proof.
  intros checkpoint_lsn' segment Hcovered.
  unfold prune_segments.
  simpl.
  unfold segment_safe_to_prune.
  simpl.
  assert (Hle : (segment_last_lsn segment <=? checkpoint_lsn') = true).
  { apply Nat.leb_le. exact Hcovered. }
  rewrite Hle.
  reflexivity.
Qed.

Theorem valid_checkpoint_retains_uncovered_singleton :
  forall checkpoint_lsn' segment,
    checkpoint_lsn' < segment_last_lsn segment ->
    prune_segments (mkCheckpointState true checkpoint_lsn') [segment] = [segment].
Proof.
  intros checkpoint_lsn' segment Huncovered.
  unfold prune_segments.
  simpl.
  unfold segment_safe_to_prune.
  simpl.
  assert (Hgt : (segment_last_lsn segment <=? checkpoint_lsn') = false).
  { apply Nat.leb_gt. exact Huncovered. }
  rewrite Hgt.
  reflexivity.
Qed.

Theorem pruned_singleton_was_checkpoint_covered :
  forall checkpoint_lsn' segment,
    prune_segments (mkCheckpointState true checkpoint_lsn') [segment] = [] ->
    segment_last_lsn segment <= checkpoint_lsn'.
Proof.
  intros checkpoint_lsn' segment Hpruned.
  unfold prune_segments in Hpruned.
  simpl in Hpruned.
  unfold segment_safe_to_prune in Hpruned.
  simpl in Hpruned.
  destruct (segment_last_lsn segment <=? checkpoint_lsn') eqn:Hle.
  - apply Nat.leb_le. exact Hle.
  - discriminate Hpruned.
Qed.

Theorem checkpoint_record_does_not_change_replay :
  forall lsn map,
    replay_entries [mkWalEntry lsn (WalCheckpoint lsn)] map = map.
Proof.
  reflexivity.
Qed.

Theorem batch_then_delete_replay_removes_key :
  forall lsn_batch lsn_delete key value map,
    lookup
      (replay_entries
        [ mkWalEntry lsn_batch (WalBatchPut [(key, value)]);
          mkWalEntry lsn_delete (WalDelete key) ]
        map)
      key = None.
Proof.
  intros lsn_batch lsn_delete key value map.
  simpl.
  apply map_remove_lookup_same.
Qed.

Theorem active_tail_put_visible_after_archive :
  forall archived active_lsn key value,
    lookup
      (replay_segments
        (collect_retained_segments
          [archived]
          []
          (Some (mkSegment Active active_lsn active_lsn
                   [mkWalEntry active_lsn (WalPut key value)])))
        empty_map)
      key = Some value.
Proof.
  intros archived active_lsn key value.
  rewrite archive_pending_active_replay_order.
  - simpl. apply map_put_lookup_same.
  - discriminate.
Qed.
