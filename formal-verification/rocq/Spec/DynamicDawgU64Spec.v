(** * DynamicDawgU64Spec: u64 Sequence DAWG Semantic Preservation

    This module states the proof boundary for libdictenstein's public
    [DynamicDawgU64] backend.  Unlike the byte/Unicode [DynamicDawg] backends,
    [DynamicDawgU64] has a separate ArcSwap/CAS implementation and its primary
    key type is a sequence of 64-bit labels.  The correctness claim here is
    semantic: sequence mutation, value lookup, string/f64 adapter APIs,
    iterator enumeration, and zipper navigation refine an abstract finite
    set/map over u64 sequences.

    The model abstracts away node layout, Arc reference counts, hash quality,
    progress/liveness, exact node counts, and canonical minimization.
*)

From Stdlib Require Import Lists.List.
From Stdlib Require Import Bool.Bool.
From Stdlib Require Import Arith.PeanoNat.
From Stdlib Require Import Logic.FunctionalExtensionality.
Require Import ARTrie.Spec.DictionaryLawSpec.
Import ListNotations.

Definition U64Label := nat.
Definition U64Sequence := list U64Label.
Definition U64State := nat.

Definition sequence_eq_dec
  (left right : U64Sequence) : {left = right} + {left <> right} :=
  list_eq_dec Nat.eq_dec left right.

(** ** Reference set and map over u64 sequences *)

Definition U64Set := U64Sequence -> bool.
Definition U64Map (V : Type) := U64Sequence -> option V.

Definition u64_set_empty : U64Set := fun _ => false.

Definition u64_set_contains (set : U64Set) (sequence : U64Sequence) : bool :=
  set sequence.

Definition u64_set_insert (set : U64Set) (sequence : U64Sequence) : U64Set :=
  fun query => if sequence_eq_dec sequence query then true else set query.

Definition u64_set_remove (set : U64Set) (sequence : U64Sequence) : U64Set :=
  fun query => if sequence_eq_dec sequence query then false else set query.

Definition u64_map_empty {V : Type} : U64Map V := fun _ => None.

Definition u64_map_lookup {V : Type}
  (map : U64Map V) (sequence : U64Sequence) : option V :=
  map sequence.

Definition u64_map_insert {V : Type}
  (map : U64Map V) (sequence : U64Sequence) (value : V) : U64Map V :=
  fun query => if sequence_eq_dec sequence query then Some value else map query.

Definition u64_map_delete {V : Type}
  (map : U64Map V) (sequence : U64Sequence) : U64Map V :=
  fun query => if sequence_eq_dec sequence query then None else map query.

Definition u64_map_update_or_insert {V : Type}
  (map : U64Map V) (sequence : U64Sequence) (default : V) (update : V -> V)
  : U64Map V :=
  fun query =>
    if sequence_eq_dec sequence query then
      match map sequence with
      | Some old => Some (update old)
      | None => Some default
      end
    else map query.

Fixpoint u64_insert_sequences (sequences : list U64Sequence) (set : U64Set)
  : U64Set :=
  match sequences with
  | [] => set
  | sequence :: rest =>
      u64_insert_sequences rest (u64_set_insert set sequence)
  end.

Fixpoint u64_remove_sequences (sequences : list U64Sequence) (set : U64Set)
  : U64Set :=
  match sequences with
  | [] => set
  | sequence :: rest =>
      u64_remove_sequences rest (u64_set_remove set sequence)
  end.

Fixpoint u64_delete_sequences {V : Type}
  (sequences : list U64Sequence) (map : U64Map V) : U64Map V :=
  match sequences with
  | [] => map
  | sequence :: rest =>
      u64_delete_sequences rest (u64_map_delete map sequence)
  end.

Fixpoint u64_count_new_sequences
  (sequences : list U64Sequence) (set : U64Set) : nat :=
  match sequences with
  | [] => 0
  | sequence :: rest =>
      if u64_set_contains set sequence then
        u64_count_new_sequences rest set
      else
        S (u64_count_new_sequences rest (u64_set_insert set sequence))
  end.

Fixpoint u64_count_removed_sequences
  (sequences : list U64Sequence) (set : U64Set) : nat :=
  match sequences with
  | [] => 0
  | sequence :: rest =>
      if u64_set_contains set sequence then
        S (u64_count_removed_sequences rest (u64_set_remove set sequence))
      else
        u64_count_removed_sequences rest set
  end.

Theorem u64_set_insert_contains_same : forall set sequence,
  u64_set_contains (u64_set_insert set sequence) sequence = true.
Proof.
  intros set sequence.
  unfold u64_set_contains, u64_set_insert.
  destruct (sequence_eq_dec sequence sequence) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem u64_set_insert_contains_other : forall set sequence query,
  sequence <> query ->
  u64_set_contains (u64_set_insert set sequence) query =
  u64_set_contains set query.
Proof.
  intros set sequence query Hneq.
  unfold u64_set_contains, u64_set_insert.
  destruct (sequence_eq_dec sequence query) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem u64_set_remove_contains_same : forall set sequence,
  u64_set_contains (u64_set_remove set sequence) sequence = false.
Proof.
  intros set sequence.
  unfold u64_set_contains, u64_set_remove.
  destruct (sequence_eq_dec sequence sequence) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem u64_set_remove_contains_other : forall set sequence query,
  sequence <> query ->
  u64_set_contains (u64_set_remove set sequence) query =
  u64_set_contains set query.
Proof.
  intros set sequence query Hneq.
  unfold u64_set_contains, u64_set_remove.
  destruct (sequence_eq_dec sequence query) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem u64_map_lookup_insert_same : forall (V : Type) map sequence value,
  u64_map_lookup (V := V) (u64_map_insert map sequence value) sequence =
  Some value.
Proof.
  intros V map sequence value.
  unfold u64_map_lookup, u64_map_insert.
  destruct (sequence_eq_dec sequence sequence) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem u64_map_lookup_insert_other :
  forall (V : Type) map sequence query value,
    sequence <> query ->
    u64_map_lookup (V := V) (u64_map_insert map sequence value) query =
    u64_map_lookup map query.
Proof.
  intros V map sequence query value Hneq.
  unfold u64_map_lookup, u64_map_insert.
  destruct (sequence_eq_dec sequence query) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem u64_map_lookup_delete_same : forall (V : Type) map sequence,
  u64_map_lookup (V := V) (u64_map_delete map sequence) sequence = None.
Proof.
  intros V map sequence.
  unfold u64_map_lookup, u64_map_delete.
  destruct (sequence_eq_dec sequence sequence) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem u64_map_lookup_delete_other :
  forall (V : Type) map sequence query,
    sequence <> query ->
    u64_map_lookup (V := V) (u64_map_delete map sequence) query =
    u64_map_lookup map query.
Proof.
  intros V map sequence query Hneq.
  unfold u64_map_lookup, u64_map_delete.
  destruct (sequence_eq_dec sequence query) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem u64_update_or_insert_lookup_same :
  forall (V : Type) map sequence default update,
    u64_map_lookup (V := V)
      (u64_map_update_or_insert map sequence default update)
      sequence =
    match u64_map_lookup map sequence with
    | Some old => Some (update old)
    | None => Some default
    end.
Proof.
  intros V map sequence default update.
  unfold u64_map_lookup, u64_map_update_or_insert.
  destruct (sequence_eq_dec sequence sequence) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem u64_update_or_insert_lookup_other :
  forall (V : Type) map sequence query default update,
    sequence <> query ->
    u64_map_lookup (V := V)
      (u64_map_update_or_insert map sequence default update)
      query =
    u64_map_lookup map query.
Proof.
  intros V map sequence query default update Hneq.
  unfold u64_map_lookup, u64_map_update_or_insert.
  destruct (sequence_eq_dec sequence query) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Lemma u64_insert_sequences_preserves_present :
  forall sequences set sequence,
    u64_set_contains set sequence = true ->
    u64_set_contains (u64_insert_sequences sequences set) sequence = true.
Proof.
  induction sequences as [| head rest IH]; intros set sequence Hpresent.
  - exact Hpresent.
  - simpl.
    apply IH.
    unfold u64_set_contains, u64_set_insert.
    destruct (sequence_eq_dec head sequence) as [_ | _].
    + reflexivity.
    + exact Hpresent.
Qed.

Lemma u64_insert_sequences_contains_in :
  forall sequences set sequence,
    In sequence sequences ->
    u64_set_contains (u64_insert_sequences sequences set) sequence = true.
Proof.
  induction sequences as [| head rest IH]; intros set sequence Hin.
  - contradiction.
  - simpl in Hin.
    simpl.
    destruct Hin as [Heq | Hin].
    + subst.
      apply u64_insert_sequences_preserves_present.
      apply u64_set_insert_contains_same.
    + apply IH.
      exact Hin.
Qed.

Lemma u64_remove_sequences_preserves_absent :
  forall sequences set sequence,
    u64_set_contains set sequence = false ->
    u64_set_contains (u64_remove_sequences sequences set) sequence = false.
Proof.
  induction sequences as [| head rest IH]; intros set sequence Habsent.
  - exact Habsent.
  - simpl.
    apply IH.
    unfold u64_set_contains, u64_set_remove.
    destruct (sequence_eq_dec head sequence) as [_ | _].
    + reflexivity.
    + exact Habsent.
Qed.

Lemma u64_remove_sequences_removes_in :
  forall sequences set sequence,
    In sequence sequences ->
    u64_set_contains (u64_remove_sequences sequences set) sequence = false.
Proof.
  induction sequences as [| head rest IH]; intros set sequence Hin.
  - contradiction.
  - simpl in Hin.
    simpl.
    destruct Hin as [Heq | Hin].
    + subst.
      apply u64_remove_sequences_preserves_absent.
      apply u64_set_remove_contains_same.
    + apply IH.
      exact Hin.
Qed.

Lemma u64_delete_sequences_preserves_absent :
  forall (V : Type) sequences (map : U64Map V) sequence,
    u64_map_lookup map sequence = None ->
    u64_map_lookup (u64_delete_sequences sequences map) sequence = None.
Proof.
  intros V sequences.
  induction sequences as [| head rest IH]; intros map sequence Habsent.
  - exact Habsent.
  - simpl.
    apply IH.
    unfold u64_map_lookup, u64_map_delete.
    destruct (sequence_eq_dec head sequence) as [_ | _].
    + reflexivity.
    + exact Habsent.
Qed.

Lemma u64_delete_sequences_removes_in :
  forall (V : Type) sequences (map : U64Map V) sequence,
    In sequence sequences ->
    u64_map_lookup (u64_delete_sequences sequences map) sequence = None.
Proof.
  intros V sequences.
  induction sequences as [| head rest IH]; intros map sequence Hin.
  - contradiction.
  - simpl in Hin.
    simpl.
    destruct Hin as [Heq | Hin].
    + subst.
      apply u64_delete_sequences_preserves_absent.
      apply u64_map_lookup_delete_same.
    + apply IH.
      exact Hin.
Qed.

Lemma u64_count_new_sequences_zero_for_present :
  forall sequences set,
    (forall sequence,
      In sequence sequences ->
      u64_set_contains set sequence = true) ->
    u64_count_new_sequences sequences set = 0.
Proof.
  induction sequences as [| head rest IH]; intros set Hall.
  - reflexivity.
  - simpl.
    rewrite Hall by (left; reflexivity).
    apply IH.
    intros sequence Hin.
    apply Hall.
    right.
    exact Hin.
Qed.

Lemma u64_count_removed_sequences_zero_for_absent :
  forall sequences set,
    (forall sequence,
      In sequence sequences ->
      u64_set_contains set sequence = false) ->
    u64_count_removed_sequences sequences set = 0.
Proof.
  induction sequences as [| head rest IH]; intros set Hall.
  - reflexivity.
  - simpl.
    rewrite Hall by (left; reflexivity).
    apply IH.
    intros sequence Hin.
    apply Hall.
    right.
    exact Hin.
Qed.

(** ** Graph semantics *)

Definition U64Step := U64State -> U64Label -> option U64State.

Fixpoint u64_walk_from
  (step : U64Step) (state : U64State) (sequence : U64Sequence)
  : option U64State :=
  match sequence with
  | [] => Some state
  | label :: rest =>
      match step state label with
      | Some next => u64_walk_from step next rest
      | None => None
      end
  end.

Record DynamicDawgU64Graph (V : Type) := {
  u64_root : U64State;
  u64_step : U64Step;
  u64_final : U64State -> bool;
  u64_value : U64State -> option V
}.

Definition u64_graph_state_for {V : Type}
  (graph : DynamicDawgU64Graph V) (sequence : U64Sequence)
  : option U64State :=
  u64_walk_from (u64_step V graph) (u64_root V graph) sequence.

Definition u64_graph_contains {V : Type}
  (graph : DynamicDawgU64Graph V) (sequence : U64Sequence) : bool :=
  match u64_graph_state_for graph sequence with
  | Some state => u64_final V graph state
  | None => false
  end.

Definition u64_graph_lookup {V : Type}
  (graph : DynamicDawgU64Graph V) (sequence : U64Sequence) : option V :=
  match u64_graph_state_for graph sequence with
  | Some state =>
      if u64_final V graph state then u64_value V graph state else None
  | None => None
  end.

Definition u64_graph_language {V : Type}
  (graph : DynamicDawgU64Graph V) : U64Set :=
  u64_graph_contains graph.

Definition u64_graph_map {V : Type}
  (graph : DynamicDawgU64Graph V) : U64Map V :=
  u64_graph_lookup graph.

(** ** Zipper and enumeration semantics *)

Record U64Zipper (V : Type) := {
  zipper_state : U64State;
  zipper_path : U64Sequence
}.

Definition zipper_valid {V : Type}
  (graph : DynamicDawgU64Graph V) (zipper : U64Zipper V) : Prop :=
  u64_graph_state_for graph (zipper_path V zipper) =
  Some (zipper_state V zipper).

Definition zipper_final {V : Type}
  (graph : DynamicDawgU64Graph V) (zipper : U64Zipper V) : bool :=
  u64_final V graph (zipper_state V zipper).

Definition zipper_value {V : Type}
  (graph : DynamicDawgU64Graph V) (zipper : U64Zipper V) : option V :=
  if zipper_final graph zipper then
    u64_value V graph (zipper_state V zipper)
  else None.

Definition zipper_descend {V : Type}
  (graph : DynamicDawgU64Graph V) (zipper : U64Zipper V) (label : U64Label)
  : option (U64Zipper V) :=
  match u64_step V graph (zipper_state V zipper) label with
  | Some next =>
      Some {| zipper_state := next;
              zipper_path := zipper_path V zipper ++ [label] |}
  | None => None
  end.

Lemma u64_walk_append_one :
  forall step root path state label next,
    u64_walk_from step root path = Some state ->
    step state label = Some next ->
    u64_walk_from step root (path ++ [label]) = Some next.
Proof.
  intros step root path.
  revert root.
  induction path as [| head rest IH]; intros root state label next Hwalk Hstep.
  - simpl in Hwalk.
    injection Hwalk as Heq.
    subst.
    simpl.
    rewrite Hstep.
    reflexivity.
  - simpl in Hwalk.
    simpl.
    destruct (step root head) as [child |] eqn:Hchild.
    + apply IH with (state := state).
      * exact Hwalk.
      * exact Hstep.
    + discriminate.
Qed.

Theorem zipper_descend_preserves_valid :
  forall (V : Type) graph zipper label child,
    zipper_valid (V := V) graph zipper ->
    zipper_descend (V := V) graph zipper label = Some child ->
    zipper_valid (V := V) graph child.
Proof.
  intros V graph zipper label child Hvalid Hdesc.
  unfold zipper_descend in Hdesc.
  destruct (u64_step V graph (zipper_state V zipper) label) as [next |]
    eqn:Hstep; try discriminate.
  injection Hdesc as Hchild.
  subst.
  unfold zipper_valid in *.
  simpl.
  apply u64_walk_append_one with (state := zipper_state V zipper).
  - exact Hvalid.
  - exact Hstep.
Qed.

Theorem zipper_valid_final_agrees_with_contains :
  forall (V : Type) graph zipper,
    zipper_valid (V := V) graph zipper ->
    zipper_final (V := V) graph zipper =
    u64_graph_contains graph (zipper_path V zipper).
Proof.
  intros V graph zipper Hvalid.
  unfold zipper_final, u64_graph_contains, zipper_valid in *.
  rewrite Hvalid.
  reflexivity.
Qed.

Theorem zipper_valid_value_agrees_with_lookup :
  forall (V : Type) graph zipper,
    zipper_valid (V := V) graph zipper ->
    zipper_value (V := V) graph zipper =
    u64_graph_lookup graph (zipper_path V zipper).
Proof.
  intros V graph zipper Hvalid.
  unfold zipper_value, zipper_final, u64_graph_lookup, zipper_valid in *.
  rewrite Hvalid.
  reflexivity.
Qed.

Definition iterates_exactly {V : Type}
  (graph : DynamicDawgU64Graph V) (sequences : list U64Sequence) : Prop :=
  NoDup sequences /\
  forall sequence,
    In sequence sequences <->
    u64_graph_contains graph sequence = true.

Definition iterates_values_exactly {V : Type}
  (graph : DynamicDawgU64Graph V) (entries : list (U64Sequence * V)) : Prop :=
  NoDup (map fst entries) /\
  forall sequence value,
    In (sequence, value) entries <->
    u64_graph_lookup graph sequence = Some value.

Theorem iterator_sound :
  forall (V : Type) graph sequences sequence,
    iterates_exactly (V := V) graph sequences ->
    In sequence sequences ->
    u64_graph_contains (V := V) graph sequence = true.
Proof.
  intros V graph sequences sequence [_ Hexact] Hin.
  apply Hexact.
  exact Hin.
Qed.

Theorem iterator_complete :
  forall (V : Type) graph sequences sequence,
    iterates_exactly (V := V) graph sequences ->
    u64_graph_contains (V := V) graph sequence = true ->
    In sequence sequences.
Proof.
  intros V graph sequences sequence [_ Hexact] Hcontains.
  apply Hexact.
  exact Hcontains.
Qed.

Theorem valued_iterator_sound :
  forall (V : Type) graph entries sequence value,
    iterates_values_exactly (V := V) graph entries ->
    In (sequence, value) entries ->
    u64_graph_lookup (V := V) graph sequence = Some value.
Proof.
  intros V graph entries sequence value [_ Hexact] Hin.
  apply Hexact.
  exact Hin.
Qed.

Theorem valued_iterator_complete :
  forall (V : Type) graph entries sequence value,
    iterates_values_exactly (V := V) graph entries ->
    u64_graph_lookup (V := V) graph sequence = Some value ->
    In (sequence, value) entries.
Proof.
  intros V graph entries sequence value [_ Hexact] Hlookup.
  apply Hexact.
  exact Hlookup.
Qed.

(** ** Public operation laws *)

Section DynamicDawgU64OperationLaws.

Variable V : Type.
Variable StringTerm : Type.
Variable FloatTerm : Type.

Variable EncodeString : StringTerm -> U64Sequence.
Variable EncodeFloat : FloatTerm -> U64Label.

Definition encode_float_series (series : list FloatTerm) : U64Sequence :=
  map EncodeFloat series.

Variable InsertSequence :
  DynamicDawgU64Graph V -> U64Sequence -> DynamicDawgU64Graph V.
Variable InsertSequenceWithValue :
  DynamicDawgU64Graph V -> U64Sequence -> V -> DynamicDawgU64Graph V.
Variable UpdateOrInsertSequence :
  DynamicDawgU64Graph V -> U64Sequence -> V -> (V -> V) ->
  DynamicDawgU64Graph V.
Variable RemoveSequence :
  DynamicDawgU64Graph V -> U64Sequence -> DynamicDawgU64Graph V.
Variable Compact :
  DynamicDawgU64Graph V -> DynamicDawgU64Graph V.
Variable Minimize :
  DynamicDawgU64Graph V -> DynamicDawgU64Graph V.
Variable InsertManySequences :
  DynamicDawgU64Graph V -> list U64Sequence -> DynamicDawgU64Graph V.
Variable RemoveManySequences :
  DynamicDawgU64Graph V -> list U64Sequence -> DynamicDawgU64Graph V.

Variable InsertSequenceReturnsNew :
  DynamicDawgU64Graph V -> U64Sequence -> bool.
Variable InsertSequenceWithValueReturnsNew :
  DynamicDawgU64Graph V -> U64Sequence -> V -> bool.
Variable UpdateOrInsertSequenceReturnsNew :
  DynamicDawgU64Graph V -> U64Sequence -> V -> (V -> V) -> bool.
Variable RemoveSequenceReturnsPresent :
  DynamicDawgU64Graph V -> U64Sequence -> bool.
Variable InsertManySequencesReturnsAdded :
  DynamicDawgU64Graph V -> list U64Sequence -> nat.
Variable RemoveManySequencesReturnsRemoved :
  DynamicDawgU64Graph V -> list U64Sequence -> nat.

Variable InsertString :
  DynamicDawgU64Graph V -> StringTerm -> DynamicDawgU64Graph V.
Variable RemoveString :
  DynamicDawgU64Graph V -> StringTerm -> DynamicDawgU64Graph V.
Variable InsertFloatSeries :
  DynamicDawgU64Graph V -> list FloatTerm -> DynamicDawgU64Graph V.
Variable RemoveFloatSeries :
  DynamicDawgU64Graph V -> list FloatTerm -> DynamicDawgU64Graph V.

Record DynamicDawgU64Laws := {
  u64_insert_sequence_refines_set :
    forall graph sequence query,
      u64_graph_contains (InsertSequence graph sequence) query =
      u64_set_contains
        (u64_set_insert (u64_graph_language graph) sequence)
        query;

  u64_insert_sequence_returns_new :
    forall graph sequence,
      InsertSequenceReturnsNew graph sequence =
      negb (u64_graph_contains graph sequence);

  u64_insert_sequence_with_value_refines_map :
    forall graph sequence value query,
      u64_graph_lookup (InsertSequenceWithValue graph sequence value) query =
      u64_map_lookup
        (u64_map_insert (u64_graph_map graph) sequence value)
        query;

  u64_insert_sequence_with_value_refines_set :
    forall graph sequence value query,
      u64_graph_contains (InsertSequenceWithValue graph sequence value) query =
      u64_set_contains
        (u64_set_insert (u64_graph_language graph) sequence)
        query;

  u64_insert_sequence_with_value_returns_new :
    forall graph sequence value,
      InsertSequenceWithValueReturnsNew graph sequence value =
      negb (u64_graph_contains graph sequence);

  u64_update_or_insert_sequence_refines_map :
    forall graph sequence default update query,
      u64_graph_lookup
        (UpdateOrInsertSequence graph sequence default update)
        query =
      u64_map_lookup
        (u64_map_update_or_insert
          (u64_graph_map graph) sequence default update)
        query;

  u64_update_or_insert_sequence_refines_set :
    forall graph sequence default update query,
      u64_graph_contains
        (UpdateOrInsertSequence graph sequence default update)
        query =
      u64_set_contains
        (u64_set_insert (u64_graph_language graph) sequence)
        query;

  u64_update_or_insert_sequence_returns_new :
    forall graph sequence default update,
      UpdateOrInsertSequenceReturnsNew graph sequence default update =
      negb (u64_graph_contains graph sequence);

  u64_remove_sequence_refines_set :
    forall graph sequence query,
      u64_graph_contains (RemoveSequence graph sequence) query =
      u64_set_contains
        (u64_set_remove (u64_graph_language graph) sequence)
        query;

  u64_remove_sequence_refines_map :
    forall graph sequence query,
      u64_graph_lookup (RemoveSequence graph sequence) query =
      u64_map_lookup
        (u64_map_delete (u64_graph_map graph) sequence)
        query;

  u64_remove_sequence_returns_present :
    forall graph sequence,
      RemoveSequenceReturnsPresent graph sequence =
      u64_graph_contains graph sequence;

  u64_compact_preserves_set :
    forall graph query,
      u64_graph_contains (Compact graph) query =
      u64_graph_contains graph query;

  u64_compact_preserves_map :
    forall graph query,
      u64_graph_lookup (Compact graph) query =
      u64_graph_lookup graph query;

  u64_minimize_preserves_set :
    forall graph query,
      u64_graph_contains (Minimize graph) query =
      u64_graph_contains graph query;

  u64_minimize_preserves_map :
    forall graph query,
      u64_graph_lookup (Minimize graph) query =
      u64_graph_lookup graph query;

  u64_insert_many_refines_set :
    forall graph sequences query,
      u64_graph_contains (InsertManySequences graph sequences) query =
      u64_set_contains
        (u64_insert_sequences sequences (u64_graph_language graph))
        query;

  u64_insert_many_returns_added :
    forall graph sequences,
      InsertManySequencesReturnsAdded graph sequences =
      u64_count_new_sequences sequences (u64_graph_language graph);

  u64_remove_many_refines_set :
    forall graph sequences query,
      u64_graph_contains (RemoveManySequences graph sequences) query =
      u64_set_contains
        (u64_remove_sequences sequences (u64_graph_language graph))
        query;

  u64_remove_many_refines_map :
    forall graph sequences query,
      u64_graph_lookup (RemoveManySequences graph sequences) query =
      u64_map_lookup
        (u64_delete_sequences sequences (u64_graph_map graph))
        query;

  u64_remove_many_returns_removed :
    forall graph sequences,
      RemoveManySequencesReturnsRemoved graph sequences =
      u64_count_removed_sequences sequences (u64_graph_language graph);

  u64_string_insert_refines_sequence :
    forall graph text query,
      u64_graph_contains (InsertString graph text) query =
      u64_graph_contains
        (InsertSequence graph (EncodeString text))
        query;

  u64_string_remove_refines_sequence :
    forall graph text query,
      u64_graph_contains (RemoveString graph text) query =
      u64_graph_contains
        (RemoveSequence graph (EncodeString text))
        query;

  u64_f64_insert_refines_sequence :
    forall graph series query,
      u64_graph_contains (InsertFloatSeries graph series) query =
      u64_graph_contains
        (InsertSequence graph (encode_float_series series))
        query;

  u64_f64_remove_refines_sequence :
    forall graph series query,
      u64_graph_contains (RemoveFloatSeries graph series) query =
      u64_graph_contains
        (RemoveSequence graph (encode_float_series series))
        query
}.

Variable laws : DynamicDawgU64Laws.

Theorem u64_insert_sequence_contains_same : forall graph sequence,
  u64_graph_contains (InsertSequence graph sequence) sequence = true.
Proof.
  intros graph sequence.
  rewrite (u64_insert_sequence_refines_set laws graph sequence sequence).
  apply u64_set_insert_contains_same.
Qed.

Theorem u64_insert_sequence_contains_other :
  forall graph sequence query,
    sequence <> query ->
    u64_graph_contains (InsertSequence graph sequence) query =
    u64_graph_contains graph query.
Proof.
  intros graph sequence query Hneq.
  rewrite (u64_insert_sequence_refines_set laws graph sequence query).
  apply u64_set_insert_contains_other.
  exact Hneq.
Qed.

Theorem u64_insert_sequence_duplicate_return_false :
  forall graph sequence,
    u64_graph_contains graph sequence = true ->
    InsertSequenceReturnsNew graph sequence = false.
Proof.
  intros graph sequence Hpresent.
  rewrite (u64_insert_sequence_returns_new laws graph sequence).
  rewrite Hpresent.
  reflexivity.
Qed.

Theorem u64_insert_sequence_absent_return_true :
  forall graph sequence,
    u64_graph_contains graph sequence = false ->
    InsertSequenceReturnsNew graph sequence = true.
Proof.
  intros graph sequence Habsent.
  rewrite (u64_insert_sequence_returns_new laws graph sequence).
  rewrite Habsent.
  reflexivity.
Qed.

Theorem u64_insert_sequence_with_value_lookup_same :
  forall graph sequence value,
    u64_graph_lookup
      (InsertSequenceWithValue graph sequence value)
      sequence = Some value.
Proof.
  intros graph sequence value.
  rewrite (u64_insert_sequence_with_value_refines_map
    laws graph sequence value sequence).
  apply u64_map_lookup_insert_same.
Qed.

Theorem u64_insert_sequence_with_value_lookup_other :
  forall graph sequence query value,
    sequence <> query ->
    u64_graph_lookup
      (InsertSequenceWithValue graph sequence value)
      query =
    u64_graph_lookup graph query.
Proof.
  intros graph sequence query value Hneq.
  rewrite (u64_insert_sequence_with_value_refines_map
    laws graph sequence value query).
  apply u64_map_lookup_insert_other.
  exact Hneq.
Qed.

Theorem u64_update_or_insert_sequence_lookup_same :
  forall graph sequence default update,
    u64_graph_lookup
      (UpdateOrInsertSequence graph sequence default update)
      sequence =
    match u64_graph_lookup graph sequence with
    | Some old => Some (update old)
    | None => Some default
    end.
Proof.
  intros graph sequence default update.
  rewrite (u64_update_or_insert_sequence_refines_map
    laws graph sequence default update sequence).
  apply u64_update_or_insert_lookup_same.
Qed.

Theorem u64_update_or_insert_sequence_lookup_other :
  forall graph sequence query default update,
    sequence <> query ->
    u64_graph_lookup
      (UpdateOrInsertSequence graph sequence default update)
      query =
    u64_graph_lookup graph query.
Proof.
  intros graph sequence query default update Hneq.
  rewrite (u64_update_or_insert_sequence_refines_map
    laws graph sequence default update query).
  apply u64_update_or_insert_lookup_other.
  exact Hneq.
Qed.

Theorem u64_remove_sequence_contains_same : forall graph sequence,
  u64_graph_contains (RemoveSequence graph sequence) sequence = false.
Proof.
  intros graph sequence.
  rewrite (u64_remove_sequence_refines_set laws graph sequence sequence).
  apply u64_set_remove_contains_same.
Qed.

Theorem u64_remove_sequence_contains_other :
  forall graph sequence query,
    sequence <> query ->
    u64_graph_contains (RemoveSequence graph sequence) query =
    u64_graph_contains graph query.
Proof.
  intros graph sequence query Hneq.
  rewrite (u64_remove_sequence_refines_set laws graph sequence query).
  apply u64_set_remove_contains_other.
  exact Hneq.
Qed.

Theorem u64_remove_sequence_lookup_same : forall graph sequence,
  u64_graph_lookup (RemoveSequence graph sequence) sequence = None.
Proof.
  intros graph sequence.
  rewrite (u64_remove_sequence_refines_map laws graph sequence sequence).
  apply u64_map_lookup_delete_same.
Qed.

Theorem u64_remove_sequence_lookup_other :
  forall graph sequence query,
    sequence <> query ->
    u64_graph_lookup (RemoveSequence graph sequence) query =
    u64_graph_lookup graph query.
Proof.
  intros graph sequence query Hneq.
  rewrite (u64_remove_sequence_refines_map laws graph sequence query).
  apply u64_map_lookup_delete_other.
  exact Hneq.
Qed.

Theorem u64_compact_minimize_commute_semantically :
  forall graph query,
    u64_graph_contains (Compact (Minimize graph)) query =
    u64_graph_contains (Minimize (Compact graph)) query.
Proof.
  intros graph query.
  rewrite (u64_compact_preserves_set laws).
  rewrite (u64_minimize_preserves_set laws).
  rewrite (u64_minimize_preserves_set laws).
  rewrite (u64_compact_preserves_set laws).
  reflexivity.
Qed.

Theorem u64_insert_many_contains_listed :
  forall graph sequences sequence,
    In sequence sequences ->
    u64_graph_contains (InsertManySequences graph sequences) sequence = true.
Proof.
  intros graph sequences sequence Hin.
  rewrite (u64_insert_many_refines_set laws graph sequences sequence).
  apply u64_insert_sequences_contains_in.
  exact Hin.
Qed.

Theorem u64_insert_many_all_present_returns_zero :
  forall graph sequences,
    (forall sequence,
      In sequence sequences ->
      u64_graph_contains graph sequence = true) ->
    InsertManySequencesReturnsAdded graph sequences = 0.
Proof.
  intros graph sequences Hall.
  rewrite (u64_insert_many_returns_added laws graph sequences).
  apply u64_count_new_sequences_zero_for_present.
  exact Hall.
Qed.

Theorem u64_remove_many_removes_listed :
  forall graph sequences sequence,
    In sequence sequences ->
    u64_graph_contains (RemoveManySequences graph sequences) sequence = false.
Proof.
  intros graph sequences sequence Hin.
  rewrite (u64_remove_many_refines_set laws graph sequences sequence).
  apply u64_remove_sequences_removes_in.
  exact Hin.
Qed.

Theorem u64_remove_many_lookup_deleted :
  forall graph sequences sequence,
    In sequence sequences ->
    u64_graph_lookup (RemoveManySequences graph sequences) sequence = None.
Proof.
  intros graph sequences sequence Hin.
  rewrite (u64_remove_many_refines_map laws graph sequences sequence).
  apply u64_delete_sequences_removes_in.
  exact Hin.
Qed.

Theorem u64_remove_many_all_absent_returns_zero :
  forall graph sequences,
    (forall sequence,
      In sequence sequences ->
      u64_graph_contains graph sequence = false) ->
    RemoveManySequencesReturnsRemoved graph sequences = 0.
Proof.
  intros graph sequences Hall.
  rewrite (u64_remove_many_returns_removed laws graph sequences).
  apply u64_count_removed_sequences_zero_for_absent.
  exact Hall.
Qed.

Theorem u64_string_insert_contains_encoded :
  forall graph text,
    u64_graph_contains (InsertString graph text) (EncodeString text) = true.
Proof.
  intros graph text.
  rewrite (u64_string_insert_refines_sequence
    laws graph text (EncodeString text)).
  apply u64_insert_sequence_contains_same.
Qed.

Theorem u64_string_remove_removes_encoded :
  forall graph text,
    u64_graph_contains (RemoveString graph text) (EncodeString text) = false.
Proof.
  intros graph text.
  rewrite (u64_string_remove_refines_sequence
    laws graph text (EncodeString text)).
  apply u64_remove_sequence_contains_same.
Qed.

Theorem u64_f64_insert_contains_encoded :
  forall graph series,
    u64_graph_contains
      (InsertFloatSeries graph series)
      (encode_float_series series) = true.
Proof.
  intros graph series.
  rewrite (u64_f64_insert_refines_sequence
    laws graph series (encode_float_series series)).
  apply u64_insert_sequence_contains_same.
Qed.

Theorem u64_f64_remove_removes_encoded :
  forall graph series,
    u64_graph_contains
      (RemoveFloatSeries graph series)
      (encode_float_series series) = false.
Proof.
  intros graph series.
  rewrite (u64_f64_remove_refines_sequence
    laws graph series (encode_float_series series)).
  apply u64_remove_sequence_contains_same.
Qed.

End DynamicDawgU64OperationLaws.
