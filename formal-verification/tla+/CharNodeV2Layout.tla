-------------------------- MODULE CharNodeV2Layout --------------------------
EXTENDS Naturals, TLC

(***************************************************************************)
(* Bounded model for persistent char/vocab v2 node-layout canonicality.      *)
(* The model checks that a node decode succeeds only for known node kinds,   *)
(* valid child counts, coherent relative/sequential flags, zero reserved     *)
(* header bytes, and an exact data_size. Corrupt layouts fail closed.        *)
(***************************************************************************)

CONSTANTS MaxPrefixLen, MaxCount, MaxDataSize

Kinds == {"N4", "N16", "N48", "Bucket"}
KindValues == Kinds \cup {"Bad"}
Counts == 0..MaxCount
PrefixLens == 0..MaxPrefixLen
DataSizes == 0..MaxDataSize

VARIABLES kind, relativeFlag, sequentialFlag, reservedOk, paddingOk,
          prefixLen, childCount, dataSize, decoded

Vars == <<kind, relativeFlag, sequentialFlag, reservedOk, paddingOk,
          prefixLen, childCount, dataSize, decoded>>

StaticVars == <<kind, relativeFlag, sequentialFlag, reservedOk, paddingOk,
                prefixLen, childCount, dataSize>>

Capacity(k) ==
    CASE k = "N4" -> 4
      [] k = "N16" -> 16
      [] k = "N48" -> 48
      [] k = "Bucket" -> MaxCount
      [] OTHER -> 0

FixedSlots(k) ==
    CASE k = "N4" -> 4
      [] k = "N16" -> 16
      [] k = "N48" -> 48
      [] k = "Bucket" -> childCount
      [] OTHER -> 0

PrefixBytes ==
    IF prefixLen = 0 THEN 0 ELSE 24

KeyBytes ==
    CASE kind = "N4" -> 16
      [] kind = "N16" -> 64
      [] kind = "N48" -> 192
      [] kind = "Bucket" -> 4 + (4 * childCount)
      [] OTHER -> 0

ChildBytes ==
    IF sequentialFlag THEN
        1
    ELSE IF relativeFlag THEN
        childCount
    ELSE
        8 * FixedSlots(kind)

ExpectedDataSize ==
    PrefixBytes + KeyBytes + ChildBytes + 8

KnownKind ==
    kind \in Kinds

CapacityOk ==
    childCount <= Capacity(kind)

SequentialFlagsOk ==
    sequentialFlag => relativeFlag

SequentialNonempty ==
    sequentialFlag => childCount > 0

ValidLayout ==
    /\ KnownKind
    /\ prefixLen <= MaxPrefixLen
    /\ reservedOk
    /\ paddingOk
    /\ CapacityOk
    /\ SequentialFlagsOk
    /\ SequentialNonempty
    /\ dataSize = ExpectedDataSize

Init ==
    /\ kind \in KindValues
    /\ relativeFlag \in BOOLEAN
    /\ sequentialFlag \in BOOLEAN
    /\ reservedOk \in BOOLEAN
    /\ paddingOk \in BOOLEAN
    /\ prefixLen \in PrefixLens
    /\ childCount \in Counts
    /\ dataSize \in DataSizes
    /\ decoded = "Pending"

DecodeOk ==
    /\ decoded = "Pending"
    /\ ValidLayout
    /\ decoded' = "Ok"
    /\ UNCHANGED StaticVars

DecodeErr ==
    /\ decoded = "Pending"
    /\ ~ValidLayout
    /\ decoded' = "Err"
    /\ UNCHANGED StaticVars

Done ==
    /\ decoded # "Pending"
    /\ UNCHANGED Vars

Next == DecodeOk \/ DecodeErr \/ Done

Spec == Init /\ [][Next]_Vars

TypeInvariant ==
    /\ kind \in KindValues
    /\ relativeFlag \in BOOLEAN
    /\ sequentialFlag \in BOOLEAN
    /\ reservedOk \in BOOLEAN
    /\ paddingOk \in BOOLEAN
    /\ prefixLen \in PrefixLens
    /\ childCount \in Counts
    /\ dataSize \in DataSizes
    /\ decoded \in {"Pending", "Ok", "Err"}

OkImpliesKnownKind ==
    decoded = "Ok" => KnownKind

OkImpliesCapacity ==
    decoded = "Ok" => CapacityOk

OkImpliesSequentialSafe ==
    decoded = "Ok" /\ sequentialFlag => relativeFlag /\ childCount > 0

OkImpliesExactDataSize ==
    decoded = "Ok" => dataSize = ExpectedDataSize

ErrImpliesInvalidLayout ==
    decoded = "Err" => ~ValidLayout

NoOversizedN4Decode ==
    decoded = "Ok" /\ kind = "N4" => childCount <= 4

=============================================================================
