(** * StructuralPreservation: Checked Operation Preservation Contracts

    The total canonical rebuild functions are semantic models. Structural
    preservation is stated for the checked constructors, because those are the
    constructors that can reject states violating fixed bucket capacity.
*)

Require Import Coq.Lists.List.
Require Import Coq.Arith.Arith.
Require Import Coq.Bool.Bool.
Require Import Coq.micromega.Lia.
Require Import ARTrie.Invariants.StructuralInvariants.
Require Import ARTrie.Spec.ARTrieSpec.
Require Import ARTrie.Model.Key.
Require Import ARTrie.Model.NodeTypes.
Require Import ARTrie.Model.Bucket.
Import ListNotations.

(** A checked insert preserves structure when the checked canonical builder
    succeeds. The source invariant is retained in the contract because it is
    required by the production operation model, even though this reduction only
    needs the builder success proof. *)
Definition checked_insert_preserves_structural_obligation : Prop :=
  forall t (k : Key) (v : Value) t',
    structural_invariant t ->
    trie_insert_checked t k v = Some t' ->
    structural_invariant t'.

Definition checked_delete_preserves_structural_obligation : Prop :=
  forall t (k : Key) t',
    structural_invariant t ->
    trie_delete_checked t k = Some t' ->
    structural_invariant t'.

Definition checked_canonical_builder_preserves_structural_obligation : Prop :=
  forall entries t,
    build_canonical_trie_checked entries = Some t ->
    structural_invariant t.

Theorem empty_trie_checked_builder_preserves_structural :
  forall t,
    build_canonical_trie_checked [] = Some t ->
    structural_invariant t.
Proof.
  intros t Hbuild.
  simpl in Hbuild.
  injection Hbuild as Ht. subst t.
  apply empty_structural_invariant.
Qed.

Lemma filter_true_length : forall {A : Type} (l : list A),
  length (filter (fun _ => true) l) = length l.
Proof.
  induction l as [| x xs IH]; simpl; [reflexivity|].
  rewrite IH. reflexivity.
Qed.

Lemma canonical_root_find_child : forall entries b,
  find_child (canonical_root entries) b = BucketPtr (byte_val b).
Proof.
  intros entries b. unfold canonical_root, find_child. simpl. reflexivity.
Qed.

Lemma canonical_root_wf : forall entries,
  wf_node (canonical_root entries).
Proof.
  intro entries.
  unfold canonical_root, wf_node, wf_node_data, get_node_type,
    node_capacity. simpl.
  split; [exact I|change (256 <= 256); lia].
Qed.

Lemma canonical_root_child_counts_valid : forall entries,
  child_count_accurate (canonical_root entries) /\
  child_count_bounded (canonical_root entries).
Proof.
  intro entries.
  split.
  - unfold child_count_accurate, canonical_root. simpl.
    change (256 = length (filter (fun _ : Byte => true) enumerate_bytes)).
    rewrite filter_true_length.
    symmetry. apply enumerate_bytes_length.
  - unfold child_count_bounded, canonical_root, get_node_type,
      node_capacity. simpl. change (256 <= 256). lia.
Qed.

Lemma canonical_root_prefix_valid : forall entries,
  prefix_len_accurate (canonical_root entries) /\
  prefix_bounded (canonical_root entries).
Proof.
  intro entries.
  unfold prefix_len_accurate, prefix_bounded, canonical_root. simpl.
  split; [reflexivity|lia].
Qed.

Lemma canonical_nodes_only_root : forall entries nid n,
  (if Nat.eqb nid 0 then Some (canonical_root entries) else None) = Some n ->
  nid = 0 /\ n = canonical_root entries.
Proof.
  intros entries nid n Hnode.
  destruct (Nat.eqb nid 0) eqn:Hnid; [| discriminate].
  apply Nat.eqb_eq in Hnid.
  injection Hnode as Hn. subst n.
  split; [exact Hnid|reflexivity].
Qed.

Lemma canonical_bucket_exists_for_byte : forall entries first,
  canonical_buckets_fit entries = true ->
  canonical_buckets_checked entries (byte_val first) <> None.
Proof.
  intros entries first Hfit.
  unfold canonical_buckets_checked.
  rewrite byte_of_nat_option_byte_val.
  pose proof (canonical_buckets_fit_complete entries first Hfit)
    as [bucket Hbucket].
  rewrite Hbucket. discriminate.
Qed.

Lemma canonical_reachable_only_root : forall entries,
  canonical_buckets_fit entries = true ->
  forall nid,
  reachable
    (mkARTrie
      (Some 0)
      (fun nid => if Nat.eqb nid 0 then Some (canonical_root entries) else None)
      (canonical_buckets_checked entries)
      1 256 (length entries))
    nid ->
  nid = 0.
Proof.
  intros entries Hfit nid Hreach.
  remember
    (mkARTrie
      (Some 0)
      (fun nid => if Nat.eqb nid 0 then Some (canonical_root entries) else None)
      (canonical_buckets_checked entries)
      1 256 (length entries)) as t eqn:Ht.
  induction Hreach.
  - subst t. simpl in H. injection H as Hrid. symmetry. exact Hrid.
  - subst t. simpl in *.
    match goal with
    | Hnode : (if Nat.eqb ?pid 0 then Some (canonical_root entries) else None) = Some ?node,
      Hchild : find_child ?node ?byte = NodePtr ?cid |- _ =>
        apply canonical_nodes_only_root in Hnode as [_ Hn];
        subst node;
        rewrite canonical_root_find_child in Hchild;
        discriminate
    end.
Qed.

Theorem nonempty_checked_canonical_builder_preserves_structural :
  forall entries t,
    entries <> [] ->
    canonical_entries_wf entries ->
    build_canonical_trie_checked_normalized entries = Some t ->
    structural_invariant t.
Proof.
  intros entries t Hnonempty Hentries_wf Hbuild.
  unfold build_canonical_trie_checked_normalized in Hbuild.
  destruct entries as [| entry entries']; [contradiction|].
  destruct (canonical_buckets_fit (entry :: entries')) eqn:Hfit;
    [| discriminate].
  injection Hbuild as Ht. subst t.
  set (entries := entry :: entries') in *.
  unfold structural_invariant.
  split.
  - unfold wf_trie.
    split; [| split; [| split]].
    + unfold all_nodes_wf. intros nid n Hnode.
      apply canonical_nodes_only_root in Hnode as [_ Hn]. subst n.
      apply canonical_root_wf.
    + unfold all_buckets_wf. intros bid b Hbucket.
      simpl in Hbucket.
      unfold canonical_buckets_checked in Hbucket.
      destruct (byte_of_nat_option bid) as [first |] eqn:Hbyte;
        [| discriminate].
      eapply canonical_bucket_checked_wf.
      * destruct Hentries_wf as [Huniq _].
        apply bucket_entries_for_unique. exact Huniq.
      * exact Hbucket.
    + unfold no_dangling_ptrs. intros nid n b Hnode Hnonnull.
      apply canonical_nodes_only_root in Hnode as [_ Hn]. subst n.
      rewrite canonical_root_find_child.
      apply canonical_bucket_exists_for_byte. exact Hfit.
    + unfold root_exists. split; intro H.
      * exists 0. split; [reflexivity|].
        simpl. discriminate.
      * simpl. lia.
  - split.
    + unfold no_cycles. intros nid n b Hreach Hnode Hloop.
      pose proof (canonical_reachable_only_root entries Hfit nid Hreach)
        as Hreach_root.
      subst nid.
      apply canonical_nodes_only_root in Hnode as [_ Hn]. subst n.
      rewrite canonical_root_find_child in Hloop. discriminate.
    + split.
      * unfold reachable_nodes_exist. intros nid Hreach.
        pose proof (canonical_reachable_only_root entries Hfit nid Hreach)
          as Hreach_root.
        subst nid. simpl. discriminate.
      * split.
        -- unfold reachable_buckets_exist.
           intros nid n b bid Hreach Hnode Hchild.
           pose proof (canonical_reachable_only_root entries Hfit nid Hreach)
             as Hreach_root.
           subst nid.
           apply canonical_nodes_only_root in Hnode as [_ Hn]. subst n.
           rewrite canonical_root_find_child in Hchild.
           injection Hchild as Hbid. subst bid.
           apply canonical_bucket_exists_for_byte. exact Hfit.
        -- split.
           ++ unfold all_child_counts_valid. intros nid n Hnode.
              apply canonical_nodes_only_root in Hnode as [_ Hn]. subst n.
              apply canonical_root_child_counts_valid.
           ++ split.
              ** unfold all_prefixes_valid. intros nid n Hnode.
                 apply canonical_nodes_only_root in Hnode as [_ Hn]. subst n.
                 apply canonical_root_prefix_valid.
              ** split.
                 --- unfold all_buckets_sorted. intros bid b Hbucket.
                     simpl in Hbucket.
                     unfold canonical_buckets_checked in Hbucket.
                     destruct (byte_of_nat_option bid) as [first |] eqn:Hbyte;
                       [| discriminate].
                     pose proof (canonical_bucket_checked_wf_sorted
                       entries first b Hentries_wf Hbucket) as [_ Hsorted].
                     exact Hsorted.
                 --- unfold all_buckets_count_valid. intros bid b Hbucket.
                     simpl in Hbucket.
                     unfold canonical_buckets_checked in Hbucket.
                     destruct (byte_of_nat_option bid) as [first |] eqn:Hbyte;
                       [| discriminate].
                     pose proof (canonical_bucket_checked_wf_sorted
                       entries first b Hentries_wf Hbucket) as [[_ [Hcount _]] _].
                     exact Hcount.
Qed.

Theorem checked_canonical_builder_preserves_structural :
  checked_canonical_builder_preserves_structural_obligation.
Proof.
  unfold checked_canonical_builder_preserves_structural_obligation.
  intros entries t Hbuild.
  unfold build_canonical_trie_checked in Hbuild.
  destruct (kv_normalize entries) as [| entry rest] eqn:Hnorm.
  - unfold build_canonical_trie_checked_normalized in Hbuild.
    injection Hbuild as Ht. subst t.
    apply empty_structural_invariant.
  - eapply (nonempty_checked_canonical_builder_preserves_structural
      (entry :: rest)).
    + intro Hnil. discriminate Hnil.
    + rewrite <- Hnorm. apply kv_normalize_canonical_entries_wf.
    + exact Hbuild.
Qed.

Theorem checked_insert_preserves_structural :
  checked_insert_preserves_structural_obligation.
Proof.
  unfold checked_insert_preserves_structural_obligation.
  intros t k v t' _ Hinsert.
  eapply checked_canonical_builder_preserves_structural. exact Hinsert.
Qed.

Theorem checked_delete_preserves_structural :
  checked_delete_preserves_structural_obligation.
Proof.
  unfold checked_delete_preserves_structural_obligation.
  intros t k t' _ Hdelete.
  eapply checked_canonical_builder_preserves_structural. exact Hdelete.
Qed.

Theorem insert_preserves_structural :
  insert_preserves_structural_obligation.
Proof.
  exact checked_insert_preserves_structural.
Qed.

Theorem delete_preserves_structural :
  delete_preserves_structural_obligation.
Proof.
  exact checked_delete_preserves_structural.
Qed.
