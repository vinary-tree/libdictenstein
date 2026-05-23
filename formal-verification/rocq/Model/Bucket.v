(** * Bucket: B-trie Bucket Model

    This module defines the bucket structure used at the leaves of
    the ARTrie. Buckets are B-trie nodes that store multiple key
    suffixes with their associated values.

    Key features:
    - Fixed 8KB page size
    - Up to 256 entries per bucket
    - Sorted entries for binary search
    - Suffix storage (trie path determines common prefix)
*)

Require Import Coq.Lists.List.
Require Import Coq.Arith.Arith.
Require Import Coq.Bool.Bool.
Require Import Coq.Sorting.Sorted.
Require Import Coq.micromega.Lia.
Require Import ARTrie.Model.Key.
Import ListNotations.

(** ** Bucket Constants *)

Definition BUCKET_PAGE_SIZE : nat := 8192.
Definition MAX_BUCKET_ENTRIES : nat := 256.
Definition BUCKET_HEADER_SIZE : nat := 32.
Definition BUCKET_ENTRY_SIZE : nat := 8.

(** ** Value Type *)

(** Values are byte sequences *)
Definition Value := list Byte.

(** ** Bucket Entry *)

(** A bucket entry stores a suffix and optional value *)
Record BucketEntry := mkEntry {
  entry_suffix : Key;
  entry_value : option Value
}.

(** Entry comparison based on suffix (for sorting) *)
Definition entry_compare (e1 e2 : BucketEntry) : comparison :=
  key_compare (entry_suffix e1) (entry_suffix e2).

Definition entry_lt (e1 e2 : BucketEntry) : Prop :=
  key_compare (entry_suffix e1) (entry_suffix e2) = Lt.

Definition entry_le (e1 e2 : BucketEntry) : Prop :=
  key_compare (entry_suffix e1) (entry_suffix e2) <> Gt.

(** ** Bucket Structure *)

Record Bucket := mkBucket {
  bucket_entries : list BucketEntry;
  bucket_data_size : nat;    (* Total data used *)
  bucket_free_space : nat    (* Remaining space *)
}.

(** ** Bucket Invariants *)

(** Entries are sorted by suffix *)
Definition bucket_sorted (b : Bucket) : Prop :=
  Sorted entry_le (bucket_entries b).

(** Suffixes are unique. This is the property needed for lookup/delete laws. *)
Definition bucket_keys_unique (b : Bucket) : Prop :=
  NoDup (map entry_suffix (bucket_entries b)).

(** Entry count is within bounds *)
Definition bucket_count_valid (b : Bucket) : Prop :=
  length (bucket_entries b) <= MAX_BUCKET_ENTRIES.

(** Calculate space needed for an entry *)
Definition entry_space (suffix : Key) (value : option Value) : nat :=
  BUCKET_ENTRY_SIZE + length suffix +
  match value with
  | None => 0
  | Some v => length v
  end.

Fixpoint entries_space (entries : list BucketEntry) : nat :=
  match entries with
  | [] => 0
  | e :: rest => entry_space (entry_suffix e) (entry_value e) + entries_space rest
  end.

Definition bucket_data_size_valid (b : Bucket) : Prop :=
  bucket_data_size b = BUCKET_HEADER_SIZE + entries_space (bucket_entries b).

(** Space accounting is correct *)
Definition bucket_space_valid (b : Bucket) : Prop :=
  bucket_data_size b <= BUCKET_PAGE_SIZE /\
  bucket_free_space b = BUCKET_PAGE_SIZE - bucket_data_size b.

(** Bucket is well-formed *)
Definition wf_bucket (b : Bucket) : Prop :=
  bucket_keys_unique b /\
  bucket_count_valid b /\
  bucket_space_valid b /\
  bucket_data_size_valid b.

(** Sorted well-formed buckets are the representation contract used by the
    production binary-search path. The legacy [wf_bucket] predicate is kept
    as the space/accounting contract so existing refinement proofs remain
    stable while sortedness obligations are made explicit. *)
Definition wf_sorted_bucket (b : Bucket) : Prop :=
  wf_bucket b /\ bucket_sorted b.

(** ** Empty Bucket *)

Definition empty_bucket : Bucket :=
  mkBucket [] BUCKET_HEADER_SIZE (BUCKET_PAGE_SIZE - BUCKET_HEADER_SIZE).

Lemma empty_bucket_wf : wf_bucket empty_bucket.
Proof.
  unfold wf_bucket, empty_bucket. simpl.
  split; [| split].
  - constructor.
  - unfold bucket_count_valid, MAX_BUCKET_ENTRIES. simpl. auto with arith.
  - split.
    + unfold bucket_space_valid, BUCKET_PAGE_SIZE, BUCKET_HEADER_SIZE. simpl.
      split; [apply Nat.leb_le; vm_compute; reflexivity|reflexivity].
    + unfold bucket_data_size_valid, BUCKET_HEADER_SIZE. simpl. reflexivity.
Qed.

Lemma empty_bucket_sorted : bucket_sorted empty_bucket.
Proof.
  unfold bucket_sorted, empty_bucket. simpl. constructor.
Qed.

Lemma empty_bucket_wf_sorted : wf_sorted_bucket empty_bucket.
Proof.
  split.
  - apply empty_bucket_wf.
  - apply empty_bucket_sorted.
Qed.

Lemma wf_sorted_bucket_wf : forall b,
  wf_sorted_bucket b -> wf_bucket b.
Proof.
  intros b [Hwf _]. exact Hwf.
Qed.

(** ** Binary Search *)

(** Find position where entry should be inserted (or found) *)
Fixpoint binary_search_aux (entries : list BucketEntry) (suffix : Key)
  (lo hi : nat) (fuel : nat) : nat :=
  match fuel with
  | 0 => lo
  | S fuel' =>
      if lo <? hi then
        let mid := (lo + hi) / 2 in
        match nth_error entries mid with
        | None => lo
        | Some e =>
            match key_compare suffix (entry_suffix e) with
            | Lt => binary_search_aux entries suffix lo mid fuel'
            | Eq => mid
            | Gt => binary_search_aux entries suffix (S mid) hi fuel'
            end
        end
      else lo
  end.

Fixpoint binary_search_lower_bound (entries : list BucketEntry) (suffix : Key)
  : nat :=
  match entries with
  | [] => 0
  | e :: rest =>
      match key_compare (entry_suffix e) suffix with
      | Lt => S (binary_search_lower_bound rest suffix)
      | Eq => 0
      | Gt => 0
      end
  end.

Definition binary_search (entries : list BucketEntry) (suffix : Key) : nat :=
  binary_search_lower_bound entries suffix.

(** ** Entry Operations *)

Fixpoint entries_lookup (entries : list BucketEntry) (suffix : Key) : option Value :=
  match entries with
  | [] => None
  | e :: rest =>
      if key_eqb suffix (entry_suffix e)
      then entry_value e
      else entries_lookup rest suffix
  end.

Fixpoint entries_upsert (entries : list BucketEntry) (suffix : Key)
  (value : option Value) : list BucketEntry :=
  match entries with
  | [] => [mkEntry suffix value]
  | e :: rest =>
      if key_eqb suffix (entry_suffix e)
      then mkEntry suffix value :: rest
      else e :: entries_upsert rest suffix value
  end.

Fixpoint entries_delete (entries : list BucketEntry) (suffix : Key)
  : list BucketEntry :=
  match entries with
  | [] => []
  | e :: rest =>
      if key_eqb suffix (entry_suffix e)
      then rest
      else e :: entries_delete rest suffix
  end.

Definition bucket_size_for_entries (entries : list BucketEntry) : nat :=
  BUCKET_HEADER_SIZE + entries_space entries.

Definition bucket_from_entries (entries : list BucketEntry) : option Bucket :=
  let size := bucket_size_for_entries entries in
  if (length entries <=? MAX_BUCKET_ENTRIES) && (size <=? BUCKET_PAGE_SIZE) then
    Some (mkBucket entries size (BUCKET_PAGE_SIZE - size))
  else None.

(** ** Lookup Operation *)

(** Find an entry by suffix *)
Definition bucket_lookup (b : Bucket) (suffix : Key) : option Value :=
  entries_lookup (bucket_entries b) suffix.

(** ** Insert Operation *)

(** Insert an entry at a given position *)
Definition insert_at {A} (l : list A) (pos : nat) (x : A) : list A :=
  firstn pos l ++ [x] ++ skipn pos l.

(** Check if bucket has space for entry *)
Definition bucket_has_space (b : Bucket) (suffix : Key) (value : option Value)
  : bool :=
  (length (bucket_entries b) <? MAX_BUCKET_ENTRIES) &&
  (entry_space suffix value <=? bucket_free_space b).

(** Insert an entry into the bucket *)
Definition bucket_insert (b : Bucket) (suffix : Key) (value : option Value)
  : option Bucket :=
  bucket_from_entries (entries_upsert (bucket_entries b) suffix value).

(** ** Delete Operation *)

(** Delete an entry by suffix *)
Definition bucket_delete (b : Bucket) (suffix : Key) : Bucket :=
  let entries := entries_delete (bucket_entries b) suffix in
  let size := bucket_size_for_entries entries in
  mkBucket entries size (BUCKET_PAGE_SIZE - size).

(** ** Bucket Split *)

(** Determine split point (median) *)
Definition split_point (b : Bucket) : nat :=
  length (bucket_entries b) / 2.

(** Split bucket into two *)
Definition bucket_split (b : Bucket) : Bucket * Bucket :=
  let mid := split_point b in
  let left_entries := firstn mid (bucket_entries b) in
  let right_entries := skipn mid (bucket_entries b) in
  let left_size := entries_space left_entries in
  let right_size := entries_space right_entries in
  (mkBucket left_entries
            (BUCKET_HEADER_SIZE + left_size)
            (BUCKET_PAGE_SIZE - (BUCKET_HEADER_SIZE + left_size)),
   mkBucket right_entries
            (BUCKET_HEADER_SIZE + right_size)
            (BUCKET_PAGE_SIZE - (BUCKET_HEADER_SIZE + right_size))).

(** ** Split Key *)

(** Get the first key of the right bucket (split key) *)
Definition bucket_split_key (b : Bucket) : option Key :=
  let mid := split_point b in
  match nth_error (bucket_entries b) mid with
  | None => None
  | Some e => Some (entry_suffix e)
  end.

(** ** Correctness Lemmas *)

(** Entry-list helper lemmas *)

Lemma entries_lookup_upsert_same : forall entries suffix value,
  entries_lookup (entries_upsert entries suffix value) suffix = value.
Proof.
  induction entries as [| e rest IH]; intros suffix value; simpl.
  - rewrite key_eqb_refl. reflexivity.
  - destruct (key_eqb suffix (entry_suffix e)) eqn:Heq; simpl.
    + rewrite key_eqb_refl. reflexivity.
    + rewrite Heq. apply IH.
Qed.

Lemma entries_lookup_upsert_other : forall entries suffix1 suffix2 value,
  suffix1 <> suffix2 ->
  entries_lookup (entries_upsert entries suffix1 value) suffix2 =
  entries_lookup entries suffix2.
Proof.
  induction entries as [| e rest IH]; intros suffix1 suffix2 value Hneq; simpl.
  - destruct (key_eqb suffix2 suffix1) eqn:Heq.
    + apply key_eqb_eq in Heq. subst. exfalso. apply Hneq. reflexivity.
    + reflexivity.
  - destruct (key_eqb suffix1 (entry_suffix e)) eqn:Hhit; simpl.
    + apply key_eqb_eq in Hhit. subst.
      destruct (key_eqb suffix2 (entry_suffix e)) eqn:Hother.
      * apply key_eqb_eq in Hother. subst. exfalso. apply Hneq. reflexivity.
      * reflexivity.
    + destruct (key_eqb suffix2 (entry_suffix e)); [reflexivity|].
      apply IH. exact Hneq.
Qed.

Lemma entries_lookup_not_in : forall entries suffix,
  ~ In suffix (map entry_suffix entries) ->
  entries_lookup entries suffix = None.
Proof.
  induction entries as [| e rest IH]; intros suffix Hnotin; simpl.
  - reflexivity.
  - destruct (key_eqb suffix (entry_suffix e)) eqn:Heq.
    + apply key_eqb_eq in Heq. subst.
      exfalso. apply Hnotin. left. reflexivity.
    + apply IH. intro Hin. apply Hnotin. right. exact Hin.
Qed.

Lemma entries_lookup_delete_same : forall entries suffix,
  NoDup (map entry_suffix entries) ->
  entries_lookup (entries_delete entries suffix) suffix = None.
Proof.
  induction entries as [| e rest IH]; intros suffix Huniq; simpl.
  - reflexivity.
  - inversion Huniq as [| x xs Hnotin Htail]; subst.
    destruct (key_eqb suffix (entry_suffix e)) eqn:Heq.
    + apply key_eqb_eq in Heq. subst.
      apply entries_lookup_not_in. exact Hnotin.
    + simpl. rewrite Heq. apply IH. exact Htail.
Qed.

Lemma entries_lookup_delete_other : forall entries suffix1 suffix2,
  suffix1 <> suffix2 ->
  entries_lookup (entries_delete entries suffix1) suffix2 =
  entries_lookup entries suffix2.
Proof.
  induction entries as [| e rest IH]; intros suffix1 suffix2 Hneq; simpl.
  - reflexivity.
  - destruct (key_eqb suffix1 (entry_suffix e)) eqn:Hhit.
    + apply key_eqb_eq in Hhit. subst. simpl.
      destruct (key_eqb suffix2 (entry_suffix e)) eqn:Hother.
      * apply key_eqb_eq in Hother. subst. exfalso. apply Hneq. reflexivity.
      * reflexivity.
    + simpl. destruct (key_eqb suffix2 (entry_suffix e)); [reflexivity|].
      apply IH. exact Hneq.
Qed.

Lemma entries_delete_keys_subset : forall entries suffix k,
  In k (map entry_suffix (entries_delete entries suffix)) ->
  In k (map entry_suffix entries).
Proof.
  induction entries as [| e rest IH]; intros suffix k Hin; simpl in *.
  - contradiction.
  - destruct (key_eqb suffix (entry_suffix e)) eqn:Hhit.
    + right. exact Hin.
    + simpl in Hin. destruct Hin as [Hin | Hin].
      * left. exact Hin.
      * right. apply IH with (suffix := suffix). exact Hin.
Qed.

Lemma entries_delete_unique : forall entries suffix,
  NoDup (map entry_suffix entries) ->
  NoDup (map entry_suffix (entries_delete entries suffix)).
Proof.
  induction entries as [| e rest IH]; intros suffix Huniq; simpl.
  - constructor.
  - inversion Huniq as [| x xs Hnotin Htail]; subst.
    destruct (key_eqb suffix (entry_suffix e)) eqn:Hhit.
    + exact Htail.
    + simpl. constructor.
      * intro Hin. apply Hnotin.
        apply entries_delete_keys_subset with (suffix := suffix). exact Hin.
      * apply IH. exact Htail.
Qed.

Lemma entries_upsert_keys_cases : forall entries suffix value k,
  In k (map entry_suffix (entries_upsert entries suffix value)) ->
  k = suffix \/ In k (map entry_suffix entries).
Proof.
  induction entries as [| e rest IH]; intros suffix value k Hin; simpl in *.
  - destruct Hin as [Hin | []]. left. symmetry. exact Hin.
  - destruct (key_eqb suffix (entry_suffix e)) eqn:Hhit; simpl in Hin.
    + destruct Hin as [Hin | Hin].
      * left. symmetry. exact Hin.
      * right. right. exact Hin.
    + destruct Hin as [Hin | Hin].
      * right. left. exact Hin.
      * apply IH in Hin as [Hin | Hin].
        -- left. exact Hin.
        -- right. right. exact Hin.
Qed.

Lemma entries_upsert_unique : forall entries suffix value,
  NoDup (map entry_suffix entries) ->
  NoDup (map entry_suffix (entries_upsert entries suffix value)).
Proof.
  induction entries as [| e rest IH]; intros suffix value Huniq; simpl.
  - constructor; [intro H; inversion H|constructor].
  - inversion Huniq as [| x xs Hnotin Htail]; subst.
    destruct (key_eqb suffix (entry_suffix e)) eqn:Hhit; simpl.
    + apply key_eqb_eq in Hhit. subst. constructor; assumption.
    + constructor.
      * intro Hin.
        apply entries_upsert_keys_cases in Hin as [Hin | Hin].
        -- subst. rewrite key_eqb_refl in Hhit. discriminate.
        -- apply Hnotin. exact Hin.
      * apply IH. exact Htail.
Qed.

Lemma entries_delete_length_le : forall entries suffix,
  length (entries_delete entries suffix) <= length entries.
Proof.
  induction entries as [| e rest IH]; intros suffix; simpl.
  - lia.
  - specialize (IH suffix).
    destruct (key_eqb suffix (entry_suffix e)); simpl; lia.
Qed.

Lemma entries_delete_space_le : forall entries suffix,
  entries_space (entries_delete entries suffix) <= entries_space entries.
Proof.
  induction entries as [| e rest IH]; intros suffix; simpl.
  - lia.
  - specialize (IH suffix).
    destruct (key_eqb suffix (entry_suffix e)); simpl; lia.
Qed.

Lemma entries_space_app : forall l1 l2,
  entries_space (l1 ++ l2) = entries_space l1 + entries_space l2.
Proof.
  induction l1 as [| e rest IH]; intros l2; simpl; [reflexivity|].
  rewrite IH. lia.
Qed.

Lemma entries_space_firstn_le : forall entries n,
  entries_space (firstn n entries) <= entries_space entries.
Proof.
  intros entries n.
  pose proof (entries_space_app (firstn n entries) (skipn n entries)) as Happ.
  rewrite firstn_skipn in Happ. lia.
Qed.

Lemma entries_space_skipn_le : forall entries n,
  entries_space (skipn n entries) <= entries_space entries.
Proof.
  intros entries n.
  pose proof (entries_space_app (firstn n entries) (skipn n entries)) as Happ.
  rewrite firstn_skipn in Happ. lia.
Qed.

Lemma in_firstn_in : forall {A : Type} n (l : list A) x,
  In x (firstn n l) -> In x l.
Proof.
  induction n as [| n IH]; intros [| y ys] x Hin; simpl in *; try contradiction.
  destruct Hin as [Hin | Hin].
  - left. exact Hin.
  - right. apply IH. exact Hin.
Qed.

Lemma map_firstn : forall {A B : Type} (f : A -> B) n l,
  map f (firstn n l) = firstn n (map f l).
Proof.
  induction n as [| n IH]; intros [| x xs]; simpl; try reflexivity.
  rewrite IH. reflexivity.
Qed.

Lemma map_skipn : forall {A B : Type} (f : A -> B) n l,
  map f (skipn n l) = skipn n (map f l).
Proof.
  induction n as [| n IH]; intros [| x xs]; simpl; try reflexivity.
  apply IH.
Qed.

Lemma NoDup_firstn : forall {A : Type} n (l : list A),
  NoDup l -> NoDup (firstn n l).
Proof.
  induction n as [| n IH]; intros l Hnodup; destruct l as [| x xs]; simpl.
  - constructor.
  - constructor.
  - constructor.
  - inversion Hnodup as [| y ys Hnotin Htail]; subst.
    constructor.
    + intro Hin. apply Hnotin.
      apply in_firstn_in with (n := n). exact Hin.
    + apply IH. exact Htail.
Qed.

Lemma NoDup_skipn : forall {A : Type} n (l : list A),
  NoDup l -> NoDup (skipn n l).
Proof.
  induction n as [| n IH]; intros [| x xs] Hnodup; simpl; try assumption; try constructor.
  inversion Hnodup as [| y ys Hnotin Htail]; subst.
  apply IH. exact Htail.
Qed.

Lemma bucket_from_entries_wf : forall entries,
  NoDup (map entry_suffix entries) ->
  match bucket_from_entries entries with
  | Some b => wf_bucket b
  | None => True
  end.
Proof.
  intros entries Huniq.
  unfold bucket_from_entries, bucket_size_for_entries.
  set (size := BUCKET_HEADER_SIZE + entries_space entries).
  destruct ((length entries <=? MAX_BUCKET_ENTRIES) &&
            (size <=? BUCKET_PAGE_SIZE)) eqn:Hcheck; [|trivial].
  apply andb_true_iff in Hcheck.
  destruct Hcheck as [Hcount Hsize].
  apply Nat.leb_le in Hcount.
  apply Nat.leb_le in Hsize.
  change (wf_bucket (mkBucket entries size (BUCKET_PAGE_SIZE - size))).
  unfold wf_bucket. split.
  - unfold bucket_keys_unique. cbn. exact Huniq.
  - split.
    + unfold bucket_count_valid. cbn. exact Hcount.
    + split.
      * change (size <= BUCKET_PAGE_SIZE /\
                BUCKET_PAGE_SIZE - size = BUCKET_PAGE_SIZE - size).
        split; [exact Hsize|reflexivity].
      * change (size = BUCKET_HEADER_SIZE + entries_space entries).
        unfold size. reflexivity.
Qed.

Lemma binary_search_mid_bounds : forall lo hi,
  lo < hi ->
  lo <= (lo + hi) / 2 /\ (lo + hi) / 2 < hi.
Proof.
  intros lo hi Hlt.
  split.
  - apply Nat.div_le_lower_bound; lia.
  - apply Nat.div_lt_upper_bound; lia.
Qed.

Lemma binary_search_aux_upper_bound : forall fuel entries suffix lo hi,
  lo <= hi ->
  binary_search_aux entries suffix lo hi fuel <= hi.
Proof.
  induction fuel as [| fuel IH]; intros entries suffix lo hi Hle; simpl.
  - exact Hle.
  - destruct (lo <? hi) eqn:Hltb.
    + apply Nat.ltb_lt in Hltb.
      set (mid := (lo + hi) / 2).
      assert (Hlo_mid : lo <= mid /\ mid < hi).
      { unfold mid. apply binary_search_mid_bounds. exact Hltb. }
      destruct Hlo_mid as [Hlo_mid Hmid_hi].
      replace (fst (Nat.divmod (lo + hi) 1 0 1)) with mid
        by (unfold mid; reflexivity).
      destruct (nth_error entries mid) as [e|].
      * destruct (key_compare suffix (entry_suffix e)) eqn:Hcmp.
        -- lia.
        -- eapply Nat.le_trans.
           ++ apply IH. exact Hlo_mid.
           ++ lia.
        -- apply IH. lia.
      * exact Hle.
    + exact Hle.
Qed.

Lemma binary_search_in_bounds : forall entries suffix,
  binary_search entries suffix <= length entries.
Proof.
  induction entries as [| e rest IH]; intros suffix; simpl.
  - lia.
  - specialize (IH suffix).
    destruct (key_compare (entry_suffix e) suffix); simpl; lia.
Qed.

Definition entries_before_suffix (entries : list BucketEntry) (suffix : Key)
  : Prop :=
  Forall (fun e => key_compare (entry_suffix e) suffix = Lt) entries.

Definition entries_at_or_after_suffix (entries : list BucketEntry) (suffix : Key)
  : Prop :=
  Forall (fun e => key_compare suffix (entry_suffix e) <> Gt) entries.

Definition binary_search_partition_valid
  (entries : list BucketEntry) (suffix : Key) (pos : nat) : Prop :=
  pos <= length entries /\
  entries_before_suffix (firstn pos entries) suffix /\
  entries_at_or_after_suffix (skipn pos entries) suffix.

Lemma entry_le_trans : forall e1 e2 e3,
  entry_le e1 e2 ->
  entry_le e2 e3 ->
  entry_le e1 e3.
Proof.
  intros e1 e2 e3 H12 H23.
  unfold entry_le in *.
  eapply key_compare_le_trans; eauto.
Qed.

Lemma Forall_entry_le_trans : forall e1 e2 rest,
  entry_le e1 e2 ->
  Forall (entry_le e2) rest ->
  Forall (entry_le e1) rest.
Proof.
  intros e1 e2 rest He12 Hall.
  induction Hall as [| e3 rest He23 _ IH].
  - constructor.
  - constructor.
    + eapply entry_le_trans; eauto.
    + exact IH.
Qed.

Lemma sorted_entry_le_forall_tail : forall e rest,
  Sorted entry_le (e :: rest) ->
  Forall (entry_le e) rest.
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
      eapply Forall_entry_le_trans; eauto.
Qed.

Lemma Forall_suffix_entry_le_trans : forall suffix e rest,
  key_compare suffix (entry_suffix e) <> Gt ->
  Forall (entry_le e) rest ->
  entries_at_or_after_suffix rest suffix.
Proof.
  intros suffix e rest Hsuffix_e Hall.
  unfold entries_at_or_after_suffix.
  induction Hall as [| x xs Hex _ IH].
  - constructor.
  - constructor.
    + eapply key_compare_le_trans.
      * exact Hsuffix_e.
      * exact Hex.
    + exact IH.
Qed.

Lemma entries_at_or_after_from_sorted_head : forall e rest suffix,
  Sorted entry_le (e :: rest) ->
  key_compare suffix (entry_suffix e) <> Gt ->
  entries_at_or_after_suffix (e :: rest) suffix.
Proof.
  intros e rest suffix Hsorted Hsuffix_e.
  unfold entries_at_or_after_suffix.
  constructor; [exact Hsuffix_e|].
  pose proof (sorted_entry_le_forall_tail e rest Hsorted) as Hall.
  apply Forall_suffix_entry_le_trans with (e := e); assumption.
Qed.

Lemma binary_search_lower_bound_partition : forall entries suffix,
  Sorted entry_le entries ->
  binary_search_partition_valid entries suffix
    (binary_search_lower_bound entries suffix).
Proof.
  induction entries as [| e rest IH]; intros suffix Hsorted; simpl.
  - unfold binary_search_partition_valid, entries_before_suffix,
      entries_at_or_after_suffix. simpl.
    split; [lia|split; constructor].
  - inversion Hsorted as [| ? ? Htail Hhd]; subst.
    destruct (key_compare (entry_suffix e) suffix) eqn:Hcmp.
    + unfold binary_search_partition_valid, entries_before_suffix,
        entries_at_or_after_suffix. simpl.
      split; [lia|split; [constructor|]].
      apply entries_at_or_after_from_sorted_head; [exact Hsorted|].
      apply key_compare_eq in Hcmp. subst. rewrite key_compare_refl.
      discriminate.
    + pose proof (IH suffix Htail) as [Hbound [Hbefore Hafter]].
      unfold binary_search_partition_valid, entries_before_suffix,
        entries_at_or_after_suffix in *.
      simpl.
      split; [lia|].
      split.
      * constructor; assumption.
      * exact Hafter.
    + unfold binary_search_partition_valid, entries_before_suffix,
        entries_at_or_after_suffix. simpl.
      split; [lia|split; [constructor|]].
      apply entries_at_or_after_from_sorted_head; [exact Hsorted|].
      apply key_compare_gt_flip_le. exact Hcmp.
Qed.

Theorem binary_search_partition : forall entries suffix,
  Sorted entry_le entries ->
  binary_search_partition_valid entries suffix
    (binary_search entries suffix).
Proof.
  intros entries suffix Hsorted.
  unfold binary_search.
  apply binary_search_lower_bound_partition.
  exact Hsorted.
Qed.

Definition binary_search_position_valid
  (entries : list BucketEntry) (suffix : Key) (pos : nat) : Prop :=
  pos = binary_search entries suffix /\ pos <= length entries.

Lemma binary_search_correct : forall entries suffix,
  exists pos, binary_search_position_valid entries suffix pos.
Proof.
  intros entries suffix.
  exists (binary_search entries suffix).
  split; [reflexivity|apply binary_search_in_bounds].
Qed.

Lemma binary_search_empty : forall suffix,
  binary_search [] suffix = 0.
Proof.
  intros suffix. reflexivity.
Qed.

Lemma binary_search_singleton_eq : forall suffix value,
  binary_search [mkEntry suffix value] suffix = 0.
Proof.
  intros suffix value.
  unfold binary_search. simpl.
  rewrite key_compare_refl. reflexivity.
Qed.

(** Lookup after successful insert of same key returns the inserted value *)
Lemma bucket_lookup_insert_same : forall b suffix value,
  wf_bucket b ->
  match bucket_insert b suffix value with
  | Some b' => bucket_lookup b' suffix = value
  | None => True
  end.
Proof.
  intros b suffix value _.
  unfold bucket_insert, bucket_lookup.
  unfold bucket_from_entries, bucket_size_for_entries.
  set (entries := entries_upsert (bucket_entries b) suffix value).
  set (size := BUCKET_HEADER_SIZE + entries_space entries).
  destruct ((length entries <=? MAX_BUCKET_ENTRIES) &&
            (size <=? BUCKET_PAGE_SIZE)); [|trivial].
  change (entries_lookup entries suffix = value).
  unfold entries. apply entries_lookup_upsert_same.
Qed.

(** Lookup after insert of different key returns original value *)
Lemma bucket_lookup_insert_other : forall b suffix1 suffix2 value,
  wf_bucket b ->
  suffix1 <> suffix2 ->
  match bucket_insert b suffix1 value with
  | Some b' => bucket_lookup b' suffix2 = bucket_lookup b suffix2
  | None => True
  end.
Proof.
  intros b suffix1 suffix2 value _ Hneq.
  unfold bucket_insert, bucket_lookup.
  unfold bucket_from_entries, bucket_size_for_entries.
  set (entries := entries_upsert (bucket_entries b) suffix1 value).
  set (size := BUCKET_HEADER_SIZE + entries_space entries).
  destruct ((length entries <=? MAX_BUCKET_ENTRIES) &&
            (size <=? BUCKET_PAGE_SIZE)); [|trivial].
  change (entries_lookup entries suffix2 = entries_lookup (bucket_entries b) suffix2).
  unfold entries. apply entries_lookup_upsert_other. exact Hneq.
Qed.

(** Lookup after delete of same key returns None *)
Lemma bucket_lookup_delete_same : forall b suffix,
  wf_bucket b ->
  bucket_lookup (bucket_delete b suffix) suffix = None.
Proof.
  intros b suffix Hwf.
  unfold wf_bucket in Hwf.
  destruct Hwf as [Huniq _].
  unfold bucket_lookup, bucket_delete.
  set (entries := entries_delete (bucket_entries b) suffix).
  set (size := bucket_size_for_entries entries).
  change (entries_lookup entries suffix = None).
  unfold entries. apply entries_lookup_delete_same. exact Huniq.
Qed.

(** Insert preserves well-formedness *)
Lemma bucket_insert_wf : forall b suffix value,
  wf_bucket b ->
  match bucket_insert b suffix value with
  | Some b' => wf_bucket b'
  | None => True
  end.
Proof.
  intros b suffix value Hwf.
  unfold bucket_insert.
  apply bucket_from_entries_wf.
  unfold wf_bucket in Hwf. destruct Hwf as [Huniq _].
  apply entries_upsert_unique. exact Huniq.
Qed.

(** Delete preserves well-formedness *)
Lemma bucket_delete_wf : forall b suffix,
  wf_bucket b ->
  wf_bucket (bucket_delete b suffix).
Proof.
  intros b suffix Hwf.
  unfold wf_bucket in Hwf.
  destruct Hwf as [Huniq [Hcount [Hspace Hdata]]].
  destruct Hspace as [Hsize _].
  unfold bucket_count_valid in Hcount.
  unfold bucket_delete.
  set (entries := entries_delete (bucket_entries b) suffix).
  set (size := bucket_size_for_entries entries).
  change (wf_bucket (mkBucket entries size (BUCKET_PAGE_SIZE - size))).
  unfold wf_bucket. split.
  - unfold bucket_keys_unique. cbn. unfold entries.
    apply entries_delete_unique. exact Huniq.
  - split.
    + unfold bucket_count_valid. cbn. unfold entries.
      pose proof (entries_delete_length_le (bucket_entries b) suffix). lia.
    + split.
      * change (size <= BUCKET_PAGE_SIZE /\
                BUCKET_PAGE_SIZE - size = BUCKET_PAGE_SIZE - size).
        split.
        -- unfold size, bucket_size_for_entries, entries.
           pose proof (entries_delete_space_le (bucket_entries b) suffix).
           unfold bucket_data_size_valid in Hdata.
           lia.
        -- reflexivity.
      * change (size = BUCKET_HEADER_SIZE + entries_space entries).
        unfold size, bucket_size_for_entries. reflexivity.
Qed.

Lemma entries_lookup_app_unique : forall l1 l2 suffix,
  NoDup (map entry_suffix (l1 ++ l2)) ->
  entries_lookup (l1 ++ l2) suffix =
    match entries_lookup l1 suffix with
    | Some v => Some v
    | None => entries_lookup l2 suffix
    end.
Proof.
  induction l1 as [| e rest IH]; intros l2 suffix Huniq; simpl.
  - reflexivity.
  - inversion Huniq as [| x xs Hnotin Htail]; subst.
    destruct (key_eqb suffix (entry_suffix e)) eqn:Heq.
    + apply key_eqb_eq in Heq. subst.
      destruct (entry_value e) as [v|] eqn:Hvalue; [reflexivity|].
      symmetry. apply entries_lookup_not_in.
      intro Hin. apply Hnotin.
      rewrite map_app. apply in_or_app. right. exact Hin.
    + apply IH. exact Htail.
Qed.

(** Split produces well-formed buckets *)
Lemma bucket_split_wf : forall b,
  wf_bucket b ->
  let (bl, br) := bucket_split b in
  wf_bucket bl /\ wf_bucket br.
Proof.
  intros b Hwf.
  unfold wf_bucket in Hwf.
  destruct Hwf as [Huniq [Hcount [Hspace Hdata]]].
  destruct Hspace as [Hsize _].
  unfold bucket_count_valid in Hcount.
  unfold bucket_split, split_point.
  set (mid := length (bucket_entries b) / 2).
  set (left_entries := firstn mid (bucket_entries b)).
  set (right_entries := skipn mid (bucket_entries b)).
  set (left_size := BUCKET_HEADER_SIZE + entries_space left_entries).
  set (right_size := BUCKET_HEADER_SIZE + entries_space right_entries).
  change (wf_bucket (mkBucket left_entries left_size (BUCKET_PAGE_SIZE - left_size)) /\
          wf_bucket (mkBucket right_entries right_size (BUCKET_PAGE_SIZE - right_size))).
  split.
  - unfold wf_bucket. split.
    + unfold bucket_keys_unique. cbn. unfold left_entries.
      rewrite map_firstn. apply NoDup_firstn. exact Huniq.
    + split.
      * unfold bucket_count_valid. cbn. unfold left_entries.
        rewrite length_firstn.
        eapply Nat.le_trans; [apply Nat.le_min_r|exact Hcount].
      * split.
        -- change (left_size <= BUCKET_PAGE_SIZE /\
                   BUCKET_PAGE_SIZE - left_size = BUCKET_PAGE_SIZE - left_size).
           split.
           ++ unfold left_size, left_entries.
              pose proof (entries_space_firstn_le (bucket_entries b) mid).
              unfold bucket_data_size_valid in Hdata.
              lia.
           ++ reflexivity.
        -- change (left_size = BUCKET_HEADER_SIZE + entries_space left_entries).
           unfold left_size. reflexivity.
  - unfold wf_bucket. split.
    + unfold bucket_keys_unique. cbn. unfold right_entries.
      rewrite map_skipn. apply NoDup_skipn. exact Huniq.
    + split.
      * unfold bucket_count_valid. cbn. unfold right_entries.
        rewrite length_skipn. lia.
      * split.
        -- change (right_size <= BUCKET_PAGE_SIZE /\
                   BUCKET_PAGE_SIZE - right_size = BUCKET_PAGE_SIZE - right_size).
           split.
           ++ unfold right_size, right_entries.
              pose proof (entries_space_skipn_le (bucket_entries b) mid).
              unfold bucket_data_size_valid in Hdata.
              lia.
           ++ reflexivity.
        -- change (right_size = BUCKET_HEADER_SIZE + entries_space right_entries).
           unfold right_size. reflexivity.
Qed.

(** Split preserves all entries *)
Lemma bucket_split_preserves : forall b suffix,
  wf_bucket b ->
  let (bl, br) := bucket_split b in
  bucket_lookup b suffix =
    match bucket_lookup bl suffix with
    | Some v => Some v
    | None => bucket_lookup br suffix
    end.
Proof.
  intros b suffix Hwf.
  unfold wf_bucket in Hwf.
  destruct Hwf as [Huniq _].
  unfold bucket_split, bucket_lookup, split_point.
  set (mid := length (bucket_entries b) / 2).
  change (entries_lookup (bucket_entries b) suffix =
    match entries_lookup (firstn mid (bucket_entries b)) suffix with
    | Some v => Some v
    | None => entries_lookup (skipn mid (bucket_entries b)) suffix
    end).
  rewrite <- (firstn_skipn mid (bucket_entries b)) at 1.
  apply entries_lookup_app_unique.
  rewrite firstn_skipn. exact Huniq.
Qed.
