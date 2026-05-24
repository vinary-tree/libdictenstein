------------------------- MODULE StorageSyscallOutcome -------------------------
(****************************************************************************)
(* Bounded model for storage syscall outcomes at the WAL/block durability     *)
(* boundary.                                                                 *)
(*                                                                           *)
(* Scope: a record becomes reported durable only after its write completed    *)
(* fully and its sync completed successfully. Short writes, negative errors,  *)
(* interrupted operations, cancellation, and missing completions fail closed   *)
(* and cannot advance the durable prefix. Recovery applies only that durable  *)
(* prefix. Kernel and filesystem internals remain outside this model.         *)
(****************************************************************************)

EXTENDS Naturals, TLC

CONSTANT MaxRecord

Records == 1..MaxRecord
Phases == {"Idle", "WriteIssued", "WriteChecked", "SyncIssued", "SyncChecked"}
WriteOutcomes == {"None", "Full", "Short", "Error", "Interrupted",
                  "Cancelled", "MissingCompletion"}
SyncOutcomes == {"None", "Ok", "Error", "Interrupted", "MissingCompletion"}
FailedWriteOutcomes == WriteOutcomes \ {"None", "Full"}
FailedSyncOutcomes == SyncOutcomes \ {"None", "Ok"}

VARIABLES phase, writeOutcome, syncOutcome, failed,
          durableLsn, reportedLsn, recoveredLsn

Vars == <<phase, writeOutcome, syncOutcome, failed,
          durableLsn, reportedLsn, recoveredLsn>>

TypeInvariant ==
    /\ MaxRecord \in Nat
    /\ MaxRecord > 0
    /\ phase \in [Records -> Phases]
    /\ writeOutcome \in [Records -> WriteOutcomes]
    /\ syncOutcome \in [Records -> SyncOutcomes]
    /\ failed \subseteq Records
    /\ durableLsn \in 0..MaxRecord
    /\ reportedLsn \in 0..MaxRecord
    /\ recoveredLsn \in 0..MaxRecord

Init ==
    /\ phase = [r \in Records |-> "Idle"]
    /\ writeOutcome = [r \in Records |-> "None"]
    /\ syncOutcome = [r \in Records |-> "None"]
    /\ failed = {}
    /\ durableLsn = 0
    /\ reportedLsn = 0
    /\ recoveredLsn = 0

StartWrite(r) ==
    /\ r \in Records
    /\ r = durableLsn + 1
    /\ phase[r] \in {"Idle", "WriteChecked"}
    /\ writeOutcome[r] # "Full"
    /\ phase' = [phase EXCEPT ![r] = "WriteIssued"]
    /\ writeOutcome' = [writeOutcome EXCEPT ![r] = "None"]
    /\ syncOutcome' = [syncOutcome EXCEPT ![r] = "None"]
    /\ failed' = failed \ {r}
    /\ UNCHANGED <<durableLsn, reportedLsn, recoveredLsn>>

CompleteWrite(r, outcome) ==
    /\ r \in Records
    /\ outcome \in WriteOutcomes \ {"None"}
    /\ phase[r] = "WriteIssued"
    /\ phase' = [phase EXCEPT ![r] = "WriteChecked"]
    /\ writeOutcome' = [writeOutcome EXCEPT ![r] = outcome]
    /\ failed' = IF outcome = "Full" THEN failed \ {r} ELSE failed \cup {r}
    /\ UNCHANGED <<syncOutcome, durableLsn, reportedLsn, recoveredLsn>>

StartSync(r) ==
    /\ r \in Records
    /\ r = durableLsn + 1
    /\ writeOutcome[r] = "Full"
    /\ syncOutcome[r] # "Ok"
    /\ phase[r] \in {"WriteChecked", "SyncChecked"}
    /\ phase' = [phase EXCEPT ![r] = "SyncIssued"]
    /\ syncOutcome' = [syncOutcome EXCEPT ![r] = "None"]
    /\ UNCHANGED <<writeOutcome, failed, durableLsn, reportedLsn, recoveredLsn>>

CompleteSync(r, outcome) ==
    /\ r \in Records
    /\ outcome \in SyncOutcomes \ {"None"}
    /\ phase[r] = "SyncIssued"
    /\ phase' = [phase EXCEPT ![r] = "SyncChecked"]
    /\ syncOutcome' = [syncOutcome EXCEPT ![r] = outcome]
    /\ failed' = IF outcome = "Ok" THEN failed \ {r} ELSE failed \cup {r}
    /\ durableLsn' = IF outcome = "Ok" THEN r ELSE durableLsn
    /\ reportedLsn' = IF outcome = "Ok" THEN r ELSE reportedLsn
    /\ UNCHANGED <<writeOutcome, recoveredLsn>>

RecoverNext ==
    /\ recoveredLsn < durableLsn
    /\ recoveredLsn' = recoveredLsn + 1
    /\ UNCHANGED <<phase, writeOutcome, syncOutcome, failed, durableLsn, reportedLsn>>

Next ==
    \/ \E r \in Records : StartWrite(r)
    \/ \E r \in Records, outcome \in WriteOutcomes \ {"None"} :
        CompleteWrite(r, outcome)
    \/ \E r \in Records : StartSync(r)
    \/ \E r \in Records, outcome \in SyncOutcomes \ {"None"} :
        CompleteSync(r, outcome)
    \/ RecoverNext

DurableOnlyAfterFullWriteAndSuccessfulSync ==
    \A r \in Records :
        r <= durableLsn => /\ writeOutcome[r] = "Full"
                           /\ syncOutcome[r] = "Ok"

ReportedSuccessOnlyAfterDurability ==
    reportedLsn <= durableLsn

FailedWritesDoNotAdvanceDurability ==
    \A r \in Records :
        writeOutcome[r] \in FailedWriteOutcomes => r > durableLsn

FailedSyncsDoNotAdvanceDurability ==
    \A r \in Records :
        syncOutcome[r] \in FailedSyncOutcomes => r > durableLsn

MissingCompletionFailsClosed ==
    \A r \in Records :
        \/ writeOutcome[r] = "MissingCompletion"
        \/ syncOutcome[r] = "MissingCompletion"
        => r > reportedLsn

CancelledWritesFailClosed ==
    \A r \in Records :
        writeOutcome[r] = "Cancelled" => r > reportedLsn

RecoveryAppliesOnlyDurablePrefix ==
    recoveredLsn <= durableLsn

DurabilityIsPrefixClosed ==
    \A r \in Records :
        r <= durableLsn => \A earlier \in Records :
            earlier <= r => /\ writeOutcome[earlier] = "Full"
                            /\ syncOutcome[earlier] = "Ok"

Spec == Init /\ [][Next]_Vars

=============================================================================
