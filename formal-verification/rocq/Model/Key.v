(** * Key: Key Representation for ARTrie

    This module defines the key representation used in the ARTrie.
    Keys are byte sequences with operations for:
    - Prefix matching
    - Common prefix computation
    - Key comparison
    - Splitting and concatenation
*)

Require Import Coq.Lists.List.
Require Import Coq.Arith.Arith.
Require Import Coq.Bool.Bool.
Require Import Coq.micromega.Lia.
Require Import Coq.Logic.FunctionalExtensionality.
Import ListNotations.

(** ** Proof Irrelevance Axiom (needed for Byte equality proofs) *)
Axiom proof_irrelevance : forall (P : Prop) (p1 p2 : P), p1 = p2.

(** ** Byte Type *)

(** A byte is a natural number bounded to [0, 255] *)
Definition Byte := {n : nat | n < 256}.

(** Smart constructor for bytes *)
Program Definition make_byte (n : nat) (H : n < 256) : Byte := n.

(** Extract the natural number from a byte *)
Definition byte_val (b : Byte) : nat := proj1_sig b.

Coercion byte_val : Byte >-> nat.

(** Byte equality is decidable *)
Definition byte_eqb (b1 b2 : Byte) : bool :=
  Nat.eqb (byte_val b1) (byte_val b2).

Lemma byte_eqb_refl : forall b, byte_eqb b b = true.
Proof.
  intros [n H]. unfold byte_eqb, byte_val. simpl.
  apply Nat.eqb_refl.
Qed.

Lemma byte_eqb_eq : forall b1 b2,
  byte_eqb b1 b2 = true <-> b1 = b2.
Proof.
  intros [n1 H1] [n2 H2].
  unfold byte_eqb, byte_val. simpl.
  split.
  - intros H. apply Nat.eqb_eq in H. subst.
    f_equal. apply proof_irrelevance.
  - intros H. injection H as Hn. subst.
    apply Nat.eqb_refl.
Qed.

(** ** Key Type *)

Definition Key := list Byte.

(** Empty key *)
Definition empty_key : Key := [].

(** Key length *)
Definition key_length (k : Key) : nat := length k.

(** Get byte at index (with default) *)
Definition key_at (k : Key) (i : nat) (default : Byte) : Byte :=
  nth i k default.

(** Key equality *)
Fixpoint key_eqb (k1 k2 : Key) : bool :=
  match k1, k2 with
  | [], [] => true
  | b1 :: t1, b2 :: t2 => byte_eqb b1 b2 && key_eqb t1 t2
  | _, _ => false
  end.

Lemma key_eqb_refl : forall k, key_eqb k k = true.
Proof.
  induction k as [| b k' IH]; simpl.
  - reflexivity.
  - rewrite byte_eqb_refl. simpl. exact IH.
Qed.

Lemma key_eqb_eq : forall k1 k2,
  key_eqb k1 k2 = true <-> k1 = k2.
Proof.
  induction k1 as [| b1 k1' IH]; intros [| b2 k2']; simpl.
  - split; auto.
  - split; discriminate.
  - split; discriminate.
  - rewrite andb_true_iff. split.
    + intros [Hb Hk]. apply byte_eqb_eq in Hb. apply IH in Hk.
      subst. reflexivity.
    + intros H. injection H as Hb Hk. subst.
      split; [apply byte_eqb_refl | apply IH; reflexivity].
Qed.

(** ** Prefix Operations *)

(** Check if k1 is a prefix of k2 *)
Fixpoint is_prefix (k1 k2 : Key) : bool :=
  match k1, k2 with
  | [], _ => true
  | _, [] => false
  | b1 :: t1, b2 :: t2 => byte_eqb b1 b2 && is_prefix t1 t2
  end.

(** The empty key is a prefix of any key *)
Lemma empty_is_prefix : forall k, is_prefix [] k = true.
Proof.
  intros k. reflexivity.
Qed.

(** A key is a prefix of itself *)
Lemma key_is_prefix_self : forall k, is_prefix k k = true.
Proof.
  induction k as [| b k' IH]; simpl.
  - reflexivity.
  - rewrite byte_eqb_refl. simpl. exact IH.
Qed.

(** Prefix transitivity *)
Lemma is_prefix_trans : forall k1 k2 k3,
  is_prefix k1 k2 = true ->
  is_prefix k2 k3 = true ->
  is_prefix k1 k3 = true.
Proof.
  induction k1 as [| b1 k1' IH]; intros k2 k3 H12 H23; simpl.
  - reflexivity.
  - destruct k2 as [| b2 k2']; simpl in H12.
    + discriminate.
    + destruct k3 as [| b3 k3']; simpl in H23.
      * discriminate.
      * rewrite andb_true_iff in H12, H23.
        destruct H12 as [Hb12 Hk12].
        destruct H23 as [Hb23 Hk23].
        apply byte_eqb_eq in Hb12, Hb23. subst.
        rewrite byte_eqb_refl. simpl.
        apply (IH k2' k3'); assumption.
Qed.

(** ** Common Prefix *)

(** Compute the longest common prefix of two keys *)
Fixpoint common_prefix (k1 k2 : Key) : Key :=
  match k1, k2 with
  | [], _ => []
  | _, [] => []
  | b1 :: t1, b2 :: t2 =>
      if byte_eqb b1 b2
      then b1 :: common_prefix t1 t2
      else []
  end.

(** Length of common prefix *)
Fixpoint common_prefix_length (k1 k2 : Key) : nat :=
  match k1, k2 with
  | [], _ => 0
  | _, [] => 0
  | b1 :: t1, b2 :: t2 =>
      if byte_eqb b1 b2
      then S (common_prefix_length t1 t2)
      else 0
  end.

(** Common prefix is indeed a prefix of both keys *)
Lemma common_prefix_is_prefix_left : forall k1 k2,
  is_prefix (common_prefix k1 k2) k1 = true.
Proof.
  induction k1 as [| b1 k1' IH]; intros k2; simpl.
  - reflexivity.
  - destruct k2 as [| b2 k2']; simpl.
    + reflexivity.
    + destruct (byte_eqb b1 b2) eqn:Heq; simpl.
      * rewrite byte_eqb_refl. simpl. apply IH.
      * reflexivity.
Qed.

Lemma common_prefix_is_prefix_right : forall k1 k2,
  is_prefix (common_prefix k1 k2) k2 = true.
Proof.
  induction k1 as [| b1 k1' IH]; intros k2; simpl.
  - reflexivity.
  - destruct k2 as [| b2 k2']; simpl.
    + reflexivity.
    + destruct (byte_eqb b1 b2) eqn:Heq; simpl.
      * apply byte_eqb_eq in Heq. subst.
        rewrite byte_eqb_refl. simpl. apply IH.
      * reflexivity.
Qed.

(** Common prefix is the longest common prefix *)
Lemma common_prefix_maximal : forall k1 k2 p,
  is_prefix p k1 = true ->
  is_prefix p k2 = true ->
  is_prefix p (common_prefix k1 k2) = true.
Proof.
  induction k1 as [| b1 k1' IH]; intros k2 p Hp1 Hp2; simpl.
  - destruct p; [reflexivity | simpl in Hp1; discriminate].
  - destruct k2 as [| b2 k2'].
    + destruct p; [reflexivity | simpl in Hp2; discriminate].
    + destruct p as [| bp p']; [reflexivity |].
      simpl in Hp1, Hp2.
      rewrite andb_true_iff in Hp1, Hp2.
      destruct Hp1 as [Hbp1 Hp1'].
      destruct Hp2 as [Hbp2 Hp2'].
      apply byte_eqb_eq in Hbp1, Hbp2. subst.
      rewrite byte_eqb_refl. simpl.
      rewrite byte_eqb_refl. simpl.
      apply IH; assumption.
Qed.

(** ** Key Splitting *)

(** Split a key at a given position *)
Definition key_split (k : Key) (n : nat) : Key * Key :=
  (firstn n k, skipn n k).

(** Split preserves concatenation *)
Lemma key_split_concat : forall k n,
  fst (key_split k n) ++ snd (key_split k n) = k.
Proof.
  intros k n. unfold key_split. simpl.
  apply firstn_skipn.
Qed.

(** ** Suffix Operations *)

(** Get the suffix after removing a prefix *)
Definition key_suffix (k : Key) (prefix_len : nat) : Key :=
  skipn prefix_len k.

(** Suffix after prefix *)
Lemma key_suffix_after_prefix : forall k1 k2,
  is_prefix k1 k2 = true ->
  k1 ++ key_suffix k2 (length k1) = k2.
Proof.
  induction k1 as [| b1 k1' IH]; intros k2 H; simpl in *.
  - reflexivity.
  - destruct k2 as [| b2 k2']; [discriminate |].
    rewrite andb_true_iff in H. destruct H as [Hb Hk].
    apply byte_eqb_eq in Hb. subst.
    simpl. f_equal. apply IH. assumption.
Qed.

(** ** Prefix Match Results *)

Inductive PrefixMatchResult :=
  | FullMatch           (* Key fully matches prefix *)
  | PartialMatch (pos : nat) (kb : Byte) (pb : Byte)  (* Mismatch at position *)
  | KeyTooShort (pos : nat).   (* Key ended before prefix *)

(** Match a key against a prefix starting at offset *)
Fixpoint match_prefix_aux (key : Key) (prefix : Key) (pos : nat)
  : PrefixMatchResult :=
  match prefix with
  | [] => FullMatch
  | pb :: prefix' =>
      match key with
      | [] => KeyTooShort pos
      | kb :: key' =>
          if byte_eqb kb pb
          then match_prefix_aux key' prefix' (S pos)
          else PartialMatch pos kb pb
      end
  end.

Definition match_prefix (key : Key) (key_offset : nat) (prefix : Key)
  : PrefixMatchResult :=
  match_prefix_aux (skipn key_offset key) prefix 0.

(** Byte equality is symmetric *)
Lemma byte_eqb_sym : forall b1 b2, byte_eqb b1 b2 = byte_eqb b2 b1.
Proof.
  intros [n1 H1] [n2 H2]. unfold byte_eqb, byte_val. simpl.
  rewrite Nat.eqb_sym. reflexivity.
Qed.

(** Match prefix result correctness *)
Lemma match_prefix_full_match : forall key offset prefix,
  match_prefix key offset prefix = FullMatch ->
  is_prefix prefix (skipn offset key) = true.
Proof.
  intros key offset prefix.
  unfold match_prefix.
  generalize 0 as pos.
  generalize (skipn offset key) as key'.
  clear key offset.
  induction prefix as [| pb prefix' IH]; intros key' pos H; simpl.
  - reflexivity.
  - destruct key' as [| kb key'']; simpl in H.
    + discriminate.
    + destruct (byte_eqb kb pb) eqn:Heq.
      * rewrite byte_eqb_sym, Heq. simpl. apply IH in H. assumption.
      * discriminate.
Qed.

(** ** Key Comparison *)

(** Lexicographic comparison *)
Fixpoint key_compare (k1 k2 : Key) : comparison :=
  match k1, k2 with
  | [], [] => Eq
  | [], _ => Lt
  | _, [] => Gt
  | b1 :: t1, b2 :: t2 =>
      match Nat.compare (byte_val b1) (byte_val b2) with
      | Lt => Lt
      | Gt => Gt
      | Eq => key_compare t1 t2
      end
  end.

(** Key comparison is reflexive *)
Lemma key_compare_refl : forall k, key_compare k k = Eq.
Proof.
  induction k as [| b k' IH]; simpl.
  - reflexivity.
  - rewrite Nat.compare_refl. exact IH.
Qed.

(** Key comparison reflects equality *)
Lemma key_compare_eq : forall k1 k2,
  key_compare k1 k2 = Eq <-> k1 = k2.
Proof.
  induction k1 as [| b1 k1' IH]; intros [| b2 k2']; simpl; split; intro H;
    try discriminate; try reflexivity.
  - destruct (Nat.compare (byte_val b1) (byte_val b2)) eqn:Hcmp; try discriminate.
    apply Nat.compare_eq in Hcmp.
    assert (Hb : b1 = b2).
    { destruct b1 as [n1 H1], b2 as [n2 H2]. simpl in Hcmp.
      subst. f_equal. apply proof_irrelevance. }
    apply IH in H. subst. reflexivity.
  - injection H as Hb Hk. subst.
    rewrite Nat.compare_refl. apply IH. reflexivity.
Qed.
