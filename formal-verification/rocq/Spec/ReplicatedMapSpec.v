Require Import Coq.Lists.List.

Require Import ARTrie.Model.Bucket.
Require Import ARTrie.Model.HotStuff.
Require Import ARTrie.Model.Key.
Require Import ARTrie.Spec.ARTrieSpec.

Import ListNotations.

Inductive ReplicatedCommand : Type :=
  | CmdPut : Key -> Value -> ReplicatedCommand
  | CmdRemove : Key -> ReplicatedCommand.

Definition apply_command_entries
    (entries : list KeyValue) (cmd : ReplicatedCommand) : list KeyValue :=
  match cmd with
  | CmdPut k v => kv_upsert entries k v
  | CmdRemove k => kv_delete entries k
  end.

Fixpoint apply_log_entries
    (entries : list KeyValue) (log : list ReplicatedCommand) : list KeyValue :=
  match log with
  | [] => entries
  | cmd :: rest => apply_log_entries (apply_command_entries entries cmd) rest
  end.

Definition ReplicatedCertificate := QuorumCertificate ReplicatedCommand.

Definition replicated_logs_compatible
    (left right : ReplicatedCertificate) : Prop :=
  Compatible (qc_log ReplicatedCommand left) (qc_log ReplicatedCommand right).

Theorem apply_command_lookup_put :
  forall entries k v query,
    kv_lookup (apply_command_entries entries (CmdPut k v)) query =
    if key_eqb k query then Some v else kv_lookup entries query.
Proof.
  intros entries k v query.
  unfold apply_command_entries.
  destruct (key_eqb k query) eqn:Hsame.
  - apply key_eqb_eq in Hsame.
    subst query.
    apply kv_lookup_upsert_same.
  - apply kv_lookup_upsert_other.
    intro Heq.
    subst query.
    rewrite key_eqb_refl in Hsame.
    discriminate.
Qed.

Theorem apply_command_lookup_remove :
  forall entries k query,
    kv_lookup (apply_command_entries entries (CmdRemove k)) query =
    if key_eqb k query then None else kv_lookup entries query.
Proof.
  intros entries k query.
  unfold apply_command_entries.
  destruct (key_eqb k query) eqn:Hsame.
  - apply key_eqb_eq in Hsame.
    subst query.
    apply kv_lookup_delete_same.
  - apply kv_lookup_delete_other.
    intro Heq.
    subst query.
    rewrite key_eqb_refl in Hsame.
    discriminate.
Qed.

Theorem apply_log_entries_app :
  forall log_suffix log_prefix entries,
    apply_log_entries entries (log_prefix ++ log_suffix) =
    apply_log_entries (apply_log_entries entries log_prefix) log_suffix.
Proof.
  induction log_prefix as [| cmd rest IH]; intros entries; simpl.
  - reflexivity.
  - apply IH.
Qed.

Theorem prefix_replay_factorizes :
  forall prefix log entries,
    Prefix prefix log ->
    exists suffix,
      apply_log_entries entries log =
      apply_log_entries (apply_log_entries entries prefix) suffix.
Proof.
  intros prefix log entries [suffix Hprefix].
  exists suffix.
  rewrite <- Hprefix.
  apply apply_log_entries_app.
Qed.
