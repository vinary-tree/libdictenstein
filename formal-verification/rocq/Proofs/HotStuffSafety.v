Require Import Coq.Lists.List.

Require Import ARTrie.Model.HotStuff.
Require Import ARTrie.Spec.ARTrieSpec.
Require Import ARTrie.Spec.ReplicatedMapSpec.

Import ListNotations.

Theorem replicated_hotstuff_committed_logs_compatible :
  forall (ctx : HotStuffSafetyContext ReplicatedCommand)
         (c1 c2 : ReplicatedCertificate),
    In c1 (hs_certificates ReplicatedCommand ctx) ->
    In c2 (hs_certificates ReplicatedCommand ctx) ->
    replicated_logs_compatible c1 c2.
Proof.
  intros ctx c1 c2 Hin1 Hin2.
  unfold replicated_logs_compatible.
  eapply hotstuff_committed_logs_compatible; eauto.
Qed.

Theorem replicated_hotstuff_committed_replays_share_prefix :
  forall (ctx : HotStuffSafetyContext ReplicatedCommand)
         (c1 c2 : ReplicatedCertificate)
         (entries : list KeyValue),
    In c1 (hs_certificates ReplicatedCommand ctx) ->
    In c2 (hs_certificates ReplicatedCommand ctx) ->
    (exists suffix,
        apply_log_entries entries (qc_log ReplicatedCommand c2) =
        apply_log_entries
          (apply_log_entries entries (qc_log ReplicatedCommand c1))
          suffix) \/
    (exists suffix,
        apply_log_entries entries (qc_log ReplicatedCommand c1) =
        apply_log_entries
          (apply_log_entries entries (qc_log ReplicatedCommand c2))
          suffix).
Proof.
  intros ctx c1 c2 entries Hin1 Hin2.
  pose proof
    (replicated_hotstuff_committed_logs_compatible ctx c1 c2 Hin1 Hin2)
    as Hcompatible.
  unfold replicated_logs_compatible in Hcompatible.
  destruct Hcompatible as [Hprefix | Hprefix].
  - left. eapply prefix_replay_factorizes. exact Hprefix.
  - right. eapply prefix_replay_factorizes. exact Hprefix.
Qed.
