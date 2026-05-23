--------------------------- MODULE VersionLifecycle ---------------------------
(****************************************************************************)
(* Bounded MVCC/version lifecycle model.                                    *)
(*                                                                          *)
(* Scope: readers pin versions, writers retire versions, and reclamation    *)
(* is allowed only when no reader still references a retired version.       *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Versions, Readers, NoVersion

VARIABLES versionState, readerVersion, durableVersions

Vars == <<versionState, readerVersion, durableVersions>>

VersionStates == {"Active", "Retired", "Reclaimed"}

TypeInvariant ==
    /\ versionState \in [Versions -> VersionStates]
    /\ readerVersion \in [Readers -> Versions \cup {NoVersion}]
    /\ durableVersions \in SUBSET Versions

Init ==
    /\ versionState = [v \in Versions |-> "Active"]
    /\ readerVersion = [r \in Readers |-> NoVersion]
    /\ durableVersions = {}

BeginRead(r, v) ==
    /\ r \in Readers
    /\ v \in Versions
    /\ readerVersion[r] = NoVersion
    /\ versionState[v] # "Reclaimed"
    /\ readerVersion' = [readerVersion EXCEPT ![r] = v]
    /\ UNCHANGED <<versionState, durableVersions>>

EndRead(r) ==
    /\ r \in Readers
    /\ readerVersion[r] # NoVersion
    /\ readerVersion' = [readerVersion EXCEPT ![r] = NoVersion]
    /\ UNCHANGED <<versionState, durableVersions>>

Retire(v) ==
    /\ v \in Versions
    /\ versionState[v] = "Active"
    /\ versionState' = [versionState EXCEPT ![v] = "Retired"]
    /\ UNCHANGED <<readerVersion, durableVersions>>

Reclaim(v) ==
    /\ v \in Versions
    /\ versionState[v] = "Retired"
    /\ v \notin durableVersions
    /\ \A r \in Readers : readerVersion[r] # v
    /\ versionState' = [versionState EXCEPT ![v] = "Reclaimed"]
    /\ UNCHANGED <<readerVersion, durableVersions>>

MarkDurable(v) ==
    /\ v \in Versions
    /\ versionState[v] # "Reclaimed"
    /\ durableVersions' = durableVersions \cup {v}
    /\ UNCHANGED <<versionState, readerVersion>>

Next ==
    \/ \E r \in Readers, v \in Versions : BeginRead(r, v)
    \/ \E r \in Readers : EndRead(r)
    \/ \E v \in Versions : Retire(v)
    \/ \E v \in Versions : Reclaim(v)
    \/ \E v \in Versions : MarkDurable(v)

NoReaderReferencesReclaimedVersion ==
    \A r \in Readers :
        readerVersion[r] # NoVersion =>
            versionState[readerVersion[r]] # "Reclaimed"

ReclaimedVersionHasNoReaders ==
    \A v \in Versions :
        versionState[v] = "Reclaimed" =>
            \A r \in Readers : readerVersion[r] # v

DurableVersionsNotReclaimed ==
    \A v \in durableVersions : versionState[v] # "Reclaimed"

Spec == Init /\ [][Next]_Vars

=============================================================================
