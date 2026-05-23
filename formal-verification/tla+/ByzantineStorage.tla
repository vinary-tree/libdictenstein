---------------------------- MODULE ByzantineStorage ----------------------------
(****************************************************************************)
(* Bounded Byzantine storage/WAL fault model.                               *)
(*                                                                          *)
(* Scope: storage records may be written, dropped, or corrupted. Recovery   *)
(* may apply only committed records with valid authentication.              *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS LSNs, Keys, Values

VARIABLES stored, applied

Vars == <<stored, applied>>

RecordSet ==
    [lsn : LSNs, key : Keys, value : Values,
     committed : BOOLEAN, authenticated : BOOLEAN]

TypeInvariant ==
    /\ stored \in SUBSET RecordSet
    /\ applied \in SUBSET RecordSet
    /\ applied \subseteq stored

Init ==
    /\ stored = {}
    /\ applied = {}

WriteRecord(r) ==
    /\ r \in RecordSet
    /\ stored' = stored \cup {r}
    /\ UNCHANGED applied

DropRecord(r) ==
    /\ r \in stored
    /\ r \notin applied
    /\ stored' = stored \ {r}
    /\ UNCHANGED applied

CorruptRecord(r) ==
    /\ r \in stored
    /\ r \notin applied
    /\ LET bad == [r EXCEPT !.authenticated = FALSE] IN
        stored' = (stored \ {r}) \cup {bad}
    /\ UNCHANGED applied

RecoverRecord(r) ==
    /\ r \in stored
    /\ r.committed
    /\ r.authenticated
    /\ applied' = applied \cup {r}
    /\ UNCHANGED stored

Next ==
    \/ \E r \in RecordSet : WriteRecord(r)
    \/ \E r \in stored : DropRecord(r)
    \/ \E r \in stored : CorruptRecord(r)
    \/ \E r \in stored : RecoverRecord(r)

AppliedOnlyCommittedAuthenticated ==
    \A r \in applied : r.committed /\ r.authenticated

UnauthenticatedRecordsNeverApplied ==
    \A r \in applied : r.authenticated

UncommittedRecordsNeverApplied ==
    \A r \in applied : r.committed

Spec == Init /\ [][Next]_Vars

=============================================================================
