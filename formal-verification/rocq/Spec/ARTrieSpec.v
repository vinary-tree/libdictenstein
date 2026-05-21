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

Definition build_canonical_trie_checked (entries : list KeyValue)
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
  destruct entries as [| entry entries'].
  - injection Hbuild as Ht. subst t.
    destruct k; reflexivity.
  - destruct (canonical_buckets_fit (entry :: entries')) eqn:Hfit;
      [| discriminate].
    injection Hbuild as Ht. subst t.
    destruct k as [| first suffix].
    + unfold trie_lookup. cbn.
      unfold lookup_aux, traverse_node, match_prefix. cbn.
      reflexivity.
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
          kv_lookup (entry :: entries')
            (exist (fun n => n < 256) n Hn :: suffix)).
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
