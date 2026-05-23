------------------------- MODULE AsyncWalGroupCommit -------------------------
(****************************************************************************)
(* Bounded async WAL and group-commit model.                                *)
(*                                                                          *)
(* Scope: records are first buffered, callers join a group commit queue,    *)
(* and fsync atomically publishes all pending LSNs as durable.              *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Threads, MaxLSN

VARIABLES nextLsn, pending, durable, durableLsn, groupQueue

Vars == <<nextLsn, pending, durable, durableLsn, groupQueue>>

LsnRange == 1..MaxLSN

TypeInvariant ==
    /\ nextLsn \in 1..(MaxLSN + 1)
    /\ pending \in SUBSET LsnRange
    /\ durable \in SUBSET LsnRange
    /\ durableLsn \in 0..MaxLSN
    /\ groupQueue \in SUBSET Threads

Init ==
    /\ nextLsn = 1
    /\ pending = {}
    /\ durable = {}
    /\ durableLsn = 0
    /\ groupQueue = {}

AppendRecord(t) ==
    /\ t \in Threads
    /\ nextLsn <= MaxLSN
    /\ pending' = pending \cup {nextLsn}
    /\ groupQueue' = groupQueue \cup {t}
    /\ nextLsn' = nextLsn + 1
    /\ UNCHANGED <<durable, durableLsn>>

Fsync ==
    /\ pending # {}
    /\ durable' = durable \cup pending
    /\ durableLsn' = nextLsn - 1
    /\ pending' = {}
    /\ groupQueue' = {}
    /\ UNCHANGED nextLsn

Next ==
    \/ \E t \in Threads : AppendRecord(t)
    \/ Fsync

PendingRecordsAreNotDurable ==
    pending \cap durable = {}

DurablePrefixClosed ==
    \A lsn \in LsnRange :
        lsn <= durableLsn => lsn \in durable

GroupQueueImpliesPending ==
    groupQueue # {} => pending # {}

FsyncClearsWaiters ==
    pending = {} => groupQueue = {}

Spec == Init /\ [][Next]_Vars

=============================================================================
