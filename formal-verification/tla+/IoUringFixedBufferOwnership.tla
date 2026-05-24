--------------------- MODULE IoUringFixedBufferOwnership ---------------------
(****************************************************************************)
(* Bounded ownership model for io_uring fixed-buffer registration.           *)
(*                                                                          *)
(* Scope: BufferManager owns aligned buffers, registers them with the        *)
(* io_uring backend, may submit fixed-buffer reads/writes while registered,  *)
(* unregisters before the owner drops the buffer pool, and falls back to      *)
(* non-fixed I/O after unregister or failed registration. The model does not *)
(* verify kernel SQE/CQE internals; it checks the lifetime protocol around   *)
(* the unsafe registered-pointer boundary.                                   *)
(****************************************************************************)

EXTENDS FiniteSets, TLC

CONSTANTS Buffers, Threads, NoBuffer

VARIABLES state, registered, inFlight, fixedUse, standardUse, rejected

Vars == <<state, registered, inFlight, fixedUse, standardUse, rejected>>

States == {"Unregistered", "Registered", "Dropped"}
BufferOrNone == Buffers \cup {NoBuffer}

TypeInvariant ==
    /\ NoBuffer \notin Buffers
    /\ state \in [Buffers -> States]
    /\ registered \in SUBSET Buffers
    /\ inFlight \in [Threads -> BufferOrNone]
    /\ fixedUse \in SUBSET Buffers
    /\ standardUse \in SUBSET Buffers
    /\ rejected \in SUBSET Buffers

Init ==
    /\ state = [b \in Buffers |-> "Unregistered"]
    /\ registered = {}
    /\ inFlight = [t \in Threads |-> NoBuffer]
    /\ fixedUse = {}
    /\ standardUse = {}
    /\ rejected = {}

RegisterValid(b) ==
    /\ b \in Buffers
    /\ state[b] = "Unregistered"
    /\ b \notin rejected
    /\ state' = [state EXCEPT ![b] = "Registered"]
    /\ registered' = registered \cup {b}
    /\ UNCHANGED <<inFlight, fixedUse, standardUse, rejected>>

RejectInvalidRegistration(b) ==
    /\ b \in Buffers
    /\ state[b] = "Unregistered"
    /\ b \notin fixedUse
    /\ rejected' = rejected \cup {b}
    /\ UNCHANGED <<state, registered, inFlight, fixedUse, standardUse>>

SubmitFixed(t, b) ==
    /\ t \in Threads
    /\ b \in Buffers
    /\ inFlight[t] = NoBuffer
    /\ state[b] = "Registered"
    /\ b \in registered
    /\ inFlight' = [inFlight EXCEPT ![t] = b]
    /\ fixedUse' = fixedUse \cup {b}
    /\ UNCHANGED <<state, registered, standardUse, rejected>>

CompleteFixed(t) ==
    /\ t \in Threads
    /\ inFlight[t] # NoBuffer
    /\ inFlight' = [inFlight EXCEPT ![t] = NoBuffer]
    /\ UNCHANGED <<state, registered, fixedUse, standardUse, rejected>>

Unregister(b) ==
    /\ b \in Buffers
    /\ state[b] = "Registered"
    /\ b \in registered
    /\ \A t \in Threads : inFlight[t] # b
    /\ state' = [state EXCEPT ![b] = "Unregistered"]
    /\ registered' = registered \ {b}
    /\ UNCHANGED <<inFlight, fixedUse, standardUse, rejected>>

DropOwner(b) ==
    /\ b \in Buffers
    /\ state[b] = "Unregistered"
    /\ b \notin registered
    /\ \A t \in Threads : inFlight[t] # b
    /\ state' = [state EXCEPT ![b] = "Dropped"]
    /\ UNCHANGED <<registered, inFlight, fixedUse, standardUse, rejected>>

UseStandardIo(t, b) ==
    /\ t \in Threads
    /\ b \in Buffers
    /\ inFlight[t] = NoBuffer
    /\ state[b] \in {"Unregistered", "Registered"}
    /\ standardUse' = standardUse \cup {b}
    /\ UNCHANGED <<state, registered, inFlight, fixedUse, rejected>>

Next ==
    \/ \E b \in Buffers : RegisterValid(b)
    \/ \E b \in Buffers : RejectInvalidRegistration(b)
    \/ \E t \in Threads, b \in Buffers : SubmitFixed(t, b)
    \/ \E t \in Threads : CompleteFixed(t)
    \/ \E b \in Buffers : Unregister(b)
    \/ \E b \in Buffers : DropOwner(b)
    \/ \E t \in Threads, b \in Buffers : UseStandardIo(t, b)

CapabilityMatchesRegisteredBuffers ==
    registered = {b \in Buffers : state[b] = "Registered"}

InFlightFixedBuffersAreRegistered ==
    \A t \in Threads :
        inFlight[t] # NoBuffer => /\ inFlight[t] \in registered
                               /\ state[inFlight[t]] = "Registered"

DroppedBuffersAreNotRegisteredOrInFlight ==
    \A b \in Buffers :
        state[b] = "Dropped" =>
            /\ b \notin registered
            /\ \A t \in Threads : inFlight[t] # b

RejectedBuffersNeverBecomeRegistered ==
    rejected \cap registered = {}

FixedUseWasAccepted ==
    fixedUse \cap rejected = {}

NoUseAfterDropInFlight ==
    \A t \in Threads :
        inFlight[t] # NoBuffer => state[inFlight[t]] # "Dropped"

Spec == Init /\ [][Next]_Vars

=============================================================================
