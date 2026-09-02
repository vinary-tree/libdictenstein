---------------- MODULE DelayedFaultCoordinatorAcquisition ----------------
(***************************************************************************)
(* Delayed acquisition of exact eviction/fault publication authority.      *)
(*                                                                         *)
(* A reader first captures an immutable root revision and classifies the    *)
(* encountered slot. Resident-present, resident-absent, and null/absent     *)
(* slots complete without consulting the coordinator. Only a non-null       *)
(* OnDisk slot acquires the owner and decodes. Publication still requires   *)
(* the captured root, generation, owner, and slot to remain exact.          *)
(*                                                                         *)
(* USE_DELAYED_ACQUISITION = FALSE is the performance-contract negative     *)
(* control: it eagerly acquires the coordinator for resident observations.  *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS USE_DELAYED_ACQUISITION, MaxRoot, MaxGeneration

Slots == {"ResidentPresent", "ResidentAbsent", "OnDisk"}
Authorities == {"Active", "Retired"}
CapturedAuthorities == Authorities \cup {"Unknown"}
ReadPhases == {"Idle", "Probed", "Acquired", "Decoded", "Done"}
ReadResults == {"None", "Present", "Absent", "Faulted", "Lost"}

VARIABLES
    rootRevision,
    generation,
    slot,
    authority,
    readPhase,
    capturedRoot,
    capturedGeneration,
    capturedSlot,
    capturedAuthority,
    coordinatorAcquisitions,
    readResult,
    readerPublished,
    publicationWasExact

Vars ==
    <<rootRevision, generation, slot, authority, readPhase, capturedRoot,
      capturedGeneration, capturedSlot, capturedAuthority,
      coordinatorAcquisitions, readResult, readerPublished,
      publicationWasExact>>

TypeInvariant ==
    /\ rootRevision \in 1..MaxRoot
    /\ generation \in 1..MaxGeneration
    /\ slot \in Slots
    /\ authority \in Authorities
    /\ readPhase \in ReadPhases
    /\ capturedRoot \in 0..MaxRoot
    /\ capturedGeneration \in 0..MaxGeneration
    /\ capturedSlot \in Slots \cup {"None"}
    /\ capturedAuthority \in CapturedAuthorities
    /\ coordinatorAcquisitions \in 0..1
    /\ readResult \in ReadResults
    /\ readerPublished \in BOOLEAN
    /\ publicationWasExact \in BOOLEAN

Init ==
    /\ rootRevision = 1
    /\ generation = 1
    /\ slot \in Slots
    /\ authority = "Active"
    /\ readPhase = "Idle"
    /\ capturedRoot = 0
    /\ capturedGeneration = 0
    /\ capturedSlot = "None"
    /\ capturedAuthority = "Unknown"
    /\ coordinatorAcquisitions = 0
    /\ readResult = "None"
    /\ readerPublished = FALSE
    /\ publicationWasExact = FALSE

ResidentResult(s) == IF s = "ResidentPresent" THEN "Present" ELSE "Absent"

StartRead ==
    /\ readPhase = "Idle"
    /\ capturedRoot' = rootRevision
    /\ capturedGeneration' = generation
    /\ capturedSlot' = slot
    /\ readerPublished' = FALSE
    /\ publicationWasExact' = FALSE
    /\ IF USE_DELAYED_ACQUISITION
          THEN /\ coordinatorAcquisitions' = 0
               /\ capturedAuthority' = "Unknown"
               /\ IF slot = "OnDisk"
                     THEN /\ readPhase' = "Probed"
                          /\ readResult' = "None"
                     ELSE /\ readPhase' = "Done"
                          /\ readResult' = ResidentResult(slot)
          ELSE /\ coordinatorAcquisitions' = 1
               /\ capturedAuthority' = authority
               /\ IF slot = "OnDisk"
                     THEN /\ readPhase' = "Acquired"
                          /\ readResult' = "None"
                     ELSE /\ readPhase' = "Done"
                          /\ readResult' = ResidentResult(slot)
    /\ UNCHANGED <<rootRevision, generation, slot, authority>>

AcquireCoordinator ==
    /\ readPhase = "Probed"
    /\ capturedSlot = "OnDisk"
    /\ coordinatorAcquisitions = 0
    /\ readPhase' = "Acquired"
    /\ coordinatorAcquisitions' = 1
    /\ capturedAuthority' = authority
    /\ UNCHANGED <<rootRevision, generation, slot, authority, capturedRoot,
                    capturedGeneration, capturedSlot, readResult,
                    readerPublished, publicationWasExact>>

DecodeCandidate ==
    /\ readPhase = "Acquired"
    /\ capturedSlot = "OnDisk"
    /\ readPhase' = "Decoded"
    /\ UNCHANGED <<rootRevision, generation, slot, authority, capturedRoot,
                    capturedGeneration, capturedSlot, capturedAuthority,
                    coordinatorAcquisitions, readResult, readerPublished,
                    publicationWasExact>>

ExactCapturedAuthority ==
    /\ capturedSlot = "OnDisk"
    /\ capturedAuthority = "Active"
    /\ authority = "Active"
    /\ capturedRoot = rootRevision
    /\ capturedGeneration = generation
    /\ slot = "OnDisk"

PublishFault ==
    /\ readPhase = "Decoded"
    /\ ExactCapturedAuthority
    /\ rootRevision < MaxRoot
    /\ rootRevision' = rootRevision + 1
    /\ slot' = "ResidentPresent"
    /\ readPhase' = "Done"
    /\ readResult' = "Faulted"
    /\ readerPublished' = TRUE
    /\ publicationWasExact' = TRUE
    /\ UNCHANGED <<generation, authority, capturedRoot, capturedGeneration,
                    capturedSlot, capturedAuthority, coordinatorAcquisitions>>

LoseFault ==
    /\ readPhase = "Decoded"
    /\ ~ExactCapturedAuthority
    /\ readPhase' = "Done"
    /\ readResult' = "Lost"
    /\ readerPublished' = FALSE
    /\ publicationWasExact' = FALSE
    /\ UNCHANGED <<rootRevision, generation, slot, authority, capturedRoot,
                    capturedGeneration, capturedSlot, capturedAuthority,
                    coordinatorAcquisitions>>

RetireOwner ==
    /\ authority = "Active"
    /\ rootRevision < MaxRoot
    /\ rootRevision' = rootRevision + 1
    /\ authority' = "Retired"
    /\ UNCHANGED <<generation, slot, readPhase, capturedRoot,
                    capturedGeneration, capturedSlot, capturedAuthority,
                    coordinatorAcquisitions, readResult, readerPublished,
                    publicationWasExact>>

ReenableOwner ==
    /\ authority = "Retired"
    /\ rootRevision < MaxRoot
    /\ generation < MaxGeneration
    /\ rootRevision' = rootRevision + 1
    /\ generation' = generation + 1
    /\ authority' = "Active"
    /\ UNCHANGED <<slot, readPhase, capturedRoot, capturedGeneration,
                    capturedSlot, capturedAuthority, coordinatorAcquisitions,
                    readResult, readerPublished, publicationWasExact>>

PublishCompetingRoot(nextSlot) ==
    /\ nextSlot \in Slots
    /\ rootRevision < MaxRoot
    /\ rootRevision' = rootRevision + 1
    /\ slot' = nextSlot
    /\ UNCHANGED <<generation, authority, readPhase, capturedRoot,
                    capturedGeneration, capturedSlot, capturedAuthority,
                    coordinatorAcquisitions, readResult, readerPublished,
                    publicationWasExact>>

Next ==
    \/ StartRead
    \/ AcquireCoordinator
    \/ DecodeCandidate
    \/ PublishFault
    \/ LoseFault
    \/ RetireOwner
    \/ ReenableOwner
    \/ \E nextSlot \in Slots : PublishCompetingRoot(nextSlot)

Spec == Init /\ [][Next]_Vars

ResidentCompletionNeverAcquires ==
    readResult \in {"Present", "Absent"} => coordinatorAcquisitions = 0

CoordinatorOnlyAfterOnDiskProbe ==
    coordinatorAcquisitions = 1 => capturedSlot = "OnDisk"

PublishedFaultWasExact == readerPublished => publicationWasExact

LostFaultPublishesNothing == readResult = "Lost" => ~readerPublished

ResidentObservationIsExact ==
    /\ (readResult = "Present" => capturedSlot = "ResidentPresent")
    /\ (readResult = "Absent" => capturedSlot = "ResidentAbsent")

=============================================================================
