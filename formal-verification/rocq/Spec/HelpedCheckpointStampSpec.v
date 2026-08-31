(** * Root-published, idempotently helped checkpoint stamps

    A checkpoint prepares durable storage and all stamp records before its
    exact root CAS.  The CAS winner publishes a pending catalog.  Stamp stores
    are idempotent, and activation is permitted only after every required stamp
    is present.  A root-CAS loser returns its original stamp state unchanged,
    so storage that may be abandoned is never exposed through a node stamp.
*)

From Stdlib Require Import Arith Bool List Logic.FunctionalExtensionality.
Import ListNotations.

Module HelpedCheckpointStampSpec.

Definition stamps := nat -> bool.

Definition store_stamp (state : stamps) (stamp : nat) : stamps :=
  fun observed => if Nat.eq_dec observed stamp then true else state observed.

Theorem duplicate_stamp_store_idempotent :
  forall state stamp,
    store_stamp (store_stamp state stamp) stamp = store_stamp state stamp.
Proof.
  intros state stamp. apply functional_extensionality. intro observed.
  unfold store_stamp.
  destruct (Nat.eq_dec observed stamp); reflexivity.
Qed.

Definition all_stamps_applied
    (required : list nat) (state : stamps) : Prop :=
  forall stamp, In stamp required -> state stamp = true.

Definition activation_allowed
    (required : list nat) (state : stamps) : Prop :=
  all_stamps_applied required state.

Theorem activation_requires_every_stamp :
  forall required state,
    activation_allowed required state ->
    forall stamp, In stamp required -> state stamp = true.
Proof.
  intros required state Hallowed stamp Hin.
  exact (Hallowed stamp Hin).
Qed.

Record checkpoint_root : Type := mkCheckpointRoot {
  checkpoint_revision : nat;
  checkpoint_catalog : option nat
}.

Definition checkpoint_root_cas
    (expected_revision : nat) (candidate observed : checkpoint_root)
    : checkpoint_root :=
  if Nat.eq_dec (checkpoint_revision observed) expected_revision
  then candidate
  else observed.

Record checkpoint_attempt : Type := mkCheckpointAttempt {
  attempt_root : checkpoint_root;
  attempt_stamps : stamps
}.

Definition publish_pending_checkpoint
    (expected_revision : nat)
    (candidate observed : checkpoint_root)
    (prior_stamps : stamps) : checkpoint_attempt :=
  if Nat.eq_dec (checkpoint_revision observed) expected_revision
  then mkCheckpointAttempt candidate prior_stamps
  else mkCheckpointAttempt observed prior_stamps.

Theorem losing_checkpoint_cas_preserves_all_stamps :
  forall expected candidate observed prior_stamps,
    checkpoint_revision observed <> expected ->
    attempt_stamps
      (publish_pending_checkpoint expected candidate observed prior_stamps) =
    prior_stamps.
Proof.
  intros expected candidate observed prior_stamps Hmiss.
  unfold publish_pending_checkpoint.
  destruct (Nat.eq_dec (checkpoint_revision observed) expected);
    [contradiction | reflexivity].
Qed.

Theorem losing_checkpoint_cas_preserves_root :
  forall expected candidate observed prior_stamps,
    checkpoint_revision observed <> expected ->
    attempt_root
      (publish_pending_checkpoint expected candidate observed prior_stamps) =
    observed.
Proof.
  intros expected candidate observed prior_stamps Hmiss.
  unfold publish_pending_checkpoint.
  destruct (Nat.eq_dec (checkpoint_revision observed) expected);
    [contradiction | reflexivity].
Qed.

End HelpedCheckpointStampSpec.
