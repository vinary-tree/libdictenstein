------------------------ MODULE CharV3ArenaPublication ------------------------
(*****************************************************************************)
(* Atomic publication for character node-record format V3.                   *)
(*                                                                           *)
(* Encoding arithmetic belongs in Rocq. This state model covers the durable   *)
(* boundary: V2 target headers resolve erased child types; V3 record bytes are*)
(* prepared, checksummed, and committed before an exact-generation root can   *)
(* publish them. A crash discards uncommitted work. Version-aware dispatch     *)
(* accepts both durable V2 and V3 roots in the current reader, while an old    *)
(* reader rejects V3 before interpreting reused header bytes.                  *)
(*****************************************************************************)

EXTENDS Naturals, TLC

CONSTANTS
    UnsafePublishBeforeCommit,
    UnsafePublishBeforeChecksum,
    UnsafeOldReaderAcceptsV3,
    UnsafeCurrentReaderRejectsV2,
    UnsafeCurrentReaderRejectsV3,
    UnsafePublishStaleRoot,
    UnsafeLossyV2Synthesis

ArenaPhases == {"Empty", "Prepared", "Checksummed", "Committed"}
Capabilities == {"V2", "V3"}
ReaderKinds == {"Old", "V3Aware"}
OpenResults == {"None", "Accepted", "Rejected"}

VARIABLES
    arenaPhase,
    rootCapability,
    rootGeneration,
    openResult,
    crashed,
    targetHeadersResolved,
    badUncommittedRoot,
    badUncheckedRoot,
    badOldReaderAcceptance,
    badCurrentReaderRejection,
    badCommittedV3Reopen,
    badStaleRoot,
    badLossyMigration

vars ==
    <<arenaPhase, rootCapability, rootGeneration, openResult, crashed,
      targetHeadersResolved, badUncommittedRoot, badUncheckedRoot,
      badOldReaderAcceptance, badCurrentReaderRejection,
      badCommittedV3Reopen, badStaleRoot, badLossyMigration>>

RecordVersion(capability) == IF capability = "V2" THEN 2 ELSE 3
ReaderMaxVersion(reader) == IF reader = "Old" THEN 2 ELSE 3
ReaderAccepts(reader, capability) ==
    IF reader = "Old" /\ capability = "V3" /\ UnsafeOldReaderAcceptsV3
    THEN TRUE
    ELSE IF reader = "V3Aware" /\ capability = "V2"
            /\ UnsafeCurrentReaderRejectsV2
         THEN FALSE
         ELSE IF reader = "V3Aware" /\ capability = "V3"
                 /\ UnsafeCurrentReaderRejectsV3
              THEN FALSE
              ELSE RecordVersion(capability) <= ReaderMaxVersion(reader)

TypeOK ==
    /\ arenaPhase \in ArenaPhases
    /\ rootCapability \in Capabilities
    /\ rootGeneration \in 0..1
    /\ openResult \in OpenResults
    /\ crashed \in BOOLEAN
    /\ targetHeadersResolved \in BOOLEAN
    /\ badUncommittedRoot \in BOOLEAN
    /\ badUncheckedRoot \in BOOLEAN
    /\ badOldReaderAcceptance \in BOOLEAN
    /\ badCurrentReaderRejection \in BOOLEAN
    /\ badCommittedV3Reopen \in BOOLEAN
    /\ badStaleRoot \in BOOLEAN
    /\ badLossyMigration \in BOOLEAN

NoRootToUncommittedV3Arena == ~badUncommittedRoot
ChecksumPrecedesV3Root == ~badUncheckedRoot
OldReaderRejectsV3 == ~badOldReaderAcceptance
CurrentReaderAcceptsSupportedRoots == ~badCurrentReaderRejection
CommittedV3ReopensAfterCrash == ~badCommittedV3Reopen
PublishedRootHasExactGeneration == ~badStaleRoot
V2MigrationUsesTargetHeaders == ~badLossyMigration

Init ==
    /\ arenaPhase = "Empty"
    /\ rootCapability = "V2"
    /\ rootGeneration = 0
    /\ openResult = "None"
    /\ crashed = FALSE
    /\ targetHeadersResolved = FALSE
    /\ badUncommittedRoot = FALSE
    /\ badUncheckedRoot = FALSE
    /\ badOldReaderAcceptance = FALSE
    /\ badCurrentReaderRejection = FALSE
    /\ badCommittedV3Reopen = FALSE
    /\ badStaleRoot = FALSE
    /\ badLossyMigration = FALSE

ResolveV2TargetHeaders ==
    /\ ~targetHeadersResolved
    /\ targetHeadersResolved' = TRUE
    /\ UNCHANGED <<arenaPhase, rootCapability, rootGeneration, openResult,
                    crashed, badUncommittedRoot, badUncheckedRoot,
                    badOldReaderAcceptance, badCurrentReaderRejection,
                    badCommittedV3Reopen, badStaleRoot, badLossyMigration>>

PrepareV3 ==
    /\ arenaPhase = "Empty"
    /\ (targetHeadersResolved \/ UnsafeLossyV2Synthesis)
    /\ arenaPhase' = "Prepared"
    /\ badLossyMigration' =
         (badLossyMigration \/ ~targetHeadersResolved)
    /\ UNCHANGED <<rootCapability, rootGeneration, openResult, crashed,
                    targetHeadersResolved,
                    badUncommittedRoot, badUncheckedRoot,
                    badOldReaderAcceptance, badCurrentReaderRejection,
                    badCommittedV3Reopen, badStaleRoot>>

FinalizeChecksum ==
    /\ arenaPhase = "Prepared"
    /\ arenaPhase' = "Checksummed"
    /\ UNCHANGED <<rootCapability, rootGeneration, openResult, crashed,
                    targetHeadersResolved,
                    badUncommittedRoot, badUncheckedRoot,
                    badOldReaderAcceptance, badCurrentReaderRejection,
                    badCommittedV3Reopen, badStaleRoot, badLossyMigration>>

CommitArena ==
    /\ arenaPhase = "Checksummed"
    /\ arenaPhase' = "Committed"
    /\ UNCHANGED <<rootCapability, rootGeneration, openResult, crashed,
                    targetHeadersResolved,
                    badUncommittedRoot, badUncheckedRoot,
                    badOldReaderAcceptance, badCurrentReaderRejection,
                    badCommittedV3Reopen, badStaleRoot, badLossyMigration>>

PublishV3Root ==
    /\ (arenaPhase = "Committed" \/ UnsafePublishBeforeCommit)
    /\ (arenaPhase \in {"Checksummed", "Committed"}
        \/ UnsafePublishBeforeChecksum)
    /\ rootCapability' = "V3"
    /\ rootGeneration' = IF UnsafePublishStaleRoot THEN 0 ELSE 1
    /\ badUncommittedRoot' =
         (badUncommittedRoot \/ arenaPhase # "Committed")
    /\ badUncheckedRoot' =
         (badUncheckedRoot \/ arenaPhase \notin {"Checksummed", "Committed"})
    /\ badStaleRoot' = (badStaleRoot \/ UnsafePublishStaleRoot)
    /\ UNCHANGED <<arenaPhase, openResult, crashed,
                    targetHeadersResolved, badOldReaderAcceptance,
                    badCurrentReaderRejection, badCommittedV3Reopen,
                    badLossyMigration>>

Open(reader) ==
    /\ reader \in ReaderKinds
    /\ rootCapability \in Capabilities
    /\ openResult' =
         IF ReaderAccepts(reader, rootCapability)
         THEN "Accepted"
         ELSE "Rejected"
    /\ badOldReaderAcceptance' =
         (badOldReaderAcceptance
          \/ (reader = "Old" /\ rootCapability = "V3"
              /\ ReaderAccepts(reader, rootCapability)))
    /\ badCurrentReaderRejection' =
         (badCurrentReaderRejection
          \/ (reader = "V3Aware"
              /\ ~ReaderAccepts(reader, rootCapability)))
    /\ badCommittedV3Reopen' =
         (badCommittedV3Reopen
          \/ (crashed /\ arenaPhase = "Committed"
              /\ rootCapability = "V3" /\ reader = "V3Aware"
              /\ ~ReaderAccepts(reader, rootCapability)))
    /\ UNCHANGED <<arenaPhase, rootCapability, rootGeneration, crashed,
                    targetHeadersResolved, badUncommittedRoot,
                    badUncheckedRoot, badStaleRoot, badLossyMigration>>

Crash ==
    /\ ~crashed
    /\ crashed' = TRUE
    /\ arenaPhase' = IF arenaPhase = "Committed" THEN "Committed" ELSE "Empty"
    /\ UNCHANGED <<rootCapability, rootGeneration, openResult,
                    targetHeadersResolved, badUncommittedRoot,
                    badUncheckedRoot, badOldReaderAcceptance,
                    badCurrentReaderRejection, badCommittedV3Reopen,
                    badStaleRoot, badLossyMigration>>

Next ==
    \/ ResolveV2TargetHeaders
    \/ PrepareV3
    \/ FinalizeChecksum
    \/ CommitArena
    \/ PublishV3Root
    \/ \E reader \in ReaderKinds : Open(reader)
    \/ Crash

Spec == Init /\ [][Next]_vars

=============================================================================
