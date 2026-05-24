(** * DictionaryLawSpec: Public Dictionary API Laws

    This module states the law surface expected from libdictenstein's public
    dictionary traits. It is deliberately backend-neutral: exact dictionaries
    refine finite-set membership, mapped dictionaries refine partial maps, set
    zippers refine Boolean set algebra, mutation traces refine reference-map
    replay, and bijective dictionaries refine a pair of inverse partial maps.
*)

Require Import Coq.Lists.List.
Require Import Coq.Bool.Bool.
Require Import Coq.Logic.FunctionalExtensionality.
Require Import ARTrie.Spec.MapSpec.
Import ListNotations.

(** ** Exact Dictionary Laws *)

Definition DictSet := MapSpec.Key -> bool.

Definition set_empty : DictSet := fun _ => false.

Definition set_contains (s : DictSet) (k : MapSpec.Key) : bool := s k.

Definition set_insert (s : DictSet) (k : MapSpec.Key) : DictSet :=
  fun q => if MapSpec.key_eq_dec k q then true else s q.

Definition set_remove (s : DictSet) (k : MapSpec.Key) : DictSet :=
  fun q => if MapSpec.key_eq_dec k q then false else s q.

Definition set_union (a b : DictSet) : DictSet :=
  fun k => orb (a k) (b k).

Definition set_intersection (a b : DictSet) : DictSet :=
  fun k => andb (a k) (b k).

Definition set_difference (a b : DictSet) : DictSet :=
  fun k => andb (a k) (negb (b k)).

Definition set_symmetric_difference (a b : DictSet) : DictSet :=
  fun k => xorb (a k) (b k).

Theorem set_empty_contains_none : forall k,
  set_contains set_empty k = false.
Proof.
  reflexivity.
Qed.

Theorem set_insert_contains_same : forall s k,
  set_contains (set_insert s k) k = true.
Proof.
  intros s k.
  unfold set_contains, set_insert.
  destruct (MapSpec.key_eq_dec k k) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem set_insert_contains_other : forall s k q,
  k <> q ->
  set_contains (set_insert s k) q = set_contains s q.
Proof.
  intros s k q Hneq.
  unfold set_contains, set_insert.
  destruct (MapSpec.key_eq_dec k q) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem set_remove_contains_same : forall s k,
  set_contains (set_remove s k) k = false.
Proof.
  intros s k.
  unfold set_contains, set_remove.
  destruct (MapSpec.key_eq_dec k k) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem set_remove_contains_other : forall s k q,
  k <> q ->
  set_contains (set_remove s k) q = set_contains s q.
Proof.
  intros s k q Hneq.
  unfold set_contains, set_remove.
  destruct (MapSpec.key_eq_dec k q) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem set_insert_idempotent : forall s k,
  set_insert (set_insert s k) k = set_insert s k.
Proof.
  intros s k.
  apply functional_extensionality.
  intros q.
  unfold set_insert.
  destruct (MapSpec.key_eq_dec k q); reflexivity.
Qed.

Theorem set_remove_idempotent : forall s k,
  set_remove (set_remove s k) k = set_remove s k.
Proof.
  intros s k.
  apply functional_extensionality.
  intros q.
  unfold set_remove.
  destruct (MapSpec.key_eq_dec k q); reflexivity.
Qed.

Theorem set_union_contains : forall a b k,
  set_contains (set_union a b) k =
  orb (set_contains a k) (set_contains b k).
Proof.
  reflexivity.
Qed.

Theorem set_intersection_contains : forall a b k,
  set_contains (set_intersection a b) k =
  andb (set_contains a k) (set_contains b k).
Proof.
  reflexivity.
Qed.

Theorem set_difference_contains : forall a b k,
  set_contains (set_difference a b) k =
  andb (set_contains a k) (negb (set_contains b k)).
Proof.
  reflexivity.
Qed.

Theorem set_symmetric_difference_contains : forall a b k,
  set_contains (set_symmetric_difference a b) k =
  xorb (set_contains a k) (set_contains b k).
Proof.
  reflexivity.
Qed.

Theorem set_union_commutative : forall a b,
  set_union a b = set_union b a.
Proof.
  intros a b.
  apply functional_extensionality.
  intros k.
  unfold set_union.
  apply orb_comm.
Qed.

Theorem set_intersection_commutative : forall a b,
  set_intersection a b = set_intersection b a.
Proof.
  intros a b.
  apply functional_extensionality.
  intros k.
  unfold set_intersection.
  apply andb_comm.
Qed.

Theorem set_union_idempotent : forall a,
  set_union a a = a.
Proof.
  intros a.
  apply functional_extensionality.
  intros k.
  unfold set_union.
  destruct (a k); reflexivity.
Qed.

Theorem set_intersection_idempotent : forall a,
  set_intersection a a = a.
Proof.
  intros a.
  apply functional_extensionality.
  intros k.
  unfold set_intersection.
  destruct (a k); reflexivity.
Qed.

Theorem set_difference_self_empty : forall a,
  set_difference a a = set_empty.
Proof.
  intros a.
  apply functional_extensionality.
  intros k.
  unfold set_difference, set_empty.
  destruct (a k); reflexivity.
Qed.

Theorem set_symmetric_difference_self_empty : forall a,
  set_symmetric_difference a a = set_empty.
Proof.
  intros a.
  apply functional_extensionality.
  intros k.
  unfold set_symmetric_difference, set_empty.
  destruct (a k); reflexivity.
Qed.

Theorem set_symmetric_difference_as_union_of_differences : forall a b,
  set_symmetric_difference a b =
  set_union (set_difference a b) (set_difference b a).
Proof.
  intros a b.
  apply functional_extensionality.
  intros k.
  unfold set_symmetric_difference, set_union, set_difference.
  destruct (a k), (b k); reflexivity.
Qed.

(** ** Mapped Dictionary Laws *)

Section MappedDictionaryLaws.

Variable V : Type.

Definition DictMap := MapSpec.Key -> option V.

Definition dict_empty : DictMap := fun _ => None.

Definition dict_lookup (m : DictMap) (k : MapSpec.Key) : option V := m k.

Definition dict_insert (m : DictMap) (k : MapSpec.Key) (v : V) : DictMap :=
  fun q => if MapSpec.key_eq_dec k q then Some v else m q.

Definition dict_delete (m : DictMap) (k : MapSpec.Key) : DictMap :=
  fun q => if MapSpec.key_eq_dec k q then None else m q.

Definition dict_contains (m : DictMap) (k : MapSpec.Key) : bool :=
  match m k with
  | Some _ => true
  | None => false
  end.

Definition dict_domain (m : DictMap) : DictSet := dict_contains m.

Theorem dict_empty_lookup_none : forall k,
  dict_lookup dict_empty k = None.
Proof.
  reflexivity.
Qed.

Theorem dict_lookup_insert_same : forall m k v,
  dict_lookup (dict_insert m k v) k = Some v.
Proof.
  intros m k v.
  unfold dict_lookup, dict_insert.
  destruct (MapSpec.key_eq_dec k k) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem dict_lookup_insert_other : forall m k q v,
  k <> q ->
  dict_lookup (dict_insert m k v) q = dict_lookup m q.
Proof.
  intros m k q v Hneq.
  unfold dict_lookup, dict_insert.
  destruct (MapSpec.key_eq_dec k q) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem dict_lookup_delete_same : forall m k,
  dict_lookup (dict_delete m k) k = None.
Proof.
  intros m k.
  unfold dict_lookup, dict_delete.
  destruct (MapSpec.key_eq_dec k k) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem dict_lookup_delete_other : forall m k q,
  k <> q ->
  dict_lookup (dict_delete m k) q = dict_lookup m q.
Proof.
  intros m k q Hneq.
  unfold dict_lookup, dict_delete.
  destruct (MapSpec.key_eq_dec k q) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem dict_contains_lookup : forall m k,
  dict_contains m k = true <-> exists v, dict_lookup m k = Some v.
Proof.
  intros m k.
  unfold dict_contains, dict_lookup.
  split.
  - intros H.
    destruct (m k) as [v |] eqn:Hmk.
    + exists v. reflexivity.
    + discriminate.
  - intros [v Hv].
    rewrite Hv. reflexivity.
Qed.

Theorem dict_contains_insert_matches_set_insert : forall m k v q,
  dict_contains (dict_insert m k v) q =
  set_contains (set_insert (dict_domain m) k) q.
Proof.
  intros m k v q.
  unfold dict_contains, dict_insert, set_contains, set_insert, dict_domain.
  destruct (MapSpec.key_eq_dec k q) as [_ | _].
  - reflexivity.
  - unfold dict_contains.
    destruct (m q); reflexivity.
Qed.

Theorem dict_contains_delete_matches_set_remove : forall m k q,
  dict_contains (dict_delete m k) q =
  set_contains (set_remove (dict_domain m) k) q.
Proof.
  intros m k q.
  unfold dict_contains, dict_delete, set_contains, set_remove, dict_domain.
  destruct (MapSpec.key_eq_dec k q) as [_ | _].
  - reflexivity.
  - unfold dict_contains.
    destruct (m q); reflexivity.
Qed.

Theorem dict_insert_overwrite : forall m k v1 v2,
  dict_insert (dict_insert m k v1) k v2 = dict_insert m k v2.
Proof.
  intros m k v1 v2.
  apply functional_extensionality.
  intros q.
  unfold dict_insert.
  destruct (MapSpec.key_eq_dec k q); reflexivity.
Qed.

Theorem dict_delete_insert_absent : forall m k v,
  dict_lookup m k = None ->
  dict_delete (dict_insert m k v) k = m.
Proof.
  intros m k v Habsent.
  apply functional_extensionality.
  intros q.
  unfold dict_delete, dict_insert, dict_lookup in *.
  destruct (MapSpec.key_eq_dec k q) as [Heq | Hneq].
  - subst. symmetry. exact Habsent.
  - reflexivity.
Qed.

Inductive DictOp :=
| Put : MapSpec.Key -> V -> DictOp
| Delete : MapSpec.Key -> DictOp.

Definition apply_map_op (op : DictOp) (m : DictMap) : DictMap :=
  match op with
  | Put k v => dict_insert m k v
  | Delete k => dict_delete m k
  end.

Definition apply_set_op (op : DictOp) (s : DictSet) : DictSet :=
  match op with
  | Put k _ => set_insert s k
  | Delete k => set_remove s k
  end.

Fixpoint replay_map (ops : list DictOp) (m : DictMap) : DictMap :=
  match ops with
  | [] => m
  | op :: rest => replay_map rest (apply_map_op op m)
  end.

Fixpoint replay_set (ops : list DictOp) (s : DictSet) : DictSet :=
  match ops with
  | [] => s
  | op :: rest => replay_set rest (apply_set_op op s)
  end.

Theorem replay_domain_matches_set : forall ops m s,
  (forall k, dict_contains m k = set_contains s k) ->
  forall k,
    dict_contains (replay_map ops m) k =
    set_contains (replay_set ops s) k.
Proof.
  induction ops as [| op ops IH]; simpl; intros m s Hdom k.
  - apply Hdom.
  - apply IH.
    destruct op as [put_key value | del_key];
      unfold apply_map_op, apply_set_op;
      intros q.
    + unfold dict_contains, dict_insert, set_contains, set_insert.
      destruct (MapSpec.key_eq_dec put_key q) as [_ | _].
      * reflexivity.
      * apply Hdom.
    + unfold dict_contains, dict_delete, set_contains, set_remove.
      destruct (MapSpec.key_eq_dec del_key q) as [_ | _].
      * reflexivity.
      * apply Hdom.
Qed.

End MappedDictionaryLaws.

(** ** Bijective Dictionary Laws *)

Section BijectiveDictionaryLaws.

Variable V : Type.

Definition ForwardMap := MapSpec.Key -> option V.
Definition ReverseMap := V -> option MapSpec.Key.

Definition forward_contains (f : ForwardMap) (k : MapSpec.Key) : bool :=
  match f k with
  | Some _ => true
  | None => false
  end.

Definition reverse_contains (r : ReverseMap) (v : V) : bool :=
  match r v with
  | Some _ => true
  | None => false
  end.

Definition reverse_refines_forward (f : ForwardMap) (r : ReverseMap) : Prop :=
  forall k v, f k = Some v -> r v = Some k.

Definition forward_refines_reverse (f : ForwardMap) (r : ReverseMap) : Prop :=
  forall v k, r v = Some k -> f k = Some v.

Definition bijective_refinement (f : ForwardMap) (r : ReverseMap) : Prop :=
  reverse_refines_forward f r /\ forward_refines_reverse f r.

Definition forward_injective (f : ForwardMap) : Prop :=
  forall k1 k2 v, f k1 = Some v -> f k2 = Some v -> k1 = k2.

Theorem bijective_refinement_forward_injective : forall f r,
  bijective_refinement f r ->
  forward_injective f.
Proof.
  intros f r [Hreverse _] k1 k2 v Hk1 Hk2.
  pose proof (Hreverse k1 v Hk1) as Hr1.
  pose proof (Hreverse k2 v Hk2) as Hr2.
  rewrite Hr1 in Hr2.
  inversion Hr2.
  reflexivity.
Qed.

Theorem bijective_forward_reverse_roundtrip : forall f r k v,
  bijective_refinement f r ->
  f k = Some v ->
  r v = Some k.
Proof.
  intros f r k v [Hreverse _] Hlookup.
  apply Hreverse.
  exact Hlookup.
Qed.

Theorem bijective_reverse_forward_roundtrip : forall f r v k,
  bijective_refinement f r ->
  r v = Some k ->
  f k = Some v.
Proof.
  intros f r v k [_ Hforward] Hlookup.
  apply Hforward.
  exact Hlookup.
Qed.

Theorem bijective_contains_value_if_contains_term : forall f r k v,
  bijective_refinement f r ->
  f k = Some v ->
  reverse_contains r v = true.
Proof.
  intros f r k v Hbij Hlookup.
  unfold reverse_contains.
  rewrite (bijective_forward_reverse_roundtrip f r k v Hbij Hlookup).
  reflexivity.
Qed.

Theorem bijective_contains_term_if_contains_value : forall f r v k,
  bijective_refinement f r ->
  r v = Some k ->
  forward_contains f k = true.
Proof.
  intros f r v k Hbij Hlookup.
  unfold forward_contains.
  rewrite (bijective_reverse_forward_roundtrip f r v k Hbij Hlookup).
  reflexivity.
Qed.

End BijectiveDictionaryLaws.
