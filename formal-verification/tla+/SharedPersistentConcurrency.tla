-------------------- MODULE SharedPersistentConcurrency --------------------
(****************************************************************************)
(* Bounded model for Arc<RwLock<...>> persistent public APIs.                *)
(*                                                                          *)
(* Scope: public shared writes acquire the write lock until WAL-before-      *)
(* visible publication is complete; public checkpoint acquires the same      *)
(* exclusive lock until snapshot publication, checkpoint WAL sync, and WAL   *)
(* truncation are complete; reads observe only completed visible states;     *)
(* sync does not publish checkpoint state.                                   *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Writers, Terms, None, MaxLSN

VARIABLES nextLsn, phase, opTerm, opLsn,
          visible, termLsn, walTerms, syncedLsn, walRetainedFrom,
          durableCheckpoint, checkpointLsn, dirty, lock,
          ckptPhase, ckptSnapshot, ckptTarget, ckptPrevDirty,
          readerObserved, readerObservedLsn, readerFresh,
          recovered, recoveryFresh

Vars == <<nextLsn, phase, opTerm, opLsn,
          visible, termLsn, walTerms, syncedLsn, walRetainedFrom,
          durableCheckpoint, checkpointLsn, dirty, lock,
          ckptPhase, ckptSnapshot, ckptTarget, ckptPrevDirty,
          readerObserved, readerObservedLsn, readerFresh,
          recovered, recoveryFresh>>

Lsns == 1..MaxLSN
LsnOrNone == Lsns \cup {None}
WriterPhases == {"Idle", "Writing"}
LockStates == {"Free", "Writer", "Checkpoint"}
CheckpointPhases == {"Idle", "Snapshotted"}
EmptyLsnMap == [k \in Terms |-> None]
EmptyWal == [l \in Lsns |-> None]
EmptyBoolMap == [k \in Terms |-> FALSE]

MaxNat(a, b) == IF a >= b THEN a ELSE b

MaxVisibleLsn(m, lsns) ==
    CHOOSE n \in 0..MaxLSN :
        /\ \A k \in Terms : m[k] # None => lsns[k] # None /\ lsns[k] <= n
        /\ n = 0 \/ \E k \in Terms : m[k] # None /\ lsns[k] = n

RecoveredMap ==
    [k \in Terms |->
        IF durableCheckpoint[k] # None
        THEN durableCheckpoint[k]
        ELSE IF /\ termLsn[k] # None
                /\ termLsn[k] >= walRetainedFrom
                /\ termLsn[k] <= syncedLsn
        THEN visible[k]
        ELSE None]

TypeInvariant ==
    /\ None \notin Terms
    /\ None \notin Lsns
    /\ nextLsn \in 1..(MaxLSN + 1)
    /\ phase \in [Writers -> WriterPhases]
    /\ opTerm \in [Writers -> Terms \cup {None}]
    /\ opLsn \in [Writers -> LsnOrNone]
    /\ visible \in [Terms -> LsnOrNone]
    /\ termLsn \in [Terms -> LsnOrNone]
    /\ walTerms \in [Lsns -> Terms \cup {None}]
    /\ syncedLsn \in 0..MaxLSN
    /\ walRetainedFrom \in 1..(MaxLSN + 1)
    /\ durableCheckpoint \in [Terms -> LsnOrNone]
    /\ checkpointLsn \in 0..MaxLSN
    /\ dirty \in BOOLEAN
    /\ lock \in LockStates
    /\ ckptPhase \in CheckpointPhases
    /\ ckptSnapshot \in [Terms -> LsnOrNone]
    /\ ckptTarget \in 0..MaxLSN
    /\ ckptPrevDirty \in BOOLEAN
    /\ readerObserved \in [Terms -> LsnOrNone]
    /\ readerObservedLsn \in [Terms -> LsnOrNone]
    /\ readerFresh \in [Terms -> BOOLEAN]
    /\ recovered \in [Terms -> LsnOrNone]
    /\ recoveryFresh \in BOOLEAN

Init ==
    /\ nextLsn = 1
    /\ phase = [w \in Writers |-> "Idle"]
    /\ opTerm = [w \in Writers |-> None]
    /\ opLsn = [w \in Writers |-> None]
    /\ visible = EmptyLsnMap
    /\ termLsn = EmptyLsnMap
    /\ walTerms = EmptyWal
    /\ syncedLsn = 0
    /\ walRetainedFrom = 1
    /\ durableCheckpoint = EmptyLsnMap
    /\ checkpointLsn = 0
    /\ dirty = FALSE
    /\ lock = "Free"
    /\ ckptPhase = "Idle"
    /\ ckptSnapshot = EmptyLsnMap
    /\ ckptTarget = 0
    /\ ckptPrevDirty = FALSE
    /\ readerObserved = EmptyLsnMap
    /\ readerObservedLsn = EmptyLsnMap
    /\ readerFresh = EmptyBoolMap
    /\ recovered = EmptyLsnMap
    /\ recoveryFresh = FALSE

StartWrite(w, k) ==
    /\ w \in Writers
    /\ k \in Terms
    /\ lock = "Free"
    /\ phase[w] = "Idle"
    /\ visible[k] = None
    /\ nextLsn <= MaxLSN
    /\ lock' = "Writer"
    /\ phase' = [phase EXCEPT ![w] = "Writing"]
    /\ opTerm' = [opTerm EXCEPT ![w] = k]
    /\ opLsn' = [opLsn EXCEPT ![w] = nextLsn]
    /\ nextLsn' = nextLsn + 1
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<visible, termLsn, walTerms, syncedLsn, walRetainedFrom,
                  durableCheckpoint, checkpointLsn, dirty, ckptPhase,
                  ckptSnapshot, ckptTarget, ckptPrevDirty,
                  readerObserved, readerObservedLsn, readerFresh, recovered>>

FinishWriteSuccess(w) ==
    /\ w \in Writers
    /\ lock = "Writer"
    /\ phase[w] = "Writing"
    /\ opTerm[w] \in Terms
    /\ opLsn[w] \in Lsns
    /\ visible[opTerm[w]] = None
    /\ walTerms' = [walTerms EXCEPT ![opLsn[w]] = opTerm[w]]
    /\ termLsn' = [termLsn EXCEPT ![opTerm[w]] = opLsn[w]]
    /\ visible' = [visible EXCEPT ![opTerm[w]] = opLsn[w]]
    /\ syncedLsn' = MaxNat(syncedLsn, opLsn[w])
    /\ dirty' = TRUE
    /\ lock' = "Free"
    /\ phase' = [phase EXCEPT ![w] = "Idle"]
    /\ opTerm' = [opTerm EXCEPT ![w] = None]
    /\ opLsn' = [opLsn EXCEPT ![w] = None]
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, walRetainedFrom, durableCheckpoint,
                  checkpointLsn, ckptPhase, ckptSnapshot, ckptTarget,
                  ckptPrevDirty, readerObserved, readerObservedLsn,
                  readerFresh, recovered>>

FinishWriteFailure(w) ==
    /\ w \in Writers
    /\ lock = "Writer"
    /\ phase[w] = "Writing"
    /\ lock' = "Free"
    /\ phase' = [phase EXCEPT ![w] = "Idle"]
    /\ opTerm' = [opTerm EXCEPT ![w] = None]
    /\ opLsn' = [opLsn EXCEPT ![w] = None]
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, visible, termLsn, walTerms, syncedLsn,
                  walRetainedFrom, durableCheckpoint, checkpointLsn, dirty,
                  ckptPhase, ckptSnapshot, ckptTarget, ckptPrevDirty,
                  readerObserved, readerObservedLsn, readerFresh, recovered>>

ReadTerm(k) ==
    /\ k \in Terms
    /\ lock = "Free"
    /\ readerObserved' = [readerObserved EXCEPT ![k] = visible[k]]
    /\ readerObservedLsn' = [readerObservedLsn EXCEPT ![k] = termLsn[k]]
    /\ readerFresh' = [readerFresh EXCEPT ![k] = TRUE]
    /\ UNCHANGED <<nextLsn, phase, opTerm, opLsn, visible, termLsn, walTerms,
                  syncedLsn, walRetainedFrom, durableCheckpoint,
                  checkpointLsn, dirty, lock, ckptPhase, ckptSnapshot,
                  ckptTarget, ckptPrevDirty, recovered, recoveryFresh>>

StartCheckpoint ==
    /\ lock = "Free"
    /\ ckptPhase = "Idle"
    /\ \A w \in Writers : phase[w] = "Idle"
    /\ lock' = "Checkpoint"
    /\ ckptPhase' = "Snapshotted"
    /\ ckptSnapshot' = visible
    /\ ckptTarget' = MaxVisibleLsn(visible, termLsn)
    /\ ckptPrevDirty' = dirty
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, phase, opTerm, opLsn, visible, termLsn, walTerms,
                  syncedLsn, walRetainedFrom, durableCheckpoint,
                  checkpointLsn, dirty, readerObserved, readerObservedLsn,
                  readerFresh, recovered>>

FinishCheckpointSuccess ==
    /\ lock = "Checkpoint"
    /\ ckptPhase = "Snapshotted"
    /\ durableCheckpoint' = ckptSnapshot
    /\ checkpointLsn' = MaxNat(checkpointLsn, ckptTarget)
    /\ walRetainedFrom' = MaxNat(walRetainedFrom, ckptTarget + 1)
    /\ syncedLsn' = MaxNat(syncedLsn, ckptTarget)
    /\ dirty' = FALSE
    /\ lock' = "Free"
    /\ ckptPhase' = "Idle"
    /\ ckptSnapshot' = EmptyLsnMap
    /\ ckptTarget' = 0
    /\ ckptPrevDirty' = FALSE
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, phase, opTerm, opLsn, visible, termLsn, walTerms,
                  readerObserved, readerObservedLsn, readerFresh, recovered>>

FinishCheckpointFailure ==
    /\ lock = "Checkpoint"
    /\ ckptPhase = "Snapshotted"
    /\ dirty' = ckptPrevDirty
    /\ lock' = "Free"
    /\ ckptPhase' = "Idle"
    /\ ckptSnapshot' = EmptyLsnMap
    /\ ckptTarget' = 0
    /\ ckptPrevDirty' = FALSE
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, phase, opTerm, opLsn, visible, termLsn, walTerms,
                  syncedLsn, walRetainedFrom, durableCheckpoint,
                  checkpointLsn, readerObserved, readerObservedLsn,
                  readerFresh, recovered>>

SyncOnly ==
    /\ lock = "Free"
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, phase, opTerm, opLsn, visible, termLsn, walTerms,
                  syncedLsn, walRetainedFrom, durableCheckpoint,
                  checkpointLsn, dirty, lock, ckptPhase, ckptSnapshot,
                  ckptTarget, ckptPrevDirty, readerObserved,
                  readerObservedLsn, readerFresh, recovered>>

CrashRecover ==
    /\ recovered' = RecoveredMap
    /\ recoveryFresh' = TRUE
    /\ UNCHANGED <<nextLsn, phase, opTerm, opLsn, visible, termLsn, walTerms,
                  syncedLsn, walRetainedFrom, durableCheckpoint,
                  checkpointLsn, dirty, lock, ckptPhase, ckptSnapshot,
                  ckptTarget, ckptPrevDirty, readerObserved,
                  readerObservedLsn, readerFresh>>

Next ==
    \/ \E w \in Writers, k \in Terms : StartWrite(w, k)
    \/ \E w \in Writers : FinishWriteSuccess(w)
    \/ \E w \in Writers : FinishWriteFailure(w)
    \/ \E k \in Terms : ReadTerm(k)
    \/ StartCheckpoint
    \/ FinishCheckpointSuccess
    \/ FinishCheckpointFailure
    \/ SyncOnly
    \/ CrashRecover

WalBeforeVisible ==
    \A k \in Terms :
        visible[k] # None =>
            /\ termLsn[k] # None
            /\ walTerms[termLsn[k]] = k
            /\ termLsn[k] <= syncedLsn

CheckpointSnapshotIsVisible ==
    \A k \in Terms :
        durableCheckpoint[k] # None => durableCheckpoint[k] = visible[k]

CheckpointLsnCoversSnapshot ==
    \A k \in Terms :
        durableCheckpoint[k] # None => durableCheckpoint[k] <= checkpointLsn

TruncationKeepsUncheckpointedVisibleWal ==
    \A k \in Terms :
        /\ visible[k] # None
        /\ durableCheckpoint[k] # visible[k]
        => termLsn[k] >= walRetainedFrom

DirtyFalseMeansCheckpointCoversVisible ==
    dirty = FALSE => durableCheckpoint = visible

NoWriteWhileCheckpointHeld ==
    lock = "Checkpoint" => \A w \in Writers : phase[w] = "Idle"

NoCheckpointWhileWriteHeld ==
    lock = "Writer" => ckptPhase = "Idle"

WriterLockHasOneOwner ==
    lock = "Writer" =>
        Cardinality({w \in Writers : phase[w] = "Writing"}) = 1

IdleWritersHaveNoReservedOperation ==
    \A w \in Writers :
        phase[w] = "Idle" =>
            /\ opTerm[w] = None
            /\ opLsn[w] = None

ReadsObserveCompletedVisibleState ==
    \A k \in Terms :
        /\ readerFresh[k]
        /\ readerObserved[k] # None
        =>
            /\ readerObserved[k] = visible[k]
            /\ readerObservedLsn[k] = termLsn[k]

RecoveryFreshMatchesVisible ==
    recoveryFresh => recovered = visible

RecoveredNeverInventsVisibleState ==
    \A k \in Terms : recovered[k] # None => recovered[k] = visible[k]

Spec == Init /\ [][Next]_Vars

=============================================================================
