(** Persistent lazy-mutation atomicity model.

    This specification models the write boundary for disk-backed persistent
    tries that may need to lazy-load an existing child before mutating.  The
    intended implementation rule is:

    - lazy-load failure rejects the public mutation before appending WAL;
    - no-op mutations do not append replayable records;
    - successful mutations append the record and apply the same map step;
    - WAL replay of the appended record matches the in-memory post-state.
 *)

Require Import Coq.Bool.Bool.
Require Import Coq.Lists.List.
Require Import Coq.Arith.PeanoNat.
Import ListNotations.

Definition Key := nat.
Definition Value := nat.
Definition RefMap := Key -> option Value.

Definition empty_map : RefMap := fun _ => None.

Definition lookup (map : RefMap) (key : Key) : option Value :=
  map key.

Definition map_put (map : RefMap) (key : Key) (value : Value) : RefMap :=
  fun query => if Nat.eqb query key then Some value else map query.

Definition map_remove (map : RefMap) (key : Key) : RefMap :=
  fun query => if Nat.eqb query key then None else map query.

Inductive Mutation : Type :=
| InsertTerm : Key -> Mutation
| InsertValue : Key -> Value -> Mutation
| RemoveTerm : Key -> Mutation.

Definition mutation_key (op : Mutation) : Key :=
  match op with
  | InsertTerm key => key
  | InsertValue key _ => key
  | RemoveTerm key => key
  end.

Definition apply_mutation (map : RefMap) (op : Mutation) : RefMap :=
  match op with
  | InsertTerm key => map_put map key 0
  | InsertValue key value => map_put map key value
  | RemoveTerm key => map_remove map key
  end.

Definition mutation_needs_wal (map : RefMap) (op : Mutation) : bool :=
  match op with
  | InsertTerm key =>
      match lookup map key with
      | Some _ => false
      | None => true
      end
  | InsertValue _ _ => true
  | RemoveTerm key =>
      match lookup map key with
      | Some _ => true
      | None => false
      end
  end.

Inductive LazyResult : Type :=
| LazyLoaded
| LazyMissing
| LazyError.

Record PersistentState : Type := {
  state_map : RefMap;
  state_wal : list Mutation
}.

Definition append_wal (wal : list Mutation) (op : Mutation) : list Mutation :=
  wal ++ [op].

Fixpoint replay_from (wal : list Mutation) (map : RefMap) : RefMap :=
  match wal with
  | [] => map
  | op :: rest => replay_from rest (apply_mutation map op)
  end.

Definition run_lazy_mutation
  (state : PersistentState)
  (lazy : LazyResult)
  (op : Mutation)
  : option PersistentState :=
  match lazy with
  | LazyError => None
  | LazyLoaded | LazyMissing =>
      if mutation_needs_wal (state_map state) op then
        Some {|
          state_map := apply_mutation (state_map state) op;
          state_wal := append_wal (state_wal state) op
        |}
      else
        Some state
  end.

Definition map_after_attempt
  (state : PersistentState)
  (lazy : LazyResult)
  (op : Mutation)
  : RefMap :=
  match run_lazy_mutation state lazy op with
  | Some state' => state_map state'
  | None => state_map state
  end.

Definition wal_after_attempt
  (state : PersistentState)
  (lazy : LazyResult)
  (op : Mutation)
  : list Mutation :=
  match run_lazy_mutation state lazy op with
  | Some state' => state_wal state'
  | None => state_wal state
  end.

Lemma replay_from_app_single :
  forall wal op map,
    replay_from (append_wal wal op) map =
    apply_mutation (replay_from wal map) op.
Proof.
  unfold append_wal.
  induction wal as [|head rest IH]; intros op map.
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Theorem lazy_error_rejects_mutation :
  forall state op,
    run_lazy_mutation state LazyError op = None.
Proof.
  reflexivity.
Qed.

Theorem lazy_error_preserves_map :
  forall state op key,
    lookup (map_after_attempt state LazyError op) key =
    lookup (state_map state) key.
Proof.
  reflexivity.
Qed.

Theorem lazy_error_does_not_append_wal :
  forall state op,
    wal_after_attempt state LazyError op = state_wal state.
Proof.
  reflexivity.
Qed.

Theorem duplicate_term_insert_is_noop :
  forall state key value,
    lookup (state_map state) key = Some value ->
    run_lazy_mutation state LazyLoaded (InsertTerm key) = Some state.
Proof.
  intros state key value Hlookup.
  unfold run_lazy_mutation, mutation_needs_wal.
  rewrite Hlookup. reflexivity.
Qed.

Theorem duplicate_term_insert_does_not_append_wal :
  forall state key value,
    lookup (state_map state) key = Some value ->
    wal_after_attempt state LazyLoaded (InsertTerm key) = state_wal state.
Proof.
  intros state key value Hlookup.
  unfold wal_after_attempt.
  rewrite (duplicate_term_insert_is_noop state key value Hlookup).
  reflexivity.
Qed.

Theorem absent_remove_is_noop :
  forall state key,
    lookup (state_map state) key = None ->
    run_lazy_mutation state LazyLoaded (RemoveTerm key) = Some state.
Proof.
  intros state key Hlookup.
  unfold run_lazy_mutation, mutation_needs_wal.
  rewrite Hlookup. reflexivity.
Qed.

Theorem absent_remove_does_not_append_wal :
  forall state key,
    lookup (state_map state) key = None ->
    wal_after_attempt state LazyLoaded (RemoveTerm key) = state_wal state.
Proof.
  intros state key Hlookup.
  unfold wal_after_attempt.
  rewrite (absent_remove_is_noop state key Hlookup).
  reflexivity.
Qed.

Theorem successful_term_insert_appends_wal :
  forall state key,
    lookup (state_map state) key = None ->
    wal_after_attempt state LazyLoaded (InsertTerm key) =
    append_wal (state_wal state) (InsertTerm key).
Proof.
  intros state key Hlookup.
  unfold wal_after_attempt, run_lazy_mutation, mutation_needs_wal.
  rewrite Hlookup. reflexivity.
Qed.

Theorem successful_term_insert_sets_membership :
  forall state key,
    lookup (state_map state) key = None ->
    lookup (map_after_attempt state LazyLoaded (InsertTerm key)) key = Some 0.
Proof.
  intros state key Hlookup.
  unfold map_after_attempt, run_lazy_mutation, mutation_needs_wal.
  rewrite Hlookup.
  unfold lookup, apply_mutation, map_put.
  simpl. rewrite Nat.eqb_refl. reflexivity.
Qed.

Theorem value_insert_appends_even_when_present :
  forall state key old value,
    lookup (state_map state) key = Some old ->
    wal_after_attempt state LazyLoaded (InsertValue key value) =
    append_wal (state_wal state) (InsertValue key value).
Proof.
  reflexivity.
Qed.

Theorem value_insert_sets_value :
  forall state key value lazy,
    lazy <> LazyError ->
    lookup (map_after_attempt state lazy (InsertValue key value)) key = Some value.
Proof.
  intros state key value lazy Hnot_error.
  destruct lazy; try contradiction;
    unfold map_after_attempt, run_lazy_mutation, mutation_needs_wal,
           lookup, apply_mutation, map_put;
    simpl; rewrite Nat.eqb_refl; reflexivity.
Qed.

Theorem successful_remove_appends_wal :
  forall state key value,
    lookup (state_map state) key = Some value ->
    wal_after_attempt state LazyLoaded (RemoveTerm key) =
    append_wal (state_wal state) (RemoveTerm key).
Proof.
  intros state key value Hlookup.
  unfold wal_after_attempt, run_lazy_mutation, mutation_needs_wal.
  rewrite Hlookup. reflexivity.
Qed.

Theorem successful_remove_deletes_key :
  forall state key value,
    lookup (state_map state) key = Some value ->
    lookup (map_after_attempt state LazyLoaded (RemoveTerm key)) key = None.
Proof.
  intros state key value Hlookup.
  unfold map_after_attempt, run_lazy_mutation, mutation_needs_wal.
  rewrite Hlookup.
  unfold lookup, apply_mutation, map_remove.
  simpl. rewrite Nat.eqb_refl. reflexivity.
Qed.

Theorem lazy_missing_insert_can_create_path :
  forall state key,
    lookup (state_map state) key = None ->
    lookup (map_after_attempt state LazyMissing (InsertTerm key)) key = Some 0.
Proof.
  intros state key Hlookup.
  unfold map_after_attempt, run_lazy_mutation, mutation_needs_wal.
  rewrite Hlookup.
  unfold lookup, apply_mutation, map_put.
  simpl. rewrite Nat.eqb_refl. reflexivity.
Qed.

Theorem lazy_missing_remove_absent_is_noop :
  forall state key,
    lookup (state_map state) key = None ->
    run_lazy_mutation state LazyMissing (RemoveTerm key) = Some state.
Proof.
  intros state key Hlookup.
  unfold run_lazy_mutation, mutation_needs_wal.
  rewrite Hlookup. reflexivity.
Qed.

Theorem appended_replay_matches_applied_mutation :
  forall wal op map key,
    lookup (replay_from (append_wal wal op) map) key =
    lookup (apply_mutation (replay_from wal map) op) key.
Proof.
  intros wal op map key.
  rewrite replay_from_app_single.
  reflexivity.
Qed.

Theorem successful_mutation_replay_matches_memory :
  forall state lazy op state' base key,
    lazy <> LazyError ->
    mutation_needs_wal (state_map state) op = true ->
    replay_from (state_wal state) base = state_map state ->
    run_lazy_mutation state lazy op = Some state' ->
    lookup (replay_from (state_wal state') base) key =
    lookup (state_map state') key.
Proof.
  intros state lazy op state' base key Hnot_error Hneeds Hreplay Hrun.
  destruct lazy; try contradiction;
    unfold run_lazy_mutation in Hrun;
    rewrite Hneeds in Hrun;
    inversion Hrun; subst; simpl.
  - rewrite replay_from_app_single. rewrite Hreplay. reflexivity.
  - rewrite replay_from_app_single. rewrite Hreplay. reflexivity.
Qed.

Theorem no_wal_needed_preserves_replay_correspondence :
  forall state lazy op state' base key,
    lazy <> LazyError ->
    mutation_needs_wal (state_map state) op = false ->
    replay_from (state_wal state) base = state_map state ->
    run_lazy_mutation state lazy op = Some state' ->
    lookup (replay_from (state_wal state') base) key =
    lookup (state_map state') key.
Proof.
  intros state lazy op state' base key Hnot_error Hneeds Hreplay Hrun.
  destruct lazy; try contradiction;
    unfold run_lazy_mutation in Hrun;
    rewrite Hneeds in Hrun;
    inversion Hrun; subst; simpl;
    rewrite Hreplay; reflexivity.
Qed.

Inductive Backend : Type :=
| ByteBackend
| CharBackend.

Definition backend_run
  (_backend : Backend)
  (state : PersistentState)
  (lazy : LazyResult)
  (op : Mutation)
  : option PersistentState :=
  run_lazy_mutation state lazy op.

Theorem byte_char_lazy_mutation_use_same_model :
  forall state lazy op,
    backend_run ByteBackend state lazy op =
    backend_run CharBackend state lazy op.
Proof.
  reflexivity.
Qed.
