------------------ MODULE VariableWidthVocabularyPublication ------------------
EXTENDS Naturals, Sequences, FiniteSets, TLC

(***************************************************************************)
(* Exact dictionary-local publication and recovery model.                  *)
(*                                                                         *)
(* The two generations and two ID positions are finite TLC counterexample  *)
(* bounds. They are not library key, depth, atom, result, or work limits.   *)
(* Functional generality and open carrier widths are proved in Rocq.       *)
(*                                                                         *)
(* Each generation follows the same staged protocol:                       *)
(*                                                                         *)
(*   build exact canonical metadata                                        *)
(*   -> durabilize each live ID                                             *)
(*   -> seal sparse vocabulary eligibility                                 *)
(*   -> expose and durabilize the dependent sequence                       *)
(*   -> write/sync immutable vocabulary and sequence objects                *)
(*   -> atomically replace the head                                         *)
(*                                                                         *)
(* AllocatorHighWater is deliberately independent from LiveIds. Generation *)
(* one publishes two adjacent nonempty spans, while generation two         *)
(* publishes ID 0 with ID 1 as a legal gap. No invariant interprets a       *)
(* high-water mark as a dense allocation claim. Every referenced ID is     *)
(* checked for exact membership and exact canonical payload/span ownership. *)
(*                                                                         *)
(* Durable objects are keyed by immutable object identity. The atomic head *)
(* refers to one exact vocabulary/sequence pair. Old heads and objects are  *)
(* retained for captured readers. Recovery returns the exact current head  *)
(* pair or an explicit error; it never synthesizes an empty dictionary.     *)
(* Availability and corruption are tracked independently for vocabulary    *)
(* and sequence objects.                                                    *)
(*                                                                         *)
(* The four Boolean constants are isolated negative controls. Each changes *)
(* exactly one protocol decision and is paired with its own TLC config.     *)
(***************************************************************************)

CONSTANTS PublishSequenceBeforeVocabulary,
          OverclaimVocabularyFrontier,
          AllowCrossGenerationResume,
          MissingVocabularyAsEmpty

ASSUME /\ PublishSequenceBeforeVocabulary \in BOOLEAN
       /\ OverclaimVocabularyFrontier \in BOOLEAN
       /\ AllowCrossGenerationResume \in BOOLEAN
       /\ MissingVocabularyAsEmpty \in BOOLEAN

Generations == 1..2
Atoms == {"Alpha", "Beta"}
Ids == 0..1
NoAtom == "NoAtom"
AtomOrNone == Atoms \cup {NoAtom}

NoFiber == [identity |-> "None",
            generation |-> 0,
            atomProfile |-> "None",
            codec |-> "None",
            layout |-> "None",
            abiVersion |-> 0,
            carrierFormat |-> 0,
            carrierWidth |-> 0]

Fiber(generation) ==
  [identity |-> "Vocabulary-A",
   generation |-> generation,
   atomProfile |-> "CanonicalULEB",
   codec |-> "CanonicalULEB-v1",
   layout |-> "LogicalUnit-v1",
   abiVersion |-> 1,
   carrierFormat |-> 32,
   carrierWidth |-> 4]

Fibers == {Fiber(g) : g \in Generations}
FiberOrNone == Fibers \cup {NoFiber}

NoTermFiber ==
  [vocabularyFiber |-> NoFiber,
   identity |-> "None",
   generation |-> 0,
   carrierFormat |-> 0,
   carrierWidth |-> 0]

TermFiber(generation) ==
  [vocabularyFiber |-> Fiber(generation),
   identity |-> "TermDictionary-A",
   generation |-> generation,
   carrierFormat |-> 32,
   carrierWidth |-> 4]

TermFibers == {TermFiber(g) : g \in Generations}
TermFiberOrNone == TermFibers \cup {NoTermFiber}

CanonicalBytes(atom) ==
  CASE atom = "Alpha" -> <<129, 1>>
    [] atom = "Beta"  -> <<130, 1>>
    [] OTHER          -> <<>>

CanonicalUlebCodeword(bytes) ==
  /\ Len(bytes) > 0
  /\ \A index \in 1..Len(bytes) : bytes[index] \in 0..255
  /\ bytes[Len(bytes)] < 128
  /\ \A index \in 1..(Len(bytes) - 1) : bytes[index] >= 128
  /\ (Len(bytes) = 1 \/ bytes[Len(bytes)] # 0)

DescriptorCanonicalCodeword(fiber, atom, bytes) ==
  /\ fiber.atomProfile = "CanonicalULEB"
  /\ fiber.codec = "CanonicalULEB-v1"
  /\ fiber.layout = "LogicalUnit-v1"
  /\ fiber.abiVersion = 1
  /\ atom \in Atoms
  /\ bytes = CanonicalBytes(atom)
  /\ CanonicalUlebCodeword(bytes)

GenerationLiveIds(generation) ==
  IF generation = 1 THEN Ids ELSE {0}

GenerationAtom(generation, id) ==
  IF generation = 1 /\ id = 0 THEN "Alpha"
  ELSE IF generation = 1 /\ id = 1 THEN "Beta"
  ELSE IF generation = 2 /\ id = 0 THEN "Beta"
  ELSE NoAtom

GenerationSequence(generation) ==
  IF generation = 1 THEN <<0, 1>> ELSE <<0>>

EmptyAtomMap == [id \in Ids |-> NoAtom]
EmptyPayloadMap == [id \in Ids |-> <<>>]
NoSpan == [owner |-> NoAtom, offset |-> 0, length |-> 0]
EmptySpanMap == [id \in Ids |-> NoSpan]

GenerationAtomMap(generation) ==
  [id \in Ids |-> GenerationAtom(generation, id)]

GenerationPayloadMap(generation) ==
  [id \in Ids |->
    IF id \in GenerationLiveIds(generation)
    THEN CanonicalBytes(GenerationAtom(generation, id))
    ELSE <<>>]

GenerationPackedBytes(generation) ==
  IF generation = 1
  THEN CanonicalBytes("Alpha") \o CanonicalBytes("Beta")
  ELSE CanonicalBytes("Beta")

GenerationSpanMap(generation) ==
  [id \in Ids |->
    IF id \in GenerationLiveIds(generation)
    THEN [owner |-> GenerationAtom(generation, id),
          offset |->
            IF generation = 1 /\ id = 1 THEN 2 ELSE 0,
          length |-> Len(CanonicalBytes(GenerationAtom(generation, id)))]
    ELSE NoSpan]

IdsBelow(highWater) == {id \in Ids : id < highWater}
SequenceIdSet(sequence) ==
  {sequence[index] : index \in 1..Len(sequence)}

ObjectPhases == {"Absent", "Written", "Durable"}
VocabObjectIds == {"V1", "V2"}
SequenceObjectIds == {"S1", "S2"}
NoVocabObjectId == "NoVocab"
NoSequenceObjectId == "NoSequence"

VocabObjectId(generation) ==
  IF generation = 1 THEN "V1" ELSE "V2"

SequenceObjectId(generation) ==
  IF generation = 1 THEN "S1" ELSE "S2"

AtomMapType == [Ids -> AtomOrNone]
PayloadMapType == [Ids -> Seq(0..255)]
SpanValueType ==
  [owner : AtomOrNone, offset : 0..4, length : 0..4]
SpanMapType == [Ids -> SpanValueType]

EmptyWork ==
  [generation |-> 0,
   fiber |-> NoFiber,
   termFiber |-> NoTermFiber,
   allocatorHighWater |-> 0,
   liveIds |-> {},
   atomById |-> EmptyAtomMap,
   payloadById |-> EmptyPayloadMap,
   spanById |-> EmptySpanMap,
   packedBytes |-> <<>>,
   durableIds |-> {},
   durableHighWater |-> 0,
   publishedHighWater |-> 0,
   publishedLiveIds |-> {},
   sequenceStaged |-> FALSE,
   sequenceIds |-> <<>>,
   sequenceFiber |-> NoFiber,
   sequenceRequiredHighWater |-> 0,
   sequenceVisible |-> FALSE,
   sequenceDurable |-> FALSE,
   termEnabled |-> FALSE,
   termId |-> 0,
   termSequence |-> <<>>]

WorkFor(generation, enableTermDictionary) ==
  [generation |-> generation,
   fiber |-> Fiber(generation),
   termFiber |-> TermFiber(generation),
   allocatorHighWater |-> 2,
   liveIds |-> GenerationLiveIds(generation),
   atomById |-> GenerationAtomMap(generation),
   payloadById |-> GenerationPayloadMap(generation),
   spanById |-> GenerationSpanMap(generation),
   packedBytes |-> GenerationPackedBytes(generation),
   durableIds |-> {},
   durableHighWater |-> 0,
   publishedHighWater |-> 0,
   publishedLiveIds |-> {},
   sequenceStaged |-> FALSE,
   sequenceIds |-> <<>>,
   sequenceFiber |-> NoFiber,
   sequenceRequiredHighWater |-> 0,
   sequenceVisible |-> FALSE,
   sequenceDurable |-> FALSE,
   termEnabled |-> enableTermDictionary,
   termId |-> 0,
   termSequence |->
     IF enableTermDictionary THEN GenerationSequence(generation) ELSE <<>>]

CompletedTermWorkFor(generation) ==
  [WorkFor(generation, TRUE) EXCEPT
    !.durableIds = GenerationLiveIds(generation),
    !.durableHighWater = 2,
    !.publishedHighWater = 2,
    !.publishedLiveIds = GenerationLiveIds(generation),
    !.sequenceStaged = TRUE,
    !.sequenceIds = GenerationSequence(generation),
    !.sequenceFiber = Fiber(generation),
    !.sequenceRequiredHighWater = 2,
    !.sequenceVisible = TRUE,
    !.sequenceDurable = TRUE]

WorkType ==
  [generation : 0..2,
   fiber : FiberOrNone,
   termFiber : TermFiberOrNone,
   allocatorHighWater : 0..2,
   liveIds : SUBSET Ids,
   atomById : AtomMapType,
   payloadById : PayloadMapType,
   spanById : SpanMapType,
   packedBytes : Seq(0..255),
   durableIds : SUBSET Ids,
   durableHighWater : 0..2,
   publishedHighWater : 0..2,
   publishedLiveIds : SUBSET Ids,
   sequenceStaged : BOOLEAN,
   sequenceIds : Seq(Ids),
   sequenceFiber : FiberOrNone,
   sequenceRequiredHighWater : 0..2,
   sequenceVisible : BOOLEAN,
   sequenceDurable : BOOLEAN,
   termEnabled : BOOLEAN,
   termId : 0..2,
   termSequence : Seq(Ids)]

EmptyVocabObject ==
  [present |-> FALSE,
   phase |-> "Absent",
   generation |-> 0,
   fiber |-> NoFiber,
   allocatorHighWater |-> 0,
   liveIds |-> {},
   atomById |-> EmptyAtomMap,
   payloadById |-> EmptyPayloadMap,
   spanById |-> EmptySpanMap,
   packedBytes |-> <<>>]

VocabObjectFromWork(phase, working) ==
  [present |-> TRUE,
   phase |-> phase,
   generation |-> working.generation,
   fiber |-> working.fiber,
   allocatorHighWater |-> working.publishedHighWater,
   liveIds |-> working.publishedLiveIds,
   atomById |-> working.atomById,
   payloadById |-> working.payloadById,
   spanById |-> working.spanById,
   packedBytes |-> working.packedBytes]

VocabObjectType ==
  [present : BOOLEAN,
   phase : ObjectPhases,
   generation : 0..2,
   fiber : FiberOrNone,
   allocatorHighWater : 0..2,
   liveIds : SUBSET Ids,
   atomById : AtomMapType,
   payloadById : PayloadMapType,
   spanById : SpanMapType,
   packedBytes : Seq(0..255)]

EmptySequenceObject ==
  [present |-> FALSE,
   phase |-> "Absent",
   generation |-> 0,
   fiber |-> NoFiber,
   termFiber |-> NoTermFiber,
   requiredHighWater |-> 0,
   ids |-> <<>>,
   termEnabled |-> FALSE,
   termId |-> 0,
   termSequence |-> <<>>]

SequenceObjectFromWork(phase, working) ==
  [present |-> TRUE,
   phase |-> phase,
   generation |-> working.generation,
   fiber |-> working.sequenceFiber,
   termFiber |-> working.termFiber,
   requiredHighWater |-> working.sequenceRequiredHighWater,
   ids |-> working.sequenceIds,
   termEnabled |-> working.termEnabled,
   termId |-> working.termId,
   termSequence |-> working.termSequence]

SequenceObjectType ==
  [present : BOOLEAN,
   phase : ObjectPhases,
   generation : 0..2,
   fiber : FiberOrNone,
   termFiber : TermFiberOrNone,
   requiredHighWater : 0..2,
   ids : Seq(Ids),
   termEnabled : BOOLEAN,
   termId : 0..2,
   termSequence : Seq(Ids)]

NoHead ==
  [present |-> FALSE,
   generation |-> 0,
   vocabObject |-> NoVocabObjectId,
   sequenceObject |-> NoSequenceObjectId]

HeadFor(generation) ==
  [present |-> TRUE,
   generation |-> generation,
   vocabObject |-> VocabObjectId(generation),
   sequenceObject |-> SequenceObjectId(generation)]

HeadType ==
  [present : BOOLEAN,
   generation : 0..2,
   vocabObject : VocabObjectIds \cup {NoVocabObjectId},
   sequenceObject : SequenceObjectIds \cup {NoSequenceObjectId}]

EmptyObservation ==
  [present |-> FALSE,
   head |-> NoHead,
   vocabulary |-> EmptyVocabObject,
   sequence |-> EmptySequenceObject]

ObservationType ==
  [present : BOOLEAN,
   head : HeadType,
   vocabulary : VocabObjectType,
   sequence : SequenceObjectType]

EmptyReader ==
  [captured |-> FALSE,
   head |-> NoHead,
   initialObservation |-> EmptyObservation,
   continuationSaved |-> FALSE,
   resumed |-> FALSE,
   resumeObservation |-> EmptyObservation]

ReaderType ==
  [captured : BOOLEAN,
   head : HeadType,
   initialObservation : ObservationType,
   continuationSaved : BOOLEAN,
   resumed : BOOLEAN,
   resumeObservation : ObservationType]

ReadPackedSpan(packedBytes, span) ==
  SubSeq(
    packedBytes,
    span.offset + 1,
    span.offset + span.length)

SpansDisjoint(left, right) ==
  left.offset + left.length <= right.offset \/
  right.offset + right.length <= left.offset

SpanContainsOffset(span, offset) ==
  /\ span.offset <= offset
  /\ offset < span.offset + span.length

ExactIdMetadata(fiber, atomMap, payloadMap, spanMap, packedBytes, id) ==
  /\ atomMap[id] \in Atoms
  /\ payloadMap[id] = CanonicalBytes(atomMap[id])
  /\ payloadMap[id] # <<>>
  /\ DescriptorCanonicalCodeword(fiber, atomMap[id], payloadMap[id])
  /\ spanMap[id].owner = atomMap[id]
  /\ spanMap[id].length = Len(payloadMap[id])
  /\ 0 < spanMap[id].length
  /\ spanMap[id].offset + spanMap[id].length <= Len(packedBytes)
  /\ ReadPackedSpan(packedBytes, spanMap[id]) = payloadMap[id]

ExactPackedMetadata(fiber, atomMap, payloadMap, spanMap, packedBytes, liveIds) ==
  /\ \A id \in liveIds :
       ExactIdMetadata(fiber, atomMap, payloadMap, spanMap, packedBytes, id)
  /\ \A left \in liveIds, right \in liveIds :
       left # right => SpansDisjoint(spanMap[left], spanMap[right])
  /\ \A offset \in 0..(Len(packedBytes) - 1) :
       Cardinality(
         {id \in liveIds : SpanContainsOffset(spanMap[id], offset)}) = 1

ExactWorkingVocabulary(working) ==
  /\ working.generation \in Generations
  /\ working.fiber = Fiber(working.generation)
  /\ working.termFiber = TermFiber(working.generation)
  /\ working.termFiber.vocabularyFiber = working.fiber
  /\ working.allocatorHighWater = 2
  /\ working.liveIds = GenerationLiveIds(working.generation)
  /\ working.atomById = GenerationAtomMap(working.generation)
  /\ working.payloadById = GenerationPayloadMap(working.generation)
  /\ working.spanById = GenerationSpanMap(working.generation)
  /\ working.packedBytes = GenerationPackedBytes(working.generation)
  /\ ExactPackedMetadata(
       working.fiber,
       working.atomById,
       working.payloadById,
       working.spanById,
       working.packedBytes,
       working.liveIds)
  /\ \A id \in working.liveIds :
       /\ id < working.allocatorHighWater
       /\ ExactIdMetadata(
            working.fiber,
            working.atomById,
            working.payloadById,
            working.spanById,
            working.packedBytes,
            id)

ExactVocabObject(object) ==
  /\ object.present
  /\ object.generation \in Generations
  /\ object.fiber = Fiber(object.generation)
  /\ object.allocatorHighWater = 2
  /\ object.liveIds = GenerationLiveIds(object.generation)
  /\ object.atomById = GenerationAtomMap(object.generation)
  /\ object.payloadById = GenerationPayloadMap(object.generation)
  /\ object.spanById = GenerationSpanMap(object.generation)
  /\ object.packedBytes = GenerationPackedBytes(object.generation)
  /\ ExactPackedMetadata(
       object.fiber,
       object.atomById,
       object.payloadById,
       object.spanById,
       object.packedBytes,
       object.liveIds)
  /\ \A id \in object.liveIds :
       /\ id < object.allocatorHighWater
       /\ ExactIdMetadata(
            object.fiber,
            object.atomById,
            object.payloadById,
            object.spanById,
            object.packedBytes,
            id)

ExactSequenceObject(object) ==
  /\ object.present
  /\ object.generation \in Generations
  /\ object.fiber = Fiber(object.generation)
  /\ object.termFiber = TermFiber(object.generation)
  /\ object.termFiber.vocabularyFiber = object.fiber
  /\ object.requiredHighWater = 2
  /\ object.ids = GenerationSequence(object.generation)
  /\ IF object.termEnabled
     THEN /\ object.termId = 0
          /\ object.termSequence = object.ids
     ELSE /\ object.termId = 0
          /\ object.termSequence = <<>>

HeadCoherent(vocabularyStore, sequenceStore, candidateHead) ==
  /\ candidateHead.present
  /\ candidateHead.generation \in Generations
  /\ candidateHead.vocabObject =
       VocabObjectId(candidateHead.generation)
  /\ candidateHead.sequenceObject =
       SequenceObjectId(candidateHead.generation)
  /\ LET vocabulary == vocabularyStore[candidateHead.vocabObject]
         sequence == sequenceStore[candidateHead.sequenceObject]
     IN /\ vocabulary.phase = "Durable"
        /\ sequence.phase = "Durable"
        /\ ExactVocabObject(vocabulary)
        /\ ExactSequenceObject(sequence)
        /\ vocabulary.generation = candidateHead.generation
        /\ sequence.generation = candidateHead.generation
        /\ sequence.fiber = vocabulary.fiber
        /\ sequence.requiredHighWater <= vocabulary.allocatorHighWater
        /\ \A id \in SequenceIdSet(sequence.ids) :
             /\ id \in vocabulary.liveIds
             /\ id < sequence.requiredHighWater
             /\ ExactIdMetadata(
                  vocabulary.fiber,
                  vocabulary.atomById,
                  vocabulary.payloadById,
                  vocabulary.spanById,
                  vocabulary.packedBytes,
                  id)

ObserveHead(vocabularyStore, sequenceStore, candidateHead) ==
  IF ~candidateHead.present
  THEN EmptyObservation
  ELSE LET vocabulary == vocabularyStore[candidateHead.vocabObject]
           sequence == sequenceStore[candidateHead.sequenceObject]
       IN [present |-> TRUE,
           head |-> candidateHead,
           vocabulary |-> vocabulary,
           sequence |-> sequence]

VARIABLES work,
          vocabObjects,
          sequenceObjects,
          head,
          retainedHeads,
          availableVocabObjects,
          availableSequenceObjects,
          corruptVocabObjects,
          corruptSequenceObjects,
          reader,
          crashed,
          recoveryAttempted,
          recoveryKind,
          recoveredHead

vars ==
  <<work, vocabObjects, sequenceObjects, head, retainedHeads,
    availableVocabObjects, availableSequenceObjects,
    corruptVocabObjects, corruptSequenceObjects, reader, crashed,
    recoveryAttempted, recoveryKind, recoveredHead>>

TypeOK ==
  /\ work \in WorkType
  /\ vocabObjects \in [VocabObjectIds -> VocabObjectType]
  /\ sequenceObjects \in [SequenceObjectIds -> SequenceObjectType]
  /\ head \in HeadType
  /\ retainedHeads \in SUBSET HeadType
  /\ availableVocabObjects \in SUBSET VocabObjectIds
  /\ availableSequenceObjects \in SUBSET SequenceObjectIds
  /\ corruptVocabObjects \in SUBSET VocabObjectIds
  /\ corruptSequenceObjects \in SUBSET SequenceObjectIds
  /\ reader \in ReaderType
  /\ crashed \in BOOLEAN
  /\ recoveryAttempted \in BOOLEAN
  /\ recoveryKind \in {"None", "Pair", "Error", "Empty"}
  /\ recoveredHead \in HeadType

Init ==
  /\ work = EmptyWork
  /\ vocabObjects =
       [objectId \in VocabObjectIds |-> EmptyVocabObject]
  /\ sequenceObjects =
       [objectId \in SequenceObjectIds |-> EmptySequenceObject]
  /\ head = NoHead
  /\ retainedHeads = {}
  /\ availableVocabObjects = {}
  /\ availableSequenceObjects = {}
  /\ corruptVocabObjects = {}
  /\ corruptSequenceObjects = {}
  /\ reader = EmptyReader
  /\ crashed = FALSE
  /\ recoveryAttempted = FALSE
  /\ recoveryKind = "None"
  /\ recoveredHead = NoHead

TermFiberWitnessInit ==
  /\ work = CompletedTermWorkFor(2)
  /\ vocabObjects =
       [objectId \in VocabObjectIds |->
         CASE objectId = VocabObjectId(1) ->
                VocabObjectFromWork("Durable", CompletedTermWorkFor(1))
           [] OTHER ->
                VocabObjectFromWork("Durable", CompletedTermWorkFor(2))]
  /\ sequenceObjects =
       [objectId \in SequenceObjectIds |->
         CASE objectId = SequenceObjectId(1) ->
                SequenceObjectFromWork("Durable", CompletedTermWorkFor(1))
           [] OTHER ->
                SequenceObjectFromWork("Durable", CompletedTermWorkFor(2))]
  /\ head = HeadFor(2)
  /\ retainedHeads = {HeadFor(1), HeadFor(2)}
  /\ availableVocabObjects = VocabObjectIds
  /\ availableSequenceObjects = SequenceObjectIds
  /\ corruptVocabObjects = {}
  /\ corruptSequenceObjects = {}
  /\ reader = EmptyReader
  /\ crashed = FALSE
  /\ recoveryAttempted = FALSE
  /\ recoveryKind = "None"
  /\ recoveredHead = NoHead

BeginGeneration(generation, enableTermDictionary) ==
  /\ ~crashed
  /\ generation \in Generations
  /\ enableTermDictionary \in BOOLEAN
  /\ IF generation = 1
     THEN work.generation = 0
     ELSE /\ head = HeadFor(1)
          /\ work.generation = 1
  /\ work' = WorkFor(generation, enableTermDictionary)
  /\ UNCHANGED
       <<vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, reader,
         crashed, recoveryAttempted, recoveryKind, recoveredHead>>

DurabilizeLiveId(id) ==
  /\ ~crashed
  /\ ExactWorkingVocabulary(work)
  /\ id \in work.liveIds
  /\ id \notin work.durableIds
  /\ ExactIdMetadata(
       work.fiber,
       work.atomById, work.payloadById, work.spanById,
       work.packedBytes, id)
  /\ work' = [work EXCEPT !.durableIds = @ \cup {id}]
  /\ UNCHANGED
       <<vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, reader,
         crashed, recoveryAttempted, recoveryKind, recoveredHead>>

SealDurableVocabulary ==
  /\ ~crashed
  /\ ExactWorkingVocabulary(work)
  /\ work.durableHighWater = 0
  /\ work.liveIds \subseteq work.durableIds
  /\ work' =
       [work EXCEPT !.durableHighWater = work.allocatorHighWater]
  /\ UNCHANGED
       <<vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, reader,
         crashed, recoveryAttempted, recoveryKind, recoveredHead>>

PublishVocabularyEligibility ==
  /\ ~crashed
  /\ ExactWorkingVocabulary(work)
  /\ work.publishedHighWater = 0
  /\ (OverclaimVocabularyFrontier \/
       work.durableHighWater = work.allocatorHighWater)
  /\ work' =
       [work EXCEPT
         !.publishedHighWater = work.allocatorHighWater,
         !.publishedLiveIds = work.liveIds]
  /\ UNCHANGED
       <<vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, reader,
         crashed, recoveryAttempted, recoveryKind, recoveredHead>>

StageDependentSequence ==
  /\ ~crashed
  /\ ExactWorkingVocabulary(work)
  /\ ~work.sequenceStaged
  /\ work' =
       [work EXCEPT
         !.sequenceStaged = TRUE,
         !.sequenceIds = GenerationSequence(work.generation),
         !.sequenceFiber = work.fiber,
         !.sequenceRequiredHighWater = work.allocatorHighWater]
  /\ UNCHANGED
       <<vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, reader,
         crashed, recoveryAttempted, recoveryKind, recoveredHead>>

PublishSequenceVisibility ==
  /\ ~crashed
  /\ work.sequenceStaged
  /\ ~work.sequenceVisible
  /\ (PublishSequenceBeforeVocabulary \/
       (work.sequenceFiber = work.fiber /\
        work.sequenceRequiredHighWater <= work.publishedHighWater /\
        \A id \in SequenceIdSet(work.sequenceIds) :
          id \in work.publishedLiveIds))
  /\ work' = [work EXCEPT !.sequenceVisible = TRUE]
  /\ UNCHANGED
       <<vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, reader,
         crashed, recoveryAttempted, recoveryKind, recoveredHead>>

DurabilizeDependentSequence ==
  /\ ~crashed
  /\ work.sequenceStaged
  /\ ~work.sequenceDurable
  /\ (PublishSequenceBeforeVocabulary \/
       (work.sequenceVisible /\
        work.sequenceFiber = work.fiber /\
        work.sequenceRequiredHighWater <= work.durableHighWater /\
        \A id \in SequenceIdSet(work.sequenceIds) :
          /\ id \in work.durableIds
          /\ id \in work.liveIds
          /\ ExactIdMetadata(
               work.fiber,
               work.atomById, work.payloadById, work.spanById,
               work.packedBytes, id)))
  /\ work' = [work EXCEPT !.sequenceDurable = TRUE]
  /\ UNCHANGED
       <<vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, reader,
         crashed, recoveryAttempted, recoveryKind, recoveredHead>>

WriteVocabularyObject ==
  /\ ~crashed
  /\ ExactWorkingVocabulary(work)
  /\ work.publishedHighWater = work.allocatorHighWater
  /\ work.publishedLiveIds = work.liveIds
  /\ work.liveIds \subseteq work.durableIds
  /\ vocabObjects[VocabObjectId(work.generation)].phase = "Absent"
  /\ vocabObjects' =
       [vocabObjects EXCEPT
         ![VocabObjectId(work.generation)] =
           VocabObjectFromWork("Written", work)]
  /\ UNCHANGED
       <<work, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, reader,
         crashed, recoveryAttempted, recoveryKind, recoveredHead>>

SyncVocabularyObject ==
  /\ ~crashed
  /\ LET objectId == VocabObjectId(work.generation)
         object == vocabObjects[objectId]
     IN /\ object.phase = "Written"
        /\ ExactVocabObject(object)
        /\ vocabObjects' =
             [vocabObjects EXCEPT ![objectId].phase = "Durable"]
        /\ availableVocabObjects' =
             availableVocabObjects \cup {objectId}
  /\ UNCHANGED
       <<work, sequenceObjects, head, retainedHeads,
         availableSequenceObjects, corruptVocabObjects,
         corruptSequenceObjects, reader, crashed, recoveryAttempted,
         recoveryKind, recoveredHead>>

WriteSequenceObject ==
  /\ ~crashed
  /\ work.sequenceDurable
  /\ sequenceObjects[SequenceObjectId(work.generation)].phase = "Absent"
  /\ (PublishSequenceBeforeVocabulary \/
       vocabObjects[VocabObjectId(work.generation)].phase = "Durable")
  /\ sequenceObjects' =
       [sequenceObjects EXCEPT
         ![SequenceObjectId(work.generation)] =
           SequenceObjectFromWork("Written", work)]
  /\ UNCHANGED
       <<work, vocabObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, reader,
         crashed, recoveryAttempted, recoveryKind, recoveredHead>>

SyncSequenceObject ==
  /\ ~crashed
  /\ LET objectId == SequenceObjectId(work.generation)
         object == sequenceObjects[objectId]
     IN /\ object.phase = "Written"
        /\ ExactSequenceObject(object)
        /\ sequenceObjects' =
             [sequenceObjects EXCEPT ![objectId].phase = "Durable"]
        /\ availableSequenceObjects' =
             availableSequenceObjects \cup {objectId}
  /\ UNCHANGED
       <<work, vocabObjects, head, retainedHeads,
         availableVocabObjects, corruptVocabObjects,
         corruptSequenceObjects, reader, crashed, recoveryAttempted,
         recoveryKind, recoveredHead>>

PublishCheckpointHead ==
  /\ ~crashed
  /\ LET newHead == HeadFor(work.generation)
     IN /\ HeadCoherent(vocabObjects, sequenceObjects, newHead)
        /\ head' = newHead
        /\ retainedHeads' = retainedHeads \cup {newHead}
  /\ UNCHANGED
       <<work, vocabObjects, sequenceObjects,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, reader,
         crashed, recoveryAttempted, recoveryKind, recoveredHead>>

CaptureReader ==
  /\ ~crashed
  /\ head = HeadFor(1)
  /\ ~reader.captured
  /\ reader' =
       [reader EXCEPT
         !.captured = TRUE,
         !.head = head,
         !.initialObservation =
           ObserveHead(vocabObjects, sequenceObjects, head)]
  /\ UNCHANGED
       <<work, vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, crashed,
         recoveryAttempted, recoveryKind, recoveredHead>>

SaveReaderContinuation ==
  /\ ~crashed
  /\ reader.captured
  /\ ~reader.continuationSaved
  /\ reader' = [reader EXCEPT !.continuationSaved = TRUE]
  /\ UNCHANGED
       <<work, vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, crashed,
         recoveryAttempted, recoveryKind, recoveredHead>>

ResumeCapturedReader ==
  /\ ~crashed
  /\ reader.continuationSaved
  /\ ~reader.resumed
  /\ head = HeadFor(2)
  /\ reader' =
       [reader EXCEPT
         !.resumed = TRUE,
         !.resumeObservation =
           IF AllowCrossGenerationResume
           THEN ObserveHead(vocabObjects, sequenceObjects, head)
           ELSE ObserveHead(vocabObjects, sequenceObjects, reader.head)]
  /\ UNCHANGED
       <<work, vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, crashed,
         recoveryAttempted, recoveryKind, recoveredHead>>

LoseHeadVocabularyArtifact ==
  /\ ~crashed
  /\ head.present
  /\ head.vocabObject \in availableVocabObjects
  /\ availableVocabObjects' =
       availableVocabObjects \ {head.vocabObject}
  /\ UNCHANGED
       <<work, vocabObjects, sequenceObjects, head, retainedHeads,
         availableSequenceObjects, corruptVocabObjects,
         corruptSequenceObjects, reader, crashed, recoveryAttempted,
         recoveryKind, recoveredHead>>

LoseHeadSequenceArtifact ==
  /\ ~crashed
  /\ head.present
  /\ head.sequenceObject \in availableSequenceObjects
  /\ availableSequenceObjects' =
       availableSequenceObjects \ {head.sequenceObject}
  /\ UNCHANGED
       <<work, vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, corruptVocabObjects,
         corruptSequenceObjects, reader, crashed, recoveryAttempted,
         recoveryKind, recoveredHead>>

CorruptHeadVocabularyArtifact ==
  /\ ~crashed
  /\ head.present
  /\ head.vocabObject \notin corruptVocabObjects
  /\ corruptVocabObjects' =
       corruptVocabObjects \cup {head.vocabObject}
  /\ UNCHANGED
       <<work, vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptSequenceObjects, reader, crashed, recoveryAttempted,
         recoveryKind, recoveredHead>>

CorruptHeadSequenceArtifact ==
  /\ ~crashed
  /\ head.present
  /\ head.sequenceObject \notin corruptSequenceObjects
  /\ corruptSequenceObjects' =
       corruptSequenceObjects \cup {head.sequenceObject}
  /\ UNCHANGED
       <<work, vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, reader, crashed, recoveryAttempted,
         recoveryKind, recoveredHead>>

Crash ==
  /\ ~crashed
  /\ crashed' = TRUE
  /\ work' = EmptyWork
  /\ recoveryAttempted' = FALSE
  /\ recoveryKind' = "None"
  /\ recoveredHead' = NoHead
  /\ UNCHANGED
       <<vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, reader>>

Recover ==
  /\ crashed
  /\ ~recoveryAttempted
  /\ recoveryAttempted' = TRUE
  /\ LET vocabularyMissing ==
           head.present /\
           (head.vocabObject \notin availableVocabObjects \/
            head.vocabObject \in corruptVocabObjects)
         sequenceMissing ==
           head.present /\
           (head.sequenceObject \notin availableSequenceObjects \/
            head.sequenceObject \in corruptSequenceObjects)
         recoverable ==
           head.present /\ ~vocabularyMissing /\ ~sequenceMissing /\
           HeadCoherent(vocabObjects, sequenceObjects, head)
     IN /\ recoveryKind' =
              IF recoverable
              THEN "Pair"
              ELSE IF MissingVocabularyAsEmpty /\ vocabularyMissing
                   THEN "Empty"
                   ELSE "Error"
        /\ recoveredHead' = IF recoverable THEN head ELSE NoHead
  /\ UNCHANGED
       <<work, vocabObjects, sequenceObjects, head, retainedHeads,
         availableVocabObjects, availableSequenceObjects,
         corruptVocabObjects, corruptSequenceObjects, reader, crashed>>

Next ==
  \/ \E generation \in Generations,
       enableTermDictionary \in BOOLEAN :
       BeginGeneration(generation, enableTermDictionary)
  \/ \E id \in Ids : DurabilizeLiveId(id)
  \/ SealDurableVocabulary
  \/ PublishVocabularyEligibility
  \/ StageDependentSequence
  \/ PublishSequenceVisibility
  \/ DurabilizeDependentSequence
  \/ WriteVocabularyObject
  \/ SyncVocabularyObject
  \/ WriteSequenceObject
  \/ SyncSequenceObject
  \/ PublishCheckpointHead
  \/ CaptureReader
  \/ SaveReaderContinuation
  \/ ResumeCapturedReader
  \/ LoseHeadVocabularyArtifact
  \/ LoseHeadSequenceArtifact
  \/ CorruptHeadVocabularyArtifact
  \/ CorruptHeadSequenceArtifact
  \/ Crash
  \/ Recover

VWENC_147_PUBLISHED_FRONTIER_DOES_NOT_EXCEED_DURABLE_FRONTIER ==
  /\ work.publishedHighWater <= work.durableHighWater
  /\ work.publishedLiveIds \subseteq work.durableIds

VWENC_148_PUBLISHED_IDS_HAVE_EXACT_DURABLE_METADATA ==
  ~OverclaimVocabularyFrontier =>
    /\ \A id \in work.publishedLiveIds :
         /\ id \in work.liveIds
         /\ id \in work.durableIds
         /\ id < work.publishedHighWater
         /\ ExactIdMetadata(
              work.fiber,
              work.atomById, work.payloadById, work.spanById,
              work.packedBytes, id)
    /\ \A objectId \in VocabObjectIds :
         vocabObjects[objectId].phase # "Absent" =>
           ExactVocabObject(vocabObjects[objectId])

VWENC_149_DURABLE_SEQUENCE_REFERENCES_DURABLE_BOUND_VOCABULARY ==
  ~OverclaimVocabularyFrontier =>
    ((work.sequenceVisible \/ work.sequenceDurable) =>
      /\ work.sequenceStaged
      /\ work.sequenceFiber = work.fiber
      /\ work.sequenceRequiredHighWater <= work.publishedHighWater
      /\ work.sequenceRequiredHighWater <= work.durableHighWater
      /\ \A id \in SequenceIdSet(work.sequenceIds) :
           /\ id \in work.publishedLiveIds
           /\ id \in work.durableIds
           /\ id \in work.liveIds
           /\ id < work.sequenceRequiredHighWater
           /\ ExactIdMetadata(
                work.fiber,
                work.atomById, work.payloadById, work.spanById,
                work.packedBytes, id))

VWENC_150_SEQUENCE_OBJECT_FOLLOWS_DURABLE_VOCABULARY_OBJECT ==
  ~PublishSequenceBeforeVocabulary =>
    \A generation \in Generations :
      (sequenceObjects[SequenceObjectId(generation)].phase # "Absent") =>
        vocabObjects[VocabObjectId(generation)].phase = "Durable"

VWENC_151_SEQUENCE_DESCRIPTOR_BINDS_EXACT_VOCABULARY_FIBER ==
  ~PublishSequenceBeforeVocabulary =>
    \A generation \in Generations :
      LET sequence == sequenceObjects[SequenceObjectId(generation)]
          vocabulary == vocabObjects[VocabObjectId(generation)]
      IN (sequence.phase # "Absent") =>
           /\ ExactSequenceObject(sequence)
           /\ ExactVocabObject(vocabulary)
           /\ sequence.fiber = vocabulary.fiber
           /\ sequence.termFiber = TermFiber(sequence.generation)
           /\ sequence.termFiber.vocabularyFiber = sequence.fiber
           /\ sequence.requiredHighWater <= vocabulary.allocatorHighWater
           /\ IF sequence.termEnabled
              THEN /\ sequence.termId = 0
                   /\ sequence.termSequence = sequence.ids
              ELSE /\ sequence.termId = 0
                   /\ sequence.termSequence = <<>>

VWENC_152_HEAD_BINDS_ONE_COHERENT_DURABLE_PAIR ==
  (~PublishSequenceBeforeVocabulary) =>
    (head.present =>
      /\ HeadCoherent(vocabObjects, sequenceObjects, head)
      /\ head \in retainedHeads)

VWENC_153_RECOVERY_IS_COHERENT_OLD_NEW_OR_ERROR ==
  (~MissingVocabularyAsEmpty) =>
    (recoveryAttempted =>
      \/ recoveryKind = "Error" /\ recoveredHead = NoHead
      \/ recoveryKind = "Pair" /\
         recoveredHead = head /\
         HeadCoherent(vocabObjects, sequenceObjects, recoveredHead) /\
         recoveredHead \in retainedHeads)

VWENC_154_CAPTURED_CONTINUATION_RESUMES_IMMUTABLE_PAIR ==
  reader.resumed =>
    /\ reader.head \in retainedHeads
    /\ HeadCoherent(vocabObjects, sequenceObjects, reader.head)
    /\ reader.resumeObservation = reader.initialObservation
    /\ reader.resumeObservation =
         ObserveHead(vocabObjects, sequenceObjects, reader.head)

VWENC_155_UNAVAILABLE_OR_CORRUPT_HEAD_ARTIFACT_IS_EXPLICIT_ERROR ==
  (~MissingVocabularyAsEmpty) =>
    ((recoveryAttempted /\ head.present /\
      (head.vocabObject \notin availableVocabObjects \/
       head.vocabObject \in corruptVocabObjects \/
       head.sequenceObject \notin availableSequenceObjects \/
       head.sequenceObject \in corruptSequenceObjects))
     => recoveryKind = "Error" /\ recoveredHead = NoHead)

VWENC_156_PUBLISHED_HEAD_HAS_NO_DANGLING_ID_REFERENCE ==
  (~OverclaimVocabularyFrontier /\ ~PublishSequenceBeforeVocabulary) =>
    (head.present =>
       LET vocabulary == vocabObjects[head.vocabObject]
           sequence == sequenceObjects[head.sequenceObject]
       IN \A id \in SequenceIdSet(sequence.ids) :
            /\ id \in vocabulary.liveIds
            /\ id < sequence.requiredHighWater
            /\ ExactIdMetadata(
                 vocabulary.fiber,
                 vocabulary.atomById,
                 vocabulary.payloadById,
                 vocabulary.spanById,
                 vocabulary.packedBytes,
                 id))

VWENC_178_RECOVERY_NEVER_SYNTHESIZES_EMPTY_SUCCESS ==
  recoveryAttempted => recoveryKind # "Empty"

VWENC_179_EXACT_TERM_FIBER_SEPARATES_SAME_RAW_ID ==
  /\ \A generation \in Generations :
       LET sequence == sequenceObjects[SequenceObjectId(generation)]
       IN sequence.phase # "Absent" /\ sequence.termEnabled =>
            /\ sequence.termFiber = TermFiber(generation)
            /\ sequence.termFiber.vocabularyFiber = sequence.fiber
            /\ sequence.termId = 0
            /\ sequence.termSequence = sequence.ids
  /\ \A left \in Generations, right \in Generations :
       LET leftSequence == sequenceObjects[SequenceObjectId(left)]
           rightSequence == sequenceObjects[SequenceObjectId(right)]
       IN left # right /\
          leftSequence.phase # "Absent" /\ leftSequence.termEnabled /\
          rightSequence.phase # "Absent" /\ rightSequence.termEnabled =>
            /\ leftSequence.termId = rightSequence.termId
            /\ leftSequence.termFiber # rightSequence.termFiber

VWENC_193_TWO_GENERATION_TERM_FIBER_WITNESS_IS_CONCRETE ==
  LET firstVocabulary == vocabObjects[VocabObjectId(1)]
      secondVocabulary == vocabObjects[VocabObjectId(2)]
      firstSequence == sequenceObjects[SequenceObjectId(1)]
      secondSequence == sequenceObjects[SequenceObjectId(2)]
  IN /\ firstVocabulary.phase = "Durable"
     /\ secondVocabulary.phase = "Durable"
     /\ ExactVocabObject(firstVocabulary)
     /\ ExactVocabObject(secondVocabulary)
     /\ firstSequence.phase = "Durable"
     /\ secondSequence.phase = "Durable"
     /\ ExactSequenceObject(firstSequence)
     /\ ExactSequenceObject(secondSequence)
     /\ firstSequence.termEnabled
     /\ secondSequence.termEnabled
     /\ firstSequence.termId = 0
     /\ secondSequence.termId = 0
     /\ firstSequence.termId = secondSequence.termId
     /\ firstSequence.termSequence = GenerationSequence(1)
     /\ secondSequence.termSequence = GenerationSequence(2)
     /\ firstSequence.termFiber = TermFiber(1)
     /\ secondSequence.termFiber = TermFiber(2)
     /\ firstSequence.termFiber # secondSequence.termFiber
     /\ firstSequence.termFiber.vocabularyFiber = firstSequence.fiber
     /\ secondSequence.termFiber.vocabularyFiber = secondSequence.fiber
     /\ firstSequence.fiber = firstVocabulary.fiber
     /\ secondSequence.fiber = secondVocabulary.fiber
     /\ HeadCoherent(vocabObjects, sequenceObjects, HeadFor(1))
     /\ HeadCoherent(vocabObjects, sequenceObjects, HeadFor(2))
     /\ HeadFor(1) \in retainedHeads
     /\ HeadFor(2) \in retainedHeads
     /\ head = HeadFor(2)

Spec == Init /\ [][Next]_vars

=============================================================================
