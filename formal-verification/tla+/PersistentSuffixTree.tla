------------------------------ MODULE PersistentSuffixTree ------------------------------
EXTENDS Naturals, FiniteSets, TLC

(*
  Bounded model for persistent suffix-tree-compatible dictionaries.

  The implementation is a first-class public view over the compact native
  persistent suffix store used by PersistentSuffixAutomaton. The suffix-tree API
  exposes substring containment, frequency, and locations from immutable graph
  snapshots while writers publish copy-on-write revisions. Removing a source
  deactivates it but may leave stale payload positions until compaction; reads
  filter by the active source set. Explicit mapped values remain visible and are
  preserved by compaction. Checkpoint/reopen copies and restores the compact
  durable source/value records and rebuilds the native suffix graph.
*)

CONSTANTS S1, S2, Empty, A, B, AB, BA

Sources == {S1, S2}
Terms == {Empty, A, B, AB, BA}

Cover(s) ==
  IF s = S1 THEN {A, AB}
  ELSE IF s = S2 THEN {B, BA}
  ELSE {}

Prefix(t, key) ==
  \/ t = Empty
  \/ t = key
  \/ /\ t = A /\ key = AB
  \/ /\ t = B /\ key = BA

VARIABLES
  active,
  payload,
  values,
  nextId,
  durableActive,
  durablePayload,
  durableValues,
  durableNextId

vars == <<active, payload, values, nextId,
          durableActive, durablePayload, durableValues, durableNextId>>

Allocated(n) ==
  IF n = 0 THEN {}
  ELSE IF n = 1 THEN {S1}
  ELSE Sources

NextSource ==
  IF nextId = 0 THEN S1 ELSE S2

PayloadFor(srcs) ==
  UNION { { <<s, t>> : t \in Cover(s) } : s \in srcs }

Init ==
  /\ active = {}
  /\ payload = {}
  /\ values = {}
  /\ nextId = 0
  /\ durableActive = {}
  /\ durablePayload = {}
  /\ durableValues = {}
  /\ durableNextId = 0

InsertNext ==
  /\ nextId \in 0..1
  /\ LET s == NextSource IN
     /\ active' = active \cup {s}
     /\ payload' = payload \cup PayloadFor({s})
  /\ values' = values
  /\ nextId' = nextId + 1
  /\ UNCHANGED <<durableActive, durablePayload, durableValues, durableNextId>>

SetValue(t) ==
  /\ t \in Terms
  /\ values' = values \cup {t}
  /\ UNCHANGED <<active, payload, nextId,
                  durableActive, durablePayload, durableValues, durableNextId>>

Remove(s) ==
  /\ s \in Sources
  /\ active' = active \ {s}
  /\ UNCHANGED <<payload, values, nextId,
                  durableActive, durablePayload, durableValues, durableNextId>>

Compact ==
  /\ active' = active
  /\ payload' = PayloadFor(active)
  /\ values' = values
  /\ UNCHANGED <<nextId, durableActive, durablePayload, durableValues, durableNextId>>

Clear ==
  /\ active' = {}
  /\ payload' = {}
  /\ values' = {}
  /\ nextId' = 0
  /\ UNCHANGED <<durableActive, durablePayload, durableValues, durableNextId>>

Checkpoint ==
  /\ durableActive' = active
  /\ durablePayload' = payload
  /\ durableValues' = values
  /\ durableNextId' = nextId
  /\ UNCHANGED <<active, payload, values, nextId>>

Reopen ==
  /\ active' = durableActive
  /\ payload' = durablePayload
  /\ values' = durableValues
  /\ nextId' = durableNextId
  /\ UNCHANGED <<durableActive, durablePayload, durableValues, durableNextId>>

Next ==
  \/ InsertNext
  \/ \E t \in Terms : SetValue(t)
  \/ \E s \in Sources : Remove(s)
  \/ Compact
  \/ Clear
  \/ Checkpoint
  \/ Reopen

TypeOK ==
  /\ active \subseteq Sources
  /\ payload \subseteq (Sources \X Terms)
  /\ values \subseteq Terms
  /\ nextId \in 0..2
  /\ durableActive \subseteq Sources
  /\ durablePayload \subseteq (Sources \X Terms)
  /\ durableValues \subseteq Terms
  /\ durableNextId \in 0..2

AllocationSound ==
  /\ active \subseteq Allocated(nextId)
  /\ \A p \in payload : p[1] \in Allocated(nextId)
  /\ durableActive \subseteq Allocated(durableNextId)
  /\ \A p \in durablePayload : p[1] \in Allocated(durableNextId)

PayloadSound ==
  \A p \in payload : p[2] \in Cover(p[1])

DurablePayloadSound ==
  \A p \in durablePayload : p[2] \in Cover(p[1])

PayloadCoversActive ==
  \A s \in active : \A t \in Cover(s) : <<s, t>> \in payload

DurablePayloadCoversActive ==
  \A s \in durableActive : \A t \in Cover(s) : <<s, t>> \in durablePayload

ValuePrefixLanguage(t) ==
  \E key \in values : Prefix(t, key)

ActiveLanguage(t) ==
  \/ t = Empty
  \/ \E s \in active : t \in Cover(s)

Contains(t) ==
  \/ t = Empty
  \/ \E s \in active : <<s, t>> \in payload
  \/ ValuePrefixLanguage(t)

SuffixTreeContains ==
  \A t \in Terms : Contains(t) <=> (ActiveLanguage(t) \/ ValuePrefixLanguage(t))

Spec == Init /\ [][Next]_vars

=============================================================================
