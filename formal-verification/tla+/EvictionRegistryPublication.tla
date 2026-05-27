-------------------- MODULE EvictionRegistryPublication --------------------
(****************************************************************************)
(* Bounded model of the persistent char trie's eviction DiskLocationRegistry  *)
(* publication ordering (HEAD commit f10c43e, feature G6).                    *)
(*                                                                          *)
(* A checkpoint serializes the trie to durable on-disk images and builds a    *)
(* fresh PENDING registry of node -> disk-location entries                    *)
(* (serialize_char_node_to_disk + register_char). Only AFTER                  *)
(* verify_checkpoint() succeeds is that registry PUBLISHED to the eviction     *)
(* coordinator (update_disk_registry). The coordinator may then unswizzle      *)
(* (reclaim) an in-memory node box, but only for a published entry that        *)
(* references durable on-disk data. A public write invalidates the published   *)
(* registry at the append_to_wal chokepoint (the A1 data-loss fix). The        *)
(* registry is ephemeral: a crash discards it, and recovery restores           *)
(* membership from the durable on-disk images ALONE -- never from the          *)
(* registry.                                                                  *)
(*                                                                          *)
(* Out of scope: filesystem ordering below a successful sync, and the         *)
(* concurrent writer/checkpoint race (covered by                              *)
(* ConcurrentCheckpointPublication.tla).                                      *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Nodes        \* finite set of node identities (char-paths)

VARIABLES
    inMem,          \* [Nodes -> BOOLEAN] : node currently has an in-memory box
    onDisk,         \* [Nodes -> BOOLEAN] : node has a durable, verified on-disk image
    registry,       \* [Nodes -> BOOLEAN] : node has a PUBLISHED evictable entry
    regPtrDurable,  \* [Nodes -> BOOLEAN] : the published entry references durable data
    pendingReg,     \* [Nodes -> BOOLEAN] : registry built during serialize, not yet published
    ckptPhase,      \* checkpoint phase: "Idle" | "Serialized" | "Verified"
    recovered,      \* [Nodes -> BOOLEAN] : membership restored by the last crash recovery
    justRecovered   \* BOOLEAN : the most recent step was a crash recovery

Vars == <<inMem, onDisk, registry, regPtrDurable, pendingReg,
          ckptPhase, recovered, justRecovered>>

CheckpointPhases == {"Idle", "Serialized", "Verified"}
AllFalse == [n \in Nodes |-> FALSE]
AllTrue  == [n \in Nodes |-> TRUE]

TypeOK ==
    /\ inMem         \in [Nodes -> BOOLEAN]
    /\ onDisk        \in [Nodes -> BOOLEAN]
    /\ registry      \in [Nodes -> BOOLEAN]
    /\ regPtrDurable \in [Nodes -> BOOLEAN]
    /\ pendingReg    \in [Nodes -> BOOLEAN]
    /\ ckptPhase     \in CheckpointPhases
    /\ recovered     \in [Nodes -> BOOLEAN]
    /\ justRecovered \in BOOLEAN

Init ==
    /\ inMem         = AllTrue       \* start fully in memory
    /\ onDisk        = AllFalse
    /\ registry      = AllFalse      \* nothing published until a verify succeeds
    /\ regPtrDurable = AllFalse
    /\ pendingReg    = AllFalse
    /\ ckptPhase     = "Idle"
    /\ recovered     = AllFalse
    /\ justRecovered = FALSE

(* ------------------------------- Actions ------------------------------- *)

\* A public mutation: brings all nodes (back) into memory and INVALIDATES any
\* published registry at the append_to_wal chokepoint (the A1 fix). Permitted
\* only while no checkpoint is mid-flight.
Mutate ==
    /\ ckptPhase = "Idle"
    /\ inMem' = AllTrue
    /\ registry' = AllFalse
    /\ regPtrDurable' = AllFalse
    /\ justRecovered' = FALSE
    /\ UNCHANGED <<onDisk, pendingReg, ckptPhase, recovered>>

\* Step 1: bottom-up serialize. Writes durable on-disk images for every node and
\* builds a fresh PENDING registry. Nothing is published yet.
SerializeAndBuild ==
    /\ ckptPhase = "Idle"
    /\ ckptPhase' = "Serialized"
    /\ onDisk' = AllTrue
    /\ pendingReg' = AllTrue
    /\ justRecovered' = FALSE
    /\ UNCHANGED <<inMem, registry, regPtrDurable, recovered>>

\* Step 2a: verify_checkpoint() succeeds.
VerifySuccess ==
    /\ ckptPhase = "Serialized"
    /\ ckptPhase' = "Verified"
    /\ justRecovered' = FALSE
    /\ UNCHANGED <<inMem, onDisk, registry, regPtrDurable, pendingReg, recovered>>

\* Step 2a': verify_checkpoint() fails -> the checkpoint aborts, the pending
\* registry is dropped and never published.
VerifyFailure ==
    /\ ckptPhase = "Serialized"
    /\ ckptPhase' = "Idle"
    /\ pendingReg' = AllFalse
    /\ justRecovered' = FALSE
    /\ UNCHANGED <<inMem, onDisk, registry, regPtrDurable, recovered>>

\* Step 2b: publish the pending registry to the coordinator -- reachable ONLY
\* from the Verified phase. Each entry is marked durable iff its node's on-disk
\* image exists (it always does post-serialize, but the conjunction makes the
\* durability dependency explicit).
Publish ==
    /\ ckptPhase = "Verified"
    /\ ckptPhase' = "Idle"
    /\ registry' = pendingReg
    /\ regPtrDurable' = [n \in Nodes |-> pendingReg[n] /\ onDisk[n]]
    /\ pendingReg' = AllFalse
    /\ justRecovered' = FALSE
    /\ UNCHANGED <<inMem, onDisk, recovered>>

\* Eviction reclaims an in-memory node box, leaving its on-disk DiskRef. Only a
\* published entry that references durable data may be evicted -- the formal
\* analogue of `evict_node_at_path` unswizzling to a registry-supplied location.
Evict(n) ==
    /\ registry[n]
    /\ regPtrDurable[n]
    /\ inMem[n]
    /\ inMem' = [inMem EXCEPT ![n] = FALSE]
    /\ justRecovered' = FALSE
    /\ UNCHANGED <<onDisk, registry, regPtrDurable, pendingReg, ckptPhase, recovered>>

\* Crash + recovery. The ephemeral registry/pending-registry are discarded;
\* membership is restored from the durable on-disk images ALONE (the registry is
\* not WAL/recovery state).
CrashRecover ==
    /\ recovered' = onDisk
    /\ inMem' = onDisk
    /\ registry' = AllFalse
    /\ regPtrDurable' = AllFalse
    /\ pendingReg' = AllFalse
    /\ ckptPhase' = "Idle"
    /\ justRecovered' = TRUE
    /\ UNCHANGED <<onDisk>>

Next ==
    \/ Mutate
    \/ SerializeAndBuild
    \/ VerifySuccess
    \/ VerifyFailure
    \/ Publish
    \/ \E n \in Nodes : Evict(n)
    \/ CrashRecover

Spec == Init /\ [][Next]_Vars

(* ------------------------------ Invariants ----------------------------- *)

\* CORE SAFETY: every published, evictable entry references durable, verified
\* on-disk data. Combined with Evict's precondition, this proves eviction never
\* unswizzles a live in-memory node onto a non-durable on-disk location.
RegistryEntriesAreDurable ==
    \A n \in Nodes : registry[n] => (onDisk[n] /\ regPtrDurable[n])

\* The durability mark is honest.
RegPtrDurableImpliesOnDisk ==
    \A n \in Nodes : regPtrDurable[n] => onDisk[n]

\* A pending (unpublished) registry exists only while a checkpoint is mid-flight;
\* it never lingers at Idle, so the only path to a non-empty published registry
\* is Publish (reachable only from the Verified phase).
PendingClearedWhenIdle ==
    (ckptPhase = "Idle") => (\A n \in Nodes : ~pendingReg[n])

\* Recovery restores only durable on-disk data.
RecoveredAreDurable ==
    \A n \in Nodes : recovered[n] => onDisk[n]

\* Immediately after a crash, the recovered set equals the durable on-disk set
\* EXACTLY -- computed without any reference to the (discarded) registry. This is
\* the "registry is not recovery state" guarantee.
JustRecoveredMatchesDurable ==
    justRecovered => (recovered = onDisk)

=============================================================================
