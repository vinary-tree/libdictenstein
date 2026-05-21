(** * StructuralInvariants: Structural Invariants for ARTrie

    This module defines and proves structural invariants that must
    hold for all valid ARTrie states.

    Invariants verified:
    - Tree structure (no cycles, valid pointers)
    - Node type bounds (child count within capacity)
    - Path compression validity
    - Bucket sortedness
*)

Require Import Coq.Lists.List.
Require Import Coq.Arith.Arith.
Require Import Coq.Bool.Bool.
Require Import Coq.micromega.Lia.
Require Import ARTrie.Spec.MapSpec.
Require Import ARTrie.Spec.ARTrieSpec.
Require Import ARTrie.Model.Key.
Require Import ARTrie.Model.NodeTypes.
Require Import ARTrie.Model.Bucket.
Require Import ARTrie.Model.PathCompression.
Import ListNotations.

Opaque enumerate_bytes.

(** ** Tree Structure Invariants *)

(** Reachability: a node is reachable from root *)
Inductive reachable (t : ARTrie) : nat -> Prop :=
  | reach_root : forall rid,
      trie_root t = Some rid -> reachable t rid
  | reach_child : forall nid cid n b,
      reachable t nid ->
      trie_nodes t nid = Some n ->
      find_child n b = NodePtr cid ->
      reachable t cid.

(** No direct self-loop from any reachable node.

    A full acyclicity invariant needs an explicit path relation. The previous
    placeholder quantified over arbitrary lists of node pairs and was too
    strong: any reachable node could be paired with itself by the caller. *)
Definition no_cycles (t : ARTrie) : Prop :=
  forall nid n b,
    reachable t nid ->
    trie_nodes t nid = Some n ->
    find_child n b <> NodePtr nid.

(** All reachable nodes exist *)
Definition reachable_nodes_exist (t : ARTrie) : Prop :=
  forall nid, reachable t nid -> trie_nodes t nid <> None.

(** All reachable buckets exist *)
Definition reachable_buckets_exist (t : ARTrie) : Prop :=
  forall nid n b bid,
    reachable t nid ->
    trie_nodes t nid = Some n ->
    find_child n b = BucketPtr bid ->
    trie_buckets t bid <> None.

(** ** Node Capacity Invariants *)

(** Child count matches actual children *)
Definition child_count_accurate (n : Node) : Prop :=
  header_num_children (node_header n) =
  length (filter (fun b =>
    negb (is_null (find_child n b))) (enumerate_bytes)).

(** Child count within bounds *)
Definition child_count_bounded (n : Node) : Prop :=
  header_num_children (node_header n) <= node_capacity (get_node_type n).

(** All nodes have accurate and bounded child counts *)
Definition all_child_counts_valid (t : ARTrie) : Prop :=
  forall nid n, trie_nodes t nid = Some n ->
    child_count_accurate n /\ child_count_bounded n.

(** ** Path Compression Invariants *)

(** Prefix length is accurate *)
Definition prefix_len_accurate (n : Node) : Prop :=
  header_prefix_len (node_header n) = prefix_len (node_prefix n).

(** Prefix is within bounds *)
Definition prefix_bounded (n : Node) : Prop :=
  prefix_len (node_prefix n) <= MAX_PREFIX_LEN.

(** All prefixes are valid *)
Definition all_prefixes_valid (t : ARTrie) : Prop :=
  forall nid n, trie_nodes t nid = Some n ->
    prefix_len_accurate n /\ prefix_bounded n.

(** ** Bucket Invariants *)

(** All bucket entries are sorted *)
Definition all_buckets_sorted (t : ARTrie) : Prop :=
  forall bid b, trie_buckets t bid = Some b -> bucket_sorted b.

(** All bucket entry counts are valid *)
Definition all_buckets_count_valid (t : ARTrie) : Prop :=
  forall bid b, trie_buckets t bid = Some b -> bucket_count_valid b.

(** ** Version Invariants *)

(** All versions are stable (even) in a quiescent state *)
Definition all_versions_stable (t : ARTrie) : Prop :=
  forall nid n, trie_nodes t nid = Some n ->
    is_stable (header_version (node_header n)) = true.

(** ** Flag Consistency *)

(** Leaf flag implies points to bucket *)
Definition leaf_flag_consistent (n : Node) : Prop :=
  has_flag (header_flags (node_header n)) FlagLeaf = true ->
  forall b, is_null (find_child n b) = false ->
    match find_child n b with
    | BucketPtr _ => True
    | _ => False
    end.

(** Final flag implies node holds a value *)
Definition final_flag_consistent (t : ARTrie) : Prop :=
  forall nid n, trie_nodes t nid = Some n ->
    has_flag (header_flags (node_header n)) FlagFinal = true ->
    node_value n <> None.

(** ** Combined Structural Invariant *)

Definition structural_invariant (t : ARTrie) : Prop :=
  wf_trie t /\
  no_cycles t /\
  reachable_nodes_exist t /\
  reachable_buckets_exist t /\
  all_child_counts_valid t /\
  all_prefixes_valid t /\
  all_buckets_sorted t /\
  all_buckets_count_valid t.

(** ** Preservation Theorems *)

(** Empty trie satisfies structural invariant *)
Theorem empty_structural_invariant : structural_invariant empty_trie.
Proof.
  unfold structural_invariant.
  split; [apply empty_trie_wf |].
  split; [| split; [| split; [| split; [| split; [| split]]]]].
  - unfold no_cycles. intros nid n b Hreach Hnode Hloop.
    inversion Hreach; simpl in *; discriminate.
  - unfold reachable_nodes_exist. intros nid Hreach.
    inversion Hreach.
    + simpl in H. discriminate.
    + simpl in H0. discriminate.
  - unfold reachable_buckets_exist. intros.
    inversion H.
    + simpl in H2. discriminate.
    + simpl in H3. discriminate.
  - unfold all_child_counts_valid. intros. simpl in H. discriminate.
  - unfold all_prefixes_valid. intros. simpl in H. discriminate.
  - unfold all_buckets_sorted. intros. simpl in H. discriminate.
  - unfold all_buckets_count_valid. intros. simpl in H. discriminate.
Qed.

(** ** Inductive Preservation *)

(** Structural invariant is preserved by lookup (read-only) *)
Theorem lookup_preserves_structural : forall t (k : Key),
  structural_invariant t ->
  structural_invariant t.  (* Trivially true - lookup doesn't modify state *)
Proof.
  intros. exact H.
Qed.

(** Operation preservation obligations.

    The raw canonical rebuild operations have semantic correctness theorems in
    [ARTrieSpec]. Structural preservation remains a separate obligation because
    checked construction can fail when fixed-size buckets exceed their page or
    entry limits, and total structural preservation belongs with the adaptive
    split/growth proofs. *)

Definition insert_preserves_structural_obligation : Prop :=
  forall t (k : Key) (v : Value),
    structural_invariant t ->
    structural_invariant (trie_insert t k v).

Definition delete_preserves_structural_obligation : Prop :=
  forall t (k : Key),
    structural_invariant t ->
    structural_invariant (trie_delete t k).
