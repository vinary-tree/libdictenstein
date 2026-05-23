Require Import Coq.Arith.Arith.
Require Import Coq.Lists.List.
Require Import Coq.micromega.Lia.

Import ListNotations.

Definition Replica := nat.

Definition Prefix {A : Type} (xs ys : list A) : Prop :=
  exists suffix, xs ++ suffix = ys.

Definition Compatible {A : Type} (xs ys : list A) : Prop :=
  Prefix xs ys \/ Prefix ys xs.

Lemma prefix_refl : forall (A : Type) (xs : list A), Prefix xs xs.
Proof.
  intros A xs. exists []. rewrite app_nil_r. reflexivity.
Qed.

Lemma compatible_refl : forall (A : Type) (xs : list A), Compatible xs xs.
Proof.
  intros A xs. left. apply prefix_refl.
Qed.

Lemma compatible_sym :
  forall (A : Type) (xs ys : list A), Compatible xs ys -> Compatible ys xs.
Proof.
  intros A xs ys H.
  destruct H as [H | H]; [right | left]; exact H.
Qed.

Record QuorumCertificate (Command : Type) := mkQC {
  qc_log : list Command;
  qc_voters : list Replica
}.

Definition replica_count (faults : nat) : nat := 3 * faults + 1.

Definition quorum_size (faults : nat) : nat := 2 * faults + 1.

Definition quorum
    (universe : list Replica) (faults : nat) (voters : list Replica) : Prop :=
  NoDup voters /\
  incl voters universe /\
  quorum_size faults <= length voters.

Definition honest_intersection {Command : Type}
    (certs : list (QuorumCertificate Command))
    (honest : list Replica) : Prop :=
  forall c1 c2,
    In c1 certs ->
    In c2 certs ->
    exists r,
      In r (qc_voters Command c1) /\
      In r (qc_voters Command c2) /\
      In r honest.

Definition honest_vote_lock {Command : Type}
    (certs : list (QuorumCertificate Command))
    (honest : list Replica) : Prop :=
  forall c1 c2 r,
    In c1 certs ->
    In c2 certs ->
    In r (qc_voters Command c1) ->
    In r (qc_voters Command c2) ->
    In r honest ->
    Compatible (qc_log Command c1) (qc_log Command c2).

Record HotStuffSafetyContext (Command : Type) := mkHotStuffSafetyContext {
  hs_certificates : list (QuorumCertificate Command);
  hs_honest : list Replica;
  hs_intersection :
    honest_intersection hs_certificates hs_honest;
  hs_honest_lock :
    honest_vote_lock hs_certificates hs_honest
}.

Theorem hotstuff_committed_logs_compatible :
  forall (Command : Type) (ctx : HotStuffSafetyContext Command)
         (c1 c2 : QuorumCertificate Command),
    In c1 (hs_certificates Command ctx) ->
    In c2 (hs_certificates Command ctx) ->
    Compatible (qc_log Command c1) (qc_log Command c2).
Proof.
  intros Command ctx c1 c2 Hin1 Hin2.
  destruct ctx as [certs honest Hintersection Hlock].
  simpl in *.
  destruct (Hintersection c1 c2 Hin1 Hin2) as
      [r [Hvoter1 [Hvoter2 Hhonest]]].
  eapply Hlock; eauto.
Qed.

Lemma quorum_overlap_numeric :
  forall faults, replica_count faults < quorum_size faults + quorum_size faults.
Proof.
  intros faults.
  unfold replica_count, quorum_size.
  lia.
Qed.

Theorem quorum_sets_cannot_be_disjoint :
  forall faults universe q1 q2,
    length universe = replica_count faults ->
    NoDup universe ->
    quorum universe faults q1 ->
    quorum universe faults q2 ->
    (forall r, In r q1 -> ~ In r q2) ->
    False.
Proof.
  intros faults universe q1 q2 Huniverse_len Huniverse_nodup Hq1 Hq2 Hdisjoint.
  destruct Hq1 as [Hq1_nodup [Hq1_incl Hq1_len]].
  destruct Hq2 as [Hq2_nodup [Hq2_incl Hq2_len]].
  assert (NoDup (q1 ++ q2)) as Happend_nodup.
  {
    apply NoDup_app; repeat split; auto.
  }
  assert (incl (q1 ++ q2) universe) as Happend_incl.
  {
    intros r Hr.
    apply in_app_or in Hr.
    destruct Hr as [Hr | Hr]; [apply Hq1_incl | apply Hq2_incl]; exact Hr.
  }
  pose proof (NoDup_incl_length Happend_nodup Happend_incl) as Happend_len.
  rewrite length_app in Happend_len.
  pose proof (quorum_overlap_numeric faults) as Hoverlap.
  lia.
Qed.

Theorem quorum_intersection_not_disjoint :
  forall faults universe q1 q2,
    length universe = replica_count faults ->
    NoDup universe ->
    quorum universe faults q1 ->
    quorum universe faults q2 ->
    ~ (forall r, In r q1 -> ~ In r q2).
Proof.
  intros faults universe q1 q2 Huniverse_len Huniverse_nodup Hq1 Hq2 Hdisjoint.
  exact
    (quorum_sets_cannot_be_disjoint
      faults universe q1 q2 Huniverse_len Huniverse_nodup Hq1 Hq2 Hdisjoint).
Qed.
