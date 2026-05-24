(** * DynamicDawgMutationSpec: Mutable DAWG Semantic Preservation

    This module states the proof boundary for libdictenstein's mutable
    in-memory DAWG backends.  The claim is semantic: public mutation,
    compaction, and minimization preserve the accepted language and mapped
    values described by the reference set/map laws.  It deliberately does not
    claim that the node graph is optimally small; the Rust implementation
    documents that [minimize()] and [compact()] may produce different node
    counts while preserving the same terms.
*)

From Stdlib Require Import Lists.List.
From Stdlib Require Import Bool.Bool.
From Stdlib Require Import Arith.PeanoNat.
From Stdlib Require Import Logic.FunctionalExtensionality.
From Stdlib Require Import micromega.Lia.
Require Import ARTrie.Spec.DictionaryLawSpec.
Import ListNotations.

Definition DawgState := nat.
Definition DawgLabel := MapSpec.Byte.
Definition DawgTerm := MapSpec.Key.

Definition DawgStep := DawgState -> DawgLabel -> option DawgState.

Fixpoint dawg_walk_from
  (step : DawgStep) (state : DawgState) (term : DawgTerm)
  : option DawgState :=
  match term with
  | [] => Some state
  | label :: rest =>
      match step state label with
      | Some next => dawg_walk_from step next rest
      | None => None
      end
  end.

Record DynamicDawgGraph (V : Type) := {
  dd_root : DawgState;
  dd_step : DawgStep;
  dd_final : DawgState -> bool;
  dd_value : DawgState -> option V
}.

Definition graph_state_for {V : Type}
  (graph : DynamicDawgGraph V) (term : DawgTerm) : option DawgState :=
  dawg_walk_from (dd_step V graph) (dd_root V graph) term.

Definition graph_contains {V : Type}
  (graph : DynamicDawgGraph V) (term : DawgTerm) : bool :=
  match graph_state_for graph term with
  | Some state => dd_final V graph state
  | None => false
  end.

Definition graph_lookup {V : Type}
  (graph : DynamicDawgGraph V) (term : DawgTerm) : option V :=
  match graph_state_for graph term with
  | Some state =>
      if dd_final V graph state then dd_value V graph state else None
  | None => None
  end.

Definition graph_language {V : Type}
  (graph : DynamicDawgGraph V) : DictSet :=
  graph_contains graph.

Definition graph_map {V : Type}
  (graph : DynamicDawgGraph V) : DictMap V :=
  graph_lookup graph.

Definition valued_domain_consistent {V : Type}
  (graph : DynamicDawgGraph V) : Prop :=
  forall term,
    graph_contains graph term = true <->
    exists value, graph_lookup graph term = Some value.

Lemma valued_domain_consistent_dict_contains :
  forall (V : Type) (graph : DynamicDawgGraph V) term,
    valued_domain_consistent graph ->
    dict_contains V (graph_map graph) term = graph_contains graph term.
Proof.
  intros V graph term Hconsistent.
  unfold dict_contains, graph_map.
  destruct (graph_lookup graph term) as [value |] eqn:Hlookup;
    destruct (graph_contains graph term) eqn:Hcontains;
    try reflexivity.
  - exfalso.
    destruct (Hconsistent term) as [_ Hfrom_lookup].
    specialize (Hfrom_lookup (ex_intro _ value Hlookup)).
    rewrite Hcontains in Hfrom_lookup.
    discriminate.
  - exfalso.
    destruct (Hconsistent term) as [Hto_lookup _].
    destruct (Hto_lookup Hcontains) as [value Hvalue].
    rewrite Hlookup in Hvalue.
    discriminate.
Qed.

Section DynamicDawgReferenceMaps.

Variable V : Type.

Definition update_or_insert_map
  (m : DictMap V) (term : DawgTerm) (default : V) (update : V -> V)
  : DictMap V :=
  fun query =>
    if MapSpec.key_eq_dec term query then
      match m term with
      | Some old => Some (update old)
      | None => Some default
      end
    else m query.

Fixpoint insert_terms_set (terms : list DawgTerm) (s : DictSet) : DictSet :=
  match terms with
  | [] => s
  | term :: rest => insert_terms_set rest (set_insert s term)
  end.

Fixpoint delete_terms_set (terms : list DawgTerm) (s : DictSet) : DictSet :=
  match terms with
  | [] => s
  | term :: rest => delete_terms_set rest (set_remove s term)
  end.

Fixpoint delete_terms_map (terms : list DawgTerm) (m : DictMap V) : DictMap V :=
  match terms with
  | [] => m
  | term :: rest => delete_terms_map rest (dict_delete V m term)
  end.

Fixpoint count_new_terms (terms : list DawgTerm) (s : DictSet) : nat :=
  match terms with
  | [] => 0
  | term :: rest =>
      if set_contains s term then
        count_new_terms rest s
      else
        S (count_new_terms rest (set_insert s term))
  end.

Fixpoint count_removed_terms (terms : list DawgTerm) (s : DictSet) : nat :=
  match terms with
  | [] => 0
  | term :: rest =>
      if set_contains s term then
        S (count_removed_terms rest (set_remove s term))
      else
        count_removed_terms rest s
  end.

Lemma update_or_insert_lookup_same : forall m term default update,
  dict_lookup V (update_or_insert_map m term default update) term =
  match dict_lookup V m term with
  | Some old => Some (update old)
  | None => Some default
  end.
Proof.
  intros m term default update.
  unfold dict_lookup, update_or_insert_map.
  destruct (MapSpec.key_eq_dec term term) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Lemma update_or_insert_lookup_other : forall m term query default update,
  term <> query ->
  dict_lookup V (update_or_insert_map m term default update) query =
  dict_lookup V m query.
Proof.
  intros m term query default update Hneq.
  unfold dict_lookup, update_or_insert_map.
  destruct (MapSpec.key_eq_dec term query) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Lemma update_or_insert_contains_matches_set_insert :
  forall m term default update query,
    dict_contains V (update_or_insert_map m term default update) query =
    set_contains (set_insert (dict_domain V m) term) query.
Proof.
  intros m term default update query.
  unfold dict_contains, update_or_insert_map, set_contains, set_insert,
    dict_domain.
  destruct (MapSpec.key_eq_dec term query) as [Heq | Hneq].
  - destruct (m term); reflexivity.
  - unfold dict_contains.
    destruct (m query); reflexivity.
Qed.

Lemma insert_terms_set_preserves_present : forall terms s term,
  set_contains s term = true ->
  set_contains (insert_terms_set terms s) term = true.
Proof.
  induction terms as [| head rest IH]; intros s term Hpresent.
  - exact Hpresent.
  - simpl.
    apply IH.
    unfold set_contains, set_insert.
    destruct (MapSpec.key_eq_dec head term) as [_ | _].
    + reflexivity.
    + exact Hpresent.
Qed.

Lemma insert_terms_set_contains_in :
  forall terms s term,
    In term terms ->
    set_contains (insert_terms_set terms s) term = true.
Proof.
  induction terms as [| head rest IH]; intros s term Hin.
  - contradiction.
  - simpl in Hin.
    simpl.
    destruct Hin as [Heq | Hin].
    + subst.
      apply insert_terms_set_preserves_present.
      apply set_insert_contains_same.
    + apply IH.
      exact Hin.
Qed.

Lemma delete_terms_set_preserves_absent : forall terms s term,
  set_contains s term = false ->
  set_contains (delete_terms_set terms s) term = false.
Proof.
  induction terms as [| head rest IH]; intros s term Habsent.
  - exact Habsent.
  - simpl.
    apply IH.
    unfold set_contains, set_remove.
    destruct (MapSpec.key_eq_dec head term) as [_ | _].
    + reflexivity.
    + exact Habsent.
Qed.

Lemma delete_terms_set_removes_in :
  forall terms s term,
    In term terms ->
    set_contains (delete_terms_set terms s) term = false.
Proof.
  induction terms as [| head rest IH]; intros s term Hin.
  - contradiction.
  - simpl in Hin.
    simpl.
    destruct Hin as [Heq | Hin].
    + subst.
      apply delete_terms_set_preserves_absent.
      apply set_remove_contains_same.
    + apply IH.
      exact Hin.
Qed.

Lemma delete_terms_map_preserves_absent : forall terms (m : DictMap V) term,
  dict_lookup V m term = None ->
  dict_lookup V (delete_terms_map terms m) term = None.
Proof.
  induction terms as [| head rest IH]; intros m term Habsent.
  - exact Habsent.
  - simpl.
    apply IH.
    unfold dict_lookup, dict_delete.
    destruct (MapSpec.key_eq_dec head term) as [_ | _].
    + reflexivity.
    + exact Habsent.
Qed.

Lemma delete_terms_map_removes_in :
  forall terms (m : DictMap V) term,
    In term terms ->
    dict_lookup V (delete_terms_map terms m) term = None.
Proof.
  induction terms as [| head rest IH]; intros m term Hin.
  - contradiction.
  - simpl in Hin.
    simpl.
    destruct Hin as [Heq | Hin].
    + subst.
      apply delete_terms_map_preserves_absent.
      apply dict_lookup_delete_same.
    + apply IH.
      exact Hin.
Qed.

Lemma count_new_terms_zero_for_present : forall terms s,
  (forall term, In term terms -> set_contains s term = true) ->
  count_new_terms terms s = 0.
Proof.
  induction terms as [| head rest IH]; intros s Hall.
  - reflexivity.
  - simpl.
    rewrite Hall by (left; reflexivity).
    apply IH.
    intros term Hin.
    apply Hall.
    right.
    exact Hin.
Qed.

Lemma count_removed_terms_zero_for_absent : forall terms s,
  (forall term, In term terms -> set_contains s term = false) ->
  count_removed_terms terms s = 0.
Proof.
  induction terms as [| head rest IH]; intros s Hall.
  - reflexivity.
  - simpl.
    rewrite Hall by (left; reflexivity).
    apply IH.
    intros term Hin.
    apply Hall.
    right.
    exact Hin.
Qed.

End DynamicDawgReferenceMaps.

Section DynamicDawgOperationLaws.

Variable V : Type.

Variable Insert : DynamicDawgGraph V -> DawgTerm -> DynamicDawgGraph V.
Variable InsertWithValue : DynamicDawgGraph V -> DawgTerm -> V -> DynamicDawgGraph V.
Variable UpdateOrInsert :
  DynamicDawgGraph V -> DawgTerm -> V -> (V -> V) -> DynamicDawgGraph V.
Variable Remove : DynamicDawgGraph V -> DawgTerm -> DynamicDawgGraph V.
Variable Compact : DynamicDawgGraph V -> DynamicDawgGraph V.
Variable Minimize : DynamicDawgGraph V -> DynamicDawgGraph V.
Variable Extend : DynamicDawgGraph V -> list DawgTerm -> DynamicDawgGraph V.
Variable RemoveMany : DynamicDawgGraph V -> list DawgTerm -> DynamicDawgGraph V.

Variable InsertReturnsNew : DynamicDawgGraph V -> DawgTerm -> bool.
Variable InsertWithValueReturnsNew : DynamicDawgGraph V -> DawgTerm -> V -> bool.
Variable UpdateOrInsertReturnsNew :
  DynamicDawgGraph V -> DawgTerm -> V -> (V -> V) -> bool.
Variable RemoveReturnsPresent : DynamicDawgGraph V -> DawgTerm -> bool.
Variable ExtendReturnsAdded : DynamicDawgGraph V -> list DawgTerm -> nat.
Variable RemoveManyReturnsRemoved : DynamicDawgGraph V -> list DawgTerm -> nat.

Record DynamicDawgMutationLaws := {
  dd_insert_refines_set :
    forall graph term query,
      graph_contains (Insert graph term) query =
      set_contains (set_insert (graph_language graph) term) query;

  dd_insert_returns_new :
    forall graph term,
      InsertReturnsNew graph term =
      negb (graph_contains graph term);

  dd_insert_with_value_refines_map :
    forall graph term value query,
      graph_lookup (InsertWithValue graph term value) query =
      dict_lookup V (dict_insert V (graph_map graph) term value) query;

  dd_insert_with_value_refines_set :
    forall graph term value query,
      graph_contains (InsertWithValue graph term value) query =
      set_contains (set_insert (graph_language graph) term) query;

  dd_insert_with_value_returns_new :
    forall graph term value,
      InsertWithValueReturnsNew graph term value =
      negb (graph_contains graph term);

  dd_update_or_insert_refines_map :
    forall graph term default update query,
      graph_lookup (UpdateOrInsert graph term default update) query =
      dict_lookup V
        (update_or_insert_map V (graph_map graph) term default update)
        query;

  dd_update_or_insert_refines_set :
    forall graph term default update query,
      graph_contains (UpdateOrInsert graph term default update) query =
      set_contains (set_insert (graph_language graph) term) query;

  dd_update_or_insert_returns_new :
    forall graph term default update,
      UpdateOrInsertReturnsNew graph term default update =
      negb (graph_contains graph term);

  dd_remove_refines_set :
    forall graph term query,
      graph_contains (Remove graph term) query =
      set_contains (set_remove (graph_language graph) term) query;

  dd_remove_refines_map :
    forall graph term query,
      graph_lookup (Remove graph term) query =
      dict_lookup V (dict_delete V (graph_map graph) term) query;

  dd_remove_returns_present :
    forall graph term,
      RemoveReturnsPresent graph term = graph_contains graph term;

  dd_compact_preserves_set :
    forall graph query,
      graph_contains (Compact graph) query = graph_contains graph query;

  dd_compact_preserves_map :
    forall graph query,
      graph_lookup (Compact graph) query = graph_lookup graph query;

  dd_minimize_preserves_set :
    forall graph query,
      graph_contains (Minimize graph) query = graph_contains graph query;

  dd_minimize_preserves_map :
    forall graph query,
      graph_lookup (Minimize graph) query = graph_lookup graph query;

  dd_extend_refines_set :
    forall graph terms query,
      graph_contains (Extend graph terms) query =
      set_contains (insert_terms_set terms (graph_language graph)) query;

  dd_extend_returns_added :
    forall graph terms,
      ExtendReturnsAdded graph terms =
      count_new_terms terms (graph_language graph);

  dd_remove_many_refines_set :
    forall graph terms query,
      graph_contains (RemoveMany graph terms) query =
      set_contains (delete_terms_set terms (graph_language graph)) query;

  dd_remove_many_refines_map :
    forall graph terms query,
      graph_lookup (RemoveMany graph terms) query =
      dict_lookup V (delete_terms_map V terms (graph_map graph)) query;

  dd_remove_many_returns_removed :
    forall graph terms,
      RemoveManyReturnsRemoved graph terms =
      count_removed_terms terms (graph_language graph)
}.

Variable laws : DynamicDawgMutationLaws.

Theorem dynamic_insert_contains_same : forall graph term,
  graph_contains (Insert graph term) term = true.
Proof.
  intros graph term.
  rewrite (dd_insert_refines_set laws graph term term).
  apply set_insert_contains_same.
Qed.

Theorem dynamic_insert_contains_other : forall graph term query,
  term <> query ->
  graph_contains (Insert graph term) query = graph_contains graph query.
Proof.
  intros graph term query Hneq.
  rewrite (dd_insert_refines_set laws graph term query).
  apply set_insert_contains_other.
  exact Hneq.
Qed.

Theorem dynamic_insert_duplicate_return_false : forall graph term,
  graph_contains graph term = true ->
  InsertReturnsNew graph term = false.
Proof.
  intros graph term Hpresent.
  rewrite (dd_insert_returns_new laws graph term).
  rewrite Hpresent.
  reflexivity.
Qed.

Theorem dynamic_insert_absent_return_true : forall graph term,
  graph_contains graph term = false ->
  InsertReturnsNew graph term = true.
Proof.
  intros graph term Habsent.
  rewrite (dd_insert_returns_new laws graph term).
  rewrite Habsent.
  reflexivity.
Qed.

Theorem dynamic_insert_with_value_lookup_same : forall graph term value,
  graph_lookup (InsertWithValue graph term value) term = Some value.
Proof.
  intros graph term value.
  rewrite (dd_insert_with_value_refines_map laws graph term value term).
  apply dict_lookup_insert_same.
Qed.

Theorem dynamic_insert_with_value_lookup_other : forall graph term query value,
  term <> query ->
  graph_lookup (InsertWithValue graph term value) query = graph_lookup graph query.
Proof.
  intros graph term query value Hneq.
  rewrite (dd_insert_with_value_refines_map laws graph term value query).
  apply dict_lookup_insert_other.
  exact Hneq.
Qed.

Theorem dynamic_insert_with_value_contains_same : forall graph term value,
  graph_contains (InsertWithValue graph term value) term = true.
Proof.
  intros graph term value.
  rewrite (dd_insert_with_value_refines_set laws graph term value term).
  apply set_insert_contains_same.
Qed.

Theorem dynamic_insert_with_value_overwrites : forall graph term old new query,
  graph_lookup
    (InsertWithValue (InsertWithValue graph term old) term new)
    query =
  graph_lookup (InsertWithValue graph term new) query.
Proof.
  intros graph term old new query.
  rewrite (dd_insert_with_value_refines_map
    laws (InsertWithValue graph term old) term new query).
  rewrite (dd_insert_with_value_refines_map laws graph term new query).
  unfold graph_map, dict_lookup, dict_insert.
  destruct (MapSpec.key_eq_dec term query) as [_ | Hneq].
  - reflexivity.
  - rewrite (dd_insert_with_value_refines_map laws graph term old query).
    unfold dict_lookup, dict_insert.
    destruct (MapSpec.key_eq_dec term query) as [Heq | _].
    + contradiction.
    + reflexivity.
Qed.

Theorem dynamic_update_or_insert_lookup_same : forall graph term default update,
  graph_lookup (UpdateOrInsert graph term default update) term =
  match graph_lookup graph term with
  | Some old => Some (update old)
  | None => Some default
  end.
Proof.
  intros graph term default update.
  rewrite (dd_update_or_insert_refines_map laws graph term default update term).
  apply update_or_insert_lookup_same.
Qed.

Theorem dynamic_update_or_insert_lookup_other :
  forall graph term query default update,
    term <> query ->
    graph_lookup (UpdateOrInsert graph term default update) query =
    graph_lookup graph query.
Proof.
  intros graph term query default update Hneq.
  rewrite (dd_update_or_insert_refines_map laws graph term default update query).
  apply update_or_insert_lookup_other.
  exact Hneq.
Qed.

Theorem dynamic_update_or_insert_contains_same :
  forall graph term default update,
    graph_contains (UpdateOrInsert graph term default update) term = true.
Proof.
  intros graph term default update.
  rewrite (dd_update_or_insert_refines_set laws graph term default update term).
  apply set_insert_contains_same.
Qed.

Theorem dynamic_remove_contains_same : forall graph term,
  graph_contains (Remove graph term) term = false.
Proof.
  intros graph term.
  rewrite (dd_remove_refines_set laws graph term term).
  apply set_remove_contains_same.
Qed.

Theorem dynamic_remove_contains_other : forall graph term query,
  term <> query ->
  graph_contains (Remove graph term) query = graph_contains graph query.
Proof.
  intros graph term query Hneq.
  rewrite (dd_remove_refines_set laws graph term query).
  apply set_remove_contains_other.
  exact Hneq.
Qed.

Theorem dynamic_remove_lookup_same : forall graph term,
  graph_lookup (Remove graph term) term = None.
Proof.
  intros graph term.
  rewrite (dd_remove_refines_map laws graph term term).
  apply dict_lookup_delete_same.
Qed.

Theorem dynamic_remove_lookup_other : forall graph term query,
  term <> query ->
  graph_lookup (Remove graph term) query = graph_lookup graph query.
Proof.
  intros graph term query Hneq.
  rewrite (dd_remove_refines_map laws graph term query).
  apply dict_lookup_delete_other.
  exact Hneq.
Qed.

Theorem dynamic_remove_present_return_true : forall graph term,
  graph_contains graph term = true ->
  RemoveReturnsPresent graph term = true.
Proof.
  intros graph term Hpresent.
  rewrite (dd_remove_returns_present laws graph term).
  exact Hpresent.
Qed.

Theorem dynamic_remove_absent_return_false : forall graph term,
  graph_contains graph term = false ->
  RemoveReturnsPresent graph term = false.
Proof.
  intros graph term Habsent.
  rewrite (dd_remove_returns_present laws graph term).
  exact Habsent.
Qed.

Theorem dynamic_compact_preserves_contains : forall graph query,
  graph_contains (Compact graph) query = graph_contains graph query.
Proof.
  intros graph query.
  apply (dd_compact_preserves_set laws).
Qed.

Theorem dynamic_compact_preserves_lookup : forall graph query,
  graph_lookup (Compact graph) query = graph_lookup graph query.
Proof.
  intros graph query.
  apply (dd_compact_preserves_map laws).
Qed.

Theorem dynamic_minimize_preserves_contains : forall graph query,
  graph_contains (Minimize graph) query = graph_contains graph query.
Proof.
  intros graph query.
  apply (dd_minimize_preserves_set laws).
Qed.

Theorem dynamic_minimize_preserves_lookup : forall graph query,
  graph_lookup (Minimize graph) query = graph_lookup graph query.
Proof.
  intros graph query.
  apply (dd_minimize_preserves_map laws).
Qed.

Theorem dynamic_compact_minimize_commute_semantically : forall graph query,
  graph_contains (Compact (Minimize graph)) query =
  graph_contains (Minimize (Compact graph)) query.
Proof.
  intros graph query.
  rewrite (dd_compact_preserves_set laws).
  rewrite (dd_minimize_preserves_set laws).
  rewrite (dd_minimize_preserves_set laws).
  rewrite (dd_compact_preserves_set laws).
  reflexivity.
Qed.

Theorem dynamic_extend_contains_inserted : forall graph terms term,
  In term terms ->
  graph_contains (Extend graph terms) term = true.
Proof.
  intros graph terms term Hin.
  rewrite (dd_extend_refines_set laws graph terms term).
  apply insert_terms_set_contains_in.
  exact Hin.
Qed.

Theorem dynamic_extend_all_present_returns_zero : forall graph terms,
  (forall term, In term terms -> graph_contains graph term = true) ->
  ExtendReturnsAdded graph terms = 0.
Proof.
  intros graph terms Hall.
  rewrite (dd_extend_returns_added laws graph terms).
  apply count_new_terms_zero_for_present.
  exact Hall.
Qed.

Theorem dynamic_remove_many_removes_listed : forall graph terms term,
  In term terms ->
  graph_contains (RemoveMany graph terms) term = false.
Proof.
  intros graph terms term Hin.
  rewrite (dd_remove_many_refines_set laws graph terms term).
  apply delete_terms_set_removes_in.
  exact Hin.
Qed.

Theorem dynamic_remove_many_all_absent_returns_zero : forall graph terms,
  (forall term, In term terms -> graph_contains graph term = false) ->
  RemoveManyReturnsRemoved graph terms = 0.
Proof.
  intros graph terms Hall.
  rewrite (dd_remove_many_returns_removed laws graph terms).
  apply count_removed_terms_zero_for_absent.
  exact Hall.
Qed.

Theorem dynamic_remove_many_lookup_deleted : forall graph terms term,
  In term terms ->
  graph_lookup (RemoveMany graph terms) term = None.
Proof.
  intros graph terms term Hin.
  rewrite (dd_remove_many_refines_map laws graph terms term).
  apply delete_terms_map_removes_in.
  exact Hin.
Qed.

Theorem dynamic_insert_with_value_domain_matches_set :
  forall graph term value query,
    valued_domain_consistent graph ->
    dict_contains V (graph_map (InsertWithValue graph term value)) query =
    set_contains (set_insert (graph_language graph) term) query.
Proof.
  intros graph term value query Hconsistent.
  unfold dict_contains, graph_map.
  rewrite (dd_insert_with_value_refines_map laws graph term value query).
  destruct (MapSpec.key_eq_dec term query) as [Heq | Hneq].
  - subst.
    rewrite dict_lookup_insert_same.
    symmetry.
    apply set_insert_contains_same.
  - rewrite dict_lookup_insert_other by exact Hneq.
    rewrite set_insert_contains_other by exact Hneq.
    change (dict_contains V (graph_map graph) query = graph_contains graph query).
    apply valued_domain_consistent_dict_contains.
    exact Hconsistent.
Qed.

Theorem dynamic_replay_domain_after_mutations :
  forall ops graph initial_set query,
    (forall term,
      dict_contains V (graph_map graph) term =
      set_contains initial_set term) ->
    dict_contains V (replay_map V ops (graph_map graph)) query =
    set_contains (replay_set V ops initial_set) query.
Proof.
  intros ops graph initial_set query Hdomain.
  apply replay_domain_matches_set.
  exact Hdomain.
Qed.

End DynamicDawgOperationLaws.
