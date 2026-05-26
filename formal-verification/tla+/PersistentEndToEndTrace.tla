----------------------- MODULE PersistentEndToEndTrace -----------------------
(***************************************************************************)
(* End-to-end public persistence trace model.                               *)
(*                                                                         *)
(* Scope: successful public mutations are acknowledged only after their WAL *)
(* records are durable, checkpoint and compaction publish the current live  *)
(* map as the new durable snapshot, and crash/reopen reconstructs the live  *)
(* map by replaying the retained WAL tail over that snapshot.               *)
(***************************************************************************)

EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS Keys, MaxLSN

VARIABLES mem, checkpoint, wal, nextLsn, syncedLsn, checkpointLsn, reopened

Vars == <<mem, checkpoint, wal, nextLsn, syncedLsn, checkpointLsn, reopened>>

WalRecord == [key : Keys, present : BOOLEAN, lsn : 1..MaxLSN]

ApplyRecord(state, record) ==
    IF record.present
    THEN state \cup {record.key}
    ELSE state \ {record.key}

RECURSIVE Replay(_, _)
Replay(seq, state) ==
    IF Len(seq) = 0
    THEN state
    ELSE Replay(Tail(seq), ApplyRecord(state, Head(seq)))

TypeInvariant ==
    /\ Keys # {}
    /\ mem \subseteq Keys
    /\ checkpoint \subseteq Keys
    /\ reopened \subseteq Keys
    /\ wal \in Seq(WalRecord)
    /\ nextLsn \in 1..(MaxLSN + 1)
    /\ syncedLsn \in 0..MaxLSN
    /\ checkpointLsn \in 0..MaxLSN

Init ==
    /\ mem = {}
    /\ checkpoint = {}
    /\ wal = <<>>
    /\ nextLsn = 1
    /\ syncedLsn = 0
    /\ checkpointLsn = 0
    /\ reopened = {}

Put(k) ==
    /\ k \in Keys
    /\ nextLsn <= MaxLSN
    /\ LET newMem == mem \cup {k} IN
       /\ mem' = newMem
       /\ wal' = Append(wal, [key |-> k, present |-> TRUE, lsn |-> nextLsn])
       /\ nextLsn' = nextLsn + 1
       /\ syncedLsn' = nextLsn
       /\ reopened' = newMem
    /\ UNCHANGED <<checkpoint, checkpointLsn>>

Remove(k) ==
    /\ k \in Keys
    /\ nextLsn <= MaxLSN
    /\ LET newMem == mem \ {k} IN
       /\ mem' = newMem
       /\ wal' = Append(wal, [key |-> k, present |-> FALSE, lsn |-> nextLsn])
       /\ nextLsn' = nextLsn + 1
       /\ syncedLsn' = nextLsn
       /\ reopened' = newMem
    /\ UNCHANGED <<checkpoint, checkpointLsn>>

Checkpoint ==
    /\ checkpoint' = mem
    /\ checkpointLsn' = syncedLsn
    /\ wal' = <<>>
    /\ reopened' = mem
    /\ UNCHANGED <<mem, nextLsn, syncedLsn>>

CompactRewrite ==
    /\ checkpoint' = mem
    /\ checkpointLsn' = syncedLsn
    /\ wal' = <<>>
    /\ reopened' = mem
    /\ UNCHANGED <<mem, nextLsn, syncedLsn>>

CrashReopen ==
    /\ LET recovered == Replay(wal, checkpoint) IN
       /\ mem' = recovered
       /\ reopened' = recovered
    /\ UNCHANGED <<checkpoint, wal, nextLsn, syncedLsn, checkpointLsn>>

Next ==
    \/ \E k \in Keys : Put(k)
    \/ \E k \in Keys : Remove(k)
    \/ Checkpoint
    \/ CompactRewrite
    \/ CrashReopen

WalLsnContiguous ==
    \A i \in DOMAIN wal :
        wal[i].lsn = checkpointLsn + i

WalTailMatchesFrontier ==
    Len(wal) = syncedLsn - checkpointLsn

CheckpointWithinSynced ==
    checkpointLsn <= syncedLsn

SyncedTracksNextLsn ==
    syncedLsn + 1 = nextLsn

ReopenEqualsCheckpointPlusTail ==
    reopened = Replay(wal, checkpoint)

MemEqualsRecoverableState ==
    mem = Replay(wal, checkpoint)

Spec == Init /\ [][Next]_Vars

=============================================================================
