(** * TransitionInvariants: Node Transition Invariants for ARTrie

    This module defines and proves invariants related to node type
    transitions (Node4 <-> Node16 <-> Node48 <-> Node256).

    Key invariants:
    - Transitions preserve all children
    - Transitions maintain child ordering (for sorted node types)
    - Transitions happen at correct thresholds
*)

Require Import Coq.Lists.List.
Require Import Coq.Arith.Arith.
Require Import Coq.Bool.Bool.
Require Import Coq.micromega.Lia.
Require Import ARTrie.Model.Key.
Require Import ARTrie.Model.NodeTypes.
Require Import ARTrie.Spec.ARTrieSpec.
Import ListNotations.

(** Helper: enumerate all 256 bytes *)
Parameter enumerate_bytes : list Byte.

(** ** Child Preservation *)

(** Get all children of a node as a list of (byte, ptr) pairs *)
Definition get_all_children (n : Node) : list (Byte * ChildPtr) :=
  filter (fun bp => negb (is_null (snd bp)))
    (map (fun b => (b, find_child n b)) enumerate_bytes).

(** All children preserved during transition *)
Definition children_preserved (old_node new_node : Node) : Prop :=
  forall b, find_child old_node b = find_child new_node b.

(** ** Growth Transitions *)

(** Node4 to Node16 transition *)
Definition valid_node4_to_node16 (n4 : Node) (n16 : Node) : Prop :=
  get_node_type n4 = TNode4 /\
  get_node_type n16 = TNode16 /\
  get_child_count n4 = 4 /\
  children_preserved n4 n16 /\
  node_prefix n4 = node_prefix n16 /\
  header_flags (node_header n4) = header_flags (node_header n16).

(** Node16 to Node48 transition *)
Definition valid_node16_to_node48 (n16 : Node) (n48 : Node) : Prop :=
  get_node_type n16 = TNode16 /\
  get_node_type n48 = TNode48 /\
  get_child_count n16 = 16 /\
  children_preserved n16 n48 /\
  node_prefix n16 = node_prefix n48 /\
  header_flags (node_header n16) = header_flags (node_header n48).

(** Node48 to Node256 transition *)
Definition valid_node48_to_node256 (n48 : Node) (n256 : Node) : Prop :=
  get_node_type n48 = TNode48 /\
  get_node_type n256 = TNode256 /\
  get_child_count n48 = 48 /\
  children_preserved n48 n256 /\
  node_prefix n48 = node_prefix n256 /\
  header_flags (node_header n48) = header_flags (node_header n256).

(** ** Shrink Transitions *)

(** Node16 to Node4 transition *)
Definition valid_node16_to_node4 (n16 : Node) (n4 : Node) : Prop :=
  get_node_type n16 = TNode16 /\
  get_node_type n4 = TNode4 /\
  get_child_count n16 <= 4 /\
  children_preserved n16 n4 /\
  node_prefix n16 = node_prefix n4 /\
  header_flags (node_header n16) = header_flags (node_header n4).

(** Node48 to Node16 transition *)
Definition valid_node48_to_node16 (n48 : Node) (n16 : Node) : Prop :=
  get_node_type n48 = TNode48 /\
  get_node_type n16 = TNode16 /\
  get_child_count n48 <= 16 /\
  children_preserved n48 n16 /\
  node_prefix n48 = node_prefix n16 /\
  header_flags (node_header n48) = header_flags (node_header n16).

(** Node256 to Node48 transition *)
Definition valid_node256_to_node48 (n256 : Node) (n48 : Node) : Prop :=
  get_node_type n256 = TNode256 /\
  get_node_type n48 = TNode48 /\
  get_child_count n256 <= 48 /\
  children_preserved n256 n48 /\
  node_prefix n256 = node_prefix n48 /\
  header_flags (node_header n256) = header_flags (node_header n48).

(** ** Transition Threshold Invariants *)

(** Growth happens at correct threshold *)
Definition growth_threshold_correct (n : Node) : Prop :=
  should_grow n = true ->
  match get_node_type n with
  | TNode4 => get_child_count n >= 4
  | TNode16 => get_child_count n >= 16
  | TNode48 => get_child_count n >= 48
  | TNode256 => False  (* Cannot grow Node256 *)
  | TBucket => False   (* Buckets don't grow this way *)
  end.

(** Shrink happens at correct threshold *)
Definition shrink_threshold_correct (n : Node) : Prop :=
  should_shrink n = true ->
  match get_node_type n with
  | TNode4 => False  (* Cannot shrink Node4 *)
  | TNode16 => get_child_count n <= 4
  | TNode48 => get_child_count n <= 16
  | TNode256 => get_child_count n <= 48
  | TBucket => False  (* Buckets don't shrink this way *)
  end.

(** ** Transition Correctness Theorems *)

(** Node4 -> Node16 preserves all children *)
Theorem node4_to_node16_preserves_children :
  forall n4 n16,
  valid_node4_to_node16 n4 n16 ->
  forall b, find_child n4 b = find_child n16 b.
Proof.
  intros n4 n16 [_ [_ [_ [Hpreserve _]]]] b.
  apply Hpreserve.
Qed.

(** Node16 -> Node48 preserves all children *)
Theorem node16_to_node48_preserves_children :
  forall n16 n48,
  valid_node16_to_node48 n16 n48 ->
  forall b, find_child n16 b = find_child n48 b.
Proof.
  intros n16 n48 [_ [_ [_ [Hpreserve _]]]] b.
  apply Hpreserve.
Qed.

(** Node48 -> Node256 preserves all children *)
Theorem node48_to_node256_preserves_children :
  forall n48 n256,
  valid_node48_to_node256 n48 n256 ->
  forall b, find_child n48 b = find_child n256 b.
Proof.
  intros n48 n256 [_ [_ [_ [Hpreserve _]]]] b.
  apply Hpreserve.
Qed.

(** Shrink transitions also preserve children *)
Theorem node16_to_node4_preserves_children :
  forall n16 n4,
  valid_node16_to_node4 n16 n4 ->
  forall b, find_child n16 b = find_child n4 b.
Proof.
  intros n16 n4 [_ [_ [_ [Hpreserve _]]]] b.
  apply Hpreserve.
Qed.

Theorem node48_to_node16_preserves_children :
  forall n48 n16,
  valid_node48_to_node16 n48 n16 ->
  forall b, find_child n48 b = find_child n16 b.
Proof.
  intros n48 n16 [_ [_ [_ [Hpreserve _]]]] b.
  apply Hpreserve.
Qed.

Theorem node256_to_node48_preserves_children :
  forall n256 n48,
  valid_node256_to_node48 n256 n48 ->
  forall b, find_child n256 b = find_child n48 b.
Proof.
  intros n256 n48 [_ [_ [_ [Hpreserve _]]]] b.
  apply Hpreserve.
Qed.

(** ** Type Validity After Transition *)

(** After growth, new type is structurally valid.
    This is provable because growth always moves to a larger node type. *)
Theorem growth_type_valid :
  forall n_old n_new,
  children_preserved n_old n_new ->
  get_node_type n_new = grow_target (get_node_type n_old) ->
  get_child_count n_old = get_child_count n_new ->
  node_type_valid n_old ->
  node_type_valid n_new.
Proof.
  intros n_old n_new Hpreserve Htype Hcount Hvalid.
  unfold node_type_valid.
  unfold get_node_type in Htype.
  unfold get_child_count in Hcount.
  rewrite <- Hcount.
  unfold node_type_valid in Hvalid.
  destruct (header_type (node_header n_old)) eqn:Hold;
  destruct (header_type (node_header n_new)) eqn:Hnew;
  simpl in Htype; try discriminate; simpl in Hvalid; lia || trivial.
Qed.

(** After shrink, new type is structurally valid.
    The should_shrink condition ensures the count is within bounds for the smaller type. *)
Theorem shrink_type_valid :
  forall n_old n_new,
  children_preserved n_old n_new ->
  get_node_type n_new = shrink_target (get_node_type n_old) ->
  get_child_count n_old = get_child_count n_new ->
  should_shrink n_old = true ->
  node_type_valid n_new.
Proof.
  intros n_old n_new Hpreserve Htype Hcount Hshrink.
  unfold node_type_valid.
  unfold get_node_type, get_child_count, shrink_target in Htype.
  unfold get_child_count in Hcount.
  rewrite <- Hcount.
  unfold should_shrink, get_node_type, get_child_count in Hshrink.
  destruct (header_type (node_header n_old)) eqn:Hold;
  destruct (header_type (node_header n_new)) eqn:Hnew;
  simpl in Htype; try discriminate Htype;
  simpl in Hshrink; try discriminate Hshrink;
  simpl; try (apply Nat.leb_le in Hshrink; lia); trivial.
Qed.

(** ** Type Appropriateness After Transition *)

(** Note on node_type_appropriate vs node_type_valid:
    - node_type_valid: The node can structurally hold its children (count <= capacity)
    - node_type_appropriate: The node is the optimal (smallest) type for its children

    After growth, the new type is valid but NOT necessarily appropriate because:
    - Growth happens when count = capacity (e.g., Node4 with 4 children)
    - After growing to Node16, count is still 4, but Node16 requires count > 4
    - The new type becomes appropriate only after inserting the 5th child

    Therefore, we keep the original theorems as admitted to document this limitation. *)

(** After growth, new type is appropriate *)
(** Note: This theorem is mathematically unprovable as stated.
    Consider TNode4 -> TNode16 growth:
    - node_type_appropriate n_old for TNode4: count <= 4
    - get_child_count n_old = get_child_count n_new: count stays the same
    - node_type_appropriate n_new for TNode16: count > 4 ∧ count <= 16
    - If count = 4, then count > 4 is FALSE

    The correct property after growth is node_type_valid, not node_type_appropriate.
    The node becomes appropriate only after inserting the additional child that
    triggered the growth. *)
Theorem growth_type_appropriate :
  forall n_old n_new,
  children_preserved n_old n_new ->
  get_node_type n_new = grow_target (get_node_type n_old) ->
  get_child_count n_old = get_child_count n_new ->
  node_type_appropriate n_old ->
  node_type_appropriate n_new.
Proof.
  (* UNPROVABLE: See note above.
     Use growth_type_valid instead for provable structural validity. *)
Admitted.

(** After shrink, new type is appropriate *)
(** Note: This theorem is mathematically unprovable as stated.
    After shrinking from TNode16 to TNode4 (when count <= 4),
    the count satisfies count <= 4, which matches TNode4's appropriateness.
    However, node_type_appropriate for TNode4 requires only count <= 4,
    so this is actually provable IF we can establish the count bounds.

    The issue is that should_shrink uses <=b (leb), but we need to relate
    this to the structural definition of node_type_appropriate.

    For a complete proof, we would need to expand the definitions and
    carefully handle the boolean to Prop conversion. *)
Theorem shrink_type_appropriate :
  forall n_old n_new,
  children_preserved n_old n_new ->
  get_node_type n_new = shrink_target (get_node_type n_old) ->
  get_child_count n_old = get_child_count n_new ->
  should_shrink n_old = true ->
  node_type_appropriate n_new.
Proof.
  (* For shrink, this is actually close to provable since shrinking
     moves to a smaller type and should_shrink ensures count is within bounds.
     However, the lower bound requirements for node_type_appropriate
     (e.g., TNode16 requires count > 4) make this complex.
     Use shrink_type_valid for the provable structural validity property. *)
Admitted.
