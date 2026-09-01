--------------------- MODULE ResidencyRevisionOrdinalABA ---------------------
(*****************************************************************************)
(* A packed per-word revision ordinal must not repeat within one retained     *)
(* materialization array. Generation rollover installs a fresh array identity *)
(* before ordinal exhaustion. Otherwise a delayed D1 helper can match the     *)
(* repeated predecessor tag after inverse D2 and resurrect stale residency.   *)
(*****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Tags, UnsafeReuseOrdinal

ASSUME Cardinality(Tags) >= 3

VARIABLES phase, initialTag, firstTag, secondTag, cellBit, cellTag, badABA

vars == <<phase, initialTag, firstTag, secondTag, cellBit, cellTag, badABA>>

Phases == {"Initial", "FirstApplied", "InverseApplied", "DelayedRan"}

TypeOK ==
    /\ phase \in Phases
    /\ initialTag \in Tags
    /\ firstTag \in Tags
    /\ secondTag \in Tags
    /\ cellBit \in BOOLEAN
    /\ cellTag \in Tags
    /\ badABA \in BOOLEAN

Init ==
    LET tag == CHOOSE value \in Tags : TRUE
    IN  /\ phase = "Initial"
        /\ initialTag = tag
        /\ firstTag = tag
        /\ secondTag = tag
        /\ cellBit = FALSE
        /\ cellTag = tag
        /\ badABA = FALSE

ApplyFirstFault(tag) ==
    /\ phase = "Initial"
    /\ tag \in Tags \ {initialTag}
    /\ phase' = "FirstApplied"
    /\ firstTag' = tag
    /\ cellBit' = TRUE
    /\ cellTag' = tag
    /\ UNCHANGED <<initialTag, secondTag, badABA>>

ApplyInverseEviction(tag) ==
    /\ phase = "FirstApplied"
    /\ tag \in Tags
    /\ tag = IF UnsafeReuseOrdinal
              THEN initialTag
              ELSE CHOOSE fresh \in Tags \ {initialTag, firstTag} : TRUE
    /\ phase' = "InverseApplied"
    /\ secondTag' = tag
    /\ cellBit' = FALSE
    /\ cellTag' = tag
    /\ UNCHANGED <<initialTag, firstTag, badABA>>

RunDelayedFirstHelper ==
    /\ phase = "InverseApplied"
    /\ phase' = "DelayedRan"
    /\ IF cellBit = FALSE /\ cellTag = initialTag
       THEN /\ cellBit' = TRUE
            /\ cellTag' = firstTag
            /\ badABA' = TRUE
       ELSE /\ UNCHANGED <<cellBit, cellTag>>
            /\ badABA' = badABA
    /\ UNCHANGED <<initialTag, firstTag, secondTag>>

Next ==
    \/ \E tag \in Tags : ApplyFirstFault(tag)
    \/ \E tag \in Tags : ApplyInverseEviction(tag)
    \/ RunDelayedFirstHelper

Spec == Init /\ [][Next]_vars

NoDelayedHelperABA == ~badABA

=============================================================================
