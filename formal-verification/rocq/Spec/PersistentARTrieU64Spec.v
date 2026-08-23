(** * PersistentARTrieU64Spec: Sequence-Keyed Persistent Trie Laws

    The implementation exposes u64-native sequence operations while storing
    fixed-width little-endian bytes in the established persistent byte ARTrie.
    This specification names the proof boundary: the byte encoding is injective
    at sequence boundaries, exact membership refines an abstract u64-sequence
    set, and checkpoint/reopen preserves the abstract set.
*)

From Coq Require Import Lists.List.
From Coq Require Import Arith.PeanoNat.
Require Import ARTrie.Spec.DynamicDawgU64Spec.
Import ListNotations.

Definition U64Bytes := list nat.

Record U64PersistentState := {
  u64_live : U64Set;
  u64_durable : U64Set
}.

Definition fixed_width_u64_encoding
  (encode : U64Sequence -> U64Bytes) : Prop :=
  forall sequence, length (encode sequence) = 8 * length sequence.

Definition sequence_boundary_key (bytes : U64Bytes) : Prop :=
  exists n, length bytes = 8 * n.

Definition u64_persistent_init : U64PersistentState := {|
  u64_live := u64_set_empty;
  u64_durable := u64_set_empty
|}.

Definition u64_persistent_insert
  (state : U64PersistentState) (sequence : U64Sequence) : U64PersistentState :=
  {|
    u64_live := u64_set_insert (u64_live state) sequence;
    u64_durable := u64_durable state
  |}.

Definition u64_persistent_remove
  (state : U64PersistentState) (sequence : U64Sequence) : U64PersistentState :=
  {|
    u64_live := u64_set_remove (u64_live state) sequence;
    u64_durable := u64_durable state
  |}.

Definition u64_persistent_checkpoint
  (state : U64PersistentState) : U64PersistentState :=
  {|
    u64_live := u64_live state;
    u64_durable := u64_live state
  |}.

Definition u64_persistent_reopen
  (state : U64PersistentState) : U64PersistentState :=
  {|
    u64_live := u64_durable state;
    u64_durable := u64_durable state
  |}.

Theorem fixed_width_encoding_has_sequence_boundary :
  forall encode sequence,
    fixed_width_u64_encoding encode ->
    sequence_boundary_key (encode sequence).
Proof.
  intros encode sequence Hwidth.
  unfold fixed_width_u64_encoding in Hwidth.
  unfold sequence_boundary_key.
  exists (length sequence).
  apply Hwidth.
Qed.

Theorem persistent_u64_insert_contains :
  forall state sequence,
    u64_set_contains
      (u64_live (u64_persistent_insert state sequence))
      sequence = true.
Proof.
  intros state sequence.
  simpl.
  apply u64_set_insert_contains_same.
Qed.

Theorem persistent_u64_remove_absent :
  forall state sequence,
    u64_set_contains
      (u64_live (u64_persistent_remove state sequence))
      sequence = false.
Proof.
  intros state sequence.
  simpl.
  apply u64_set_remove_contains_same.
Qed.

Theorem persistent_u64_checkpoint_reopen_preserves_live :
  forall state sequence,
    u64_set_contains
      (u64_live (u64_persistent_reopen (u64_persistent_checkpoint state)))
      sequence =
    u64_set_contains (u64_live state) sequence.
Proof.
  intros state sequence.
  reflexivity.
Qed.

Theorem persistent_u64_reopen_uses_durable :
  forall state sequence,
    u64_set_contains
      (u64_live (u64_persistent_reopen state))
      sequence =
    u64_set_contains (u64_durable state) sequence.
Proof.
  intros state sequence.
  reflexivity.
Qed.
