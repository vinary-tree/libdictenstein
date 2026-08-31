----------------------- MODULE ResidentBudgetEviction -----------------------
(***************************************************************************)
(* Bounded refinement model for resident-budget selection and execution.    *)
(*                                                                         *)
(* The representative topology is the laminar chain                         *)
(*                                                                         *)
(*     root -> middle -> leaf                                               *)
(*                                                                         *)
(* with cold-priority order leaf, middle, root (the production tie-break for *)
(* equal subtree coldness: deeper first). Each node has one byte unit; using  *)
(* unit weights preserves the overlap/convergence counterexample while       *)
(* keeping the state space focused. `Covered` is the exact union of selected  *)
(* subtrees, hence Cardinality(Covered(...)) is the exact closure weight.     *)
(*                                                                         *)
(* The safe design selects a minimal priority prefix by exact closure and    *)
(* executes it ancestor-first: the shallowest still-exact selected anchor    *)
(* wins and covers its selected descendants. The historical control uses     *)
(* local candidate weights and descendant-first execution. Selecting leaf    *)
(* then middle plans two units, but the leaf path-copy makes middle stale, so *)
(* only one unit is reclaimed.                                               *)
(*                                                                         *)
(* Snapshot capture is also two-phase. BeginCapture records a generation and *)
(* dimensions before allocation outside the registry lock; FinishCapture     *)
(* must revalidate that generation before accepting copied residency bits.   *)
(* The second negative control omits this revalidation and accepts a mixed-   *)
(* generation snapshot.                                                      *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    USE_EXACT_CLOSURE,
    USE_ANCESTOR_EXECUTION,
    REVALIDATE_SNAPSHOT,
    TargetBytes,
    CandidateCap

TreeNodes == {"root", "middle", "leaf"}
Priority == <<"leaf", "middle", "root">>
Generations == 1..2
Phases == {"Idle", "Planned", "Captured", "Selected", "Committed", "Rejected"}

Subtree(n) ==
    CASE n = "root"   -> TreeNodes
      [] n = "middle" -> {"middle", "leaf"}
      [] n = "leaf"   -> {"leaf"}

Covered(anchors) ==
    {n \in TreeNodes : \E anchor \in anchors : n \in Subtree(anchor)}

BoundedCap ==
    IF CandidateCap < Len(Priority)
    THEN CandidateCap
    ELSE Len(Priority)

Prefix(k) == {Priority[i] : i \in 1..k}
CandidatePrefix(k, eligibleResident) == Prefix(k) \cap eligibleResident

PlannedFor(anchors, residentImage) ==
    IF USE_EXACT_CLOSURE
    THEN Cardinality(Covered(anchors) \cap residentImage)
    ELSE Cardinality(anchors \cap residentImage)

StoppingIndexes(eligibleResident) ==
    {k \in 0..BoundedCap :
        PlannedFor(CandidatePrefix(k, eligibleResident), eligibleResident)
            >= TargetBytes}

Minimum(S) == CHOOSE m \in S : \A n \in S : m <= n

ChosenCount(eligibleResident) ==
    IF StoppingIndexes(eligibleResident) # {}
    THEN Minimum(StoppingIndexes(eligibleResident))
    ELSE BoundedCap

SelectionFor(eligibleResident) ==
    CandidatePrefix(ChosenCount(eligibleResident), eligibleResident)

PriorityRank(n) == CHOOSE i \in 1..Len(Priority) : Priority[i] = n
FirstPriority(anchors) ==
    CHOOSE n \in anchors :
        \A other \in anchors : PriorityRank(n) <= PriorityRank(other)

DescendantFirstReclaim(anchors, residentImage) ==
    IF anchors = {}
    THEN {}
    ELSE Subtree(FirstPriority(anchors)) \cap residentImage

VARIABLES
    phase,
    generation,
    registryEligible,
    resident,
    planGeneration,
    snapshotGeneration,
    snapshotEligible,
    snapshotResident,
    selected,
    plannedBytes,
    reclaimedBytes,
    acceptedMismatchedSnapshot,
    staleCommit

Vars ==
    <<phase, generation, registryEligible, resident, planGeneration,
      snapshotGeneration, snapshotEligible, snapshotResident, selected,
      plannedBytes, reclaimedBytes, acceptedMismatchedSnapshot, staleCommit>>

TypeOK ==
    /\ phase \in Phases
    /\ generation \in Generations
    /\ registryEligible \subseteq TreeNodes
    /\ resident \subseteq TreeNodes
    /\ planGeneration \in 0..2
    /\ snapshotGeneration \in 0..2
    /\ snapshotEligible \subseteq TreeNodes
    /\ snapshotResident \subseteq TreeNodes
    /\ selected \subseteq TreeNodes
    /\ plannedBytes \in 0..Cardinality(TreeNodes)
    /\ reclaimedBytes \in 0..Cardinality(TreeNodes)
    /\ acceptedMismatchedSnapshot \in BOOLEAN
    /\ staleCommit \in BOOLEAN

Init ==
    /\ phase = "Idle"
    /\ generation = 1
    /\ registryEligible = TreeNodes
    /\ resident = TreeNodes
    /\ planGeneration = 0
    /\ snapshotGeneration = 0
    /\ snapshotEligible = {}
    /\ snapshotResident = {}
    /\ selected = {}
    /\ plannedBytes = 0
    /\ reclaimedBytes = 0
    /\ acceptedMismatchedSnapshot = FALSE
    /\ staleCommit = FALSE

\* First short read-lock section: capture only generation and buffer dimensions.
BeginCapture ==
    /\ phase = "Idle"
    /\ phase' = "Planned"
    /\ planGeneration' = generation
    /\ UNCHANGED <<generation, registryEligible, resident,
                    snapshotGeneration, snapshotEligible, snapshotResident,
                    selected, plannedBytes, reclaimedBytes,
                    acceptedMismatchedSnapshot, staleCommit>>

\* A replacement registry may be installed while allocation occurs outside
\* the lock. Its exact topology/eligibility belongs to generation 2.
AdvanceGeneration ==
    /\ phase = "Planned"
    /\ generation = 1
    /\ generation' = 2
    /\ registryEligible' = {"root"}
    /\ UNCHANGED <<phase, resident, planGeneration, snapshotGeneration,
                    snapshotEligible, snapshotResident, selected,
                    plannedBytes, reclaimedBytes,
                    acceptedMismatchedSnapshot, staleCommit>>

\* Second short read-lock section: safe capture accepts copied bits only after
\* exact generation revalidation. The unsafe control deliberately omits it.
FinishCapture ==
    /\ phase = "Planned"
    /\ (~REVALIDATE_SNAPSHOT \/ planGeneration = generation)
    /\ phase' = "Captured"
    /\ snapshotGeneration' = planGeneration
    /\ snapshotEligible' = registryEligible
    /\ snapshotResident' = resident
    /\ acceptedMismatchedSnapshot' =
         (acceptedMismatchedSnapshot \/ planGeneration # generation)
    /\ UNCHANGED <<generation, registryEligible, resident, planGeneration,
                    selected, plannedBytes, reclaimedBytes, staleCommit>>

RejectChangedCapture ==
    /\ phase = "Planned"
    /\ REVALIDATE_SNAPSHOT
    /\ planGeneration # generation
    /\ phase' = "Rejected"
    /\ UNCHANGED <<generation, registryEligible, resident, planGeneration,
                    snapshotGeneration, snapshotEligible, snapshotResident,
                    selected, plannedBytes, reclaimedBytes,
                    acceptedMismatchedSnapshot, staleCommit>>

SelectPriorityPrefix ==
    LET eligibleResident == snapshotEligible \cap snapshotResident
        choice == SelectionFor(eligibleResident)
    IN  /\ phase = "Captured"
        /\ phase' = "Selected"
        /\ selected' = choice
        /\ plannedBytes' = PlannedFor(choice, eligibleResident)
        /\ UNCHANGED <<generation, registryEligible, resident,
                        planGeneration, snapshotGeneration,
                        snapshotEligible, snapshotResident, reclaimedBytes,
                        acceptedMismatchedSnapshot, staleCommit>>

CommitBatch ==
    LET admitted ==
            IF REVALIDATE_SNAPSHOT
            THEN selected \cap registryEligible
            ELSE selected
        reclaimed ==
            IF USE_ANCESTOR_EXECUTION
            THEN Covered(admitted) \cap resident
            ELSE DescendantFirstReclaim(admitted, resident)
    IN  /\ phase = "Selected"
        /\ (~REVALIDATE_SNAPSHOT \/ snapshotGeneration = generation)
        /\ phase' = "Committed"
        /\ resident' = resident \ reclaimed
        /\ reclaimedBytes' = Cardinality(reclaimed)
        /\ staleCommit' =
             (staleCommit \/ snapshotGeneration # generation
                          \/ ~(selected \subseteq registryEligible))
        /\ UNCHANGED <<generation, registryEligible, planGeneration,
                        snapshotGeneration, snapshotEligible,
                        snapshotResident, selected, plannedBytes,
                        acceptedMismatchedSnapshot>>

Next ==
    \/ BeginCapture
    \/ AdvanceGeneration
    \/ FinishCapture
    \/ RejectChangedCapture
    \/ SelectPriorityPrefix
    \/ CommitBatch

Spec == Init /\ [][Next]_Vars

(* ------------------------------ Invariants ----------------------------- *)

AcceptedSnapshotsWereRevalidated == ~acceptedMismatchedSnapshot

NoStaleGenerationCommit == ~staleCommit

SelectionRespectsPriorityCap ==
    Cardinality(selected) <= BoundedCap

PlannedBytesAreExactClosure ==
    (phase \in {"Selected", "Committed"} /\ USE_EXACT_CLOSURE) =>
        plannedBytes =
            Cardinality(Covered(selected) \cap
                        snapshotEligible \cap snapshotResident)

PlannedReclamationIsReal ==
    phase = "Committed" => reclaimedBytes >= plannedBytes

TargetMetWhenPlanned ==
    (phase = "Committed" /\ plannedBytes >= TargetBytes) =>
        reclaimedBytes >= TargetBytes
=============================================================================
