(** Shared persistent public-API concurrency laws.

    This small model captures the Arc/RwLock-backed public API boundary used by
    the persistent byte, char, and vocab tries:

    - public writes hold the exclusive write lock until WAL-before-visible
      publication is complete;
    - public checkpoint holds the same exclusive lock across data snapshot,
      checkpoint WAL publication, and WAL truncation;
    - sync is not checkpoint publication;
    - failed writes/checkpoints leave replay evidence intact;
    - crash recovery uses either the durable checkpoint image or a retained WAL
      tail for visible operations.
 *)

Require Import Coq.Arith.PeanoNat.
Require Import Coq.Bool.Bool.

Definition Term := nat.
Definition Value := nat.
Definition Lsn := nat.
Definition StateMap := Term -> option Value.
Definition LsnMap := Term -> option Lsn.

Definition empty_state_map : StateMap := fun _ => None.
Definition empty_lsn_map : LsnMap := fun _ => None.

Definition map_put {A : Type} (map : Term -> option A) (key : Term) (value : A)
  : Term -> option A :=
  fun query => if Nat.eqb query key then Some value else map query.

Inductive LockState : Type :=
| LockFree
| LockWriter
| LockCheckpoint.

Record SharedState : Type := {
  shared_visible : StateMap;
  shared_term_lsn : LsnMap;
  shared_durable : StateMap;
  shared_wal_retained_from : Lsn;
  shared_synced_lsn : Lsn;
  shared_checkpoint_lsn : Lsn;
  shared_lock : LockState;
  shared_dirty : bool
}.

Definition can_start_write (state : SharedState) : bool :=
  match shared_lock state with
  | LockFree => true
  | LockWriter => false
  | LockCheckpoint => false
  end.

Definition begin_write (state : SharedState) : SharedState :=
  {|
    shared_visible := shared_visible state;
    shared_term_lsn := shared_term_lsn state;
    shared_durable := shared_durable state;
    shared_wal_retained_from := shared_wal_retained_from state;
    shared_synced_lsn := shared_synced_lsn state;
    shared_checkpoint_lsn := shared_checkpoint_lsn state;
    shared_lock := LockWriter;
    shared_dirty := shared_dirty state
  |}.

Definition finish_write_success
  (state : SharedState)
  (term : Term)
  (value : Value)
  (lsn : Lsn)
  : SharedState :=
  {|
    shared_visible := map_put (shared_visible state) term value;
    shared_term_lsn := map_put (shared_term_lsn state) term lsn;
    shared_durable := shared_durable state;
    shared_wal_retained_from := shared_wal_retained_from state;
    shared_synced_lsn := Nat.max (shared_synced_lsn state) lsn;
    shared_checkpoint_lsn := shared_checkpoint_lsn state;
    shared_lock := LockFree;
    shared_dirty := true
  |}.

Definition finish_write_failure (state : SharedState) : SharedState :=
  {|
    shared_visible := shared_visible state;
    shared_term_lsn := shared_term_lsn state;
    shared_durable := shared_durable state;
    shared_wal_retained_from := shared_wal_retained_from state;
    shared_synced_lsn := shared_synced_lsn state;
    shared_checkpoint_lsn := shared_checkpoint_lsn state;
    shared_lock := LockFree;
    shared_dirty := shared_dirty state
  |}.

Definition begin_checkpoint (state : SharedState) : SharedState :=
  {|
    shared_visible := shared_visible state;
    shared_term_lsn := shared_term_lsn state;
    shared_durable := shared_durable state;
    shared_wal_retained_from := shared_wal_retained_from state;
    shared_synced_lsn := shared_synced_lsn state;
    shared_checkpoint_lsn := shared_checkpoint_lsn state;
    shared_lock := LockCheckpoint;
    shared_dirty := shared_dirty state
  |}.

Definition finish_checkpoint_success
  (state : SharedState)
  (snapshot_lsn : Lsn)
  : SharedState :=
  {|
    shared_visible := shared_visible state;
    shared_term_lsn := shared_term_lsn state;
    shared_durable := shared_visible state;
    shared_wal_retained_from := S snapshot_lsn;
    shared_synced_lsn := Nat.max (shared_synced_lsn state) snapshot_lsn;
    shared_checkpoint_lsn := Nat.max (shared_checkpoint_lsn state) snapshot_lsn;
    shared_lock := LockFree;
    shared_dirty := false
  |}.

Definition finish_checkpoint_failure (state : SharedState) : SharedState :=
  {|
    shared_visible := shared_visible state;
    shared_term_lsn := shared_term_lsn state;
    shared_durable := shared_durable state;
    shared_wal_retained_from := shared_wal_retained_from state;
    shared_synced_lsn := shared_synced_lsn state;
    shared_checkpoint_lsn := shared_checkpoint_lsn state;
    shared_lock := LockFree;
    shared_dirty := shared_dirty state
  |}.

Definition sync_only (state : SharedState) : SharedState := state.

Definition read_value (state : SharedState) (term : Term) : option Value :=
  shared_visible state term.

Definition recover_value (state : SharedState) (term : Term) : option Value :=
  match shared_durable state term with
  | Some value => Some value
  | None =>
      match shared_term_lsn state term with
      | Some lsn =>
          if (shared_wal_retained_from state <=? lsn) &&
             (lsn <=? shared_synced_lsn state)
          then shared_visible state term
          else None
      | None => None
      end
  end.

Theorem map_put_eq :
  forall (A : Type) (map : Term -> option A) key value,
    map_put map key value key = Some value.
Proof.
  intros A map key value.
  unfold map_put.
  rewrite Nat.eqb_refl.
  reflexivity.
Qed.

Theorem map_put_neq :
  forall (A : Type) (map : Term -> option A) key other value,
    Nat.eqb other key = false ->
    map_put map key value other = map other.
Proof.
  intros A map key other value Hneq.
  unfold map_put.
  rewrite Hneq.
  reflexivity.
Qed.

Theorem checkpoint_lock_blocks_write :
  forall state,
    can_start_write (begin_checkpoint state) = false.
Proof.
  intros state.
  unfold can_start_write, begin_checkpoint.
  reflexivity.
Qed.

Theorem writer_lock_blocks_second_write :
  forall state,
    can_start_write (begin_write state) = false.
Proof.
  intros state.
  unfold can_start_write, begin_write.
  reflexivity.
Qed.

Theorem begin_checkpoint_preserves_visible :
  forall state term,
    shared_visible (begin_checkpoint state) term = shared_visible state term.
Proof.
  intros state term.
  unfold begin_checkpoint.
  reflexivity.
Qed.

Theorem finish_write_success_sets_visible :
  forall state term value lsn,
    shared_visible (finish_write_success state term value lsn) term = Some value.
Proof.
  intros state term value lsn.
  unfold finish_write_success.
  apply map_put_eq.
Qed.

Theorem finish_write_success_sets_term_lsn :
  forall state term value lsn,
    shared_term_lsn (finish_write_success state term value lsn) term = Some lsn.
Proof.
  intros state term value lsn.
  unfold finish_write_success.
  apply map_put_eq.
Qed.

Theorem finish_write_success_releases_lock :
  forall state term value lsn,
    shared_lock (finish_write_success state term value lsn) = LockFree.
Proof.
  intros state term value lsn.
  unfold finish_write_success.
  reflexivity.
Qed.

Theorem finish_write_success_marks_dirty :
  forall state term value lsn,
    shared_dirty (finish_write_success state term value lsn) = true.
Proof.
  intros state term value lsn.
  unfold finish_write_success.
  reflexivity.
Qed.

Theorem finish_write_failure_preserves_visible :
  forall state term,
    shared_visible (finish_write_failure state) term = shared_visible state term.
Proof.
  intros state term.
  unfold finish_write_failure.
  reflexivity.
Qed.

Theorem finish_write_failure_releases_lock :
  forall state,
    shared_lock (finish_write_failure state) = LockFree.
Proof.
  intros state.
  unfold finish_write_failure.
  reflexivity.
Qed.

Theorem finish_checkpoint_success_publishes_visible_snapshot :
  forall state snapshot_lsn term,
    shared_durable (finish_checkpoint_success state snapshot_lsn) term =
    shared_visible state term.
Proof.
  intros state snapshot_lsn term.
  unfold finish_checkpoint_success.
  reflexivity.
Qed.

Theorem finish_checkpoint_success_truncates_after_snapshot :
  forall state snapshot_lsn,
    shared_wal_retained_from (finish_checkpoint_success state snapshot_lsn) =
    S snapshot_lsn.
Proof.
  intros state snapshot_lsn.
  unfold finish_checkpoint_success.
  reflexivity.
Qed.

Theorem finish_checkpoint_success_clears_dirty :
  forall state snapshot_lsn,
    shared_dirty (finish_checkpoint_success state snapshot_lsn) = false.
Proof.
  intros state snapshot_lsn.
  unfold finish_checkpoint_success.
  reflexivity.
Qed.

Theorem finish_checkpoint_success_releases_lock :
  forall state snapshot_lsn,
    shared_lock (finish_checkpoint_success state snapshot_lsn) = LockFree.
Proof.
  intros state snapshot_lsn.
  unfold finish_checkpoint_success.
  reflexivity.
Qed.

Theorem finish_checkpoint_failure_preserves_visible :
  forall state term,
    shared_visible (finish_checkpoint_failure state) term =
    shared_visible state term.
Proof.
  intros state term.
  unfold finish_checkpoint_failure.
  reflexivity.
Qed.

Theorem finish_checkpoint_failure_preserves_wal_retention :
  forall state,
    shared_wal_retained_from (finish_checkpoint_failure state) =
    shared_wal_retained_from state.
Proof.
  intros state.
  unfold finish_checkpoint_failure.
  reflexivity.
Qed.

Theorem sync_only_does_not_publish_checkpoint :
  forall state term,
    shared_durable (sync_only state) term = shared_durable state term /\
    shared_checkpoint_lsn (sync_only state) = shared_checkpoint_lsn state.
Proof.
  intros state term.
  unfold sync_only.
  split; reflexivity.
Qed.

Theorem read_observes_visible_state :
  forall state term,
    read_value state term = shared_visible state term.
Proof.
  intros state term.
  unfold read_value.
  reflexivity.
Qed.

Theorem recover_uses_durable_checkpoint :
  forall state term value,
    shared_durable state term = Some value ->
    recover_value state term = Some value.
Proof.
  intros state term value Hdurable.
  unfold recover_value.
  rewrite Hdurable.
  reflexivity.
Qed.

Theorem recover_replays_retained_visible_wal :
  forall state term value lsn,
    shared_durable state term = None ->
    shared_visible state term = Some value ->
    shared_term_lsn state term = Some lsn ->
    shared_wal_retained_from state <= lsn ->
    lsn <= shared_synced_lsn state ->
    recover_value state term = Some value.
Proof.
  intros state term value lsn Hdurable Hvisible Hlsn Hretained Hsynced.
  unfold recover_value.
  rewrite Hdurable.
  rewrite Hlsn.
  apply Nat.leb_le in Hretained.
  apply Nat.leb_le in Hsynced.
  rewrite Hretained.
  rewrite Hsynced.
  simpl.
  exact Hvisible.
Qed.

Theorem recover_rejects_unretained_wal :
  forall state term value lsn,
    shared_durable state term = None ->
    shared_visible state term = Some value ->
    shared_term_lsn state term = Some lsn ->
    lsn < shared_wal_retained_from state ->
    recover_value state term = None.
Proof.
  intros state term value lsn Hdurable _ Hlsn Htruncated.
  unfold recover_value.
  rewrite Hdurable.
  rewrite Hlsn.
  apply Nat.leb_gt in Htruncated.
  rewrite Htruncated.
  reflexivity.
Qed.

Theorem checkpoint_success_recovery_matches_visible :
  forall state snapshot_lsn term,
    recover_value (finish_checkpoint_success state snapshot_lsn) term =
    shared_visible state term.
Proof.
  intros state snapshot_lsn term.
  unfold recover_value, finish_checkpoint_success.
  simpl.
  destruct (shared_visible state term) as [value|].
  - reflexivity.
  - destruct (shared_term_lsn state term) as [lsn|].
    + destruct lsn as [|lsn']; simpl.
      * reflexivity.
      * destruct ((snapshot_lsn <=? lsn') &&
                  match Nat.max (shared_synced_lsn state) snapshot_lsn with
                  | 0 => false
                  | S max' => lsn' <=? max'
                  end);
        reflexivity.
    + reflexivity.
Qed.
