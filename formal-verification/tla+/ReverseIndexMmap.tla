-------------------------- MODULE ReverseIndexMmap --------------------------
(***************************************************************************)
(* Reverse-index mmap/remap publication model.                              *)
(*                                                                         *)
(* Scope: VocabReverseIndex create/open/grow. The unsafe mmap boundary is   *)
(* valid only if the mapped capacity is backed by the file and published    *)
(* header capacity never exceeds the live mapping.                          *)
(***************************************************************************)

EXTENDS Naturals, TLC

CONSTANTS Capacities

VARIABLES fileCapacity, mapCapacity, headerCapacity, entryCount, mapped

Vars == <<fileCapacity, mapCapacity, headerCapacity, entryCount, mapped>>

TypeInvariant ==
    /\ fileCapacity \in Capacities
    /\ mapCapacity \in Capacities
    /\ headerCapacity \in Capacities
    /\ entryCount \in Capacities
    /\ mapped \in BOOLEAN

Init ==
    /\ fileCapacity = 0
    /\ mapCapacity = 0
    /\ headerCapacity = 0
    /\ entryCount = 0
    /\ mapped = FALSE

Create(c) ==
    /\ c \in Capacities \ {0}
    /\ fileCapacity = 0
    /\ fileCapacity' = c
    /\ mapCapacity' = c
    /\ headerCapacity' = c
    /\ entryCount' = 0
    /\ mapped' = TRUE

Open ==
    /\ fileCapacity > 0
    /\ mapCapacity' = fileCapacity
    /\ mapped' = TRUE
    /\ UNCHANGED <<fileCapacity, headerCapacity, entryCount>>

SetEntry(i) ==
    /\ mapped
    /\ i \in Capacities
    /\ i > 0
    /\ i <= headerCapacity
    /\ entryCount' = IF i > entryCount THEN i ELSE entryCount
    /\ UNCHANGED <<fileCapacity, mapCapacity, headerCapacity, mapped>>

ExtendFile(c) ==
    /\ c \in Capacities
    /\ c > fileCapacity
    /\ fileCapacity' = c
    /\ UNCHANGED <<mapCapacity, headerCapacity, entryCount, mapped>>

Remap ==
    /\ fileCapacity > mapCapacity
    /\ mapCapacity' = fileCapacity
    /\ mapped' = TRUE
    /\ UNCHANGED <<fileCapacity, headerCapacity, entryCount>>

PublishHeader ==
    /\ mapped
    /\ mapCapacity >= headerCapacity
    /\ headerCapacity' = mapCapacity
    /\ UNCHANGED <<fileCapacity, mapCapacity, entryCount, mapped>>

Next ==
    \/ \E c \in Capacities : Create(c)
    \/ Open
    \/ \E i \in Capacities : SetEntry(i)
    \/ \E c \in Capacities : ExtendFile(c)
    \/ Remap
    \/ PublishHeader

Spec == Init /\ [][Next]_Vars

MappedWithinFile == mapCapacity <= fileCapacity

HeaderWithinMap == headerCapacity <= mapCapacity

EntriesWithinHeader == entryCount <= headerCapacity

PublishedHeaderWithinFile == headerCapacity <= fileCapacity

=============================================================================
