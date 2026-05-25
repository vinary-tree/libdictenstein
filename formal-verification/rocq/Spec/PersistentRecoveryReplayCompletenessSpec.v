(** Persistent recovery replay completeness model.

    This specification captures the proof boundary for replaying persistent ART
    WAL records after a crash or corruption rebuild:

    - every mutating WAL record is converted to the same replay operation by
      byte recovery, char recovery, archive recovery, and incremental recovery;
    - batch records expand in source order and failed compare-and-swap records
      contribute no operation;
    - replay stops at the first corrupt WAL record and ignores every suffix
      record after it; and
    - replay applies no-WAL trie mutations, so recovery does not echo recovered
      operations back into the active WAL.
 *)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
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

Definition option_value (value : option Value) : Value :=
  match value with
  | Some current => current
  | None => 0
  end.

Definition map_increment (map : RefMap) (key : Key) (delta : Value)
  : RefMap :=
  map_put map key (option_value (lookup map key) + delta).

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

Theorem map_increment_lookup_same :
  forall map key delta,
    lookup (map_increment map key delta) key =
      Some (option_value (lookup map key) + delta).
Proof.
  intros map key delta.
  unfold map_increment.
  apply map_put_lookup_same.
Qed.

Inductive WalRecord : Type :=
| RecordInsert : Key -> option Value -> WalRecord
| RecordRemove : Key -> WalRecord
| RecordIncrement : Key -> Value -> Value -> WalRecord
| RecordUpsert : Key -> Value -> WalRecord
| RecordCompareAndSwap : Key -> Value -> bool -> WalRecord
| RecordBatchInsert : list (Key * option Value) -> WalRecord
| RecordBatchIncrement : list (Key * Value) -> WalRecord
| RecordCheckpoint : Lsn -> WalRecord
| RecordBeginTx : nat -> WalRecord
| RecordCommitTx : nat -> WalRecord
| RecordAbortTx : nat -> WalRecord
| RecordVersionUpdate : WalRecord
| RecordVersionDurable : WalRecord
| RecordVersionGc : WalRecord.

Inductive RecoveredOperation : Type :=
| OpInsert : Lsn -> Key -> option Value -> RecoveredOperation
| OpRemove : Lsn -> Key -> RecoveredOperation
| OpIncrement : Lsn -> Key -> Value -> Value -> RecoveredOperation
| OpUpsert : Lsn -> Key -> Value -> RecoveredOperation
| OpCompareAndSwap : Lsn -> Key -> Value -> RecoveredOperation.

Definition op_lsn (op : RecoveredOperation) : Lsn :=
  match op with
  | OpInsert lsn _ _ => lsn
  | OpRemove lsn _ => lsn
  | OpIncrement lsn _ _ _ => lsn
  | OpUpsert lsn _ _ => lsn
  | OpCompareAndSwap lsn _ _ => lsn
  end.

Definition ops_of_record
  (lsn : Lsn)
  (record : WalRecord)
  : list RecoveredOperation :=
  match record with
  | RecordInsert key value => [OpInsert lsn key value]
  | RecordRemove key => [OpRemove lsn key]
  | RecordIncrement key delta result =>
      [OpIncrement lsn key delta result]
  | RecordUpsert key value => [OpUpsert lsn key value]
  | RecordCompareAndSwap key new_value success =>
      if success then [OpCompareAndSwap lsn key new_value] else []
  | RecordBatchInsert entries =>
      map
        (fun entry =>
          let '(key, value) := entry in OpInsert lsn key value)
        entries
  | RecordBatchIncrement entries =>
      map
        (fun entry =>
          let '(key, delta) := entry in OpIncrement lsn key delta 0)
        entries
  | RecordCheckpoint _
  | RecordBeginTx _
  | RecordCommitTx _
  | RecordAbortTx _
  | RecordVersionUpdate
  | RecordVersionDurable
  | RecordVersionGc => []
  end.

Definition apply_insert_value
  (map : RefMap)
  (key : Key)
  (value : option Value)
  : RefMap :=
  match value with
  | Some stored => map_put map key stored
  | None => map_put map key 0
  end.

Definition apply_increment_result
  (map : RefMap)
  (key : Key)
  (delta result : Value)
  : RefMap :=
  if result =? 0
  then map_increment map key delta
  else map_put map key result.

Definition apply_op (map : RefMap) (op : RecoveredOperation) : RefMap :=
  match op with
  | OpInsert _ key value => apply_insert_value map key value
  | OpRemove _ key => map_remove map key
  | OpIncrement _ key delta result =>
      apply_increment_result map key delta result
  | OpUpsert _ key value => map_put map key value
  | OpCompareAndSwap _ key new_value => map_put map key new_value
  end.

Fixpoint apply_ops
  (ops : list RecoveredOperation)
  (map : RefMap)
  : RefMap :=
  match ops with
  | [] => map
  | op :: rest => apply_ops rest (apply_op map op)
  end.

Definition apply_record
  (lsn : Lsn)
  (record : WalRecord)
  (map : RefMap)
  : RefMap :=
  apply_ops (ops_of_record lsn record) map.

Theorem insert_record_maps_to_insert :
  forall lsn key value,
    ops_of_record lsn (RecordInsert key value) =
      [OpInsert lsn key value].
Proof. reflexivity. Qed.

Theorem remove_record_maps_to_remove :
  forall lsn key,
    ops_of_record lsn (RecordRemove key) = [OpRemove lsn key].
Proof. reflexivity. Qed.

Theorem increment_record_maps_to_increment :
  forall lsn key delta result,
    ops_of_record lsn (RecordIncrement key delta result) =
      [OpIncrement lsn key delta result].
Proof. reflexivity. Qed.

Theorem upsert_record_maps_to_upsert :
  forall lsn key value,
    ops_of_record lsn (RecordUpsert key value) =
      [OpUpsert lsn key value].
Proof. reflexivity. Qed.

Theorem successful_cas_maps_to_cas :
  forall lsn key new_value,
    ops_of_record lsn (RecordCompareAndSwap key new_value true) =
      [OpCompareAndSwap lsn key new_value].
Proof. reflexivity. Qed.

Theorem failed_cas_maps_to_no_ops :
  forall lsn key new_value,
    ops_of_record lsn (RecordCompareAndSwap key new_value false) = [].
Proof. reflexivity. Qed.

Theorem batch_insert_expands_in_order :
  forall lsn entries,
    ops_of_record lsn (RecordBatchInsert entries) =
      map
        (fun entry =>
          let '(key, value) := entry in OpInsert lsn key value)
        entries.
Proof. reflexivity. Qed.

Theorem batch_increment_expands_in_order :
  forall lsn entries,
    ops_of_record lsn (RecordBatchIncrement entries) =
      map
        (fun entry =>
          let '(key, delta) := entry in OpIncrement lsn key delta 0)
        entries.
Proof. reflexivity. Qed.

Theorem checkpoint_maps_to_no_ops :
  forall lsn checkpoint_lsn,
    ops_of_record lsn (RecordCheckpoint checkpoint_lsn) = [].
Proof. reflexivity. Qed.

Theorem tx_control_maps_to_no_ops :
  forall lsn tx_id,
    ops_of_record lsn (RecordBeginTx tx_id) = [] /\
    ops_of_record lsn (RecordCommitTx tx_id) = [] /\
    ops_of_record lsn (RecordAbortTx tx_id) = [].
Proof.
  intros lsn tx_id.
  repeat split; reflexivity.
Qed.

Theorem version_records_map_to_no_ops :
  forall lsn,
    ops_of_record lsn RecordVersionUpdate = [] /\
    ops_of_record lsn RecordVersionDurable = [] /\
    ops_of_record lsn RecordVersionGc = [].
Proof.
  intros lsn.
  repeat split; reflexivity.
Qed.

Theorem apply_insert_record_lookup_same :
  forall map lsn key value,
    lookup (apply_record lsn (RecordInsert key (Some value)) map) key =
      Some value.
Proof.
  intros map lsn key value.
  simpl.
  apply map_put_lookup_same.
Qed.

Theorem apply_remove_record_lookup_same :
  forall map lsn key,
    lookup (apply_record lsn (RecordRemove key) map) key = None.
Proof.
  intros map lsn key.
  simpl.
  apply map_remove_lookup_same.
Qed.

Theorem apply_increment_record_lookup_same :
  forall map lsn key delta result,
    result <> 0 ->
    lookup (apply_record lsn (RecordIncrement key delta result) map) key =
      Some result.
Proof.
  intros map lsn key delta result Hnonzero.
  cbn [apply_record apply_ops ops_of_record apply_op apply_increment_result].
  unfold apply_increment_result.
  destruct (result =? 0) eqn:Hzero.
  - apply Nat.eqb_eq in Hzero. contradiction.
  - apply map_put_lookup_same.
Qed.

Theorem apply_batch_increment_recomputes_lookup_same :
  forall map lsn key delta,
    lookup (apply_record lsn (RecordBatchIncrement [(key, delta)]) map) key =
      Some (option_value (lookup map key) + delta).
Proof.
  intros map lsn key delta.
  simpl.
  apply map_increment_lookup_same.
Qed.

Theorem apply_successful_cas_lookup_same :
  forall map lsn key new_value,
    lookup
      (apply_record lsn (RecordCompareAndSwap key new_value true) map)
      key =
      Some new_value.
Proof.
  intros map lsn key new_value.
  simpl.
  apply map_put_lookup_same.
Qed.

Theorem apply_failed_cas_preserves_lookup :
  forall map lsn key new_value query,
    lookup
      (apply_record lsn (RecordCompareAndSwap key new_value false) map)
      query =
      lookup map query.
Proof. reflexivity. Qed.

Inductive RecoveryEndpoint : Type :=
| ByteOpen
| ByteCorruptionRebuild
| ByteIoUringOpen
| CharOpen
| CharArchiveRecovery
| CharRecoveryManager
| IncrementalReplay.

Definition endpoint_ops
  (_endpoint : RecoveryEndpoint)
  (lsn : Lsn)
  (record : WalRecord)
  : list RecoveredOperation :=
  ops_of_record lsn record.

Theorem endpoint_replay_mapping_parity :
  forall endpoint_a endpoint_b lsn record,
    endpoint_ops endpoint_a lsn record =
    endpoint_ops endpoint_b lsn record.
Proof. reflexivity. Qed.

Theorem char_archive_replays_batch_increment :
  forall lsn key delta,
    endpoint_ops CharArchiveRecovery lsn
      (RecordBatchIncrement [(key, delta)]) =
      [OpIncrement lsn key delta 0].
Proof. reflexivity. Qed.

Theorem byte_rebuild_replays_successful_cas :
  forall lsn key new_value,
    endpoint_ops ByteCorruptionRebuild lsn
      (RecordCompareAndSwap key new_value true) =
      [OpCompareAndSwap lsn key new_value].
Proof. reflexivity. Qed.

Inductive ScanItem : Type :=
| DurableRecord : Lsn -> WalRecord -> ScanItem
| CorruptRecord : ScanItem.

Definition scan_of_entry (entry : Lsn * WalRecord) : ScanItem :=
  let '(lsn, record) := entry in DurableRecord lsn record.

Fixpoint durable_prefix (scan : list ScanItem) : list (Lsn * WalRecord) :=
  match scan with
  | [] => []
  | DurableRecord lsn record :: rest =>
      (lsn, record) :: durable_prefix rest
  | CorruptRecord :: _ => []
  end.

Definition replay_entry (entry : Lsn * WalRecord) (map : RefMap) : RefMap :=
  let '(lsn, record) := entry in apply_record lsn record map.

Fixpoint replay_entries
  (entries : list (Lsn * WalRecord))
  (map : RefMap)
  : RefMap :=
  match entries with
  | [] => map
  | entry :: rest => replay_entries rest (replay_entry entry map)
  end.

Definition replay_scan (scan : list ScanItem) (map : RefMap) : RefMap :=
  replay_entries (durable_prefix scan) map.

Theorem durable_prefix_stops_at_corruption :
  forall prefix suffix,
    durable_prefix (map scan_of_entry prefix ++ CorruptRecord :: suffix) =
      prefix.
Proof.
  induction prefix as [| [lsn record] rest IH]; intros suffix.
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Theorem replay_scan_ignores_corrupt_suffix :
  forall prefix suffix state_map,
    replay_scan (map scan_of_entry prefix ++ CorruptRecord :: suffix) state_map =
      replay_entries prefix state_map.
Proof.
  intros prefix suffix state_map.
  unfold replay_scan.
  rewrite durable_prefix_stops_at_corruption.
  reflexivity.
Qed.

Record ReplayState : Type := mkReplayState {
  replay_map : RefMap;
  replay_echo_wal : list WalRecord
}.

Definition apply_op_no_wal
  (state : ReplayState)
  (op : RecoveredOperation)
  : ReplayState :=
  mkReplayState
    (apply_op (replay_map state) op)
    (replay_echo_wal state).

Fixpoint apply_ops_no_wal
  (ops : list RecoveredOperation)
  (state : ReplayState)
  : ReplayState :=
  match ops with
  | [] => state
  | op :: rest => apply_ops_no_wal rest (apply_op_no_wal state op)
  end.

Theorem apply_op_no_wal_preserves_echo_wal :
  forall state op,
    replay_echo_wal (apply_op_no_wal state op) = replay_echo_wal state.
Proof. reflexivity. Qed.

Theorem apply_ops_no_wal_preserves_echo_wal :
  forall ops state,
    replay_echo_wal (apply_ops_no_wal ops state) =
      replay_echo_wal state.
Proof.
  induction ops as [| op rest IH]; intros state.
  - reflexivity.
  - simpl. rewrite IH. reflexivity.
Qed.

Definition replay_record_no_wal
  (state : ReplayState)
  (lsn : Lsn)
  (record : WalRecord)
  : ReplayState :=
  apply_ops_no_wal (ops_of_record lsn record) state.

Theorem replay_record_no_wal_preserves_echo_wal :
  forall state lsn record,
    replay_echo_wal (replay_record_no_wal state lsn record) =
      replay_echo_wal state.
Proof.
  intros state lsn record.
  unfold replay_record_no_wal.
  apply apply_ops_no_wal_preserves_echo_wal.
Qed.
