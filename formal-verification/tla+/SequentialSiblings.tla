------------------------ MODULE SequentialSiblings -------------------------
(****************************************************************************)
(* SequentialSiblings: State machine for sequential sibling encoding/decoding*)
(*                                                                          *)
(* This module models the sequential sibling optimization in ARTrie         *)
(* serialization, where consecutive child slots are encoded efficiently by  *)
(* storing only the first slot ID plus a count.                             *)
(*                                                                          *)
(* Key invariant verified:                                                  *)
(* - DecodedSlotsValid: All decoded slot IDs are within arena bounds        *)
(*                                                                          *)
(* The bug: decode_sequential_siblings() blindly adds indices to first_slot *)
(* without checking for u32 overflow or arena bounds, producing invalid     *)
(* slot IDs like 4849 in an arena with only 4726 nodes.                     *)
(*                                                                          *)
(* Fix strategy:                                                            *)
(* 1. check_sequential_char_children() validates first_slot + count - 1     *)
(*    won't overflow before choosing sequential encoding                    *)
(* 2. decode_sequential_siblings() uses checked_add as defense-in-depth     *)
(****************************************************************************)

EXTENDS Integers, Sequences, FiniteSets, TLC

--------------------------------------------------------------------------------
(* CONSTANTS                                                                 *)
--------------------------------------------------------------------------------

CONSTANTS
    \* Maximum slot ID value (models u32::MAX)
    MaxSlotId,

    \* Maximum number of children per node
    MaxChildCount,

    \* Maximum number of nodes per arena (simplified model)
    MaxArenaNodes,

    \* Maximum state transitions for bounded model checking
    MaxSteps

\* Validity assumptions
ASSUME MaxSlotId \in Nat /\ MaxSlotId > 0
ASSUME MaxChildCount \in Nat /\ MaxChildCount > 0
ASSUME MaxArenaNodes \in Nat /\ MaxArenaNodes > 0
ASSUME MaxSteps \in Nat

--------------------------------------------------------------------------------
(* STATE VARIABLES                                                           *)
--------------------------------------------------------------------------------

VARIABLES
    \* Current arena node count (simulates arena capacity check)
    arena_node_count,

    \* First child slot ID in sequential encoding
    first_child_slot,

    \* Number of sequential children
    child_count,

    \* Whether data has been encoded (TRUE = sequential encoding was used)
    encoded,

    \* Result of decoding: sequence of slot IDs
    decoded_slots,

    \* Whether an error occurred during decode
    decode_error,

    \* Step counter for bounded model checking
    step

vars == <<arena_node_count, first_child_slot, child_count, encoded, decoded_slots, decode_error, step>>

--------------------------------------------------------------------------------
(* TYPE INVARIANT                                                            *)
--------------------------------------------------------------------------------

TypeInvariant ==
    /\ arena_node_count \in 0..MaxArenaNodes
    /\ first_child_slot \in 0..MaxSlotId
    /\ child_count \in 0..MaxChildCount
    /\ encoded \in BOOLEAN
    /\ decoded_slots \in Seq(0..MaxSlotId)
    /\ decode_error \in BOOLEAN
    /\ step \in Nat

--------------------------------------------------------------------------------
(* SAFETY INVARIANT                                                          *)
(* The core property: all decoded slots must be valid arena indices          *)
--------------------------------------------------------------------------------

\* All decoded slot IDs must be less than arena_node_count
DecodedSlotsValid ==
    \A i \in 1..Len(decoded_slots) :
        decoded_slots[i] < arena_node_count

\* Combined safety: either no error, or if no error then slots are valid
SafetyInvariant ==
    decode_error = TRUE \/ DecodedSlotsValid

--------------------------------------------------------------------------------
(* HELPER OPERATORS                                                          *)
--------------------------------------------------------------------------------

\* Checked addition: returns -1 if overflow would occur
CheckedAdd(a, b) ==
    IF a + b > MaxSlotId THEN -1 ELSE a + b

\* Compute last slot ID for sequential siblings
LastSlot(first, count) ==
    IF count = 0 THEN first
    ELSE first + count - 1

--------------------------------------------------------------------------------
(* INITIAL STATE                                                             *)
--------------------------------------------------------------------------------

Init ==
    /\ arena_node_count \in 1..MaxArenaNodes  \* Non-empty arena
    /\ first_child_slot \in 0..MaxSlotId
    /\ child_count \in 1..MaxChildCount       \* At least 1 child
    /\ encoded = FALSE
    /\ decoded_slots = <<>>
    /\ decode_error = FALSE
    /\ step = 0

--------------------------------------------------------------------------------
(* ENCODING OPERATIONS                                                       *)
--------------------------------------------------------------------------------

\* BUGGY encoding: accepts any sequential siblings without bounds check
\* This allows encoding that will produce invalid slot IDs on decode
EncodeBuggy ==
    /\ step < MaxSteps
    /\ encoded = FALSE
    /\ child_count > 0
    \* BUG: No check that first_child_slot + child_count - 1 < arena_node_count!
    /\ encoded' = TRUE
    /\ UNCHANGED <<arena_node_count, first_child_slot, child_count, decoded_slots, decode_error>>
    /\ step' = step + 1

\* FIXED encoding: validates bounds before accepting sequential encoding
\* Rejects encoding if last slot would exceed arena bounds or overflow u32
EncodeFixed ==
    /\ step < MaxSteps
    /\ encoded = FALSE
    /\ child_count > 0
    \* Key fix: Check that last slot is valid
    /\ LET last == LastSlot(first_child_slot, child_count)
       IN /\ last < arena_node_count     \* Within arena bounds
          /\ last <= MaxSlotId           \* No u32 overflow
    /\ encoded' = TRUE
    /\ UNCHANGED <<arena_node_count, first_child_slot, child_count, decoded_slots, decode_error>>
    /\ step' = step + 1

\* Encoding rejected: bounds check fails, so fall back to non-sequential encoding
\* This models check_sequential_char_children() returning None
EncodeRejected ==
    /\ step < MaxSteps
    /\ encoded = FALSE
    /\ child_count > 0
    \* Bounds check fails - reject sequential encoding
    /\ LET last == LastSlot(first_child_slot, child_count)
       IN \/ last >= arena_node_count    \* Exceeds arena bounds
          \/ last > MaxSlotId            \* Would overflow u32
    \* Mark as encoded with error to signal rejection
    /\ decode_error' = TRUE              \* Signal that sequential encoding was rejected
    /\ UNCHANGED <<arena_node_count, first_child_slot, child_count, encoded, decoded_slots>>
    /\ step' = step + 1

--------------------------------------------------------------------------------
(* DECODING OPERATIONS                                                       *)
--------------------------------------------------------------------------------

\* BUGGY decode: blindly adds indices without checking overflow
\* This is the actual bug in decode_sequential_siblings()
DecodeBuggy ==
    /\ step < MaxSteps
    /\ encoded = TRUE
    /\ decoded_slots = <<>>
    /\ decode_error = FALSE
    \* BUG: Generates slots without validating bounds!
    /\ decoded_slots' = [i \in 1..child_count |-> first_child_slot + (i - 1)]
    /\ UNCHANGED <<arena_node_count, first_child_slot, child_count, encoded, decode_error>>
    /\ step' = step + 1

\* FIXED decode with checked_add: uses defensive overflow checking
\* On overflow, sets error flag instead of producing invalid slots
DecodeWithCheckedAdd ==
    /\ step < MaxSteps
    /\ encoded = TRUE
    /\ decoded_slots = <<>>
    /\ decode_error = FALSE
    /\ LET last == LastSlot(first_child_slot, child_count)
       IN IF last > MaxSlotId
          THEN
              \* Overflow detected - signal error
              /\ decode_error' = TRUE
              /\ decoded_slots' = <<>>
          ELSE
              \* Safe to decode
              /\ decoded_slots' = [i \in 1..child_count |-> first_child_slot + (i - 1)]
              /\ decode_error' = FALSE
    /\ UNCHANGED <<arena_node_count, first_child_slot, child_count, encoded>>
    /\ step' = step + 1

\* FIXED decode with arena validation: checks both overflow AND arena bounds
\* This is the complete fix combining encoding check with decode validation
DecodeFixed ==
    /\ step < MaxSteps
    /\ encoded = TRUE
    /\ decoded_slots = <<>>
    /\ decode_error = FALSE
    /\ LET last == LastSlot(first_child_slot, child_count)
       IN IF last > MaxSlotId \/ last >= arena_node_count
          THEN
              \* Invalid: either overflow or exceeds arena
              /\ decode_error' = TRUE
              /\ decoded_slots' = <<>>
          ELSE
              \* Safe to decode - all slots will be valid
              /\ decoded_slots' = [i \in 1..child_count |-> first_child_slot + (i - 1)]
              /\ decode_error' = FALSE
    /\ UNCHANGED <<arena_node_count, first_child_slot, child_count, encoded>>
    /\ step' = step + 1

--------------------------------------------------------------------------------
(* ACCESS OPERATIONS                                                         *)
--------------------------------------------------------------------------------

\* Attempt to access a decoded slot - triggers invariant check
\* In the Rust code, this corresponds to arena.read(slot_id)
AccessSlot ==
    /\ step < MaxSteps
    /\ Len(decoded_slots) > 0
    /\ decode_error = FALSE
    \* If SafetyInvariant holds, this access is safe
    /\ UNCHANGED vars

--------------------------------------------------------------------------------
(* STUTTERING STEP                                                           *)
--------------------------------------------------------------------------------

Stutter ==
    /\ step >= MaxSteps
    /\ UNCHANGED vars

--------------------------------------------------------------------------------
(* NEXT STATE RELATIONS                                                      *)
--------------------------------------------------------------------------------

\* Specification with buggy encode and decode (will find invariant violation)
NextBuggy ==
    \/ EncodeBuggy
    \/ DecodeBuggy
    \/ AccessSlot
    \/ Stutter

\* Specification with fixed encoding but buggy decode
\* (Fixed encoding should prevent issues even with buggy decode)
NextFixedEncode ==
    \/ EncodeFixed
    \/ DecodeBuggy
    \/ AccessSlot
    \/ Stutter

\* Specification with buggy encoding but fixed decode
\* (Fixed decode should detect issues from buggy encoding)
NextFixedDecode ==
    \/ EncodeBuggy
    \/ DecodeFixed
    \/ AccessSlot
    \/ Stutter

\* Fully fixed specification (defense in depth)
NextFixed ==
    \/ EncodeFixed
    \/ EncodeRejected
    \/ DecodeFixed
    \/ AccessSlot
    \/ Stutter

--------------------------------------------------------------------------------
(* SPECIFICATIONS                                                            *)
--------------------------------------------------------------------------------

\* Buggy spec - TLC will find DecodedSlotsValid violation
SpecBuggy == Init /\ [][NextBuggy]_vars

\* Fixed encode only - should preserve safety through encoding check
SpecFixedEncode == Init /\ [][NextFixedEncode]_vars

\* Fixed decode only - should preserve safety through decode check
SpecFixedDecode == Init /\ [][NextFixedDecode]_vars

\* Fully fixed - defense in depth
SpecFixed == Init /\ [][NextFixed]_vars

--------------------------------------------------------------------------------
(* PROPERTIES                                                                *)
--------------------------------------------------------------------------------

\* Safety: decoded slots are always valid (or decode_error is set)
AlwaysSafe == []SafetyInvariant

\* Liveness: encoding followed by decoding eventually produces result
\* (Not strictly necessary but good to verify termination)
EventuallyDecoded ==
    encoded = TRUE ~> (Len(decoded_slots) > 0 \/ decode_error = TRUE)

--------------------------------------------------------------------------------
(* THEOREMS (verified by TLC)                                                *)
--------------------------------------------------------------------------------

\* Theorem 1: Buggy spec violates DecodedSlotsValid
\* Proof: TLC finds counterexample with SpecBuggy where:
\*   first_child_slot = arena_node_count - 1
\*   child_count > 1
\*   => decoded_slots contains slots >= arena_node_count

\* Theorem 2: Fixed encoding prevents invalid decodes
\* Proof: EncodeFixed precondition ensures last_slot < arena_node_count
\*        DecodeBuggy generates slots in [first_child_slot, last_slot]
\*        => all slots < arena_node_count

\* Theorem 3: Fixed decode catches invalid encodings
\* Proof: DecodeFixed checks last_slot against arena_node_count
\*        Sets decode_error = TRUE if check fails
\*        => SafetyInvariant preserved

\* Theorem 4: Defense in depth - both fixes together
\* Proof: EncodeFixed blocks bad input, DecodeFixed catches any remaining
\*        => strongest guarantee of SafetyInvariant

================================================================================
