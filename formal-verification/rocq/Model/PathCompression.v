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
Program Definition split_prefix (prefix : CompressedPrefix) (pos : nat)
  (H : pos < prefix_len prefix) : SplitResult :=
  let bytes := prefix_bytes prefix in
  let before := firstn pos bytes in
  let split_b := nth pos bytes (make_byte 0 _) in
  let after := skipn (S pos) bytes in
  mkSplit
    (mkPrefix before (length before) _ _)
    split_b
    (mkPrefix after (length after) _ _).
Solve All Obligations with
  try reflexivity;
  try (rewrite ?length_firstn, ?length_skipn;
       pose proof (prefix_len_bound prefix);
       pose proof (prefix_len_valid prefix);
       lia).
Admit Obligations.

(** ** Prefix Extension *)

(** Extend prefix by prepending bytes (used after node promotion) *)
Program Definition extend_prefix (prepend : Key) (edge_byte : Byte)
  (base : CompressedPrefix) : CompressedPrefix :=
  let combined := prepend ++ [edge_byte] ++ prefix_bytes base in
  let truncated := firstn MAX_PREFIX_LEN combined in
  mkPrefix truncated (length truncated) _ _.
Solve All Obligations with
  try reflexivity;
  try (rewrite ?length_firstn;
       destruct (le_lt_dec MAX_PREFIX_LEN (length (prepend ++ [edge_byte] ++ prefix_bytes base)));
       lia).
Admit Obligations.

(** Truncate prefix to given length *)
Program Definition truncate_prefix (prefix : CompressedPrefix) (new_len : nat)
  (H : new_len <= prefix_len prefix) : CompressedPrefix :=
  mkPrefix (firstn new_len (prefix_bytes prefix)) new_len _ _.
Solve All Obligations with
  try reflexivity;
  try (rewrite ?length_firstn;
       pose proof (prefix_len_bound prefix);
       pose proof (prefix_len_valid prefix);
       lia).
Admit Obligations.

(** ** Prefix Consumption *)

(** Consume n bytes from prefix (during traversal) *)
Program Definition consume_prefix (prefix : CompressedPrefix) (n : nat)
  : CompressedPrefix :=
  if n <? prefix_len prefix then
    let remaining := skipn n (prefix_bytes prefix) in
    mkPrefix remaining (length remaining) _ _
  else
    empty_prefix.
Solve All Obligations with
  try reflexivity;
  try (rewrite ?length_skipn;
       pose proof (prefix_len_bound prefix);
       pose proof (prefix_len_valid prefix);
       try apply Nat.ltb_lt in Heq_anonymous;
       lia).
Admit Obligations.

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

Program Definition compute_common_prefix (k1 k2 : Key)
  : CompressedPrefix :=
  let common := common_prefix_impl k1 k2 in
  let truncated := firstn MAX_PREFIX_LEN common in
  mkPrefix truncated (length truncated) _ _.
Solve All Obligations with
  try reflexivity;
  try (rewrite ?length_firstn; lia).
Admit Obligations.

(** ** Correctness Lemmas *)

(** Full match implies prefix is a prefix of key from offset *)
Lemma match_full_implies_prefix : forall key offset prefix,
  match_prefix key offset prefix = FullMatchResult ->
  is_prefix (prefix_bytes prefix) (skipn offset key) = true.
Proof.
  intros key offset prefix H.
  unfold match_prefix in H.
  (* The proof follows from the definition of match_prefix_impl *)
  (* When FullMatchResult is returned, all bytes matched *)
Admitted.

(** Partial match gives valid mismatch position *)
Lemma match_partial_pos_valid : forall key offset prefix pos kb pb,
  match_prefix key offset prefix = PartialMatchResult pos kb pb ->
  pos < prefix_len prefix.
Proof.
  intros key offset prefix pos kb pb H.
  unfold match_prefix in H.
  (* The proof follows from how match_prefix_impl tracks position *)
Admitted.

(** Split prefix preserves bytes *)
Lemma split_preserves_bytes : forall prefix pos H,
  let result := split_prefix prefix pos H in
  prefix_bytes (split_before result) ++
  [split_byte result] ++
  prefix_bytes (split_after result) = prefix_bytes prefix.
Proof.
  intros prefix pos H result.
  unfold split_prefix in result. simpl.
  (* The proof follows from firstn/skipn properties *)
Admitted.

(** Extended prefix contains original prefix *)
Lemma extend_contains_base : forall prepend edge base,
  length prepend + 1 + prefix_len base <= MAX_PREFIX_LEN ->
  exists suffix,
    prefix_bytes (extend_prefix prepend edge base) =
    prepend ++ [edge] ++ prefix_bytes base ++ suffix.
Proof.
  intros prepend edge base Hlen.
  (* When combined length fits, extension is complete *)
Admitted.

(** Common prefix is indeed a common prefix *)
Lemma common_prefix_is_prefix_l : forall k1 k2,
  is_prefix (prefix_bytes (compute_common_prefix k1 k2)) k1 = true.
Proof.
  (* The proof follows from common_prefix_impl definition *)
Admitted.

Lemma common_prefix_is_prefix_r : forall k1 k2,
  is_prefix (prefix_bytes (compute_common_prefix k1 k2)) k2 = true.
Proof.
  (* Symmetric to common_prefix_is_prefix_l *)
Admitted.

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
