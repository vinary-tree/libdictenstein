(** * Exact/detached eviction capability separation

    Root-bound eviction and fault commits require an exact capability carrying
    both the captured root revision and its exact registry generation.  The
    legacy materialized callback API receives a detached catalog snapshot.  A
    detached snapshot remains safe across catalog replacement because ownership
    retains the immutable old value, but it cannot authorize a root-preserving
    transition.  Semantic mutation needs no callback coordination: its existing
    root CAS clears the exact binding at the linearization point.
*)

From Coq Require Import Arith Bool Lia List PeanoNat.

Module DetachedCallbackSeparationSpec.

Import ListNotations.

Inductive registry_capability : Type :=
| ExactCapability : nat -> nat -> registry_capability
| DetachedCapability : nat -> registry_capability.

Definition authorizes_exact
    (capability : registry_capability)
    (root_revision root_generation : nat) : bool :=
  match capability with
  | ExactCapability captured_revision captured_generation =>
      Nat.eqb captured_revision root_revision
      && Nat.eqb captured_generation root_generation
  | DetachedCapability _ => false
  end.

Theorem detached_never_authorizes_exact :
  forall catalog root_revision root_generation,
    authorizes_exact
      (DetachedCapability catalog) root_revision root_generation = false.
Proof.
  reflexivity.
Qed.

Theorem exact_authority_requires_both_captured_identities :
  forall captured_revision captured_generation root_revision root_generation,
    authorizes_exact
      (ExactCapability captured_revision captured_generation)
      root_revision root_generation = true ->
    captured_revision = root_revision /\
    captured_generation = root_generation.
Proof.
  intros captured_revision captured_generation root_revision root_generation H.
  unfold authorizes_exact in H.
  apply andb_true_iff in H as [Hrevision Hgeneration].
  apply Nat.eqb_eq in Hrevision.
  apply Nat.eqb_eq in Hgeneration.
  auto.
Qed.

Record root_state : Type := mkRootState {
  root_revision : nat;
  root_generation : option nat
}.

Definition semantic_successor (state : root_state) (revision : nat) : root_state :=
  mkRootState revision None.

Theorem semantic_root_cas_clears_exact_authority :
  forall state revision,
    root_generation (semantic_successor state revision) = None.
Proof.
  reflexivity.
Qed.

(** A failed exact commit may be retried only when the winning root revision
    retained the same exact generation.  An unbound or differently bound winner
    has lost the captured authority and must terminate without a futile retry. *)
Inductive failed_exact_commit_outcome : Type :=
| RootAdvanced
| AuthorityLost.

Definition classify_failed_exact_commit
    (captured_generation : nat) (actual : root_state)
    : failed_exact_commit_outcome :=
  match root_generation actual with
  | Some actual_generation =>
      if Nat.eqb captured_generation actual_generation
      then RootAdvanced
      else AuthorityLost
  | None => AuthorityLost
  end.

Theorem semantic_winner_classifies_as_authority_lost :
  forall captured_generation state revision,
    classify_failed_exact_commit
      captured_generation (semantic_successor state revision) = AuthorityLost.
Proof.
  reflexivity.
Qed.

Theorem same_generation_winner_classifies_as_retriable_root_advance :
  forall captured_generation revision,
    classify_failed_exact_commit
      captured_generation (mkRootState revision (Some captured_generation)) =
    RootAdvanced.
Proof.
  intros captured_generation revision.
  change ((if Nat.eqb captured_generation captured_generation
           then RootAdvanced
           else AuthorityLost) = RootAdvanced).
  destruct (Nat.eqb_spec captured_generation captured_generation).
  - reflexivity.
  - contradiction.
Qed.

Theorem different_generation_winner_classifies_as_authority_lost :
  forall captured_generation actual_generation revision,
    captured_generation <> actual_generation ->
    classify_failed_exact_commit
      captured_generation (mkRootState revision (Some actual_generation)) =
    AuthorityLost.
Proof.
  intros captured_generation actual_generation revision Hneq.
  change ((if Nat.eqb captured_generation actual_generation
           then RootAdvanced
           else AuthorityLost) = AuthorityLost).
  destruct (Nat.eqb_spec captured_generation actual_generation) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Record detached_snapshot : Type := mkDetachedSnapshot {
  detached_catalog_id : nat
}.

Definition replace_detached_catalog
    (_old : detached_snapshot) (replacement : detached_snapshot) := replacement.

(** Retaining an immutable callback snapshot is independent of replacing the
    discovery slot.  This is the ownership property implemented by [ArcSwap]:
    replacement changes the slot, not the callback's retained [Arc]. *)
Theorem retained_detached_snapshot_survives_replacement :
  forall retained replacement,
    detached_catalog_id retained = detached_catalog_id retained /\
    replace_detached_catalog retained replacement = replacement.
Proof.
  intros retained replacement. split; reflexivity.
Qed.

Record separated_catalogs : Type := mkSeparatedCatalogs {
  exact_catalog : option nat;
  detached_catalog : option detached_snapshot
}.

Definition publish_exact_checkpoint
    (catalogs : separated_catalogs) (generation : nat) : separated_catalogs :=
  mkSeparatedCatalogs (Some generation) (detached_catalog catalogs).

Theorem exact_checkpoint_does_not_populate_or_replace_detached_catalog :
  forall catalogs generation,
    detached_catalog (publish_exact_checkpoint catalogs generation) =
    detached_catalog catalogs.
Proof.
  reflexivity.
Qed.

Definition clear_detached_catalog (catalogs : separated_catalogs)
    : separated_catalogs :=
  mkSeparatedCatalogs (exact_catalog catalogs) None.

Theorem clearing_detached_catalog_preserves_exact_authority :
  forall catalogs,
    exact_catalog (clear_detached_catalog catalogs) = exact_catalog catalogs /\
    detached_catalog (clear_detached_catalog catalogs) = None.
Proof.
  intros catalogs.
  destruct catalogs.
  split; reflexivity.
Qed.

Definition callback_capability (snapshot : detached_snapshot) : registry_capability :=
  DetachedCapability (detached_catalog_id snapshot).

Corollary callback_snapshot_cannot_construct_exact_authority :
  forall snapshot root_revision root_generation,
    authorizes_exact
      (callback_capability snapshot) root_revision root_generation = false.
Proof.
  intros snapshot root_revision root_generation.
  apply detached_never_authorizes_exact.
Qed.

(** Compatibility inspection reconstructs an owned public value from compact
    immutable metadata plus the segmented path topology. Materialization is
    fallible, matching the allocation and Unicode-scalar checks performed by
    the Rust topology. Hash buckets are ordered and public lookup/remove select
    the last occurrence, exactly as [HashMap<u64, Vec<RegistryPathId>>::last]. *)
Record compact_entry : Type := mkCompactEntry {
  compact_path_hash : nat;
  compact_path_id : nat;
  compact_disk_address : nat;
  compact_size_bytes : nat;
  compact_depth : nat;
  compact_node_type : nat
}.

Record materialized_entry : Type := mkMaterializedEntry {
  materialized_path : list nat;
  materialized_disk_address : nat;
  materialized_size_bytes : nat;
  materialized_depth : nat;
  materialized_node_type : nat
}.

Definition materializer := nat -> option (list nat).

Definition try_materialize_owned
    (path_of : materializer) (entry : compact_entry)
    : option materialized_entry :=
  match path_of (compact_path_id entry) with
  | Some path =>
      Some (mkMaterializedEntry
        path
        (compact_disk_address entry)
        (compact_size_bytes entry)
        (compact_depth entry)
        (compact_node_type entry))
  | None => None
  end.

Theorem owned_materialization_preserves_all_public_fields :
  forall path_of entry path result,
    path_of (compact_path_id entry) = Some path ->
    try_materialize_owned path_of entry = Some result ->
    materialized_path result = path /\
    materialized_disk_address result =
      compact_disk_address entry /\
    materialized_size_bytes result =
      compact_size_bytes entry /\
    materialized_depth result =
      compact_depth entry /\
    materialized_node_type result =
      compact_node_type entry.
Proof.
  intros path_of entry path result Hpath Hmaterialized.
  unfold try_materialize_owned in Hmaterialized.
  rewrite Hpath in Hmaterialized.
  inversion Hmaterialized.
  repeat split; reflexivity.
Qed.

Fixpoint last_option {A : Type} (values : list A) : option A :=
  match values with
  | [] => None
  | value :: [] => Some value
  | _ :: remainder => last_option remainder
  end.

Fixpoint remove_last {A : Type} (values : list A) : list A :=
  match values with
  | [] => []
  | _ :: [] => []
  | value :: remainder => value :: remove_last remainder
  end.

Lemma last_option_map :
  forall (A B : Type) (project : A -> B) values,
    last_option (map project values) =
    option_map project (last_option values).
Proof.
  intros A B project values.
  induction values as [|value values IH].
  - reflexivity.
  - destruct values as [|next remainder].
    + reflexivity.
    + simpl in *. exact IH.
Qed.

Record cacheless_registry_state : Type := mkCachelessRegistryState {
  registry_bucket : list compact_entry;
  registry_authority : nat;
  registry_accounting : nat
}.

Definition cacheless_lookup
    (state : cacheless_registry_state) (path_of : materializer)
    : option materialized_entry :=
  match last_option (registry_bucket state) with
  | Some entry => try_materialize_owned path_of entry
  | None => None
  end.

Definition cacheless_lookup_transition
    (state : cacheless_registry_state) (path_of : materializer)
    : cacheless_registry_state * option materialized_entry :=
  (state, cacheless_lookup state path_of).

Theorem cacheless_lookup_uses_last_collision_occurrence :
  forall authority accounting first last path_of,
    cacheless_lookup
      (mkCachelessRegistryState [first; last] authority accounting)
      path_of =
    try_materialize_owned path_of last.
Proof.
  reflexivity.
Qed.

Definition legacy_cached_bucket
    (path_of : materializer) (bucket : list compact_entry)
    : list (compact_entry * option materialized_entry) :=
  map (fun entry => (entry, try_materialize_owned path_of entry)) bucket.

Definition legacy_cached_lookup
    (bucket : list (compact_entry * option materialized_entry))
    : option materialized_entry :=
  match last_option bucket with
  | Some (_, cached) => cached
  | None => None
  end.

Theorem cache_erasure_refines_legacy_lookup_on_materialization_success :
  forall state path_of result,
    cacheless_lookup state path_of = Some result ->
    legacy_cached_lookup
      (legacy_cached_bucket path_of (registry_bucket state)) = Some result.
Proof.
  intros state path_of result Hlookup.
  unfold legacy_cached_lookup, legacy_cached_bucket.
  rewrite last_option_map.
  unfold cacheless_lookup in Hlookup.
  destruct (last_option (registry_bucket state)) as [entry |] eqn:Hlast.
  - simpl. exact Hlookup.
  - discriminate.
Qed.

Theorem repeated_owned_lookup_is_extensionally_stable :
  forall state path_of first second,
    cacheless_lookup state path_of = Some first ->
    cacheless_lookup state path_of = Some second ->
    first = second.
Proof.
  intros state path_of first second Hfirst Hsecond.
  rewrite Hfirst in Hsecond.
  inversion Hsecond.
  reflexivity.
Qed.

Theorem cacheless_lookup_success_is_read_only :
  forall state path_of result,
    cacheless_lookup state path_of = Some result ->
    fst (cacheless_lookup_transition state path_of) = state.
Proof.
  reflexivity.
Qed.

Theorem cacheless_lookup_failure_is_read_only :
  forall state path_of,
    cacheless_lookup state path_of = None ->
    fst (cacheless_lookup_transition state path_of) = state.
Proof.
  reflexivity.
Qed.

Definition state_after_remove
    (state : cacheless_registry_state) (entry : compact_entry)
    : cacheless_registry_state :=
  mkCachelessRegistryState
    (remove_last (registry_bucket state))
    (registry_authority state)
    (registry_accounting state - compact_size_bytes entry).

(** Materialization is deliberately completed before the first state mutation.
    Thus failure returns the exact pre-state, while success transfers one owned
    projection and removes the same last collision occurrence selected by the
    pre-state lookup. *)
Definition cacheless_remove
    (state : cacheless_registry_state) (path_of : materializer)
    : cacheless_registry_state * option materialized_entry :=
  match last_option (registry_bucket state) with
  | Some entry =>
      match try_materialize_owned path_of entry with
      | Some result => (state_after_remove state entry, Some result)
      | None => (state, None)
      end
  | None => (state, None)
  end.

Theorem cacheless_remove_success_returns_prestate_lookup :
  forall state path_of successor result,
    cacheless_remove state path_of = (successor, Some result) ->
    cacheless_lookup state path_of = Some result.
Proof.
  intros state path_of successor result Hremove.
  unfold cacheless_remove, cacheless_lookup in *.
  destruct (last_option (registry_bucket state)) as [entry |] eqn:Hlast.
  - destruct (try_materialize_owned path_of entry) as [owned |] eqn:Howned.
    + inversion Hremove. reflexivity.
    + discriminate.
  - discriminate.
Qed.

Theorem cacheless_remove_success_removes_last_occurrence :
  forall state path_of successor result,
    cacheless_remove state path_of = (successor, Some result) ->
    registry_bucket successor = remove_last (registry_bucket state).
Proof.
  intros state path_of successor result Hremove.
  unfold cacheless_remove in Hremove.
  destruct (last_option (registry_bucket state)) as [entry |] eqn:Hlast.
  - destruct (try_materialize_owned path_of entry) as [owned |] eqn:Howned.
    + inversion Hremove. reflexivity.
    + discriminate.
  - discriminate.
Qed.

Theorem cacheless_remove_failure_is_atomic :
  forall state path_of,
    snd (cacheless_remove state path_of) = None ->
    fst (cacheless_remove state path_of) = state.
Proof.
  intros state path_of Hfailure.
  unfold cacheless_remove in *.
  destruct (last_option (registry_bucket state)) as [entry |] eqn:Hlast.
  - destruct (try_materialize_owned path_of entry) as [owned |] eqn:Howned.
    + discriminate.
    + reflexivity.
  - reflexivity.
Qed.

Theorem cacheless_remove_preserves_authority :
  forall state path_of,
    registry_authority (fst (cacheless_remove state path_of)) =
    registry_authority state.
Proof.
  intros state path_of.
  unfold cacheless_remove.
  destruct (last_option (registry_bucket state)) as [entry |] eqn:Hlast.
  - destruct (try_materialize_owned path_of entry); reflexivity.
  - reflexivity.
Qed.

Definition byte_materializer (topology : nat -> list nat) : materializer :=
  fun path_id => Some (topology path_id).

Theorem byte_materialization_is_identity :
  forall topology entry,
    try_materialize_owned (byte_materializer topology) entry =
    Some (mkMaterializedEntry
      (topology (compact_path_id entry))
      (compact_disk_address entry)
      (compact_size_bytes entry)
      (compact_depth entry)
      (compact_node_type entry)).
Proof.
  reflexivity.
Qed.

Definition unicode_scalar_b (unit : nat) : bool :=
  Nat.leb unit 1114111 &&
  negb (Nat.leb 55296 unit && Nat.leb unit 57343).

Theorem unicode_scalar_b_spec :
  forall unit,
    unicode_scalar_b unit = true <->
    unit <= 1114111 /\ ~ (55296 <= unit /\ unit <= 57343).
Proof.
  intros unit.
  unfold unicode_scalar_b.
  rewrite andb_true_iff, negb_true_iff, andb_false_iff.
  repeat rewrite Nat.leb_le.
  repeat rewrite Nat.leb_gt.
  lia.
Qed.

Fixpoint decode_char_units (units : list nat) : option (list nat) :=
  match units with
  | [] => Some []
  | unit :: remainder =>
      if unicode_scalar_b unit
      then option_map (cons unit) (decode_char_units remainder)
      else None
  end.

Lemma decode_char_units_projection :
  forall units,
    decode_char_units units =
    if forallb unicode_scalar_b units then Some units else None.
Proof.
  intros units.
  induction units as [|unit remainder IH].
  - reflexivity.
  - simpl.
    destruct (unicode_scalar_b unit) eqn:Hunit.
    + rewrite IH.
      destruct (forallb unicode_scalar_b remainder); reflexivity.
    + reflexivity.
Qed.

Theorem char_materialization_accepts_exactly_unicode_scalars :
  forall units,
    decode_char_units units = Some units <->
    (forall unit, In unit units -> unicode_scalar_b unit = true).
Proof.
  intros units.
  rewrite decode_char_units_projection.
  destruct (forallb unicode_scalar_b units) eqn:Hvalid.
  - split.
    + intros _. apply forallb_forall. exact Hvalid.
    + reflexivity.
  - split.
    + discriminate.
    + intro Hall.
      apply forallb_forall in Hall.
      congruence.
Qed.

Definition materialized_callback_capability
    (catalog_id : nat) (_entry : materialized_entry) : registry_capability :=
  DetachedCapability catalog_id.

Theorem cacheless_materialized_result_has_only_detached_capability :
  forall catalog_id entry root_revision root_generation,
    authorizes_exact
      (materialized_callback_capability catalog_id entry)
      root_revision root_generation = false.
Proof.
  reflexivity.
Qed.

(** The deprecated unit-returning installer is a total projection of the
    fallible installer.  Rejection covers both malformed topology and a
    retired coordinator.  In either case the pre-existing detached catalog is
    preserved; callers that need the reason use the typed [try_*] result. *)
Record detached_install_state : Type := mkDetachedInstallState {
  install_live : bool;
  installed_detached_catalog : option nat
}.

Inductive detached_install_error : Type :=
| MalformedDetachedCatalog
| RetiredDetachedCoordinator.

Definition try_detached_install
    (state : detached_install_state)
    (structurally_valid : bool)
    (candidate : nat)
    : detached_install_state * option detached_install_error :=
  if install_live state
  then
    if structurally_valid
    then
      (mkDetachedInstallState true (Some candidate), None)
    else (state, Some MalformedDetachedCatalog)
  else (state, Some RetiredDetachedCoordinator).

Definition legacy_detached_update
    (state : detached_install_state)
    (structurally_valid : bool)
    (candidate : nat) : detached_install_state :=
  fst (try_detached_install state structurally_valid candidate).

Theorem legacy_update_rejection_preserves_catalog :
  forall state structurally_valid candidate error,
    snd (try_detached_install state structurally_valid candidate) = Some error ->
    legacy_detached_update state structurally_valid candidate = state.
Proof.
  intros [live catalog] structurally_valid candidate error Herror.
  destruct live, structurally_valid; simpl in *;
    try discriminate; reflexivity.
Qed.

Theorem legacy_update_success_installs_candidate :
  forall state candidate,
    install_live state = true ->
    installed_detached_catalog
      (legacy_detached_update state true candidate) = Some candidate.
Proof.
  intros [live catalog] candidate Hlive.
  simpl in Hlive. subst live. reflexivity.
Qed.

Theorem retired_legacy_update_is_state_preserving :
  forall state structurally_valid candidate,
    install_live state = false ->
    legacy_detached_update state structurally_valid candidate = state.
Proof.
  intros [live catalog] structurally_valid candidate Hlive.
  simpl in Hlive. subst live. reflexivity.
Qed.

End DetachedCallbackSeparationSpec.
