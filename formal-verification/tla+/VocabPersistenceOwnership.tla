---------------------- MODULE VocabPersistenceOwnership ----------------------
(****************************************************************************)
(* Bounded vocab persistence/reopen ownership model for the V6 overlay-only  *)
(* vocabulary.                                                              *)
(*                                                                          *)
(* Scope: the published overlay is the sole source of truth for term -> id   *)
(* bindings. The in-memory reverse_term_map is a derived exact inverse that  *)
(* is populated on insert and rebuilt from the checkpoint image plus WAL     *)
(* tail on reopen. There is no owned parent-pointer tree, node_map, reverse  *)
(* index sidecar, or vocab overlay eviction action in this model.            *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Terms, MaxIndex, None

VARIABLES termIndex, nextIndex, checkpointed, walLive, reverseMap,
          forwardObserved, reverseObserved, recovered, dirty

Vars == <<termIndex, nextIndex, checkpointed, walLive, reverseMap,
          forwardObserved, reverseObserved, recovered, dirty>>

Indexes == 0..MaxIndex
IndexOrNone == Indexes \cup {None}
TermOrNone == Terms \cup {None}
EmptyIndexMap == [t \in Terms |-> None]
EmptyBoolMap == [t \in Terms |-> FALSE]
EmptyReverseMap == [i \in Indexes |-> None]

Inserted(t) == termIndex[t] # None
UsedIndexes(m) == {m[t] : t \in Terms} \ {None}

MaxNat(a, b) == IF a >= b THEN a ELSE b

TermForIndex(m, i) ==
    IF \E t \in Terms : m[t] = i
    THEN CHOOSE t \in Terms : m[t] = i
    ELSE None

ReverseFrom(m) == [i \in Indexes |-> TermForIndex(m, i)]

RecoveredMap ==
    [t \in Terms |->
        IF checkpointed[t] # None
        THEN checkpointed[t]
        ELSE IF walLive[t]
             THEN termIndex[t]
             ELSE None]

TypeInvariant ==
    /\ None \notin Terms
    /\ None \notin Indexes
    /\ termIndex \in [Terms -> IndexOrNone]
    /\ nextIndex \in 0..(MaxIndex + 1)
    /\ checkpointed \in [Terms -> IndexOrNone]
    /\ walLive \in [Terms -> BOOLEAN]
    /\ reverseMap \in [Indexes -> TermOrNone]
    /\ forwardObserved \in [Terms -> IndexOrNone]
    /\ reverseObserved \in [Indexes -> TermOrNone]
    /\ recovered \in [Terms -> IndexOrNone]
    /\ dirty \in BOOLEAN

Init ==
    /\ termIndex = EmptyIndexMap
    /\ nextIndex = 0
    /\ checkpointed = EmptyIndexMap
    /\ walLive = EmptyBoolMap
    /\ reverseMap = EmptyReverseMap
    /\ forwardObserved = EmptyIndexMap
    /\ reverseObserved = EmptyReverseMap
    /\ recovered = EmptyIndexMap
    /\ dirty = FALSE

InsertTerm(t) ==
    /\ t \in Terms
    /\ ~Inserted(t)
    /\ nextIndex <= MaxIndex
    /\ LET newTermIndex == [termIndex EXCEPT ![t] = nextIndex] IN
       /\ termIndex' = newTermIndex
       /\ reverseMap' = ReverseFrom(newTermIndex)
    /\ walLive' = [walLive EXCEPT ![t] = TRUE]
    /\ nextIndex' = nextIndex + 1
    /\ dirty' = TRUE
    /\ UNCHANGED <<checkpointed, forwardObserved, reverseObserved, recovered>>

ManualInsert(t, i) ==
    /\ t \in Terms
    /\ i \in Indexes
    /\ ~Inserted(t)
    /\ i \notin UsedIndexes(termIndex)
    /\ LET newTermIndex == [termIndex EXCEPT ![t] = i] IN
       /\ termIndex' = newTermIndex
       /\ reverseMap' = ReverseFrom(newTermIndex)
    /\ walLive' = [walLive EXCEPT ![t] = TRUE]
    /\ nextIndex' = MaxNat(nextIndex, i + 1)
    /\ dirty' = TRUE
    /\ UNCHANGED <<checkpointed, forwardObserved, reverseObserved, recovered>>

DuplicateInsert(t) ==
    /\ t \in Terms
    /\ Inserted(t)
    /\ forwardObserved' = [forwardObserved EXCEPT ![t] = termIndex[t]]
    /\ UNCHANGED <<termIndex, nextIndex, checkpointed, walLive, reverseMap,
                  reverseObserved, recovered, dirty>>

Checkpoint ==
    /\ checkpointed' = termIndex
    /\ walLive' = EmptyBoolMap
    /\ dirty' = FALSE
    /\ UNCHANGED <<termIndex, nextIndex, reverseMap,
                  forwardObserved, reverseObserved, recovered>>

Reopen ==
    /\ LET newTermIndex == RecoveredMap IN
       /\ termIndex' = newTermIndex
       /\ reverseMap' = ReverseFrom(newTermIndex)
    /\ dirty' = \E t \in Terms : walLive[t]
    /\ UNCHANGED <<nextIndex, checkpointed, walLive,
                  forwardObserved, reverseObserved, recovered>>

CrashRecover ==
    /\ recovered' = RecoveredMap
    /\ UNCHANGED <<termIndex, nextIndex, checkpointed, walLive, reverseMap,
                  forwardObserved, reverseObserved, dirty>>

ForwardLookup(t) ==
    /\ t \in Terms
    /\ forwardObserved' = [forwardObserved EXCEPT ![t] = termIndex[t]]
    /\ UNCHANGED <<termIndex, nextIndex, checkpointed, walLive, reverseMap,
                  reverseObserved, recovered, dirty>>

ReverseLookup(i) ==
    /\ i \in Indexes
    /\ reverseObserved' = [reverseObserved EXCEPT ![i] = reverseMap[i]]
    /\ UNCHANGED <<termIndex, nextIndex, checkpointed, walLive, reverseMap,
                  forwardObserved, recovered, dirty>>

Next ==
    \/ \E t \in Terms : InsertTerm(t)
    \/ \E t \in Terms, i \in Indexes : ManualInsert(t, i)
    \/ \E t \in Terms : DuplicateInsert(t)
    \/ Checkpoint
    \/ Reopen
    \/ CrashRecover
    \/ \E t \in Terms : ForwardLookup(t)
    \/ \E i \in Indexes : ReverseLookup(i)

ReverseMapIsExactInverse ==
    reverseMap = ReverseFrom(termIndex)

IndexesAreUnique ==
    \A t1 \in Terms, t2 \in Terms :
        /\ Inserted(t1)
        /\ Inserted(t2)
        /\ t1 # t2
        => termIndex[t1] # termIndex[t2]

CheckpointedBindingsAreStable ==
    \A t \in Terms :
        checkpointed[t] # None => termIndex[t] = checkpointed[t]

WalTailCoversUncheckpointedVisibleTerms ==
    \A t \in Terms :
        /\ termIndex[t] # None
        /\ checkpointed[t] # termIndex[t]
        => walLive[t]

ForwardObservationsAreStable ==
    \A t \in Terms :
        forwardObserved[t] # None => forwardObserved[t] = termIndex[t]

ReverseObservationsAreStable ==
    \A i \in Indexes :
        reverseObserved[i] # None =>
            /\ reverseMap[i] = reverseObserved[i]
            /\ termIndex[reverseObserved[i]] = i

RecoveredNeverInventsVisibleState ==
    \A t \in Terms :
        recovered[t] # None => termIndex[t] = recovered[t]

DirtyFalseMeansCheckpointCoversVisible ==
    dirty = FALSE => checkpointed = termIndex

Spec == Init /\ [][Next]_Vars

=============================================================================
