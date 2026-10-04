----------------------- MODULE LockFreeDawgDropRace -----------------------
(***************************************************************************)
(* Source correspondence: src/dynamic_dawg/lockfree.rs, LockFreeDawgNode  *)
(* Drop. Each edge owns a distinct Arc strong token. Public node handles    *)
(* and captured roots are external tokens. A failed try_unwrap retains its  *)
(* Err token until cleanup; into_inner consumes its token at the decision.  *)
(*                                                                         *)
(* The graph is a finite chain, optionally with an extra forward edge to   *)
(* exercise DAG sharing. Depth is a TLC instance bound, not a library      *)
(* limit. The rank-increasing construction excludes cycles. External       *)
(* holders may release on idle threads; user-defined V::drop reentrancy is *)
(* outside the library-owned native-stack claim. An extracted node's       *)
(* outgoing edges are transferred to the same heap-backed work loop; its  *)
(* later empty Drop is counted as one transient nested native activation.  *)
(***************************************************************************)

EXTENDS Integers, FiniteSets, Sequences, TLC

CONSTANTS Depth, Shared, Policy
ASSUME /\ Depth \in Nat /\ Depth >= 2
       /\ Shared \in BOOLEAN
       /\ Policy \in {"IntoInner", "TryUnwrap"}

Nodes == 0..Depth
Threads == {"destructor", "releaser"}
NoThread == "none"
ChainEdges == {<<"edge", i>> : i \in 0..(Depth - 1)}
SharedEdges == IF Shared THEN {<<"shared", 0>>} ELSE {}
EdgeTokens == ChainEdges \cup SharedEdges
(** The published root and a captured reader revision are separate strong
    owners of node 0. Per-node handles permit the deterministic release race
    at each depth without conflating equal-target token identities. *)
HolderTokens == {<<"node_handle", i>> : i \in 1..Depth} \cup
                {<<"current_root", 0>>, <<"retained_root", 0>>}
Tokens == EdgeTokens \cup HolderTokens

Source(e) == IF e[1] = "edge" THEN e[2] ELSE 0
Target(e) == CASE e[1] = "edge" -> e[2] + 1
               [] e[1] = "shared" -> 2
               [] OTHER -> e[2]
Outgoing(n) == {e \in EdgeTokens : Source(e) = n}
Max(a, b) == IF a >= b THEN a ELSE b

VARIABLES location, owner, frameOwner, phase, frames, work, working,
          destroyedCount, peak
vars == <<location, owner, frameOwner, phase, frames, work, working,
          destroyedCount, peak>>

Strong(n) == Cardinality({r \in Tokens : Target(r) = n /\
                                         location[r] # "gone"})
Top(t) == frames[t][Len(frames[t])]
Pending(t) == IF Len(frames[t]) = 0 THEN {}
              ELSE {e \in EdgeTokens : location[e] = "pending" /\
                      owner[e] = t /\ frameOwner[e] = Top(t)}
Failed(t) == IF Len(frames[t]) = 0 THEN {}
             ELSE {e \in EdgeTokens : location[e] = "failed" /\
                     owner[e] = t /\ frameOwner[e] = Top(t)}

ActivateEdges(n) ==
  [r \in Tokens |-> IF r \in Outgoing(n) THEN "pending" ELSE location[r]]
AssignEdges(n, t) ==
  [r \in Tokens |-> IF r \in Outgoing(n) THEN t ELSE owner[r]]
AssignFrame(n, frame) ==
  [r \in Tokens |-> IF r \in Outgoing(n) THEN frame ELSE frameOwner[r]]

Init ==
  /\ location = [r \in Tokens |-> IF r \in EdgeTokens THEN "edge"
                                  ELSE "external"]
  /\ owner = [r \in Tokens |-> NoThread]
  /\ frameOwner = [r \in Tokens |-> -1]
  /\ phase = [n \in Nodes |-> "live"]
  /\ frames = [t \in Threads |-> <<>>]
  /\ work = [n \in Nodes |-> <<>>]
  /\ working = [n \in Nodes |-> -1]
  /\ destroyedCount = [n \in Nodes |-> 0]
  /\ peak = 0

(** Ordinary release of a public handle or captured root. The releaser is
    idle on this library-owned path; callbacks from arbitrary V are excluded. *)
ReleaseHolder(t, h) ==
  /\ t \in Threads /\ h \in HolderTokens
  /\ location[h] = "external" /\ Len(frames[t]) = 0
  /\ LET n == Target(h)
         final == Strong(n) = 1
     IN /\ location' = IF final THEN
                          [ActivateEdges(n) EXCEPT ![h] = "gone"]
                       ELSE [location EXCEPT ![h] = "gone"]
        /\ owner' = IF final THEN AssignEdges(n, t) ELSE owner
        /\ frameOwner' = IF final THEN AssignFrame(n, n)
                           ELSE frameOwner
        /\ phase' = IF final THEN [phase EXCEPT ![n] = "dropping"]
                     ELSE phase
        /\ frames' = IF final THEN [frames EXCEPT ![t] = Append(@, n)]
                       ELSE frames
        /\ peak' = IF final THEN Max(peak, 1) ELSE peak
  /\ UNCHANGED <<work, working, destroyedCount>>

(** The edge token is consumed by into_inner even on a shared result. In
    contrast, try_unwrap failure moves the still-owning Err token to failed. *)
ConsumeEdge(t, e) ==
  /\ t \in Threads /\ e \in Pending(t) /\ Len(frames[t]) > 0
  /\ LET n == Target(e)
         final == Strong(n) = 1
         fail == Policy = "TryUnwrap" /\ ~final
         frame == Top(t)
     IN /\ location' = [location EXCEPT ![e] = IF fail THEN "failed"
                                                    ELSE "gone"]
        /\ phase' = IF final THEN [phase EXCEPT ![n] = "extracted"]
                     ELSE phase
        /\ work' = IF final THEN [work EXCEPT ![frame] = Append(@, n)]
                    ELSE work
  /\ UNCHANGED <<owner, frameOwner, frames, working,
                destroyedCount, peak>>

(** If another holder released between failure and Err cleanup, this last
    ordinary Arc release synchronously enters the child's Drop on the SAME
    native call stack. Repetition along a chain is the unsafe witness. *)
DiscardFailed(t, e) ==
  /\ Policy = "TryUnwrap"
  /\ t \in Threads /\ e \in Failed(t) /\ Len(frames[t]) > 0
  /\ LET n == Target(e)
         final == Strong(n) = 1
     IN /\ location' = IF final THEN
                          [ActivateEdges(n) EXCEPT ![e] = "gone"]
                       ELSE [location EXCEPT ![e] = "gone"]
        /\ owner' = IF final THEN AssignEdges(n, t) ELSE owner
        /\ frameOwner' = IF final THEN AssignFrame(n, n)
                           ELSE frameOwner
        /\ phase' = IF final THEN [phase EXCEPT ![n] = "dropping"]
                     ELSE phase
        /\ frames' = IF final THEN [frames EXCEPT ![t] = Append(@, n)]
                       ELSE frames
        /\ peak' = IF final THEN Max(peak, Len(frames[t]) + 1)
                    ELSE peak
  /\ UNCHANGED <<work, working, destroyedCount>>

(** A payload extracted from its Arc is processed by the existing heap
    work loop. BeginExtracted transfers its edges to the current native
    frame; FinishExtracted runs only after those edge attempts finish. *)
BeginExtracted(t) ==
  /\ t \in Threads /\ Len(frames[t]) > 0
  /\ Pending(t) = {} /\ Failed(t) = {}
  /\ working[Top(t)] = -1 /\ work[Top(t)] # <<>>
  /\ LET frame == Top(t)
         n == Head(work[frame])
     IN /\ phase[n] = "extracted"
        /\ location' = ActivateEdges(n)
        /\ owner' = AssignEdges(n, t)
        /\ frameOwner' = AssignFrame(n, frame)
        /\ phase' = [phase EXCEPT ![n] = "draining"]
        /\ work' = [work EXCEPT ![frame] = Tail(@)]
        /\ working' = [working EXCEPT ![frame] = n]
  /\ UNCHANGED <<frames, destroyedCount, peak>>

(** The local node's Drop now sees an empty edge vector. It occupies one
    transient native activation above the current Drop frame, then returns. *)
FinishExtracted(t) ==
  /\ t \in Threads /\ Len(frames[t]) > 0
  /\ Pending(t) = {} /\ Failed(t) = {}
  /\ LET frame == Top(t)
         n == working[frame]
     IN /\ n \in Nodes /\ phase[n] = "draining"
        /\ phase' = [phase EXCEPT ![n] = "destroyed"]
        /\ working' = [working EXCEPT ![frame] = -1]
        /\ destroyedCount' = [destroyedCount EXCEPT ![n] = @ + 1]
        /\ peak' = Max(peak, Len(frames[t]) + 1)
  /\ UNCHANGED <<location, owner, frameOwner, frames, work>>

FinishDrop(t) ==
  /\ t \in Threads /\ Len(frames[t]) > 0
  /\ Pending(t) = {} /\ Failed(t) = {} /\ work[Top(t)] = <<>>
  /\ working[Top(t)] = -1
  /\ LET n == Top(t)
     IN /\ phase[n] = "dropping"
        /\ phase' = [phase EXCEPT ![n] = "destroyed"]
        /\ destroyedCount' = [destroyedCount EXCEPT ![n] = @ + 1]
        /\ frames' = [frames EXCEPT ![t] = SubSeq(@, 1, Len(@) - 1)]
  /\ UNCHANGED <<location, owner, frameOwner, work, working, peak>>

Next == \/ \E t \in Threads, h \in HolderTokens : ReleaseHolder(t, h)
        \/ \E t \in Threads, e \in EdgeTokens : ConsumeEdge(t, e)
        \/ \E t \in Threads, e \in EdgeTokens : DiscardFailed(t, e)
        \/ \E t \in Threads : BeginExtracted(t)
        \/ \E t \in Threads : FinishExtracted(t)
        \/ \E t \in Threads : FinishDrop(t)

Spec == Init /\ [][Next]_vars
(** Weak fairness of the complete non-stuttering transition relation models an
    environment that eventually releases every public holder. It does NOT
    promise reclamation while an application deliberately retains a handle.
    Every actual transition advances a finite token or payload phase; the
    Rocq potential argument must establish the unbounded version of this law. *)
FairSpec == Spec /\ WF_vars(Next)

TypeOK ==
  /\ location \in [Tokens -> {"edge", "external", "pending", "failed", "gone"}]
  /\ owner \in [Tokens -> Threads \cup {NoThread}]
  /\ frameOwner \in [Tokens -> Nodes \cup {-1}]
  /\ phase \in [Nodes -> {"live", "dropping", "extracted",
                         "draining", "destroyed"}]
  /\ frames \in [Threads -> Seq(Nodes)]
  /\ work \in [Nodes -> Seq(Nodes)]
  /\ working \in [Nodes -> Nodes \cup {-1}]
  /\ destroyedCount \in [Nodes -> Nat]
  /\ peak \in Nat

ReferenceConservation ==
  Cardinality({r \in Tokens : location[r] # "gone"}) +
  Cardinality({r \in Tokens : location[r] = "gone"}) = Cardinality(Tokens)
RetainedSafety == \A n \in Nodes : Strong(n) > 0 => phase[n] = "live"
ExactlyOnce == \A n \in Nodes : destroyedCount[n] <= 1
PayloadLifecycle == \A n \in Nodes :
  (phase[n] = "live" <=> Strong(n) > 0) /\
  (phase[n] = "destroyed" <=> destroyedCount[n] = 1)
InFlightEdgeHasFrame == \A e \in EdgeTokens :
  location[e] \in {"pending", "failed"} =>
    \E t \in Threads : owner[e] = t /\
      \E i \in 1..Len(frames[t]) : frames[t][i] = frameOwner[e]
EdgeLifecycle == \A e \in EdgeTokens :
  CASE phase[Source(e)] \in {"live", "extracted"} ->
         location[e] = "edge"
    [] phase[Source(e)] = "destroyed" ->
         location[e] = "gone"
    [] OTHER -> location[e] \in {"pending", "failed", "gone"}
NativeBound == peak <= 2
RecursiveDropBound == \A t \in Threads : Len(frames[t]) <= 2
AllDestroyed == \A n \in Nodes : phase[n] = "destroyed"
ProgressEnabled == AllDestroyed \/ ENABLED Next
EventuallyDestroyed == <>AllDestroyed

=============================================================================
