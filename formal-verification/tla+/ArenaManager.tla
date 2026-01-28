--------------------------- MODULE ArenaManager ----------------------------
(****************************************************************************)
(* ArenaManager: State machine specification for arena allocation.          *)
(*                                                                          *)
(* This module models the ArenaManager state and operations, verifying:     *)
(* 1. The arenas list is never empty                                        *)
(* 2. active_arena is always a valid index                                  *)
(* 3. All operations preserve the safety invariant                          *)
(*                                                                          *)
(* Key properties verified:                                                 *)
(* - SafetyInvariant: arenas # {} /\ active_arena < Len(arenas)            *)
(* - BuggyTransition: clear_for_loading_BUGGY violates invariant           *)
(* - Recovery: ensure_valid establishes invariant from any state           *)
(****************************************************************************)

EXTENDS Integers, Sequences, FiniteSets, TLC

--------------------------------------------------------------------------------
(* CONSTANTS                                                                 *)
--------------------------------------------------------------------------------

CONSTANTS
    \* Maximum number of arenas for model checking
    MaxArenas,

    \* Default arena capacity
    DefaultArenaSize,

    \* Maximum number of state transitions to explore
    MaxSteps

ASSUME MaxArenas \in Nat \ {0}
ASSUME DefaultArenaSize \in Nat \ {0}
ASSUME MaxSteps \in Nat

--------------------------------------------------------------------------------
(* TYPE DEFINITIONS                                                          *)
--------------------------------------------------------------------------------

\* Arena record - simplified model
ArenaRecord == [
    id: Nat,
    node_count: Nat,
    capacity: Nat,
    block_id: Nat \cup {-1}  \* -1 = not persisted
]

\* New arena constructor
NewArena(id, cap) == [
    id |-> id,
    node_count |-> 0,
    capacity |-> cap,
    block_id |-> -1
]

--------------------------------------------------------------------------------
(* STATE VARIABLES                                                           *)
--------------------------------------------------------------------------------

VARIABLES
    \* List of arenas (sequence)
    arenas,

    \* Index of active arena for allocations
    active_arena,

    \* Arena size for new arena creation
    arena_size,

    \* Step counter for bounded model checking
    step

vars == <<arenas, active_arena, arena_size, step>>

--------------------------------------------------------------------------------
(* TYPE INVARIANT                                                            *)
--------------------------------------------------------------------------------

TypeInvariant ==
    /\ arenas \in Seq(ArenaRecord)
    /\ active_arena \in Nat
    /\ arena_size \in Nat \ {0}
    /\ step \in Nat

--------------------------------------------------------------------------------
(* SAFETY INVARIANT                                                          *)
(* The core invariant that must always hold                                  *)
--------------------------------------------------------------------------------

\* arenas is never empty AND active_arena is a valid index
SafetyInvariant ==
    /\ Len(arenas) > 0
    /\ active_arena < Len(arenas)

--------------------------------------------------------------------------------
(* INITIAL STATE                                                             *)
--------------------------------------------------------------------------------

Init ==
    /\ arenas = <<NewArena(0, DefaultArenaSize)>>
    /\ active_arena = 0
    /\ arena_size = DefaultArenaSize
    /\ step = 0

--------------------------------------------------------------------------------
(* STATE TRANSITIONS                                                         *)
--------------------------------------------------------------------------------

\* Clear and reset to initial state (safe)
Clear ==
    /\ step < MaxSteps
    /\ arenas' = <<NewArena(0, arena_size)>>
    /\ active_arena' = 0
    /\ UNCHANGED arena_size
    /\ step' = step + 1

\* BUGGY clear_for_loading (violates invariant)
\* This is the bug we discovered: clears arenas but sets active_arena = 0
ClearForLoadingBuggy ==
    /\ step < MaxSteps
    /\ arenas' = <<>>  \* EMPTY - THIS IS THE BUG
    /\ active_arena' = 0
    /\ UNCHANGED arena_size
    /\ step' = step + 1

\* FIXED clear_for_loading (maintains invariant)
\* Keeps a fallback arena to prevent panic if load_arena fails
ClearForLoadingFixed ==
    /\ step < MaxSteps
    /\ arenas' = <<NewArena(0, arena_size)>>  \* Keep fallback arena
    /\ active_arena' = 0
    /\ UNCHANGED arena_size
    /\ step' = step + 1

\* Load an arena from disk
\* Appends the arena to the list
LoadArena ==
    /\ step < MaxSteps
    /\ Len(arenas) < MaxArenas
    \* Non-deterministically choose arena parameters (simplified)
    /\ \E id \in 0..MaxArenas-1, nc \in 0..DefaultArenaSize :
        LET a == [id |-> id, node_count |-> nc, capacity |-> DefaultArenaSize, block_id |-> id + 1]
        IN arenas' = Append(arenas, a)
    /\ UNCHANGED <<active_arena, arena_size>>
    /\ step' = step + 1

\* Set active arena (with bounds check)
SetActiveArena ==
    /\ step < MaxSteps
    /\ Len(arenas) > 0  \* Precondition
    /\ \E idx \in 0..MaxArenas :
        active_arena' = IF idx < Len(arenas)
                        THEN idx
                        ELSE Len(arenas) - 1
    /\ UNCHANGED <<arenas, arena_size>>
    /\ step' = step + 1

\* Allocate in current arena (may create new arena)
Allocate ==
    /\ step < MaxSteps
    /\ Len(arenas) > 0  \* Precondition (invariant)
    /\ LET currentArena == arenas[active_arena + 1]  \* TLA+ is 1-indexed
       IN IF currentArena.node_count < currentArena.capacity
          THEN
              \* Allocate in current arena
              /\ arenas' = [arenas EXCEPT ![active_arena + 1].node_count = @ + 1]
              /\ UNCHANGED active_arena
          ELSE IF Len(arenas) < MaxArenas
          THEN
              \* Create new arena
              /\ arenas' = Append(arenas, NewArena(Len(arenas), arena_size))
              /\ active_arena' = Len(arenas)  \* Point to new arena (0-indexed)
          ELSE
              \* Cannot allocate - no change
              /\ UNCHANGED <<arenas, active_arena>>
    /\ UNCHANGED arena_size
    /\ step' = step + 1

\* EnsureValid - recovery function that establishes invariant from any state
EnsureValid ==
    /\ step < MaxSteps
    /\ IF Len(arenas) = 0
       THEN
           \* Reset completely - create new arena
           /\ arenas' = <<NewArena(0, arena_size)>>
           /\ active_arena' = 0
       ELSE IF active_arena >= Len(arenas)
       THEN
           \* Fix active_arena to valid index
           /\ active_arena' = Len(arenas) - 1
           /\ UNCHANGED arenas
       ELSE
           \* Already valid - no change
           /\ UNCHANGED <<arenas, active_arena>>
    /\ UNCHANGED arena_size
    /\ step' = step + 1

\* Read operation - demonstrates invariant is needed
\* Accesses arenas[active_arena] which REQUIRES SafetyInvariant
Read ==
    /\ step < MaxSteps
    /\ Len(arenas) > 0
    /\ active_arena < Len(arenas)
    \* Successful read - no state change
    /\ UNCHANGED vars

\* Next slot operation - the operation that panics in the bug
\* Accesses arenas[active_arena].node_count which REQUIRES SafetyInvariant
NextSlot ==
    /\ step < MaxSteps
    /\ Len(arenas) > 0
    /\ active_arena < Len(arenas)
    \* Returns (active_arena, arenas[active_arena].node_count)
    /\ UNCHANGED vars

--------------------------------------------------------------------------------
(* NEXT STATE RELATION                                                       *)
--------------------------------------------------------------------------------

\* Stuttering step - allows termination when step >= MaxSteps
Stutter ==
    /\ step >= MaxSteps
    /\ UNCHANGED vars

\* Safe operations only (for checking SafetyInvariant is preserved)
NextSafe ==
    \/ Clear
    \/ ClearForLoadingFixed
    \/ LoadArena
    \/ SetActiveArena
    \/ Allocate
    \/ EnsureValid
    \/ Read
    \/ NextSlot
    \/ Stutter

\* Include buggy operation (to demonstrate invariant violation)
NextWithBug ==
    \/ NextSafe
    \/ ClearForLoadingBuggy

--------------------------------------------------------------------------------
(* SPECIFICATION                                                             *)
--------------------------------------------------------------------------------

\* Spec with only safe operations - should preserve SafetyInvariant
SpecSafe == Init /\ [][NextSafe]_vars

\* Spec that includes buggy operation - TLC will find counterexample
SpecWithBug == Init /\ [][NextWithBug]_vars

--------------------------------------------------------------------------------
(* TEMPORAL PROPERTIES                                                       *)
--------------------------------------------------------------------------------

\* Safety: invariant always holds (for SpecSafe)
AlwaysSafe == []SafetyInvariant

\* After EnsureValid, invariant holds (recovery property)
\* This is implicitly true since EnsureValid establishes invariant
\* Note: Expressed as invariant, EnsureValid's postcondition ensures SafetyInvariant

\* Liveness: system can always make progress (not deadlocked)
CanProgress == []<><<NextSafe>>_vars

\* NextSlot operation is always possible when invariant holds
\* (Property expressed as: whenever invariant holds, NextSlot can execute)

--------------------------------------------------------------------------------
(* DERIVED THEOREMS                                                          *)
--------------------------------------------------------------------------------

\* Theorem: SafetyInvariant is an inductive invariant for SpecSafe
\* Proof: Init establishes it, NextSafe preserves it
\* TLC verifies this by model checking

\* Theorem: ClearForLoadingBuggy violates SafetyInvariant
\* Proof: After ClearForLoadingBuggy, Len(arenas) = 0, violating Len(arenas) > 0
\* TLC finds counterexample demonstrating this

\* Theorem: EnsureValid establishes SafetyInvariant from any state
\* Proof: Case analysis on Len(arenas) and active_arena
\* - If Len(arenas) = 0: creates arena, sets active_arena = 0, invariant holds
\* - If active_arena >= Len(arenas): sets active_arena = Len(arenas) - 1
\* - Otherwise: already valid

================================================================================
