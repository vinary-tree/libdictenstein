(** * PersistentMergeSpec: Cursor Pagination and Merge Correspondence

    This module states the semantic proof boundary for libdictenstein's
    persistent byte trie merge paths.  The Rust implementation has several
    operational variants:

    - cursor-based prefix iteration used to keep batches memory-bounded;
    - ordinary batched merge;
    - arena-grouped batched merge; and
    - feature-gated parallel read/merge followed by serialized insertion.

    The checked claim here is semantic, not performance-oriented: once cursor
    pagination covers the source map without skipped or duplicated keys, every
    batching/grouping/partitioning strategy refines the same reference map
    merge.
*)

From Stdlib Require Import Lists.List.
From Stdlib Require Import Arith.PeanoNat.
From Stdlib Require Import Logic.FunctionalExtensionality.
Require Import ARTrie.Spec.MapSpec.
Import ListNotations.

Definition Key := MapSpec.Key.

Definition DictMap (V : Type) := Key -> option V.

Definition same_map {V : Type} (a b : DictMap V) : Prop :=
  forall k, a k = b k.

Definition starts_with (prefix key : Key) : Prop :=
  exists suffix, key = prefix ++ suffix.

Section PersistentMerge.

Variable V : Type.
Variable merge_value : V -> V -> V.
Variable key_lt : Key -> Key -> Prop.

Definition Entry := (Key * V)%type.
Definition Page := list Entry.
Definition Pages := list Page.

Fixpoint page_lookup (page : Page) (query : Key) : option V :=
  match page with
  | [] => None
  | (key, value) :: rest =>
      if MapSpec.key_eq_dec key query then Some value else page_lookup rest query
  end.

Definition flatten_pages (pages : Pages) : Page := concat pages.

Definition after_cursor (cursor : option Key) (key : Key) : Prop :=
  match cursor with
  | None => True
  | Some c => key_lt c key
  end.

Definition page_keys (page : Page) : list Key := map fst page.

Fixpoint page_sorted_by_cursor (page : Page) : Prop :=
  match page with
  | [] => True
  | [_] => True
  | (before_key, _) :: ((after_key, _) :: _ as rest) =>
      key_lt before_key after_key /\ page_sorted_by_cursor rest
  end.

Record CursorPageLaws
  (source : DictMap V)
  (prefix : Key)
  (cursor : option Key)
  (limit : nat)
  (page : Page) : Prop := {
  cursor_page_sound :
    forall key value,
      page_lookup page key = Some value ->
      source key = Some value /\ starts_with prefix key /\ after_cursor cursor key;
  cursor_page_no_duplicate_keys :
    NoDup (page_keys page);
  cursor_page_within_limit :
    length page <= limit;
  cursor_page_sorted :
    page_sorted_by_cursor page
}.

Record PaginationLaws
  (source : DictMap V)
  (pages : Pages) : Prop := {
  pagination_lookup_exact :
    forall key,
      page_lookup (flatten_pages pages) key = source key;
  pagination_no_duplicate_keys :
    NoDup (page_keys (flatten_pages pages))
}.

Definition merge_map (target source : DictMap V) : DictMap V :=
  fun key =>
    match target key, source key with
    | Some old, Some new => Some (merge_value old new)
    | None, Some new => Some new
    | Some old, None => Some old
    | None, None => None
    end.

Definition apply_page_merge (target : DictMap V) (page : Page) : DictMap V :=
  fun key =>
    match target key, page_lookup page key with
    | Some old, Some new => Some (merge_value old new)
    | None, Some new => Some new
    | Some old, None => Some old
    | None, None => None
    end.

Definition apply_pages_merge (target : DictMap V) (pages : Pages) : DictMap V :=
  apply_page_merge target (flatten_pages pages).

Definition equivalent_pages (left right : Page) : Prop :=
  forall key, page_lookup left key = page_lookup right key.

Theorem merge_lookup_conflict : forall target source key old new,
  target key = Some old ->
  source key = Some new ->
  merge_map target source key = Some (merge_value old new).
Proof.
  intros target source key old new Htarget Hsource.
  unfold merge_map.
  rewrite Htarget, Hsource.
  reflexivity.
Qed.

Theorem merge_lookup_source_only : forall target source key new,
  target key = None ->
  source key = Some new ->
  merge_map target source key = Some new.
Proof.
  intros target source key new Htarget Hsource.
  unfold merge_map.
  rewrite Htarget, Hsource.
  reflexivity.
Qed.

Theorem merge_lookup_target_only : forall target source key old,
  target key = Some old ->
  source key = None ->
  merge_map target source key = Some old.
Proof.
  intros target source key old Htarget Hsource.
  unfold merge_map.
  rewrite Htarget, Hsource.
  reflexivity.
Qed.

Theorem exact_page_merge_matches_reference : forall target source page,
  (forall key, page_lookup page key = source key) ->
  apply_page_merge target page = merge_map target source.
Proof.
  intros target source page Hexact.
  apply functional_extensionality.
  intros key.
  unfold apply_page_merge, merge_map.
  rewrite Hexact.
  destruct (target key), (source key); reflexivity.
Qed.

Theorem batched_merge_equiv_single_pass : forall target source pages,
  PaginationLaws source pages ->
  apply_pages_merge target pages = merge_map target source.
Proof.
  intros target source pages Hpages.
  unfold apply_pages_merge.
  apply exact_page_merge_matches_reference.
  exact (pagination_lookup_exact source pages Hpages).
Qed.

Theorem grouped_page_order_preserves_merge : forall target page grouped,
  equivalent_pages page grouped ->
  apply_page_merge target grouped = apply_page_merge target page.
Proof.
  intros target page grouped Hequiv.
  apply functional_extensionality.
  intros key.
  unfold apply_page_merge.
  rewrite Hequiv.
  reflexivity.
Qed.

Theorem grouped_batched_merge_equiv_single_pass :
  forall target source pages grouped_pages,
    PaginationLaws source pages ->
    equivalent_pages (flatten_pages pages) (flatten_pages grouped_pages) ->
    apply_pages_merge target grouped_pages = merge_map target source.
Proof.
  intros target source pages grouped_pages Hpages Hequiv.
  unfold apply_pages_merge.
  rewrite (grouped_page_order_preserves_merge
    target (flatten_pages pages) (flatten_pages grouped_pages) Hequiv).
  exact (batched_merge_equiv_single_pass target source pages Hpages).
Qed.

Theorem parallel_partition_merge_equiv_single_pass :
  forall target source partitions,
    PaginationLaws source partitions ->
    apply_pages_merge target partitions = merge_map target source.
Proof.
  intros target source partitions Hpartitions.
  exact (batched_merge_equiv_single_pass target source partitions Hpartitions).
Qed.

Theorem cursor_page_entries_are_source_entries :
  forall source prefix cursor limit page key value,
    CursorPageLaws source prefix cursor limit page ->
    page_lookup page key = Some value ->
    source key = Some value.
Proof.
  intros source prefix cursor limit page key value Hpage Hlookup.
  destruct (cursor_page_sound source prefix cursor limit page Hpage key value Hlookup)
    as [Hsource _].
  exact Hsource.
Qed.

Theorem cursor_page_entries_match_prefix :
  forall source prefix cursor limit page key value,
    CursorPageLaws source prefix cursor limit page ->
    page_lookup page key = Some value ->
    starts_with prefix key.
Proof.
  intros source prefix cursor limit page key value Hpage Hlookup.
  destruct (cursor_page_sound source prefix cursor limit page Hpage key value Hlookup)
    as [_ [Hprefix _]].
  exact Hprefix.
Qed.

Theorem cursor_page_entries_are_after_cursor :
  forall source prefix cursor limit page key value,
    CursorPageLaws source prefix cursor limit page ->
    page_lookup page key = Some value ->
    after_cursor cursor key.
Proof.
  intros source prefix cursor limit page key value Hpage Hlookup.
  destruct (cursor_page_sound source prefix cursor limit page Hpage key value Hlookup)
    as [_ [_ Hcursor]].
  exact Hcursor.
Qed.

End PersistentMerge.
