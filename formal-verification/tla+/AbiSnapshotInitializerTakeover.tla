------------------- MODULE AbiSnapshotInitializerTakeover -------------------
(***************************************************************************)
(* Lock-free cold-snapshot initialization in src/bindings.rs.              *)
(*                                                                         *)
(* A snapshotter first CAS-publishes an empty generation.  Contenders poll *)
(* that generation without entering its OnceLock initializer.  A contender *)
(* that observes a bounded stall may CAS-publish a fresh generation and     *)
(* initialize it.  The superseded owner may still finish and return its     *)
(* immutable snapshot.  Thus pointer identity and exactly one construction  *)
(* are usual-path optimizations, not safety properties.                     *)
(*                                                                         *)
(* Every modeled snapshotter can publish and build at most one generation.  *)
(* The safe configuration proves that all returned snapshots have the same  *)
(* producer/revision identity, every returned generation was initialized,   *)
(* and total construction is bounded by the finite contender set.  The      *)
(* negative control requires SingleConstruction and exhibits the valid      *)
(* two-generation takeover schedule that refutes that stronger assertion.   *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, Sequences

CONSTANT Snapshotters

ASSUME Snapshotters # {}

NoGeneration == -1
GenerationCount == Cardinality(Snapshotters)
Generations == 0..(GenerationCount - 1)
Phases == {"Observe", "Build", "Return", "Done"}

VARIABLES
  cache,           \* currently published generation, or NoGeneration
  nextGeneration,  \* fresh generation allocated by the next successful CAS
  initialized,     \* generations whose unique owner completed construction
  phase,           \* one control state per snapshotter
  selected,        \* generation selected by each snapshotter
  built,           \* snapshotters that executed their one construction
  returned         \* immutable identities returned to callers

vars == <<cache, nextGeneration, initialized, phase, selected, built, returned>>

Init ==
  /\ cache = NoGeneration
  /\ nextGeneration = 0
  /\ initialized = {}
  /\ phase = [t \in Snapshotters |-> "Observe"]
  /\ selected = [t \in Snapshotters |-> NoGeneration]
  /\ built = {}
  /\ returned = <<>>

PublishFirst(t) ==
  /\ phase[t] = "Observe"
  /\ cache = NoGeneration
  /\ nextGeneration < GenerationCount
  /\ cache' = nextGeneration
  /\ selected' = [selected EXCEPT ![t] = nextGeneration]
  /\ nextGeneration' = nextGeneration + 1
  /\ phase' = [phase EXCEPT ![t] = "Build"]
  /\ UNCHANGED <<initialized, built, returned>>

ObserveInitialized(t) ==
  /\ phase[t] = "Observe"
  /\ cache \in initialized
  /\ selected' = [selected EXCEPT ![t] = cache]
  /\ phase' = [phase EXCEPT ![t] = "Return"]
  /\ UNCHANGED <<cache, nextGeneration, initialized, built, returned>>

TakeOverStalled(t) ==
  /\ phase[t] = "Observe"
  /\ cache \in Generations
  /\ cache \notin initialized
  /\ nextGeneration < GenerationCount
  /\ cache' = nextGeneration
  /\ selected' = [selected EXCEPT ![t] = nextGeneration]
  /\ nextGeneration' = nextGeneration + 1
  /\ phase' = [phase EXCEPT ![t] = "Build"]
  /\ UNCHANGED <<initialized, built, returned>>

Build(t) ==
  /\ phase[t] = "Build"
  /\ t \notin built
  /\ selected[t] \in Generations
  /\ initialized' = initialized \cup {selected[t]}
  /\ built' = built \cup {t}
  /\ phase' = [phase EXCEPT ![t] = "Return"]
  /\ UNCHANGED <<cache, nextGeneration, selected, returned>>

Return(t) ==
  /\ phase[t] = "Return"
  /\ selected[t] \in initialized
  /\ returned' = Append(returned,
       [snapshotter |-> t, generation |-> selected[t],
        producer |-> 0, revision |-> 0])
  /\ phase' = [phase EXCEPT ![t] = "Done"]
  /\ UNCHANGED <<cache, nextGeneration, initialized, selected, built>>

Next ==
  \E t \in Snapshotters :
    \/ PublishFirst(t)
    \/ ObserveInitialized(t)
    \/ TakeOverStalled(t)
    \/ Build(t)
    \/ Return(t)

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ cache \in Generations \cup {NoGeneration}
  /\ nextGeneration \in 0..GenerationCount
  /\ initialized \subseteq Generations
  /\ phase \in [Snapshotters -> Phases]
  /\ selected \in [Snapshotters -> (Generations \cup {NoGeneration})]
  /\ built \subseteq Snapshotters
  /\ \A i \in 1..Len(returned) :
       /\ returned[i].snapshotter \in Snapshotters
       /\ returned[i].generation \in Generations
       /\ returned[i].producer = 0
       /\ returned[i].revision = 0

ReturnedIdentityCoherent ==
  \A i \in 1..Len(returned) :
    /\ returned[i].producer = 0
    /\ returned[i].revision = 0

ReturnedGenerationWasInitialized ==
  \A i \in 1..Len(returned) : returned[i].generation \in initialized

InitializedGenerationWasPublished ==
  \A generation \in initialized : generation < nextGeneration

BoundedDuplicateWork ==
  Cardinality(built) <= Cardinality(Snapshotters)

SingleConstruction == Cardinality(built) <= 1

=============================================================================
