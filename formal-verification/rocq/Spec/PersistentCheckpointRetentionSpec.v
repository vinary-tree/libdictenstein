(** Persistent checkpoint/WAL retention model.

    This specification captures the persistence boundary for checkpoint
    publication, WAL retention, and corruption rebuild:

    - a valid checkpoint may justify replaying only WAL records after its LSN;
    - an invalid checkpoint/root descriptor must not justify any skip threshold;
    - active WAL records must be retained before recreating the data file;
    - corruption rebuild replays archive, pending, then active-tail segments;
    - byte and char persistent tries share this same retention model.

    Kernel/filesystem internals below the WAL segment abstraction and certified
    Rust compilation are outside this proof boundary.
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

Definition map_eq (left right : RefMap) : Prop :=
  forall key, lookup left key = lookup right key.

Theorem map_put_lookup_same :
  forall map key value,
    lookup (map_put map key value) key = Some value.
Proof.
  intros map key value.
  unfold lookup, map_put.
  destruct (Nat.eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem map_remove_lookup_same :
  forall map key,
    lookup (map_remove map key) key = None.
Proof.
  intros map key.
  unfold lookup, map_remove.
  destruct (Nat.eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem map_put_lookup_other :
  forall map key other value,
    key <> other ->
    lookup (map_put map key value) other = lookup map other.
Proof.
  intros map key other value Hneq.
  unfold lookup, map_put.
  destruct (Nat.eq_dec other key) as [Heq | _].
  - exfalso. apply Hneq. symmetry. exact Heq.
  - reflexivity.
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

Fixpoint replay_segments (segments : list (list WalEntry)) (map : RefMap)
  : RefMap :=
  match segments with
  | [] => map
  | segment :: rest => replay_segments rest (replay_entries segment map)
  end.

Definition should_skip (threshold : option Lsn) (entry : WalEntry) : bool :=
  match threshold with
  | Some checkpoint_lsn => entry_lsn entry <=? checkpoint_lsn
  | None => false
  end.

Fixpoint replay_entries_after
  (threshold : option Lsn)
  (entries : list WalEntry)
  (map : RefMap)
  : RefMap :=
  match entries with
  | [] => map
  | entry :: rest =>
      if should_skip threshold entry then
        replay_entries_after threshold rest map
      else
        replay_entries_after threshold rest (apply_wal_entry map entry)
  end.

Fixpoint replay_segments_after
  (threshold : option Lsn)
  (segments : list (list WalEntry))
  (map : RefMap)
  : RefMap :=
  match segments with
  | [] => map
  | segment :: rest =>
      replay_segments_after threshold rest
        (replay_entries_after threshold segment map)
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

Theorem replay_segments_app :
  forall left right map,
    replay_segments (left ++ right) map =
    replay_segments right (replay_segments left map).
Proof.
  induction left as [| segment rest IH]; intros right map.
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Record CheckpointState : Type := mkCheckpointState {
  checkpoint_valid : bool;
  checkpoint_lsn : Lsn;
  checkpoint_map : RefMap
}.

Definition recovery_base (checkpoint : CheckpointState) : RefMap :=
  if checkpoint_valid checkpoint then checkpoint_map checkpoint else empty_map.

Definition recovery_threshold (checkpoint : CheckpointState) : option Lsn :=
  if checkpoint_valid checkpoint then Some (checkpoint_lsn checkpoint) else None.

Definition recover_from_retained
  (checkpoint : CheckpointState)
  (segments : list (list WalEntry))
  : RefMap :=
  replay_segments_after
    (recovery_threshold checkpoint)
    segments
    (recovery_base checkpoint).

Definition invalid_checkpoint : CheckpointState :=
  mkCheckpointState false 0 empty_map.

Theorem invalid_checkpoint_has_no_skip_threshold :
  recovery_threshold invalid_checkpoint = None.
Proof.
  reflexivity.
Qed.

Theorem valid_checkpoint_uses_checkpoint_lsn :
  forall lsn map,
    recovery_threshold (mkCheckpointState true lsn map) = Some lsn.
Proof.
  reflexivity.
Qed.

Theorem invalid_checkpoint_replays_from_empty :
  forall segments,
    recover_from_retained invalid_checkpoint segments =
    replay_segments_after None segments empty_map.
Proof.
  reflexivity.
Qed.

Theorem invalid_checkpoint_replays_old_put :
  forall lsn key value,
    lookup
      (recover_from_retained
         invalid_checkpoint
         [[mkWalEntry lsn (WalPut key value)]])
      key = Some value.
Proof.
  intros lsn key value.
  simpl.
  apply map_put_lookup_same.
Qed.

Theorem valid_checkpoint_skips_covered_put :
  forall checkpoint_lsn' disk_map lsn key value,
    lsn <= checkpoint_lsn' ->
    replay_entries_after
      (Some checkpoint_lsn')
      [mkWalEntry lsn (WalPut key value)]
      disk_map = disk_map.
Proof.
  intros checkpoint_lsn' disk_map lsn key value Hle.
  simpl. unfold should_skip. simpl.
  assert (Hskip : (lsn <=? checkpoint_lsn') = true).
  { apply Nat.leb_le. exact Hle. }
  rewrite Hskip.
  reflexivity.
Qed.

Theorem valid_checkpoint_replays_tail_put :
  forall checkpoint_lsn' disk_map lsn key value,
    checkpoint_lsn' < lsn ->
    lookup
      (replay_entries_after
         (Some checkpoint_lsn')
         [mkWalEntry lsn (WalPut key value)]
         disk_map)
      key = Some value.
Proof.
  intros checkpoint_lsn' disk_map lsn key value Hlt.
  simpl. unfold should_skip. simpl.
  assert (Hnoskip : (lsn <=? checkpoint_lsn') = false).
  { apply Nat.leb_gt. exact Hlt. }
  rewrite Hnoskip.
  apply map_put_lookup_same.
Qed.

Definition segment_has_records (segment : list WalEntry) : bool :=
  match segment with
  | [] => false
  | _ :: _ => true
  end.

Definition collect_retained_segments
  (archive pending : list (list WalEntry))
  (active : list WalEntry)
  : list (list WalEntry) :=
  archive ++ pending ++
    if segment_has_records active then [active] else [].

Theorem empty_active_wal_is_not_retained :
  forall archive pending,
    collect_retained_segments archive pending [] = archive ++ pending.
Proof.
  intros archive pending.
  unfold collect_retained_segments.
  rewrite app_nil_r.
  reflexivity.
Qed.

Theorem nonempty_active_wal_is_retained :
  forall archive pending active,
    active <> [] ->
    In active (collect_retained_segments archive pending active).
Proof.
  intros archive pending active Hnonempty.
  destruct active as [| entry rest].
  - contradiction Hnonempty. reflexivity.
  - unfold collect_retained_segments, segment_has_records.
    apply in_or_app. right.
    apply in_or_app. right.
    simpl. left. reflexivity.
Qed.

Theorem retained_active_tail_is_replayed :
  forall active map,
    active <> [] ->
    replay_segments (collect_retained_segments [] [] active) map =
    replay_entries active map.
Proof.
  intros active map Hnonempty.
  destruct active as [| entry rest].
  - contradiction Hnonempty. reflexivity.
  - reflexivity.
Qed.

Theorem archive_then_active_tail_replay_order :
  forall archive pending active map,
    active <> [] ->
    replay_segments (collect_retained_segments archive pending active) map =
    replay_entries active (replay_segments pending (replay_segments archive map)).
Proof.
  intros archive pending active map Hnonempty.
  unfold collect_retained_segments.
  destruct active as [| entry rest].
  - contradiction Hnonempty. reflexivity.
  - simpl segment_has_records.
    repeat rewrite replay_segments_app.
    reflexivity.
Qed.

Theorem active_tail_put_visible_after_archives :
  forall archive pending lsn key value,
    lookup
      (replay_segments
         (collect_retained_segments
            archive
            pending
            [mkWalEntry lsn (WalPut key value)])
         empty_map)
      key = Some value.
Proof.
  intros archive pending lsn key value.
  rewrite archive_then_active_tail_replay_order.
  - simpl. apply map_put_lookup_same.
  - discriminate.
Qed.

Theorem dropping_active_tail_can_lose_put :
  forall lsn key value,
    lookup
      (replay_segments
         (collect_retained_segments [] [] [mkWalEntry lsn (WalPut key value)])
         empty_map)
      key = Some value /\
    lookup
      (replay_segments (collect_retained_segments [] [] []) empty_map)
      key = None.
Proof.
  intros lsn key value.
  split.
  - simpl. apply map_put_lookup_same.
  - reflexivity.
Qed.

Theorem active_batch_then_remove_matches_reference :
  forall lsn_batch lsn_remove key value map,
    lookup
      (replay_entries
         [ mkWalEntry lsn_batch (WalBatchPut [(key, value)]);
           mkWalEntry lsn_remove (WalDelete key) ]
         map)
      key = None.
Proof.
  intros lsn_batch lsn_remove key value map.
  simpl.
  apply map_remove_lookup_same.
Qed.

Definition truncate_safe
  (checkpoint : CheckpointState)
  (removed_prefix_lsn : Lsn)
  : Prop :=
  checkpoint_valid checkpoint = true /\
  removed_prefix_lsn <= checkpoint_lsn checkpoint.

Theorem invalid_checkpoint_cannot_justify_truncation :
  forall checkpoint removed_prefix_lsn,
    checkpoint_valid checkpoint = false ->
    ~ truncate_safe checkpoint removed_prefix_lsn.
Proof.
  intros checkpoint removed_prefix_lsn Hinvalid Hsafe.
  unfold truncate_safe in Hsafe.
  destruct Hsafe as [Hvalid _].
  rewrite Hinvalid in Hvalid.
  discriminate.
Qed.

Theorem safe_truncation_prefix_is_checkpointed :
  forall checkpoint removed_prefix_lsn,
    truncate_safe checkpoint removed_prefix_lsn ->
    removed_prefix_lsn <= checkpoint_lsn checkpoint.
Proof.
  intros checkpoint removed_prefix_lsn Hsafe.
  unfold truncate_safe in Hsafe.
  tauto.
Qed.

Inductive Backend : Type :=
| ByteBackend
| CharBackend.

Definition recover_backend
  (_backend : Backend)
  (checkpoint : CheckpointState)
  (segments : list (list WalEntry))
  : RefMap :=
  recover_from_retained checkpoint segments.

Theorem byte_and_char_use_same_retention_model :
  forall checkpoint segments,
    map_eq
      (recover_backend ByteBackend checkpoint segments)
      (recover_backend CharBackend checkpoint segments).
Proof.
  intros checkpoint segments key.
  reflexivity.
Qed.
