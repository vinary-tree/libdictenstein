---------------- MODULE CapturedCheckpointEvictionRoute ----------------
(***************************************************************************)
(* A checkpoint snapshot owns its eviction-publication route.              *)
(*                                                                         *)
(* The mutable coordinator slot may be disabled or replaced while a frozen *)
(* snapshot is being serialized.  Publication must therefore branch on the *)
(* generation captured in the snapshot, never on a later slot observation.  *)
(* A captured generation that is no longer installed is discarded; it is   *)
(* never substituted with the current generation.                          *)
(***************************************************************************)

EXTENDS Naturals, TLC

CONSTANTS Generations, NoGeneration, UnsafeLiveRouteReprobe

ASSUME /\ Generations # {}
       /\ NoGeneration \notin Generations

GenerationValues == Generations \cup {NoGeneration}
Phases == {"Idle", "Captured", "Published"}
Outcomes == {"None", "Disabled", "Exact", "StaleDiscard", "WrongGeneration"}

VARIABLES
    slotGeneration,
    phase,
    capturedGeneration,
    selectedGeneration,
    publicationOutcome,
    badRoute

vars ==
    <<slotGeneration, phase, capturedGeneration, selectedGeneration,
      publicationOutcome, badRoute>>

TypeInvariant ==
    /\ slotGeneration \in GenerationValues
    /\ phase \in Phases
    /\ capturedGeneration \in GenerationValues
    /\ selectedGeneration \in GenerationValues
    /\ publicationOutcome \in Outcomes
    /\ badRoute \in BOOLEAN

PublicationUsesCapturedRoute == ~badRoute

ExactPublicationIsCurrentAndCaptured ==
    publicationOutcome # "Exact"
    \/ /\ selectedGeneration \in Generations
       /\ selectedGeneration = capturedGeneration
       /\ selectedGeneration = slotGeneration

StaleCapturedGenerationNeverPublishes ==
    phase # "Published"
    \/ capturedGeneration = NoGeneration
    \/ capturedGeneration = slotGeneration
    \/ publicationOutcome = "StaleDiscard"

CaptureWithoutEvictionStaysWithoutEviction ==
    phase # "Published"
    \/ capturedGeneration # NoGeneration
    \/ publicationOutcome = "Disabled"

Init ==
    /\ slotGeneration = NoGeneration
    /\ phase = "Idle"
    /\ capturedGeneration = NoGeneration
    /\ selectedGeneration = NoGeneration
    /\ publicationOutcome = "None"
    /\ badRoute = FALSE

Install(generation) ==
    /\ generation \in Generations
    /\ slotGeneration' = generation
    /\ publicationOutcome' =
         IF /\ phase = "Published"
            /\ publicationOutcome = "Exact"
            /\ selectedGeneration # generation
         THEN "StaleDiscard"
         ELSE publicationOutcome
    /\ UNCHANGED <<phase, capturedGeneration, selectedGeneration, badRoute>>

Disable ==
    /\ slotGeneration # NoGeneration
    /\ slotGeneration' = NoGeneration
    /\ publicationOutcome' =
         IF /\ phase = "Published"
            /\ publicationOutcome = "Exact"
         THEN "StaleDiscard"
         ELSE publicationOutcome
    /\ UNCHANGED <<phase, capturedGeneration, selectedGeneration, badRoute>>

Capture ==
    /\ phase = "Idle"
    /\ phase' = "Captured"
    /\ capturedGeneration' = slotGeneration
    /\ selectedGeneration' = NoGeneration
    /\ publicationOutcome' = "None"
    /\ UNCHANGED <<slotGeneration, badRoute>>

Publish ==
    /\ phase = "Captured"
    /\ LET selected ==
              IF UnsafeLiveRouteReprobe
              THEN slotGeneration
              ELSE capturedGeneration
       IN /\ phase' = "Published"
          /\ selectedGeneration' = selected
          /\ badRoute' = (badRoute \/ selected # capturedGeneration)
          /\ publicationOutcome' =
                IF selected # capturedGeneration
                THEN "WrongGeneration"
                ELSE IF selected = NoGeneration
                     THEN "Disabled"
                     ELSE IF selected = slotGeneration
                          THEN "Exact"
                          ELSE "StaleDiscard"
    /\ UNCHANGED <<slotGeneration, capturedGeneration>>

Reset ==
    /\ phase = "Published"
    /\ phase' = "Idle"
    /\ capturedGeneration' = NoGeneration
    /\ selectedGeneration' = NoGeneration
    /\ publicationOutcome' = "None"
    /\ UNCHANGED <<slotGeneration, badRoute>>

Next ==
    \/ \E generation \in Generations : Install(generation)
    \/ Disable
    \/ Capture
    \/ Publish
    \/ Reset

Spec == Init /\ [][Next]_vars

=============================================================================
