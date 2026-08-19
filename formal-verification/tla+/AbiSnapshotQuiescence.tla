------------------------- MODULE AbiSnapshotQuiescence -------------------------
(***************************************************************************)
(* Starvation-prevention half of SnapshotMemo::get_or_create.              *)
(*                                                                         *)
(* Snapshot capture first makes a bounded number of optimistic attempts.   *)
(* If continuously arriving writers prevent a zero-writer observation, the *)
(* fallback atomically closes writer admission, lets already admitted       *)
(* writers drain, captures one revision, and reopens admission.             *)
(*                                                                         *)
(* The model begins when the bounded optimistic phase has been exhausted.   *)
(* USE_ADMISSION_GATE is the design choice under test: TRUE models the high *)
(* bit in SnapshotMemo::writer_state; FALSE models the rejected reader that *)
(* merely waits for activeWriters = 0 while new writers remain admissible.  *)
(*                                                                         *)
(* Fairness boundary: an enabled fallback request, an admitted writer, and  *)
(* an enabled capture eventually take a step. This is scheduler fairness,   *)
(* not a wait-free claim. The safe design is starvation-free under that     *)
(* boundary because admission closure makes the drain condition stable.    *)
(***************************************************************************)
EXTENDS Naturals, TLC

CONSTANTS
  MaxWriters,
  USE_ADMISSION_GATE

ASSUME MaxWriters \in Nat \ {0}
ASSUME USE_ADMISSION_GATE \in BOOLEAN

VARIABLES
  activeWriters, \* writers admitted before or between snapshot observations
  phase,         \* "requesting", "waiting", or "captured"
  gate           \* TRUE exactly while the fallback has closed admission

vars == <<activeWriters, phase, gate>>

Init ==
  /\ activeWriters = 1
  /\ phase = "requesting"
  /\ gate = FALSE

EnterFallback ==
  /\ phase = "requesting"
  /\ phase' = "waiting"
  /\ gate' = USE_ADMISSION_GATE
  /\ UNCHANGED activeWriters

WriterEnter ==
  /\ phase # "captured"
  /\ ~gate
  /\ activeWriters < MaxWriters
  /\ activeWriters' = activeWriters + 1
  /\ UNCHANGED <<phase, gate>>

WriterLeave ==
  /\ activeWriters > 0
  /\ activeWriters' = activeWriters - 1
  /\ UNCHANGED <<phase, gate>>

Capture ==
  /\ phase = "waiting"
  /\ activeWriters = 0
  /\ phase' = "captured"
  /\ gate' = FALSE
  /\ UNCHANGED activeWriters

Next == EnterFallback \/ WriterEnter \/ WriterLeave \/ Capture

Spec ==
  /\ Init
  /\ [][Next]_vars
  /\ WF_vars(EnterFallback)
  /\ WF_vars(WriterLeave)
  /\ WF_vars(Capture)

TypeOK ==
  /\ activeWriters \in 0..MaxWriters
  /\ phase \in {"requesting", "waiting", "captured"}
  /\ gate \in BOOLEAN

GateOwnedByWaitingSnapshot == gate => phase = "waiting"

(* Once the gate is closed, no transition may increase the admitted count. *)
AdmissionsNeverIncreaseWhileGate ==
  [][gate => activeWriters' <= activeWriters]_vars

(* Already admitted writers eventually drain after admission is closed. *)
AdmittedWritersDrain == gate ~> activeWriters = 0

(* The fallback request actually reaches admission closure in the design. *)
GateEventuallyCloses == phase = "requesting" ~> gate

(* The production obligation: sustained writer churn cannot starve capture. *)
SnapshotEventuallyCompletes == <>(phase = "captured")

=============================================================================
