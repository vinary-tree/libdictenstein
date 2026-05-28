-------------------------- MODULE EvictionWalkEBR --------------------------
(***************************************************************************)
(* Eviction-vs-walk epoch-based reclamation (EBR) safety model.            *)
(*                                                                         *)
(* The lock-free `DictionaryNode` walk hands out raw pointers (node ids)     *)
(* that escape any lock. Eviction must reclaim nodes non-blockingly without  *)
(* freeing one a live walk still holds. The protocol:                       *)
(*                                                                         *)
(*   * a reader pins the epoch (EnterRead) and loads/faults pointers to      *)
(*     currently-LINKED nodes (LoadPtr); after an unlink it re-faults a      *)
(*     fresh linked node, never the retired one;                            *)
(*   * eviction UNLINKS a node (so new readers cannot reach it) and RETIRES  *)
(*     it — it does NOT free inline;                                        *)
(*   * Reclaim frees retired nodes only when GATED on quiescence (no active  *)
(*     reader). With the gate, no active reader can hold a freed node.       *)
(*                                                                         *)
(* `Gated` is a CONSTANT: TRUE models the implemented drain-to-zero gate     *)
(* (`evict_char_nodes` frees only after `active_reader_count() = 0` or a      *)
(* successful `wait_for_quiescence`). Setting it FALSE (inline free) makes    *)
(* TLC find a NoUseAfterFree violation — i.e. the gate is necessary.         *)
(*                                                                         *)
(* Liveness note (NOT asserted): under sustained overlapping readers `active`*)
(* may never reach {} so reclamation is deferred (bounded by the quiescence  *)
(* timeout in the implementation). This is a documented perf/liveness        *)
(* trade-off, not a safety property.                                        *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Readers, Nodes, Gated

VARIABLES reachable, retired, freed, active, latched

Vars == <<reachable, retired, freed, active, latched>>

TypeInvariant ==
    /\ reachable \subseteq Nodes
    /\ retired \subseteq Nodes
    /\ freed \subseteq Nodes
    /\ active \subseteq Readers
    /\ latched \in [Readers -> SUBSET Nodes]
    /\ Gated \in BOOLEAN

Init ==
    /\ reachable = Nodes
    /\ retired = {}
    /\ freed = {}
    /\ active = {}
    /\ latched = [r \in Readers |-> {}]

(* A reader pins the epoch, starting with no latched pointers. *)
EnterRead(r) ==
    /\ r \notin active
    /\ active' = active \cup {r}
    /\ latched' = [latched EXCEPT ![r] = {}]
    /\ UNCHANGED <<reachable, retired, freed>>

(* An active reader loads / FAULTS a pointer to a currently-LINKED node. *)
LoadPtr(r, n) ==
    /\ r \in active
    /\ n \in reachable
    /\ latched' = [latched EXCEPT ![r] = @ \cup {n}]
    /\ UNCHANGED <<reachable, retired, freed, active>>

(* Eviction unlinks a node and retires it (does NOT free). *)
Unlink(n) ==
    /\ n \in reachable
    /\ reachable' = reachable \ {n}
    /\ retired' = retired \cup {n}
    /\ UNCHANGED <<freed, active, latched>>

(* Reclaim frees all retired nodes, GATED on quiescence (no active reader). *)
Reclaim ==
    /\ retired # {}
    /\ (Gated => active = {})
    /\ freed' = freed \cup retired
    /\ retired' = {}
    /\ UNCHANGED <<reachable, active, latched>>

(* A reader exits its epoch, dropping its latched pointers. *)
ExitRead(r) ==
    /\ r \in active
    /\ active' = active \ {r}
    /\ latched' = [latched EXCEPT ![r] = {}]
    /\ UNCHANGED <<reachable, retired, freed>>

Next ==
    \/ \E r \in Readers : EnterRead(r)
    \/ \E r \in Readers, n \in Nodes : LoadPtr(r, n)
    \/ \E n \in Nodes : Unlink(n)
    \/ Reclaim
    \/ \E r \in Readers : ExitRead(r)

Spec == Init /\ [][Next]_Vars

(* ---- Safety invariants ---- *)

(* HEADLINE: no active reader holds a pointer to a freed node. *)
NoUseAfterFree == \A r \in active : latched[r] \cap freed = {}

(* A linked node is never freed. *)
ReachableNotFreed == reachable \cap freed = {}

(* A node is linked OR retired, never both. *)
ReachableRetiredDisjoint == reachable \cap retired = {}

=============================================================================
