(** Persistent recovery planner model.

    This specification composes the root/checkpoint trust decision, WAL segment
    collection, and durable-prefix replay obligations used by byte and char
    persistent trie recovery:

    - missing files create a fresh trie, clean files open normally, and corrupt
      files rebuild only when retained WAL input exists;
    - checkpoint skip thresholds are trusted only after a valid root load and a
      valid checkpoint record;
    - WAL replay stops at the first corrupt record and never applies later
      records outside the durable prefix;
    - corruption rebuild replays archive, pending, then active-tail segments;
    - byte and char recovery entry points refine the same planner.

    Kernel/filesystem internals below the WAL record abstraction, production
    Byzantine consensus, and certified Rust compilation are outside this proof
    boundary.
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

Definition map_increment (map : RefMap) (key : Key) (delta : Value)
  : RefMap :=
  let current :=
    match lookup map key with
    | Some value => value
    | None => 0
    end in
  map_put map key (current + delta).

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

Theorem map_put_lookup_other :
  forall map key other value,
    key <> other ->
    lookup (map_put map key value) other = lookup map other.
Proof.
  intros map key other value Hneq.
  unfold lookup, map_put.
  destruct (Nat.eq_dec other key) as [Heq | _].
  - exfalso. apply Hneq. symmetry. exact Heq.
  - reflexivity.
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

Theorem map_increment_lookup_same :
  forall map key delta,
    lookup (map_increment map key delta) key =
      Some
        ((match lookup map key with
          | Some value => value
          | None => 0
          end) + delta).
Proof.
  intros map key delta.
  unfold map_increment.
  apply map_put_lookup_same.
Qed.

Inductive WalOp : Type :=
| WalPut : Key -> Value -> WalOp
| WalDelete : Key -> WalOp
| WalIncrement : Key -> Value -> WalOp
| WalBatchPut : list (Key * Value) -> WalOp
| WalBatchIncrement : list (Key * Value) -> WalOp
| WalCheckpoint : Lsn -> WalOp.

Record WalEntry : Type := mkWalEntry {
  entry_lsn : Lsn;
  entry_op : WalOp
}.

Definition apply_batch_put
  (entries : list (Key * Value))
  (map : RefMap)
  : RefMap :=
  fold_left
    (fun acc entry =>
      let '(key, value) := entry in map_put acc key value)
    entries
    map.

Definition apply_batch_increment
  (entries : list (Key * Value))
  (map : RefMap)
  : RefMap :=
  fold_left
    (fun acc entry =>
      let '(key, delta) := entry in map_increment acc key delta)
    entries
    map.

Definition apply_wal_entry (map : RefMap) (entry : WalEntry) : RefMap :=
  match entry_op entry with
  | WalPut key value => map_put map key value
  | WalDelete key => map_remove map key
  | WalIncrement key delta => map_increment map key delta
  | WalBatchPut entries => apply_batch_put entries map
  | WalBatchIncrement entries => apply_batch_increment entries map
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

Definition should_skip (threshold : option Lsn) (entry : WalEntry) : bool :=
  match threshold with
  | Some checkpoint_lsn => entry_lsn entry <=? checkpoint_lsn
  | None => false
  end.

Fixpoint replay_entries_after
  (threshold : option Lsn)
  (entries : list WalEntry)
  (map : RefMap)
  : RefMap :=
  match entries with
  | [] => map
  | entry :: rest =>
      if should_skip threshold entry then
        replay_entries_after threshold rest map
      else
        replay_entries_after threshold rest (apply_wal_entry map entry)
  end.

Theorem no_threshold_matches_plain_replay :
  forall entries map,
    replay_entries_after None entries map = replay_entries entries map.
Proof.
  induction entries as [| entry rest IH]; intros map.
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Inductive RootTrust : Type :=
| RootLoaded
| RootRejected.

Record CheckpointState : Type := mkCheckpointState {
  checkpoint_valid : bool;
  checkpoint_lsn : Lsn;
  checkpoint_map : RefMap
}.

Definition checkpoint_trusted
  (root : RootTrust)
  (checkpoint : CheckpointState)
  : bool :=
  match root with
  | RootLoaded => checkpoint_valid checkpoint
  | RootRejected => false
  end.

Definition recovery_base
  (root : RootTrust)
  (checkpoint : CheckpointState)
  : RefMap :=
  if checkpoint_trusted root checkpoint
  then checkpoint_map checkpoint
  else empty_map.

Definition recovery_threshold
  (root : RootTrust)
  (checkpoint : CheckpointState)
  : option Lsn :=
  if checkpoint_trusted root checkpoint
  then Some (checkpoint_lsn checkpoint)
  else None.

Definition recover_from_entries
  (root : RootTrust)
  (checkpoint : CheckpointState)
  (entries : list WalEntry)
  : RefMap :=
  replay_entries_after
    (recovery_threshold root checkpoint)
    entries
    (recovery_base root checkpoint).

Definition invalid_checkpoint : CheckpointState :=
  mkCheckpointState false 0 empty_map.

Theorem rejected_root_has_no_skip_threshold :
  forall checkpoint,
    recovery_threshold RootRejected checkpoint = None.
Proof.
  intros checkpoint.
  destruct checkpoint as [[] lsn map]; reflexivity.
Qed.

Theorem invalid_checkpoint_has_no_skip_threshold :
  forall root lsn map,
    recovery_threshold root (mkCheckpointState false lsn map) = None.
Proof.
  intros root lsn map.
  destruct root; reflexivity.
Qed.

Theorem loaded_valid_checkpoint_uses_lsn :
  forall lsn map,
    recovery_threshold RootLoaded (mkCheckpointState true lsn map) = Some lsn.
Proof.
  reflexivity.
Qed.

Theorem rejected_root_replays_from_empty :
  forall checkpoint entries,
    recover_from_entries RootRejected checkpoint entries =
    replay_entries entries empty_map.
Proof.
  intros checkpoint entries.
  unfold recover_from_entries, recovery_threshold, recovery_base.
  destruct checkpoint as [[] lsn map]; simpl; apply no_threshold_matches_plain_replay.
Qed.

Theorem rejected_root_replays_old_put :
  forall checkpoint lsn key value,
    lookup
      (recover_from_entries
         RootRejected
         checkpoint
         [mkWalEntry lsn (WalPut key value)])
      key = Some value.
Proof.
  intros checkpoint lsn key value.
  rewrite rejected_root_replays_from_empty.
  simpl.
  apply map_put_lookup_same.
Qed.

Theorem loaded_valid_checkpoint_skips_covered_put :
  forall checkpoint_lsn' disk_map lsn key value,
    lsn <= checkpoint_lsn' ->
    recover_from_entries
      RootLoaded
      (mkCheckpointState true checkpoint_lsn' disk_map)
      [mkWalEntry lsn (WalPut key value)] = disk_map.
Proof.
  intros checkpoint_lsn' disk_map lsn key value Hle.
  unfold recover_from_entries, recovery_threshold, recovery_base.
  simpl. unfold should_skip. simpl.
  assert (Hskip : (lsn <=? checkpoint_lsn') = true).
  { apply Nat.leb_le. exact Hle. }
  rewrite Hskip.
  reflexivity.
Qed.

Theorem loaded_valid_checkpoint_replays_tail_put :
  forall checkpoint_lsn' disk_map lsn key value,
    checkpoint_lsn' < lsn ->
    lookup
      (recover_from_entries
         RootLoaded
         (mkCheckpointState true checkpoint_lsn' disk_map)
         [mkWalEntry lsn (WalPut key value)])
      key = Some value.
Proof.
  intros checkpoint_lsn' disk_map lsn key value Hlt.
  unfold recover_from_entries, recovery_threshold, recovery_base.
  simpl. unfold should_skip. simpl.
  assert (Hnoskip : (lsn <=? checkpoint_lsn') = false).
  { apply Nat.leb_gt. exact Hlt. }
  rewrite Hnoskip.
  apply map_put_lookup_same.
Qed.

Record ReplayRecord : Type := mkReplayRecord {
  replay_record_valid : bool;
  replay_record_entry : WalEntry
}.

Fixpoint durable_prefix (stream : list ReplayRecord) : list WalEntry :=
  match stream with
  | [] => []
  | record :: rest =>
      if replay_record_valid record then
        replay_record_entry record :: durable_prefix rest
      else
        []
  end.

Fixpoint all_valid (stream : list ReplayRecord) : bool :=
  match stream with
  | [] => true
  | record :: rest => replay_record_valid record && all_valid rest
  end.

Theorem durable_prefix_empty :
  durable_prefix [] = [].
Proof.
  reflexivity.
Qed.

Theorem durable_prefix_valid_cons :
  forall entry rest,
    durable_prefix (mkReplayRecord true entry :: rest) =
    entry :: durable_prefix rest.
Proof.
  reflexivity.
Qed.

Theorem durable_prefix_invalid_cons :
  forall entry rest,
    durable_prefix (mkReplayRecord false entry :: rest) = [].
Proof.
  reflexivity.
Qed.

Theorem all_valid_prefix_identity :
  forall stream,
    all_valid stream = true ->
    durable_prefix stream = map replay_record_entry stream.
Proof.
  induction stream as [| record rest IH]; intros Hall.
  - reflexivity.
  - destruct record as [valid entry]. simpl in *.
    apply andb_true_iff in Hall as [Hvalid Hall].
    rewrite Hvalid. simpl. rewrite IH by exact Hall. reflexivity.
Qed.

Theorem durable_prefix_stops_at_first_invalid :
  forall prefix bad suffix,
    all_valid prefix = true ->
    durable_prefix (prefix ++ mkReplayRecord false bad :: suffix) =
    map replay_record_entry prefix.
Proof.
  induction prefix as [| record rest IH]; intros bad suffix Hall.
  - reflexivity.
  - destruct record as [valid entry]. simpl in *.
    apply andb_true_iff in Hall as [Hvalid Hall].
    rewrite Hvalid. simpl. rewrite IH by exact Hall. reflexivity.
Qed.

Theorem durable_prefix_keeps_prefix_before_corruption :
  forall first bad,
    durable_prefix
      [mkReplayRecord true first; mkReplayRecord false bad] =
    [first].
Proof.
  reflexivity.
Qed.

Definition recover_from_stream
  (root : RootTrust)
  (checkpoint : CheckpointState)
  (stream : list ReplayRecord)
  : RefMap :=
  recover_from_entries root checkpoint (durable_prefix stream).

Theorem corrupt_middle_replays_prefix_put :
  forall first_lsn bad_lsn later_lsn key value bad_key later_key later_value,
    lookup
      (recover_from_stream
        RootRejected
        invalid_checkpoint
        [ mkReplayRecord true (mkWalEntry first_lsn (WalPut key value));
          mkReplayRecord false (mkWalEntry bad_lsn (WalPut bad_key value));
          mkReplayRecord true (mkWalEntry later_lsn (WalPut later_key later_value))])
      key = Some value.
Proof.
  intros first_lsn bad_lsn later_lsn key value bad_key later_key later_value.
  unfold recover_from_stream.
  simpl.
  apply map_put_lookup_same.
Qed.

Theorem corrupt_middle_does_not_replay_later_put :
  forall first_lsn bad_lsn later_lsn first_key later_key first_value bad_value later_value,
    first_key <> later_key ->
    lookup
      (recover_from_stream
        RootRejected
        invalid_checkpoint
        [ mkReplayRecord true (mkWalEntry first_lsn (WalPut first_key first_value));
          mkReplayRecord false (mkWalEntry bad_lsn (WalPut first_key bad_value));
          mkReplayRecord true (mkWalEntry later_lsn (WalPut later_key later_value))])
      later_key = None.
Proof.
  intros first_lsn bad_lsn later_lsn first_key later_key first_value bad_value later_value Hneq.
  unfold recover_from_stream, recover_from_entries, recovery_threshold,
    recovery_base, invalid_checkpoint.
  simpl. unfold apply_wal_entry. simpl.
  unfold lookup, map_put.
  destruct (Nat.eq_dec later_key first_key) as [Heq | _].
  - exfalso. apply Hneq. symmetry. exact Heq.
  - reflexivity.
Qed.

Inductive SegmentState : Type :=
| Active
| Pending
| Archived.

Record Segment : Type := mkSegment {
  segment_state : SegmentState;
  segment_first_lsn : Lsn;
  segment_entries : list WalEntry
}.

Definition segment_has_records (segment : Segment) : bool :=
  match segment_entries segment with
  | [] => false
  | _ :: _ => true
  end.

Definition replay_segment (segment : Segment) (map : RefMap) : RefMap :=
  replay_entries (segment_entries segment) map.

Fixpoint replay_segments (segments : list Segment) (map : RefMap) : RefMap :=
  match segments with
  | [] => map
  | segment :: rest => replay_segments rest (replay_segment segment map)
  end.

Theorem replay_segments_app :
  forall left right map,
    replay_segments (left ++ right) map =
    replay_segments right (replay_segments left map).
Proof.
  induction left as [| segment rest IH]; intros right map.
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Definition collect_retained_segments
  (archive pending : list Segment)
  (active : option Segment)
  : list Segment :=
  archive ++ pending ++
    match active with
    | Some segment =>
        if segment_has_records segment then [segment] else []
    | None => []
    end.

Theorem empty_active_tail_not_retained :
  forall archive pending first,
    collect_retained_segments
      archive pending (Some (mkSegment Active first [])) =
    archive ++ pending.
Proof.
  intros archive pending first.
  unfold collect_retained_segments, segment_has_records.
  rewrite app_nil_r.
  reflexivity.
Qed.

Theorem nonempty_active_tail_retained :
  forall archive pending active,
    segment_entries active <> [] ->
    In active (collect_retained_segments archive pending (Some active)).
Proof.
  intros archive pending active Hnonempty.
  unfold collect_retained_segments.
  destruct (segment_entries active) as [| entry rest] eqn:Hentries.
  - contradiction Hnonempty. reflexivity.
  - unfold segment_has_records. rewrite Hentries. simpl.
    apply in_or_app. right.
    apply in_or_app. right.
    simpl. left. reflexivity.
Qed.

Theorem archive_pending_active_replay_order :
  forall archive pending active map,
    segment_entries active <> [] ->
    replay_segments
      (collect_retained_segments archive pending (Some active))
      map =
    replay_segment active (replay_segments pending (replay_segments archive map)).
Proof.
  intros archive pending active map Hnonempty.
  unfold collect_retained_segments.
  destruct (segment_entries active) as [| entry rest] eqn:Hentries.
  - contradiction Hnonempty. reflexivity.
  - unfold segment_has_records. rewrite Hentries. simpl.
    repeat rewrite replay_segments_app.
    reflexivity.
Qed.

Fixpoint segments_have_any_records (segments : list Segment) : bool :=
  match segments with
  | [] => false
  | segment :: rest => segment_has_records segment || segments_have_any_records rest
  end.

Inductive DataFileState : Type :=
| FileMissing
| FileClean
| FileCorrupt.

Inductive PlannerMode : Type :=
| PlanNormal
| PlanCreatedNew
| PlanRebuildFromWal
| PlanUnrecoverable.

Definition planner_mode
  (file_state : DataFileState)
  (segments : list Segment)
  : PlannerMode :=
  match file_state with
  | FileMissing => PlanCreatedNew
  | FileClean => PlanNormal
  | FileCorrupt =>
      if segments_have_any_records segments
      then PlanRebuildFromWal
      else PlanUnrecoverable
  end.

Theorem missing_file_creates_new :
  forall segments,
    planner_mode FileMissing segments = PlanCreatedNew.
Proof.
  reflexivity.
Qed.

Theorem clean_file_opens_normal :
  forall segments,
    planner_mode FileClean segments = PlanNormal.
Proof.
  reflexivity.
Qed.

Theorem corrupt_file_without_segments_unrecoverable :
  planner_mode FileCorrupt [] = PlanUnrecoverable.
Proof.
  reflexivity.
Qed.

Theorem corrupt_file_with_nonempty_segment_rebuilds :
  forall state first entries rest,
    planner_mode
      FileCorrupt
      (mkSegment state first (entries :: rest) :: []) =
    PlanRebuildFromWal.
Proof.
  reflexivity.
Qed.

Theorem corrupt_file_with_empty_active_only_unrecoverable :
  forall first,
    planner_mode FileCorrupt [mkSegment Active first []] = PlanUnrecoverable.
Proof.
  reflexivity.
Qed.

Inductive Backend : Type :=
| ByteBackend
| CharBackend.

Definition recover_backend
  (_backend : Backend)
  (root : RootTrust)
  (checkpoint : CheckpointState)
  (stream : list ReplayRecord)
  : RefMap :=
  recover_from_stream root checkpoint stream.

Theorem byte_and_char_recovery_refine_same_planner :
  forall root checkpoint stream,
    recover_backend ByteBackend root checkpoint stream =
    recover_backend CharBackend root checkpoint stream.
Proof.
  reflexivity.
Qed.

Theorem byte_rejected_root_uses_durable_prefix :
  forall checkpoint stream,
    recover_backend ByteBackend RootRejected checkpoint stream =
    replay_entries (durable_prefix stream) empty_map.
Proof.
  intros checkpoint stream.
  unfold recover_backend, recover_from_stream.
  apply rejected_root_replays_from_empty.
Qed.

Theorem char_rejected_root_uses_durable_prefix :
  forall checkpoint stream,
    recover_backend CharBackend RootRejected checkpoint stream =
    replay_entries (durable_prefix stream) empty_map.
Proof.
  intros checkpoint stream.
  unfold recover_backend, recover_from_stream.
  apply rejected_root_replays_from_empty.
Qed.
