-------------------- MODULE ConcurrentCheckpointPublication --------------------
(****************************************************************************)
(* Bounded model for concurrent mutation versus checkpoint publication.       *)
(*                                                                          *)
(* Scope: a public insert reserves a monotonically increasing LSN/index,      *)
(* records the WAL entry before becoming visible, and checkpoints acquire a   *)
(* publication gate before snapshotting and truncating WAL. The model checks  *)
(* that no visible mutation can be lost by a racing checkpoint and that sync  *)
(* or WAL rotation do not publish checkpoint state.                           *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Writers, Terms, None, MaxLSN, MaxIndex

VARIABLES nextLsn, nextIndex, phase, opTerm, opLsn, opIndex,
          visible, termLsn, walTerms, syncedLsn, walRetainedFrom,
          durableCheckpoint, checkpointLsn, dirty, gate,
          ckptPhase, ckptSnapshot, ckptTarget, ckptPrevDirty,
          recovered, recoveryFresh

Vars == <<nextLsn, nextIndex, phase, opTerm, opLsn, opIndex,
          visible, termLsn, walTerms, syncedLsn, walRetainedFrom,
          durableCheckpoint, checkpointLsn, dirty, gate,
          ckptPhase, ckptSnapshot, ckptTarget, ckptPrevDirty,
          recovered, recoveryFresh>>

Indexes == 0..MaxIndex
Lsns == 1..MaxLSN
IndexOrNone == Indexes \cup {None}
LsnOrNone == Lsns \cup {None}
InsertPhases == {"Idle", "Claimed"}
GateStates == {"Open", "Checkpoint"}
CheckpointPhases == {"Idle", "Snapshotted"}
EmptyIndexMap == [k \in Terms |-> None]
EmptyLsnMap == [k \in Terms |-> None]
EmptyWal == [l \in Lsns |-> None]

MaxNat(a, b) == IF a >= b THEN a ELSE b

CommittedTerms(m) == {k \in Terms : m[k] # None}

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
    /\ None \notin Indexes
    /\ None \notin Lsns
    /\ nextLsn \in 1..(MaxLSN + 1)
    /\ nextIndex \in 0..(MaxIndex + 1)
    /\ phase \in [Writers -> InsertPhases]
    /\ opTerm \in [Writers -> Terms \cup {None}]
    /\ opLsn \in [Writers -> LsnOrNone]
    /\ opIndex \in [Writers -> IndexOrNone]
    /\ visible \in [Terms -> IndexOrNone]
    /\ termLsn \in [Terms -> LsnOrNone]
    /\ walTerms \in [Lsns -> Terms \cup {None}]
    /\ syncedLsn \in 0..MaxLSN
    /\ walRetainedFrom \in 1..(MaxLSN + 1)
    /\ durableCheckpoint \in [Terms -> IndexOrNone]
    /\ checkpointLsn \in 0..MaxLSN
    /\ dirty \in BOOLEAN
    /\ gate \in GateStates
    /\ ckptPhase \in CheckpointPhases
    /\ ckptSnapshot \in [Terms -> IndexOrNone]
    /\ ckptTarget \in 0..MaxLSN
    /\ ckptPrevDirty \in BOOLEAN
    /\ recovered \in [Terms -> IndexOrNone]
    /\ recoveryFresh \in BOOLEAN

Init ==
    /\ nextLsn = 1
    /\ nextIndex = 0
    /\ phase = [w \in Writers |-> "Idle"]
    /\ opTerm = [w \in Writers |-> None]
    /\ opLsn = [w \in Writers |-> None]
    /\ opIndex = [w \in Writers |-> None]
    /\ visible = EmptyIndexMap
    /\ termLsn = EmptyLsnMap
    /\ walTerms = EmptyWal
    /\ syncedLsn = 0
    /\ walRetainedFrom = 1
    /\ durableCheckpoint = EmptyIndexMap
    /\ checkpointLsn = 0
    /\ dirty = FALSE
    /\ gate = "Open"
    /\ ckptPhase = "Idle"
    /\ ckptSnapshot = EmptyIndexMap
    /\ ckptTarget = 0
    /\ ckptPrevDirty = FALSE
    /\ recovered = EmptyIndexMap
    /\ recoveryFresh = FALSE

StartInsert(w, k) ==
    /\ w \in Writers
    /\ k \in Terms
    /\ gate = "Open"
    /\ phase[w] = "Idle"
    /\ visible[k] = None
    /\ nextLsn <= MaxLSN
    /\ nextIndex <= MaxIndex
    /\ phase' = [phase EXCEPT ![w] = "Claimed"]
    /\ opTerm' = [opTerm EXCEPT ![w] = k]
    /\ opLsn' = [opLsn EXCEPT ![w] = nextLsn]
    /\ opIndex' = [opIndex EXCEPT ![w] = nextIndex]
    /\ nextLsn' = nextLsn + 1
    /\ nextIndex' = nextIndex + 1
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<visible, termLsn, walTerms, syncedLsn, walRetainedFrom,
                  durableCheckpoint, checkpointLsn, dirty, gate, ckptPhase,
                  ckptSnapshot, ckptTarget, ckptPrevDirty, recovered>>

PublishInsert(w) ==
    /\ w \in Writers
    /\ gate = "Open"
    /\ phase[w] = "Claimed"
    /\ opTerm[w] \in Terms
    /\ opLsn[w] \in Lsns
    /\ opIndex[w] \in Indexes
    /\ visible[opTerm[w]] = None
    /\ walTerms' = [walTerms EXCEPT ![opLsn[w]] = opTerm[w]]
    /\ termLsn' = [termLsn EXCEPT ![opTerm[w]] = opLsn[w]]
    /\ visible' = [visible EXCEPT ![opTerm[w]] = opIndex[w]]
    /\ syncedLsn' = MaxNat(syncedLsn, opLsn[w])
    /\ dirty' = TRUE
    /\ phase' = [phase EXCEPT ![w] = "Idle"]
    /\ opTerm' = [opTerm EXCEPT ![w] = None]
    /\ opLsn' = [opLsn EXCEPT ![w] = None]
    /\ opIndex' = [opIndex EXCEPT ![w] = None]
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, nextIndex, walRetainedFrom, durableCheckpoint,
                  checkpointLsn, gate, ckptPhase, ckptSnapshot, ckptTarget,
                  ckptPrevDirty, recovered>>

\* A resident read of a committed term. Admitted both outside a checkpoint
\* (`gate = "Open"`) AND DURING one (`gate = "Checkpoint"`): the non-blocking
\* (write->read downgrade) checkpoint admits concurrent readers while it
\* publishes + reclaims, whereas the prior blocking checkpoint held the trie
\* write lock throughout and excluded them. A checkpoint never mutates `visible`,
\* so a read admitted during publish observes consistent committed state — TLC
\* verifies that admitting reads during a checkpoint violates no safety invariant.
ObserveExisting(w, k) ==
    /\ w \in Writers
    /\ k \in Terms
    /\ gate \in GateStates
    /\ phase[w] = "Idle"
    /\ visible[k] # None
    /\ UNCHANGED Vars

StartCheckpoint ==
    /\ gate = "Open"
    /\ ckptPhase = "Idle"
    /\ \A w \in Writers : phase[w] = "Idle"
    /\ gate' = "Checkpoint"
    /\ ckptPhase' = "Snapshotted"
    /\ ckptSnapshot' = visible
    /\ ckptTarget' = MaxVisibleLsn(visible, termLsn)
    /\ ckptPrevDirty' = dirty
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, nextIndex, phase, opTerm, opLsn, opIndex,
                  visible, termLsn, walTerms, syncedLsn, walRetainedFrom,
                  durableCheckpoint, checkpointLsn, dirty, recovered>>

FinishCheckpointSuccess ==
    /\ gate = "Checkpoint"
    /\ ckptPhase = "Snapshotted"
    /\ durableCheckpoint' = ckptSnapshot
    /\ checkpointLsn' = MaxNat(checkpointLsn, ckptTarget)
    /\ walRetainedFrom' = MaxNat(walRetainedFrom, ckptTarget + 1)
    /\ syncedLsn' = MaxNat(syncedLsn, ckptTarget)
    /\ dirty' = FALSE
    /\ gate' = "Open"
    /\ ckptPhase' = "Idle"
    /\ ckptSnapshot' = EmptyIndexMap
    /\ ckptTarget' = 0
    /\ ckptPrevDirty' = FALSE
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, nextIndex, phase, opTerm, opLsn, opIndex,
                  visible, termLsn, walTerms, recovered>>

FinishCheckpointFailure ==
    /\ gate = "Checkpoint"
    /\ ckptPhase = "Snapshotted"
    /\ dirty' = ckptPrevDirty
    /\ gate' = "Open"
    /\ ckptPhase' = "Idle"
    /\ ckptSnapshot' = EmptyIndexMap
    /\ ckptTarget' = 0
    /\ ckptPrevDirty' = FALSE
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, nextIndex, phase, opTerm, opLsn, opIndex,
                  visible, termLsn, walTerms, syncedLsn, walRetainedFrom,
                  durableCheckpoint, checkpointLsn, recovered>>

SyncOnly ==
    /\ syncedLsn' = syncedLsn
    /\ UNCHANGED <<nextLsn, nextIndex, phase, opTerm, opLsn, opIndex,
                  visible, termLsn, walTerms, walRetainedFrom,
                  durableCheckpoint, checkpointLsn, dirty, gate, ckptPhase,
                  ckptSnapshot, ckptTarget, ckptPrevDirty, recovered,
                  recoveryFresh>>

RotateWal ==
    /\ syncedLsn' = syncedLsn
    /\ UNCHANGED <<nextLsn, nextIndex, phase, opTerm, opLsn, opIndex,
                  visible, termLsn, walTerms, walRetainedFrom,
                  durableCheckpoint, checkpointLsn, dirty, gate, ckptPhase,
                  ckptSnapshot, ckptTarget, ckptPrevDirty, recovered,
                  recoveryFresh>>

CrashRecover ==
    /\ recovered' = RecoveredMap
    /\ recoveryFresh' = TRUE
    /\ UNCHANGED <<nextLsn, nextIndex, phase, opTerm, opLsn, opIndex,
                  visible, termLsn, walTerms, syncedLsn, walRetainedFrom,
                  durableCheckpoint, checkpointLsn, dirty, gate, ckptPhase,
                  ckptSnapshot, ckptTarget, ckptPrevDirty>>

Next ==
    \/ \E w \in Writers, k \in Terms : StartInsert(w, k)
    \/ \E w \in Writers : PublishInsert(w)
    \/ \E w \in Writers, k \in Terms : ObserveExisting(w, k)
    \/ StartCheckpoint
    \/ FinishCheckpointSuccess
    \/ FinishCheckpointFailure
    \/ SyncOnly
    \/ RotateWal
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
        durableCheckpoint[k] # None => termLsn[k] <= checkpointLsn

TruncationKeepsUncheckpointedVisibleWal ==
    \A k \in Terms :
        /\ visible[k] # None
        /\ durableCheckpoint[k] # visible[k]
        => termLsn[k] >= walRetainedFrom

\* The write->read DOWNGRADE keeps the WAL frontier exact. Because no writer can
\* run for the whole checkpoint (writers are excluded across capture AND publish
\* — the downgrade never releases the lock, so there is no race window), `nextLsn`
\* is unchanged from capture to WAL publish. Hence the published checkpoint frontier
\* `ckptTarget` is exactly `nextLsn - 1`, i.e. `checkpoint_lsn = next_lsn` stays
\* correct with no off-by-one and no racing-writer LSN gap. This is the model-level
\* witness of the `debug_assert_eq!(next_lsn, snapshot.next_lsn_at_capture)` in
\* `publish_durable_and_reclaim`, and is precisely why option (a) needs no
\* frontier-bounded WAL reclaim (the GAP_LEDGER #41 footgun cannot occur).
CaptureEqualsPublishFrontier ==
    gate = "Checkpoint" => nextLsn = ckptTarget + 1

DirtyFalseMeansCheckpointCoversVisible ==
    dirty = FALSE => durableCheckpoint = visible

RecoveryFreshMatchesVisible ==
    recoveryFresh => recovered = visible

RecoveredNeverInventsVisibleState ==
    \A k \in Terms : recovered[k] # None => recovered[k] = visible[k]

NoInsertWhileCheckpointGateHeld ==
    gate = "Checkpoint" => \A w \in Writers : phase[w] = "Idle"

IdleWritersHaveNoReservedOperation ==
    \A w \in Writers :
        phase[w] = "Idle" =>
            /\ opTerm[w] = None
            /\ opLsn[w] = None
            /\ opIndex[w] = None

UniqueVisibleIndexes ==
    \A k1 \in Terms, k2 \in Terms :
        /\ visible[k1] # None
        /\ visible[k2] # None
        /\ visible[k1] = visible[k2]
        => k1 = k2

Spec == Init /\ [][Next]_Vars

=============================================================================
