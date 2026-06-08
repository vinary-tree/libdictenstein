-------------------------- MODULE OverlayEvictionStale --------------------------
(***************************************************************************)
(* 1c OVERWRITE-RACE SAFETY — the `serial_disk_ptr` stamp guard (M-2a).     *)
(*                                                                         *)
(* The companion `OverlayEvictionCas.tla` proves the SET-level overlay      *)
(* safety (no-UAF, no-lost-ack, fault==durable, reachable-not-freed) but    *)
(* models `durable` as version-free membership — a node is simply `\in      *)
(* durable` or not. That model therefore CANNOT express the round-3 lost    *)
(* update: a node whose insert was acknowledged at value-version v2 while    *)
(* its on-disk image still holds v1, evicted to v1's location, so a later    *)
(* read returns the STALE v1 even though v2 was acknowledged.               *)
(*                                                                         *)
(* This spec adds the per-node VALUE-VERSION dimension and models the       *)
(* eviction guard implemented in `evict_overlay_node_at_path`:              *)
(*                                                                         *)
(*     if current.durable_stamp() != disk_ptr.to_raw() { NotEvictable }     *)
(*                                                                         *)
(* as `durableVersion[n] = liveVersion[n]` (the live node still equals the  *)
(* durable image the registry `disk_ptr` names — i.e. it has not been       *)
(* overwritten since that checkpoint). The immutable path-copy invariant    *)
(* (any write rebuilds every ancestor into a fresh stamp-0 node) is why a    *)
(* single per-node stamp suffices: an overwrite of n (or any descendant)    *)
(* moves n to a fresh version whose `durableVersion < liveVersion`.         *)
(*                                                                         *)
(*   USE_GUARD = TRUE  (OverlayEvictionStale.cfg): eviction fires only when  *)
(*     the live node still equals its durable image -> `NoStaleEvict` holds. *)
(*   USE_GUARD = FALSE (OverlayEvictionStale_Unsafe.cfg, the NEGATIVE        *)
(*     CONTROL): the guard conjunct is dropped, so a node overwritten since  *)
(*     its checkpoint can be evicted to its stale image. TLC MUST report     *)
(*     `NoStaleEvict` VIOLATED — the concrete round-3 lost update — proving   *)
(*     the guard is load-bearing (mirrors the Rust OE5 witness).            *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    Nodes,       \* finite set of trie nodes (terms)
    USE_GUARD,   \* TRUE = the 1c stamp guard is enforced; FALSE = negative control
    MaxVer,      \* TLC finiteness cap on per-node value-version
    MaxRoot      \* TLC finiteness cap on the monotone published-root version tag

VARIABLES
    root,             \* monotone published-root version tag (bumped per successful CAS)
    linkedInMem,      \* nodes reachable IN MEMORY through the published root
    onDisk,           \* nodes whose child slot the evictor swapped to OnDisk
    liveVersion,      \* [Nodes -> Nat] value-version of the live in-memory node (0 = none yet)
    durableVersion,   \* [Nodes -> Nat] value-version the node's on-disk image holds = the STAMP
                      \*               (0 = never checkpointed; the registry holds only > 0)
    ackedVersion,     \* [Nodes -> Nat] the latest acknowledged (Order-A durable) value-version
    evictedToVersion  \* [Nodes -> Nat] the image version a node's slot was unswizzled to

Vars ==
    <<root, linkedInMem, onDisk, liveVersion, durableVersion, ackedVersion,
      evictedToVersion>>

TypeInvariant ==
    /\ root \in Nat
    /\ linkedInMem \subseteq Nodes
    /\ onDisk \subseteq Nodes
    /\ liveVersion \in [Nodes -> 0..MaxVer]
    /\ durableVersion \in [Nodes -> 0..MaxVer]
    /\ ackedVersion \in [Nodes -> 0..MaxVer]
    /\ evictedToVersion \in [Nodes -> 0..MaxVer]

Init ==
    /\ root = 1
    /\ linkedInMem = {}
    /\ onDisk = {}
    /\ liveVersion = [n \in Nodes |-> 0]
    /\ durableVersion = [n \in Nodes |-> 0]
    /\ ackedVersion = [n \in Nodes |-> 0]
    /\ evictedToVersion = [n \in Nodes |-> 0]

(***************************************************************************)
(* Write(n): a lock-free writer publishes (or OVERWRITES) term `n` with a   *)
(* fresh value-version, path-copying its spine into a new InMem node and     *)
(* CAS-publishing (root bump). There is deliberately NO `n \notin           *)
(* linkedInMem` precondition — re-writing an already-linked node is the       *)
(* round-3 OVERWRITE, and allowing it is what makes the negative control      *)
(* reachable (the companion spec's `WriterCas` forbade it). A write copy is   *)
(* never a durable image, so `durableVersion` is UNCHANGED (the fresh node    *)
(* carries stamp 0 until a later Checkpoint re-stamps it). Order A: the WAL    *)
(* record is synced durable before the visibility CAS, so the acknowledged    *)
(* version equals the new live version. A write to an OnDisk node re-links it  *)
(* (the write-path fault-in).                                                 *)
(***************************************************************************)
Write(n) ==
    /\ liveVersion[n] < MaxVer
    /\ root' = root + 1
    /\ liveVersion' = [liveVersion EXCEPT ![n] = liveVersion[n] + 1]
    /\ ackedVersion' = [ackedVersion EXCEPT ![n] = liveVersion[n] + 1]
    /\ linkedInMem' = linkedInMem \cup {n}
    /\ onDisk' = onDisk \ {n}
    /\ UNCHANGED <<durableVersion, evictedToVersion>>

(***************************************************************************)
(* Checkpoint(n): serialize the LIVE node and record (stamp) its durable      *)
(* image at the CURRENT live version — `set_durable_stamp(result_ptr)` at the *)
(* register site. Only a linked node can be checkpointed (we serialize the    *)
(* in-memory overlay). Modeled as a durability action: it does not bump        *)
(* `root` (no root CAS) and does not change reachability.                     *)
(***************************************************************************)
Checkpoint(n) ==
    /\ n \in linkedInMem
    /\ durableVersion' = [durableVersion EXCEPT ![n] = liveVersion[n]]
    /\ UNCHANGED <<root, linkedInMem, onDisk, liveVersion, ackedVersion,
                   evictedToVersion>>

(***************************************************************************)
(* Evict(n): swap `n`'s slot to `OnDisk(disk_ptr)`. The `disk_ptr` names the  *)
(* durable image of version `durableVersion[n]`; the eviction registry holds   *)
(* pointers ONLY for checkpointed nodes, so `durableVersion[n] > 0` is the     *)
(* driver precondition (`disk_ptr.disk_location().is_some()`).                 *)
(*                                                                           *)
(* THE 1c GUARD (USE_GUARD): evict iff the live node still EQUALS its durable  *)
(* image — `durableVersion[n] = liveVersion[n]` — i.e. it was not overwritten  *)
(* since that checkpoint. `evictedToVersion[n]` records which image version     *)
(* the slot now points at (= the durable version). The `_Unsafe` control       *)
(* drops the guard conjunct, permitting a stale evict.                        *)
(***************************************************************************)
Evict(n) ==
    /\ n \in linkedInMem
    /\ durableVersion[n] > 0
    /\ (USE_GUARD => durableVersion[n] = liveVersion[n])
    /\ root' = root + 1
    /\ linkedInMem' = linkedInMem \ {n}
    /\ onDisk' = onDisk \cup {n}
    /\ evictedToVersion' = [evictedToVersion EXCEPT ![n] = durableVersion[n]]
    /\ UNCHANGED <<liveVersion, durableVersion, ackedVersion>>

(***************************************************************************)
(* FaultIn(n): load `n`'s on-disk image back InMem via a loser-safe root CAS.  *)
(* Fault-in writes nothing new — it restores the durable bytes — so the         *)
(* faulted node carries the version of the image it was evicted to              *)
(* (`evictedToVersion[n]`). If that image is STALE (the 1c bug, only reachable  *)
(* with the guard off), the restored `liveVersion < ackedVersion`; but           *)
(* `NoStaleEvict` already flags the stale evict that produced the image, so      *)
(* the violation is caught at the evict, independent of whether a fault-in       *)
(* follows.                                                                     *)
(***************************************************************************)
FaultIn(n) ==
    /\ n \in onDisk
    /\ root' = root + 1
    /\ liveVersion' = [liveVersion EXCEPT ![n] = evictedToVersion[n]]
    /\ linkedInMem' = linkedInMem \cup {n}
    /\ onDisk' = onDisk \ {n}
    /\ UNCHANGED <<durableVersion, ackedVersion, evictedToVersion>>

Next ==
    \/ \E n \in Nodes : Write(n)
    \/ \E n \in Nodes : Checkpoint(n)
    \/ \E n \in Nodes : Evict(n)
    \/ \E n \in Nodes : FaultIn(n)

Spec == Init /\ [][Next]_Vars

\* TLC finiteness: cap the monotone root version tag (the standard idiom; no
\* invariant references its numeric value). `liveVersion` is capped in `Write`.
RootBound == root <= MaxRoot

------------------------------------------------------------------------------
\* Invariants

(***************************************************************************)
(* NoStaleEvict — THE 1c safety property. Every acknowledged write is          *)
(* OBSERVABLE at its acknowledged version: the node is in memory at that         *)
(* version, OR it is OnDisk with an image version EQUAL to the acked version     *)
(* (so a fault-in restores exactly the acked value). A STALE evict (acked v2,    *)
(* on-disk image v1) leaves the node OnDisk with `evictedToVersion = v1 \ne v2`  *)
(* and not linked -> BOTH disjuncts false -> VIOLATED. With the guard ON this    *)
(* is unreachable (eviction requires `durableVersion = liveVersion`, and an      *)
(* acked node has `ackedVersion = liveVersion`, so `evictedToVersion =           *)
(* durableVersion = ackedVersion`).                                            *)
(***************************************************************************)
NoStaleEvict ==
    \A n \in Nodes :
        ackedVersion[n] > 0 =>
            \/ (n \in linkedInMem /\ liveVersion[n] = ackedVersion[n])
            \/ (n \in onDisk /\ evictedToVersion[n] = ackedVersion[n])

\* Sanity: a slot is linked XOR on-disk, never both.
LinkedAndOnDiskDisjoint == linkedInMem \cap onDisk = {}
==============================================================================
