-------------------- MODULE PublicReadSnapshotTraversal --------------------
(***************************************************************************)
(* Public read traversal snapshot model.                                    *)
(*                                                                         *)
(* Scope: bounded public iter/prefix/zipper-style traversals over a         *)
(* persistent trie. A successful read returns exactly the visible snapshot   *)
(* under the requested prefix. A lazy/disk corruption hit fails closed: it   *)
(* returns no fabricated entries and does not mutate visible or checkpoint   *)
(* state. Checkpoint/recovery are modeled at the trie snapshot boundary, not *)
(* below the filesystem/syscall abstraction.                                *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS MaxKey, MaxPrefix

VARIABLES visible, checkpointed, diskCorrupt,
          readerPrefix, readerSnapshot, readerResult, readerStatus,
          recovered, recoveryFresh

Vars == <<visible, checkpointed, diskCorrupt,
          readerPrefix, readerSnapshot, readerResult, readerStatus,
          recovered, recoveryFresh>>

Keys == 1..MaxKey
Prefixes == 0..MaxPrefix
ReaderStatuses == {"Idle", "Ok", "Err"}

HasPrefix(prefix, key) == prefix = 0 \/ prefix = key

SnapshotUnder(snapshot, prefix) == {key \in snapshot : HasPrefix(prefix, key)}
VisibleUnder(prefix) == SnapshotUnder(visible, prefix)
CorruptUnder(prefix) == {key \in diskCorrupt : HasPrefix(prefix, key)}

TypeInvariant ==
    /\ MaxKey >= 1
    /\ MaxPrefix >= 0
    /\ visible \subseteq Keys
    /\ checkpointed \subseteq Keys
    /\ diskCorrupt \subseteq checkpointed
    /\ readerPrefix \in Prefixes
    /\ readerSnapshot \subseteq Keys
    /\ readerResult \subseteq Keys
    /\ readerStatus \in ReaderStatuses
    /\ recovered \subseteq Keys
    /\ recoveryFresh \in BOOLEAN

Init ==
    /\ visible = {}
    /\ checkpointed = {}
    /\ diskCorrupt = {}
    /\ readerPrefix = 0
    /\ readerSnapshot = {}
    /\ readerResult = {}
    /\ readerStatus = "Idle"
    /\ recovered = {}
    /\ recoveryFresh = FALSE

PublishWrite(key) ==
    /\ key \in Keys
    /\ visible' = visible \cup {key}
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<checkpointed, diskCorrupt, readerPrefix,
                  readerSnapshot, readerResult, readerStatus, recovered>>

Checkpoint ==
    /\ checkpointed' = visible
    /\ diskCorrupt' = diskCorrupt \cap visible
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<visible, readerPrefix, readerSnapshot, readerResult,
                  readerStatus, recovered>>

CorruptDisk(key) ==
    /\ key \in checkpointed
    /\ diskCorrupt' = diskCorrupt \cup {key}
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<visible, checkpointed, readerPrefix,
                  readerSnapshot, readerResult, readerStatus, recovered>>

RepairDisk(key) ==
    /\ key \in diskCorrupt
    /\ diskCorrupt' = diskCorrupt \ {key}
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<visible, checkpointed, readerPrefix,
                  readerSnapshot, readerResult, readerStatus, recovered>>

ReadPrefixOk(prefix) ==
    /\ prefix \in Prefixes
    /\ CorruptUnder(prefix) = {}
    /\ readerPrefix' = prefix
    /\ readerSnapshot' = visible
    /\ readerResult' = VisibleUnder(prefix)
    /\ readerStatus' = "Ok"
    /\ UNCHANGED <<visible, checkpointed, diskCorrupt, recovered, recoveryFresh>>

ReadPrefixFailure(prefix) ==
    /\ prefix \in Prefixes
    /\ CorruptUnder(prefix) # {}
    /\ readerPrefix' = prefix
    /\ readerSnapshot' = visible
    /\ readerResult' = {}
    /\ readerStatus' = "Err"
    /\ UNCHANGED <<visible, checkpointed, diskCorrupt, recovered, recoveryFresh>>

Recover ==
    /\ recovered' = checkpointed \ diskCorrupt
    /\ recoveryFresh' = TRUE
    /\ UNCHANGED <<visible, checkpointed, diskCorrupt,
                  readerPrefix, readerSnapshot, readerResult, readerStatus>>

Next ==
    \/ \E key \in Keys : PublishWrite(key)
    \/ Checkpoint
    \/ \E key \in Keys : CorruptDisk(key)
    \/ \E key \in Keys : RepairDisk(key)
    \/ \E prefix \in Prefixes : ReadPrefixOk(prefix)
    \/ \E prefix \in Prefixes : ReadPrefixFailure(prefix)
    \/ Recover

CheckpointContainsOnlyVisible ==
    checkpointed \subseteq visible

ReadDoesNotFabricate ==
    readerResult \subseteq readerSnapshot

ReaderSnapshotWasVisible ==
    readerSnapshot \subseteq visible

SuccessfulReadIsSound ==
    readerStatus = "Ok" =>
        /\ readerResult \subseteq readerSnapshot
        /\ \A key \in readerResult : HasPrefix(readerPrefix, key)

SuccessfulReadIsComplete ==
    readerStatus = "Ok" =>
        SnapshotUnder(readerSnapshot, readerPrefix) \subseteq readerResult

SuccessfulReadIsExact ==
    readerStatus = "Ok" =>
        readerResult = SnapshotUnder(readerSnapshot, readerPrefix)

FailedReadIsClosed ==
    readerStatus = "Err" =>
        readerResult = {}

RecoveredUsesOnlyCheckpoint ==
    recoveryFresh => recovered \subseteq checkpointed

RecoveredSkipsCorruptDisk ==
    recoveryFresh => recovered \cap diskCorrupt = {}

Spec == Init /\ [][Next]_Vars

=============================================================================
