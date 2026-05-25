(** Persistent compaction safety model.

    This specification captures the proof boundary for byte persistent trie
    compaction.  Compaction is a file rewrite: it copies the visible trie
    snapshot into a temporary trie, checkpoints the temporary trie, then
    optionally finalizes by renaming it over the original.

    The checked obligations are semantic rather than performance-oriented:

    - the compacted trie preserves the exact key/value/term-only snapshot;
    - term-count equality is not a sufficient verification condition;
    - temporary data and WAL sidecars must be disjoint from the original;
    - failures before finalization preserve the original durable state; and
    - successful finalization installs the compacted snapshot and discards the
      stale original WAL rather than replaying it on reopen.
 *)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Logic.FunctionalExtensionality.
Import ListNotations.

Definition Key := nat.
Definition Value := nat.
Definition PathId := nat.

Inductive StoredValue : Type :=
| TermOnly : StoredValue
| TermValue : Value -> StoredValue.

Definition RefMap := Key -> option StoredValue.

Definition empty_map : RefMap := fun _ => None.

Definition lookup (map : RefMap) (key : Key) : option StoredValue :=
  map key.

Definition same_map (left right : RefMap) : Prop :=
  forall key, lookup left key = lookup right key.

Definition map_put_term (map : RefMap) (key : Key) : RefMap :=
  fun query => if Nat.eq_dec query key then Some TermOnly else map query.

Definition map_put_value
  (map : RefMap)
  (key : Key)
  (value : Value)
  : RefMap :=
  fun query =>
    if Nat.eq_dec query key then Some (TermValue value) else map query.

Definition map_remove (map : RefMap) (key : Key) : RefMap :=
  fun query => if Nat.eq_dec query key then None else map query.

Definition Snapshot := list (Key * StoredValue).

Fixpoint snapshot_lookup
  (snapshot : Snapshot)
  (key : Key)
  : option StoredValue :=
  match snapshot with
  | [] => None
  | (entry_key, stored) :: rest =>
      if Nat.eq_dec key entry_key
      then Some stored
      else snapshot_lookup rest key
  end.

Definition compacted_map (snapshot : Snapshot) : RefMap :=
  fun key => snapshot_lookup snapshot key.

Definition snapshot_exact (source : RefMap) (snapshot : Snapshot) : Prop :=
  forall key, snapshot_lookup snapshot key = lookup source key.

Fixpoint count_present (keys : list Key) (map : RefMap) : nat :=
  match keys with
  | [] => 0
  | key :: rest =>
      match lookup map key with
      | Some _ => S (count_present rest map)
      | None => count_present rest map
      end
  end.

Inductive WalOp : Type :=
| WalPutTerm : Key -> WalOp
| WalPutValue : Key -> Value -> WalOp
| WalDelete : Key -> WalOp.

Definition apply_wal_op (op : WalOp) (map : RefMap) : RefMap :=
  match op with
  | WalPutTerm key => map_put_term map key
  | WalPutValue key value => map_put_value map key value
  | WalDelete key => map_remove map key
  end.

Fixpoint replay_wal (wal : list WalOp) (map : RefMap) : RefMap :=
  match wal with
  | [] => map
  | op :: rest => replay_wal rest (apply_wal_op op map)
  end.

Record CompactionPaths : Type := mkCompactionPaths {
  original_data_path : PathId;
  original_wal_path : PathId;
  temp_data_path : PathId;
  temp_wal_path : PathId
}.

Definition paths_disjoint (paths : CompactionPaths) : Prop :=
  temp_data_path paths <> original_data_path paths /\
  temp_wal_path paths <> original_wal_path paths /\
  temp_data_path paths <> original_wal_path paths /\
  temp_wal_path paths <> original_data_path paths.

Inductive PathDecision : Type :=
| PermitCompaction
| RejectCompaction.

Definition validate_paths (paths : CompactionPaths) : PathDecision :=
  if Nat.eq_dec (temp_wal_path paths) (original_wal_path paths)
  then RejectCompaction
  else if Nat.eq_dec (temp_data_path paths) (original_data_path paths)
       then RejectCompaction
       else PermitCompaction.

Record DurableState : Type := mkDurableState {
  durable_data : RefMap;
  durable_wal : list WalOp;
  temp_data : option RefMap;
  temp_wal : list WalOp
}.

Inductive FailurePoint : Type :=
| FailBeforeCopy
| FailDuringCopy
| FailCheckpoint
| FailBeforeRename.

Definition fail_before_finalize
  (_point : FailurePoint)
  (state : DurableState)
  : DurableState :=
  state.

Definition finalize_compaction
  (compacted : RefMap)
  (_state : DurableState)
  : DurableState :=
  mkDurableState compacted [] None [].

Definition output_file_compaction
  (output : RefMap)
  (state : DurableState)
  : DurableState :=
  mkDurableState
    (durable_data state)
    (durable_wal state)
    (Some output)
    [].

Theorem map_put_term_lookup_same :
  forall map key,
    lookup (map_put_term map key) key = Some TermOnly.
Proof.
  intros map key.
  unfold lookup, map_put_term.
  destruct (Nat.eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem map_put_value_lookup_same :
  forall map key value,
    lookup (map_put_value map key value) key = Some (TermValue value).
Proof.
  intros map key value.
  unfold lookup, map_put_value.
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

Theorem exact_snapshot_compaction_identity :
  forall source snapshot,
    snapshot_exact source snapshot ->
    same_map (compacted_map snapshot) source.
Proof.
  intros source snapshot Hsnapshot key.
  unfold lookup, compacted_map.
  exact (Hsnapshot key).
Qed.

Theorem exact_snapshot_functional_identity :
  forall source snapshot,
    snapshot_exact source snapshot ->
    compacted_map snapshot = source.
Proof.
  intros source snapshot Hsnapshot.
  apply functional_extensionality.
  intros key.
  unfold compacted_map.
  exact (Hsnapshot key).
Qed.

Theorem compacted_snapshot_lookup_value :
  lookup (compacted_map [(0, TermValue 42)]) 0 = Some (TermValue 42).
Proof.
  reflexivity.
Qed.

Theorem compacted_snapshot_lookup_term_only :
  lookup (compacted_map [(0, TermOnly)]) 0 = Some TermOnly.
Proof.
  reflexivity.
Qed.

Definition singleton_value_map (value : Value) : RefMap :=
  fun key => if Nat.eq_dec key 0 then Some (TermValue value) else None.

Theorem term_count_verification_is_insufficient :
  count_present [0] (singleton_value_map 1) =
    count_present [0] (singleton_value_map 2) /\
  ~ same_map (singleton_value_map 1) (singleton_value_map 2).
Proof.
  split.
  - reflexivity.
  - unfold same_map, singleton_value_map, lookup.
    intros Hsame.
    specialize (Hsame 0).
    destruct (Nat.eq_dec 0 0) as [_ | Hneq].
    + discriminate Hsame.
    + exfalso. apply Hneq. reflexivity.
Qed.

Definition singleton_term_map : RefMap :=
  fun key => if Nat.eq_dec key 0 then Some TermOnly else None.

Theorem values_only_copy_drops_term_only_entries :
  ~ same_map (compacted_map []) singleton_term_map.
Proof.
  unfold same_map, compacted_map, singleton_term_map, lookup.
  intros Hsame.
  specialize (Hsame 0).
  simpl in Hsame.
  destruct (Nat.eq_dec 0 0) as [_ | Hneq].
  - discriminate Hsame.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem replay_empty_wal_identity :
  forall map,
    replay_wal [] map = map.
Proof.
  reflexivity.
Qed.

Theorem stale_wal_can_reintroduce_removed_term :
  lookup (replay_wal [WalPutValue 0 7] empty_map) 0 =
    Some (TermValue 7).
Proof.
  simpl.
  apply map_put_value_lookup_same.
Qed.

Theorem finalized_compaction_ignores_stale_wal :
  forall compacted old_state,
    durable_wal (finalize_compaction compacted old_state) = [].
Proof.
  reflexivity.
Qed.

Theorem reopen_after_finalize_is_compacted_snapshot :
  forall compacted old_state,
    replay_wal
      (durable_wal (finalize_compaction compacted old_state))
      (durable_data (finalize_compaction compacted old_state)) =
    compacted.
Proof.
  reflexivity.
Qed.

Theorem validate_paths_rejects_wal_sidecar_collision :
  forall paths,
    temp_wal_path paths = original_wal_path paths ->
    validate_paths paths = RejectCompaction.
Proof.
  intros paths Hcollision.
  unfold validate_paths.
  destruct (Nat.eq_dec (temp_wal_path paths) (original_wal_path paths))
    as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. exact Hcollision.
Qed.

Theorem validate_paths_rejects_data_path_collision :
  forall paths,
    temp_wal_path paths <> original_wal_path paths ->
    temp_data_path paths = original_data_path paths ->
    validate_paths paths = RejectCompaction.
Proof.
  intros paths Hwal Hdata.
  unfold validate_paths.
  destruct (Nat.eq_dec (temp_wal_path paths) (original_wal_path paths))
    as [Heq_wal | _].
  - exfalso. apply Hwal. exact Heq_wal.
  - destruct (Nat.eq_dec (temp_data_path paths) (original_data_path paths))
      as [_ | Hneq_data].
    + reflexivity.
    + exfalso. apply Hneq_data. exact Hdata.
Qed.

Theorem disjoint_paths_have_distinct_wal_sidecars :
  forall paths,
    paths_disjoint paths ->
    temp_wal_path paths <> original_wal_path paths.
Proof.
  intros paths [_ [Hwal _]].
  exact Hwal.
Qed.

Theorem disjoint_paths_have_distinct_data_files :
  forall paths,
    paths_disjoint paths ->
    temp_data_path paths <> original_data_path paths.
Proof.
  intros paths [Hdata _].
  exact Hdata.
Qed.

Theorem failure_before_finalize_preserves_durable_data :
  forall point state,
    durable_data (fail_before_finalize point state) = durable_data state.
Proof.
  reflexivity.
Qed.

Theorem failure_before_finalize_preserves_durable_wal :
  forall point state,
    durable_wal (fail_before_finalize point state) = durable_wal state.
Proof.
  reflexivity.
Qed.

Theorem finalize_installs_compacted_data :
  forall compacted state,
    durable_data (finalize_compaction compacted state) = compacted.
Proof.
  reflexivity.
Qed.

Theorem finalize_removes_temporary_state :
  forall compacted state,
    temp_data (finalize_compaction compacted state) = None /\
    temp_wal (finalize_compaction compacted state) = [].
Proof.
  split; reflexivity.
Qed.

Theorem output_file_compaction_preserves_original_data :
  forall output state,
    durable_data (output_file_compaction output state) = durable_data state.
Proof.
  reflexivity.
Qed.

Theorem output_file_compaction_preserves_original_wal :
  forall output state,
    durable_wal (output_file_compaction output state) = durable_wal state.
Proof.
  reflexivity.
Qed.

Theorem output_file_compaction_writes_snapshot_to_temp :
  forall output state,
    temp_data (output_file_compaction output state) = Some output.
Proof.
  reflexivity.
Qed.
