(** * MapSpec: Abstract Map Specification

    This module defines the abstract specification for a key-value map.
    The ARTrie implementation must refine this specification.

    The specification uses Coq's standard library and provides:
    - Type definitions for keys and values
    - Abstract map operations (lookup, insert, delete)
    - Map invariants and properties
    - Algebraic laws that all implementations must satisfy
*)

Require Import Coq.Lists.List.
Require Import Coq.Arith.Arith.
Require Import Coq.Bool.Bool.
Require Import Coq.Logic.FunctionalExtensionality.
Import ListNotations.

(** ** Proof Irrelevance Axiom *)
(* We assume proof irrelevance for simplifying Byte equality proofs *)
Axiom proof_irrelevance : forall (P : Prop) (p1 p2 : P), p1 = p2.

(** ** Key and Value Types *)

(** Keys are byte sequences (list of nat bounded to 0-255) *)
Definition Byte := {n : nat | n < 256}.

Definition byte_of_nat (n : nat) (H : n < 256) : Byte := exist _ n H.

Definition byte_to_nat (b : Byte) : nat := proj1_sig b.

Definition Key := list Byte.

(** ** Key Equality *)

(** Byte equality is decidable *)
Lemma byte_eq_dec : forall (b1 b2 : Byte), {b1 = b2} + {b1 <> b2}.
Proof.
  intros [n1 H1] [n2 H2].
  destruct (Nat.eq_dec n1 n2).
  - left. subst. f_equal. apply proof_irrelevance.
  - right. intro H. injection H. auto.
Defined.

(** Key equality is decidable (sumbool version for if-then-else) *)
Fixpoint key_eq_dec (k1 k2 : Key) : {k1 = k2} + {k1 <> k2}.
Proof.
  destruct k1 as [| b1 t1]; destruct k2 as [| b2 t2].
  - left. reflexivity.
  - right. discriminate.
  - right. discriminate.
  - destruct (byte_eq_dec b1 b2) as [Heq | Hneq].
    + destruct (key_eq_dec t1 t2) as [Heqt | Hneqt].
      * left. subst. reflexivity.
      * right. intro H. injection H. auto.
    + right. intro H. injection H. auto.
Defined.

(** Boolean version for propositional reasoning *)
Definition key_eqb (k1 k2 : Key) : bool :=
  if key_eq_dec k1 k2 then true else false.

(** Values can be any type, parameterized *)
Section MapSpec.

Variable Value : Type.

(** Decidable equality for values *)
Variable Value_eq_dec : forall (v1 v2 : Value), {v1 = v2} + {v1 <> v2}.

(** ** Abstract Map Type *)

(** A map is abstractly a partial function from keys to values *)
Definition Map := Key -> option Value.

(** Empty map *)
Definition empty_map : Map := fun _ => None.

(** ** Core Operations *)

(** Lookup: retrieve value for a key *)
Definition map_lookup (m : Map) (k : Key) : option Value := m k.

(** Insert: associate a key with a value *)
Definition map_insert (m : Map) (k : Key) (v : Value) : Map :=
  fun k' => if key_eq_dec k k' then Some v else m k'.

(** Delete: remove a key from the map *)
Definition map_delete (m : Map) (k : Key) : Map :=
  fun k' => if key_eq_dec k k' then None else m k'.

(** Contains: check if a key exists *)
Definition map_contains (m : Map) (k : Key) : bool :=
  match m k with
  | Some _ => true
  | None => false
  end.

(** ** Map Algebraic Laws *)

(** Lookup after insert of same key returns the inserted value *)
Theorem map_lookup_insert_same : forall m k v,
  map_lookup (map_insert m k v) k = Some v.
Proof.
  intros m k v.
  unfold map_lookup, map_insert.
  destruct (key_eq_dec k k) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

(** Lookup after insert of different key returns original value *)
Theorem map_lookup_insert_other : forall m k1 k2 v,
  k1 <> k2 -> map_lookup (map_insert m k1 v) k2 = map_lookup m k2.
Proof.
  intros m k1 k2 v Hneq.
  unfold map_lookup, map_insert.
  destruct (key_eq_dec k1 k2) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

(** Lookup after delete of same key returns None *)
Theorem map_lookup_delete_same : forall m k,
  map_lookup (map_delete m k) k = None.
Proof.
  intros m k.
  unfold map_lookup, map_delete.
  destruct (key_eq_dec k k) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

(** Lookup after delete of different key returns original value *)
Theorem map_lookup_delete_other : forall m k1 k2,
  k1 <> k2 -> map_lookup (map_delete m k1) k2 = map_lookup m k2.
Proof.
  intros m k1 k2 Hneq.
  unfold map_lookup, map_delete.
  destruct (key_eq_dec k1 k2) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

(** Insert is idempotent *)
Theorem map_insert_idempotent : forall m k v,
  map_insert (map_insert m k v) k v = map_insert m k v.
Proof.
  intros m k v.
  apply functional_extensionality.
  intros k'.
  unfold map_insert.
  destruct (key_eq_dec k k'); reflexivity.
Qed.

(** Insert commutes for different keys *)
Theorem map_insert_commute : forall m k1 k2 v1 v2,
  k1 <> k2 ->
  map_insert (map_insert m k1 v1) k2 v2 = map_insert (map_insert m k2 v2) k1 v1.
Proof.
  intros m k1 k2 v1 v2 Hneq.
  apply functional_extensionality.
  intros k.
  unfold map_insert.
  destruct (key_eq_dec k2 k) as [Heq2 | Hneq2];
  destruct (key_eq_dec k1 k) as [Heq1 | Hneq1];
  try reflexivity.
  - subst. contradiction.
Qed.

(** Delete is idempotent *)
Theorem map_delete_idempotent : forall m k,
  map_delete (map_delete m k) k = map_delete m k.
Proof.
  intros m k.
  apply functional_extensionality.
  intros k'.
  unfold map_delete.
  destruct (key_eq_dec k k'); reflexivity.
Qed.

(** Delete commutes *)
Theorem map_delete_commute : forall m k1 k2,
  map_delete (map_delete m k1) k2 = map_delete (map_delete m k2) k1.
Proof.
  intros m k1 k2.
  apply functional_extensionality.
  intros k.
  unfold map_delete.
  destruct (key_eq_dec k1 k);
  destruct (key_eq_dec k2 k);
  reflexivity.
Qed.

(** Insert then delete same key is a no-op if key didn't exist *)
Theorem map_delete_insert_absent : forall m k v,
  map_lookup m k = None ->
  map_delete (map_insert m k v) k = m.
Proof.
  intros m k v Habsent.
  apply functional_extensionality.
  intros k'.
  unfold map_delete, map_insert.
  destruct (key_eq_dec k k') as [Heq | Hneq].
  - subst. symmetry. assumption.
  - reflexivity.
Qed.

(** Contains is consistent with lookup *)
Theorem map_contains_lookup : forall m k,
  map_contains m k = true <-> exists v, map_lookup m k = Some v.
Proof.
  intros m k.
  unfold map_contains, map_lookup.
  split.
  - intros H. destruct (m k) eqn:Hmk.
    + exists v. reflexivity.
    + discriminate.
  - intros [v Hv]. rewrite Hv. reflexivity.
Qed.

(** ** Map Size *)

(** Size of a map (requires finite key domain for computation) *)
(* For a general map, we define size as a property rather than computable *)
Definition map_size_prop (m : Map) (n : nat) : Prop :=
  exists keys : list Key,
    NoDup keys /\
    (forall k, In k keys <-> map_contains m k = true) /\
    length keys = n.

(** Empty map has size 0 *)
Theorem empty_map_size : map_size_prop empty_map 0.
Proof.
  exists [].
  split; [constructor |].
  split.
  - intros k. split.
    + intros H. inversion H.
    + unfold map_contains, empty_map. intros H. discriminate.
  - reflexivity.
Qed.

End MapSpec.

(** ** Type Class for Map Implementations *)

(** Any implementation must provide these operations and satisfy the laws *)
(** Specific to our Key type which has decidable equality *)
Class MapImpl (V : Type) := {
  map_t : Type;
  impl_empty : map_t;
  impl_lookup : map_t -> Key -> option V;
  impl_insert : map_t -> Key -> V -> map_t;
  impl_delete : map_t -> Key -> map_t;

  (** Correctness: implementation refines abstract map *)
  impl_interpret : map_t -> Key -> option V;

  impl_lookup_correct : forall m k,
    impl_lookup m k = impl_interpret m k;

  impl_insert_correct : forall m k v k',
    impl_interpret (impl_insert m k v) k' =
      if key_eq_dec k k' then Some v else impl_interpret m k';

  impl_delete_correct : forall m k k',
    impl_interpret (impl_delete m k) k' =
      if key_eq_dec k k' then None else impl_interpret m k';
}.

(** The abstract Map is trivially a MapImpl *)
#[global]
Instance AbstractMapImpl (V : Type) : MapImpl V := {
  map_t := Map V;
  impl_empty := empty_map V;
  impl_lookup := map_lookup V;
  impl_insert := map_insert V;
  impl_delete := map_delete V;
  impl_interpret := fun m k => m k;
  impl_lookup_correct := fun m k => eq_refl;
  impl_insert_correct := fun m k v k' => eq_refl;
  impl_delete_correct := fun m k k' => eq_refl;
}.
