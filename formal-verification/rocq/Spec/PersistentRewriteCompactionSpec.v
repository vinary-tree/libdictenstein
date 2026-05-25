(** Persistent char/vocab rewrite-compaction safety model.

    Char and vocabulary persistent tries do not currently expose the byte
    trie's public file-size compaction API.  Their durable rewrite surface is
    checkpoint publication: serialize the visible trie snapshot, publish the
    root/header and sidecars, then publish the WAL checkpoint/truncation or
    archive boundary.

    This specification captures the semantic obligations for that rewrite:

    - successful char rewrites preserve the exact key/value snapshot;
    - successful vocab rewrites preserve sparse forward/reverse index
      snapshots;
    - post-checkpoint WAL tails replay over the checkpointed snapshot; and
    - failures during arena/header/sidecar/WAL publication preserve dirty
      evidence and replayable WAL.
 *)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Logic.FunctionalExtensionality.
Import ListNotations.

Definition Key := nat.
Definition Term := nat.
Definition Value := nat.
Definition Index := nat.

Definition RefMap := Key -> option Value.

Definition lookup (map : RefMap) (key : Key) : option Value :=
  map key.

Definition same_map (left right : RefMap) : Prop :=
  forall key, lookup left key = lookup right key.

Definition map_put (map : RefMap) (key : Key) (value : Value) : RefMap :=
  fun query => if Nat.eq_dec query key then Some value else map query.

Definition map_remove (map : RefMap) (key : Key) : RefMap :=
  fun query => if Nat.eq_dec query key then None else map query.

Inductive CharWalOp : Type :=
| CharPut : Key -> Value -> CharWalOp
| CharDelete : Key -> CharWalOp.

Definition apply_char_wal_op (op : CharWalOp) (map : RefMap) : RefMap :=
  match op with
  | CharPut key value => map_put map key value
  | CharDelete key => map_remove map key
  end.

Fixpoint replay_char_wal (wal : list CharWalOp) (map : RefMap) : RefMap :=
  match wal with
  | [] => map
  | op :: rest => replay_char_wal rest (apply_char_wal_op op map)
  end.

Inductive RewriteFailure : Type :=
| FailArenaWrite
| FailHeaderPublish
| FailSidecarPublish
| FailWalCheckpoint
| FailWalTruncateOrArchive.

Inductive RewriteOutcome : Type :=
| RewriteSuccess
| RewriteFailed : RewriteFailure -> RewriteOutcome.

Record CharRewriteState : Type := mkCharRewriteState {
  char_visible : RefMap;
  char_checkpoint : RefMap;
  char_wal : list CharWalOp;
  char_dirty : bool
}.

Definition char_recover (state : CharRewriteState) : RefMap :=
  replay_char_wal (char_wal state) (char_checkpoint state).

Definition char_checkpoint_rewrite
  (outcome : RewriteOutcome)
  (state : CharRewriteState)
  : CharRewriteState :=
  match outcome with
  | RewriteSuccess =>
      mkCharRewriteState (char_visible state) (char_visible state) [] false
  | RewriteFailed _ => state
  end.

Definition char_persist_descriptor_only
  (state : CharRewriteState)
  : CharRewriteState :=
  mkCharRewriteState
    (char_visible state)
    (char_visible state)
    (char_wal state)
    (char_dirty state).

Definition char_append_tail
  (state : CharRewriteState)
  (op : CharWalOp)
  : CharRewriteState :=
  mkCharRewriteState
    (apply_char_wal_op op (char_visible state))
    (char_checkpoint state)
    (char_wal state ++ [op])
    true.

Definition VocabSnapshot := list (Term * Index).
Definition VocabWal := list (Term * Index).

Fixpoint vocab_forward_lookup
  (snapshot : VocabSnapshot)
  (term : Term)
  : option Index :=
  match snapshot with
  | [] => None
  | (entry_term, entry_index) :: rest =>
      if Nat.eq_dec term entry_term
      then Some entry_index
      else vocab_forward_lookup rest term
  end.

Fixpoint vocab_reverse_lookup
  (snapshot : VocabSnapshot)
  (index : Index)
  : option Term :=
  match snapshot with
  | [] => None
  | (entry_term, entry_index) :: rest =>
      if Nat.eq_dec index entry_index
      then Some entry_term
      else vocab_reverse_lookup rest index
  end.

Definition same_vocab_snapshot
  (left right : VocabSnapshot)
  : Prop :=
  forall term index,
    vocab_forward_lookup left term = vocab_forward_lookup right term /\
    vocab_reverse_lookup left index = vocab_reverse_lookup right index.

Record VocabRewriteState : Type := mkVocabRewriteState {
  vocab_visible : VocabSnapshot;
  vocab_checkpoint : VocabSnapshot;
  vocab_wal : VocabWal;
  vocab_dirty : bool
}.

Definition replay_vocab_wal
  (wal : VocabWal)
  (snapshot : VocabSnapshot)
  : VocabSnapshot :=
  snapshot ++ wal.

Definition vocab_recover (state : VocabRewriteState) : VocabSnapshot :=
  replay_vocab_wal (vocab_wal state) (vocab_checkpoint state).

Definition vocab_checkpoint_rewrite
  (outcome : RewriteOutcome)
  (state : VocabRewriteState)
  : VocabRewriteState :=
  match outcome with
  | RewriteSuccess =>
      mkVocabRewriteState (vocab_visible state) (vocab_visible state) [] false
  | RewriteFailed _ => state
  end.

Definition vocab_append_tail
  (state : VocabRewriteState)
  (entry : Term * Index)
  : VocabRewriteState :=
  mkVocabRewriteState
    (vocab_visible state ++ [entry])
    (vocab_checkpoint state)
    (vocab_wal state ++ [entry])
    true.

Definition rebuild_vocab_reverse_sidecar
  (snapshot : VocabSnapshot)
  : VocabSnapshot :=
  snapshot.

Theorem map_put_lookup_same :
  forall map key value,
    lookup (map_put map key value) key = Some value.
Proof.
  intros map key value.
  unfold lookup, map_put.
  destruct (Nat.eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem map_remove_lookup_same :
  forall map key,
    lookup (map_remove map key) key = None.
Proof.
  intros map key.
  unfold lookup, map_remove.
  destruct (Nat.eq_dec key key) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem replay_char_wal_app :
  forall left right map,
    replay_char_wal (left ++ right) map =
      replay_char_wal right (replay_char_wal left map).
Proof.
  induction left as [| op rest IH]; intros right map.
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Theorem char_successful_rewrite_checkpoint_is_visible :
  forall state,
    same_map
      (char_checkpoint (char_checkpoint_rewrite RewriteSuccess state))
      (char_visible state).
Proof.
  intros state key.
  reflexivity.
Qed.

Theorem char_successful_rewrite_recovery_is_visible :
  forall state,
    same_map (char_recover (char_checkpoint_rewrite RewriteSuccess state))
      (char_visible state).
Proof.
  intros state key.
  reflexivity.
Qed.

Theorem char_successful_rewrite_clears_dirty :
  forall state,
    char_dirty (char_checkpoint_rewrite RewriteSuccess state) = false.
Proof.
  intros state. reflexivity.
Qed.

Theorem char_failed_rewrite_preserves_dirty :
  forall state failure,
    char_dirty (char_checkpoint_rewrite (RewriteFailed failure) state) =
      char_dirty state.
Proof.
  intros state failure. reflexivity.
Qed.

Theorem char_failed_rewrite_retains_wal :
  forall state failure,
    char_wal (char_checkpoint_rewrite (RewriteFailed failure) state) =
      char_wal state.
Proof.
  intros state failure. reflexivity.
Qed.

Theorem char_failed_rewrite_preserves_recovery :
  forall state failure,
    same_map
      (char_recover (char_checkpoint_rewrite (RewriteFailed failure) state))
      (char_recover state).
Proof.
  intros state failure key.
  reflexivity.
Qed.

Theorem char_persist_descriptor_only_keeps_dirty :
  forall state,
    char_dirty (char_persist_descriptor_only state) = char_dirty state.
Proof.
  intros state. reflexivity.
Qed.

Theorem char_persist_descriptor_only_retains_wal :
  forall state,
    char_wal (char_persist_descriptor_only state) = char_wal state.
Proof.
  intros state. reflexivity.
Qed.

Theorem char_tail_after_successful_rewrite_replays_over_checkpoint :
  forall state op,
    same_map
      (char_recover
        (char_append_tail
          (char_checkpoint_rewrite RewriteSuccess state)
          op))
      (apply_char_wal_op op (char_visible state)).
Proof.
  intros state op key.
  reflexivity.
Qed.

Theorem vocab_successful_rewrite_preserves_forward :
  forall state term,
    vocab_forward_lookup
      (vocab_checkpoint (vocab_checkpoint_rewrite RewriteSuccess state))
      term =
    vocab_forward_lookup (vocab_visible state) term.
Proof.
  intros state term. reflexivity.
Qed.

Theorem vocab_successful_rewrite_preserves_reverse :
  forall state index,
    vocab_reverse_lookup
      (vocab_checkpoint (vocab_checkpoint_rewrite RewriteSuccess state))
      index =
    vocab_reverse_lookup (vocab_visible state) index.
Proof.
  intros state index. reflexivity.
Qed.

Theorem vocab_successful_rewrite_clears_dirty :
  forall state,
    vocab_dirty (vocab_checkpoint_rewrite RewriteSuccess state) = false.
Proof.
  intros state. reflexivity.
Qed.

Theorem vocab_failed_rewrite_preserves_dirty :
  forall state failure,
    vocab_dirty (vocab_checkpoint_rewrite (RewriteFailed failure) state) =
      vocab_dirty state.
Proof.
  intros state failure. reflexivity.
Qed.

Theorem vocab_failed_rewrite_retains_wal :
  forall state failure,
    vocab_wal (vocab_checkpoint_rewrite (RewriteFailed failure) state) =
      vocab_wal state.
Proof.
  intros state failure. reflexivity.
Qed.

Theorem vocab_failed_rewrite_preserves_recovery :
  forall state failure,
    vocab_recover (vocab_checkpoint_rewrite (RewriteFailed failure) state) =
      vocab_recover state.
Proof.
  intros state failure. reflexivity.
Qed.

Theorem vocab_missing_sidecar_rebuilds_reverse_lookup :
  forall snapshot index,
    vocab_reverse_lookup (rebuild_vocab_reverse_sidecar snapshot) index =
      vocab_reverse_lookup snapshot index.
Proof.
  intros snapshot index. reflexivity.
Qed.

Theorem vocab_missing_sidecar_rebuilds_forward_lookup :
  forall snapshot term,
    vocab_forward_lookup (rebuild_vocab_reverse_sidecar snapshot) term =
      vocab_forward_lookup snapshot term.
Proof.
  intros snapshot term. reflexivity.
Qed.

Theorem vocab_sparse_index_preserved_by_rewrite :
  forall state term index,
    vocab_forward_lookup (vocab_visible state) term = Some index ->
    vocab_forward_lookup
      (vocab_checkpoint (vocab_checkpoint_rewrite RewriteSuccess state))
      term = Some index.
Proof.
  intros state term index Hlookup.
  exact Hlookup.
Qed.

Theorem vocab_reverse_sparse_index_preserved_by_rewrite :
  forall state term index,
    vocab_reverse_lookup (vocab_visible state) index = Some term ->
    vocab_reverse_lookup
      (vocab_checkpoint (vocab_checkpoint_rewrite RewriteSuccess state))
      index = Some term.
Proof.
  intros state term index Hlookup.
  exact Hlookup.
Qed.

Theorem vocab_tail_after_successful_rewrite_replays_over_checkpoint :
  forall state entry,
    vocab_recover
      (vocab_append_tail
        (vocab_checkpoint_rewrite RewriteSuccess state)
        entry) =
      vocab_visible state ++ [entry].
Proof.
  intros state entry.
  unfold vocab_recover, vocab_append_tail, vocab_checkpoint_rewrite,
    replay_vocab_wal.
  simpl. reflexivity.
Qed.

Theorem same_vocab_snapshot_reflexive :
  forall snapshot,
    same_vocab_snapshot snapshot snapshot.
Proof.
  intros snapshot term index.
  split; reflexivity.
Qed.

Theorem successful_vocab_rewrite_same_snapshot :
  forall state,
    same_vocab_snapshot
      (vocab_checkpoint (vocab_checkpoint_rewrite RewriteSuccess state))
      (vocab_visible state).
Proof.
  intros state term index.
  split; reflexivity.
Qed.

Theorem char_rewrite_success_recovery_extensional :
  forall state,
    char_recover (char_checkpoint_rewrite RewriteSuccess state) =
      char_visible state.
Proof.
  intros state.
  apply functional_extensionality.
  intro key.
  reflexivity.
Qed.
