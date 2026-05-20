(** * PathCompression: Prefix Compression for ARTrie

    This module defines path compression operations used in the ARTrie.
    Path compression stores common key prefixes inline in nodes to
    reduce tree height.

    Key operations:
    - Prefix matching against keys
    - Prefix splitting at divergence points
    - Prefix extension after node deletion
*)

Require Import Coq.Lists.List.
Require Import Coq.Arith.Arith.
Require Import Coq.Bool.Bool.
Require Import Coq.micromega.Lia.
Require Import ARTrie.Model.Key.
Require Import ARTrie.Model.NodeTypes.
Import ListNotations.

Definition zero_byte : Byte := make_byte 0 ltac:(lia).

(** ** Prefix Match Types *)

(** Result of matching a key against a node's prefix *)
Inductive MatchResult :=
  | FullMatchResult          (* Key fully matches prefix *)
  | PartialMatchResult       (* Mismatch at some position *)
      (mismatch_pos : nat)   (* Position of first mismatch *)
      (key_byte : Byte)      (* Byte in key at mismatch *)
      (prefix_byte : Byte)   (* Byte in prefix at mismatch *)
  | KeyTooShortResult        (* Key ended before prefix *)
      (key_end_pos : nat).   (* Position where key ended *)

(** ** Prefix Matching *)

(** Match key against prefix starting at key_offset *)
Fixpoint match_prefix_impl (key : Key) (key_offset : nat)
  (prefix : Key) (pos : nat) : MatchResult :=
  match prefix with
  | [] => FullMatchResult
  | pb :: prefix_tail =>
      match nth_error key (key_offset + pos) with
      | None => KeyTooShortResult pos
      | Some kb =>
          if byte_eqb kb pb
          then match_prefix_impl key key_offset prefix_tail (S pos)
          else PartialMatchResult pos kb pb
      end
  end.

Definition match_prefix (key : Key) (key_offset : nat) (prefix : CompressedPrefix)
  : MatchResult :=
  match_prefix_impl key key_offset (prefix_bytes prefix) 0.

(** ** Prefix Split *)

(** Result of splitting a prefix at a mismatch *)
Record SplitResult := mkSplit {
  split_before : CompressedPrefix;     (* Prefix before split point *)
  split_byte : Byte;                   (* Byte at split point *)
  split_after : CompressedPrefix       (* Prefix after split point *)
}.

(** Split prefix at position (for insertion divergence) *)
Definition split_prefix (prefix : CompressedPrefix) (pos : nat)
  (H : pos < prefix_len prefix) : SplitResult.
Proof.
  pose (bytes := prefix_bytes prefix).
  pose (before := firstn pos bytes).
  pose (after := skipn (S pos) bytes).
  refine (mkSplit
    (mkPrefix before (length before) eq_refl _)
    (nth pos bytes zero_byte)
    (mkPrefix after (length after) eq_refl _)).
  - subst before bytes.
    rewrite length_firstn.
    pose proof (prefix_len_bound prefix).
    pose proof (prefix_len_valid prefix).
    lia.
  - subst after bytes.
    rewrite length_skipn.
    pose proof (prefix_len_bound prefix).
    pose proof (prefix_len_valid prefix).
    lia.
Defined.

(** ** Prefix Extension *)

(** Extend prefix by prepending bytes (used after node promotion) *)
Definition extend_prefix (prepend : Key) (edge_byte : Byte)
  (base : CompressedPrefix) : CompressedPrefix.
Proof.
  pose (combined := prepend ++ [edge_byte] ++ prefix_bytes base).
  pose (truncated := firstn MAX_PREFIX_LEN combined).
  refine (mkPrefix truncated (length truncated) eq_refl _).
  subst truncated combined.
  rewrite length_firstn.
  lia.
Defined.

(** Truncate prefix to given length *)
Definition truncate_prefix (prefix : CompressedPrefix) (new_len : nat)
  (H : new_len <= prefix_len prefix) : CompressedPrefix.
Proof.
  refine (mkPrefix (firstn new_len (prefix_bytes prefix)) new_len _ _).
  - rewrite length_firstn.
    pose proof (prefix_len_valid prefix).
    lia.
  - pose proof (prefix_len_bound prefix).
    lia.
Defined.

(** ** Prefix Consumption *)

(** Consume n bytes from prefix (during traversal) *)
Definition consume_prefix (prefix : CompressedPrefix) (n : nat)
  : CompressedPrefix.
Proof.
  destruct (n <? prefix_len prefix) eqn:Hn.
  - pose (remaining := skipn n (prefix_bytes prefix)).
    refine (mkPrefix remaining (length remaining) eq_refl _).
    subst remaining.
    rewrite length_skipn.
    pose proof (prefix_len_bound prefix).
    pose proof (prefix_len_valid prefix).
    lia.
  - exact empty_prefix.
Defined.

(** ** Common Prefix Computation *)

(** Compute common prefix of two keys *)
Fixpoint common_prefix_impl (k1 k2 : Key) : Key :=
  match k1, k2 with
  | [], _ => []
  | _, [] => []
  | b1 :: t1, b2 :: t2 =>
      if byte_eqb b1 b2
      then b1 :: common_prefix_impl t1 t2
      else []
  end.

Definition compute_common_prefix (k1 k2 : Key)
  : CompressedPrefix.
Proof.
  pose (common := common_prefix_impl k1 k2).
  pose (truncated := firstn MAX_PREFIX_LEN common).
  refine (mkPrefix truncated (length truncated) eq_refl _).
  subst truncated common.
  rewrite length_firstn.
  lia.
Defined.

Lemma nth_error_skipn_cons : forall {A : Type} (l : list A) n x,
  nth_error l n = Some x ->
  exists rest, skipn n l = x :: rest.
Proof.
  induction l as [| a l IH]; intros [| n] x H; simpl in *; try discriminate.
  - injection H as Hx. subst. exists l. reflexivity.
  - apply IH in H as [rest Hrest]. exists rest. exact Hrest.
Qed.

Lemma firstn_nth_skipn : forall {A : Type} (l : list A) n d,
  n < length l ->
  firstn n l ++ [nth n l d] ++ skipn (S n) l = l.
Proof.
  induction l as [| a l IH]; intros [| n] d H; simpl in *; try lia.
  - reflexivity.
  - f_equal. apply IH. lia.
Qed.

Lemma match_prefix_impl_full_prefix : forall bytes key offset pos,
  match_prefix_impl key offset bytes pos = FullMatchResult ->
  is_prefix bytes (skipn (offset + pos) key) = true.
Proof.
  induction bytes as [| pb bytes IH]; intros key offset pos Hmatch; simpl in *.
  - reflexivity.
  - destruct (nth_error key (offset + pos)) as [kb|] eqn:Hnth; try discriminate.
    destruct (byte_eqb kb pb) eqn:Heq; try discriminate.
    destruct (nth_error_skipn_cons key (offset + pos) kb Hnth) as [rest Hrest].
    rewrite Hrest. simpl. rewrite byte_eqb_sym, Heq. simpl.
    apply IH in Hmatch.
    replace (offset + S pos) with (S (offset + pos)) in Hmatch by lia.
    replace rest with (skipn (S (offset + pos)) key); [exact Hmatch|].
    symmetry.
    replace (S (offset + pos)) with (1 + (offset + pos)) by lia.
    rewrite <- skipn_skipn.
    rewrite Hrest. reflexivity.
Qed.

Lemma match_prefix_impl_partial_pos : forall bytes key offset start pos kb pb,
  match_prefix_impl key offset bytes start = PartialMatchResult pos kb pb ->
  start <= pos < start + length bytes.
Proof.
  induction bytes as [| b bytes IH]; intros key offset start pos kb pb Hmatch; simpl in *.
  - discriminate.
  - destruct (nth_error key (offset + start)) as [kbyte|] eqn:Hnth; try discriminate.
    destruct (byte_eqb kbyte b) eqn:Heq.
    + apply IH in Hmatch. lia.
    + injection Hmatch as Hpos Hkb Hpb. subst. lia.
Qed.

Lemma is_prefix_firstn : forall n p k,
  is_prefix p k = true ->
  is_prefix (firstn n p) k = true.
Proof.
  induction n as [| n IH]; intros [| bp p] [| bk k] H; simpl in *; try reflexivity; try discriminate.
  rewrite andb_true_iff in H.
  destruct H as [Hb Hp].
  rewrite Hb. simpl. apply IH. exact Hp.
Qed.

Lemma common_prefix_impl_eq : forall k1 k2,
  common_prefix_impl k1 k2 = common_prefix k1 k2.
Proof.
  induction k1 as [| b1 k1 IH]; intros [| b2 k2]; simpl; try reflexivity.
Qed.

(** ** Correctness Lemmas *)

(** Full match implies prefix is a prefix of key from offset *)
Lemma match_full_implies_prefix : forall key offset prefix,
  match_prefix key offset prefix = FullMatchResult ->
  is_prefix (prefix_bytes prefix) (skipn offset key) = true.
Proof.
  intros key offset prefix H.
  unfold match_prefix in H.
  replace (skipn offset key) with (skipn (offset + 0) key) by (rewrite Nat.add_0_r; reflexivity).
  eapply match_prefix_impl_full_prefix. exact H.
Qed.

(** Partial match gives valid mismatch position *)
Lemma match_partial_pos_valid : forall key offset prefix pos kb pb,
  match_prefix key offset prefix = PartialMatchResult pos kb pb ->
  pos < prefix_len prefix.
Proof.
  intros key offset prefix pos kb pb H.
  unfold match_prefix in H.
  apply match_prefix_impl_partial_pos in H.
  pose proof (prefix_len_valid prefix).
  lia.
Qed.

(** Split prefix preserves bytes *)
Lemma split_preserves_bytes : forall prefix pos H,
  let result := split_prefix prefix pos H in
  prefix_bytes (split_before result) ++
  [split_byte result] ++
  prefix_bytes (split_after result) = prefix_bytes prefix.
Proof.
  intros prefix pos H.
  unfold split_prefix. simpl.
  apply firstn_nth_skipn.
  pose proof (prefix_len_valid prefix).
  lia.
Qed.

(** Extended prefix contains original prefix *)
Lemma extend_contains_base : forall prepend edge base,
  length prepend + 1 + prefix_len base <= MAX_PREFIX_LEN ->
  exists suffix,
    prefix_bytes (extend_prefix prepend edge base) =
    prepend ++ [edge] ++ prefix_bytes base ++ suffix.
Proof.
  intros prepend edge base Hlen.
  exists [].
  unfold extend_prefix. cbn [prefix_bytes].
  rewrite firstn_all2; [rewrite app_nil_r; reflexivity|].
  repeat rewrite length_app. simpl.
  pose proof (prefix_len_valid base).
  lia.
Qed.

(** Common prefix is indeed a common prefix *)
Lemma common_prefix_is_prefix_l : forall k1 k2,
  is_prefix (prefix_bytes (compute_common_prefix k1 k2)) k1 = true.
Proof.
  intros k1 k2.
  unfold compute_common_prefix. cbn [prefix_bytes].
  apply is_prefix_firstn.
  rewrite common_prefix_impl_eq.
  apply common_prefix_is_prefix_left.
Qed.

Lemma common_prefix_is_prefix_r : forall k1 k2,
  is_prefix (prefix_bytes (compute_common_prefix k1 k2)) k2 = true.
Proof.
  intros k1 k2.
  unfold compute_common_prefix. cbn [prefix_bytes].
  apply is_prefix_firstn.
  rewrite common_prefix_impl_eq.
  apply common_prefix_is_prefix_right.
Qed.

(** ** Path Compression Invariant *)

(** All keys in a subtree share the node's prefix *)
Definition subtree_shares_prefix (keys : list Key) (prefix : CompressedPrefix)
  (offset : nat) : Prop :=
  forall k, In k keys ->
    is_prefix (prefix_bytes prefix) (skipn offset k) = true.

(** Path compression is maximally compressed *)
Definition maximally_compressed (keys : list Key) (prefix : CompressedPrefix)
  (offset : nat) : Prop :=
  subtree_shares_prefix keys prefix offset /\
  (prefix_len prefix = MAX_PREFIX_LEN \/
   ~(exists longer_prefix,
     prefix_len longer_prefix > prefix_len prefix /\
     subtree_shares_prefix keys longer_prefix offset)).
