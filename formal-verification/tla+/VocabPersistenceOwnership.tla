---------------------- MODULE VocabPersistenceOwnership ----------------------
(****************************************************************************)
(* Bounded vocab persistence/reopen/eviction ownership model.                *)
(*                                                                          *)
(* Scope: a vocabulary term gets one stable index and one live node-map      *)
(* entry while resident. Checkpoint/reopen may rebuild node-map entries      *)
(* using fresh nodes. Eviction may move a node to disk and drop the old raw  *)
(* pointer, but it must first remove the raw pointer from the node map.      *)
(* Forward/reverse observations may fail closed while a term is evicted, but *)
(* any successful observation must match the stable reverse index.           *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Terms, Nodes, MaxIndex, None

VARIABLES termIndex, nextIndex, nodeState, nodeTerm, nodeMap,
          reverseIndex, reverseCache, forwardObserved, reverseObserved

Vars == <<termIndex, nextIndex, nodeState, nodeTerm, nodeMap,
          reverseIndex, reverseCache, forwardObserved, reverseObserved>>

Indexes == {i \in 0..MaxIndex : TRUE}
IndexOrNone == Indexes \cup {None}
TermOrNone == Terms \cup {None}
NodeOrNone == Nodes \cup {None}
NodeStates == {"Free", "Live", "Disk", "Dropped"}

Inserted(t) == termIndex[t] # None
LiveMapped(t) == nodeMap[t] # None /\ nodeState[nodeMap[t]] = "Live"
NodeMapImage == {nodeMap[t] : t \in Terms} \ {None}

TypeInvariant ==
    /\ None \notin Terms
    /\ None \notin Nodes
    /\ termIndex \in [Terms -> IndexOrNone]
    /\ nextIndex \in 0..(MaxIndex + 1)
    /\ nodeState \in [Nodes -> NodeStates]
    /\ nodeTerm \in [Nodes -> TermOrNone]
    /\ nodeMap \in [Terms -> NodeOrNone]
    /\ reverseIndex \in [Indexes -> TermOrNone]
    /\ reverseCache \in [Indexes -> TermOrNone]
    /\ forwardObserved \in [Terms -> IndexOrNone]
    /\ reverseObserved \in [Indexes -> TermOrNone]

Init ==
    /\ termIndex = [t \in Terms |-> None]
    /\ nextIndex = 0
    /\ nodeState = [n \in Nodes |-> "Free"]
    /\ nodeTerm = [n \in Nodes |-> None]
    /\ nodeMap = [t \in Terms |-> None]
    /\ reverseIndex = [i \in Indexes |-> None]
    /\ reverseCache = [i \in Indexes |-> None]
    /\ forwardObserved = [t \in Terms |-> None]
    /\ reverseObserved = [i \in Indexes |-> None]

InsertTerm(t, n) ==
    /\ t \in Terms
    /\ n \in Nodes
    /\ ~Inserted(t)
    /\ nodeState[n] = "Free"
    /\ nextIndex <= MaxIndex
    /\ termIndex' = [termIndex EXCEPT ![t] = nextIndex]
    /\ reverseIndex' = [reverseIndex EXCEPT ![nextIndex] = t]
    /\ reverseCache' = [reverseCache EXCEPT ![nextIndex] = t]
    /\ nodeState' = [nodeState EXCEPT ![n] = "Live"]
    /\ nodeTerm' = [nodeTerm EXCEPT ![n] = t]
    /\ nodeMap' = [nodeMap EXCEPT ![t] = n]
    /\ nextIndex' = nextIndex + 1
    /\ UNCHANGED <<forwardObserved, reverseObserved>>

DuplicateInsert(t) ==
    /\ t \in Terms
    /\ Inserted(t)
    /\ forwardObserved' = [forwardObserved EXCEPT ![t] = termIndex[t]]
    /\ UNCHANGED <<termIndex, nextIndex, nodeState, nodeTerm, nodeMap,
                  reverseIndex, reverseCache, reverseObserved>>

Checkpoint ==
    UNCHANGED Vars

EvictTerm(t) ==
    LET n == nodeMap[t] IN
    /\ t \in Terms
    /\ Inserted(t)
    /\ n # None
    /\ nodeState[n] = "Live"
    /\ nodeTerm[n] = t
    /\ nodeState' = [nodeState EXCEPT ![n] = "Disk"]
    /\ nodeMap' = [nodeMap EXCEPT ![t] = None]
    /\ UNCHANGED <<termIndex, nextIndex, nodeTerm, reverseIndex,
                  reverseCache, forwardObserved, reverseObserved>>

ReopenTerm(t, n) ==
    /\ t \in Terms
    /\ n \in Nodes
    /\ Inserted(t)
    /\ nodeMap[t] = None
    /\ nodeState[n] = "Free"
    /\ nodeState' = [nodeState EXCEPT ![n] = "Live"]
    /\ nodeTerm' = [nodeTerm EXCEPT ![n] = t]
    /\ nodeMap' = [nodeMap EXCEPT ![t] = n]
    /\ UNCHANGED <<termIndex, nextIndex, reverseIndex, reverseCache,
                  forwardObserved, reverseObserved>>

ClearReverseCache(i) ==
    /\ i \in Indexes
    /\ reverseCache[i] # None
    /\ reverseCache' = [reverseCache EXCEPT ![i] = None]
    /\ UNCHANGED <<termIndex, nextIndex, nodeState, nodeTerm, nodeMap,
                  reverseIndex, forwardObserved, reverseObserved>>

ForwardLookup(t) ==
    /\ t \in Terms
    /\ IF Inserted(t) /\ LiveMapped(t)
       THEN forwardObserved' = [forwardObserved EXCEPT ![t] = termIndex[t]]
       ELSE forwardObserved' = [forwardObserved EXCEPT ![t] = None]
    /\ UNCHANGED <<termIndex, nextIndex, nodeState, nodeTerm, nodeMap,
                  reverseIndex, reverseCache, reverseObserved>>

ReverseLookup(i) ==
    /\ i \in Indexes
    /\ IF reverseCache[i] # None
       THEN reverseObserved' = [reverseObserved EXCEPT ![i] = reverseCache[i]]
       ELSE IF reverseIndex[i] # None /\ LiveMapped(reverseIndex[i])
            THEN reverseObserved' = [reverseObserved EXCEPT ![i] = reverseIndex[i]]
            ELSE reverseObserved' = [reverseObserved EXCEPT ![i] = None]
    /\ UNCHANGED <<termIndex, nextIndex, nodeState, nodeTerm, nodeMap,
                  reverseIndex, reverseCache, forwardObserved>>

Next ==
    \/ \E t \in Terms, n \in Nodes : InsertTerm(t, n)
    \/ \E t \in Terms : DuplicateInsert(t)
    \/ Checkpoint
    \/ \E t \in Terms : EvictTerm(t)
    \/ \E t \in Terms, n \in Nodes : ReopenTerm(t, n)
    \/ \E i \in Indexes : ClearReverseCache(i)
    \/ \E t \in Terms : ForwardLookup(t)
    \/ \E i \in Indexes : ReverseLookup(i)

ReverseIndexMatchesStableTermIndex ==
    \A t \in Terms :
        Inserted(t) => reverseIndex[termIndex[t]] = t

IndexesAreUnique ==
    \A t1 \in Terms, t2 \in Terms :
        /\ Inserted(t1)
        /\ Inserted(t2)
        /\ t1 # t2
        => termIndex[t1] # termIndex[t2]

NodeMapPointsOnlyToLiveOwnedNodes ==
    \A t \in Terms :
        nodeMap[t] # None =>
            /\ nodeState[nodeMap[t]] = "Live"
            /\ nodeTerm[nodeMap[t]] = t

NoStaleMapEntriesToDiskOrDroppedNodes ==
    \A n \in Nodes :
        nodeState[n] \in {"Disk", "Dropped"} => n \notin NodeMapImage

NoNodeMapAliasing ==
    \A n \in Nodes :
        Cardinality({t \in Terms : nodeMap[t] = n}) <= 1

ReverseCacheConsistentWithReverseIndex ==
    \A i \in Indexes :
        reverseCache[i] # None => reverseIndex[i] = reverseCache[i]

ForwardObservationsAreStable ==
    \A t \in Terms :
        forwardObserved[t] # None => forwardObserved[t] = termIndex[t]

ReverseObservationsAreStable ==
    \A i \in Indexes :
        reverseObserved[i] # None => reverseIndex[i] = reverseObserved[i]

Spec == Init /\ [][Next]_Vars

=============================================================================
