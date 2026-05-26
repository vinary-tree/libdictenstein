(** Lock-free counter merge atomicity model.

    The lock-free counter overlay stores nonnegative counters, while the
    persistent WAL stores merge deltas in the signed i64 domain.  This small
    model captures the checked boundary used by the Rust implementation:

    - overlay increments reject overflow without changing the overlay;
    - merge preflight checks every persistent + overlay sum before WAL append;
    - successful merge appends one batch record, applies the prepared map, and
      clears the overlay;
    - failed preflight or WAL append leaves persistent state and WAL unchanged.
 *)

Require Import Coq.Lists.List.
Require Import Coq.Arith.PeanoNat.
Import ListNotations.

Definition Key := nat.
Definition Value := nat.
Definition MaxCounter := 5.
Definition CounterMap := Key -> Value.

Definition zero_map : CounterMap := fun _ => 0.

Definition map_put (map : CounterMap) (key : Key) (value : Value)
  : CounterMap :=
  fun query => if Nat.eqb query key then value else map query.

Definition checked_add (current delta : Value) : option Value :=
  if (current + delta) <=? MaxCounter
  then Some (current + delta)
  else None.

Definition checked_overlay_increment
  (overlay : CounterMap)
  (key : Key)
  (delta : Value)
  : option CounterMap :=
  match checked_add (overlay key) delta with
  | Some result => Some (map_put overlay key result)
  | None => None
  end.

Fixpoint checked_apply_merge
  (entries : list (Key * Value))
  (persistent : CounterMap)
  : option CounterMap :=
  match entries with
  | [] => Some persistent
  | (key, delta) :: rest =>
      match checked_add (persistent key) delta with
      | Some result => checked_apply_merge rest (map_put persistent key result)
      | None => None
      end
  end.

Inductive WalResult : Type :=
| WalOk
| WalError.

Record LockFreeState : Type := {
  persistent_map : CounterMap;
  overlay_map : CounterMap;
  merge_wal : list (list (Key * Value))
}.

Definition run_checked_merge
  (state : LockFreeState)
  (entries : list (Key * Value))
  (wal_result : WalResult)
  : option LockFreeState :=
  match checked_apply_merge entries (persistent_map state) with
  | None => None
  | Some prepared =>
      match wal_result with
      | WalError => None
      | WalOk =>
          Some {|
            persistent_map := prepared;
            overlay_map := zero_map;
            merge_wal := merge_wal state ++ [entries]
          |}
      end
  end.

Definition persistent_after_merge_attempt
  (state : LockFreeState)
  (entries : list (Key * Value))
  (wal_result : WalResult)
  : CounterMap :=
  match run_checked_merge state entries wal_result with
  | Some next => persistent_map next
  | None => persistent_map state
  end.

Definition overlay_after_merge_attempt
  (state : LockFreeState)
  (entries : list (Key * Value))
  (wal_result : WalResult)
  : CounterMap :=
  match run_checked_merge state entries wal_result with
  | Some next => overlay_map next
  | None => overlay_map state
  end.

Definition wal_after_merge_attempt
  (state : LockFreeState)
  (entries : list (Key * Value))
  (wal_result : WalResult)
  : list (list (Key * Value)) :=
  match run_checked_merge state entries wal_result with
  | Some next => merge_wal next
  | None => merge_wal state
  end.

Theorem checked_overlay_increment_overflow_rejects :
  forall overlay key delta,
    checked_add (overlay key) delta = None ->
    checked_overlay_increment overlay key delta = None.
Proof.
  intros overlay key delta Hoverflow.
  unfold checked_overlay_increment.
  rewrite Hoverflow.
  reflexivity.
Qed.

Theorem checked_overlay_increment_success_sets_key :
  forall overlay key delta result next,
    checked_add (overlay key) delta = Some result ->
    checked_overlay_increment overlay key delta = Some next ->
    next key = result.
Proof.
  intros overlay key delta result next Hchecked Hnext.
  unfold checked_overlay_increment in Hnext.
  rewrite Hchecked in Hnext.
  inversion Hnext; subst.
  unfold map_put.
  rewrite Nat.eqb_refl.
  reflexivity.
Qed.

Theorem checked_overlay_increment_success_preserves_other_keys :
  forall overlay key other delta result next,
    Nat.eqb other key = false ->
    checked_add (overlay key) delta = Some result ->
    checked_overlay_increment overlay key delta = Some next ->
    next other = overlay other.
Proof.
  intros overlay key other delta result next Hneq Hchecked Hnext.
  unfold checked_overlay_increment in Hnext.
  rewrite Hchecked in Hnext.
  inversion Hnext; subst.
  unfold map_put.
  rewrite Hneq.
  reflexivity.
Qed.

Theorem checked_merge_overflow_rejects_commit :
  forall state entries,
    checked_apply_merge entries (persistent_map state) = None ->
    run_checked_merge state entries WalOk = None.
Proof.
  intros state entries Hoverflow.
  unfold run_checked_merge.
  rewrite Hoverflow.
  reflexivity.
Qed.

Theorem checked_merge_overflow_preserves_persistent :
  forall state entries query,
    checked_apply_merge entries (persistent_map state) = None ->
    persistent_after_merge_attempt state entries WalOk query =
    persistent_map state query.
Proof.
  intros state entries query Hoverflow.
  unfold persistent_after_merge_attempt.
  rewrite checked_merge_overflow_rejects_commit by exact Hoverflow.
  reflexivity.
Qed.

Theorem checked_merge_overflow_preserves_overlay :
  forall state entries query,
    checked_apply_merge entries (persistent_map state) = None ->
    overlay_after_merge_attempt state entries WalOk query =
    overlay_map state query.
Proof.
  intros state entries query Hoverflow.
  unfold overlay_after_merge_attempt.
  rewrite checked_merge_overflow_rejects_commit by exact Hoverflow.
  reflexivity.
Qed.

Theorem checked_merge_overflow_does_not_append_wal :
  forall state entries,
    checked_apply_merge entries (persistent_map state) = None ->
    wal_after_merge_attempt state entries WalOk = merge_wal state.
Proof.
  intros state entries Hoverflow.
  unfold wal_after_merge_attempt.
  rewrite checked_merge_overflow_rejects_commit by exact Hoverflow.
  reflexivity.
Qed.

Theorem checked_merge_wal_failure_preserves_persistent :
  forall state entries prepared query,
    checked_apply_merge entries (persistent_map state) = Some prepared ->
    persistent_after_merge_attempt state entries WalError query =
    persistent_map state query.
Proof.
  intros state entries prepared query Hchecked.
  unfold persistent_after_merge_attempt, run_checked_merge.
  rewrite Hchecked.
  reflexivity.
Qed.

Theorem checked_merge_wal_failure_does_not_append_wal :
  forall state entries prepared,
    checked_apply_merge entries (persistent_map state) = Some prepared ->
    wal_after_merge_attempt state entries WalError = merge_wal state.
Proof.
  intros state entries prepared Hchecked.
  unfold wal_after_merge_attempt, run_checked_merge.
  rewrite Hchecked.
  reflexivity.
Qed.

Theorem checked_merge_success_appends_one_batch :
  forall state entries prepared,
    checked_apply_merge entries (persistent_map state) = Some prepared ->
    wal_after_merge_attempt state entries WalOk =
    merge_wal state ++ [entries].
Proof.
  intros state entries prepared Hchecked.
  unfold wal_after_merge_attempt, run_checked_merge.
  rewrite Hchecked.
  reflexivity.
Qed.

Theorem checked_merge_success_applies_prepared_map :
  forall state entries prepared query,
    checked_apply_merge entries (persistent_map state) = Some prepared ->
    persistent_after_merge_attempt state entries WalOk query = prepared query.
Proof.
  intros state entries prepared query Hchecked.
  unfold persistent_after_merge_attempt, run_checked_merge.
  rewrite Hchecked.
  reflexivity.
Qed.

Theorem checked_merge_success_clears_overlay :
  forall state entries prepared query,
    checked_apply_merge entries (persistent_map state) = Some prepared ->
    overlay_after_merge_attempt state entries WalOk query = 0.
Proof.
  intros state entries prepared query Hchecked.
  unfold overlay_after_merge_attempt, run_checked_merge.
  rewrite Hchecked.
  unfold zero_map.
  reflexivity.
Qed.

Theorem rejected_merge_retry_has_no_duplicate_wal_growth :
  forall state entries,
    checked_apply_merge entries (persistent_map state) = None ->
    wal_after_merge_attempt state entries WalOk =
    wal_after_merge_attempt state entries WalError.
Proof.
  intros state entries Hoverflow.
  unfold wal_after_merge_attempt, run_checked_merge.
  rewrite Hoverflow.
  reflexivity.
Qed.
