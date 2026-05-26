(** Public durability acknowledgement laws.

    The model captures the implementation contract shared by the byte, char,
    and vocabulary persistent tries:

    - Immediate and GroupCommit are full durability policies, so a successful
      public mutation acknowledgement implies the appended WAL LSN is covered
      by the synced frontier.
    - Periodic and None may acknowledge visible state without advancing the
      synced frontier.
    - blocking and completed async syncs cover their requested LSN frontier.
    - checkpoints and recovery never publish beyond the synced frontier.
 *)

Require Import Coq.Arith.PeanoNat.
Require Import Coq.Bool.Bool.
Require Import Coq.micromega.Lia.

Definition Lsn := nat.

Inductive Policy : Type :=
| Immediate
| GroupCommit
| Periodic
| NoDurability.

Definition full_policy (policy : Policy) : bool :=
  match policy with
  | Immediate => true
  | GroupCommit => true
  | Periodic => false
  | NoDurability => false
  end.

Record DurabilityState : Type := {
  next_lsn : Lsn;
  synced_lsn : Lsn;
  ack_lsn : option Lsn;
  ack_policy : option Policy;
  durability_policy : Policy;
  async_target : option Lsn;
  async_done : bool;
  checkpoint_lsn : Lsn
}.

Definition current_tail (state : DurabilityState) : Lsn :=
  Nat.pred (next_lsn state).

Definition append_and_ack (state : DurabilityState) : DurabilityState :=
  let lsn := next_lsn state in
  {|
    next_lsn := S lsn;
    synced_lsn :=
      if full_policy (durability_policy state)
      then Nat.max (synced_lsn state) lsn
      else synced_lsn state;
    ack_lsn := Some lsn;
    ack_policy := Some (durability_policy state);
    durability_policy := durability_policy state;
    async_target := async_target state;
    async_done := false;
    checkpoint_lsn := checkpoint_lsn state
  |}.

Definition blocking_sync (state : DurabilityState) : DurabilityState :=
  {|
    next_lsn := next_lsn state;
    synced_lsn := Nat.max (synced_lsn state) (current_tail state);
    ack_lsn := ack_lsn state;
    ack_policy := ack_policy state;
    durability_policy := durability_policy state;
    async_target := async_target state;
    async_done := true;
    checkpoint_lsn := checkpoint_lsn state
  |}.

Definition start_async_sync (state : DurabilityState) : DurabilityState :=
  {|
    next_lsn := next_lsn state;
    synced_lsn := synced_lsn state;
    ack_lsn := ack_lsn state;
    ack_policy := ack_policy state;
    durability_policy := durability_policy state;
    async_target := Some (current_tail state);
    async_done := false;
    checkpoint_lsn := checkpoint_lsn state
  |}.

Definition finish_async_sync (state : DurabilityState) : DurabilityState :=
  match async_target state with
  | Some target =>
      {|
        next_lsn := next_lsn state;
        synced_lsn := Nat.max (synced_lsn state) target;
        ack_lsn := ack_lsn state;
        ack_policy := ack_policy state;
        durability_policy := durability_policy state;
        async_target := async_target state;
        async_done := true;
        checkpoint_lsn := checkpoint_lsn state
      |}
  | None =>
      {|
        next_lsn := next_lsn state;
        synced_lsn := synced_lsn state;
        ack_lsn := ack_lsn state;
        ack_policy := ack_policy state;
        durability_policy := durability_policy state;
        async_target := async_target state;
        async_done := true;
        checkpoint_lsn := checkpoint_lsn state
      |}
  end.

Definition checkpoint (state : DurabilityState) : DurabilityState :=
  {|
    next_lsn := next_lsn state;
    synced_lsn := synced_lsn state;
    ack_lsn := ack_lsn state;
    ack_policy := ack_policy state;
    durability_policy := durability_policy state;
    async_target := async_target state;
    async_done := async_done state;
    checkpoint_lsn := synced_lsn state
  |}.

Definition recovered_lsn (state : DurabilityState) (lsn : Lsn) : bool :=
  (lsn <=? synced_lsn state) || (lsn <=? checkpoint_lsn state).

Definition full_policy_ack_durable (state : DurabilityState) : Prop :=
  match ack_lsn state, ack_policy state with
  | Some lsn, Some policy =>
      full_policy policy = true -> lsn <= synced_lsn state
  | _, _ => True
  end.

Theorem full_policy_ack_durable_after_append :
  forall state,
    full_policy_ack_durable (append_and_ack state).
Proof.
  intros state.
  unfold full_policy_ack_durable, append_and_ack.
  destruct (durability_policy state); simpl; intros; lia.
Qed.

Theorem immediate_ack_is_synced :
  forall state,
    durability_policy state = Immediate ->
    next_lsn state <= synced_lsn (append_and_ack state).
Proof.
  intros state Hpolicy.
  unfold append_and_ack.
  rewrite Hpolicy.
  simpl.
  lia.
Qed.

Theorem group_commit_ack_is_synced :
  forall state,
    durability_policy state = GroupCommit ->
    next_lsn state <= synced_lsn (append_and_ack state).
Proof.
  intros state Hpolicy.
  unfold append_and_ack.
  rewrite Hpolicy.
  simpl.
  lia.
Qed.

Theorem periodic_ack_does_not_force_sync :
  forall state,
    durability_policy state = Periodic ->
    synced_lsn (append_and_ack state) = synced_lsn state /\
    ack_lsn (append_and_ack state) = Some (next_lsn state).
Proof.
  intros state Hpolicy.
  unfold append_and_ack.
  rewrite Hpolicy.
  simpl.
  split; reflexivity.
Qed.

Theorem no_durability_ack_does_not_force_sync :
  forall state,
    durability_policy state = NoDurability ->
    synced_lsn (append_and_ack state) = synced_lsn state /\
    ack_lsn (append_and_ack state) = Some (next_lsn state).
Proof.
  intros state Hpolicy.
  unfold append_and_ack.
  rewrite Hpolicy.
  simpl.
  split; reflexivity.
Qed.

Theorem blocking_sync_covers_current_tail :
  forall state,
    current_tail state <= synced_lsn (blocking_sync state).
Proof.
  intros state.
  unfold blocking_sync.
  simpl.
  lia.
Qed.

Theorem async_start_targets_current_tail :
  forall state,
    async_target (start_async_sync state) = Some (current_tail state) /\
    async_done (start_async_sync state) = false.
Proof.
  intros state.
  unfold start_async_sync.
  simpl.
  split; reflexivity.
Qed.

Theorem async_completion_implies_target_synced :
  forall state target,
    async_target state = Some target ->
    target <= synced_lsn (finish_async_sync state) /\
    async_done (finish_async_sync state) = true.
Proof.
  intros state target Htarget.
  unfold finish_async_sync.
  rewrite Htarget.
  simpl.
  split; lia.
Qed.

Theorem checkpoint_within_synced_frontier :
  forall state,
    checkpoint_lsn (checkpoint state) <= synced_lsn (checkpoint state).
Proof.
  intros state.
  unfold checkpoint.
  simpl.
  lia.
Qed.

Theorem recovered_lsn_is_synced_or_checkpointed :
  forall state lsn,
    recovered_lsn state lsn = true ->
    lsn <= synced_lsn state \/ lsn <= checkpoint_lsn state.
Proof.
  intros state lsn Hrecovered.
  unfold recovered_lsn in Hrecovered.
  apply Bool.orb_true_iff in Hrecovered.
  destruct Hrecovered as [Hsynced | Hcheckpoint].
  - left. apply Nat.leb_le. exact Hsynced.
  - right. apply Nat.leb_le. exact Hcheckpoint.
Qed.

Theorem checkpoint_recovery_is_within_synced_frontier :
  forall state lsn,
    checkpoint_lsn state <= synced_lsn state ->
    recovered_lsn state lsn = true ->
    lsn <= synced_lsn state.
Proof.
  intros state lsn Hcheckpoint Hrecovered.
  apply recovered_lsn_is_synced_or_checkpointed in Hrecovered.
  destruct Hrecovered; lia.
Qed.
