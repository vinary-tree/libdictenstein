------------------------ MODULE CachelessOwnedRegistry ------------------------
(***************************************************************************)
(* Refinement model for cacheless owned registry lookup and removal.       *)
(*                                                                         *)
(* The abstract side always selects the last occurrence in an ordered hash *)
(* collision bucket. The implementation side is identical unless a single  *)
(* negative-control switch enables one rejected design. Materialization is  *)
(* completed before removal mutates live records, residency, or accounting. *)
(***************************************************************************)

EXTENDS FiniteSets, Naturals, Sequences, TLC

CONSTANTS UnsafeSelectFirst, UnsafeMutateBeforeMaterialize

Ids == {"a", "b", "c"}
Types == {"byteNode", "charNode"}
InitialBucket == <<"a", "b", "c">>
PathOf ==
    [id \in Ids |->
       CASE id = "a" -> <<1>>
         [] id = "b" -> <<2, 3>>
         [] OTHER -> <<4, 5, 6>>]
SizeOf ==
    [id \in Ids |->
       CASE id = "a" -> 11
         [] id = "b" -> 13
         [] OTHER -> 17]
TypeOf ==
    [id \in Ids |-> IF id = "a" THEN "byteNode" ELSE "charNode"]
Materializable == Ids
InitialAuthority == "exactGeneration"

NoId == "NoId"
NoResult ==
    [occurrence |-> NoId,
     path |-> <<>>,
     capability |-> "None"]

LastId(bucket) == bucket[Len(bucket)]

SelectedId(bucket) ==
    IF UnsafeSelectFirst THEN Head(bucket) ELSE LastId(bucket)

RemoveId(bucket, id) == SelectSeq(bucket, LAMBDA candidate: candidate # id)

LiveOf(bucket) == {bucket[index] : index \in 1..Len(bucket)}

CountType(bucket, nodeType) ==
    Cardinality({index \in 1..Len(bucket) : TypeOf[bucket[index]] = nodeType})

RECURSIVE SequenceBytes(_)
SequenceBytes(bucket) ==
    IF Len(bucket) = 0
    THEN 0
    ELSE SizeOf[Head(bucket)] + SequenceBytes(Tail(bucket))

TypeCountsOf(bucket) == [nodeType \in Types |-> CountType(bucket, nodeType)]

Owned(id) ==
    [occurrence |-> id,
     path |-> PathOf[id],
     capability |-> "Detached"]

OwnedResults == {Owned(id) : id \in Ids}
ResultValues == OwnedResults \cup {NoResult}

Projection(live, bucket, residency, totalBytes, typeCounts, authority) ==
    [live |-> live,
     bucket |-> bucket,
     residency |-> residency,
     totalBytes |-> totalBytes,
     typeCounts |-> typeCounts,
     authority |-> authority]

ProjectionValues ==
    [live : SUBSET Ids,
     bucket : Seq(Ids),
     residency : SUBSET Ids,
     totalBytes : Nat,
     typeCounts : [Types -> Nat],
     authority : {InitialAuthority}]

VARIABLES
    specLive,
    implLive,
    specBucket,
    implBucket,
    specResidency,
    implResidency,
    specTotalBytes,
    implTotalBytes,
    specTypeCounts,
    implTypeCounts,
    specResult,
    implResult,
    specAuthority,
    implAuthority,
    chosenId,
    expectedLast,
    beforeProjection,
    lastAction

vars ==
    <<specLive, implLive, specBucket, implBucket,
      specResidency, implResidency, specTotalBytes, implTotalBytes,
      specTypeCounts, implTypeCounts, specResult, implResult,
      specAuthority, implAuthority, chosenId, expectedLast,
      beforeProjection, lastAction>>

CurrentImplProjection ==
    Projection(implLive, implBucket, implResidency, implTotalBytes,
               implTypeCounts, implAuthority)

CurrentSpecProjection ==
    Projection(specLive, specBucket, specResidency, specTotalBytes,
               specTypeCounts, specAuthority)

TypeOK ==
    /\ specLive \subseteq Ids
    /\ implLive \subseteq Ids
    /\ specBucket \in Seq(Ids)
    /\ implBucket \in Seq(Ids)
    /\ specResidency \subseteq Ids
    /\ implResidency \subseteq Ids
    /\ specTotalBytes \in Nat
    /\ implTotalBytes \in Nat
    /\ specTypeCounts \in [Types -> Nat]
    /\ implTypeCounts \in [Types -> Nat]
    /\ specResult \in ResultValues
    /\ implResult \in ResultValues
    /\ chosenId \in Ids \cup {NoId}
    /\ expectedLast \in Ids \cup {NoId}
    /\ beforeProjection \in ProjectionValues
    /\ lastAction \in
         {"Init", "LookupSuccess", "LookupFailure",
          "RemoveSuccess", "RemoveFailure", "Clear"}

PublicProjectionEquivalent == CurrentSpecProjection = CurrentImplProjection

ResultEquivalent == specResult = implResult

LastCollisionOccurrenceEquivalent ==
    lastAction \in {"Init", "Clear"} \/ chosenId = expectedLast

LookupReadOnly ==
    ~(lastAction \in {"LookupSuccess", "LookupFailure"})
    \/ CurrentImplProjection = beforeProjection

FailedRemovePreservesProjection ==
    lastAction # "RemoveFailure"
    \/ CurrentImplProjection = beforeProjection

SuccessfulRemoveAccountingEquivalent ==
    lastAction # "RemoveSuccess"
    \/ /\ chosenId = expectedLast
       /\ implBucket = RemoveId(beforeProjection.bucket, expectedLast)
       /\ implLive = LiveOf(implBucket)
       /\ implResidency = beforeProjection.residency \ {expectedLast}
       /\ implTotalBytes = beforeProjection.totalBytes - SizeOf[expectedLast]
       /\ implTypeCounts =
            [beforeProjection.typeCounts EXCEPT
               ![TypeOf[expectedLast]] = @ - 1]

AuthorityUnaffectedByLookupOrRemove ==
    /\ specAuthority = InitialAuthority
    /\ implAuthority = InitialAuthority

ResultHasOnlyDetachedCapability(result) ==
    result = NoResult \/ \E id \in Ids : result = Owned(id)

MaterializedResultNeverAuthorizesExact ==
    /\ ResultHasOnlyDetachedCapability(specResult)
    /\ ResultHasOnlyDetachedCapability(implResult)

Init ==
    /\ specBucket = InitialBucket
    /\ implBucket = InitialBucket
    /\ specLive = LiveOf(InitialBucket)
    /\ implLive = LiveOf(InitialBucket)
    /\ specResidency = LiveOf(InitialBucket)
    /\ implResidency = LiveOf(InitialBucket)
    /\ specTotalBytes = SequenceBytes(InitialBucket)
    /\ implTotalBytes = SequenceBytes(InitialBucket)
    /\ specTypeCounts = TypeCountsOf(InitialBucket)
    /\ implTypeCounts = TypeCountsOf(InitialBucket)
    /\ specResult = NoResult
    /\ implResult = NoResult
    /\ specAuthority = InitialAuthority
    /\ implAuthority = InitialAuthority
    /\ chosenId = NoId
    /\ expectedLast = NoId
    /\ beforeProjection =
         Projection(LiveOf(InitialBucket), InitialBucket,
                    LiveOf(InitialBucket), SequenceBytes(InitialBucket),
                    TypeCountsOf(InitialBucket), InitialAuthority)
    /\ lastAction = "Init"

LookupSuccess ==
    LET specChoice == LastId(specBucket)
        implChoice == SelectedId(implBucket)
    IN  /\ Len(specBucket) > 0
        /\ Len(implBucket) > 0
        /\ specChoice \in Materializable
        /\ implChoice \in Materializable
        /\ specResult' = Owned(specChoice)
        /\ implResult' = Owned(implChoice)
        /\ chosenId' = implChoice
        /\ expectedLast' = specChoice
        /\ beforeProjection' = CurrentImplProjection
        /\ lastAction' = "LookupSuccess"
        /\ UNCHANGED <<specLive, implLive, specBucket, implBucket,
                        specResidency, implResidency,
                        specTotalBytes, implTotalBytes,
                        specTypeCounts, implTypeCounts,
                        specAuthority, implAuthority>>

LookupFailure ==
    LET specChoice == LastId(specBucket)
        implChoice == SelectedId(implBucket)
    IN  /\ Len(specBucket) > 0
        /\ Len(implBucket) > 0
        /\ specResult' = NoResult
        /\ implResult' = NoResult
        /\ chosenId' = implChoice
        /\ expectedLast' = specChoice
        /\ beforeProjection' = CurrentImplProjection
        /\ lastAction' = "LookupFailure"
        /\ UNCHANGED <<specLive, implLive, specBucket, implBucket,
                        specResidency, implResidency,
                        specTotalBytes, implTotalBytes,
                        specTypeCounts, implTypeCounts,
                        specAuthority, implAuthority>>

RemoveSuccess ==
    LET specChoice == LastId(specBucket)
        implChoice == SelectedId(implBucket)
        nextSpecBucket == RemoveId(specBucket, specChoice)
        nextImplBucket == RemoveId(implBucket, implChoice)
    IN  /\ Len(specBucket) > 0
        /\ Len(implBucket) > 0
        /\ specChoice \in Materializable
        /\ implChoice \in Materializable
        /\ specBucket' = nextSpecBucket
        /\ implBucket' = nextImplBucket
        /\ specLive' = LiveOf(nextSpecBucket)
        /\ implLive' = LiveOf(nextImplBucket)
        /\ specResidency' = specResidency \ {specChoice}
        /\ implResidency' = implResidency \ {implChoice}
        /\ specTotalBytes' = specTotalBytes - SizeOf[specChoice]
        /\ implTotalBytes' = implTotalBytes - SizeOf[implChoice]
        /\ specTypeCounts' =
             [specTypeCounts EXCEPT ![TypeOf[specChoice]] = @ - 1]
        /\ implTypeCounts' =
             [implTypeCounts EXCEPT ![TypeOf[implChoice]] = @ - 1]
        /\ specResult' = Owned(specChoice)
        /\ implResult' = Owned(implChoice)
        /\ chosenId' = implChoice
        /\ expectedLast' = specChoice
        /\ beforeProjection' = CurrentImplProjection
        /\ lastAction' = "RemoveSuccess"
        /\ UNCHANGED <<specAuthority, implAuthority>>

RemoveMaterializationFailure ==
    LET specChoice == LastId(specBucket)
        implChoice == SelectedId(implBucket)
        nextImplBucket == RemoveId(implBucket, implChoice)
    IN  /\ Len(specBucket) > 0
        /\ Len(implBucket) > 0
        /\ specResult' = NoResult
        /\ implResult' = NoResult
        /\ specBucket' = specBucket
        /\ specLive' = specLive
        /\ specResidency' = specResidency
        /\ specTotalBytes' = specTotalBytes
        /\ specTypeCounts' = specTypeCounts
        /\ implBucket' =
             IF UnsafeMutateBeforeMaterialize THEN nextImplBucket ELSE implBucket
        /\ implLive' =
             IF UnsafeMutateBeforeMaterialize
             THEN LiveOf(nextImplBucket)
             ELSE implLive
        /\ implResidency' =
             IF UnsafeMutateBeforeMaterialize
             THEN implResidency \ {implChoice}
             ELSE implResidency
        /\ implTotalBytes' =
             IF UnsafeMutateBeforeMaterialize
             THEN implTotalBytes - SizeOf[implChoice]
             ELSE implTotalBytes
        /\ implTypeCounts' =
             IF UnsafeMutateBeforeMaterialize
             THEN [implTypeCounts EXCEPT ![TypeOf[implChoice]] = @ - 1]
             ELSE implTypeCounts
        /\ chosenId' = implChoice
        /\ expectedLast' = specChoice
        /\ beforeProjection' = CurrentImplProjection
        /\ lastAction' = "RemoveFailure"
        /\ UNCHANGED <<specAuthority, implAuthority>>

ClearObservation ==
    /\ specResult' = NoResult
    /\ implResult' = NoResult
    /\ chosenId' = NoId
    /\ expectedLast' = NoId
    /\ beforeProjection' = CurrentImplProjection
    /\ lastAction' = "Clear"
    /\ UNCHANGED <<specLive, implLive, specBucket, implBucket,
                    specResidency, implResidency,
                    specTotalBytes, implTotalBytes,
                    specTypeCounts, implTypeCounts,
                    specAuthority, implAuthority>>

Next ==
    \/ LookupSuccess
    \/ LookupFailure
    \/ RemoveSuccess
    \/ RemoveMaterializationFailure
    \/ ClearObservation

Spec == Init /\ [][Next]_vars

=============================================================================
