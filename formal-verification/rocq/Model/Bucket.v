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

(** Entry count is within bounds *)
Definition bucket_count_valid (b : Bucket) : Prop :=
  length (bucket_entries b) <= MAX_BUCKET_ENTRIES.

(** Space accounting is correct *)
Definition bucket_space_valid (b : Bucket) : Prop :=
  bucket_data_size b + bucket_free_space b = BUCKET_PAGE_SIZE.

(** Bucket is well-formed *)
Definition wf_bucket (b : Bucket) : Prop :=
  bucket_sorted b /\
  bucket_count_valid b /\
  bucket_space_valid b.

(** ** Empty Bucket *)

Definition empty_bucket : Bucket :=
  mkBucket [] BUCKET_HEADER_SIZE (BUCKET_PAGE_SIZE - BUCKET_HEADER_SIZE).

Lemma empty_bucket_wf : wf_bucket empty_bucket.
Proof.
  unfold wf_bucket, empty_bucket. simpl.
  split; [| split].
  - constructor.
  - unfold bucket_count_valid, MAX_BUCKET_ENTRIES. simpl. auto with arith.
  - unfold bucket_space_valid, BUCKET_PAGE_SIZE, BUCKET_HEADER_SIZE. simpl.
    native_compute. reflexivity.
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

Definition binary_search (entries : list BucketEntry) (suffix : Key) : nat :=
  binary_search_aux entries suffix 0 (length entries) (length entries + 1).

(** ** Lookup Operation *)

(** Find an entry by suffix *)
Definition bucket_lookup (b : Bucket) (suffix : Key) : option Value :=
  let pos := binary_search (bucket_entries b) suffix in
  match nth_error (bucket_entries b) pos with
  | None => None
  | Some e =>
      if key_eqb suffix (entry_suffix e)
      then entry_value e
      else None
  end.

(** ** Insert Operation *)

(** Calculate space needed for an entry *)
Definition entry_space (suffix : Key) (value : option Value) : nat :=
  BUCKET_ENTRY_SIZE + length suffix +
  match value with
  | None => 0
  | Some v => length v
  end.

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
  if bucket_has_space b suffix value then
    let entry := mkEntry suffix value in
    let pos := binary_search (bucket_entries b) suffix in
    (* Check if updating existing entry *)
    match nth_error (bucket_entries b) pos with
    | Some e =>
        if key_eqb suffix (entry_suffix e) then
          (* Update existing entry *)
          let new_entries := firstn pos (bucket_entries b) ++
                            [entry] ++
                            skipn (S pos) (bucket_entries b) in
          let old_space := entry_space (entry_suffix e) (entry_value e) in
          let new_space := entry_space suffix value in
          Some (mkBucket new_entries
                        (bucket_data_size b - old_space + new_space)
                        (bucket_free_space b + old_space - new_space))
        else
          (* Insert new entry *)
          let new_entries := insert_at (bucket_entries b) pos entry in
          let space := entry_space suffix value in
          Some (mkBucket new_entries
                        (bucket_data_size b + space)
                        (bucket_free_space b - space))
    | None =>
        (* Insert at end *)
        let new_entries := bucket_entries b ++ [entry] in
        let space := entry_space suffix value in
        Some (mkBucket new_entries
                      (bucket_data_size b + space)
                      (bucket_free_space b - space))
    end
  else
    None.

(** ** Delete Operation *)

(** Delete an entry by suffix *)
Definition bucket_delete (b : Bucket) (suffix : Key) : Bucket :=
  let pos := binary_search (bucket_entries b) suffix in
  match nth_error (bucket_entries b) pos with
  | None => b
  | Some e =>
      if key_eqb suffix (entry_suffix e) then
        let new_entries := firstn pos (bucket_entries b) ++
                          skipn (S pos) (bucket_entries b) in
        let freed := entry_space (entry_suffix e) (entry_value e) in
        mkBucket new_entries
                 (bucket_data_size b - freed)
                 (bucket_free_space b + freed)
      else b
  end.

(** ** Bucket Split *)

(** Determine split point (median) *)
Definition split_point (b : Bucket) : nat :=
  length (bucket_entries b) / 2.

(** Split bucket into two *)
Definition bucket_split (b : Bucket) : Bucket * Bucket :=
  let mid := split_point b in
  let left_entries := firstn mid (bucket_entries b) in
  let right_entries := skipn mid (bucket_entries b) in
  let left_size := fold_left (fun acc e =>
    acc + entry_space (entry_suffix e) (entry_value e)) left_entries 0 in
  let right_size := fold_left (fun acc e =>
    acc + entry_space (entry_suffix e) (entry_value e)) right_entries 0 in
  (mkBucket left_entries
            (BUCKET_HEADER_SIZE + left_size)
            (BUCKET_PAGE_SIZE - BUCKET_HEADER_SIZE - left_size),
   mkBucket right_entries
            (BUCKET_HEADER_SIZE + right_size)
            (BUCKET_PAGE_SIZE - BUCKET_HEADER_SIZE - right_size)).

(** ** Split Key *)

(** Get the first key of the right bucket (split key) *)
Definition bucket_split_key (b : Bucket) : option Key :=
  let mid := split_point b in
  match nth_error (bucket_entries b) mid with
  | None => None
  | Some e => Some (entry_suffix e)
  end.

(** ** Correctness Lemmas *)

(** Binary search finds the correct position *)
Lemma binary_search_correct : forall entries suffix,
  Sorted entry_le entries ->
  let pos := binary_search entries suffix in
  (forall i, i < pos -> i < length entries ->
    key_compare (entry_suffix (nth i entries (mkEntry [] None))) suffix = Lt) /\
  (forall i, pos <= i -> i < length entries ->
    key_compare suffix (entry_suffix (nth i entries (mkEntry [] None))) <> Gt).
Proof.
  (* Proof would require extensive case analysis on binary search *)
  (* Omitted for brevity - would be filled in during actual verification *)
Admitted.

(** Lookup after insert of same key returns the inserted value *)
Lemma bucket_lookup_insert_same : forall b suffix value b',
  wf_bucket b ->
  bucket_insert b suffix value = Some b' ->
  bucket_lookup b' suffix = value.
Proof.
  (* Proof would show insert preserves sortedness and lookup finds the entry *)
Admitted.

(** Lookup after insert of different key returns original value *)
Lemma bucket_lookup_insert_other : forall b suffix1 suffix2 value b',
  wf_bucket b ->
  suffix1 <> suffix2 ->
  bucket_insert b suffix1 value = Some b' ->
  bucket_lookup b' suffix2 = bucket_lookup b suffix2.
Proof.
  (* Proof would show other entries are preserved *)
Admitted.

(** Lookup after delete of same key returns None *)
Lemma bucket_lookup_delete_same : forall b suffix,
  wf_bucket b ->
  bucket_lookup (bucket_delete b suffix) suffix = None.
Proof.
  (* Proof would show the entry is removed *)
Admitted.

(** Insert preserves well-formedness *)
Lemma bucket_insert_wf : forall b suffix value b',
  wf_bucket b ->
  bucket_insert b suffix value = Some b' ->
  wf_bucket b'.
Proof.
  (* Proof would show sortedness and bounds are preserved *)
Admitted.

(** Delete preserves well-formedness *)
Lemma bucket_delete_wf : forall b suffix,
  wf_bucket b ->
  wf_bucket (bucket_delete b suffix).
Proof.
  (* Proof would show sortedness and bounds are preserved *)
Admitted.

(** Split produces well-formed buckets *)
Lemma bucket_split_wf : forall b,
  wf_bucket b ->
  let (bl, br) := bucket_split b in
  wf_bucket bl /\ wf_bucket br.
Proof.
  (* Proof would show both halves maintain invariants *)
Admitted.

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
  (* Proof would show entries are partitioned between the two buckets *)
Admitted.
