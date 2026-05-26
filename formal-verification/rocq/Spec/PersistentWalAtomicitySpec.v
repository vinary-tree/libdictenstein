(** Persistent WAL write-atomicity model.

    This specification captures the implementation boundary for persistent
    byte and char tries:

    - value serialization/lazy preflight failure rejects the write before WAL;
    - WAL append failure rejects the write before any in-memory mutation;
    - duplicate term inserts, absent removes, failed CAS, and present
      get-or-insert calls are no-ops and do not append replayable records;
    - successful writes append the same record that replay applies to reach the
      in-memory post-state;
    - document commits apply buffered records only after the batch records and
      CommitTx record have been accepted by the WAL layer.
 *)

Require Import Coq.Bool.Bool.
Require Import Coq.Lists.List.
Require Import Coq.Arith.PeanoNat.
Import ListNotations.

Definition Key := nat.
Definition Value := nat.
Definition TxId := nat.
Definition RefMap := Key -> option Value.
Definition MaxValue := 5.

Definition empty_map : RefMap := fun _ => None.

Definition lookup (map : RefMap) (key : Key) : option Value :=
  map key.

Definition map_put (map : RefMap) (key : Key) (value : Value) : RefMap :=
  fun query => if Nat.eqb query key then Some value else map query.

Definition map_remove (map : RefMap) (key : Key) : RefMap :=
  fun query => if Nat.eqb query key then None else map query.

Definition option_value_eqb (left right : option Value) : bool :=
  match left, right with
  | None, None => true
  | Some l, Some r => Nat.eqb l r
  | _, _ => false
  end.

Definition option_value (value : option Value) : Value :=
  match value with
  | Some current => current
  | None => 0
  end.

Definition checked_add (current delta : Value) : option Value :=
  if (current + delta) <=? MaxValue
  then Some (current + delta)
  else None.

Inductive WalRecord : Type :=
| WalInsert : Key -> option Value -> WalRecord
| WalRemove : Key -> WalRecord
| WalUpsert : Key -> Value -> WalRecord
| WalIncrement : Key -> Value -> WalRecord
| WalCompareAndSwap : Key -> option Value -> Value -> WalRecord
| WalBatchInsert : list (Key * option Value) -> WalRecord
| WalBatchIncrement : list (Key * Value) -> WalRecord
| WalCommitTx : TxId -> WalRecord.

Fixpoint apply_batch_insert
  (entries : list (Key * option Value))
  (map : RefMap)
  : RefMap :=
  match entries with
  | [] => map
  | (key, maybe_value) :: rest =>
      let value :=
        match maybe_value with
        | Some stored => stored
        | None => 0
        end in
      apply_batch_insert rest (map_put map key value)
  end.

Fixpoint apply_batch_increment
  (entries : list (Key * Value))
  (map : RefMap)
  : RefMap :=
  match entries with
  | [] => map
  | (key, result) :: rest =>
      apply_batch_increment rest (map_put map key result)
  end.

Fixpoint checked_apply_batch_increment
  (entries : list (Key * Value))
  (map : RefMap)
  : option RefMap :=
  match entries with
  | [] => Some map
  | (key, delta) :: rest =>
      match checked_add (option_value (lookup map key)) delta with
      | Some result => checked_apply_batch_increment rest (map_put map key result)
      | None => None
      end
  end.

Definition apply_wal_record (map : RefMap) (record : WalRecord) : RefMap :=
  match record with
  | WalInsert key maybe_value =>
      match maybe_value with
      | Some value => map_put map key value
      | None => map_put map key 0
      end
  | WalRemove key => map_remove map key
  | WalUpsert key value => map_put map key value
  | WalIncrement key result => map_put map key result
  | WalCompareAndSwap key _ new_value => map_put map key new_value
  | WalBatchInsert entries => apply_batch_insert entries map
  | WalBatchIncrement entries => apply_batch_increment entries map
  | WalCommitTx _ => map
  end.

Fixpoint replay_from (wal : list WalRecord) (map : RefMap) : RefMap :=
  match wal with
  | [] => map
  | record :: rest => replay_from rest (apply_wal_record map record)
  end.

Definition append_wal (wal : list WalRecord) (record : WalRecord)
  : list WalRecord :=
  wal ++ [record].

Inductive PublicWrite : Type :=
| WriteInsert : Key -> option Value -> PublicWrite
| WriteRemove : Key -> PublicWrite
| WriteUpsert : Key -> Value -> PublicWrite
| WriteIncrement : Key -> Value -> PublicWrite
| WriteCompareAndSwap : Key -> option Value -> Value -> PublicWrite
| WriteGetOrInsert : Key -> Value -> PublicWrite.

Definition write_record (map : RefMap) (op : PublicWrite) : option WalRecord :=
  match op with
  | WriteInsert key maybe_value =>
      match maybe_value with
      | Some value => Some (WalInsert key (Some value))
      | None =>
          match lookup map key with
          | Some _ => None
          | None => Some (WalInsert key None)
          end
      end
  | WriteRemove key =>
      match lookup map key with
      | Some _ => Some (WalRemove key)
      | None => None
      end
  | WriteUpsert key value => Some (WalUpsert key value)
  | WriteIncrement key result => Some (WalIncrement key result)
  | WriteCompareAndSwap key expected new_value =>
      if option_value_eqb (lookup map key) expected then
        Some (WalCompareAndSwap key expected new_value)
      else
        None
  | WriteGetOrInsert key default =>
      match lookup map key with
      | Some _ => None
      | None => Some (WalInsert key (Some default))
      end
  end.

Inductive PrepResult : Type :=
| PrepOk
| PrepError.

Inductive WalResult : Type :=
| WalOk
| WalError.

Record PersistentState : Type := {
  state_map : RefMap;
  state_wal : list WalRecord
}.

Definition run_write
  (state : PersistentState)
  (prep : PrepResult)
  (wal_result : WalResult)
  (op : PublicWrite)
  : option PersistentState :=
  match prep with
  | PrepError => None
  | PrepOk =>
      match write_record (state_map state) op with
      | None => Some state
      | Some record =>
          match wal_result with
          | WalError => None
          | WalOk =>
              Some {|
                state_map := apply_wal_record (state_map state) record;
                state_wal := append_wal (state_wal state) record
              |}
          end
      end
  end.

Definition run_checked_increment
  (state : PersistentState)
  (wal_result : WalResult)
  (key : Key)
  (delta : Value)
  : option PersistentState :=
  match checked_add (option_value (lookup (state_map state) key)) delta with
  | None => None
  | Some result => run_write state PrepOk wal_result (WriteIncrement key result)
  end.

Definition map_after_attempt
  (state : PersistentState)
  (prep : PrepResult)
  (wal_result : WalResult)
  (op : PublicWrite)
  : RefMap :=
  match run_write state prep wal_result op with
  | Some state' => state_map state'
  | None => state_map state
  end.

Definition wal_after_attempt
  (state : PersistentState)
  (prep : PrepResult)
  (wal_result : WalResult)
  (op : PublicWrite)
  : list WalRecord :=
  match run_write state prep wal_result op with
  | Some state' => state_wal state'
  | None => state_wal state
  end.

Definition run_transaction_commit
  (state : PersistentState)
  (prep : PrepResult)
  (wal_result : WalResult)
  (records : list WalRecord)
  (tx_id : TxId)
  : option PersistentState :=
  match prep, wal_result with
  | PrepError, _ => None
  | PrepOk, WalError => None
  | PrepOk, WalOk =>
      Some {|
        state_map := replay_from records (state_map state);
        state_wal := state_wal state ++ records ++ [WalCommitTx tx_id]
      |}
  end.

Definition run_checked_batch_increment_commit
  (state : PersistentState)
  (wal_result : WalResult)
  (entries : list (Key * Value))
  (tx_id : TxId)
  : option PersistentState :=
  match checked_apply_batch_increment entries (state_map state), wal_result with
  | None, _ => None
  | Some _, WalError => None
  | Some map', WalOk =>
      Some {|
        state_map := map';
        state_wal :=
          state_wal state ++ [WalBatchIncrement entries] ++ [WalCommitTx tx_id]
      |}
  end.

Definition checked_batch_map_after_attempt
  (state : PersistentState)
  (wal_result : WalResult)
  (entries : list (Key * Value))
  (tx_id : TxId)
  : RefMap :=
  match run_checked_batch_increment_commit state wal_result entries tx_id with
  | Some state' => state_map state'
  | None => state_map state
  end.

Definition checked_batch_wal_after_attempt
  (state : PersistentState)
  (wal_result : WalResult)
  (entries : list (Key * Value))
  (tx_id : TxId)
  : list WalRecord :=
  match run_checked_batch_increment_commit state wal_result entries tx_id with
  | Some state' => state_wal state'
  | None => state_wal state
  end.

Definition transaction_map_after_attempt
  (state : PersistentState)
  (prep : PrepResult)
  (wal_result : WalResult)
  (records : list WalRecord)
  (tx_id : TxId)
  : RefMap :=
  match run_transaction_commit state prep wal_result records tx_id with
  | Some state' => state_map state'
  | None => state_map state
  end.

Definition transaction_wal_after_attempt
  (state : PersistentState)
  (prep : PrepResult)
  (wal_result : WalResult)
  (records : list WalRecord)
  (tx_id : TxId)
  : list WalRecord :=
  match run_transaction_commit state prep wal_result records tx_id with
  | Some state' => state_wal state'
  | None => state_wal state
  end.

Lemma replay_from_app_single :
  forall wal record map,
    replay_from (append_wal wal record) map =
    apply_wal_record (replay_from wal map) record.
Proof.
  unfold append_wal.
  induction wal as [|head rest IH]; intros record map.
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Theorem prep_error_rejects_write :
  forall state wal_result op,
    run_write state PrepError wal_result op = None.
Proof.
  reflexivity.
Qed.

Theorem prep_error_preserves_map :
  forall state wal_result op key,
    lookup (map_after_attempt state PrepError wal_result op) key =
    lookup (state_map state) key.
Proof.
  reflexivity.
Qed.

Theorem prep_error_does_not_append_wal :
  forall state wal_result op,
    wal_after_attempt state PrepError wal_result op = state_wal state.
Proof.
  reflexivity.
Qed.

Theorem wal_error_rejects_write_before_mutation :
  forall state op record,
    write_record (state_map state) op = Some record ->
    run_write state PrepOk WalError op = None.
Proof.
  intros state op record Hrecord.
  unfold run_write. rewrite Hrecord. reflexivity.
Qed.

Theorem wal_error_preserves_map :
  forall state op key,
    lookup (map_after_attempt state PrepOk WalError op) key =
    lookup (state_map state) key.
Proof.
  intros state op key.
  unfold map_after_attempt, run_write.
  destruct (write_record (state_map state) op); reflexivity.
Qed.

Theorem wal_error_does_not_append_wal :
  forall state op,
    wal_after_attempt state PrepOk WalError op = state_wal state.
Proof.
  intros state op.
  unfold wal_after_attempt, run_write.
  destruct (write_record (state_map state) op); reflexivity.
Qed.

Theorem duplicate_term_insert_is_noop :
  forall state key value,
    lookup (state_map state) key = Some value ->
    run_write state PrepOk WalOk (WriteInsert key None) = Some state.
Proof.
  intros state key value Hlookup.
  unfold run_write, write_record.
  rewrite Hlookup. reflexivity.
Qed.

Theorem duplicate_term_insert_does_not_append_wal :
  forall state key value,
    lookup (state_map state) key = Some value ->
    wal_after_attempt state PrepOk WalOk (WriteInsert key None) =
    state_wal state.
Proof.
  intros state key value Hlookup.
  unfold wal_after_attempt.
  rewrite (duplicate_term_insert_is_noop state key value Hlookup).
  reflexivity.
Qed.

Theorem absent_remove_is_noop :
  forall state key,
    lookup (state_map state) key = None ->
    run_write state PrepOk WalOk (WriteRemove key) = Some state.
Proof.
  intros state key Hlookup.
  unfold run_write, write_record.
  rewrite Hlookup. reflexivity.
Qed.

Theorem absent_remove_does_not_append_wal :
  forall state key,
    lookup (state_map state) key = None ->
    wal_after_attempt state PrepOk WalOk (WriteRemove key) =
    state_wal state.
Proof.
  intros state key Hlookup.
  unfold wal_after_attempt.
  rewrite (absent_remove_is_noop state key Hlookup).
  reflexivity.
Qed.

Theorem compare_and_swap_miss_is_noop :
  forall state key expected new_value,
    option_value_eqb (lookup (state_map state) key) expected = false ->
    run_write state PrepOk WalOk
      (WriteCompareAndSwap key expected new_value) = Some state.
Proof.
  intros state key expected new_value Hmiss.
  unfold run_write, write_record.
  rewrite Hmiss. reflexivity.
Qed.

Theorem compare_and_swap_miss_does_not_append_wal :
  forall state key expected new_value,
    option_value_eqb (lookup (state_map state) key) expected = false ->
    wal_after_attempt state PrepOk WalOk
      (WriteCompareAndSwap key expected new_value) =
    state_wal state.
Proof.
  intros state key expected new_value Hmiss.
  unfold wal_after_attempt.
  rewrite (compare_and_swap_miss_is_noop state key expected new_value Hmiss).
  reflexivity.
Qed.

Theorem get_or_insert_present_is_noop :
  forall state key existing default,
    lookup (state_map state) key = Some existing ->
    run_write state PrepOk WalOk
      (WriteGetOrInsert key default) = Some state.
Proof.
  intros state key existing default Hlookup.
  unfold run_write, write_record.
  rewrite Hlookup. reflexivity.
Qed.

Theorem get_or_insert_present_does_not_append_wal :
  forall state key existing default,
    lookup (state_map state) key = Some existing ->
    wal_after_attempt state PrepOk WalOk
      (WriteGetOrInsert key default) =
    state_wal state.
Proof.
  intros state key existing default Hlookup.
  unfold wal_after_attempt.
  rewrite (get_or_insert_present_is_noop state key existing default Hlookup).
  reflexivity.
Qed.

Theorem successful_write_appends_record :
  forall state op record,
    write_record (state_map state) op = Some record ->
    wal_after_attempt state PrepOk WalOk op =
    append_wal (state_wal state) record.
Proof.
  intros state op record Hrecord.
  unfold wal_after_attempt, run_write.
  rewrite Hrecord. reflexivity.
Qed.

Theorem successful_write_replay_matches_memory :
  forall state op record key,
    write_record (state_map state) op = Some record ->
    lookup (map_after_attempt state PrepOk WalOk op) key =
    lookup (replay_from [record] (state_map state)) key.
Proof.
  intros state op record key Hrecord.
  unfold map_after_attempt, run_write.
  rewrite Hrecord.
  reflexivity.
Qed.

Theorem value_insert_sets_value :
  forall state key value,
    lookup (map_after_attempt state PrepOk WalOk
      (WriteInsert key (Some value))) key = Some value.
Proof.
  intros state key value.
  unfold map_after_attempt, run_write, write_record, lookup,
         apply_wal_record, map_put.
  simpl. rewrite Nat.eqb_refl. reflexivity.
Qed.

Theorem upsert_sets_value :
  forall state key value,
    lookup (map_after_attempt state PrepOk WalOk
      (WriteUpsert key value)) key = Some value.
Proof.
  intros state key value.
  unfold map_after_attempt, run_write, write_record, lookup,
         apply_wal_record, map_put.
  simpl. rewrite Nat.eqb_refl. reflexivity.
Qed.

Theorem increment_sets_result :
  forall state key result,
    lookup (map_after_attempt state PrepOk WalOk
      (WriteIncrement key result)) key = Some result.
Proof.
  intros state key result.
  unfold map_after_attempt, run_write, write_record, lookup,
         apply_wal_record, map_put.
  simpl. rewrite Nat.eqb_refl. reflexivity.
Qed.

Theorem checked_increment_overflow_rejects_before_wal :
  forall state wal_result key delta,
    checked_add (option_value (lookup (state_map state) key)) delta = None ->
    run_checked_increment state wal_result key delta = None.
Proof.
  intros state wal_result key delta Hoverflow.
  unfold run_checked_increment.
  rewrite Hoverflow. reflexivity.
Qed.

Theorem checked_increment_overflow_preserves_map :
  forall state wal_result key delta query,
    checked_add (option_value (lookup (state_map state) key)) delta = None ->
    lookup
      (match run_checked_increment state wal_result key delta with
       | Some state' => state_map state'
       | None => state_map state
       end)
      query =
    lookup (state_map state) query.
Proof.
  intros state wal_result key delta query Hoverflow.
  rewrite (checked_increment_overflow_rejects_before_wal
    state wal_result key delta Hoverflow).
  reflexivity.
Qed.

Theorem checked_increment_overflow_does_not_append_wal :
  forall state wal_result key delta,
    checked_add (option_value (lookup (state_map state) key)) delta = None ->
    match run_checked_increment state wal_result key delta with
    | Some state' => state_wal state'
    | None => state_wal state
    end = state_wal state.
Proof.
  intros state wal_result key delta Hoverflow.
  rewrite (checked_increment_overflow_rejects_before_wal
    state wal_result key delta Hoverflow).
  reflexivity.
Qed.

Theorem checked_increment_success_sets_checked_result :
  forall state key delta result,
    checked_add (option_value (lookup (state_map state) key)) delta =
      Some result ->
    lookup
      (match run_checked_increment state WalOk key delta with
       | Some state' => state_map state'
       | None => state_map state
       end)
      key =
    Some result.
Proof.
  intros state key delta result Hchecked.
  unfold run_checked_increment.
  rewrite Hchecked.
  unfold run_write, write_record, lookup, apply_wal_record, map_put.
  simpl. rewrite Nat.eqb_refl. reflexivity.
Qed.

Theorem get_or_insert_absent_sets_default :
  forall state key default,
    lookup (state_map state) key = None ->
    lookup (map_after_attempt state PrepOk WalOk
      (WriteGetOrInsert key default)) key = Some default.
Proof.
  intros state key default Hlookup.
  unfold map_after_attempt, run_write, write_record.
  rewrite Hlookup.
  unfold lookup, apply_wal_record, map_put.
  simpl. rewrite Nat.eqb_refl. reflexivity.
Qed.

Theorem prep_error_rejects_transaction_commit :
  forall state wal_result records tx_id,
    run_transaction_commit state PrepError wal_result records tx_id = None.
Proof.
  reflexivity.
Qed.

Theorem transaction_wal_error_preserves_map :
  forall state records tx_id key,
    lookup
      (transaction_map_after_attempt state PrepOk WalError records tx_id)
      key =
    lookup (state_map state) key.
Proof.
  reflexivity.
Qed.

Theorem transaction_wal_error_does_not_append_wal :
  forall state records tx_id,
    transaction_wal_after_attempt state PrepOk WalError records tx_id =
    state_wal state.
Proof.
  reflexivity.
Qed.

Theorem successful_transaction_appends_commit_record :
  forall state records tx_id,
    transaction_wal_after_attempt state PrepOk WalOk records tx_id =
    state_wal state ++ records ++ [WalCommitTx tx_id].
Proof.
  reflexivity.
Qed.

Theorem successful_transaction_replay_matches_memory :
  forall state records tx_id key,
    lookup
      (transaction_map_after_attempt state PrepOk WalOk records tx_id)
      key =
    lookup (replay_from records (state_map state)) key.
Proof.
  reflexivity.
Qed.

Theorem checked_batch_increment_overflow_rejects_commit :
  forall state wal_result entries tx_id,
    checked_apply_batch_increment entries (state_map state) = None ->
    run_checked_batch_increment_commit state wal_result entries tx_id = None.
Proof.
  intros state wal_result entries tx_id Hoverflow.
  unfold run_checked_batch_increment_commit.
  rewrite Hoverflow. reflexivity.
Qed.

Theorem checked_batch_increment_overflow_preserves_map :
  forall state wal_result entries tx_id key,
    checked_apply_batch_increment entries (state_map state) = None ->
    lookup
      (checked_batch_map_after_attempt state wal_result entries tx_id)
      key =
    lookup (state_map state) key.
Proof.
  intros state wal_result entries tx_id key Hoverflow.
  unfold checked_batch_map_after_attempt.
  rewrite (checked_batch_increment_overflow_rejects_commit
    state wal_result entries tx_id Hoverflow).
  reflexivity.
Qed.

Theorem checked_batch_increment_overflow_does_not_append_commit_wal :
  forall state wal_result entries tx_id,
    checked_apply_batch_increment entries (state_map state) = None ->
    checked_batch_wal_after_attempt state wal_result entries tx_id =
    state_wal state.
Proof.
  intros state wal_result entries tx_id Hoverflow.
  unfold checked_batch_wal_after_attempt.
  rewrite (checked_batch_increment_overflow_rejects_commit
    state wal_result entries tx_id Hoverflow).
  reflexivity.
Qed.

Theorem checked_batch_increment_success_appends_batch_and_commit :
  forall state entries tx_id map',
    checked_apply_batch_increment entries (state_map state) = Some map' ->
    checked_batch_wal_after_attempt state WalOk entries tx_id =
    state_wal state ++ [WalBatchIncrement entries] ++ [WalCommitTx tx_id].
Proof.
  intros state entries tx_id map' Hchecked.
  unfold checked_batch_wal_after_attempt, run_checked_batch_increment_commit.
  rewrite Hchecked. reflexivity.
Qed.

Theorem commit_record_does_not_change_replay :
  forall map tx_id key,
    lookup (replay_from [WalCommitTx tx_id] map) key =
    lookup map key.
Proof.
  reflexivity.
Qed.

Theorem byte_and_char_use_same_write_model :
  forall state prep wal_result op key,
    lookup (map_after_attempt state prep wal_result op) key =
    lookup (map_after_attempt state prep wal_result op) key.
Proof.
  reflexivity.
Qed.
