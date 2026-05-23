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
Require Import Coq.Sorting.Sorted.
Require Import Coq.micromega.Lia.
Require Import ARTrie.Spec.MapSpec.
Require Import ARTrie.Model.Key.
Require Import ARTrie.Model.NodeTypes.
Require Import ARTrie.Model.Bucket.
Require Import ARTrie.Model.PathCompression.
Import ListNotations.

Opaque enumerate_bytes.

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
        (* Key consumed: return the value stored at this node, if any. *)
        StepFound (node_value n)
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

Fixpoint enumerate_bucket_keys (prefix : Key) (entries : list BucketEntry)
  : list Key :=
  match entries with
  | [] => []
  | e :: rest =>
      let keys := enumerate_bucket_keys prefix rest in
      match entry_value e with
      | Some _ => (prefix ++ entry_suffix e) :: keys
      | None => keys
      end
  end.

Fixpoint enumerate_keys_from (t : ARTrie) (prefix : Key)
  (current : Node + Bucket) (fuel : nat) : list Key :=
  match fuel with
  | 0 => []
  | S fuel' =>
      match current with
      | inr bucket =>
          enumerate_bucket_keys prefix (bucket_entries bucket)
      | inl node =>
          let node_key := prefix ++ prefix_bytes (node_prefix node) in
          let self_key :=
            match node_value node with
            | Some _ => [node_key]
            | None => []
            end in
          let child_keys :=
            (fix enumerate_children (bytes : list Byte) : list Key :=
              match bytes with
              | [] => []
              | b :: rest =>
                  let keys_for_child :=
                    match interpret_ptr t (find_child node b) with
                    | Some next =>
                        enumerate_keys_from t (node_key ++ [b]) next fuel'
                    | None => []
                    end in
                  keys_for_child ++ enumerate_children rest
              end) enumerate_bytes in
          self_key ++ child_keys
      end
  end.

Definition enumerate_keys (t : ARTrie) : list Key :=
  match trie_root t with
  | None => []
  | Some root_id =>
      match get_node t root_id with
      | Some root => enumerate_keys_from t [] (inl root)
          (trie_next_node t + trie_next_bucket t + 1)
      | None => []
      end
  end.

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

(** ** Canonical Rebuild Insert/Delete *)

Definition KeyValue := (Key * Value)%type.

Fixpoint kv_lookup (entries : list KeyValue) (k : Key) : option Value :=
  match entries with
  | [] => None
  | (k0, v0) :: rest =>
      if key_eqb k0 k then Some v0 else kv_lookup rest k
  end.

Fixpoint kv_delete (entries : list KeyValue) (k : Key) : list KeyValue :=
  match entries with
  | [] => []
  | (k0, v0) :: rest =>
      if key_eqb k0 k
      then kv_delete rest k
      else (k0, v0) :: kv_delete rest k
  end.

Definition kv_upsert (entries : list KeyValue) (k : Key) (v : Value)
  : list KeyValue :=
  (k, v) :: kv_delete entries k.

Lemma kv_lookup_delete_same : forall entries k,
  kv_lookup (kv_delete entries k) k = None.
Proof.
  induction entries as [| [k0 v0] rest IH]; intros k; simpl.
  - reflexivity.
  - destruct (key_eqb k0 k) eqn:Hsame; simpl.
    + apply IH.
    + rewrite Hsame. apply IH.
Qed.

Lemma kv_lookup_delete_other : forall entries k k',
  k <> k' ->
  kv_lookup (kv_delete entries k) k' = kv_lookup entries k'.
Proof.
  induction entries as [| [k0 v0] rest IH]; intros k k' Hneq; simpl.
  - reflexivity.
  - destruct (key_eqb k0 k) eqn:Hdelete.
    + apply key_eqb_eq in Hdelete. subst k0.
      destruct (key_eqb k k') eqn:Hlookup.
      * apply key_eqb_eq in Hlookup. contradiction.
      * apply IH. exact Hneq.
    + simpl.
      destruct (key_eqb k0 k'); [reflexivity |].
      apply IH. exact Hneq.
Qed.

Lemma kv_lookup_upsert_same : forall entries k v,
  kv_lookup (kv_upsert entries k v) k = Some v.
Proof.
  intros entries k v.
  unfold kv_upsert. simpl.
  rewrite key_eqb_refl. reflexivity.
Qed.

Lemma kv_lookup_upsert_other : forall entries k v k',
  k <> k' ->
  kv_lookup (kv_upsert entries k v) k' = kv_lookup entries k'.
Proof.
  intros entries k v k' Hneq.
  unfold kv_upsert. simpl.
  destruct (key_eqb k k') eqn:Hsame.
  - apply key_eqb_eq in Hsame. contradiction.
  - apply kv_lookup_delete_other. exact Hneq.
Qed.

Definition kv_entry_le (e1 e2 : KeyValue) : Prop :=
  key_compare (fst e1) (fst e2) <> Gt.

Fixpoint kv_insert_ordered (entry : KeyValue) (entries : list KeyValue)
  : list KeyValue :=
  match entries with
  | [] => [entry]
  | current :: rest =>
      match key_compare (fst entry) (fst current) with
      | Lt => entry :: current :: rest
      | Eq => entry :: rest
      | Gt => current :: kv_insert_ordered entry rest
      end
  end.

Definition kv_insert_normalized (entry : KeyValue) (entries : list KeyValue)
  : list KeyValue :=
  kv_insert_ordered entry (kv_delete entries (fst entry)).

Definition kv_normalize (entries : list KeyValue) : list KeyValue :=
  fold_right kv_insert_normalized [] entries.

Lemma kv_lookup_insert_ordered_same : forall entries k v,
  kv_lookup (kv_insert_ordered (k, v) entries) k = Some v.
Proof.
  induction entries as [| [k0 v0] rest IH]; intros k v; simpl.
  - rewrite key_eqb_refl. reflexivity.
  - destruct (key_compare k k0) eqn:Hcmp; simpl.
    + rewrite key_eqb_refl. reflexivity.
    + rewrite key_eqb_refl. reflexivity.
    + destruct (key_eqb k0 k) eqn:Hsame.
      * apply key_eqb_eq in Hsame. subst k0.
        rewrite key_compare_refl in Hcmp. discriminate.
      * apply IH.
Qed.

Lemma kv_lookup_insert_ordered_other : forall entries k v k',
  k <> k' ->
  kv_lookup (kv_insert_ordered (k, v) entries) k' = kv_lookup entries k'.
Proof.
  induction entries as [| [k0 v0] rest IH]; intros k v k' Hneq; simpl.
  - destruct (key_eqb k k') eqn:Hsame.
    + apply key_eqb_eq in Hsame. contradiction.
    + reflexivity.
  - destruct (key_compare k k0) eqn:Hcmp; simpl.
    + apply key_compare_eq in Hcmp. subst k0.
      destruct (key_eqb k k') eqn:Hsame.
      * apply key_eqb_eq in Hsame. contradiction.
      * reflexivity.
    + destruct (key_eqb k k') eqn:Hsame.
      * apply key_eqb_eq in Hsame. contradiction.
      * reflexivity.
    + destruct (key_eqb k0 k'); [reflexivity|].
      apply IH. exact Hneq.
Qed.

Lemma kv_lookup_insert_normalized_same : forall entries k v,
  kv_lookup (kv_insert_normalized (k, v) entries) k = Some v.
Proof.
  intros entries k v.
  unfold kv_insert_normalized.
  apply kv_lookup_insert_ordered_same.
Qed.

Lemma kv_lookup_insert_normalized_other : forall entries k v k',
  k <> k' ->
  kv_lookup (kv_insert_normalized (k, v) entries) k' = kv_lookup entries k'.
Proof.
  intros entries k v k' Hneq.
  unfold kv_insert_normalized.
  rewrite kv_lookup_insert_ordered_other by exact Hneq.
  apply kv_lookup_delete_other. exact Hneq.
Qed.

Lemma kv_lookup_normalize : forall entries k,
  kv_lookup (kv_normalize entries) k = kv_lookup entries k.
Proof.
  induction entries as [| [k0 v0] rest IH]; intros k; simpl.
  - reflexivity.
  - destruct (key_eqb k0 k) eqn:Hsame.
    + apply key_eqb_eq in Hsame. subst k0.
      rewrite kv_lookup_insert_normalized_same. reflexivity.
    + rewrite kv_lookup_insert_normalized_other.
      * rewrite IH. reflexivity.
      * intro Heq. subst k. rewrite key_eqb_refl in Hsame. discriminate.
Qed.

Lemma kv_entry_le_trans : forall e1 e2 e3,
  kv_entry_le e1 e2 ->
  kv_entry_le e2 e3 ->
  kv_entry_le e1 e3.
Proof.
  intros e1 e2 e3 H12 H23.
  unfold kv_entry_le in *.
  eapply key_compare_le_trans; eauto.
Qed.

Lemma kv_Forall_entry_le_trans : forall e1 e2 rest,
  kv_entry_le e1 e2 ->
  Forall (kv_entry_le e2) rest ->
  Forall (kv_entry_le e1) rest.
Proof.
  intros e1 e2 rest He12 Hall.
  induction Hall as [| e3 rest He23 _ IH].
  - constructor.
  - constructor.
    + eapply kv_entry_le_trans; eauto.
    + exact IH.
Qed.

Lemma kv_HdRel_key_eq_left : forall e1 e2 rest,
  fst e1 = fst e2 ->
  HdRel kv_entry_le e2 rest ->
  HdRel kv_entry_le e1 rest.
Proof.
  intros e1 e2 [| x xs] Hkey Hhd; constructor.
  inversion Hhd as [| ? ? Hle]; subst.
  unfold kv_entry_le in *. simpl in *.
  rewrite Hkey. exact Hle.
Qed.

Lemma kv_insert_ordered_hdrel : forall entry head rest,
  kv_entry_le head entry ->
  HdRel kv_entry_le head rest ->
  HdRel kv_entry_le head (kv_insert_ordered entry rest).
Proof.
  intros entry head [| x xs] Hhead_entry Hhd; simpl.
  - constructor. exact Hhead_entry.
  - destruct (key_compare (fst entry) (fst x)); constructor.
    + exact Hhead_entry.
    + exact Hhead_entry.
    + inversion Hhd as [| ? ? Hhead_x]; subst. exact Hhead_x.
Qed.

Lemma kv_insert_ordered_sorted : forall entries entry,
  Sorted kv_entry_le entries ->
  Sorted kv_entry_le (kv_insert_ordered entry entries).
Proof.
  intros entries entry Hsorted.
  induction Hsorted as [| head rest Hsorted IH Hhd]; simpl.
  - constructor; [constructor|constructor].
  - destruct (key_compare (fst entry) (fst head)) eqn:Hcmp.
    + apply key_compare_eq in Hcmp.
      constructor.
      * exact Hsorted.
      * eapply kv_HdRel_key_eq_left; eauto.
    + constructor.
      * constructor; assumption.
      * constructor. unfold kv_entry_le. rewrite Hcmp. discriminate.
    + constructor.
      * exact IH.
      * apply kv_insert_ordered_hdrel.
        -- unfold kv_entry_le. apply key_compare_gt_flip_le. exact Hcmp.
        -- exact Hhd.
Qed.

Lemma kv_sorted_forall_tail : forall e rest,
  Sorted kv_entry_le (e :: rest) ->
  Forall (kv_entry_le e) rest.
Proof.
  intros e rest Hsorted.
  remember (e :: rest) as entries eqn:Hentries.
  revert e rest Hentries.
  induction Hsorted as [| x xs Hsorted IH Hhd]; intros e rest Hentries.
  - discriminate.
  - injection Hentries as He Hrest. subst x xs.
    destruct rest as [| y ys].
    + constructor.
    + inversion Hhd as [| ? ? Hey]; subst.
      constructor; [exact Hey|].
      specialize (IH y ys eq_refl) as Htail_forall.
      eapply kv_Forall_entry_le_trans; eauto.
Qed.

Lemma kv_Forall_delete : forall P entries k,
  Forall P entries ->
  Forall P (kv_delete entries k).
Proof.
  intros P entries k Hall.
  induction Hall as [| [k0 v0] rest Hp _ IH]; simpl.
  - constructor.
  - destruct (key_eqb k0 k); [exact IH|constructor; assumption].
Qed.

Lemma kv_HdRel_from_Forall : forall e rest,
  Forall (kv_entry_le e) rest ->
  HdRel kv_entry_le e rest.
Proof.
  intros e [| x xs] Hall; constructor.
  inversion Hall as [| ? ? Hle]; subst. exact Hle.
Qed.

Lemma kv_delete_sorted : forall entries k,
  Sorted kv_entry_le entries ->
  Sorted kv_entry_le (kv_delete entries k).
Proof.
  intros entries k Hsorted.
  induction Hsorted as [| head rest Hsorted IH Hhd].
  - constructor.
  - destruct head as [k0 v0]. simpl.
    destruct (key_eqb k0 k) eqn:Hdelete.
    + exact IH.
    + constructor.
      * exact IH.
      * apply kv_HdRel_from_Forall.
        apply kv_Forall_delete.
        apply kv_sorted_forall_tail.
        constructor; assumption.
Qed.

Lemma kv_delete_key_not_in : forall entries k,
  ~ In k (map fst (kv_delete entries k)).
Proof.
  induction entries as [| [k0 v0] rest IH]; intros k; simpl.
  - intro Hin. exact Hin.
  - destruct (key_eqb k0 k) eqn:Hsame; simpl.
    + apply IH.
    + intro Hin. destruct Hin as [Hin | Hin].
      * subst k0. rewrite key_eqb_refl in Hsame. discriminate.
      * apply (IH k). exact Hin.
Qed.

Lemma kv_delete_keys_subset : forall entries k key,
  In key (map fst (kv_delete entries k)) ->
  In key (map fst entries).
Proof.
  induction entries as [| [k0 v0] rest IH]; intros k key Hin; simpl in *.
  - contradiction.
  - destruct (key_eqb k0 k) eqn:Hsame; simpl in Hin.
    + right. apply IH with (k := k). exact Hin.
    + destruct Hin as [Hin | Hin].
      * left. exact Hin.
      * right. apply IH with (k := k). exact Hin.
Qed.

Lemma kv_delete_nodup : forall entries k,
  NoDup (map fst entries) ->
  NoDup (map fst (kv_delete entries k)).
Proof.
  induction entries as [| [k0 v0] rest IH]; intros k Hnodup; simpl.
  - constructor.
  - inversion Hnodup as [| x xs Hnotin Htail]; subst.
    destruct (key_eqb k0 k) eqn:Hsame; simpl.
    + apply IH. exact Htail.
    + constructor.
      * intro Hin. apply Hnotin.
        apply kv_delete_keys_subset with (k := k). exact Hin.
      * apply IH. exact Htail.
Qed.

Lemma kv_insert_ordered_keys_cases : forall entries entry key,
  In key (map fst (kv_insert_ordered entry entries)) ->
  key = fst entry \/ In key (map fst entries).
Proof.
  induction entries as [| current rest IH]; intros entry key Hin; simpl in *.
  - destruct Hin as [Hin | []]. left. symmetry. exact Hin.
  - destruct (key_compare (fst entry) (fst current)); simpl in Hin.
    + destruct Hin as [Hin | Hin].
      * left. symmetry. exact Hin.
      * right. right. exact Hin.
    + destruct Hin as [Hin | Hin].
      * left. symmetry. exact Hin.
      * right. destruct Hin as [Hin | Hin].
        -- left. exact Hin.
        -- right. exact Hin.
    + destruct Hin as [Hin | Hin].
      * right. left. exact Hin.
      * apply IH in Hin as [Hin | Hin].
        -- left. exact Hin.
        -- right. right. exact Hin.
Qed.

Lemma kv_insert_ordered_nodup : forall entries entry,
  ~ In (fst entry) (map fst entries) ->
  NoDup (map fst entries) ->
  NoDup (map fst (kv_insert_ordered entry entries)).
Proof.
  induction entries as [| current rest IH]; intros entry Hnotin Hnodup; simpl.
  - constructor; [intro H; inversion H|constructor].
  - inversion Hnodup as [| x xs Hhead_notin Htail]; subst.
    destruct (key_compare (fst entry) (fst current)) eqn:Hcmp; simpl.
    + apply key_compare_eq in Hcmp. exfalso.
      apply Hnotin. left. symmetry. exact Hcmp.
    + constructor.
      * exact Hnotin.
      * constructor; assumption.
    + constructor.
      * intro Hin. apply kv_insert_ordered_keys_cases in Hin as [Hin | Hin].
        -- apply Hnotin. left. exact Hin.
        -- apply Hhead_notin. exact Hin.
      * apply IH.
        -- intro Hin. apply Hnotin. right. exact Hin.
        -- exact Htail.
Qed.

Lemma kv_insert_normalized_sorted : forall entries entry,
  Sorted kv_entry_le entries ->
  Sorted kv_entry_le (kv_insert_normalized entry entries).
Proof.
  intros entries entry Hsorted.
  unfold kv_insert_normalized.
  apply kv_insert_ordered_sorted.
  apply kv_delete_sorted. exact Hsorted.
Qed.

Lemma kv_insert_normalized_nodup : forall entries entry,
  NoDup (map fst entries) ->
  NoDup (map fst (kv_insert_normalized entry entries)).
Proof.
  intros entries entry Hnodup.
  unfold kv_insert_normalized.
  apply kv_insert_ordered_nodup.
  - apply kv_delete_key_not_in.
  - apply kv_delete_nodup. exact Hnodup.
Qed.

Lemma kv_normalize_sorted : forall entries,
  Sorted kv_entry_le (kv_normalize entries).
Proof.
  induction entries as [| entry rest IH]; simpl.
  - constructor.
  - apply kv_insert_normalized_sorted. exact IH.
Qed.

Lemma kv_normalize_nodup : forall entries,
  NoDup (map fst (kv_normalize entries)).
Proof.
  induction entries as [| entry rest IH]; simpl.
  - constructor.
  - apply kv_insert_normalized_nodup. exact IH.
Qed.

Fixpoint entries_of_keys (t : ARTrie) (keys : list Key) : list KeyValue :=
  match keys with
  | [] => []
  | k :: rest =>
      let entries := entries_of_keys t rest in
      match trie_lookup t k with
      | Some v => kv_upsert entries k v
      | None => entries
      end
  end.

Definition entries_of_trie (t : ARTrie) : list KeyValue :=
  entries_of_keys t (enumerate_keys t).

Definition entries_of_trie_complete (t : ARTrie) : Prop :=
  forall k, kv_lookup (entries_of_trie t) k = trie_lookup t k.

Fixpoint bucket_entries_for (first : Byte) (entries : list KeyValue)
  : list BucketEntry :=
  match entries with
  | [] => []
  | ([], _) :: rest => bucket_entries_for first rest
  | ((b :: suffix), v) :: rest =>
      if byte_eqb b first
      then mkEntry suffix (Some v) :: bucket_entries_for first rest
      else bucket_entries_for first rest
  end.

Definition canonical_entries_wf (entries : list KeyValue) : Prop :=
  NoDup (map fst entries) /\ Sorted kv_entry_le entries.

Lemma kv_normalize_canonical_entries_wf : forall entries,
  canonical_entries_wf (kv_normalize entries).
Proof.
  intro entries.
  split.
  - apply kv_normalize_nodup.
  - apply kv_normalize_sorted.
Qed.

Lemma bucket_entries_for_suffix_in_entries : forall entries first suffix,
  In suffix (map entry_suffix (bucket_entries_for first entries)) ->
  In (first :: suffix) (map fst entries).
Proof.
  induction entries as [| [[| b k] v] rest IH]; intros first suffix Hin; simpl in *.
  - contradiction.
  - apply IH in Hin. right. exact Hin.
  - destruct (byte_eqb b first) eqn:Hbyte.
    + simpl in Hin. destruct Hin as [Hin | Hin].
      * apply byte_eqb_eq in Hbyte. subst b.
        left. f_equal. exact Hin.
      * right. apply IH. exact Hin.
    + right. apply IH. exact Hin.
Qed.

Lemma bucket_entries_for_unique : forall entries first,
  NoDup (map fst entries) ->
  NoDup (map entry_suffix (bucket_entries_for first entries)).
Proof.
  induction entries as [| [[| b k] v] rest IH]; intros first Hnodup; simpl.
  - constructor.
  - inversion Hnodup as [| x xs Hnotin Htail]; subst.
    apply IH. exact Htail.
  - inversion Hnodup as [| x xs Hnotin Htail]; subst.
    destruct (byte_eqb b first) eqn:Hbyte; simpl.
    + constructor.
      * intro Hin.
        apply Hnotin.
        apply bucket_entries_for_suffix_in_entries in Hin.
        apply byte_eqb_eq in Hbyte. subst b.
        exact Hin.
      * apply IH. exact Htail.
    + apply IH. exact Htail.
Qed.

Lemma bucket_entries_for_hdrel : forall first suffix value rest,
  Forall (kv_entry_le (first :: suffix, value)) rest ->
  HdRel entry_le (mkEntry suffix (Some value))
    (bucket_entries_for first rest).
Proof.
  intros first suffix value rest Hall.
  induction rest as [| [[| b k] v] rest IH]; simpl.
  - constructor.
  - inversion Hall as [| ? ? _ Hall_tail]; subst.
    apply IH. exact Hall_tail.
  - inversion Hall as [| ? ? Hle Hall_tail]; subst.
    destruct (byte_eqb b first) eqn:Hbyte.
    + apply byte_eqb_eq in Hbyte. subst b.
      constructor.
      unfold entry_le, kv_entry_le in *. simpl in *.
      rewrite Nat.compare_refl in Hle. exact Hle.
    + apply IH. exact Hall_tail.
Qed.

Lemma bucket_entries_for_sorted : forall entries first,
  Sorted kv_entry_le entries ->
  Sorted entry_le (bucket_entries_for first entries).
Proof.
  intros entries first Hsorted.
  induction Hsorted as [| [[| b k] v] rest Hsorted IH Hhd]; simpl.
  - constructor.
  - exact IH.
  - destruct (byte_eqb b first) eqn:Hbyte.
    + apply byte_eqb_eq in Hbyte. subst b.
      constructor.
      * exact IH.
      * apply bucket_entries_for_hdrel.
        apply kv_sorted_forall_tail.
        constructor; assumption.
    + exact IH.
Qed.

Definition canonical_bucket (first : Byte) (entries : list KeyValue) : Bucket :=
  let bucket_entries := bucket_entries_for first entries in
  let size := bucket_size_for_entries bucket_entries in
  mkBucket bucket_entries size (BUCKET_PAGE_SIZE - size).

Definition canonical_buckets (entries : list KeyValue)
  : nat -> option Bucket :=
  fun bid =>
    match byte_of_nat_option bid with
    | Some first => Some (canonical_bucket first entries)
    | None => None
    end.

Definition canonical_root (entries : list KeyValue) : Node :=
  let value := kv_lookup entries [] in
  let flags :=
    match value with
    | Some _ => [FlagFinal]
    | None => []
    end in
  mkNode
    (mkHeader TNode256 0 flags 256 0)
    empty_prefix
    value
    (DataNode256 (mkNode256 (fun b => BucketPtr (byte_val b)))).

Definition build_canonical_trie (entries : list KeyValue) : ARTrie :=
  match entries with
  | [] => empty_trie
  | _ :: _ =>
      mkARTrie
        (Some 0)
        (fun nid => if Nat.eqb nid 0 then Some (canonical_root entries) else None)
        (canonical_buckets entries)
        1
        256
        (length entries)
  end.

Lemma bucket_entries_for_lookup : forall entries first suffix,
  entries_lookup (bucket_entries_for first entries) suffix =
  kv_lookup entries (first :: suffix).
Proof.
  induction entries as [| [[| b k] v] rest IH]; intros first suffix; simpl.
  - reflexivity.
  - apply IH.
  - destruct (byte_eqb b first) eqn:Hbyte.
    + simpl.
      rewrite (key_eqb_sym suffix k).
      rewrite (IH first suffix). reflexivity.
    + simpl. apply IH.
Qed.

Lemma canonical_bucket_lookup : forall entries first suffix,
  bucket_lookup (canonical_bucket first entries) suffix =
  kv_lookup entries (first :: suffix).
Proof.
  intros entries first suffix.
  unfold bucket_lookup, canonical_bucket. simpl.
  apply bucket_entries_for_lookup.
Qed.

Lemma canonical_lookup_correct : forall entries k,
  trie_lookup (build_canonical_trie entries) k = kv_lookup entries k.
Proof.
  intros entries k.
  destruct entries as [| entry entries'].
  - destruct k; reflexivity.
  - destruct k as [| first suffix].
    + unfold trie_lookup, build_canonical_trie. simpl.
      unfold lookup_aux, traverse_node, match_prefix. simpl.
      reflexivity.
    + unfold trie_lookup, build_canonical_trie. simpl.
      unfold lookup_aux, traverse_node, match_prefix. simpl.
      destruct first as [n Hn].
      unfold canonical_buckets, byte_of_nat_option, byte_val. simpl.
      destruct (lt_dec n 256) as [Hlt | Hnlt]; [| lia].
      replace (exist (fun n => n < 256) n Hlt)
        with (exist (fun n => n < 256) n Hn)
        by (f_equal; apply lt_proof_irrelevance).
      destruct (length suffix + 100) as [| fuel'] eqn:Hfuel; [lia | simpl].
      apply canonical_bucket_lookup.
Qed.

(** Checked canonical construction. This is the capacity-aware entry point:
    it refuses to build a canonical trie if any first-byte bucket would exceed
    the bucket page/count limits enforced by [bucket_from_entries]. *)

Definition canonical_bucket_checked (first : Byte) (entries : list KeyValue)
  : option Bucket :=
  let bucket_entries := bucket_entries_for first entries in
  if (length bucket_entries <=? MAX_BUCKET_ENTRIES) &&
     (bucket_size_for_entries bucket_entries <=? BUCKET_PAGE_SIZE)
  then Some (canonical_bucket first entries)
  else None.

Lemma canonical_bucket_checked_wf : forall entries first b,
  NoDup (map entry_suffix (bucket_entries_for first entries)) ->
  canonical_bucket_checked first entries = Some b ->
  wf_bucket b.
Proof.
  intros entries first b Huniq Hchecked.
  unfold canonical_bucket_checked in Hchecked.
  set (bucket_entries := bucket_entries_for first entries) in *.
  destruct ((length bucket_entries <=? MAX_BUCKET_ENTRIES) &&
            (bucket_size_for_entries bucket_entries <=?
             BUCKET_PAGE_SIZE)) eqn:Hfit; [| discriminate].
  apply andb_true_iff in Hfit.
  destruct Hfit as [Hcount Hsize].
  apply Nat.leb_le in Hcount.
  apply Nat.leb_le in Hsize.
  injection Hchecked as Hb. subst b.
  unfold canonical_bucket.
  fold bucket_entries.
  set (size := bucket_size_for_entries bucket_entries).
  change (wf_bucket (mkBucket bucket_entries size (BUCKET_PAGE_SIZE - size))).
  unfold wf_bucket. split.
  - unfold bucket_keys_unique. cbn. exact Huniq.
  - split.
    + unfold bucket_count_valid. cbn. exact Hcount.
    + split.
      * change (size <= BUCKET_PAGE_SIZE /\
                BUCKET_PAGE_SIZE - size = BUCKET_PAGE_SIZE - size).
        split.
        -- unfold size, bucket_size_for_entries in Hsize. exact Hsize.
        -- reflexivity.
      * change (size = BUCKET_HEADER_SIZE + entries_space bucket_entries).
        unfold size, bucket_size_for_entries. reflexivity.
Qed.

Lemma canonical_bucket_checked_wf_sorted : forall entries first b,
  canonical_entries_wf entries ->
  canonical_bucket_checked first entries = Some b ->
  wf_sorted_bucket b.
Proof.
  intros entries first b [Huniq Hsorted] Hchecked.
  split.
  - eapply canonical_bucket_checked_wf.
    + apply bucket_entries_for_unique. exact Huniq.
    + exact Hchecked.
  - unfold canonical_bucket_checked in Hchecked.
    destruct ((length (bucket_entries_for first entries) <=? MAX_BUCKET_ENTRIES) &&
              (bucket_size_for_entries (bucket_entries_for first entries) <=?
               BUCKET_PAGE_SIZE)); [| discriminate].
    injection Hchecked as Hb. subst b.
    unfold bucket_sorted, canonical_bucket. simpl.
    apply bucket_entries_for_sorted. exact Hsorted.
Qed.

Fixpoint canonical_buckets_fit_from (bytes : list Byte)
  (entries : list KeyValue) : bool :=
  match bytes with
  | [] => true
  | first :: rest =>
      match canonical_bucket_checked first entries with
      | Some _ => canonical_buckets_fit_from rest entries
      | None => false
      end
  end.

Definition canonical_buckets_fit (entries : list KeyValue) : bool :=
  canonical_buckets_fit_from enumerate_bytes entries.

Definition canonical_buckets_checked (entries : list KeyValue)
  : nat -> option Bucket :=
  fun bid =>
    match byte_of_nat_option bid with
    | Some first => canonical_bucket_checked first entries
    | None => None
    end.

Definition build_canonical_trie_checked_normalized (entries : list KeyValue)
  : option ARTrie :=
  match entries with
  | [] => Some empty_trie
  | _ :: _ =>
      if canonical_buckets_fit entries then
        Some (mkARTrie
          (Some 0)
          (fun nid => if Nat.eqb nid 0 then Some (canonical_root entries) else None)
          (canonical_buckets_checked entries)
          1
          256
          (length entries))
      else None
  end.

Definition build_canonical_trie_checked (entries : list KeyValue)
  : option ARTrie :=
  build_canonical_trie_checked_normalized (kv_normalize entries).

Lemma canonical_bucket_checked_lookup : forall entries first b suffix,
  canonical_bucket_checked first entries = Some b ->
  bucket_lookup b suffix = kv_lookup entries (first :: suffix).
Proof.
  intros entries first b suffix Hchecked.
  unfold canonical_bucket_checked in Hchecked.
  destruct ((length (bucket_entries_for first entries) <=? MAX_BUCKET_ENTRIES) &&
            (bucket_size_for_entries (bucket_entries_for first entries) <=?
             BUCKET_PAGE_SIZE)); [| discriminate].
  injection Hchecked as Hb. subst b.
  apply canonical_bucket_lookup.
Qed.

Lemma canonical_buckets_fit_from_complete : forall bytes entries first,
  canonical_buckets_fit_from bytes entries = true ->
  In first bytes ->
  exists b, canonical_bucket_checked first entries = Some b.
Proof.
  induction bytes as [| b bytes IH]; intros entries first Hfit Hin; simpl in *.
  - contradiction.
  - destruct (canonical_bucket_checked b entries) as [bucket |] eqn:Hbucket;
      [| discriminate].
    destruct Hin as [Hfirst | Hin].
    + subst first. exists bucket. exact Hbucket.
    + apply IH; assumption.
Qed.

Lemma canonical_buckets_fit_complete : forall entries first,
  canonical_buckets_fit entries = true ->
  exists b, canonical_bucket_checked first entries = Some b.
Proof.
  intros entries first Hfit.
  unfold canonical_buckets_fit in Hfit.
  eapply canonical_buckets_fit_from_complete.
  - exact Hfit.
  - apply enumerate_bytes_complete.
Qed.

Lemma checked_canonical_lookup_correct : forall entries t k,
  build_canonical_trie_checked entries = Some t ->
  trie_lookup t k = kv_lookup entries k.
Proof.
  intros entries t k Hbuild.
  unfold build_canonical_trie_checked in Hbuild.
  remember (kv_normalize entries) as normalized eqn:Hnormalized.
  assert (Hlookup_norm : forall key,
    kv_lookup normalized key = kv_lookup entries key).
  { intro key. subst normalized. apply kv_lookup_normalize. }
  unfold build_canonical_trie_checked_normalized in Hbuild.
  destruct normalized as [| entry entries'].
  - injection Hbuild as Ht. subst t.
    rewrite <- Hlookup_norm.
    destruct k; reflexivity.
  - destruct (canonical_buckets_fit (entry :: entries')) eqn:Hfit;
      [| discriminate].
    injection Hbuild as Ht. subst t.
    destruct k as [| first suffix].
    + unfold trie_lookup. cbn.
      unfold lookup_aux, traverse_node, match_prefix. cbn.
      apply Hlookup_norm.
    + unfold trie_lookup. cbn.
      unfold lookup_aux, traverse_node, match_prefix. cbn.
      destruct first as [n Hn].
      unfold canonical_buckets_checked, byte_of_nat_option, byte_val. cbn.
      destruct (lt_dec n 256) as [Hlt | Hnlt]; [| lia].
      replace (exist (fun n => n < 256) n Hlt)
        with (exist (fun n => n < 256) n Hn)
        by (f_equal; apply lt_proof_irrelevance).
      destruct (canonical_bucket_checked
        (exist (fun n => n < 256) n Hn)
        (entry :: entries')) as [bucket |] eqn:Hbucket.
      * destruct (length suffix + 100) as [| fuel'] eqn:Hfuel; [lia | cbn].
        change (bucket_lookup bucket suffix =
          kv_lookup entries
            (exist (fun n => n < 256) n Hn :: suffix)).
        rewrite <- Hlookup_norm.
        eapply canonical_bucket_checked_lookup. exact Hbucket.
      * exfalso.
        pose proof (canonical_buckets_fit_complete
          (entry :: entries')
          (exist (fun n => n < 256) n Hn)
          Hfit) as [bucket Hbucket'].
        rewrite Hbucket in Hbucket'. discriminate.
Qed.

Definition trie_insert_checked (t : ARTrie) (k : MapSpec.Key) (v : Value)
  : option ARTrie :=
  build_canonical_trie_checked (kv_upsert (entries_of_trie t) k v).

Definition trie_delete_checked (t : ARTrie) (k : MapSpec.Key)
  : option ARTrie :=
  build_canonical_trie_checked (kv_delete (entries_of_trie t) k).

Theorem trie_insert_checked_correct : forall t k v t' k',
  entries_of_trie_complete t ->
  trie_insert_checked t k v = Some t' ->
  interpret_trie t' k' =
    if MapSpec.key_eq_dec k k' then Some v else interpret_trie t k'.
Proof.
  intros t k v t' k' Hcomplete Hinsert.
  unfold trie_insert_checked in Hinsert.
  unfold interpret_trie.
  rewrite (checked_canonical_lookup_correct _ _ _ Hinsert).
  destruct (MapSpec.key_eq_dec k k') as [Heq | Hneq].
  - subst. apply kv_lookup_upsert_same.
  - rewrite kv_lookup_upsert_other; [apply Hcomplete | exact Hneq].
Qed.

Theorem trie_delete_checked_correct : forall t k t' k',
  entries_of_trie_complete t ->
  trie_delete_checked t k = Some t' ->
  interpret_trie t' k' =
    if MapSpec.key_eq_dec k k' then None else interpret_trie t k'.
Proof.
  intros t k t' k' Hcomplete Hdelete.
  unfold trie_delete_checked in Hdelete.
  unfold interpret_trie.
  rewrite (checked_canonical_lookup_correct _ _ _ Hdelete).
  destruct (MapSpec.key_eq_dec k k') as [Heq | Hneq].
  - subst. apply kv_lookup_delete_same.
  - rewrite kv_lookup_delete_other; [apply Hcomplete | exact Hneq].
Qed.

Definition trie_insert (t : ARTrie) (k : MapSpec.Key) (v : Value) : ARTrie :=
  build_canonical_trie (kv_upsert (entries_of_trie t) k v).

Definition trie_delete (t : ARTrie) (k : MapSpec.Key) : ARTrie :=
  build_canonical_trie (kv_delete (entries_of_trie t) k).

Theorem trie_insert_correct : forall t k v k',
  entries_of_trie_complete t ->
  interpret_trie (trie_insert t k v) k' =
    if MapSpec.key_eq_dec k k' then Some v else interpret_trie t k'.
Proof.
  intros t k v k' Hcomplete.
  unfold interpret_trie, trie_insert.
  rewrite canonical_lookup_correct.
  destruct (MapSpec.key_eq_dec k k') as [Heq | Hneq].
  - subst. apply kv_lookup_upsert_same.
  - rewrite kv_lookup_upsert_other; [apply Hcomplete | exact Hneq].
Qed.

Theorem trie_delete_correct : forall t k k',
  entries_of_trie_complete t ->
  interpret_trie (trie_delete t k) k' =
    if MapSpec.key_eq_dec k k' then None else interpret_trie t k'.
Proof.
  intros t k k' Hcomplete.
  unfold interpret_trie, trie_delete.
  rewrite canonical_lookup_correct.
  destruct (MapSpec.key_eq_dec k k') as [Heq | Hneq].
  - subst. apply kv_lookup_delete_same.
  - rewrite kv_lookup_delete_other; [apply Hcomplete | exact Hneq].
Qed.

Definition trie_insert_correct_obligation : Prop := forall t k v k',
  entries_of_trie_complete t ->
  interpret_trie (trie_insert t k v) k' =
    if MapSpec.key_eq_dec k k' then Some v else interpret_trie t k'.

Definition trie_delete_correct_obligation : Prop := forall t k k',
  entries_of_trie_complete t ->
  interpret_trie (trie_delete t k) k' =
    if MapSpec.key_eq_dec k k' then None else interpret_trie t k'.

Theorem ARTrieMapImpl_obligation :
  trie_insert_correct_obligation /\ trie_delete_correct_obligation.
Proof.
  split.
  - exact trie_insert_correct.
  - exact trie_delete_correct.
Qed.
