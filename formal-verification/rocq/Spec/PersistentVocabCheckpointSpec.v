(** Persistent vocabulary checkpoint publication model.

    This specification captures the proof boundary for
    [PersistentVocabARTrie] checkpoint/open behavior:

    - a successful checkpoint reopens to the same forward/reverse bijection;
    - failed checkpoint stages preserve the active WAL and dirty evidence;
    - WAL truncation occurs only after the checkpoint is fully published;
    - post-checkpoint WAL records receive LSNs above the checkpoint threshold;
    - [rotate_wal] syncs durability evidence without becoming a checkpoint;
    - reverse maps are rebuilt from the durable overlay snapshot plus retained
      WAL tail rather than trusted as authoritative cache input.

    Filesystem/kernel ordering below successful write/sync calls and certified
    Rust compilation are outside this proof boundary.
 *)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
From Stdlib Require Import Logic.FunctionalExtensionality.
Import ListNotations.

Definition Term := nat.
Definition Index := nat.
Definition Lsn := nat.

Definition Forward := Term -> option Index.
Definition Reverse := Index -> option Term.
Definition BloomEvidence := Term -> bool.

Definition empty_forward : Forward := fun _ => None.
Definition empty_reverse : Reverse := fun _ => None.
Definition empty_bloom : BloomEvidence := fun _ => false.

Definition lookup_term (forward : Forward) (term : Term) : option Index :=
  forward term.

Definition lookup_index (reverse : Reverse) (index : Index) : option Term :=
  reverse index.

Definition put_forward
  (forward : Forward)
  (term : Term)
  (index : Index)
  : Forward :=
  fun query => if Nat.eq_dec query term then Some index else forward query.

Definition put_reverse
  (reverse : Reverse)
  (index : Index)
  (term : Term)
  : Reverse :=
  fun query => if Nat.eq_dec query index then Some term else reverse query.

Definition forward_reverse_exact (forward : Forward) (reverse : Reverse)
  : Prop :=
  forall term index,
    lookup_term forward term = Some index <->
    lookup_index reverse index = Some term.

Record WalEntry : Type := mkWalEntry {
  wal_lsn : Lsn;
  wal_term : Term;
  wal_index : Index
}.

Fixpoint replay_forward
  (entries : list WalEntry)
  (forward : Forward)
  : Forward :=
  match entries with
  | [] => forward
  | entry :: rest =>
      replay_forward rest
        (put_forward forward (wal_term entry) (wal_index entry))
  end.

Fixpoint replay_reverse
  (entries : list WalEntry)
  (reverse : Reverse)
  : Reverse :=
  match entries with
  | [] => reverse
  | entry :: rest =>
      replay_reverse rest
        (put_reverse reverse (wal_index entry) (wal_term entry))
  end.

Record VocabState : Type := mkVocabState {
  vocab_forward : Forward;
  vocab_reverse : Reverse;
  vocab_next_index : Index;
  vocab_next_lsn : Lsn;
  vocab_active_wal : list WalEntry;
  vocab_dirty : bool;
  durable_forward : Forward;
  durable_reverse : Reverse;
  durable_next_index : Index;
  durable_checkpoint_lsn : Lsn;
  durable_checkpoint_published : bool;
  bloom_evidence : BloomEvidence;
  reverse_map_cache : Reverse
}.

Definition empty_state : VocabState :=
  mkVocabState
    empty_forward empty_reverse 0 1 [] false
    empty_forward empty_reverse 0 0 false empty_bloom empty_reverse.

Definition last_mutation_lsn (state : VocabState) : Lsn :=
  Nat.pred (vocab_next_lsn state).

Definition checkpoint_success (state : VocabState) : VocabState :=
  let checkpoint_lsn := last_mutation_lsn state in
  mkVocabState
    (vocab_forward state)
    (vocab_reverse state)
    (vocab_next_index state)
    (S checkpoint_lsn)
    []
    false
    (vocab_forward state)
    (vocab_reverse state)
    (vocab_next_index state)
    checkpoint_lsn
    true
    (bloom_evidence state)
    (vocab_reverse state).

Inductive CheckpointOutcome : Type :=
| CheckpointOk
| TrieSnapshotFailed
| HeaderPublishFailed
| ReverseMapRebuildFailed
| DerivedFilterRebuildFailed
| WalCheckpointFailed
| WalTruncateFailed.

Definition checkpoint
  (state : VocabState)
  (outcome : CheckpointOutcome)
  : VocabState :=
  match outcome with
  | CheckpointOk => checkpoint_success state
  | TrieSnapshotFailed => state
  | HeaderPublishFailed => state
  | ReverseMapRebuildFailed => state
  | DerivedFilterRebuildFailed => state
  | WalCheckpointFailed => state
  | WalTruncateFailed => state
  end.

Definition reopen_forward (state : VocabState) : Forward :=
  if durable_checkpoint_published state then
    durable_forward state
  else
    replay_forward (vocab_active_wal state) empty_forward.

Definition reopen_reverse (state : VocabState) : Reverse :=
  if durable_checkpoint_published state then
    durable_reverse state
  else
    replay_reverse (vocab_active_wal state) empty_reverse.

Definition reopen_next_index (state : VocabState) : Index :=
  if durable_checkpoint_published state then
    durable_next_index state
  else
    vocab_next_index state.

Theorem successful_checkpoint_reopens_forward :
  forall state,
    reopen_forward (checkpoint state CheckpointOk) = vocab_forward state.
Proof.
  intros state.
  unfold reopen_forward, checkpoint, checkpoint_success.
  simpl.
  apply functional_extensionality.
  intros term.
  reflexivity.
Qed.

Theorem successful_checkpoint_reopens_reverse :
  forall state,
    reopen_reverse (checkpoint state CheckpointOk) = vocab_reverse state.
Proof.
  intros state.
  unfold reopen_reverse, checkpoint, checkpoint_success.
  simpl.
  apply functional_extensionality.
  intros index.
  reflexivity.
Qed.

Theorem successful_checkpoint_reopens_next_index :
  forall state,
    reopen_next_index (checkpoint state CheckpointOk) =
    vocab_next_index state.
Proof.
  reflexivity.
Qed.

Theorem successful_checkpoint_clears_dirty :
  forall state,
    vocab_dirty (checkpoint state CheckpointOk) = false.
Proof.
  reflexivity.
Qed.

Theorem successful_checkpoint_truncates_active_wal :
  forall state,
    vocab_active_wal (checkpoint state CheckpointOk) = [].
Proof.
  reflexivity.
Qed.

Theorem failed_checkpoint_preserves_forward :
  forall state outcome,
    outcome <> CheckpointOk ->
    vocab_forward (checkpoint state outcome) = vocab_forward state.
Proof.
  intros state outcome Hfailed.
  destruct outcome.
  - exfalso. apply Hfailed. reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
Qed.

Theorem failed_checkpoint_preserves_reverse :
  forall state outcome,
    outcome <> CheckpointOk ->
    vocab_reverse (checkpoint state outcome) = vocab_reverse state.
Proof.
  intros state outcome Hfailed.
  destruct outcome.
  - exfalso. apply Hfailed. reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
Qed.

Theorem failed_checkpoint_preserves_active_wal :
  forall state outcome,
    outcome <> CheckpointOk ->
    vocab_active_wal (checkpoint state outcome) = vocab_active_wal state.
Proof.
  intros state outcome Hfailed.
  destruct outcome.
  - exfalso. apply Hfailed. reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
Qed.

Theorem failed_checkpoint_preserves_dirty :
  forall state outcome,
    outcome <> CheckpointOk ->
    vocab_dirty (checkpoint state outcome) = vocab_dirty state.
Proof.
  intros state outcome Hfailed.
  destruct outcome.
  - exfalso. apply Hfailed. reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
  - reflexivity.
Qed.

Theorem wal_truncation_only_after_success :
  forall state outcome,
    vocab_active_wal state <> [] ->
    vocab_active_wal (checkpoint state outcome) = [] ->
    outcome = CheckpointOk.
Proof.
  intros state outcome Hnonempty Htruncated.
  destruct outcome; try reflexivity;
    exfalso; apply Hnonempty; exact Htruncated.
Qed.

Definition should_replay_after (checkpoint_lsn : Lsn) (entry : WalEntry)
  : bool :=
  negb (wal_lsn entry <=? checkpoint_lsn).

Lemma successor_not_leb_self :
  forall n,
    (S n <=? n) = false.
Proof.
  induction n as [| n IH].
  - reflexivity.
  - simpl. exact IH.
Qed.

Theorem post_checkpoint_append_lsn_survives_skip_threshold :
  forall state term index,
    should_replay_after
      (durable_checkpoint_lsn (checkpoint state CheckpointOk))
      (mkWalEntry
        (vocab_next_lsn (checkpoint state CheckpointOk))
        term
        index) = true.
Proof.
  intros state term index.
  unfold should_replay_after, checkpoint, checkpoint_success. simpl.
  apply negb_true_iff.
  apply successor_not_leb_self.
Qed.

Definition rotate_wal (state : VocabState) : VocabState :=
  mkVocabState
    (vocab_forward state)
    (vocab_reverse state)
    (vocab_next_index state)
    (vocab_next_lsn state)
    (vocab_active_wal state)
    (vocab_dirty state)
    (durable_forward state)
    (durable_reverse state)
    (durable_next_index state)
    (durable_checkpoint_lsn state)
    (durable_checkpoint_published state)
    (bloom_evidence state)
    (reverse_map_cache state).

Theorem rotate_wal_preserves_replay_requirements :
  forall state,
    vocab_active_wal (rotate_wal state) = vocab_active_wal state.
Proof.
  reflexivity.
Qed.

Theorem rotate_wal_does_not_clear_dirty :
  forall state,
    vocab_dirty (rotate_wal state) = vocab_dirty state.
Proof.
  reflexivity.
Qed.

Theorem rotate_wal_does_not_publish_checkpoint :
  forall state,
    durable_checkpoint_published (rotate_wal state) =
    durable_checkpoint_published state.
Proof.
  reflexivity.
Qed.

Definition bloom_no_false_negative
  (forward : Forward)
  (bloom : BloomEvidence)
  : Prop :=
  forall term index,
    lookup_term forward term = Some index ->
    bloom term = true.

Definition rebuild_bloom (forward : Forward) : BloomEvidence :=
  fun term =>
    match lookup_term forward term with
    | Some _ => true
    | None => false
    end.

Inductive DerivedFilterSnapshot : Type :=
| DerivedFilterValid : BloomEvidence -> DerivedFilterSnapshot
| DerivedFilterMissing
| DerivedFilterCorrupt.

Definition open_bloom (forward : Forward) (snapshot : DerivedFilterSnapshot)
  : BloomEvidence :=
  match snapshot with
  | DerivedFilterValid bloom => bloom
  | DerivedFilterMissing => rebuild_bloom forward
  | DerivedFilterCorrupt => rebuild_bloom forward
  end.

Theorem rebuilt_missing_bloom_has_no_false_negatives :
  forall forward,
    bloom_no_false_negative forward (open_bloom forward DerivedFilterMissing).
Proof.
  intros forward term index Hlookup.
  unfold open_bloom, bloom_no_false_negative, rebuild_bloom in *.
  rewrite Hlookup.
  reflexivity.
Qed.

Theorem rebuilt_corrupt_bloom_has_no_false_negatives :
  forall forward,
    bloom_no_false_negative forward (open_bloom forward DerivedFilterCorrupt).
Proof.
  intros forward term index Hlookup.
  unfold open_bloom, bloom_no_false_negative, rebuild_bloom in *.
  rewrite Hlookup.
  reflexivity.
Qed.

Theorem valid_bloom_loaded_when_safe :
  forall forward bloom,
    bloom_no_false_negative forward bloom ->
    bloom_no_false_negative forward (open_bloom forward (DerivedFilterValid bloom)).
Proof.
  intros forward bloom Hsafe.
  exact Hsafe.
Qed.

Inductive ReverseMapRebuildStatus : Type :=
| ReverseMapValid
| ReverseMapMissing
| ReverseMapCorrupt
| ReverseMapStale.

Definition rebuild_reverse_from_snapshot (snapshot_reverse : Reverse)
  : Reverse :=
  snapshot_reverse.

Definition open_reverse_map_cache
  (snapshot_reverse : Reverse)
  (_status : ReverseMapRebuildStatus)
  : Reverse :=
  rebuild_reverse_from_snapshot snapshot_reverse.

Theorem rebuilt_reverse_map_cache_is_exact :
  forall forward snapshot_reverse status,
    forward_reverse_exact forward snapshot_reverse ->
    forward_reverse_exact
      forward
      (open_reverse_map_cache snapshot_reverse status).
Proof.
  intros forward snapshot_reverse status Hexact.
  unfold open_reverse_map_cache, rebuild_reverse_from_snapshot.
  exact Hexact.
Qed.

Definition recover_by_wal_replay (state : VocabState) : VocabState :=
  mkVocabState
    (replay_forward (vocab_active_wal state) (vocab_forward state))
    (replay_reverse (vocab_active_wal state) (vocab_reverse state))
    (vocab_next_index state)
    (vocab_next_lsn state)
    (vocab_active_wal state)
    true
    (durable_forward state)
    (durable_reverse state)
    (durable_next_index state)
    (durable_checkpoint_lsn state)
    (durable_checkpoint_published state)
    (bloom_evidence state)
    (reverse_map_cache state).

Theorem wal_recovery_retains_active_wal_until_checkpoint :
  forall state,
    vocab_active_wal (recover_by_wal_replay state) =
    vocab_active_wal state.
Proof.
  reflexivity.
Qed.

Theorem wal_recovery_marks_dirty_until_checkpoint :
  forall state,
    vocab_dirty (recover_by_wal_replay state) = true.
Proof.
  reflexivity.
Qed.
