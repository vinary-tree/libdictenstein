------------------------------- MODULE PersistentScdawg -------------------------------
EXTENDS Naturals, FiniteSets, TLC

(*
  Bounded model for the native persistent SCDAWG graph.

  The implementation persists active term records plus a native SCDAWG WAL and
  rebuilds immutable compact graph snapshots on publication/reopen. Exact
  dictionary membership is active-term membership; substring search is
  membership in the active terms' substring language. Checkpoint/reopen copy
  and restore that abstract state.
*)

CONSTANTS S1, S2, Empty, A, B, AB, BA

Sources == {S1, S2}
Terms == {Empty, A, B, AB, BA}

SourceText(s) ==
  IF s = S1 THEN AB
  ELSE IF s = S2 THEN BA
  ELSE Empty

Substrings(t) ==
  IF t = Empty THEN {Empty}
  ELSE IF t = A THEN {Empty, A}
  ELSE IF t = B THEN {Empty, B}
  ELSE IF t = AB THEN {Empty, A, B, AB}
  ELSE IF t = BA THEN {Empty, B, A, BA}
  ELSE {Empty}

VARIABLES active, durableActive

vars == <<active, durableActive>>

Init ==
  /\ active = {}
  /\ durableActive = {}

Insert(s) ==
  /\ s \in Sources
  /\ active' = active \cup {s}
  /\ UNCHANGED durableActive

Remove(s) ==
  /\ s \in Sources
  /\ active' = active \ {s}
  /\ UNCHANGED durableActive

Checkpoint ==
  /\ durableActive' = active
  /\ UNCHANGED active

Reopen ==
  /\ active' = durableActive
  /\ UNCHANGED durableActive

Next ==
  \/ \E s \in Sources : Insert(s)
  \/ \E s \in Sources : Remove(s)
  \/ Checkpoint
  \/ Reopen

ExactContains(t) ==
  \E s \in active : SourceText(s) = t

SubstringContains(t) ==
  \/ t = Empty
  \/ \E s \in active : t \in Substrings(SourceText(s))

TypeOK ==
  /\ active \subseteq Sources
  /\ durableActive \subseteq Sources

ExactImpliesSubstring ==
  \A t \in Terms : ExactContains(t) => SubstringContains(t)

CheckpointReopenSound ==
  [][Checkpoint => ENABLED Reopen]_vars

Spec == Init /\ [][Next]_vars

=============================================================================
