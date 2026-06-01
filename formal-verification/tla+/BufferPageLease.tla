-------------------------- MODULE BufferPageLease --------------------------
(***************************************************************************)
(* BufferManager page lease model.                                          *)
(*                                                                         *)
(* Scope: the unsafe boundary where BufferManager turns interior page-pool  *)
(* storage into shared page references or mutable page references. The      *)
(* implementation represents the lease state with a single atomic word per  *)
(* frame: read leases may coexist with other reads, while a write lease is  *)
(* exclusive. TraversalContext may cache raw page pointers only while it     *)
(* keeps a read lease on the frame.                                         *)
(***************************************************************************)

EXTENDS FiniteSets, Naturals, TLC

CONSTANTS Frames, Blocks, NoBlock, MaxReaders

VARIABLES owner, readLeaseCount, writeLeases, cached, dirty, flushing

Vars == <<owner, readLeaseCount, writeLeases, cached, dirty, flushing>>

BlockOrEmpty == Blocks \cup {NoBlock}

ResidentFrames == {f \in Frames : owner[f] # NoBlock}

ReadPinnedFrames == {f \in Frames : readLeaseCount[f] > 0}

TypeInvariant ==
    /\ owner \in [Frames -> BlockOrEmpty]
    /\ readLeaseCount \in [Frames -> 0..MaxReaders]
    /\ writeLeases \subseteq Frames
    /\ cached \subseteq Frames
    /\ dirty \subseteq Frames
    /\ flushing \subseteq Frames
    /\ NoBlock \notin Blocks
    /\ MaxReaders \in Nat \ {0}

Init ==
    /\ owner = [f \in Frames |-> NoBlock]
    /\ readLeaseCount = [f \in Frames |-> 0]
    /\ writeLeases = {}
    /\ cached = {}
    /\ dirty = {}
    /\ flushing = {}

LoadCachedRead(f, b) ==
    /\ f \in Frames
    /\ b \in Blocks
    /\ owner[f] = NoBlock
    /\ owner' = [owner EXCEPT ![f] = b]
    /\ readLeaseCount' = [readLeaseCount EXCEPT ![f] = 1]
    /\ cached' = cached \cup {f}
    /\ flushing' = {}
    /\ UNCHANGED <<writeLeases, dirty>>

PinRead(f) ==
    /\ f \in ResidentFrames
    /\ f \notin writeLeases
    /\ readLeaseCount[f] < MaxReaders
    /\ readLeaseCount' = [readLeaseCount EXCEPT ![f] = @ + 1]
    /\ flushing' = {}
    /\ UNCHANGED <<owner, writeLeases, cached, dirty>>

PinCachedRead(f) ==
    /\ f \in ResidentFrames
    /\ f \notin writeLeases
    /\ readLeaseCount[f] < MaxReaders
    /\ readLeaseCount' = [readLeaseCount EXCEPT ![f] = @ + 1]
    /\ cached' = cached \cup {f}
    /\ flushing' = {}
    /\ UNCHANGED <<owner, writeLeases, dirty>>

ReleaseRead(f) ==
    /\ readLeaseCount[f] > 0
    /\ f \notin cached \/ readLeaseCount[f] > 1
    /\ readLeaseCount' = [readLeaseCount EXCEPT ![f] = @ - 1]
    /\ flushing' = {}
    /\ UNCHANGED <<owner, writeLeases, cached, dirty>>

ReleaseCachedRead(f) ==
    /\ f \in cached
    /\ readLeaseCount[f] > 0
    /\ readLeaseCount' = [readLeaseCount EXCEPT ![f] = @ - 1]
    /\ cached' = cached \ {f}
    /\ flushing' = {}
    /\ UNCHANGED <<owner, writeLeases, dirty>>

AcquireWrite(f) ==
    /\ f \in ResidentFrames
    /\ readLeaseCount[f] = 0
    /\ f \notin writeLeases
    /\ writeLeases' = writeLeases \cup {f}
    /\ flushing' = {}
    /\ UNCHANGED <<owner, readLeaseCount, cached, dirty>>

ReleaseWrite(f) ==
    /\ f \in writeLeases
    /\ writeLeases' = writeLeases \ {f}
    /\ dirty' = dirty \cup {f}
    /\ flushing' = {}
    /\ UNCHANGED <<owner, readLeaseCount, cached>>

Flush(f) ==
    /\ f \in dirty
    /\ f \notin writeLeases
    /\ dirty' = dirty \ {f}
    /\ flushing' = {f}
    /\ UNCHANGED <<owner, readLeaseCount, writeLeases, cached>>

Evict(f) ==
    /\ f \in ResidentFrames
    /\ readLeaseCount[f] = 0
    /\ f \notin writeLeases
    /\ owner' = [owner EXCEPT ![f] = NoBlock]
    /\ dirty' = dirty \ {f}
    /\ cached' = cached \ {f}
    /\ flushing' = {}
    /\ UNCHANGED <<readLeaseCount, writeLeases>>

Next ==
    \/ \E f \in Frames, b \in Blocks : LoadCachedRead(f, b)
    \/ \E f \in Frames : PinRead(f)
    \/ \E f \in Frames : PinCachedRead(f)
    \/ \E f \in Frames : ReleaseRead(f)
    \/ \E f \in Frames : ReleaseCachedRead(f)
    \/ \E f \in Frames : AcquireWrite(f)
    \/ \E f \in Frames : ReleaseWrite(f)
    \/ \E f \in Frames : Flush(f)
    \/ \E f \in Frames : Evict(f)

Spec == Init /\ [][Next]_Vars

CachedPagesPinned == cached \subseteq ReadPinnedFrames

NoReadWriteAlias == ReadPinnedFrames \cap writeLeases = {}

CachedFramesResident == cached \subseteq ResidentFrames

WriteFramesResident == writeLeases \subseteq ResidentFrames

DirtyFramesResident == dirty \subseteq ResidentFrames

FlushesExcludeWriteLease == flushing \cap writeLeases = {}

=============================================================================
