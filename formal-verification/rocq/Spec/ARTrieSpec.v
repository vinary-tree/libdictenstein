(** * ARTrieSpec: ARTrie-Specific Specification

    This module defines the ARTrie specification, which extends the
    abstract map specification with ARTrie-specific structure.

    The ARTrie is a persistent adaptive radix trie with:
    - Adaptive node types (Node4, Node16, Node48, Node256)
    - Path compression (up to 12 bytes per node)
    - B-trie leaf buckets
*)

Require Import Coq.Lists.List.
Require Import Coq.Arith.Arith.
Require Import Coq.Bool.Bool.
Require Import Coq.micromega.Lia.
Require Import ARTrie.Spec.MapSpec.
Require Import ARTrie.Model.Key.
Require Import ARTrie.Model.NodeTypes.
Require Import ARTrie.Model.Bucket.
Require Import ARTrie.Model.PathCompression.
Import ListNotations.

(** ** ARTrie State *)

(** Abstract trie structure *)
Record ARTrie := mkARTrie {
  trie_root : option nat;           (* Root node ID, None if empty *)
  trie_nodes : nat -> option Node;  (* Node storage *)
  trie_buckets : nat -> option Bucket;  (* Bucket storage *)
  trie_next_node : nat;             (* Next available node ID *)
  trie_next_bucket : nat;           (* Next available bucket ID *)
  trie_count : nat                  (* Number of entries *)
}.

(** ** Empty Trie *)

Definition empty_trie : ARTrie :=
  mkARTrie None (fun _ => None) (fun _ => None) 0 0 0.

(** ** Node Access *)

Definition get_node (t : ARTrie) (nid : nat) : option Node :=
  trie_nodes t nid.

Definition get_bucket (t : ARTrie) (bid : nat) : option Bucket :=
  trie_buckets t bid.

(** ** Trie Interpretation *)

(** Interpret a child pointer *)
Definition interpret_ptr (t : ARTrie) (ptr : ChildPtr) : option (Node + Bucket) :=
  match ptr with
  | NullPtr => None
  | NodePtr nid =>
      match get_node t nid with
      | None => None
      | Some n => Some (inl n)
      end
  | BucketPtr bid =>
      match get_bucket t bid with
      | None => None
      | Some b => Some (inr b)
      end
  end.

(** ** Traversal State *)

Record TraversalState := mkTraversal {
  trav_key : Key;
  trav_offset : nat;
  trav_current : Node + Bucket;
  trav_path : list (nat * Byte)  (* (node_id, edge_byte) pairs *)
}.

(** ** Lookup Implementation *)

(** Match key against node and return next step *)
Inductive TraversalStep :=
  | StepDescend (child : ChildPtr) (new_offset : nat)
  | StepFound (value : option Value)
  | StepNotFound
  | StepPrefixMismatch (pos : nat).

Definition traverse_node (n : Node) (key : Key) (offset : nat) : TraversalStep :=
  (* First, match the prefix *)
  match match_prefix key offset (node_prefix n) with
  | FullMatchResult =>
      let new_offset := offset + prefix_len (node_prefix n) in
      if Nat.leb (length key) new_offset then
        (* Key consumed: check if node is final *)
        if has_flag (header_flags (node_header n)) FlagFinal then
          (* In a real impl, value would be stored; here we return None *)
          StepFound None
        else
          StepNotFound
      else
        (* More key to consume: descend to child *)
        match nth_error key new_offset with
        | None => StepNotFound
        | Some next_byte =>
            let child := find_child n next_byte in
            if is_null child then
              StepNotFound
            else
              StepDescend child (S new_offset)
        end
  | PartialMatchResult pos _ _ =>
      StepPrefixMismatch pos
  | KeyTooShortResult _ =>
      StepNotFound
  end.

(** Full lookup traversal *)
Fixpoint lookup_aux (t : ARTrie) (key : Key) (offset : nat)
  (current : Node + Bucket) (fuel : nat) : option Value :=
  match fuel with
  | 0 => None  (* Ran out of fuel - shouldn't happen in practice *)
  | S fuel' =>
      match current with
      | inr bucket =>
          (* At a bucket: look up suffix *)
          bucket_lookup bucket (skipn offset key)
      | inl node =>
          match traverse_node node key offset with
          | StepFound v => v
          | StepNotFound => None
          | StepPrefixMismatch _ => None
          | StepDescend child new_offset =>
              match interpret_ptr t child with
              | None => None
              | Some next => lookup_aux t key new_offset next fuel'
              end
          end
      end
  end.

Definition trie_lookup (t : ARTrie) (key : Key) : option Value :=
  match trie_root t with
  | None => None
  | Some root_id =>
      match get_node t root_id with
      | None => None
      | Some root =>
          lookup_aux t key 0 (inl root) (length key + 100)  (* Extra fuel for prefix overhead *)
      end
  end.

(** ** Trie Interpretation as Map *)

(** Convert trie to abstract map *)
Definition interpret_trie (t : ARTrie) : Key -> option Value :=
  fun k => trie_lookup t k.

(** ** Well-formedness *)

(** All nodes are well-formed *)
Definition all_nodes_wf (t : ARTrie) : Prop :=
  forall nid n, trie_nodes t nid = Some n -> wf_node n.

(** All buckets are well-formed *)
Definition all_buckets_wf (t : ARTrie) : Prop :=
  forall bid b, trie_buckets t bid = Some b -> wf_bucket b.

(** No dangling pointers *)
Definition no_dangling_ptrs (t : ARTrie) : Prop :=
  forall nid n b,
    trie_nodes t nid = Some n ->
    find_child n b <> NullPtr ->
      match find_child n b with
      | NodePtr cid => trie_nodes t cid <> None
      | BucketPtr bid => trie_buckets t bid <> None
      | NullPtr => True
      end.

(** Root exists if trie is non-empty *)
Definition root_exists (t : ARTrie) : Prop :=
  trie_count t > 0 <->
  exists rid, trie_root t = Some rid /\ trie_nodes t rid <> None.

(** Combined well-formedness *)
Definition wf_trie (t : ARTrie) : Prop :=
  all_nodes_wf t /\
  all_buckets_wf t /\
  no_dangling_ptrs t /\
  root_exists t.

(** ** Node Type Invariants *)

(** Node type can structurally hold its children.
    This is structural validity - the node has enough capacity for its children.
    Contrast with node_type_appropriate which describes optimal type selection. *)
Definition node_type_valid (n : Node) : Prop :=
  let count := header_num_children (node_header n) in
  match header_type (node_header n) with
  | TNode4 => count <= 4
  | TNode16 => count <= 16
  | TNode48 => count <= 48
  | TNode256 => count <= 256
  | TBucket => True  (* Buckets have different semantics *)
  end.

(** Node type is appropriate (optimal) for child count.
    This means the node is the smallest type that can hold its children.
    Note: This is a stronger property than node_type_valid. *)
Definition node_type_appropriate (n : Node) : Prop :=
  let count := header_num_children (node_header n) in
  match header_type (node_header n) with
  | TNode4 => count <= 4
  | TNode16 => count > 4 /\ count <= 16
  | TNode48 => count > 16 /\ count <= 48
  | TNode256 => count > 48 /\ count <= 256
  | TBucket => True  (* Buckets have different semantics *)
  end.

(** Appropriate implies valid *)
Lemma node_type_appropriate_implies_valid : forall n,
  node_type_appropriate n -> node_type_valid n.
Proof.
  intros n Happ.
  unfold node_type_appropriate, node_type_valid in *.
  destruct (header_type (node_header n)); try lia; auto.
Qed.

Definition all_nodes_type_appropriate (t : ARTrie) : Prop :=
  forall nid n, trie_nodes t nid = Some n -> node_type_appropriate n.

(** ** Count Invariant *)

(** Helper to enumerate keys - would require a finite key domain in practice *)
Parameter enumerate_keys : ARTrie -> list Key.

(** Entry count matches actual entries *)
Definition count_correct (t : ARTrie) : Prop :=
  trie_count t = length (filter (fun k =>
    match trie_lookup t k with
    | Some _ => true
    | None => false
    end) (enumerate_keys t)).

(** Simplified: count matches number of keys with values *)
Definition count_matches_entries (t : ARTrie) (keys : list Key) : Prop :=
  trie_count t = length (filter (fun k =>
    match trie_lookup t k with
    | Some _ => true
    | None => false
    end) keys).

(** ** Main Theorems *)

(** Empty trie is well-formed *)
Theorem empty_trie_wf : wf_trie empty_trie.
Proof.
  unfold wf_trie, empty_trie. simpl.
  split; [| split; [| split]].
  - unfold all_nodes_wf. intros. discriminate.
  - unfold all_buckets_wf. intros. discriminate.
  - unfold no_dangling_ptrs. intros. discriminate.
  - unfold root_exists. split; intro H.
    + simpl in H. lia.
    + destruct H as [rid [H1 H2]]. discriminate.
Qed.

(** Empty trie lookup returns None *)
Theorem empty_trie_lookup : forall k, trie_lookup empty_trie k = None.
Proof.
  intros k. unfold trie_lookup, empty_trie. simpl. reflexivity.
Qed.

(** ** ARTrie Insert/Delete Obligations *)

(** Insert/delete are declared operations until an executable Operations
    module is connected here. Their MapSpec refinement laws are tracked as
    obligations rather than axioms, so importing this file no longer assumes
    correctness for an implementation that is not present. *)

Parameter trie_insert : ARTrie -> MapSpec.Key -> Value -> ARTrie.
Parameter trie_delete : ARTrie -> MapSpec.Key -> ARTrie.

Definition trie_insert_correct_obligation : Prop := forall t k v k',
  interpret_trie (trie_insert t k v) k' =
    if MapSpec.key_eq_dec k k' then Some v else interpret_trie t k'.

Definition trie_delete_correct_obligation : Prop := forall t k k',
  interpret_trie (trie_delete t k) k' =
    if MapSpec.key_eq_dec k k' then None else interpret_trie t k'.

Definition ARTrieMapImpl_obligation : Prop :=
  trie_insert_correct_obligation /\ trie_delete_correct_obligation.
