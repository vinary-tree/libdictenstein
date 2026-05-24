-------------------------- MODULE PointerOwnership --------------------------
(****************************************************************************)
(* Bounded raw-pointer ownership model for the Rust unsafe boundary.         *)
(*                                                                          *)
(* Scope: CharTrieNodeInner/VocabTrieNode-style child slots are represented  *)
(* as raw in-memory pointers that are produced by Box::into_raw and          *)
(* reclaimed by Box::from_raw. Node-map entries model any side table that    *)
(* can later reconstruct a raw pointer. Lazy-load candidates model the        *)
(* resolve_swizzled_ptr race: several threads may allocate candidates, but   *)
(* only one can publish to the slot and every losing candidate must remain   *)
(* private until it is dropped. Borrow variables model the local Send/Sync   *)
(* discipline required by unsafe implementations: shared borrows may coexist *)
(* only with shared borrows, and mutable borrows require exclusivity.         *)
(*                                                                          *)
(* The model intentionally abstracts away allocator reuse and object fields. *)
(* A pointer identity is allocated at most once, and unswizzling moves the   *)
(* logical node to disk while making the raw address unavailable.            *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Ptrs, Slots, MapEntries, Threads, NoPtr, NoThread

VARIABLES owner, slotPtr, slotDisk, mapPtr, loadCandidate, readers, writer, dropLog

Vars == <<owner, slotPtr, slotDisk, mapPtr, loadCandidate, readers, writer, dropLog>>

States == {"Free", "Box", "Slot", "Detached", "Loading", "Disk", "Dropped"}
LiveStates == {"Box", "Slot", "Detached"}
OwnedStates == LiveStates \cup {"Loading"}
PtrOrNone == Ptrs \cup {NoPtr}
ThreadOrNone == Threads \cup {NoThread}

LiveMem(p) == owner[p] \in LiveStates
NoBorrow(p) == readers[p] = {} /\ writer[p] = NoThread

SlotRefs(p) == {s \in Slots : slotPtr[s] = p}
MapRefs(p) == {m \in MapEntries : mapPtr[m] = p}
LoadRefs(p) == {t \in Threads : loadCandidate[t] = p}

InAnySlot(p) == SlotRefs(p) # {}
InAnyMap(p) == MapRefs(p) # {}
InAnyLoad(p) == LoadRefs(p) # {}

TypeInvariant ==
    /\ NoPtr \notin Ptrs
    /\ NoThread \notin Threads
    /\ owner \in [Ptrs -> States]
    /\ slotPtr \in [Slots -> PtrOrNone]
    /\ slotDisk \in [Slots -> BOOLEAN]
    /\ mapPtr \in [MapEntries -> PtrOrNone]
    /\ loadCandidate \in [Threads -> PtrOrNone]
    /\ readers \in [Ptrs -> SUBSET Threads]
    /\ writer \in [Ptrs -> ThreadOrNone]
    /\ dropLog \in SUBSET Ptrs

Init ==
    /\ owner = [p \in Ptrs |-> "Free"]
    /\ slotPtr = [s \in Slots |-> NoPtr]
    /\ slotDisk = [s \in Slots |-> FALSE]
    /\ mapPtr = [m \in MapEntries |-> NoPtr]
    /\ loadCandidate = [t \in Threads |-> NoPtr]
    /\ readers = [p \in Ptrs |-> {}]
    /\ writer = [p \in Ptrs |-> NoThread]
    /\ dropLog = {}

AllocateBox(p) ==
    /\ p \in Ptrs
    /\ owner[p] = "Free"
    /\ owner' = [owner EXCEPT ![p] = "Box"]
    /\ UNCHANGED <<slotPtr, slotDisk, mapPtr, loadCandidate, readers, writer, dropLog>>

RegisterMapEntry(m, p) ==
    /\ m \in MapEntries
    /\ p \in Ptrs
    /\ LiveMem(p)
    /\ mapPtr[m] = NoPtr
    /\ ~InAnyMap(p)
    /\ mapPtr' = [mapPtr EXCEPT ![m] = p]
    /\ UNCHANGED <<owner, slotPtr, slotDisk, loadCandidate, readers, writer, dropLog>>

ClearMapEntry(m) ==
    /\ m \in MapEntries
    /\ mapPtr[m] # NoPtr
    /\ mapPtr' = [mapPtr EXCEPT ![m] = NoPtr]
    /\ UNCHANGED <<owner, slotPtr, slotDisk, loadCandidate, readers, writer, dropLog>>

InstallInSlot(s, p) ==
    /\ s \in Slots
    /\ p \in Ptrs
    /\ owner[p] \in {"Box", "Detached"}
    /\ slotPtr[s] = NoPtr
    /\ slotDisk[s] = FALSE
    /\ ~InAnySlot(p)
    /\ NoBorrow(p)
    /\ owner' = [owner EXCEPT ![p] = "Slot"]
    /\ slotPtr' = [slotPtr EXCEPT ![s] = p]
    /\ UNCHANGED <<slotDisk, mapPtr, loadCandidate, readers, writer, dropLog>>

RemoveFromSlot(s) ==
    LET p == slotPtr[s] IN
    /\ s \in Slots
    /\ p # NoPtr
    /\ owner[p] = "Slot"
    /\ NoBorrow(p)
    /\ owner' = [owner EXCEPT ![p] = "Detached"]
    /\ slotPtr' = [slotPtr EXCEPT ![s] = NoPtr]
    /\ UNCHANGED <<slotDisk, mapPtr, loadCandidate, readers, writer, dropLog>>

ReplaceSlot(s, newPtr) ==
    LET oldPtr == slotPtr[s] IN
    /\ s \in Slots
    /\ newPtr \in Ptrs
    /\ oldPtr # NoPtr
    /\ oldPtr # newPtr
    /\ owner[oldPtr] = "Slot"
    /\ owner[newPtr] \in {"Box", "Detached"}
    /\ slotDisk[s] = FALSE
    /\ ~InAnySlot(newPtr)
    /\ NoBorrow(oldPtr)
    /\ NoBorrow(newPtr)
    /\ owner' = [owner EXCEPT ![oldPtr] = "Detached", ![newPtr] = "Slot"]
    /\ slotPtr' = [slotPtr EXCEPT ![s] = newPtr]
    /\ UNCHANGED <<slotDisk, mapPtr, loadCandidate, readers, writer, dropLog>>

CloneToFreshBox(src, dst) ==
    /\ src \in Ptrs
    /\ dst \in Ptrs
    /\ src # dst
    /\ LiveMem(src)
    /\ writer[src] = NoThread
    /\ owner[dst] = "Free"
    /\ owner' = [owner EXCEPT ![dst] = "Box"]
    /\ UNCHANGED <<slotPtr, slotDisk, mapPtr, loadCandidate, readers, writer, dropLog>>

UnswizzleSlotToDisk(s) ==
    LET p == slotPtr[s] IN
    /\ s \in Slots
    /\ p # NoPtr
    /\ owner[p] = "Slot"
    /\ NoBorrow(p)
    /\ ~InAnyMap(p)
    /\ p \notin dropLog
    /\ owner' = [owner EXCEPT ![p] = "Disk"]
    /\ slotPtr' = [slotPtr EXCEPT ![s] = NoPtr]
    /\ slotDisk' = [slotDisk EXCEPT ![s] = TRUE]
    /\ dropLog' = dropLog \cup {p}
    /\ UNCHANGED <<mapPtr, loadCandidate, readers, writer>>

BeginLazyLoad(t, p) ==
    /\ t \in Threads
    /\ p \in Ptrs
    /\ owner[p] = "Free"
    /\ loadCandidate[t] = NoPtr
    /\ owner' = [owner EXCEPT ![p] = "Loading"]
    /\ loadCandidate' = [loadCandidate EXCEPT ![t] = p]
    /\ UNCHANGED <<slotPtr, slotDisk, mapPtr, readers, writer, dropLog>>

WinLazySwizzle(t, s) ==
    LET p == loadCandidate[t] IN
    /\ t \in Threads
    /\ s \in Slots
    /\ p # NoPtr
    /\ owner[p] = "Loading"
    /\ slotDisk[s] = TRUE
    /\ slotPtr[s] = NoPtr
    /\ NoBorrow(p)
    /\ owner' = [owner EXCEPT ![p] = "Slot"]
    /\ slotPtr' = [slotPtr EXCEPT ![s] = p]
    /\ slotDisk' = [slotDisk EXCEPT ![s] = FALSE]
    /\ loadCandidate' = [loadCandidate EXCEPT ![t] = NoPtr]
    /\ UNCHANGED <<mapPtr, readers, writer, dropLog>>

DropLazyLoadCandidate(t) ==
    LET p == loadCandidate[t] IN
    /\ t \in Threads
    /\ p # NoPtr
    /\ owner[p] = "Loading"
    /\ NoBorrow(p)
    /\ ~InAnySlot(p)
    /\ ~InAnyMap(p)
    /\ p \notin dropLog
    /\ owner' = [owner EXCEPT ![p] = "Dropped"]
    /\ loadCandidate' = [loadCandidate EXCEPT ![t] = NoPtr]
    /\ dropLog' = dropLog \cup {p}
    /\ UNCHANGED <<slotPtr, slotDisk, mapPtr, readers, writer>>

DropDetachedOrBox(p) ==
    /\ p \in Ptrs
    /\ owner[p] \in {"Box", "Detached"}
    /\ NoBorrow(p)
    /\ ~InAnySlot(p)
    /\ ~InAnyMap(p)
    /\ ~InAnyLoad(p)
    /\ p \notin dropLog
    /\ owner' = [owner EXCEPT ![p] = "Dropped"]
    /\ dropLog' = dropLog \cup {p}
    /\ UNCHANGED <<slotPtr, slotDisk, mapPtr, loadCandidate, readers, writer>>

StartSharedBorrow(t, p) ==
    /\ t \in Threads
    /\ p \in Ptrs
    /\ LiveMem(p)
    /\ writer[p] = NoThread
    /\ t \notin readers[p]
    /\ readers' = [readers EXCEPT ![p] = @ \cup {t}]
    /\ UNCHANGED <<owner, slotPtr, slotDisk, mapPtr, loadCandidate, writer, dropLog>>

EndSharedBorrow(t, p) ==
    /\ t \in Threads
    /\ p \in Ptrs
    /\ t \in readers[p]
    /\ readers' = [readers EXCEPT ![p] = @ \ {t}]
    /\ UNCHANGED <<owner, slotPtr, slotDisk, mapPtr, loadCandidate, writer, dropLog>>

StartMutableBorrow(t, p) ==
    /\ t \in Threads
    /\ p \in Ptrs
    /\ LiveMem(p)
    /\ NoBorrow(p)
    /\ writer' = [writer EXCEPT ![p] = t]
    /\ UNCHANGED <<owner, slotPtr, slotDisk, mapPtr, loadCandidate, readers, dropLog>>

EndMutableBorrow(t, p) ==
    /\ t \in Threads
    /\ p \in Ptrs
    /\ writer[p] = t
    /\ writer' = [writer EXCEPT ![p] = NoThread]
    /\ UNCHANGED <<owner, slotPtr, slotDisk, mapPtr, loadCandidate, readers, dropLog>>

Next ==
    \/ \E p \in Ptrs : AllocateBox(p)
    \/ \E m \in MapEntries, p \in Ptrs : RegisterMapEntry(m, p)
    \/ \E m \in MapEntries : ClearMapEntry(m)
    \/ \E s \in Slots, p \in Ptrs : InstallInSlot(s, p)
    \/ \E s \in Slots : RemoveFromSlot(s)
    \/ \E s \in Slots, p \in Ptrs : ReplaceSlot(s, p)
    \/ \E src \in Ptrs, dst \in Ptrs : CloneToFreshBox(src, dst)
    \/ \E s \in Slots : UnswizzleSlotToDisk(s)
    \/ \E t \in Threads, p \in Ptrs : BeginLazyLoad(t, p)
    \/ \E t \in Threads, s \in Slots : WinLazySwizzle(t, s)
    \/ \E t \in Threads : DropLazyLoadCandidate(t)
    \/ \E p \in Ptrs : DropDetachedOrBox(p)
    \/ \E t \in Threads, p \in Ptrs : StartSharedBorrow(t, p)
    \/ \E t \in Threads, p \in Ptrs : EndSharedBorrow(t, p)
    \/ \E t \in Threads, p \in Ptrs : StartMutableBorrow(t, p)
    \/ \E t \in Threads, p \in Ptrs : EndMutableBorrow(t, p)

SlotPointersAreLiveAndOwned ==
    \A s \in Slots :
        slotPtr[s] # NoPtr =>
            /\ slotPtr[s] \in Ptrs
            /\ owner[slotPtr[s]] = "Slot"

SlotDiskAndRawStatesAreDisjoint ==
    \A s \in Slots :
        /\ (slotDisk[s] => slotPtr[s] = NoPtr)
        /\ (slotPtr[s] # NoPtr => slotDisk[s] = FALSE)

SlotOwnersHaveUniqueSlot ==
    \A p \in Ptrs :
        owner[p] = "Slot" => Cardinality(SlotRefs(p)) = 1

NoSlotAliasing ==
    \A p \in Ptrs :
        Cardinality(SlotRefs(p)) <= 1

MapPointersAreLive ==
    \A m \in MapEntries :
        mapPtr[m] # NoPtr =>
            /\ mapPtr[m] \in Ptrs
            /\ LiveMem(mapPtr[m])

LoadingPointersAreThreadLocal ==
    \A p \in Ptrs :
        owner[p] = "Loading" =>
            /\ Cardinality(LoadRefs(p)) = 1
            /\ ~InAnySlot(p)
            /\ ~InAnyMap(p)
            /\ readers[p] = {}
            /\ writer[p] = NoThread

NoLoadCandidateAliasing ==
    \A p \in Ptrs :
        Cardinality(LoadRefs(p)) <= 1

NoMapAliasing ==
    \A p \in Ptrs :
        Cardinality(MapRefs(p)) <= 1

UnavailablePointersHaveNoRawReferences ==
    \A p \in Ptrs :
        owner[p] \in {"Disk", "Dropped"} =>
            /\ ~InAnySlot(p)
            /\ ~InAnyMap(p)
            /\ ~InAnyLoad(p)
            /\ readers[p] = {}
            /\ writer[p] = NoThread

BorrowDiscipline ==
    \A p \in Ptrs :
        /\ (writer[p] # NoThread => readers[p] = {})
        /\ (readers[p] # {} => writer[p] = NoThread)
        /\ (writer[p] # NoThread => LiveMem(p))
        /\ (readers[p] # {} => LiveMem(p))

DroppedPointersNeverBecomeLive ==
    dropLog \subseteq {p \in Ptrs : owner[p] \notin OwnedStates}

Spec == Init /\ [][Next]_Vars

=============================================================================
