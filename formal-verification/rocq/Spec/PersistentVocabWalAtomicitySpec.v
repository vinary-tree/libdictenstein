(** Persistent vocabulary WAL atomicity and bijection model.

    This specification captures the proof boundary for
    [PersistentVocabARTrie] public mutation:

    - a new term/index assignment mutates only after the WAL accepts the
      corresponding record;
    - WAL rejection preserves the visible vocabulary state and allocator;
    - duplicate assignment of the same term/index is a no-op;
    - term reindexing and index collision are rejected;
    - batch insertion assigns indexes by first occurrence; and
    - [contains_index] is exact with respect to the reverse map, not merely an
      allocator-range check.
 *)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Logic.FunctionalExtensionality.
Import ListNotations.

Definition Term := nat.
Definition Index := nat.
Definition Forward := Term -> option Index.
Definition Reverse := Index -> option Term.

Definition empty_forward : Forward := fun _ => None.
Definition empty_reverse : Reverse := fun _ => None.

Definition lookup_term (forward : Forward) (term : Term) : option Index :=
  forward term.

Definition lookup_index (reverse : Reverse) (index : Index) : option Term :=
  reverse index.

Definition put_forward
  (forward : Forward)
  (term : Term)
  (index : Index)
  : Forward :=
  fun query => if Nat.eq_dec query term then Some index else forward query.

Definition put_reverse
  (reverse : Reverse)
  (index : Index)
  (term : Term)
  : Reverse :=
  fun query => if Nat.eq_dec query index then Some term else reverse query.

Inductive VocabWalRecord : Type :=
| VocabWalInsert : Term -> Index -> VocabWalRecord
| VocabWalBatchInsert : list (Term * Index) -> VocabWalRecord.

Record VocabState : Type := mkVocabState {
  vocab_forward : Forward;
  vocab_reverse : Reverse;
  vocab_wal : list VocabWalRecord;
  vocab_next_index : Index
}.

Definition empty_state : VocabState :=
  mkVocabState empty_forward empty_reverse [] 0.

Definition append_wal
  (wal : list VocabWalRecord)
  (record : VocabWalRecord)
  : list VocabWalRecord :=
  wal ++ [record].

Definition assign_no_wal
  (state : VocabState)
  (term : Term)
  (index : Index)
  : VocabState :=
  mkVocabState
    (put_forward (vocab_forward state) term index)
    (put_reverse (vocab_reverse state) index term)
    (vocab_wal state)
    (Nat.max (S index) (vocab_next_index state)).

Definition assign_with_wal
  (state : VocabState)
  (term : Term)
  (index : Index)
  : VocabState :=
  let assigned := assign_no_wal state term index in
  mkVocabState
    (vocab_forward assigned)
    (vocab_reverse assigned)
    (append_wal (vocab_wal state) (VocabWalInsert term index))
    (vocab_next_index assigned).

Inductive WalOutcome : Type :=
| WalAccepted
| WalRejected.

Inductive InsertDecision : Type :=
| InsertOk : VocabState -> InsertDecision
| InsertRejected.

Definition insert_with_index
  (state : VocabState)
  (wal_outcome : WalOutcome)
  (term : Term)
  (index : Index)
  : InsertDecision :=
  match lookup_term (vocab_forward state) term with
  | Some existing =>
      if Nat.eq_dec existing index then InsertOk state else InsertRejected
  | None =>
      match lookup_index (vocab_reverse state) index with
      | Some _ => InsertRejected
      | None =>
          match wal_outcome with
          | WalRejected => InsertRejected
          | WalAccepted => InsertOk (assign_with_wal state term index)
          end
      end
  end.

Definition state_after_insert_attempt
  (state : VocabState)
  (wal_outcome : WalOutcome)
  (term : Term)
  (index : Index)
  : VocabState :=
  match insert_with_index state wal_outcome term index with
  | InsertOk state' => state'
  | InsertRejected => state
  end.

Definition contains_index (state : VocabState) (index : Index) : bool :=
  match lookup_index (vocab_reverse state) index with
  | Some _ => true
  | None => false
  end.

Definition range_contains_index (state : VocabState) (index : Index) : bool :=
  Nat.ltb index (vocab_next_index state).

Definition forward_reverse_exact (state : VocabState) : Prop :=
  forall term index,
    lookup_term (vocab_forward state) term = Some index <->
    lookup_index (vocab_reverse state) index = Some term.

Definition BatchPlan : Type :=
  (list Index * list (Term * Index) * Forward * Index)%type.

Fixpoint plan_batch
  (terms : list Term)
  (forward : Forward)
  (next_index : Index)
  : BatchPlan :=
  match terms with
  | [] => ([], [], forward, next_index)
  | term :: rest =>
      match lookup_term forward term with
      | Some existing =>
          let '(indices, assignments, forward', next') :=
            plan_batch rest forward next_index in
          (existing :: indices, assignments, forward', next')
      | None =>
          let assigned_index := next_index in
          let forward1 := put_forward forward term assigned_index in
          let '(indices, assignments, forward', next') :=
            plan_batch rest forward1 (S next_index) in
          (assigned_index :: indices,
           (term, assigned_index) :: assignments,
           forward',
           next')
      end
  end.

Definition batch_indices (plan : BatchPlan) : list Index :=
  let '(indices, _, _, _) := plan in indices.

Definition batch_assignments (plan : BatchPlan) : list (Term * Index) :=
  let '(_, assignments, _, _) := plan in assignments.

Fixpoint apply_assignments_no_wal
  (assignments : list (Term * Index))
  (state : VocabState)
  : VocabState :=
  match assignments with
  | [] => state
  | (term, index) :: rest =>
      apply_assignments_no_wal rest (assign_no_wal state term index)
  end.

Definition batch_success
  (state : VocabState)
  (terms : list Term)
  : VocabState :=
  let plan := plan_batch terms (vocab_forward state) (vocab_next_index state) in
  let assigned_state := apply_assignments_no_wal (batch_assignments plan) state in
  match batch_assignments plan with
  | [] => assigned_state
  | assignments =>
      mkVocabState
        (vocab_forward assigned_state)
        (vocab_reverse assigned_state)
        (append_wal (vocab_wal state) (VocabWalBatchInsert assignments))
        (vocab_next_index assigned_state)
  end.

Theorem put_forward_lookup_same :
  forall forward term index,
    lookup_term (put_forward forward term index) term = Some index.
Proof.
  intros forward term index.
  unfold lookup_term, put_forward.
  destruct (Nat.eq_dec term term) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem put_reverse_lookup_same :
  forall reverse term index,
    lookup_index (put_reverse reverse index term) index = Some term.
Proof.
  intros reverse term index.
  unfold lookup_index, put_reverse.
  destruct (Nat.eq_dec index index) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem wal_rejection_preserves_forward :
  forall state term index,
    lookup_term (vocab_forward state) term = None ->
    lookup_index (vocab_reverse state) index = None ->
    vocab_forward
      (state_after_insert_attempt state WalRejected term index) =
    vocab_forward state.
Proof.
  intros state term index Hterm Hindex.
  unfold state_after_insert_attempt, insert_with_index.
  rewrite Hterm, Hindex.
  reflexivity.
Qed.

Theorem wal_rejection_preserves_reverse :
  forall state term index,
    lookup_term (vocab_forward state) term = None ->
    lookup_index (vocab_reverse state) index = None ->
    vocab_reverse
      (state_after_insert_attempt state WalRejected term index) =
    vocab_reverse state.
Proof.
  intros state term index Hterm Hindex.
  unfold state_after_insert_attempt, insert_with_index.
  rewrite Hterm, Hindex.
  reflexivity.
Qed.

Theorem wal_rejection_preserves_wal :
  forall state term index,
    lookup_term (vocab_forward state) term = None ->
    lookup_index (vocab_reverse state) index = None ->
    vocab_wal
      (state_after_insert_attempt state WalRejected term index) =
    vocab_wal state.
Proof.
  intros state term index Hterm Hindex.
  unfold state_after_insert_attempt, insert_with_index.
  rewrite Hterm, Hindex.
  reflexivity.
Qed.

Theorem wal_rejection_preserves_next_index :
  forall state term index,
    lookup_term (vocab_forward state) term = None ->
    lookup_index (vocab_reverse state) index = None ->
    vocab_next_index
      (state_after_insert_attempt state WalRejected term index) =
    vocab_next_index state.
Proof.
  intros state term index Hterm Hindex.
  unfold state_after_insert_attempt, insert_with_index.
  rewrite Hterm, Hindex.
  reflexivity.
Qed.

Theorem successful_insert_sets_forward :
  forall state term index,
    lookup_term (vocab_forward state) term = None ->
    lookup_index (vocab_reverse state) index = None ->
    lookup_term
      (vocab_forward
        (state_after_insert_attempt state WalAccepted term index))
      term =
    Some index.
Proof.
  intros state term index Hterm Hindex.
  unfold state_after_insert_attempt, insert_with_index.
  rewrite Hterm, Hindex.
  unfold assign_with_wal, assign_no_wal.
  apply put_forward_lookup_same.
Qed.

Theorem successful_insert_sets_reverse :
  forall state term index,
    lookup_term (vocab_forward state) term = None ->
    lookup_index (vocab_reverse state) index = None ->
    lookup_index
      (vocab_reverse
        (state_after_insert_attempt state WalAccepted term index))
      index =
    Some term.
Proof.
  intros state term index Hterm Hindex.
  unfold state_after_insert_attempt, insert_with_index.
  rewrite Hterm, Hindex.
  unfold assign_with_wal, assign_no_wal.
  apply put_reverse_lookup_same.
Qed.

Theorem successful_insert_appends_wal :
  forall state term index,
    lookup_term (vocab_forward state) term = None ->
    lookup_index (vocab_reverse state) index = None ->
    vocab_wal
      (state_after_insert_attempt state WalAccepted term index) =
    append_wal (vocab_wal state) (VocabWalInsert term index).
Proof.
  intros state term index Hterm Hindex.
  unfold state_after_insert_attempt, insert_with_index.
  rewrite Hterm, Hindex.
  reflexivity.
Qed.

Theorem successful_insert_advances_next_index :
  forall state term index,
    lookup_term (vocab_forward state) term = None ->
    lookup_index (vocab_reverse state) index = None ->
    S index <=
      vocab_next_index
        (state_after_insert_attempt state WalAccepted term index).
Proof.
  intros state term index Hterm Hindex.
  unfold state_after_insert_attempt, insert_with_index.
  rewrite Hterm, Hindex.
  unfold assign_with_wal, assign_no_wal.
  apply Nat.le_max_l.
Qed.

Theorem duplicate_same_index_is_noop :
  forall state term index,
    lookup_term (vocab_forward state) term = Some index ->
    state_after_insert_attempt state WalAccepted term index = state.
Proof.
  intros state term index Hterm.
  unfold state_after_insert_attempt, insert_with_index.
  rewrite Hterm.
  destruct (Nat.eq_dec index index) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem term_reindex_rejected_preserves_state :
  forall state term old_index new_index,
    old_index <> new_index ->
    lookup_term (vocab_forward state) term = Some old_index ->
    state_after_insert_attempt state WalAccepted term new_index = state.
Proof.
  intros state term old_index new_index Hneq Hterm.
  unfold state_after_insert_attempt, insert_with_index.
  rewrite Hterm.
  destruct (Nat.eq_dec old_index new_index) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem index_collision_rejected_preserves_state :
  forall state term index existing_term,
    lookup_term (vocab_forward state) term = None ->
    lookup_index (vocab_reverse state) index = Some existing_term ->
    state_after_insert_attempt state WalAccepted term index = state.
Proof.
  intros state term index existing_term Hterm Hindex.
  unfold state_after_insert_attempt, insert_with_index.
  rewrite Hterm, Hindex.
  reflexivity.
Qed.

Theorem contains_index_sound :
  forall state index,
    contains_index state index = true ->
    exists term, lookup_index (vocab_reverse state) index = Some term.
Proof.
  intros state index Hcontains.
  unfold contains_index in Hcontains.
  destruct (lookup_index (vocab_reverse state) index) as [term |] eqn:Hlookup.
  - exists term. reflexivity.
  - discriminate Hcontains.
Qed.

Theorem contains_index_complete :
  forall state index term,
    lookup_index (vocab_reverse state) index = Some term ->
    contains_index state index = true.
Proof.
  intros state index term Hlookup.
  unfold contains_index.
  rewrite Hlookup.
  reflexivity.
Qed.

Definition sparse_state : VocabState :=
  assign_no_wal empty_state 7 2.

Theorem range_based_contains_index_is_unsound :
  range_contains_index sparse_state 1 = true /\
  contains_index sparse_state 1 = false.
Proof.
  split; reflexivity.
Qed.

Theorem empty_batch_plan_returns_empty_indices :
  batch_indices (plan_batch [] empty_forward 0) = [].
Proof.
  reflexivity.
Qed.

Theorem batch_duplicate_terms_share_first_index :
  batch_indices (plan_batch [1; 2; 1; 3; 2] empty_forward 0) =
  [0; 1; 0; 2; 1].
Proof.
  reflexivity.
Qed.

Theorem batch_duplicate_terms_log_first_occurrences_only :
  batch_assignments (plan_batch [1; 2; 1; 3; 2] empty_forward 0) =
  [(1, 0); (2, 1); (3, 2)].
Proof.
  reflexivity.
Qed.

Theorem batch_success_appends_single_batch_record :
  vocab_wal (batch_success empty_state [1; 2; 1]) =
  [VocabWalBatchInsert [(1, 0); (2, 1)]].
Proof.
  reflexivity.
Qed.

Theorem batch_success_sets_first_occurrence_indexes :
  lookup_term
    (vocab_forward (batch_success empty_state [1; 2; 1]))
    1 =
  Some 0 /\
  lookup_term
    (vocab_forward (batch_success empty_state [1; 2; 1]))
    2 =
  Some 1.
Proof.
  split; reflexivity.
Qed.

Theorem batch_success_sets_reverse_indexes :
  lookup_index
    (vocab_reverse (batch_success empty_state [1; 2; 1]))
    0 =
  Some 1 /\
  lookup_index
    (vocab_reverse (batch_success empty_state [1; 2; 1]))
    1 =
  Some 2.
Proof.
  split; reflexivity.
Qed.

Theorem exact_forward_reverse_after_single_insert :
  forward_reverse_exact
    (state_after_insert_attempt empty_state WalAccepted 5 0).
Proof.
  unfold forward_reverse_exact.
  intros term index.
  unfold state_after_insert_attempt, insert_with_index, empty_state,
    empty_forward, empty_reverse, lookup_term, lookup_index,
    assign_with_wal, assign_no_wal, put_forward, put_reverse.
  cbn.
  split; intros H;
    repeat match goal with
    | H : context [Nat.eq_dec ?x ?y] |- _ =>
        destruct (Nat.eq_dec x y); subst; simpl in H
    | |- context [Nat.eq_dec ?x ?y] =>
        destruct (Nat.eq_dec x y); subst; simpl
    end;
    try discriminate;
    try congruence;
    reflexivity.
Qed.
