(** * Exact-root eviction publication

    This specification captures the unbounded logical contract implemented by
    the byte and character persistent ARTries. Semantic writers do not acquire
    a registry mutex or own a publication counter. Their existing root
    compare-and-swap publishes a semantic successor whose eviction binding is
    absent. Exact checkpoint, eviction, and fault operations carry the root
    revision and opaque registry generation they observed and may commit only
    against that exact pair.

    Detached compatibility callbacks are intentionally absent here. Their
    immutable advisory capability and freedom to overlap exact publication are
    proved in [DetachedCallbackSeparationSpec].
*)

From Coq Require Import Arith Bool Lia List PeanoNat.

Module EvictionExactRootPublicationSpec.

Import ListNotations.

Record state : Type := {
  root_revision : nat;
  root_generation : option nat;
  registry_revision : option nat;
  registry_generation : option nat;
  registry_stamped : bool;
  coordinator_retired : bool
}.

Definition exact_binding_safe (s : state) : Prop :=
  match root_generation s with
  | None => True
  | Some generation =>
      registry_revision s = Some (root_revision s) /\
      registry_generation s = Some generation /\
      registry_stamped s = true /\
      coordinator_retired s = false
  end.

Definition initial_state : state :=
  {| root_revision := 0;
     root_generation := None;
     registry_revision := None;
     registry_generation := None;
     registry_stamped := false;
     coordinator_retired := false |}.

Definition semantic_successor (s : state) (revision : nat) : state :=
  {| root_revision := revision;
     root_generation := None;
     registry_revision := registry_revision s;
     registry_generation := registry_generation s;
     registry_stamped := registry_stamped s;
     coordinator_retired := coordinator_retired s |}.

Definition exact_checkpoint_success (s : state) (generation : nat) : state :=
  {| root_revision := root_revision s;
     root_generation := Some generation;
     registry_revision := Some (root_revision s);
     registry_generation := Some generation;
     registry_stamped := true;
     coordinator_retired := false |}.

Definition retire_coordinator (s : state) : state :=
  {| root_revision := root_revision s;
     root_generation := None;
     registry_revision := registry_revision s;
     registry_generation := registry_generation s;
     registry_stamped := registry_stamped s;
     coordinator_retired := true |}.

Definition failed_checkpoint_publication (s : state) : state := s.

Definition exact_capability : Type := nat * nat.

Definition exact_commit_authorized
    (s : state) (capability : exact_capability) : bool :=
  let '(revision, generation) := capability in
  Nat.eqb revision (root_revision s)
  && match root_generation s with
     | Some root_gen => Nat.eqb generation root_gen
     | None => false
     end
  && match registry_revision s with
     | Some registry_rev => Nat.eqb revision registry_rev
     | None => false
     end
  && match registry_generation s with
     | Some registry_gen => Nat.eqb generation registry_gen
     | None => false
     end
  && registry_stamped s
  && negb (coordinator_retired s).

Definition captured_root_matches
    (s : state) (captured : exact_capability) : bool :=
  let '(revision, generation) := captured in
  Nat.eqb revision (root_revision s)
  && match root_generation s with
     | Some root_gen => Nat.eqb generation root_gen
     | None => false
     end.

Theorem initial_state_safe :
  exact_binding_safe initial_state.
Proof.
  exact I.
Qed.

Theorem semantic_root_cas_clears_exact_authority :
  forall s revision,
    root_generation (semantic_successor s revision) = None.
Proof.
  reflexivity.
Qed.

Theorem semantic_successor_is_safe :
  forall s revision,
    exact_binding_safe (semantic_successor s revision).
Proof.
  intros s revision.
  exact I.
Qed.

Theorem exact_checkpoint_success_is_safe :
  forall s generation,
    exact_binding_safe (exact_checkpoint_success s generation).
Proof.
  intros s generation.
  simpl.
  repeat split; reflexivity.
Qed.

Theorem retirement_fences_exact_authority :
  forall s,
    root_generation (retire_coordinator s) = None /\
    coordinator_retired (retire_coordinator s) = true /\
    exact_binding_safe (retire_coordinator s).
Proof.
  intros s.
  repeat split; try reflexivity.
Qed.

Theorem failed_publication_preserves_registry :
  forall s,
    registry_revision (failed_checkpoint_publication s) =
      registry_revision s /\
    registry_generation (failed_checkpoint_publication s) =
      registry_generation s /\
    registry_stamped (failed_checkpoint_publication s) =
      registry_stamped s /\
    root_generation (failed_checkpoint_publication s) =
      root_generation s.
Proof.
  intros s.
  repeat split; reflexivity.
Qed.

Theorem authorized_commit_has_exact_root_and_registry :
  forall s revision generation,
    exact_commit_authorized s (revision, generation) = true ->
    revision = root_revision s /\
    root_generation s = Some generation /\
    registry_revision s = Some revision /\
    registry_generation s = Some generation /\
    registry_stamped s = true /\
    coordinator_retired s = false.
Proof.
  intros s revision generation H.
  unfold exact_commit_authorized in H.
  apply andb_true_iff in H as [Hprefix Hretired].
  apply andb_true_iff in Hprefix as [Hprefix Hstamped].
  apply andb_true_iff in Hprefix as [Hprefix HgenerationRegistryCheck].
  apply andb_true_iff in Hprefix as [Hprefix HrevisionRegistryCheck].
  apply andb_true_iff in Hprefix as [HrevisionRoot HgenerationRootCheck].
  apply Nat.eqb_eq in HrevisionRoot.
  apply negb_true_iff in Hretired.
  destruct (root_generation s) as [root_gen |] eqn:Hroot;
    simpl in HgenerationRootCheck;
    try discriminate.
  apply Nat.eqb_eq in HgenerationRootCheck.
  destruct (registry_revision s) as [registry_rev |] eqn:Hrevision;
    simpl in HrevisionRegistryCheck;
    try discriminate.
  apply Nat.eqb_eq in HrevisionRegistryCheck.
  destruct (registry_generation s) as [registry_gen |] eqn:Hgeneration;
    simpl in HgenerationRegistryCheck;
    try discriminate.
  apply Nat.eqb_eq in HgenerationRegistryCheck.
  split; [exact HrevisionRoot |].
  split.
  - now rewrite HgenerationRootCheck.
  - split.
    + now rewrite HrevisionRegistryCheck.
    + split.
      * now rewrite HgenerationRegistryCheck.
      * split.
        -- exact Hstamped.
        -- exact Hretired.
Qed.

Theorem semantic_successor_rejects_retained_exact_capability :
  forall s revision captured_revision captured_generation,
    exact_commit_authorized
      (semantic_successor s revision)
      (captured_revision, captured_generation) = false.
Proof.
  intros s revision captured_revision captured_generation.
  unfold exact_commit_authorized.
  simpl.
  repeat rewrite andb_false_r.
  reflexivity.
Qed.

Theorem retired_coordinator_rejects_every_exact_capability :
  forall s revision generation,
    exact_commit_authorized
      (retire_coordinator s) (revision, generation) = false.
Proof.
  intros s revision generation.
  unfold exact_commit_authorized.
  simpl.
  repeat rewrite andb_false_r.
  reflexivity.
Qed.

Theorem semantic_advance_invalidates_checkpoint_capture :
  forall s successor_revision captured_revision captured_generation,
    successor_revision <> captured_revision ->
    captured_root_matches
      (semantic_successor s successor_revision)
      (captured_revision, captured_generation) = false.
Proof.
  intros s successor_revision captured_revision captured_generation _Hneq.
  unfold captured_root_matches.
  simpl.
  rewrite andb_false_r.
  reflexivity.
Qed.

Definition generation_is_fresh (retained : list nat) (candidate : nat) : Prop :=
  ~ In candidate retained.

Theorem fresh_generation_differs_from_every_retained_generation :
  forall retained candidate old,
    generation_is_fresh retained candidate ->
    In old retained ->
    candidate <> old.
Proof.
  intros retained candidate old Hfresh Hold Heq.
  subst old.
  apply Hfresh.
  exact Hold.
Qed.

Inductive step : state -> state -> Prop :=
| StepSemantic : forall s revision,
    step s (semantic_successor s revision)
| StepCheckpoint : forall s generation,
    coordinator_retired s = false ->
    step s (exact_checkpoint_success s generation)
| StepRetire : forall s,
    step s (retire_coordinator s)
| StepFailedCheckpoint : forall s,
    step s (failed_checkpoint_publication s)
| StepExactCommit : forall s capability,
    exact_commit_authorized s capability = true ->
    step s s.

Inductive steps : state -> state -> Prop :=
| StepsRefl : forall s, steps s s
| StepsCons : forall before middle after,
    step before middle ->
    steps middle after ->
    steps before after.

Theorem step_preserves_exact_binding_safe :
  forall before after,
    exact_binding_safe before ->
    step before after ->
    exact_binding_safe after.
Proof.
  intros before after Hsafe Hstep.
  destruct Hstep.
  - apply semantic_successor_is_safe.
  - apply exact_checkpoint_success_is_safe.
  - destruct (retirement_fences_exact_authority s) as [_ [_ H]].
    exact H.
  - exact Hsafe.
  - exact Hsafe.
Qed.

Theorem steps_preserve_exact_binding_safe :
  forall before after,
    exact_binding_safe before ->
    steps before after ->
    exact_binding_safe after.
Proof.
  intros before after Hsafe Hsteps.
  induction Hsteps.
  - exact Hsafe.
  - apply IHHsteps.
    eapply step_preserves_exact_binding_safe; eauto.
Qed.

Theorem every_reachable_state_is_safe :
  forall after,
    steps initial_state after ->
    exact_binding_safe after.
Proof.
  intros after Hsteps.
  eapply steps_preserve_exact_binding_safe.
  - apply initial_state_safe.
  - exact Hsteps.
Qed.

End EvictionExactRootPublicationSpec.
