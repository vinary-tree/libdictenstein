(** * Residual-language laws for the freeze-once sorted DAWG builder

    A deterministic dictionary state denotes the language of suffixes
    accepted from that state. The builder in [src/dynamic_dawg/lockfree.rs]
    interns only nodes with equal finality and equal ordered child identities.
    Such a merge preserves the suffix language. Conversely, prefixes with
    different suffix languages must reach different states in every correct
    deterministic dictionary. Together these laws justify the independent
    residual-language node-count oracle in
    [tests/sorted_bulk_builder_properties.rs].

    This model covers the unvalued graph. The Rust builder keeps valued nodes
    distinct because arbitrary dictionary values have no equality contract.
*)

From Coq Require Import Lists.List.
From Coq Require Import Sorting.Permutation.
Import ListNotations.

Section DeterministicDictionary.

Variable State : Type.
Variable root : State.
Variable step : State -> nat -> option State.
Variable final : State -> Prop.

Fixpoint walk (state : State) (word : list nat) : option State :=
  match word with
  | [] => Some state
  | symbol :: suffix =>
      match step state symbol with
      | Some child => walk child suffix
      | None => None
      end
  end.

Definition accepts_from (state : State) (word : list nat) : Prop :=
  match walk state word with
  | Some reached => final reached
  | None => False
  end.

Definition recognizes (terms : list (list nat)) : Prop :=
  forall word, accepts_from root word <-> In word terms.

Definition equal_signature (left right : State) : Prop :=
  (final left <-> final right) /\
  (forall symbol, step left symbol = step right symbol).

Lemma equal_signature_preserves_right_language :
  forall left right suffix,
    equal_signature left right ->
    (accepts_from left suffix <-> accepts_from right suffix).
Proof.
  intros left right suffix [Hfinal Hsteps].
  destruct suffix as [| symbol rest].
  - simpl. exact Hfinal.
  - unfold accepts_from. simpl.
    rewrite Hsteps. reflexivity.
Qed.

Section ValueSafety.

Variable Value : Type.
Variable node_value : State -> option Value.

Definition lookup_from (state : State) (word : list nat) : option Value :=
  match walk state word with
  | Some reached => node_value reached
  | None => None
  end.

Lemma merging_only_valueless_equal_signatures_preserves_lookup :
  forall left right suffix,
    equal_signature left right ->
    node_value left = None ->
    node_value right = None ->
    lookup_from left suffix = lookup_from right suffix.
Proof.
  intros left right suffix [_ Hsteps] Hleft Hright.
  destruct suffix as [| symbol rest].
  - unfold lookup_from. simpl. now rewrite Hleft, Hright.
  - unfold lookup_from. simpl. now rewrite Hsteps.
Qed.

End ValueSafety.

Lemma walk_append :
  forall state prefix suffix,
    walk state (prefix ++ suffix) =
    match walk state prefix with
    | Some reached => walk reached suffix
    | None => None
    end.
Proof.
  intros state prefix.
  revert state.
  induction prefix as [| symbol rest IH]; intros state suffix.
  - reflexivity.
  - simpl. destruct (step state symbol) as [child |] eqn:Hstep.
    + apply IH.
    + reflexivity.
Qed.

Theorem different_residuals_require_different_states :
  forall terms left_prefix right_prefix left_state right_state suffix,
    recognizes terms ->
    walk root left_prefix = Some left_state ->
    walk root right_prefix = Some right_state ->
    ~ (In (left_prefix ++ suffix) terms <->
       In (right_prefix ++ suffix) terms) ->
    left_state <> right_state.
Proof.
  intros terms left_prefix right_prefix left_state right_state suffix
    Hrecognizes Hleft Hright Hdifferent Hequal.
  subst right_state.
  apply Hdifferent.
  assert (Hsame :
    accepts_from root (left_prefix ++ suffix) <->
    accepts_from root (right_prefix ++ suffix)).
  { unfold accepts_from.
    rewrite !walk_append, Hleft, Hright.
    reflexivity. }
  split; intro Hin.
  - apply (proj1 (Hrecognizes _)).
    apply (proj1 Hsame).
    apply (proj2 (Hrecognizes _)). exact Hin.
  - apply (proj1 (Hrecognizes _)).
    apply (proj2 Hsame).
    apply (proj2 (Hrecognizes _)). exact Hin.
Qed.

End DeterministicDictionary.

Lemma unordered_bulk_sort_preserves_language :
  forall (before after : list (list nat)),
    Permutation before after ->
    forall word, In word before <-> In word after.
Proof.
  intros before after Hpermutation word.
  split; intro Hin.
  - eapply Permutation_in; eauto.
  - eapply Permutation_in; [symmetry; exact Hpermutation | exact Hin].
Qed.

Lemma duplicate_term_preserves_language :
  forall (terms : list (list nat)) term word,
    In word (term :: term :: terms) <-> In word (term :: terms).
Proof.
  intros terms term word.
  simpl. tauto.
Qed.
