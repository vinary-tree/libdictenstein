------------------- MODULE SparseResidencyWinnerAuthority -------------------
(* Winner-qualified sparse residency materialization.                        *)
(*                                                                           *)
(* Root identity and materialization ordinal are deliberately separate. Two *)
(* candidates prepared from the same exact root have the same predecessor   *)
(* and target ordinals, but only the candidate whose exact root CAS wins may *)
(* help. Untouched words retain the ordinal of their last modification.      *)
(* Full-cell tags reject a delayed helper after an inverse successor.        *)

EXTENDS Naturals, TLC

CONSTANT UnsafeLoserHelp

Words == {0, 1}
RootIds == {"InitialRoot", "RootA", "RootB", "RootInverse"}
Phases == {"Initial", "Prepared", "PublishedA", "SettledA",
           "PublishedB", "SettledB", "PublishedInverse",
           "SettledInverse", "DelayedRan", "LoserHelped"}

Cell == [tag : 0..3, bit : BOOLEAN]
Cells == [Words -> Cell]
Bits == [Words -> BOOLEAN]

VARIABLES phase, rootId, rootOrdinal, logicalBits, cells, frontier

vars == <<phase, rootId, rootOrdinal, logicalBits, cells, frontier>>

BitsOf(materialized) == [word \in Words |-> materialized[word].bit]

TypeOK ==
    /\ phase \in Phases
    /\ rootId \in RootIds
    /\ rootOrdinal \in 0..3
    /\ logicalBits \in Bits
    /\ cells \in Cells
    /\ frontier \in 0..3

SettledMaterializationMatchesRoot ==
    frontier = rootOrdinal =>
        /\ BitsOf(cells) = logicalBits
        /\ \A word \in Words : cells[word].tag <= frontier

Init ==
    /\ phase = "Initial"
    /\ rootId = "InitialRoot"
    /\ rootOrdinal = 0
    /\ logicalBits = [word \in Words |-> FALSE]
    /\ cells = [word \in Words |-> [tag |-> 0, bit |-> FALSE]]
    /\ frontier = 0

PrepareConflictingCandidates ==
    /\ phase = "Initial"
    /\ phase' = "Prepared"
    /\ UNCHANGED <<rootId, rootOrdinal, logicalBits, cells, frontier>>

(* Candidate A wins the exact root-pointer CAS. Candidate B has the same    *)
(* numeric 0 -> 1 ordinals but a conflicting word/payload.                 *)
PublishA ==
    /\ phase = "Prepared"
    /\ frontier = rootOrdinal
    /\ rootId' = "RootA"
    /\ rootOrdinal' = 1
    /\ logicalBits' = [logicalBits EXCEPT ![0] = TRUE]
    /\ phase' = "PublishedA"
    /\ UNCHANGED <<cells, frontier>>

HelpPublishedA ==
    /\ phase = "PublishedA"
    /\ cells[0] = [tag |-> 0, bit |-> FALSE]
    /\ cells' = [cells EXCEPT ![0] = [tag |-> 1, bit |-> TRUE]]
    /\ frontier' = 1
    /\ phase' = "SettledA"
    /\ UNCHANGED <<rootId, rootOrdinal, logicalBits>>

(* Negative control: let losing candidate B help merely because its numeric *)
(* ordinals match. It advances the frontier to A's ordinal while installing *)
(* B's payload, violating the root/materialization correspondence.          *)
HelpLosingB ==
    /\ UnsafeLoserHelp
    /\ phase = "PublishedA"
    /\ cells[1] = [tag |-> 0, bit |-> FALSE]
    /\ cells' = [cells EXCEPT ![1] = [tag |-> 1, bit |-> TRUE]]
    /\ frontier' = 1
    /\ phase' = "LoserHelped"
    /\ UNCHANGED <<rootId, rootOrdinal, logicalBits>>

(* A later sparse successor changes word 1. Its exact predecessor retains   *)
(* tag 0 even though the settled root ordinal is 1.                         *)
PublishSparseB ==
    /\ phase = "SettledA"
    /\ frontier = rootOrdinal
    /\ rootId' = "RootB"
    /\ rootOrdinal' = 2
    /\ logicalBits' = [logicalBits EXCEPT ![1] = TRUE]
    /\ phase' = "PublishedB"
    /\ UNCHANGED <<cells, frontier>>

HelpSparseB ==
    /\ phase = "PublishedB"
    /\ cells[1] = [tag |-> 0, bit |-> FALSE]
    /\ cells' = [cells EXCEPT ![1] = [tag |-> 2, bit |-> TRUE]]
    /\ frontier' = 2
    /\ phase' = "SettledB"
    /\ UNCHANGED <<rootId, rootOrdinal, logicalBits>>

PublishInverseA ==
    /\ phase = "SettledB"
    /\ frontier = rootOrdinal
    /\ rootId' = "RootInverse"
    /\ rootOrdinal' = 3
    /\ logicalBits' = [logicalBits EXCEPT ![0] = FALSE]
    /\ phase' = "PublishedInverse"
    /\ UNCHANGED <<cells, frontier>>

HelpInverseA ==
    /\ phase = "PublishedInverse"
    /\ cells[0] = [tag |-> 1, bit |-> TRUE]
    /\ cells' = [cells EXCEPT ![0] = [tag |-> 3, bit |-> FALSE]]
    /\ frontier' = 3
    /\ phase' = "SettledInverse"
    /\ UNCHANGED <<rootId, rootOrdinal, logicalBits>>

(* Delayed A compares the complete old cell (tag 0, bit false). The inverse *)
(* payload is false again, but tag 3 prevents ABA and the CAS is a no-op.    *)
RunDelayedA ==
    /\ phase = "SettledInverse"
    /\ cells' =
         IF cells[0] = [tag |-> 0, bit |-> FALSE]
         THEN [cells EXCEPT ![0] = [tag |-> 1, bit |-> TRUE]]
         ELSE cells
    /\ phase' = "DelayedRan"
    /\ UNCHANGED <<rootId, rootOrdinal, logicalBits, frontier>>

Next ==
    \/ PrepareConflictingCandidates
    \/ PublishA
    \/ HelpPublishedA
    \/ HelpLosingB
    \/ PublishSparseB
    \/ HelpSparseB
    \/ PublishInverseA
    \/ HelpInverseA
    \/ RunDelayedA

Spec == Init /\ [][Next]_vars

=============================================================================
