(** * ValuedSetCombinatorSpec: Zipper Value Merge Laws

    This module states the backend-neutral semantic boundary for valued
    set-combinator zippers.  ZipperLanguageSpec proves the accepted-language
    side of union/intersection/difference combinators; this file covers the
    duplicate-value conflict semantics used by UnionZipper and
    IntersectionZipper.

    The checked claim is value-map equality against an ordered reference fold:
    values are collected from dictionaries in caller-provided dictionary order,
    then merged left-to-right by the configured ValueMergeStrategy.
*)

From Stdlib Require Import Arith.PeanoNat.
From Stdlib Require Import Lists.List.
Require Import ARTrie.Spec.MapSpec.
Import ListNotations.

Definition Key := MapSpec.Key.

Definition ValueMap (V : Type) := Key -> option V.

Definition same_map {V : Type} (a b : ValueMap V) : Prop :=
  forall key, a key = b key.

Section ValuedSetCombinators.

Variable V : Type.

Definition MergeStrategy := V -> V -> V.

Fixpoint present_values (maps : list (ValueMap V)) (key : Key) : list V :=
  match maps with
  | [] => []
  | m :: rest =>
      match m key with
      | Some value => value :: present_values rest key
      | None => present_values rest key
      end
  end.

Definition fold_merge_value (merge : MergeStrategy) (values : list V) : option V :=
  match values with
  | [] => None
  | head :: tail => Some (fold_left merge tail head)
  end.

Definition valued_union_map
  (merge : MergeStrategy)
  (maps : list (ValueMap V)) : ValueMap V :=
  fun key => fold_merge_value merge (present_values maps key).

Definition valued_intersection_map
  (merge : MergeStrategy)
  (maps : list (ValueMap V)) : ValueMap V :=
  fun key =>
    let values := present_values maps key in
    if Nat.eqb (length values) (length maps)
    then fold_merge_value merge values
    else None.

Definition first_wins (existing _new : V) : V := existing.

Definition last_wins (_existing new : V) : V := new.

Lemma fold_merge_value_none_iff : forall merge values,
  fold_merge_value merge values = None <-> values = [].
Proof.
  intros merge values.
  destruct values as [| head tail].
  - split; intros _; reflexivity.
  - split.
    + intros H. discriminate H.
    + intros H. discriminate H.
Qed.

Lemma fold_merge_value_cons : forall merge head tail,
  fold_merge_value merge (head :: tail) = Some (fold_left merge tail head).
Proof.
  intros merge head tail.
  reflexivity.
Qed.

Theorem valued_union_empty_list : forall merge key,
  valued_union_map merge [] key = None.
Proof.
  intros merge key.
  reflexivity.
Qed.

Theorem valued_intersection_empty_list : forall merge key,
  valued_intersection_map merge [] key = None.
Proof.
  intros merge key.
  reflexivity.
Qed.

Theorem valued_union_two_domain : forall merge left right key,
  valued_union_map merge [left; right] key <> None <->
  (exists left_value, left key = Some left_value) \/
  (exists right_value, right key = Some right_value).
Proof.
  intros merge left right key.
  unfold valued_union_map.
  simpl.
  destruct (left key) as [left_value |] eqn:Hleft;
    destruct (right key) as [right_value |] eqn:Hright; simpl.
  - split.
    + intros _. left. exists left_value. reflexivity.
    + intros _. discriminate.
  - split.
    + intros _. left. exists left_value. reflexivity.
    + intros _. discriminate.
  - split.
    + intros _. right. exists right_value. reflexivity.
    + intros _. discriminate.
  - split.
    + intros H. exfalso. apply H. reflexivity.
    + intros [[v Hv] | [v Hv]]; congruence.
Qed.

Theorem valued_intersection_two_domain : forall merge left right key,
  valued_intersection_map merge [left; right] key <> None <->
  exists left_value right_value,
    left key = Some left_value /\ right key = Some right_value.
Proof.
  intros merge left right key.
  unfold valued_intersection_map.
  simpl.
  destruct (left key) as [left_value |] eqn:Hleft;
    destruct (right key) as [right_value |] eqn:Hright; simpl.
  - split.
    + intros _. exists left_value, right_value. split; reflexivity.
    + intros _. discriminate.
  - split.
    + intros H. exfalso. apply H. reflexivity.
    + intros [lv [rv [_ Hright_some]]]. congruence.
  - split.
    + intros H. exfalso. apply H. reflexivity.
    + intros [lv [rv [Hleft_some _]]]. congruence.
  - split.
    + intros H. exfalso. apply H. reflexivity.
    + intros [lv [rv [Hleft_some _]]]. congruence.
Qed.

Theorem valued_union_two_left_only : forall merge left right key value,
  left key = Some value ->
  right key = None ->
  valued_union_map merge [left; right] key = Some value.
Proof.
  intros merge left right key value Hleft Hright.
  unfold valued_union_map.
  simpl.
  rewrite Hleft, Hright.
  reflexivity.
Qed.

Theorem valued_union_two_right_only : forall merge left right key value,
  left key = None ->
  right key = Some value ->
  valued_union_map merge [left; right] key = Some value.
Proof.
  intros merge left right key value Hleft Hright.
  unfold valued_union_map.
  simpl.
  rewrite Hleft, Hright.
  reflexivity.
Qed.

Theorem valued_union_two_conflict_custom : forall merge left right key old new,
  left key = Some old ->
  right key = Some new ->
  valued_union_map merge [left; right] key = Some (merge old new).
Proof.
  intros merge left right key old new Hleft Hright.
  unfold valued_union_map.
  simpl.
  rewrite Hleft, Hright.
  reflexivity.
Qed.

Theorem valued_union_three_conflict_order : forall merge first second third key a b c,
  first key = Some a ->
  second key = Some b ->
  third key = Some c ->
  valued_union_map merge [first; second; third] key =
    Some (merge (merge a b) c).
Proof.
  intros merge first second third key a b c Hfirst Hsecond Hthird.
  unfold valued_union_map.
  simpl.
  rewrite Hfirst, Hsecond, Hthird.
  reflexivity.
Qed.

Theorem valued_union_two_first_wins_conflict : forall left right key old new,
  left key = Some old ->
  right key = Some new ->
  valued_union_map first_wins [left; right] key = Some old.
Proof.
  intros left right key old new Hleft Hright.
  unfold valued_union_map.
  simpl.
  rewrite Hleft, Hright.
  reflexivity.
Qed.

Theorem valued_union_two_last_wins_conflict : forall left right key old new,
  left key = Some old ->
  right key = Some new ->
  valued_union_map last_wins [left; right] key = Some new.
Proof.
  intros left right key old new Hleft Hright.
  unfold valued_union_map.
  simpl.
  rewrite Hleft, Hright.
  reflexivity.
Qed.

Theorem valued_intersection_two_present : forall merge left right key old new,
  left key = Some old ->
  right key = Some new ->
  valued_intersection_map merge [left; right] key = Some (merge old new).
Proof.
  intros merge left right key old new Hleft Hright.
  unfold valued_intersection_map.
  simpl.
  rewrite Hleft, Hright.
  reflexivity.
Qed.

Theorem valued_intersection_two_left_absent : forall merge left right key,
  left key = None ->
  valued_intersection_map merge [left; right] key = None.
Proof.
  intros merge left right key Hleft.
  unfold valued_intersection_map.
  simpl.
  rewrite Hleft.
  destruct (right key); reflexivity.
Qed.

Theorem valued_intersection_two_right_absent : forall merge left right key,
  right key = None ->
  valued_intersection_map merge [left; right] key = None.
Proof.
  intros merge left right key Hright.
  unfold valued_intersection_map.
  simpl.
  destruct (left key); rewrite Hright; reflexivity.
Qed.

Theorem valued_intersection_three_conflict_order :
  forall merge first second third key a b c,
    first key = Some a ->
    second key = Some b ->
    third key = Some c ->
    valued_intersection_map merge [first; second; third] key =
      Some (merge (merge a b) c).
Proof.
  intros merge first second third key a b c Hfirst Hsecond Hthird.
  unfold valued_intersection_map.
  simpl.
  rewrite Hfirst, Hsecond, Hthird.
  reflexivity.
Qed.

Record LatticeLaws (join meet : MergeStrategy) : Prop := {
  join_idempotent : forall a, join a a = a;
  join_commutative : forall a b, join a b = join b a;
  join_associative : forall a b c, join (join a b) c = join a (join b c);
  meet_idempotent : forall a, meet a a = a;
  meet_commutative : forall a b, meet a b = meet b a;
  meet_associative : forall a b c, meet (meet a b) c = meet a (meet b c);
  join_meet_absorption : forall a b, join a (meet a b) = a;
  meet_join_absorption : forall a b, meet a (join a b) = a
}.

Section LatticeMerge.

Variable join meet : MergeStrategy.
Variable laws : LatticeLaws join meet.

Theorem lattice_join_two_order_independent : forall a b,
  join a b = join b a.
Proof.
  intros a b.
  exact (join_commutative join meet laws a b).
Qed.

Theorem lattice_meet_two_order_independent : forall a b,
  meet a b = meet b a.
Proof.
  intros a b.
  exact (meet_commutative join meet laws a b).
Qed.

Theorem lattice_join_three_associates : forall a b c,
  join (join a b) c = join a (join b c).
Proof.
  intros a b c.
  exact (join_associative join meet laws a b c).
Qed.

Theorem lattice_meet_three_associates : forall a b c,
  meet (meet a b) c = meet a (meet b c).
Proof.
  intros a b c.
  exact (meet_associative join meet laws a b c).
Qed.

Theorem lattice_join_absorbs_meet : forall a b,
  join a (meet a b) = a.
Proof.
  intros a b.
  exact (join_meet_absorption join meet laws a b).
Qed.

Theorem lattice_meet_absorbs_join : forall a b,
  meet a (join a b) = a.
Proof.
  intros a b.
  exact (meet_join_absorption join meet laws a b).
Qed.

Theorem valued_union_lattice_join_conflict_commutes :
  forall left right key left_value right_value,
    left key = Some left_value ->
    right key = Some right_value ->
    valued_union_map join [left; right] key =
    valued_union_map join [right; left] key.
Proof.
  intros left right key left_value right_value Hleft Hright.
  rewrite (valued_union_two_conflict_custom join left right key
    left_value right_value Hleft Hright).
  rewrite (valued_union_two_conflict_custom join right left key
    right_value left_value Hright Hleft).
  f_equal.
  exact (join_commutative join meet laws left_value right_value).
Qed.

Theorem valued_intersection_lattice_meet_conflict_commutes :
  forall left right key left_value right_value,
    left key = Some left_value ->
    right key = Some right_value ->
    valued_intersection_map meet [left; right] key =
    valued_intersection_map meet [right; left] key.
Proof.
  intros left right key left_value right_value Hleft Hright.
  rewrite (valued_intersection_two_present meet left right key
    left_value right_value Hleft Hright).
  rewrite (valued_intersection_two_present meet right left key
    right_value left_value Hright Hleft).
  f_equal.
  exact (meet_commutative join meet laws left_value right_value).
Qed.

Theorem valued_union_lattice_join_three_associates :
  forall first second third key a b c,
    first key = Some a ->
    second key = Some b ->
    third key = Some c ->
    valued_union_map join [first; second; third] key =
      Some (join a (join b c)).
Proof.
  intros first second third key a b c Hfirst Hsecond Hthird.
  rewrite (valued_union_three_conflict_order join first second third key
    a b c Hfirst Hsecond Hthird).
  f_equal.
  exact (join_associative join meet laws a b c).
Qed.

Theorem valued_intersection_lattice_meet_three_associates :
  forall first second third key a b c,
    first key = Some a ->
    second key = Some b ->
    third key = Some c ->
    valued_intersection_map meet [first; second; third] key =
      Some (meet a (meet b c)).
Proof.
  intros first second third key a b c Hfirst Hsecond Hthird.
  rewrite (valued_intersection_three_conflict_order meet first second third key
    a b c Hfirst Hsecond Hthird).
  f_equal.
  exact (meet_associative join meet laws a b c).
Qed.

End LatticeMerge.

Section SemiringJoinBoundary.

Variable plus times : MergeStrategy.

Record IdempotentSemiringJoinLaws : Prop := {
  semiring_plus_idempotent : forall a, plus a a = a;
  semiring_plus_commutative : forall a b, plus a b = plus b a;
  semiring_plus_associative : forall a b c, plus (plus a b) c = plus a (plus b c)
}.

Definition semiring_join_merge : MergeStrategy := plus.

Definition semiring_times_operational_merge : MergeStrategy := times.

Variable semiring_laws : IdempotentSemiringJoinLaws.

Theorem semiring_join_two_conflict : forall left right key old new,
  left key = Some old ->
  right key = Some new ->
  valued_union_map semiring_join_merge [left; right] key =
    Some (plus old new).
Proof.
  intros left right key old new Hleft Hright.
  unfold semiring_join_merge.
  apply valued_union_two_conflict_custom; assumption.
Qed.

Theorem semiring_join_conflict_commutes : forall left right key old new,
  left key = Some old ->
  right key = Some new ->
  valued_union_map semiring_join_merge [left; right] key =
  valued_union_map semiring_join_merge [right; left] key.
Proof.
  intros left right key old new Hleft Hright.
  rewrite (semiring_join_two_conflict left right key old new Hleft Hright).
  rewrite (semiring_join_two_conflict right left key new old Hright Hleft).
  f_equal.
  exact (semiring_plus_commutative semiring_laws old new).
Qed.

Theorem semiring_join_three_associates : forall first second third key a b c,
  first key = Some a ->
  second key = Some b ->
  third key = Some c ->
  valued_union_map semiring_join_merge [first; second; third] key =
    Some (plus a (plus b c)).
Proof.
  intros first second third key a b c Hfirst Hsecond Hthird.
  unfold semiring_join_merge.
  rewrite (valued_union_three_conflict_order plus first second third key
    a b c Hfirst Hsecond Hthird).
  f_equal.
  exact (semiring_plus_associative semiring_laws a b c).
Qed.

Theorem semiring_times_boundary_is_operational : forall a b,
  semiring_times_operational_merge a b = times a b.
Proof.
  intros a b.
  reflexivity.
Qed.

End SemiringJoinBoundary.

End ValuedSetCombinators.
