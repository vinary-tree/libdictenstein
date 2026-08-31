------------------- MODULE PackedResidencyFreshCatalog -------------------
(***************************************************************************)
(* Exact single-CAS rollover of a finite packed-residency ordinal.          *)
(*                                                                         *)
(* Two conflicting candidates prepare complete, private fresh catalogs     *)
(* from the same ordinal-exhausted root. Only the candidate that wins the   *)
(* exact root-identity CAS becomes reachable. Every fresh cell is already   *)
(* tagged at ordinal zero, so publication requires no post-CAS word help.   *)
(* A delayed helper retains the old catalog address domain and cannot touch *)
(* the fresh arrays.                                                        *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Catalogs,
    RootIds,
    Words,
    OldCatalog,
    FreshCatalogA,
    FreshCatalogB,
    OldRoot,
    FreshRootA,
    FreshRootB,
    DistinguishedWord,
    PartialWord,
    MaxOrdinal,
    UnsafeReuseOrdinal,
    UnsafeWrongGeneration,
    UnsafePartialFresh,
    UnsafeNonExactRoot

Candidates == {"A", "B"}
NoWinner == "None"

ASSUME /\ MaxOrdinal >= 2
       /\ Cardinality(Words) >= 2
       /\ DistinguishedWord \in Words
       /\ PartialWord \in Words
       /\ DistinguishedWord # PartialWord
       /\ OldCatalog \in Catalogs
       /\ FreshCatalogA \in Catalogs
       /\ FreshCatalogB \in Catalogs
       /\ OldRoot \in RootIds
       /\ FreshRootA \in RootIds
       /\ FreshRootB \in RootIds
       /\ FreshCatalogA # FreshCatalogB
       /\ FreshRootA # FreshRootB
       /\ OldRoot # FreshRootA
       /\ OldRoot # FreshRootB

BoolBit(value) == IF value THEN 1 ELSE 0
Pack(ordinal, value) == 2 * ordinal + BoolBit(value)
CellOrdinal(cell) == cell \div 2
CellBit(cell) == (cell % 2) = 1

CandidateCatalog(candidate) ==
    IF UnsafeReuseOrdinal
    THEN OldCatalog
    ELSE IF candidate = "A" THEN FreshCatalogA ELSE FreshCatalogB

CandidateRoot(candidate) ==
    IF candidate = "A" THEN FreshRootA ELSE FreshRootB

CandidateTarget(candidate) ==
    [word \in Words |->
        IF candidate = "A"
        THEN word # DistinguishedWord
        ELSE word = DistinguishedWord]

PreparedCell(candidate, word) ==
    IF UnsafePartialFresh /\ candidate = "A" /\ word = PartialWord
    THEN Pack(0, FALSE)
    ELSE Pack(0, CandidateTarget(candidate)[word])

VARIABLES
    currentCatalog,
    currentRoot,
    rootOrdinal,
    rootPayload,
    cells,
    attempted,
    firstWinner,
    delayedRan

vars ==
    <<currentCatalog, currentRoot, rootOrdinal, rootPayload, cells,
      attempted, firstWinner, delayedRan>>

TypeOK ==
    /\ currentCatalog \in Catalogs
    /\ currentRoot \in RootIds
    /\ rootOrdinal \in 0..MaxOrdinal
    /\ rootPayload \in [Words -> BOOLEAN]
    /\ cells \in [Catalogs -> [Words -> 0..(2 * MaxOrdinal + 1)]]
    /\ attempted \subseteq Candidates
    /\ firstWinner \in Candidates \cup {NoWinner}
    /\ delayedRan \in BOOLEAN

Init ==
    /\ currentCatalog = OldCatalog
    /\ currentRoot = OldRoot
    /\ rootOrdinal = MaxOrdinal
    /\ rootPayload = [word \in Words |-> FALSE]
    /\ cells =
         [catalog \in Catalogs |->
           [word \in Words |->
             IF catalog = OldCatalog
             THEN Pack(MaxOrdinal, FALSE)
             ELSE Pack(0, FALSE)]]
    /\ attempted = {}
    /\ firstWinner = NoWinner
    /\ delayedRan = FALSE

AttemptPublish(candidate) ==
    /\ candidate \in Candidates \ attempted
    /\ LET mayPublish ==
              firstWinner = NoWinner \/ UnsafeNonExactRoot
           targetCatalog == CandidateCatalog(candidate)
           targetPayload == CandidateTarget(candidate)
       IN  /\ currentCatalog' =
                 IF mayPublish THEN targetCatalog ELSE currentCatalog
           /\ currentRoot' =
                 IF mayPublish THEN CandidateRoot(candidate) ELSE currentRoot
           /\ rootOrdinal' = IF mayPublish THEN 0 ELSE rootOrdinal
           /\ rootPayload' = IF mayPublish THEN targetPayload ELSE rootPayload
           /\ cells' =
                 IF mayPublish
                 THEN [cells EXCEPT
                        ![targetCatalog] =
                          [word \in Words |-> PreparedCell(candidate, word)]]
                 ELSE cells
           /\ firstWinner' =
                 IF firstWinner = NoWinner /\ mayPublish
                 THEN candidate
                 ELSE firstWinner
    /\ attempted' = attempted \cup {candidate}
    /\ UNCHANGED delayedRan

RunDelayedHelper ==
    /\ ~delayedRan
    /\ LET addressedCatalog ==
              IF UnsafeWrongGeneration THEN currentCatalog ELSE OldCatalog
           observed == cells[addressedCatalog][DistinguishedWord]
           updated ==
              IF observed = Pack(0, FALSE)
              THEN Pack(1, TRUE)
              ELSE observed
       IN cells' =
            [cells EXCEPT ![addressedCatalog][DistinguishedWord] = updated]
    /\ delayedRan' = TRUE
    /\ UNCHANGED
         <<currentCatalog, currentRoot, rootOrdinal, rootPayload,
           attempted, firstWinner>>

Next ==
    (\E candidate \in Candidates : AttemptPublish(candidate))
    \/ RunDelayedHelper

Spec == Init /\ [][Next]_vars

WinnerOwnsPublishedRoot ==
    IF firstWinner = NoWinner
    THEN /\ currentCatalog = OldCatalog
         /\ currentRoot = OldRoot
    ELSE /\ currentCatalog = CandidateCatalog(firstWinner)
         /\ currentRoot = CandidateRoot(firstWinner)
         /\ rootOrdinal = 0
         /\ rootPayload = CandidateTarget(firstWinner)

FreshAddressDomainIsDistinct ==
    firstWinner = NoWinner \/ currentCatalog # OldCatalog

PublishedCellsMatchLogicalRoot ==
    IF firstWinner = NoWinner
    THEN \A word \in Words :
           /\ CellOrdinal(cells[OldCatalog][word]) = MaxOrdinal
           /\ CellBit(cells[OldCatalog][word]) = rootPayload[word]
    ELSE \A word \in Words :
           /\ CellOrdinal(cells[currentCatalog][word]) = 0
           /\ CellBit(cells[currentCatalog][word]) = rootPayload[word]

=============================================================================
