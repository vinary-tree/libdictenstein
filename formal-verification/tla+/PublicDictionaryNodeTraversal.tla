-------------------- MODULE PublicDictionaryNodeTraversal --------------------
(***************************************************************************)
(* Faulting DictionaryNode traversal model.                                *)
(*                                                                         *)
(* Scope: the lock-free `DictionaryNode` graph walk (root -> transition* / *)
(* edges) that liblevenshtein's transducer drives over the persistent char *)
(* trie. After a checkpoint+reopen the root is resident but its children    *)
(* are *swizzled* (on-disk). The FAULTING walk (post-fix `transition`/      *)
(* `edges`) faults swizzled children in, so it reaches EXACTLY the snapshot  *)
(* regardless of residency. The pre-fix NON-faulting walk drops swizzled    *)
(* children, so it is only sound (a subset), not complete — the regression. *)
(*                                                                         *)
(* `walkSnapshot`/`walkResult` freeze the snapshot and result at walk time  *)
(* (like a reader snapshot), so later mutations do not perturb a completed  *)
(* walk's recorded outcome.                                                 *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS MaxKey

VARIABLES present, swizzled, walkSnapshot, walkResult, walkMode

Vars == <<present, swizzled, walkSnapshot, walkResult, walkMode>>

Keys == 1..MaxKey
WalkModes == {"None", "Faulting", "NonFaulting"}

TypeInvariant ==
    /\ MaxKey >= 1
    /\ present \subseteq Keys
    /\ swizzled \subseteq Keys
    /\ walkSnapshot \subseteq Keys
    /\ walkResult \subseteq Keys
    /\ walkMode \in WalkModes

Init ==
    /\ present = {}
    /\ swizzled = {}
    /\ walkSnapshot = {}
    /\ walkResult = {}
    /\ walkMode = "None"

(* Insert a key in memory (resident). *)
Insert(k) ==
    /\ k \in Keys
    /\ present' = present \cup {k}
    /\ UNCHANGED <<swizzled, walkSnapshot, walkResult, walkMode>>

(* Checkpoint + reopen: every present key's path becomes on-disk (swizzled). *)
Reopen ==
    /\ swizzled' = present
    /\ UNCHANGED <<present, walkSnapshot, walkResult, walkMode>>

(* On-demand fault of one swizzled key (resident-ify it). Never drops a key. *)
Fault(k) ==
    /\ k \in swizzled
    /\ swizzled' = swizzled \ {k}
    /\ UNCHANGED <<present, walkSnapshot, walkResult, walkMode>>

(* The faulting DictionaryNode walk (post-fix): faults in every child it touches,
   so it reaches EVERY present key regardless of residency. *)
FaultingWalk ==
    /\ walkSnapshot' = present
    /\ walkResult' = present
    /\ swizzled' = {}             \* everything visited is faulted resident
    /\ walkMode' = "Faulting"
    /\ present' = present

(* The pre-fix NON-faulting walk: drops swizzled children, so it reaches only the
   currently-resident keys (sound but incomplete). *)
NonFaultingWalk ==
    /\ walkSnapshot' = present
    /\ walkResult' = present \ swizzled
    /\ walkMode' = "NonFaulting"
    /\ UNCHANGED <<present, swizzled>>

Next ==
    \/ \E k \in Keys : Insert(k)
    \/ Reopen
    \/ \E k \in Keys : Fault(k)
    \/ FaultingWalk
    \/ NonFaultingWalk

Spec == Init /\ [][Next]_Vars

(* ---- Safety invariants ---- *)

SwizzledSubsetPresent == swizzled \subseteq present

WalkSnapshotSubsetPresent == walkSnapshot \subseteq present

(* No walk fabricates a key outside its snapshot. *)
WalkNeverFabricates == walkResult \subseteq walkSnapshot

(* HEADLINE: a completed faulting walk reaches EXACTLY the snapshot, regardless of
   how many keys were swizzled at walk time — the regression the HEAD fix repairs. *)
WalkReachesAllKeys == walkMode = "Faulting" => walkResult = walkSnapshot

(* The pre-fix non-faulting walk is sound but (when keys were swizzled) NOT
   complete: it returns only the resident subset of the snapshot. *)
NonFaultingWalkSound == walkMode = "NonFaulting" => walkResult \subseteq walkSnapshot

(* ---- Temporal property ---- *)

(* Faulting/reopen never drop a key: `present` is non-decreasing. *)
PresentMonotone == [][present \subseteq present']_present

=============================================================================
