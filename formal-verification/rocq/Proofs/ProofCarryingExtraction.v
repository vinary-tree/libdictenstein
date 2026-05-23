Require Import Coq.Lists.List.

Require Import ARTrie.Model.Bucket.
Require Import ARTrie.Model.Key.
Require Import ARTrie.Spec.ARTrieSpec.
Require Import ARTrie.Spec.ReplicatedMapSpec.

Import ListNotations.

Record CertifiedTraceStep := mkCertifiedTraceStep {
  step_before : list KeyValue;
  step_command : ReplicatedCommand;
  step_after : list KeyValue
}.

Definition step_certificate_valid (step : CertifiedTraceStep) : Prop :=
  step_after step =
  apply_command_entries (step_before step) (step_command step).

Fixpoint trace_commands (steps : list CertifiedTraceStep) : list ReplicatedCommand :=
  match steps with
  | [] => []
  | step :: rest => step_command step :: trace_commands rest
  end.

Fixpoint trace_final
    (initial : list KeyValue) (steps : list CertifiedTraceStep) : list KeyValue :=
  match steps with
  | [] => initial
  | step :: rest => trace_final (step_after step) rest
  end.

Fixpoint certified_trace_valid
    (current : list KeyValue) (steps : list CertifiedTraceStep) : Prop :=
  match steps with
  | [] => True
  | step :: rest =>
      step_before step = current /\
      step_certificate_valid step /\
      certified_trace_valid (step_after step) rest
  end.

Theorem certified_trace_replays_reference :
  forall steps initial,
    certified_trace_valid initial steps ->
    trace_final initial steps =
    apply_log_entries initial (trace_commands steps).
Proof.
  induction steps as [| step rest IH]; intros initial Hvalid; simpl in *.
  - reflexivity.
  - destruct Hvalid as [Hbefore [Hstep Hrest]].
    unfold step_certificate_valid in Hstep.
    rewrite <- Hbefore.
    rewrite <- Hstep.
    apply IH.
    exact Hrest.
Qed.

Theorem certified_trace_lookup_refines_reference :
  forall steps initial key,
    certified_trace_valid initial steps ->
    kv_lookup (trace_final initial steps) key =
    kv_lookup (apply_log_entries initial (trace_commands steps)) key.
Proof.
  intros steps initial key Hvalid.
  rewrite certified_trace_replays_reference; auto.
Qed.

Theorem invalid_step_rejected :
  forall before command after,
    after <> apply_command_entries before command ->
    ~ step_certificate_valid
        (mkCertifiedTraceStep before command after).
Proof.
  intros before command after Hneq Hvalid.
  unfold step_certificate_valid in Hvalid.
  simpl in Hvalid.
  apply Hneq.
  exact Hvalid.
Qed.
