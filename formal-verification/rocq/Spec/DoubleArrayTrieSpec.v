(** * DoubleArrayTrieSpec: BASE/CHECK Construction and Traversal Laws

    This module states the implementation-shaped proof boundary for
    libdictenstein's byte and Unicode Double-Array Trie backends.

    The model is generic in the edge label type.  The byte DAT instantiates the
    root state with 1 and an 8-bit offset function; the char DAT instantiates
    the root state with 0 and a Unicode scalar offset function.  Both backends
    share the same BASE/CHECK transition rule.
*)

Require Import Coq.Lists.List.
Require Import Coq.Bool.Bool.
Require Import Coq.Arith.PeanoNat.
Require Import Coq.Logic.FunctionalExtensionality.
Require Import Coq.Sorting.Permutation.
Require Import Coq.micromega.Lia.
Import ListNotations.

Section DoubleArrayTrieModel.

Variable Label : Type.
Variable Value : Type.
Variable label_eq_dec : forall (x y : Label), {x = y} + {x <> y}.
Variable label_offset : Label -> nat.

Definition Key := list Label.
Definition DictSet := Key -> bool.
Definition DictMap := Key -> option Value.

Fixpoint key_eq_dec (a b : Key) : {a = b} + {a <> b}.
Proof.
  decide equality.
Defined.

(** ** Reference set/map normalization *)

Definition set_empty : DictSet := fun _ => false.

Definition set_insert (s : DictSet) (k : Key) : DictSet :=
  fun q => if key_eq_dec k q then true else s q.

Fixpoint set_from_terms (terms : list Key) : DictSet :=
  match terms with
  | [] => set_empty
  | term :: rest => set_insert (set_from_terms rest) term
  end.

Definition map_empty : DictMap := fun _ => None.

Definition map_insert (m : DictMap) (k : Key) (v : Value) : DictMap :=
  fun q => if key_eq_dec k q then Some v else m q.

Definition map_domain (m : DictMap) : DictSet :=
  fun k =>
    match m k with
    | Some _ => true
    | None => false
    end.

Fixpoint map_from_entries (entries : list (Key * Value)) : DictMap :=
  match entries with
  | [] => map_empty
  | (term, value) :: rest =>
      let tail := map_from_entries rest in
      fun q =>
        match tail q with
        | Some later => Some later
        | None => if key_eq_dec term q then Some value else None
        end
  end.

Lemma set_insert_idempotent : forall s k,
  set_insert (set_insert s k) k = set_insert s k.
Proof.
  intros s k.
  apply functional_extensionality.
  intros q.
  unfold set_insert.
  destruct (key_eq_dec k q); reflexivity.
Qed.

Lemma set_insert_commutative : forall s a b,
  set_insert (set_insert s a) b =
  set_insert (set_insert s b) a.
Proof.
  intros s a b.
  apply functional_extensionality.
  intros q.
  unfold set_insert.
  destruct (key_eq_dec b q) as [Hb | Hb];
    destruct (key_eq_dec a q) as [Ha | Ha];
    subst; try reflexivity.
Qed.

Theorem duplicate_terms_collapse : forall term rest,
  set_from_terms (term :: term :: rest) =
  set_from_terms (term :: rest).
Proof.
  intros term rest.
  simpl.
  apply set_insert_idempotent.
Qed.

Theorem set_from_terms_permutation : forall xs ys,
  Permutation xs ys ->
  set_from_terms xs = set_from_terms ys.
Proof.
  intros xs ys Hperm.
  induction Hperm.
  - reflexivity.
  - simpl. rewrite IHHperm. reflexivity.
  - simpl. apply set_insert_commutative.
  - transitivity (set_from_terms l'); assumption.
Qed.

Lemma map_insert_same_overwrites : forall m k old new,
  map_insert (map_insert m k old) k new =
  map_insert m k new.
Proof.
  intros m k old new.
  apply functional_extensionality.
  intros q.
  unfold map_insert.
  destruct (key_eq_dec k q); reflexivity.
Qed.

Lemma map_from_entries_append_insert : forall entries k v,
  map_from_entries (entries ++ [(k, v)]) =
  map_insert (map_from_entries entries) k v.
Proof.
  induction entries as [| [term value] rest IH]; intros k v.
  - simpl.
    apply functional_extensionality.
    intros q.
    unfold map_insert, map_empty.
    destruct (key_eq_dec k q); reflexivity.
  - simpl.
    apply functional_extensionality.
    intros q.
    rewrite IH.
    unfold map_insert.
    destruct (key_eq_dec k q) as [Heq | Hneq].
    + reflexivity.
    + simpl.
      destruct (map_from_entries rest q) as [later |] eqn:Htail.
      * reflexivity.
      * destruct (key_eq_dec term q); reflexivity.
Qed.

Theorem map_from_entries_last_wins : forall entries k v,
  map_from_entries (entries ++ [(k, v)]) k = Some v.
Proof.
  intros entries k v.
  rewrite map_from_entries_append_insert.
  unfold map_insert.
  destruct (key_eq_dec k k) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem adjacent_duplicate_entry_keeps_later_value : forall entries k old new,
  map_from_entries (entries ++ [(k, old); (k, new)]) =
  map_from_entries (entries ++ [(k, new)]).
Proof.
  intros entries k old new.
  replace (entries ++ [(k, old); (k, new)])
    with ((entries ++ [(k, old)]) ++ [(k, new)])
    by (rewrite <- app_assoc; reflexivity).
  repeat rewrite map_from_entries_append_insert.
  apply map_insert_same_overwrites.
Qed.

(** ** BASE/CHECK array model *)

Record DoubleArrayTrie := {
  dat_base : list (option nat);
  dat_check : list (option nat);
  dat_final : list bool;
  dat_value : list (option Value);
  dat_edges : list (list Label);
  dat_root_state : nat
}.

Definition base_at (t : DoubleArrayTrie) (state : nat) : option nat :=
  match nth_error (dat_base t) state with
  | Some base => base
  | None => None
  end.

Definition check_at (t : DoubleArrayTrie) (state : nat) : option nat :=
  match nth_error (dat_check t) state with
  | Some parent => parent
  | None => None
  end.

Definition final_at (t : DoubleArrayTrie) (state : nat) : bool :=
  nth state (dat_final t) false.

Definition value_at (t : DoubleArrayTrie) (state : nat) : option Value :=
  match nth_error (dat_value t) state with
  | Some value => value
  | None => None
  end.

Definition edges_at (t : DoubleArrayTrie) (state : nat) : list Label :=
  match nth_error (dat_edges t) state with
  | Some labels => labels
  | None => []
  end.

Definition dat_transition
  (t : DoubleArrayTrie) (state : nat) (label : Label) : option nat :=
  match base_at t state with
  | Some base =>
      let next := base + label_offset label in
      match check_at t next with
      | Some parent =>
          if Nat.eq_dec parent state then Some next else None
      | None => None
      end
  | None => None
  end.

Fixpoint dat_walk
  (t : DoubleArrayTrie) (state : nat) (key : Key) : option nat :=
  match key with
  | [] => Some state
  | label :: rest =>
      match dat_transition t state label with
      | Some next => dat_walk t next rest
      | None => None
      end
  end.

Definition dat_root_walk (t : DoubleArrayTrie) (key : Key) : option nat :=
  dat_walk t (dat_root_state t) key.

Definition dat_contains (t : DoubleArrayTrie) (key : Key) : bool :=
  match dat_root_walk t key with
  | Some state => final_at t state
  | None => false
  end.

Definition dat_lookup (t : DoubleArrayTrie) (key : Key) : option Value :=
  match dat_root_walk t key with
  | Some state =>
      if final_at t state then value_at t state else None
  | None => None
  end.

Lemma dat_walk_app : forall t prefix suffix state,
  dat_walk t state (prefix ++ suffix) =
  match dat_walk t state prefix with
  | Some mid => dat_walk t mid suffix
  | None => None
  end.
Proof.
  intros t prefix.
  induction prefix as [| label rest IH]; intros suffix state.
  - reflexivity.
  - simpl.
    destruct (dat_transition t state label) as [next |] eqn:Hnext.
    + exact (IH suffix next).
    + reflexivity.
Qed.

Theorem dat_transition_parent_checked : forall t state label child,
  dat_transition t state label = Some child ->
  check_at t child = Some state.
Proof.
  intros t state label child Htransition.
  unfold dat_transition in Htransition.
  destruct (base_at t state) as [base |] eqn:Hbase; [| discriminate].
  destruct (check_at t (base + label_offset label)) as [parent |] eqn:Hcheck;
    [| discriminate].
  destruct (Nat.eq_dec parent state) as [Heq | Hneq]; [| discriminate].
  inversion Htransition; subst.
  rewrite Hcheck.
  reflexivity.
Qed.

Theorem dat_transition_uses_base_offset : forall t state label child base,
  base_at t state = Some base ->
  dat_transition t state label = Some child ->
  child = base + label_offset label.
Proof.
  intros t state label child base Hbase Htransition.
  unfold dat_transition in Htransition.
  rewrite Hbase in Htransition.
  destruct (check_at t (base + label_offset label)) as [parent |] eqn:Hcheck;
    [| discriminate].
  destruct (Nat.eq_dec parent state) as [_ | Hneq]; [| discriminate].
  inversion Htransition.
  reflexivity.
Qed.

Theorem dat_walk_cons_transition : forall t state label rest next final_state,
  dat_transition t state label = Some next ->
  dat_walk t next rest = Some final_state ->
  dat_walk t state (label :: rest) = Some final_state.
Proof.
  intros t state label rest next final_state Htransition Hwalk.
  simpl.
  rewrite Htransition.
  exact Hwalk.
Qed.

Theorem dat_zipper_descend_matches_transition :
  forall t path label state child,
    dat_root_walk t path = Some state ->
    dat_transition t state label = Some child ->
    dat_root_walk t (path ++ [label]) = Some child.
Proof.
  intros t path label state child Hpath Htransition.
  unfold dat_root_walk in *.
  rewrite dat_walk_app.
  rewrite Hpath.
  simpl.
  rewrite Htransition.
  reflexivity.
Qed.

Theorem dat_zipper_final_matches_contains :
  forall t path state,
    dat_root_walk t path = Some state ->
    dat_contains t path = final_at t state.
Proof.
  intros t path state Hpath.
  unfold dat_contains.
  rewrite Hpath.
  reflexivity.
Qed.

(** ** Public lookup/traversal law records *)

Record DATSetRefinement (t : DoubleArrayTrie) (reference : DictSet) := {
  dat_contains_refines_set : forall key,
    dat_contains t key = reference key
}.

Record DATMapRefinement (t : DoubleArrayTrie) (reference : DictMap) := {
  dat_lookup_refines_map : forall key,
    dat_lookup t key = reference key;
  dat_contains_refines_domain : forall key,
    dat_contains t key = map_domain reference key
}.

Record DATEdgeLaws (t : DoubleArrayTrie) := {
  dat_edges_sound :
    forall state label,
      In label (edges_at t state) ->
      exists child, dat_transition t state label = Some child;
  dat_edges_complete :
    forall state label child,
      dat_transition t state label = Some child ->
      In label (edges_at t state);
  dat_edges_no_duplicates :
    forall state, NoDup (edges_at t state)
}.

Section SetRefinementTheorems.

Variable t : DoubleArrayTrie.
Variable reference : DictSet.
Variable refinement : DATSetRefinement t reference.

Theorem dat_contains_iff_reference : forall key,
  dat_contains t key = true <-> reference key = true.
Proof.
  intro key.
  rewrite (dat_contains_refines_set t reference refinement key).
  split; intro H; exact H.
Qed.

Theorem dat_rejects_absent_reference : forall key,
  reference key = false ->
  dat_contains t key = false.
Proof.
  intros key Habsent.
  rewrite (dat_contains_refines_set t reference refinement key).
  exact Habsent.
Qed.

Theorem dat_accepts_present_reference : forall key,
  reference key = true ->
  dat_contains t key = true.
Proof.
  intros key Hpresent.
  rewrite (dat_contains_refines_set t reference refinement key).
  exact Hpresent.
Qed.

End SetRefinementTheorems.

Section MapRefinementTheorems.

Variable t : DoubleArrayTrie.
Variable reference : DictMap.
Variable refinement : DATMapRefinement t reference.

Theorem dat_lookup_matches_reference : forall key,
  dat_lookup t key = reference key.
Proof.
  intro key.
  exact (dat_lookup_refines_map t reference refinement key).
Qed.

Theorem dat_get_value_sound : forall key value,
  dat_lookup t key = Some value ->
  reference key = Some value.
Proof.
  intros key value Hlookup.
  rewrite <- (dat_lookup_refines_map t reference refinement key).
  exact Hlookup.
Qed.

Theorem dat_get_value_complete : forall key value,
  reference key = Some value ->
  dat_lookup t key = Some value.
Proof.
  intros key value Hreference.
  rewrite (dat_lookup_refines_map t reference refinement key).
  exact Hreference.
Qed.

Theorem dat_contains_matches_reference_domain : forall key,
  dat_contains t key = map_domain reference key.
Proof.
  intro key.
  exact (dat_contains_refines_domain t reference refinement key).
Qed.

Theorem dat_contains_when_lookup_some : forall key value,
  dat_lookup t key = Some value ->
  dat_contains t key = true.
Proof.
  intros key value Hlookup.
  rewrite (dat_contains_refines_domain t reference refinement key).
  unfold map_domain.
  rewrite <- (dat_lookup_refines_map t reference refinement key).
  rewrite Hlookup.
  reflexivity.
Qed.

Theorem dat_lookup_none_when_domain_absent : forall key,
  map_domain reference key = false ->
  dat_lookup t key = None.
Proof.
  intros key Habsent.
  destruct (dat_lookup t key) as [value |] eqn:Hlookup; [| reflexivity].
  pose proof (dat_contains_when_lookup_some key value Hlookup) as Hcontains.
  rewrite (dat_contains_refines_domain t reference refinement key) in Hcontains.
  rewrite Habsent in Hcontains.
  discriminate.
Qed.

End MapRefinementTheorems.

Section EdgeLawTheorems.

Variable t : DoubleArrayTrie.
Variable edge_laws : DATEdgeLaws t.

Theorem dat_child_iteration_sound : forall state label,
  In label (edges_at t state) ->
  exists child, dat_transition t state label = Some child.
Proof.
  intros state label Hin.
  exact (dat_edges_sound t edge_laws state label Hin).
Qed.

Theorem dat_child_iteration_complete : forall state label child,
  dat_transition t state label = Some child ->
  In label (edges_at t state).
Proof.
  intros state label child Htransition.
  exact (dat_edges_complete t edge_laws state label child Htransition).
Qed.

Theorem dat_child_labels_no_duplicates : forall state,
  NoDup (edges_at t state).
Proof.
  intro state.
  exact (dat_edges_no_duplicates t edge_laws state).
Qed.

Theorem dat_child_iteration_checked_parent : forall state label child,
  In label (edges_at t state) ->
  dat_transition t state label = Some child ->
  check_at t child = Some state.
Proof.
  intros state label child _ Htransition.
  exact (dat_transition_parent_checked t state label child Htransition).
Qed.

End EdgeLawTheorems.

End DoubleArrayTrieModel.
