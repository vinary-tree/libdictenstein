(** * ZipperLanguageSpec: Traversal-Language Laws

    This module states the backend-neutral law surface for dictionary zippers.
    A zipper refines a reference language when descent, child iteration,
    finality, and valued lookup agree with the accepted paths of that language.
*)

Require Import Coq.Lists.List.
Require Import ARTrie.Spec.MapSpec.
Import ListNotations.

Definition Label := MapSpec.Byte.
Definition Path := MapSpec.Key.
Definition Language := Path -> Prop.

Definition accepts (l : Language) (p : Path) : Prop := l p.

Definition empty_language : Language := fun _ => False.

Definition singleton_language (p : Path) : Language :=
  fun q => p = q.

Definition language_union (a b : Language) : Language :=
  fun p => a p \/ b p.

Definition language_intersection (a b : Language) : Language :=
  fun p => a p /\ b p.

Definition language_difference (a b : Language) : Language :=
  fun p => a p /\ ~ b p.

Definition language_symmetric_difference (a b : Language) : Language :=
  fun p => (a p /\ ~ b p) \/ (b p /\ ~ a p).

Definition starts_with (prefix path : Path) : Prop :=
  exists suffix, path = prefix ++ suffix.

Definition language_with_prefix (l : Language) (prefix : Path) : Language :=
  fun p => l p /\ starts_with prefix p.

Definition excludes_prefixes (prefixes : list Path) (p : Path) : Prop :=
  forall prefix, In prefix prefixes -> ~ starts_with prefix p.

Definition language_excluding_prefixes
  (l : Language) (prefixes : list Path) : Language :=
  fun p => l p /\ excludes_prefixes prefixes p.

Definition same_language (a b : Language) : Prop :=
  forall p, a p <-> b p.

Definition live_prefix (l : Language) (prefix : Path) : Prop :=
  exists accepted suffix, l accepted /\ accepted = prefix ++ suffix.

Section ZipperLaws.

Variable l : Language.
Variable Descend : Path -> Label -> Path -> Prop.
Variable Children : Path -> Label -> Path -> Prop.
Variable Final : Path -> Prop.

Record ZipperLaws := {
  descend_sound :
    forall p label q,
      Descend p label q ->
      q = p ++ [label] /\ live_prefix l q;
  descend_complete :
    forall p label,
      live_prefix l (p ++ [label]) ->
      Descend p label (p ++ [label]);
  children_sound :
    forall p label q,
      Children p label q -> Descend p label q;
  children_complete :
    forall p label q,
      Descend p label q -> Children p label q;
  final_sound :
    forall p, Final p -> accepts l p;
  final_complete :
    forall p, accepts l p -> Final p
}.

Variable laws : ZipperLaws.

Theorem descend_path_exact : forall p label q,
  Descend p label q -> q = p ++ [label].
Proof.
  intros p label q Hdesc.
  destruct (descend_sound laws p label q Hdesc) as [Hpath _].
  exact Hpath.
Qed.

Theorem descend_preserves_live_prefix : forall p label q,
  Descend p label q -> live_prefix l q.
Proof.
  intros p label q Hdesc.
  destruct (descend_sound laws p label q Hdesc) as [_ Hlive].
  exact Hlive.
Qed.

Theorem child_path_exact : forall p label q,
  Children p label q -> q = p ++ [label].
Proof.
  intros p label q Hchild.
  apply descend_path_exact.
  exact (children_sound laws p label q Hchild).
Qed.

Theorem child_iff_descend : forall p label q,
  Children p label q <-> Descend p label q.
Proof.
  intros p label q.
  split.
  - intros Hchild.
    exact (children_sound laws p label q Hchild).
  - intros Hdesc.
    exact (children_complete laws p label q Hdesc).
Qed.

Theorem final_iff_accepts : forall p,
  Final p <-> accepts l p.
Proof.
  intros p.
  split.
  - intros Hfinal.
    exact (final_sound laws p Hfinal).
  - intros Haccepts.
    exact (final_complete laws p Haccepts).
Qed.

End ZipperLaws.

Section ValuedZipperLaws.

Variable Value : Type.

Definition ValueMap := Path -> option Value.

Variable values : ValueMap.
Variable ValueAt : Path -> option Value.

Record ValuedZipperLaws := {
  value_at_matches_map : forall p, ValueAt p = values p
}.

Variable valued_laws : ValuedZipperLaws.

Theorem valued_lookup_equiv : forall p,
  ValueAt p = values p.
Proof.
  intros p.
  exact (value_at_matches_map valued_laws p).
Qed.

End ValuedZipperLaws.

Theorem language_union_contains : forall a b p,
  language_union a b p <-> a p \/ b p.
Proof.
  intros a b p. reflexivity.
Qed.

Theorem language_intersection_contains : forall a b p,
  language_intersection a b p <-> a p /\ b p.
Proof.
  intros a b p. reflexivity.
Qed.

Theorem language_difference_contains : forall a b p,
  language_difference a b p <-> a p /\ ~ b p.
Proof.
  intros a b p. reflexivity.
Qed.

Theorem language_symmetric_difference_contains : forall a b p,
  language_symmetric_difference a b p <->
  (a p /\ ~ b p) \/ (b p /\ ~ a p).
Proof.
  intros a b p. reflexivity.
Qed.

Theorem language_union_commutative : forall a b,
  same_language (language_union a b) (language_union b a).
Proof.
  intros a b p.
  split; intros H; destruct H as [H | H].
  - right. exact H.
  - left. exact H.
  - right. exact H.
  - left. exact H.
Qed.

Theorem language_intersection_commutative : forall a b,
  same_language (language_intersection a b) (language_intersection b a).
Proof.
  intros a b p.
  split; intros [Ha Hb]; split; assumption.
Qed.

Theorem language_difference_self_empty : forall a,
  same_language (language_difference a a) empty_language.
Proof.
  intros a p.
  split.
  - intros [Ha Hnot]. contradiction.
  - intros Hfalse. contradiction.
Qed.

Theorem language_symmetric_difference_self_empty : forall a,
  same_language (language_symmetric_difference a a) empty_language.
Proof.
  intros a p.
  split.
  - intros [[Ha Hnot] | [Ha Hnot]]; contradiction.
  - intros Hfalse. contradiction.
Qed.

Theorem language_symmetric_difference_as_differences : forall a b,
  same_language
    (language_symmetric_difference a b)
    (language_union (language_difference a b) (language_difference b a)).
Proof.
  intros a b p.
  reflexivity.
Qed.

Theorem prefix_language_sound : forall l prefix p,
  language_with_prefix l prefix p -> l p /\ starts_with prefix p.
Proof.
  intros l prefix p H.
  exact H.
Qed.

Theorem prefix_language_complete : forall l prefix p,
  l p -> starts_with prefix p -> language_with_prefix l prefix p.
Proof.
  intros l prefix p Haccept Hprefix.
  split; assumption.
Qed.

Theorem excluding_language_sound : forall l prefixes p,
  language_excluding_prefixes l prefixes p ->
  l p /\ excludes_prefixes prefixes p.
Proof.
  intros l prefixes p H.
  exact H.
Qed.

Theorem excluding_language_complete : forall l prefixes p,
  l p ->
  excludes_prefixes prefixes p ->
  language_excluding_prefixes l prefixes p.
Proof.
  intros l prefixes p Haccept Hexcludes.
  split; assumption.
Qed.
