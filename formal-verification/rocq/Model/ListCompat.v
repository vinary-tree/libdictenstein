(** * ListCompat: version-independent list facts

    Coq 8.18 exports the length-of-[firstn] theorem as [firstn_length].
    Rocq 9.1 exports it as [length_firstn] and keeps the old name only as a
    deprecated notation.  Proving the small fact here gives the ARTrie proof
    corpus one stable theorem name without conditional syntax or version
    aliases.
*)

From Stdlib Require Import List.
From Stdlib Require Import Arith.

Lemma firstn_length_portable :
  forall (A : Type) (count : nat) (items : list A),
    length (firstn count items) = Nat.min count (length items).
Proof.
  intros A count.
  induction count as [| count IH]; intros items.
  - reflexivity.
  - destruct items as [| item items].
    + reflexivity.
    + simpl. rewrite IH. reflexivity.
Qed.
