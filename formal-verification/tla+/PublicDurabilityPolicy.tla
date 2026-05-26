-------------------- MODULE PublicDurabilityPolicy --------------------
(***************************************************************************)
(* Public durability acknowledgement model.                                *)
(*                                                                         *)
(* Scope: successful public mutations append one WAL record and then return *)
(* an acknowledgement. Under full durability policies (Immediate and        *)
(* GroupCommit), that acknowledgement is valid only if the appended LSN is  *)
(* covered by the synced frontier. Periodic and None may acknowledge a      *)
(* visible mutation without advancing the synced frontier.                  *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANT MaxLSN

VARIABLES nextLsn, policy, wal, visible, syncedLsn, ackLsn, ackPolicy,
          asyncTarget, asyncDone, checkpointLsn, recoveredPrefix

Vars == <<nextLsn, policy, wal, visible, syncedLsn, ackLsn, ackPolicy,
          asyncTarget, asyncDone, checkpointLsn, recoveredPrefix>>

Policies == {"Immediate", "GroupCommit", "Periodic", "None"}
FullPolicies == {"Immediate", "GroupCommit"}
AckPolicies == Policies \cup {"NoAck"}
Lsns == 1..MaxLSN
LsnOrZero == 0..MaxLSN
Tail == nextLsn - 1

MaxNat(a, b) == IF a >= b THEN a ELSE b
Prefix(n) == {l \in Lsns : l <= n}

TypeInvariant ==
    /\ nextLsn \in 1..(MaxLSN + 1)
    /\ policy \in Policies
    /\ wal \subseteq Lsns
    /\ visible \subseteq Lsns
    /\ syncedLsn \in LsnOrZero
    /\ ackLsn \in LsnOrZero
    /\ ackPolicy \in AckPolicies
    /\ asyncTarget \in LsnOrZero
    /\ asyncDone \in BOOLEAN
    /\ checkpointLsn \in LsnOrZero
    /\ recoveredPrefix \subseteq Lsns

Init ==
    /\ nextLsn = 1
    /\ policy = "Immediate"
    /\ wal = {}
    /\ visible = {}
    /\ syncedLsn = 0
    /\ ackLsn = 0
    /\ ackPolicy = "NoAck"
    /\ asyncTarget = 0
    /\ asyncDone = TRUE
    /\ checkpointLsn = 0
    /\ recoveredPrefix = {}

SetPolicy(p) ==
    /\ p \in Policies
    /\ policy' = p
    /\ UNCHANGED <<nextLsn, wal, visible, syncedLsn, ackLsn, ackPolicy,
                  asyncTarget, asyncDone, checkpointLsn, recoveredPrefix>>

AppendAndAckFull ==
    /\ policy \in FullPolicies
    /\ nextLsn <= MaxLSN
    /\ LET l == nextLsn IN
       /\ wal' = wal \cup {l}
       /\ visible' = visible \cup {l}
       /\ syncedLsn' = MaxNat(syncedLsn, l)
       /\ ackLsn' = l
       /\ ackPolicy' = policy
       /\ nextLsn' = l + 1
    /\ asyncDone' = FALSE
    /\ UNCHANGED <<policy, asyncTarget, checkpointLsn, recoveredPrefix>>

AppendAndAckWeak ==
    /\ policy \notin FullPolicies
    /\ nextLsn <= MaxLSN
    /\ LET l == nextLsn IN
       /\ wal' = wal \cup {l}
       /\ visible' = visible \cup {l}
       /\ syncedLsn' = syncedLsn
       /\ ackLsn' = l
       /\ ackPolicy' = policy
       /\ nextLsn' = l + 1
    /\ asyncDone' = FALSE
    /\ UNCHANGED <<policy, asyncTarget, checkpointLsn, recoveredPrefix>>

BlockingSync ==
    /\ syncedLsn' = MaxNat(syncedLsn, Tail)
    /\ asyncDone' = TRUE
    /\ UNCHANGED <<nextLsn, policy, wal, visible, ackLsn, ackPolicy,
                  asyncTarget, checkpointLsn, recoveredPrefix>>

StartAsyncSync ==
    /\ asyncTarget' = Tail
    /\ asyncDone' = FALSE
    /\ UNCHANGED <<nextLsn, policy, wal, visible, syncedLsn, ackLsn,
                  ackPolicy, checkpointLsn, recoveredPrefix>>

FinishAsyncSync ==
    /\ syncedLsn' = MaxNat(syncedLsn, asyncTarget)
    /\ asyncDone' = TRUE
    /\ UNCHANGED <<nextLsn, policy, wal, visible, ackLsn, ackPolicy,
                  asyncTarget, checkpointLsn, recoveredPrefix>>

Checkpoint ==
    /\ checkpointLsn' = syncedLsn
    /\ UNCHANGED <<nextLsn, policy, wal, visible, syncedLsn, ackLsn,
                  ackPolicy, asyncTarget, asyncDone, recoveredPrefix>>

Recover ==
    /\ recoveredPrefix' = wal \cap Prefix(syncedLsn)
    /\ UNCHANGED <<nextLsn, policy, wal, visible, syncedLsn, ackLsn,
                  ackPolicy, asyncTarget, asyncDone, checkpointLsn>>

Next ==
    \/ \E p \in Policies : SetPolicy(p)
    \/ AppendAndAckFull
    \/ AppendAndAckWeak
    \/ BlockingSync
    \/ StartAsyncSync
    \/ FinishAsyncSync
    \/ Checkpoint
    \/ Recover

WalPrefixInvariant ==
    \A l \in wal : \A p \in Lsns : p <= l => p \in wal

VisibleWithinWal ==
    visible \subseteq wal

SyncedWithinCurrentTail ==
    syncedLsn <= Tail

SyncedPrefixInWal ==
    \A l \in Lsns : l <= syncedLsn => l \in wal

FullPolicyAckIsDurable ==
    ackPolicy \in FullPolicies /\ ackLsn # 0 => ackLsn <= syncedLsn

AsyncDoneImpliesTargetDurable ==
    asyncDone => asyncTarget <= syncedLsn

CheckpointWithinSyncedFrontier ==
    checkpointLsn <= syncedLsn

RecoveryUsesOnlySyncedPrefix ==
    recoveredPrefix \subseteq (wal \cap Prefix(syncedLsn))

Spec == Init /\ [][Next]_Vars

=============================================================================
