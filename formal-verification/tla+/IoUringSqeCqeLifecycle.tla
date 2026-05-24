------------------------ MODULE IoUringSqeCqeLifecycle ------------------------
(****************************************************************************)
(* Bounded model for io_uring SQE/CQE ownership and completion checking.     *)
(*                                                                          *)
(* Scope: each submitted request owns a buffer until exactly one completion  *)
(* is observed and checked. Negative CQE results and short reads/writes fail *)
(* closed, fixed-buffer requests require a registered buffer while in flight,*)
(* and temporary aligned buffers are not returned/reused until completion    *)
(* checking has run. Kernel SQ/CQ internals are outside this model.          *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Requests, Buffers, NoBuffer

VARIABLES phase, reqBuffer, fixedReq, registered,
          completionResult, completionCount,
          checked, successful, failed, returned

Vars == <<phase, reqBuffer, fixedReq, registered,
          completionResult, completionCount,
          checked, successful, failed, returned>>

Phases == {"Idle", "Submitted", "Completed", "Checked", "Returned"}
Results == {"None", "Ok", "Short", "Error"}
BufferOrNone == Buffers \cup {NoBuffer}
ActivePhases == {"Submitted", "Completed", "Checked"}

BufferActive(b) ==
    \E r \in Requests : reqBuffer[r] = b /\ phase[r] \in ActivePhases

TypeInvariant ==
    /\ NoBuffer \notin Buffers
    /\ phase \in [Requests -> Phases]
    /\ reqBuffer \in [Requests -> BufferOrNone]
    /\ fixedReq \in [Requests -> BOOLEAN]
    /\ registered \in SUBSET Buffers
    /\ completionResult \in [Requests -> Results]
    /\ completionCount \in [Requests -> 0..1]
    /\ checked \in SUBSET Requests
    /\ successful \in SUBSET Requests
    /\ failed \in SUBSET Requests
    /\ returned \in SUBSET Requests

Init ==
    /\ phase = [r \in Requests |-> "Idle"]
    /\ reqBuffer = [r \in Requests |-> NoBuffer]
    /\ fixedReq = [r \in Requests |-> FALSE]
    /\ registered = {}
    /\ completionResult = [r \in Requests |-> "None"]
    /\ completionCount = [r \in Requests |-> 0]
    /\ checked = {}
    /\ successful = {}
    /\ failed = {}
    /\ returned = {}

RegisterBuffer(b) ==
    /\ b \in Buffers
    /\ b \notin registered
    /\ ~BufferActive(b)
    /\ registered' = registered \cup {b}
    /\ UNCHANGED <<phase, reqBuffer, fixedReq, completionResult, completionCount,
                  checked, successful, failed, returned>>

UnregisterBuffer(b) ==
    /\ b \in Buffers
    /\ b \in registered
    /\ \A r \in Requests :
        ~(fixedReq[r] /\ reqBuffer[r] = b /\ phase[r] \in {"Submitted", "Completed"})
    /\ registered' = registered \ {b}
    /\ UNCHANGED <<phase, reqBuffer, fixedReq, completionResult, completionCount,
                  checked, successful, failed, returned>>

SubmitFixed(r, b) ==
    /\ r \in Requests
    /\ b \in Buffers
    /\ phase[r] \in {"Idle", "Returned"}
    /\ b \in registered
    /\ ~BufferActive(b)
    /\ phase' = [phase EXCEPT ![r] = "Submitted"]
    /\ reqBuffer' = [reqBuffer EXCEPT ![r] = b]
    /\ fixedReq' = [fixedReq EXCEPT ![r] = TRUE]
    /\ completionResult' = [completionResult EXCEPT ![r] = "None"]
    /\ completionCount' = [completionCount EXCEPT ![r] = 0]
    /\ checked' = checked \ {r}
    /\ successful' = successful \ {r}
    /\ failed' = failed \ {r}
    /\ returned' = returned \ {r}
    /\ UNCHANGED registered

SubmitTemporary(r, b) ==
    /\ r \in Requests
    /\ b \in Buffers
    /\ phase[r] \in {"Idle", "Returned"}
    /\ ~BufferActive(b)
    /\ phase' = [phase EXCEPT ![r] = "Submitted"]
    /\ reqBuffer' = [reqBuffer EXCEPT ![r] = b]
    /\ fixedReq' = [fixedReq EXCEPT ![r] = FALSE]
    /\ completionResult' = [completionResult EXCEPT ![r] = "None"]
    /\ completionCount' = [completionCount EXCEPT ![r] = 0]
    /\ checked' = checked \ {r}
    /\ successful' = successful \ {r}
    /\ failed' = failed \ {r}
    /\ returned' = returned \ {r}
    /\ UNCHANGED registered

Complete(r, result) ==
    /\ r \in Requests
    /\ result \in {"Ok", "Short", "Error"}
    /\ phase[r] = "Submitted"
    /\ completionCount[r] = 0
    /\ phase' = [phase EXCEPT ![r] = "Completed"]
    /\ completionResult' = [completionResult EXCEPT ![r] = result]
    /\ completionCount' = [completionCount EXCEPT ![r] = 1]
    /\ UNCHANGED <<reqBuffer, fixedReq, registered, checked, successful, failed, returned>>

CheckCompletion(r) ==
    /\ r \in Requests
    /\ phase[r] = "Completed"
    /\ completionCount[r] = 1
    /\ completionResult[r] # "None"
    /\ phase' = [phase EXCEPT ![r] = "Checked"]
    /\ checked' = checked \cup {r}
    /\ successful' = IF completionResult[r] = "Ok"
                     THEN successful \cup {r}
                     ELSE successful
    /\ failed' = IF completionResult[r] = "Ok"
                 THEN failed
                 ELSE failed \cup {r}
    /\ UNCHANGED <<reqBuffer, fixedReq, registered, completionResult,
                  completionCount, returned>>

ReturnTemporary(r) ==
    /\ r \in Requests
    /\ phase[r] = "Checked"
    /\ fixedReq[r] = FALSE
    /\ r \in checked
    /\ phase' = [phase EXCEPT ![r] = "Returned"]
    /\ reqBuffer' = [reqBuffer EXCEPT ![r] = NoBuffer]
    /\ returned' = returned \cup {r}
    /\ UNCHANGED <<fixedReq, registered, completionResult, completionCount,
                  checked, successful, failed>>

Next ==
    \/ \E b \in Buffers : RegisterBuffer(b)
    \/ \E b \in Buffers : UnregisterBuffer(b)
    \/ \E r \in Requests, b \in Buffers : SubmitFixed(r, b)
    \/ \E r \in Requests, b \in Buffers : SubmitTemporary(r, b)
    \/ \E r \in Requests, result \in {"Ok", "Short", "Error"} : Complete(r, result)
    \/ \E r \in Requests : CheckCompletion(r)
    \/ \E r \in Requests : ReturnTemporary(r)

SubmittedRequestsOwnABuffer ==
    \A r \in Requests :
        phase[r] = "Submitted" => reqBuffer[r] # NoBuffer

FixedRequestsUseRegisteredBuffersWhileInFlight ==
    \A r \in Requests :
        fixedReq[r] /\ phase[r] \in {"Submitted", "Completed"} =>
            reqBuffer[r] \in registered

EachRequestCompletesAtMostOnce ==
    \A r \in Requests : completionCount[r] <= 1

CheckedOnlyAfterACompletion ==
    checked \subseteq {r \in Requests : completionCount[r] = 1}

ShortOrErrorResultsFailClosed ==
    \A r \in Requests :
        completionResult[r] \in {"Short", "Error"} => r \notin successful

NoSuccessfulFailedOverlap ==
    successful \cap failed = {}

ReturnedTemporaryBuffersWereChecked ==
    returned \subseteq checked

ReturnedRequestsReleaseTheirBuffer ==
    \A r \in returned : reqBuffer[r] = NoBuffer

NoActiveBufferAliasing ==
    \A r1 \in Requests :
        \A r2 \in Requests :
            r1 # r2 /\ reqBuffer[r1] # NoBuffer /\ reqBuffer[r1] = reqBuffer[r2] =>
                ~(phase[r1] \in ActivePhases /\ phase[r2] \in ActivePhases)

Spec == Init /\ [][Next]_Vars

=============================================================================
