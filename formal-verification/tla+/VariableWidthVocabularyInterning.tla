------------------ MODULE VariableWidthVocabularyInterning ------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC

(***************************************************************************)
(* Concurrent canonical-atom reservation, materialization, and publication. *)
(*                                                                         *)
(* The finite atoms and IDs are TLC counterexample bounds, never library     *)
(* workload limits. CanonicalBytes deliberately collide under Fingerprint.  *)
(* A reservation allocates an ID without bytes. Materialization appends one  *)
(* nonempty canonical codeword and its exact span. Losing either kind of     *)
(* claim produces an unmaterialized or materialized orphan, respectively.    *)
(* Every materialized span is disjoint, in bounds, and collectively covers   *)
(* the append-only packed byte sequence exactly.                             *)
(***************************************************************************)

CONSTANTS FingerprintOnlyEquality, ReusePublishedId

ASSUME /\ FingerprintOnlyEquality \in BOOLEAN
       /\ ReusePublishedId \in BOOLEAN

Atoms == {"Alpha", "Beta"}
Ids == 0..1
NoAtom == "NoAtom"
NoId == 2
AtomOrNone == Atoms \cup {NoAtom}
IdOrNone == 0..2

CanonicalDescriptor ==
  [logicalAlphabet |-> "CanonicalULEB",
   codecIdentity |-> "CanonicalULEB-v1",
   layoutIdentity |-> "LogicalUnit-v1",
   abiVersion |-> 1]

DescriptorType ==
  [logicalAlphabet : {"CanonicalULEB"},
   codecIdentity : {"CanonicalULEB-v1"},
   layoutIdentity : {"LogicalUnit-v1"},
   abiVersion : {1}]

EmptyAtomToId == [atom \in Atoms |-> NoId]
EmptyIdToAtom == [id \in Ids |-> NoAtom]
EmptyPayload == [id \in Ids |-> <<>>]
EmptyNatMap == [id \in Ids |-> 0]

CanonicalBytes(atom) ==
  CASE atom = "Alpha" -> <<129, 1>>
    [] OTHER           -> <<130, 1>>

CanonicalUlebCodeword(bytes) ==
  /\ Len(bytes) > 0
  /\ \A index \in 1..Len(bytes) : bytes[index] \in 0..255
  /\ bytes[Len(bytes)] < 128
  /\ \A index \in 1..(Len(bytes) - 1) : bytes[index] >= 128
  /\ (Len(bytes) = 1 \/ bytes[Len(bytes)] # 0)

DescriptorCanonicalCodeword(descriptorValue, atom, bytes) ==
  /\ descriptorValue = CanonicalDescriptor
  /\ atom \in Atoms
  /\ bytes = CanonicalBytes(atom)
  /\ CanonicalUlebCodeword(bytes)

Fingerprint(_atom) == 7

VARIABLES descriptor,
          claims,
          atomToId,
          idToAtom,
          everPublishedOwner,
          retiredIds,
          nextId,
          allocatedIds,
          durablePayload,
          durableSpan,
          payloadById,
          packedBytes,
          spanOffset,
          spanLength,
          spanOwner

vars == <<descriptor, claims, atomToId, idToAtom, everPublishedOwner,
          retiredIds,
          nextId, allocatedIds, durablePayload, durableSpan, payloadById,
          packedBytes, spanOffset, spanLength, spanOwner>>

LiveIds == {id \in Ids : idToAtom[id] # NoAtom}
ClaimedIds == {id \in Ids : claims[id] # NoAtom}
HistoricalIds == {id \in Ids : everPublishedOwner[id] # NoAtom}
TombstonedIds == HistoricalIds \ LiveIds
OrphanIds == allocatedIds \ (HistoricalIds \cup ClaimedIds)
MaterializedIds == durablePayload \cap durableSpan
ReservedIds == ClaimedIds \ MaterializedIds
MaterializedClaimedIds == ClaimedIds \cap MaterializedIds
MaterializedOrphanIds == OrphanIds \cap MaterializedIds
UnmaterializedOrphanIds == OrphanIds \ MaterializedIds

HasFingerprintCandidate(atom) ==
  \E id \in LiveIds : Fingerprint(idToAtom[id]) = Fingerprint(atom)

FingerprintCandidate(atom) ==
  CHOOSE id \in LiveIds : Fingerprint(idToAtom[id]) = Fingerprint(atom)

ReadSpan(id) ==
  SubSeq(packedBytes, spanOffset[id] + 1, spanOffset[id] + spanLength[id])

SpansDisjoint(left, right) ==
  spanOffset[left] + spanLength[left] <= spanOffset[right] \/
  spanOffset[right] + spanLength[right] <= spanOffset[left]

SpanContainsOffset(id, offset) ==
  spanOffset[id] <= offset /\ offset < spanOffset[id] + spanLength[id]

ExactSpanForAtom(id, atom) ==
  /\ id \in MaterializedIds
  /\ DescriptorCanonicalCodeword(descriptor, atom, payloadById[id])
  /\ spanOwner[id] = atom
  /\ spanLength[id] = Len(payloadById[id])
  /\ spanLength[id] > 0
  /\ spanOffset[id] + spanLength[id] <= Len(packedBytes)
  /\ ReadSpan(id) = payloadById[id]

TypeOK ==
  /\ descriptor \in DescriptorType
  /\ claims \in [Ids -> AtomOrNone]
  /\ atomToId \in [Atoms -> IdOrNone]
  /\ idToAtom \in [Ids -> AtomOrNone]
  /\ everPublishedOwner \in [Ids -> AtomOrNone]
  /\ retiredIds \in SUBSET Ids
  /\ nextId \in 0..2
  /\ allocatedIds \in SUBSET Ids
  /\ durablePayload \in SUBSET Ids
  /\ durableSpan \in SUBSET Ids
  /\ payloadById \in [Ids -> Seq(0..255)]
  /\ packedBytes \in Seq(0..255)
  /\ spanOffset \in [Ids -> 0..4]
  /\ spanLength \in [Ids -> 0..2]
  /\ spanOwner \in [Ids -> AtomOrNone]

Init ==
  /\ descriptor = CanonicalDescriptor
  /\ claims = EmptyIdToAtom
  /\ atomToId = EmptyAtomToId
  /\ idToAtom = EmptyIdToAtom
  /\ everPublishedOwner = EmptyIdToAtom
  /\ retiredIds = {}
  /\ nextId = 0
  /\ allocatedIds = {}
  /\ durablePayload = {}
  /\ durableSpan = {}
  /\ payloadById = EmptyPayload
  /\ packedBytes = <<>>
  /\ spanOffset = EmptyNatMap
  /\ spanLength = EmptyNatMap
  /\ spanOwner = EmptyIdToAtom

MultiSpanInit ==
  /\ descriptor = CanonicalDescriptor
  /\ claims = [id \in Ids |-> IF id = 0 THEN "Alpha" ELSE "Beta"]
  /\ atomToId = EmptyAtomToId
  /\ idToAtom = EmptyIdToAtom
  /\ everPublishedOwner = EmptyIdToAtom
  /\ retiredIds = {}
  /\ nextId = 2
  /\ allocatedIds = Ids
  /\ durablePayload = Ids
  /\ durableSpan = Ids
  /\ payloadById =
       [id \in Ids |-> IF id = 0
                         THEN CanonicalBytes("Alpha")
                         ELSE CanonicalBytes("Beta")]
  /\ packedBytes = CanonicalBytes("Alpha") \o CanonicalBytes("Beta")
  /\ spanOffset = [id \in Ids |-> IF id = 0 THEN 0 ELSE 2]
  /\ spanLength = [id \in Ids |-> 2]
  /\ spanOwner = [id \in Ids |-> IF id = 0 THEN "Alpha" ELSE "Beta"]

VWENC_180_MULTISPAN_WITNESS_IS_CONCRETE ==
  /\ MaterializedIds = Ids
  /\ Cardinality(MaterializedIds) = 2
  /\ packedBytes = CanonicalBytes("Alpha") \o CanonicalBytes("Beta")
  /\ spanOffset[0] = 0
  /\ spanOffset[1] = Len(CanonicalBytes("Alpha"))
  /\ ReadSpan(0) = CanonicalBytes("Alpha")
  /\ ReadSpan(1) = CanonicalBytes("Beta")
  /\ SpansDisjoint(0, 1)

ClaimAtomId(atom) ==
  /\ atom \in Atoms
  /\ atomToId[atom] = NoId
  /\ nextId <= 1
  /\ LET candidate ==
       IF ReusePublishedId /\ 0 \in TombstonedIds
       THEN 0
       ELSE nextId
     IN /\ candidate \in Ids
        /\ claims[candidate] = NoAtom
        /\ idToAtom[candidate] = NoAtom
        /\ (ReusePublishedId \/ candidate \notin retiredIds)
        /\ (ReusePublishedId \/
             everPublishedOwner[candidate] = NoAtom)
        /\ claims' = [claims EXCEPT ![candidate] = atom]
        /\ allocatedIds' = allocatedIds \cup {candidate}
        /\ nextId' = IF candidate = nextId THEN nextId + 1 ELSE nextId
  /\ UNCHANGED <<descriptor, atomToId, idToAtom, everPublishedOwner,
                  retiredIds,
                  durablePayload, durableSpan, payloadById, packedBytes,
                  spanOffset, spanLength, spanOwner>>

WriteCanonicalPayloadAndSpan(id) ==
  /\ id \in ClaimedIds
  /\ \/ (id \notin durablePayload /\ id \notin durableSpan)
     \/ /\ ReusePublishedId
        /\ id \in TombstonedIds
        /\ id \in durablePayload
        /\ id \in durableSpan
        /\ spanOwner[id] # claims[id]
  /\ DescriptorCanonicalCodeword(
       descriptor, claims[id], CanonicalBytes(claims[id]))
  /\ payloadById' = [payloadById EXCEPT ![id] = CanonicalBytes(claims[id])]
  /\ spanOwner' = [spanOwner EXCEPT ![id] = claims[id]]
  /\ spanOffset' = [spanOffset EXCEPT ![id] = Len(packedBytes)]
  /\ spanLength' =
       [spanLength EXCEPT ![id] = Len(CanonicalBytes(claims[id]))]
  /\ packedBytes' = packedBytes \o CanonicalBytes(claims[id])
  /\ durablePayload' = durablePayload \cup {id}
  /\ durableSpan' = durableSpan \cup {id}
  /\ UNCHANGED <<descriptor, claims, atomToId, idToAtom,
                  everPublishedOwner, retiredIds, nextId, allocatedIds>>

PublishClaim(id) ==
  /\ id \in MaterializedClaimedIds
  /\ ExactSpanForAtom(id, claims[id])
  /\ idToAtom[id] = NoAtom
  /\ (ReusePublishedId \/ everPublishedOwner[id] = NoAtom)
  /\ LET atom == claims[id] IN
       /\ atomToId[atom] = NoId
       /\ atomToId' = [atomToId EXCEPT ![atom] = id]
       /\ idToAtom' = [idToAtom EXCEPT ![id] = atom]
       /\ everPublishedOwner' =
            IF everPublishedOwner[id] = NoAtom
            THEN [everPublishedOwner EXCEPT ![id] = atom]
            ELSE everPublishedOwner
       /\ claims' = [claims EXCEPT ![id] = NoAtom]
  /\ UNCHANGED <<descriptor, retiredIds, nextId, allocatedIds, durablePayload,
                  durableSpan, payloadById, packedBytes, spanOffset,
                  spanLength, spanOwner>>

TombstonePublishedId(id) ==
  /\ id \in LiveIds
  /\ LET atom == idToAtom[id] IN
       /\ atomToId[atom] = id
       /\ atomToId' = [atomToId EXCEPT ![atom] = NoId]
       /\ idToAtom' = [idToAtom EXCEPT ![id] = NoAtom]
       /\ retiredIds' = retiredIds \cup {id}
  /\ UNCHANGED <<descriptor, claims, everPublishedOwner, nextId,
                  allocatedIds, durablePayload, durableSpan, payloadById,
                  packedBytes, spanOffset, spanLength, spanOwner>>

LoseClaimToOrphan(id) ==
  /\ id \in ClaimedIds
  /\ claims' = [claims EXCEPT ![id] = NoAtom]
  /\ UNCHANGED <<descriptor, atomToId, idToAtom, everPublishedOwner,
                  retiredIds,
                  nextId, allocatedIds, durablePayload, durableSpan,
                  payloadById, packedBytes, spanOffset, spanLength,
                  spanOwner>>

ReturnFingerprintCandidateWithoutByteCheck(atom) ==
  /\ FingerprintOnlyEquality
  /\ atom \in Atoms
  /\ atomToId[atom] = NoId
  /\ HasFingerprintCandidate(atom)
  /\ LET candidate == FingerprintCandidate(atom) IN
       atomToId' = [atomToId EXCEPT ![atom] = candidate]
  /\ UNCHANGED <<descriptor, claims, idToAtom, everPublishedOwner,
                  retiredIds,
                  nextId, allocatedIds, durablePayload, durableSpan,
                  payloadById, packedBytes, spanOffset, spanLength,
                  spanOwner>>

ReturnExistingAtom(atom) ==
  /\ atom \in Atoms
  /\ atomToId[atom] # NoId
  /\ UNCHANGED vars

Next ==
  \/ \E atom \in Atoms : ClaimAtomId(atom)
  \/ \E id \in Ids : WriteCanonicalPayloadAndSpan(id)
  \/ \E id \in Ids : PublishClaim(id)
  \/ \E id \in Ids : TombstonePublishedId(id)
  \/ \E id \in Ids : LoseClaimToOrphan(id)
  \/ \E atom \in Atoms : ReturnFingerprintCandidateWithoutByteCheck(atom)
  \/ \E atom \in Atoms : ReturnExistingAtom(atom)

VWENC_141_PUBLISHED_ATOM_RELATION_IS_EXACT_BIJECTION ==
  /\ \A atom \in Atoms :
       atomToId[atom] # NoId => idToAtom[atomToId[atom]] = atom
  /\ \A id \in Ids :
       idToAtom[id] # NoAtom => atomToId[idToAtom[id]] = id

VWENC_142_FINGERPRINT_COLLISIONS_NEVER_ALIAS_DISTINCT_ATOMS ==
  \A left \in Atoms, right \in Atoms :
    /\ left # right
    /\ atomToId[left] # NoId
    /\ atomToId[right] # NoId
    => atomToId[left] # atomToId[right]

VWENC_143_RETIRED_ID_IS_NEVER_CLAIMED_OR_LIVE_AGAIN ==
  retiredIds \cap (ClaimedIds \cup LiveIds) = {}

VWENC_192_EVER_PUBLISHED_OWNER_IS_IMMUTABLE ==
  \A id \in Ids :
    /\ everPublishedOwner[id] # NoAtom
    /\ idToAtom[id] # NoAtom
    => idToAtom[id] = everPublishedOwner[id]

VWENC_144_LIVE_ID_HAS_EXACT_DURABLE_PAYLOAD_AND_SPAN ==
  \A id \in LiveIds : ExactSpanForAtom(id, idToAtom[id])

VWENC_145_ACTIVE_CLAIMS_DO_NOT_OVERWRITE_LIVE_IDS ==
  ClaimedIds \cap LiveIds = {}

VWENC_146_ORPHAN_ALLOCATIONS_HAVE_NO_LOGICAL_BINDING ==
  /\ \A id \in OrphanIds :
       /\ idToAtom[id] = NoAtom
       /\ everPublishedOwner[id] = NoAtom
       /\ \A atom \in Atoms : atomToId[atom] # id
  /\ OrphanIds = MaterializedOrphanIds \cup UnmaterializedOrphanIds
  /\ MaterializedOrphanIds \cap UnmaterializedOrphanIds = {}

VWENC_175_PACKED_SPANS_ARE_DISJOINT_AND_COVER_BYTES_EXACTLY ==
  /\ \A id \in MaterializedIds :
       ExactSpanForAtom(id, spanOwner[id])
  /\ \A left \in MaterializedIds, right \in MaterializedIds :
       left # right => SpansDisjoint(left, right)
  /\ (Len(packedBytes) > 0 =>
       \A offset \in 0..(Len(packedBytes) - 1) :
         Cardinality(
           {id \in MaterializedIds : SpanContainsOffset(id, offset)}) = 1)

VWENC_176_ALLOCATION_STATUSES_PARTITION_ALLOCATED_IDS ==
  /\ allocatedIds =
       ReservedIds \cup MaterializedClaimedIds \cup LiveIds \cup
       TombstonedIds \cup MaterializedOrphanIds \cup
       UnmaterializedOrphanIds
  /\ ReservedIds \cap MaterializedClaimedIds = {}
  /\ ClaimedIds \cap HistoricalIds = {}
  /\ LiveIds \cap TombstonedIds = {}
  /\ OrphanIds \cap (ClaimedIds \cup HistoricalIds) = {}

VWENC_177_DESCRIPTOR_GOVERNS_EVERY_MATERIALIZED_CODEWORD ==
  \A id \in MaterializedIds :
    DescriptorCanonicalCodeword(descriptor, spanOwner[id], payloadById[id])

Spec == Init /\ [][Next]_vars

=============================================================================
