---------------------- MODULE PackedResidencyRollover ----------------------
(*****************************************************************************)
(* Finite 32/32-style residency cells and generation-local ordinal rollover. *)
(*                                                                           *)
(* The model uses one payload bit and a configurable finite ordinal bound so *)
(* TLC can exhaust the state space. Production refines Pack to               *)
(* `(ordinal << 32) | payload:u32`. A delayed helper retains the exact array *)
(* generation that it captured. Reusing an ordinal in that array, or letting *)
(* the helper address a replacement array, recreates ABA.                    *)
(*****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Generations, MaxOrdinal, UnsafeReuseOrdinal, UnsafeWrongGeneration

ASSUME /\ Cardinality(Generations) >= 2
       /\ MaxOrdinal >= 2

Phases == {"Initial", "Captured", "FirstApplied", "InverseApplied",
            "Rolled", "DelayedRan"}

BoolBit(value) == IF value THEN 1 ELSE 0
Pack(ordinal, value) == 2 * ordinal + BoolBit(value)
CellOrdinal(cell) == cell \div 2
CellBit(cell) == (cell % 2) = 1
PackedCells == [Generations -> 0..(2 * MaxOrdinal + 1)]

VARIABLES
    phase,
    currentGeneration,
    usedGenerations,
    rootOrdinal,
    rootBit,
    cells,
    delayedGeneration,
    delayedExpected,
    delayedTarget

vars ==
    <<phase, currentGeneration, usedGenerations, rootOrdinal, rootBit, cells,
      delayedGeneration, delayedExpected, delayedTarget>>

TypeOK ==
    /\ phase \in Phases
    /\ currentGeneration \in Generations
    /\ usedGenerations \subseteq Generations
    /\ currentGeneration \in usedGenerations
    /\ rootOrdinal \in 0..MaxOrdinal
    /\ rootBit \in BOOLEAN
    /\ cells \in PackedCells
    /\ delayedGeneration \in Generations
    /\ delayedExpected \in 0..(2 * MaxOrdinal + 1)
    /\ delayedTarget \in 0..(2 * MaxOrdinal + 1)

CurrentCellMatchesRoot ==
    /\ CellOrdinal(cells[currentGeneration]) = rootOrdinal
    /\ CellBit(cells[currentGeneration]) = rootBit

Init ==
    LET initialGeneration == CHOOSE generation \in Generations : TRUE
    IN  /\ phase = "Initial"
        /\ currentGeneration = initialGeneration
        /\ usedGenerations = {initialGeneration}
        /\ rootOrdinal = 0
        /\ rootBit = FALSE
        /\ cells = [generation \in Generations |-> Pack(0, FALSE)]
        /\ delayedGeneration = initialGeneration
        /\ delayedExpected = Pack(0, FALSE)
        /\ delayedTarget = Pack(1, TRUE)

CaptureDelayedHelper ==
    /\ phase = "Initial"
    /\ phase' = "Captured"
    /\ delayedGeneration' = currentGeneration
    /\ delayedExpected' = cells[currentGeneration]
    /\ delayedTarget' = Pack(1, TRUE)
    /\ UNCHANGED <<currentGeneration, usedGenerations, rootOrdinal, rootBit,
                    cells>>

ApplyFirstFault ==
    /\ phase = "Captured"
    /\ phase' = "FirstApplied"
    /\ rootOrdinal' = 1
    /\ rootBit' = TRUE
    /\ cells' = [cells EXCEPT ![currentGeneration] = Pack(1, TRUE)]
    /\ UNCHANGED <<currentGeneration, usedGenerations, delayedGeneration,
                    delayedExpected, delayedTarget>>

ApplyInverseEviction ==
    /\ phase = "FirstApplied"
    /\ phase' = "InverseApplied"
    /\ rootOrdinal' = IF UnsafeReuseOrdinal THEN 0 ELSE MaxOrdinal
    /\ rootBit' = FALSE
    /\ cells' =
         [cells EXCEPT
            ![currentGeneration] =
              Pack(IF UnsafeReuseOrdinal THEN 0 ELSE MaxOrdinal, FALSE)]
    /\ UNCHANGED <<currentGeneration, usedGenerations, delayedGeneration,
                    delayedExpected, delayedTarget>>

Rollover(generation) ==
    /\ phase = "InverseApplied"
    /\ generation \in Generations \ usedGenerations
    /\ phase' = "Rolled"
    /\ currentGeneration' = generation
    /\ usedGenerations' = usedGenerations \cup {generation}
    /\ rootOrdinal' = 0
    /\ rootBit' = FALSE
    /\ cells' = [cells EXCEPT ![generation] = Pack(0, FALSE)]
    /\ UNCHANGED <<delayedGeneration, delayedExpected, delayedTarget>>

RunDelayedHelper ==
    /\ phase \in {"InverseApplied", "Rolled"}
    /\ LET addressedGeneration ==
              IF UnsafeWrongGeneration /\ phase = "Rolled"
              THEN currentGeneration
              ELSE delayedGeneration
           observed == cells[addressedGeneration]
       IN  /\ cells' =
                IF observed = delayedExpected
                THEN [cells EXCEPT ![addressedGeneration] = delayedTarget]
                ELSE cells
    /\ phase' = "DelayedRan"
    /\ UNCHANGED <<currentGeneration, usedGenerations, rootOrdinal, rootBit,
                    delayedGeneration, delayedExpected, delayedTarget>>

Next ==
    \/ CaptureDelayedHelper
    \/ ApplyFirstFault
    \/ ApplyInverseEviction
    \/ \E generation \in Generations : Rollover(generation)
    \/ RunDelayedHelper

Spec == Init /\ [][Next]_vars

=============================================================================
