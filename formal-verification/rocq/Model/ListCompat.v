(** * ListCompat: version-independent list facts

    Coq 8.18 exports the length-of-[firstn] theorem as [firstn_length].
    Rocq 9.1 exports it as [length_firstn] and keeps the old name only as a
    deprecated notation.  Proving the small fact here gives the ARTrie proof
    corpus one stable theorem name without conditional syntax or version
    aliases.
*)

From Coq Require Import Lists.List.
From Coq Require Import Arith.Arith.

Lemma firstn_length_portable :
  forall (A : Type) (count : nat) (items : list A),
    length (firstn count items) = Nat.min count (length items).
Proof.
  intros A count.
  induction count as [| count IH]; intros items.
  - destruct items; reflexivity.
  - destruct items as [| item items].
    + reflexivity.
    + simpl. rewrite IH. reflexivity.
Qed.

Lemma skipn_length_portable :
  forall (A : Type) (count : nat) (items : list A),
    length (skipn count items) = length items - count.
Proof.
  intros A count.
  induction count as [| count IH]; intros items.
  - destruct items; reflexivity.
  - destruct items as [| item items].
    + reflexivity.
    + simpl. apply IH.
Qed.

(** Coq 8.18 does not export [NoDup_app] under the name used by Rocq 9.1.
    This one-way form is exactly what the HotStuff quorum proof needs and is
    deliberately proved from the constructors shared by both releases. *)
Lemma NoDup_app_portable :
  forall (A : Type) (left right : list A),
    NoDup left ->
    NoDup right ->
    (forall item, In item left -> ~ In item right) ->
    NoDup (left ++ right).
Proof.
  intros A left right Hleft.
  induction Hleft as [| item left Hitem Hleft IH];
    intros Hright Hdisjoint.
  - simpl. exact Hright.
  - simpl. constructor.
    + intro Hin.
      apply in_app_or in Hin.
      destruct Hin as [Hin | Hin].
      * exact (Hitem Hin).
      * exact (Hdisjoint item (or_introl eq_refl) Hin).
    + apply IH.
      * exact Hright.
      * intros other Hin_left Hin_right.
        exact (Hdisjoint other (or_intror Hin_left) Hin_right).
Qed.
