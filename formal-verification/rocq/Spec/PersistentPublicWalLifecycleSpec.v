(** Persistent public WAL lifecycle laws.

    This specification links the public persistence API to the WAL recovery
    abstraction used by the byte, char, and vocabulary persistent tries:

    - public open is recovery from the persisted checkpoint plus retained WAL;
    - retained WAL tails contain only entries after the checkpoint frontier;
    - durable prefixes contain only entries at or below the synced frontier;
    - group-commit writes must use exactly the next expected LSN;
    - replaying a committed prefix and then the remaining queue is equivalent
      to replaying the whole WAL queue.
 *)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Import ListNotations.

Definition Key := nat.
Definition Value := nat.
Definition Lsn := nat.
Definition RefMap := Key -> option Value.

Definition empty_map : RefMap := fun _ => None.

Definition lookup (map : RefMap) (key : Key) : option Value :=
  map key.

Definition map_put (map : RefMap) (key : Key) (value : Value) : RefMap :=
  fun query => if Nat.eq_dec query key then Some value else map query.

Definition map_remove (map : RefMap) (key : Key) : RefMap :=
  fun query => if Nat.eq_dec query key then None else map query.

Theorem map_put_lookup_same :
  forall map key value,
    lookup (map_put map key value) key = Some value.
Proof.
  intros map key value.
  unfold lookup, map_put.
  destruct (Nat.eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - contradiction Hneq. reflexivity.
Qed.

Theorem map_remove_lookup_same :
  forall map key,
    lookup (map_remove map key) key = None.
Proof.
  intros map key.
  unfold lookup, map_remove.
  destruct (Nat.eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - contradiction Hneq. reflexivity.
Qed.

Inductive WalOp : Type :=
| WalPut : Key -> Value -> WalOp
| WalDelete : Key -> WalOp
| WalCheckpoint : Lsn -> WalOp.

Record WalEntry : Type := mkWalEntry {
  entry_lsn : Lsn;
  entry_op : WalOp
}.

Definition apply_wal_entry (map : RefMap) (entry : WalEntry) : RefMap :=
  match entry_op entry with
  | WalPut key value => map_put map key value
  | WalDelete key => map_remove map key
  | WalCheckpoint _ => map
  end.

Fixpoint replay_entries (entries : list WalEntry) (map : RefMap) : RefMap :=
  match entries with
  | [] => map
  | entry :: rest => replay_entries rest (apply_wal_entry map entry)
  end.

Theorem replay_entries_app :
  forall left right map,
    replay_entries (left ++ right) map =
    replay_entries right (replay_entries left map).
Proof.
  induction left as [| entry rest IH]; intros right map.
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Definition retained_tail (checkpoint_lsn : Lsn) (entries : list WalEntry)
  : list WalEntry :=
  filter (fun entry => checkpoint_lsn <? entry_lsn entry) entries.

Definition durable_prefix (synced_lsn : Lsn) (entries : list WalEntry)
  : list WalEntry :=
  filter (fun entry => entry_lsn entry <=? synced_lsn) entries.

Definition recover (checkpoint : RefMap) (retained : list WalEntry) : RefMap :=
  replay_entries retained checkpoint.

Definition public_open (checkpoint : RefMap) (retained : list WalEntry) : RefMap :=
  recover checkpoint retained.

Theorem public_open_recovery_equivalent :
  forall checkpoint retained,
    public_open checkpoint retained = recover checkpoint retained.
Proof.
  reflexivity.
Qed.

Theorem retained_tail_entries_are_after_checkpoint :
  forall checkpoint_lsn entries entry,
    In entry (retained_tail checkpoint_lsn entries) ->
    checkpoint_lsn < entry_lsn entry.
Proof.
  intros checkpoint_lsn entries entry Hin.
  unfold retained_tail in Hin.
  apply filter_In in Hin.
  destruct Hin as [_ Hafter].
  apply Nat.ltb_lt. exact Hafter.
Qed.

Theorem durable_prefix_entries_are_synced :
  forall synced_lsn entries entry,
    In entry (durable_prefix synced_lsn entries) ->
    entry_lsn entry <= synced_lsn.
Proof.
  intros synced_lsn entries entry Hin.
  unfold durable_prefix in Hin.
  apply filter_In in Hin.
  destruct Hin as [_ Hsynced].
  apply Nat.leb_le. exact Hsynced.
Qed.

Definition write_reserved (expected_lsn : Lsn) (entry : WalEntry)
  : option Lsn :=
  if entry_lsn entry =? expected_lsn then Some (S expected_lsn) else None.

Theorem reserved_write_accepts_exact_next_lsn :
  forall expected op,
    write_reserved expected (mkWalEntry expected op) = Some (S expected).
Proof.
  intros expected op.
  unfold write_reserved.
  rewrite Nat.eqb_refl.
  reflexivity.
Qed.

Theorem reserved_write_rejects_lsn_gap :
  forall expected entry,
    entry_lsn entry <> expected ->
    write_reserved expected entry = None.
Proof.
  intros expected entry Hneq.
  unfold write_reserved.
  assert (Hcmp : (entry_lsn entry =? expected) = false).
  { apply Nat.eqb_neq. exact Hneq. }
  rewrite Hcmp.
  reflexivity.
Qed.

Fixpoint contiguous_from (next_lsn : Lsn) (entries : list WalEntry) : Prop :=
  match entries with
  | [] => True
  | entry :: rest =>
      entry_lsn entry = next_lsn /\ contiguous_from (S next_lsn) rest
  end.

Definition head_matches_next (next_lsn : Lsn) (entries : list WalEntry) : Prop :=
  match entries with
  | [] => True
  | entry :: _ => entry_lsn entry = next_lsn
  end.

Theorem contiguous_queue_head_matches_next :
  forall next_lsn entries,
    contiguous_from next_lsn entries ->
    head_matches_next next_lsn entries.
Proof.
  intros next_lsn entries Hcontiguous.
  destruct entries as [| entry rest].
  - exact I.
  - simpl in *. destruct Hcontiguous as [Hhead _]. exact Hhead.
Qed.

Definition commit_prefix (count : nat) (entries : list WalEntry)
  : list WalEntry :=
  firstn count entries.

Definition remaining_after_commit (count : nat) (entries : list WalEntry)
  : list WalEntry :=
  skipn count entries.

Theorem commit_prefix_then_remaining_equals_full_replay :
  forall count entries map,
    replay_entries entries map =
    replay_entries
      (remaining_after_commit count entries)
      (replay_entries (commit_prefix count entries) map).
Proof.
  intros count entries map.
  unfold commit_prefix, remaining_after_commit.
  rewrite <- (firstn_skipn count entries) at 1.
  apply replay_entries_app.
Qed.

Definition ack_is_durable (synced_lsn ack_lsn : Lsn) : Prop :=
  ack_lsn <= synced_lsn.

Theorem acknowledged_public_lsn_is_within_synced_prefix :
  forall synced_lsn ack_lsn,
    ack_is_durable synced_lsn ack_lsn ->
    ack_lsn <= synced_lsn.
Proof.
  intros synced_lsn ack_lsn Hack.
  exact Hack.
Qed.

Theorem retained_tail_put_visible_after_checkpoint :
  forall checkpoint checkpoint_lsn lsn key value,
    checkpoint_lsn < lsn ->
    lookup
      (public_open
        checkpoint
        (retained_tail checkpoint_lsn [mkWalEntry lsn (WalPut key value)]))
      key = Some value.
Proof.
  intros checkpoint checkpoint_lsn lsn key value Hafter.
  unfold public_open, recover, retained_tail.
  simpl.
  assert (Hcmp : (checkpoint_lsn <? lsn) = true).
  { apply Nat.ltb_lt. exact Hafter. }
  rewrite Hcmp.
  simpl.
  apply map_put_lookup_same.
Qed.

Theorem checkpoint_record_does_not_change_public_open :
  forall checkpoint lsn,
    public_open checkpoint [mkWalEntry lsn (WalCheckpoint lsn)] = checkpoint.
Proof.
  reflexivity.
Qed.
