(** * PersistentPrefixSpec: Persistent Char Trie Prefix Semantics

    This module states the semantic proof boundary for
    [PersistentARTrieChar]'s public prefix APIs.  The Rust implementation has
    several operational variants:

    - term-only prefix iteration;
    - value-carrying prefix iteration;
    - arena-carrying prefix iteration; and
    - ordinary and batched prefix deletion.

    The checked claim is intentionally semantic.  Prefix filtering and deletion
    refine a reference finite map over character keys.  Arena IDs and batch
    sizes may affect traversal order and locality, but they must not affect the
    returned term/value set or the post-delete map.
*)

From Stdlib Require Import Lists.List.
From Stdlib Require Import Bool.Bool.
From Stdlib Require Import Arith.PeanoNat.
From Stdlib Require Import ZArith.ZArith.
From Stdlib Require Import Logic.FunctionalExtensionality.
Import ListNotations.

Definition CharLabel := nat.
Definition CharKey := list CharLabel.
Definition ArenaId := nat.

Definition CharMap (V : Type) := CharKey -> option V.
Definition CharSet := CharKey -> bool.

Definition char_key_eq_dec
  (left right : CharKey) : {left = right} + {left <> right} :=
  list_eq_dec Nat.eq_dec left right.

Definition same_char_map {V : Type} (left right : CharMap V) : Prop :=
  forall key, left key = right key.

Definition map_empty {V : Type} : CharMap V := fun _ => None.

Definition starts_with (prefix key : CharKey) : Prop :=
  exists suffix, key = prefix ++ suffix.

Fixpoint starts_with_bool (prefix key : CharKey) : bool :=
  match prefix, key with
  | [], _ => true
  | _ :: _, [] => false
  | p :: ps, k :: ks =>
      if Nat.eq_dec p k then starts_with_bool ps ks else false
  end.

Lemma starts_with_cons_inv :
  forall p ps k ks,
    starts_with (p :: ps) (k :: ks) ->
    p = k /\ starts_with ps ks.
Proof.
  intros p ps k ks Hprefix.
  destruct Hprefix as [suffix Heq].
  inversion Heq; subst.
  split.
  - reflexivity.
  - exists suffix. reflexivity.
Qed.

Lemma starts_with_bool_correct :
  forall prefix key,
    starts_with_bool prefix key = true <-> starts_with prefix key.
Proof.
  induction prefix as [| p ps IH]; intros key.
  - split.
    + intros _. exists key. reflexivity.
    + intros _. reflexivity.
  - destruct key as [| k ks].
    + split.
      * intros H. discriminate H.
      * intros [suffix Heq]. discriminate Heq.
    + simpl.
      destruct (Nat.eq_dec p k) as [Heq | Hneq].
      * split.
        -- intros Hmatch.
           destruct (proj1 (IH ks) Hmatch) as [suffix Hsuffix].
           subst k. exists suffix. simpl. f_equal. exact Hsuffix.
        -- intros Hprefix.
           apply (proj2 (IH ks)).
           destruct (starts_with_cons_inv p ps k ks Hprefix) as [_ Htail].
           exact Htail.
      * split.
        -- intros H. discriminate H.
        -- intros Hprefix.
           destruct (starts_with_cons_inv p ps k ks Hprefix) as [Hhead _].
           contradiction.
Qed.

Definition starts_with_dec
  (prefix key : CharKey) : {starts_with prefix key} + {~ starts_with prefix key}.
Proof.
  destruct (starts_with_bool prefix key) eqn:Hmatch.
  - left. apply (proj1 (starts_with_bool_correct prefix key)). exact Hmatch.
  - right. intro Hprefix.
    pose proof (proj2 (starts_with_bool_correct prefix key) Hprefix) as Htrue.
    rewrite Hmatch in Htrue. discriminate Htrue.
Defined.

Definition map_domain {V : Type} (map : CharMap V) : CharSet :=
  fun key =>
    match map key with
    | Some _ => true
    | None => false
    end.

Definition set_prefix_filter (set : CharSet) (prefix : CharKey) : CharSet :=
  fun key => if starts_with_dec prefix key then set key else false.

Definition prefix_filter {V : Type}
  (map : CharMap V) (prefix : CharKey) : CharMap V :=
  fun key => if starts_with_dec prefix key then map key else None.

Definition remove_prefix {V : Type}
  (map : CharMap V) (prefix : CharKey) : CharMap V :=
  fun key => if starts_with_dec prefix key then None else map key.

(** Batching is operational only.  The reference post-state is independent of
    whether Rust removes one term per batch, 1024 terms per batch, or all terms
    at once. *)
Definition remove_prefix_batched {V : Type}
  (map : CharMap V) (prefix : CharKey) (_batch_size : nat) : CharMap V :=
  remove_prefix map prefix.

Theorem prefix_filter_sound :
  forall (V : Type) (map : CharMap V) prefix key value,
    prefix_filter map prefix key = Some value ->
    map key = Some value /\ starts_with prefix key.
Proof.
  intros V map prefix key value Hlookup.
  unfold prefix_filter in Hlookup.
  destruct (starts_with_dec prefix key) as [Hprefix | Hnot].
  - split; assumption.
  - discriminate Hlookup.
Qed.

Theorem prefix_filter_complete :
  forall (V : Type) (map : CharMap V) prefix key value,
    map key = Some value ->
    starts_with prefix key ->
    prefix_filter map prefix key = Some value.
Proof.
  intros V map prefix key value Hlookup Hprefix.
  unfold prefix_filter.
  destruct (starts_with_dec prefix key) as [_ | Hnot].
  - exact Hlookup.
  - contradiction.
Qed.

Theorem prefix_filter_rejects_nonmatching :
  forall (V : Type) (map : CharMap V) prefix key,
    ~ starts_with prefix key ->
    prefix_filter map prefix key = None.
Proof.
  intros V map prefix key Hnot.
  unfold prefix_filter.
  destruct (starts_with_dec prefix key) as [Hprefix | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem prefix_filter_empty_returns_map :
  forall (V : Type) (map : CharMap V),
    prefix_filter map [] = map.
Proof.
  intros V map.
  apply functional_extensionality.
  intros key.
  unfold prefix_filter.
  destruct (starts_with_dec [] key) as [_ | Hnot].
  - reflexivity.
  - exfalso. apply Hnot. exists key. reflexivity.
Qed.

Theorem prefix_filter_missing_is_empty :
  forall (V : Type) (map : CharMap V) prefix,
    (forall key value, map key = Some value -> ~ starts_with prefix key) ->
    prefix_filter map prefix = map_empty.
Proof.
  intros V map prefix Hmissing.
  apply functional_extensionality.
  intros key.
  unfold prefix_filter, map_empty.
  destruct (starts_with_dec prefix key) as [Hprefix | _].
  - destruct (map key) as [value |] eqn:Hlookup.
    + exfalso. exact (Hmissing key value Hlookup Hprefix).
    + reflexivity.
  - reflexivity.
Qed.

Theorem prefix_filter_domain_agrees_with_set_filter :
  forall (V : Type) (map : CharMap V) prefix,
    map_domain (prefix_filter map prefix) =
    set_prefix_filter (map_domain map) prefix.
Proof.
  intros V map prefix.
  apply functional_extensionality.
  intros key.
  unfold map_domain, prefix_filter, set_prefix_filter.
  destruct (starts_with_dec prefix key) as [_ | _];
    destruct (map key) as [_ |]; reflexivity.
Qed.

Theorem remove_prefix_removes_matching :
  forall (V : Type) (map : CharMap V) prefix key,
    starts_with prefix key ->
    remove_prefix map prefix key = None.
Proof.
  intros V map prefix key Hprefix.
  unfold remove_prefix.
  destruct (starts_with_dec prefix key) as [_ | Hnot].
  - reflexivity.
  - contradiction.
Qed.

Theorem remove_prefix_preserves_nonmatching :
  forall (V : Type) (map : CharMap V) prefix key,
    ~ starts_with prefix key ->
    remove_prefix map prefix key = map key.
Proof.
  intros V map prefix key Hnot.
  unfold remove_prefix.
  destruct (starts_with_dec prefix key) as [Hprefix | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem remove_prefix_empty_clears_map :
  forall (V : Type) (map : CharMap V),
    remove_prefix map [] = map_empty.
Proof.
  intros V map.
  apply functional_extensionality.
  intros key.
  unfold remove_prefix, map_empty.
  destruct (starts_with_dec [] key) as [_ | Hnot].
  - reflexivity.
  - exfalso. apply Hnot. exists key. reflexivity.
Qed.

Theorem remove_prefix_idempotent :
  forall (V : Type) (map : CharMap V) prefix,
    remove_prefix (remove_prefix map prefix) prefix =
    remove_prefix map prefix.
Proof.
  intros V map prefix.
  apply functional_extensionality.
  intros key.
  unfold remove_prefix.
  destruct (starts_with_dec prefix key) as [Hprefix | Hnot].
  - destruct (starts_with_dec prefix key) as [_ | Hcontra].
    + reflexivity.
    + contradiction.
  - reflexivity.
Qed.

Theorem remove_prefix_clears_prefix_filter :
  forall (V : Type) (map : CharMap V) prefix,
    prefix_filter (remove_prefix map prefix) prefix = map_empty.
Proof.
  intros V map prefix.
  apply functional_extensionality.
  intros key.
  unfold prefix_filter, remove_prefix, map_empty.
  destruct (starts_with_dec prefix key) as [Hprefix | Hnot].
  - destruct (starts_with_dec prefix key) as [_ | Hcontra].
    + reflexivity.
    + contradiction.
  - reflexivity.
Qed.

Theorem remove_prefix_batched_equiv_unbatched :
  forall (V : Type) (map : CharMap V) prefix batch_size,
    remove_prefix_batched map prefix batch_size =
    remove_prefix map prefix.
Proof.
  reflexivity.
Qed.

(** ** Durable-prefix deletion correspondence

    The implementation of [remove_prefix_batched] is intentionally not a
    transaction.  It first collects a batch, then appends one remove record
    before each visible removal.  A crash can therefore recover any durable
    prefix of the per-term remove records, but no key may disappear unless its
    remove record exists in that durable prefix.
*)

Definition remove_key {V : Type} (map : CharMap V) (target : CharKey)
  : CharMap V :=
  fun key => if char_key_eq_dec key target then None else map key.

Fixpoint remove_keys {V : Type} (map : CharMap V) (keys : list CharKey)
  : CharMap V :=
  match keys with
  | [] => map
  | key :: rest => remove_keys (remove_key map key) rest
  end.

Theorem remove_keys_none_stays_none :
  forall (V : Type) (map : CharMap V) keys key,
    map key = None ->
    remove_keys map keys key = None.
Proof.
  intros V map keys.
  revert map.
  induction keys as [| head rest IH]; intros map key Hnone.
  - exact Hnone.
  - simpl.
    apply IH.
    unfold remove_key.
    destruct (char_key_eq_dec key head) as [_ | _].
    + reflexivity.
    + exact Hnone.
Qed.

Theorem remove_keys_removes_listed :
  forall (V : Type) (map : CharMap V) keys key,
    In key keys ->
    remove_keys map keys key = None.
Proof.
  intros V map keys.
  revert map.
  induction keys as [| head rest IH]; intros map key Hin.
  - contradiction.
  - simpl in Hin |- *.
    destruct Hin as [Heq | Hin].
    + subst head.
      apply remove_keys_none_stays_none.
      unfold remove_key.
      destruct (char_key_eq_dec key key) as [_ | Hneq].
      * reflexivity.
      * contradiction.
    + apply IH. exact Hin.
Qed.

Theorem remove_keys_preserves_not_listed :
  forall (V : Type) (map : CharMap V) keys key,
    ~ In key keys ->
    remove_keys map keys key = map key.
Proof.
  intros V map keys.
  revert map.
  induction keys as [| head rest IH]; intros map key Hnot.
  - reflexivity.
  - simpl.
    rewrite IH.
    + unfold remove_key.
      destruct (char_key_eq_dec key head) as [Heq | _].
      * exfalso. apply Hnot. left. exact (eq_sym Heq).
      * reflexivity.
    + intro Hin. apply Hnot. right. exact Hin.
Qed.

Theorem durable_prefix_visible_delete_has_record :
  forall (V : Type) (map : CharMap V) durable_keys key,
    map key <> None ->
    remove_keys map durable_keys key = None ->
    In key durable_keys.
Proof.
  intros V map durable_keys key Hpresent Hremoved.
  destruct (in_dec char_key_eq_dec key durable_keys) as [Hin | Hnot].
  - exact Hin.
  - rewrite remove_keys_preserves_not_listed in Hremoved.
    + contradiction.
    + exact Hnot.
Qed.

Theorem durable_prefix_preserves_nonrecords :
  forall (V : Type) (map : CharMap V) durable_keys key,
    ~ In key durable_keys ->
    remove_keys map durable_keys key = map key.
Proof.
  exact remove_keys_preserves_not_listed.
Qed.

Theorem durable_prefix_preserves_nonmatching_when_records_are_prefixed :
  forall (V : Type) (map : CharMap V) durable_keys prefix key,
    (forall record_key, In record_key durable_keys -> starts_with prefix record_key) ->
    ~ starts_with prefix key ->
    remove_keys map durable_keys key = map key.
Proof.
  intros V map durable_keys prefix key Hrecords Hnot_prefix.
  apply remove_keys_preserves_not_listed.
  intro Hin.
  exact (Hnot_prefix (Hrecords key Hin)).
Qed.

(** ** Checked numeric RMW correspondence *)

Definition checked_i64_add
  (min_value max_value current delta : Z) : option Z :=
  let result := (current + delta)%Z in
  if (min_value <=? result)%Z && (result <=? max_value)%Z
  then Some result
  else None.

Theorem checked_i64_add_success_in_range :
  forall min_value max_value current delta result,
    checked_i64_add min_value max_value current delta = Some result ->
    result = (current + delta)%Z /\
    (min_value <= result <= max_value)%Z.
Proof.
  intros min_value max_value current delta result Hchecked.
  unfold checked_i64_add in Hchecked.
  destruct (((min_value <=? current + delta)%Z &&
            (current + delta <=? max_value)%Z)) eqn:Hrange.
  - inversion Hchecked; subst result.
    apply andb_true_iff in Hrange.
    destruct Hrange as [Hlower Hupper].
    apply Z.leb_le in Hlower.
    apply Z.leb_le in Hupper.
    split; [reflexivity | split; assumption].
  - discriminate Hchecked.
Qed.

Theorem checked_i64_add_overflow_none :
  forall min_value max_value current delta,
    checked_i64_add min_value max_value current delta = None ->
    ((current + delta < min_value) \/ (max_value < current + delta))%Z.
Proof.
  intros min_value max_value current delta Hchecked.
  unfold checked_i64_add in Hchecked.
  destruct (((min_value <=? current + delta)%Z &&
            (current + delta <=? max_value)%Z)) eqn:Hrange.
  - discriminate Hchecked.
  - apply andb_false_iff in Hrange.
    destruct Hrange as [Hlower | Hupper].
    + apply Z.leb_gt in Hlower. left. exact Hlower.
    + apply Z.leb_gt in Hupper. right. exact Hupper.
Qed.

(** ** Executable-list correspondence shape *)

Definition Entry (V : Type) := (CharKey * V)%type.

Fixpoint entry_lookup {V : Type} (entries : list (Entry V)) (query : CharKey)
  : option V :=
  match entries with
  | [] => None
  | (key, value) :: rest =>
      if char_key_eq_dec key query then Some value else entry_lookup rest query
  end.

Definition entry_keys {V : Type} (entries : list (Entry V)) : list CharKey :=
  map fst entries.

Record PrefixValueEntriesExact {V : Type}
  (map : CharMap V) (prefix : CharKey) (entries : list (Entry V)) : Prop := {
  prefix_entries_lookup_exact :
    forall key, entry_lookup entries key = prefix_filter map prefix key;
  prefix_entries_no_duplicate_keys :
    NoDup (entry_keys entries)
}.

Theorem prefix_entries_sound :
  forall (V : Type) (map : CharMap V) prefix entries key value,
    PrefixValueEntriesExact map prefix entries ->
    entry_lookup entries key = Some value ->
    map key = Some value /\ starts_with prefix key.
Proof.
  intros V map prefix entries key value Hentries Hlookup.
  destruct Hentries as [Hexact _].
  rewrite Hexact in Hlookup.
  exact (prefix_filter_sound V map prefix key value Hlookup).
Qed.

Theorem prefix_entries_complete :
  forall (V : Type) (map : CharMap V) prefix entries key value,
    PrefixValueEntriesExact map prefix entries ->
    map key = Some value ->
    starts_with prefix key ->
    entry_lookup entries key = Some value.
Proof.
  intros V map prefix entries key value Hentries Hlookup Hprefix.
  destruct Hentries as [Hexact _].
  rewrite Hexact.
  exact (prefix_filter_complete V map prefix key value Hlookup Hprefix).
Qed.

Record PrefixTermWithArena := {
  arena_term : CharKey;
  arena_id : option ArenaId
}.

Record PrefixTermWithValueAndArena (V : Type) := {
  value_arena_term : CharKey;
  value_arena_value : V;
  value_arena_id : option ArenaId
}.

Definition strip_arena_terms (entries : list PrefixTermWithArena) : list CharKey :=
  map arena_term entries.

Definition strip_value_arena {V : Type}
  (entries : list (PrefixTermWithValueAndArena V)) : list (Entry V) :=
  map (fun entry => (value_arena_term V entry, value_arena_value V entry)) entries.

Record ArenaValueEntriesExact {V : Type}
  (map : CharMap V)
  (prefix : CharKey)
  (entries : list (PrefixTermWithValueAndArena V)) : Prop := {
  arena_entries_exact :
    PrefixValueEntriesExact map prefix (strip_value_arena entries)
}.

Theorem arena_value_entries_sound :
  forall (V : Type) map prefix entries key value,
    ArenaValueEntriesExact (V := V) map prefix entries ->
    entry_lookup (strip_value_arena entries) key = Some value ->
    map key = Some value /\ starts_with prefix key.
Proof.
  intros V map prefix entries key value Harena Hlookup.
  destruct Harena as [Hentries].
  exact (prefix_entries_sound V map prefix (strip_value_arena entries) key value
    Hentries Hlookup).
Qed.

Theorem arena_metadata_does_not_change_value_projection :
  forall (V : Type) map prefix entries key,
    ArenaValueEntriesExact (V := V) map prefix entries ->
    entry_lookup (strip_value_arena entries) key = prefix_filter map prefix key.
Proof.
  intros V map prefix entries key Harena.
  destruct Harena as [[Hexact _]].
  exact (Hexact key).
Qed.

Theorem arena_metadata_does_not_create_duplicates :
  forall (V : Type) map prefix entries,
    ArenaValueEntriesExact (V := V) map prefix entries ->
    NoDup (entry_keys (strip_value_arena entries)).
Proof.
  intros V map prefix entries Harena.
  destruct Harena as [[_ Hnodup]].
  exact Hnodup.
Qed.

Theorem arena_term_projection_membership :
  forall entries key,
    In key (strip_arena_terms entries) <->
    exists arena, In {| arena_term := key; arena_id := arena |} entries.
Proof.
  intros entries key.
  unfold strip_arena_terms.
  split.
  - intros Hin.
    apply in_map_iff in Hin.
    destruct Hin as [entry [Hkey Hentry]].
    exists (arena_id entry).
    destruct entry as [term arena].
    simpl in Hkey. subst.
    exact Hentry.
  - intros [arena Hentry].
    apply in_map_iff.
    exists {| arena_term := key; arena_id := arena |}.
    split; [reflexivity | exact Hentry].
Qed.
