-------------------------- MODULE DurabilityFrontier --------------------------
(****************************************************************************)
(* Bounded durability publication and reclamation model.                     *)
(*                                                                          *)
(* Scope: LSNs are reserved before group-commit publication, individual WAL  *)
(* completions may happen out of order, but the published synced frontier is *)
(* always prefix-closed. Checkpoints and recovery may only depend on that     *)
(* frontier. Version reclamation requires both no active readers and a        *)
(* durable VersionGc decision.                                               *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Threads, Readers, Versions, MaxLSN, NoVersion

VARIABLES nextLsn, queued, durable, syncedLsn, waiterTarget, waiterDone,
          checkpointLsn, recovered, versionState, readerVersion,
          gcQueued, gcDurable

Vars == <<nextLsn, queued, durable, syncedLsn, waiterTarget, waiterDone,
          checkpointLsn, recovered, versionState, readerVersion,
          gcQueued, gcDurable>>

LsnRange == 1..MaxLSN
VersionStates == {"Active", "Retired", "Reclaimed"}

PrefixSet(n) == {l \in LsnRange : l <= n}

MaxDurablePrefix(s) ==
    CHOOSE n \in 0..MaxLSN :
        /\ PrefixSet(n) \subseteq s
        /\ (n = MaxLSN \/ (n + 1) \notin s)

HasReader(v) == \E r \in Readers : readerVersion[r] = v

TypeInvariant ==
    /\ nextLsn \in 1..(MaxLSN + 1)
    /\ queued \in SUBSET LsnRange
    /\ durable \in SUBSET LsnRange
    /\ syncedLsn \in 0..MaxLSN
    /\ waiterTarget \in [Threads -> 0..MaxLSN]
    /\ waiterDone \in SUBSET Threads
    /\ checkpointLsn \in 0..MaxLSN
    /\ recovered \in SUBSET LsnRange
    /\ versionState \in [Versions -> VersionStates]
    /\ readerVersion \in [Readers -> Versions \cup {NoVersion}]
    /\ gcQueued \in SUBSET Versions
    /\ gcDurable \in SUBSET Versions

Init ==
    /\ nextLsn = 1
    /\ queued = {}
    /\ durable = {}
    /\ syncedLsn = 0
    /\ waiterTarget = [t \in Threads |-> 0]
    /\ waiterDone = {}
    /\ checkpointLsn = 0
    /\ recovered = {}
    /\ versionState = [v \in Versions |-> "Active"]
    /\ readerVersion = [r \in Readers |-> NoVersion]
    /\ gcQueued = {}
    /\ gcDurable = {}

ReserveQueuedLsn(t) ==
    /\ t \in Threads
    /\ waiterTarget[t] = 0
    /\ nextLsn <= MaxLSN
    /\ queued' = queued \cup {nextLsn}
    /\ waiterTarget' = [waiterTarget EXCEPT ![t] = nextLsn]
    /\ nextLsn' = nextLsn + 1
    /\ UNCHANGED <<durable, syncedLsn, waiterDone, checkpointLsn, recovered,
                  versionState, readerVersion, gcQueued, gcDurable>>

CompleteOneLsn(l) ==
    /\ l \in LsnRange
    /\ l < nextLsn
    /\ l \notin durable
    /\ durable' = durable \cup {l}
    /\ queued' = queued \ {l}
    /\ UNCHANGED <<nextLsn, syncedLsn, waiterTarget, waiterDone,
                  checkpointLsn, recovered, versionState, readerVersion,
                  gcQueued, gcDurable>>

FsyncPrefix(n) ==
    /\ n \in LsnRange
    /\ n < nextLsn
    /\ PrefixSet(n) \subseteq queued \cup durable
    /\ durable' = durable \cup PrefixSet(n)
    /\ queued' = queued \ PrefixSet(n)
    /\ syncedLsn' = MaxDurablePrefix(durable')
    /\ waiterDone' =
        waiterDone \cup {t \in Threads :
            /\ waiterTarget[t] # 0
            /\ waiterTarget[t] <= syncedLsn'}
    /\ UNCHANGED <<nextLsn, waiterTarget, checkpointLsn, recovered,
                  versionState, readerVersion, gcQueued, gcDurable>>

PublishDurableFrontier ==
    /\ syncedLsn' = MaxDurablePrefix(durable)
    /\ waiterDone' =
        waiterDone \cup {t \in Threads :
            /\ waiterTarget[t] # 0
            /\ waiterTarget[t] <= syncedLsn'}
    /\ UNCHANGED <<nextLsn, queued, durable, waiterTarget, checkpointLsn,
                  recovered, versionState, readerVersion, gcQueued, gcDurable>>

PublishCheckpoint(n) ==
    /\ n \in 0..MaxLSN
    /\ checkpointLsn <= n
    /\ n <= syncedLsn
    /\ checkpointLsn' = n
    /\ UNCHANGED <<nextLsn, queued, durable, syncedLsn, waiterTarget,
                  waiterDone, recovered, versionState, readerVersion,
                  gcQueued, gcDurable>>

CrashRecover ==
    /\ recovered' = PrefixSet(syncedLsn)
    /\ UNCHANGED <<nextLsn, queued, durable, syncedLsn, waiterTarget,
                  waiterDone, checkpointLsn, versionState, readerVersion,
                  gcQueued, gcDurable>>

BeginRead(r, v) ==
    /\ r \in Readers
    /\ v \in Versions
    /\ readerVersion[r] = NoVersion
    /\ versionState[v] # "Reclaimed"
    /\ readerVersion' = [readerVersion EXCEPT ![r] = v]
    /\ UNCHANGED <<nextLsn, queued, durable, syncedLsn, waiterTarget,
                  waiterDone, checkpointLsn, recovered, versionState,
                  gcQueued, gcDurable>>

EndRead(r) ==
    /\ r \in Readers
    /\ readerVersion[r] # NoVersion
    /\ readerVersion' = [readerVersion EXCEPT ![r] = NoVersion]
    /\ UNCHANGED <<nextLsn, queued, durable, syncedLsn, waiterTarget,
                  waiterDone, checkpointLsn, recovered, versionState,
                  gcQueued, gcDurable>>

RetireVersion(v) ==
    /\ v \in Versions
    /\ versionState[v] = "Active"
    /\ versionState' = [versionState EXCEPT ![v] = "Retired"]
    /\ UNCHANGED <<nextLsn, queued, durable, syncedLsn, waiterTarget,
                  waiterDone, checkpointLsn, recovered, readerVersion,
                  gcQueued, gcDurable>>

QueueGcRecord(v) ==
    /\ v \in Versions
    /\ versionState[v] = "Retired"
    /\ ~HasReader(v)
    /\ gcQueued' = gcQueued \cup {v}
    /\ UNCHANGED <<nextLsn, queued, durable, syncedLsn, waiterTarget,
                  waiterDone, checkpointLsn, recovered, versionState,
                  readerVersion, gcDurable>>

DurableGcRecord(v) ==
    /\ v \in gcQueued
    /\ gcDurable' = gcDurable \cup {v}
    /\ gcQueued' = gcQueued \ {v}
    /\ UNCHANGED <<nextLsn, queued, durable, syncedLsn, waiterTarget,
                  waiterDone, checkpointLsn, recovered, versionState,
                  readerVersion>>

ReclaimVersion(v) ==
    /\ v \in Versions
    /\ versionState[v] = "Retired"
    /\ v \in gcDurable
    /\ ~HasReader(v)
    /\ versionState' = [versionState EXCEPT ![v] = "Reclaimed"]
    /\ UNCHANGED <<nextLsn, queued, durable, syncedLsn, waiterTarget,
                  waiterDone, checkpointLsn, recovered, readerVersion,
                  gcQueued, gcDurable>>

Next ==
    \/ \E t \in Threads : ReserveQueuedLsn(t)
    \/ \E l \in LsnRange : CompleteOneLsn(l)
    \/ \E n \in LsnRange : FsyncPrefix(n)
    \/ PublishDurableFrontier
    \/ \E n \in 0..MaxLSN : PublishCheckpoint(n)
    \/ CrashRecover
    \/ \E r \in Readers, v \in Versions : BeginRead(r, v)
    \/ \E r \in Readers : EndRead(r)
    \/ \E v \in Versions : RetireVersion(v)
    \/ \E v \in Versions : QueueGcRecord(v)
    \/ \E v \in Versions : DurableGcRecord(v)
    \/ \E v \in Versions : ReclaimVersion(v)

SyncedLsnIsDurablePrefix ==
    PrefixSet(syncedLsn) \subseteq durable

WaitersOnlyCompleteAfterDurability ==
    \A t \in waiterDone :
        /\ waiterTarget[t] # 0
        /\ waiterTarget[t] <= syncedLsn
        /\ PrefixSet(waiterTarget[t]) \subseteq durable

CheckpointWithinDurableFrontier ==
    /\ checkpointLsn <= syncedLsn
    /\ PrefixSet(checkpointLsn) \subseteq durable

RecoveryUsesOnlyDurableRecords ==
    recovered \subseteq durable

NoReaderReferencesReclaimedVersion ==
    \A r \in Readers :
        readerVersion[r] # NoVersion =>
            versionState[readerVersion[r]] # "Reclaimed"

ReclaimedVersionHasDurableGcRecord ==
    \A v \in Versions :
        versionState[v] = "Reclaimed" => v \in gcDurable

ReclaimedVersionHasNoReaders ==
    \A v \in Versions :
        versionState[v] = "Reclaimed" => ~HasReader(v)

DurableGcRecordsReferToRetiredOrReclaimedVersions ==
    \A v \in gcDurable :
        versionState[v] \in {"Retired", "Reclaimed"}

Spec == Init /\ [][Next]_Vars

=============================================================================
