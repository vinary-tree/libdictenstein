(** * ByzantineRecovery: Bounded Fault-Filtering Model

    This file models the practical Byzantine scope used by the implementation
    correspondence: storage or WAL records may be dropped, duplicated, or
    corrupted, but recovery only applies records that are both committed and
    authenticated.
*)

Require Import Coq.Lists.List.
Require Import Coq.Bool.Bool.
Require Import Coq.Arith.Arith.
Import ListNotations.

Record FaultRecord := mkFaultRecord {
  record_lsn : nat;
  record_committed : bool;
  record_authenticated : bool;
  record_key_id : nat;
  record_value_id : nat
}.

Definition record_recoverable (r : FaultRecord) : bool :=
  record_committed r && record_authenticated r.

Definition recovered_log (records : list FaultRecord) : list FaultRecord :=
  filter record_recoverable records.

Definition committed (r : FaultRecord) : Prop :=
  record_committed r = true.

Definition authenticated (r : FaultRecord) : Prop :=
  record_authenticated r = true.

Theorem recovered_records_are_committed_and_authenticated :
  forall records r,
    In r (recovered_log records) ->
    committed r /\ authenticated r.
Proof.
  intros records r Hin.
  unfold recovered_log in Hin.
  apply filter_In in Hin.
  destruct Hin as [_ Hrecoverable].
  unfold record_recoverable in Hrecoverable.
  apply andb_true_iff in Hrecoverable.
  destruct Hrecoverable as [Hcommitted Hauthenticated].
  split; assumption.
Qed.

Theorem unauthenticated_records_fail_closed :
  forall records r,
    record_authenticated r = false ->
    ~ In r (recovered_log records).
Proof.
  intros records r Hbad Hin.
  apply recovered_records_are_committed_and_authenticated in Hin.
  destruct Hin as [_ Hauthenticated].
  unfold authenticated in Hauthenticated.
  rewrite Hbad in Hauthenticated. discriminate.
Qed.

Theorem uncommitted_records_fail_closed :
  forall records r,
    record_committed r = false ->
    ~ In r (recovered_log records).
Proof.
  intros records r Hbad Hin.
  apply recovered_records_are_committed_and_authenticated in Hin.
  destruct Hin as [Hcommitted _].
  unfold committed in Hcommitted.
  rewrite Hbad in Hcommitted. discriminate.
Qed.

Definition duplicate_record (r : FaultRecord) (records : list FaultRecord)
  : list FaultRecord :=
  r :: r :: records.

Theorem duplicated_bad_record_still_fails_closed :
  forall records r,
    record_authenticated r = false \/ record_committed r = false ->
    ~ In r (recovered_log (duplicate_record r records)).
Proof.
  intros records r Hbad.
  destruct Hbad as [Hauth | Hcommit].
  - apply unauthenticated_records_fail_closed. exact Hauth.
  - apply uncommitted_records_fail_closed. exact Hcommit.
Qed.

Definition corrupt_authentication (r : FaultRecord) : FaultRecord :=
  mkFaultRecord
    (record_lsn r)
    (record_committed r)
    false
    (record_key_id r)
    (record_value_id r).

Theorem corrupted_record_is_not_recovered :
  forall records r,
    ~ In (corrupt_authentication r)
      (recovered_log (corrupt_authentication r :: records)).
Proof.
  intros records r.
  apply unauthenticated_records_fail_closed.
  reflexivity.
Qed.
