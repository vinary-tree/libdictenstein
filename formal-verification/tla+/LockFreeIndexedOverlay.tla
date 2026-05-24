----------------------- MODULE LockFreeIndexedOverlay -----------------------
(****************************************************************************)
(* Bounded model for the value/index carrying lock-free overlays.            *)
(*                                                                          *)
(* Counter mode mirrors the char `increment_cas` path: a successful          *)
(* increment creates or finds the leaf, publishes cache visibility, and       *)
(* atomically adds the requested delta exactly once. Merge snapshots may lag  *)
(* concurrent increments but may not invent values.                           *)
(*                                                                          *)
(* Vocabulary mode mirrors the vocab `insert_cas` path: an operation first    *)
(* checks visible state, claims `next_index` with fetch_add, then publishes   *)
(* the claimed index with root CAS. A duplicate race may waste a claimed      *)
(* index, so density is not part of the contract; uniqueness and stability    *)
(* of committed term -> index bindings are.                                  *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Threads, Keys, None, MaxIndex, MaxCounter, Deltas, ModelKind

VARIABLES counter, counterVisible, counterCache, counterObserved,
          persistedCounter, incrementSum,
          vocabRoot, vocabCache, vocabPersistent, vocabPhase, vocabTerm,
          vocabExpected, vocabClaim, nextIndex, entryCount, claimedWaste,
          conflicts

CounterVars == <<counter, counterVisible, counterCache, counterObserved,
                 persistedCounter, incrementSum>>

VocabVars == <<vocabRoot, vocabCache, vocabPersistent, vocabPhase, vocabTerm,
               vocabExpected, vocabClaim, nextIndex, entryCount, claimedWaste,
               conflicts>>

Vars == <<counter, counterVisible, counterCache, counterObserved,
          persistedCounter, incrementSum,
          vocabRoot, vocabCache, vocabPersistent, vocabPhase, vocabTerm,
          vocabExpected, vocabClaim, nextIndex, entryCount, claimedWaste,
          conflicts>>

Indexes == 0..MaxIndex
IndexOrNone == Indexes \cup {None}
Phases == {"Idle", "Claimed"}
EmptyIndexMap == [k \in Keys |-> None]

IndexImage(f) == {f[k] : k \in Keys} \ {None}
VocabDomain(f) == {k \in Keys : f[k] # None}
ActiveThreads == {t \in Threads : vocabPhase[t] = "Claimed"}
ActiveClaimSet == {vocabClaim[t] : t \in Threads} \ {None}
AllUsedIndexes == IndexImage(vocabRoot) \cup claimedWaste \cup ActiveClaimSet

MaxNat(a, b) == IF a >= b THEN a ELSE b

TypeInvariant ==
    /\ ModelKind \in {"Counter", "Vocabulary"}
    /\ None \notin Keys
    /\ None \notin Indexes
    /\ counter \in [Keys -> 0..MaxCounter]
    /\ counterVisible \in SUBSET Keys
    /\ counterCache \in SUBSET Keys
    /\ counterObserved \in [Keys -> 0..MaxCounter]
    /\ persistedCounter \in [Keys -> 0..MaxCounter]
    /\ incrementSum \in [Keys -> 0..MaxCounter]
    /\ vocabRoot \in [Keys -> IndexOrNone]
    /\ vocabCache \in [Keys -> IndexOrNone]
    /\ vocabPersistent \in [Keys -> IndexOrNone]
    /\ vocabPhase \in [Threads -> Phases]
    /\ vocabTerm \in [Threads -> Keys \cup {None}]
    /\ vocabExpected \in [Threads -> [Keys -> IndexOrNone]]
    /\ vocabClaim \in [Threads -> IndexOrNone]
    /\ nextIndex \in 0..(MaxIndex + 1)
    /\ entryCount \in 0..Cardinality(Keys)
    /\ claimedWaste \in SUBSET Indexes
    /\ conflicts \in SUBSET Keys

Init ==
    /\ counter = [k \in Keys |-> 0]
    /\ counterVisible = {}
    /\ counterCache = {}
    /\ counterObserved = [k \in Keys |-> 0]
    /\ persistedCounter = [k \in Keys |-> 0]
    /\ incrementSum = [k \in Keys |-> 0]
    /\ vocabRoot = EmptyIndexMap
    /\ vocabCache = EmptyIndexMap
    /\ vocabPersistent = EmptyIndexMap
    /\ vocabPhase = [t \in Threads |-> "Idle"]
    /\ vocabTerm = [t \in Threads |-> None]
    /\ vocabExpected = [t \in Threads |-> EmptyIndexMap]
    /\ vocabClaim = [t \in Threads |-> None]
    /\ nextIndex = 0
    /\ entryCount = 0
    /\ claimedWaste = {}
    /\ conflicts = {}

CounterIncrement(t, k, d) ==
    /\ t \in Threads
    /\ k \in Keys
    /\ d \in Deltas
    /\ counter[k] + d <= MaxCounter
    /\ counter' = [counter EXCEPT ![k] = @ + d]
    /\ incrementSum' = [incrementSum EXCEPT ![k] = @ + d]
    /\ counterVisible' = counterVisible \cup {k}
    /\ counterCache' = counterCache \cup {k}
    /\ UNCHANGED <<counterObserved, persistedCounter>>
    /\ UNCHANGED VocabVars

CounterGet(t, k) ==
    /\ t \in Threads
    /\ k \in Keys
    /\ IF k \in counterVisible
       THEN counterObserved' = [counterObserved EXCEPT ![k] = counter[k]]
       ELSE counterObserved' = counterObserved
    /\ UNCHANGED <<counter, counterVisible, counterCache, persistedCounter,
                  incrementSum>>
    /\ UNCHANGED VocabVars

CounterMergeSnapshot(t) ==
    /\ t \in Threads
    /\ persistedCounter' =
        [k \in Keys |->
            IF k \in counterVisible
            THEN MaxNat(persistedCounter[k], counter[k])
            ELSE persistedCounter[k]]
    /\ UNCHANGED <<counter, counterVisible, counterCache, counterObserved,
                  incrementSum>>
    /\ UNCHANGED VocabVars

CounterNext ==
    \/ \E t \in Threads, k \in Keys, d \in Deltas :
        CounterIncrement(t, k, d)
    \/ \E t \in Threads, k \in Keys : CounterGet(t, k)
    \/ \E t \in Threads : CounterMergeSnapshot(t)

VocabReturnExisting(t, k) ==
    /\ t \in Threads
    /\ k \in Keys
    /\ vocabPhase[t] = "Idle"
    /\ vocabRoot[k] # None
    /\ vocabCache' = [vocabCache EXCEPT ![k] = vocabRoot[k]]
    /\ UNCHANGED <<vocabRoot, vocabPersistent, vocabPhase, vocabTerm,
                  vocabExpected, vocabClaim, nextIndex, entryCount,
                  claimedWaste, conflicts>>
    /\ UNCHANGED CounterVars

VocabClaim(t, k) ==
    /\ t \in Threads
    /\ k \in Keys
    /\ vocabPhase[t] = "Idle"
    /\ vocabRoot[k] = None
    /\ vocabCache[k] = None
    /\ nextIndex <= MaxIndex
    /\ vocabPhase' = [vocabPhase EXCEPT ![t] = "Claimed"]
    /\ vocabTerm' = [vocabTerm EXCEPT ![t] = k]
    /\ vocabExpected' = [vocabExpected EXCEPT ![t] = vocabRoot]
    /\ vocabClaim' = [vocabClaim EXCEPT ![t] = nextIndex]
    /\ nextIndex' = nextIndex + 1
    /\ UNCHANGED <<vocabRoot, vocabCache, vocabPersistent, entryCount,
                  claimedWaste, conflicts>>
    /\ UNCHANGED CounterVars

VocabPublish(t) ==
    /\ t \in Threads
    /\ vocabPhase[t] = "Claimed"
    /\ vocabTerm[t] \in Keys
    /\ vocabClaim[t] \in Indexes
    /\ vocabExpected[t] = vocabRoot
    /\ vocabRoot[vocabTerm[t]] = None
    /\ vocabRoot' = [vocabRoot EXCEPT ![vocabTerm[t]] = vocabClaim[t]]
    /\ vocabCache' = [vocabCache EXCEPT ![vocabTerm[t]] = vocabClaim[t]]
    /\ entryCount' = entryCount + 1
    /\ vocabPhase' = [vocabPhase EXCEPT ![t] = "Idle"]
    /\ vocabTerm' = [vocabTerm EXCEPT ![t] = None]
    /\ vocabExpected' = [vocabExpected EXCEPT ![t] = EmptyIndexMap]
    /\ vocabClaim' = [vocabClaim EXCEPT ![t] = None]
    /\ UNCHANGED <<vocabPersistent, nextIndex, claimedWaste, conflicts>>
    /\ UNCHANGED CounterVars

VocabObserveDuplicate(t) ==
    /\ t \in Threads
    /\ vocabPhase[t] = "Claimed"
    /\ vocabTerm[t] \in Keys
    /\ vocabClaim[t] \in Indexes
    /\ vocabRoot[vocabTerm[t]] # None
    /\ vocabCache' = [vocabCache EXCEPT ![vocabTerm[t]] = vocabRoot[vocabTerm[t]]]
    /\ claimedWaste' = claimedWaste \cup {vocabClaim[t]}
    /\ vocabPhase' = [vocabPhase EXCEPT ![t] = "Idle"]
    /\ vocabTerm' = [vocabTerm EXCEPT ![t] = None]
    /\ vocabExpected' = [vocabExpected EXCEPT ![t] = EmptyIndexMap]
    /\ vocabClaim' = [vocabClaim EXCEPT ![t] = None]
    /\ UNCHANGED <<vocabRoot, vocabPersistent, nextIndex, entryCount,
                  conflicts>>
    /\ UNCHANGED CounterVars

VocabObserveConflict(t) ==
    /\ t \in Threads
    /\ vocabPhase[t] = "Claimed"
    /\ vocabTerm[t] \in Keys
    /\ vocabExpected[t] # vocabRoot
    /\ vocabRoot[vocabTerm[t]] = None
    /\ vocabExpected' = [vocabExpected EXCEPT ![t] = vocabRoot]
    /\ conflicts' = conflicts \cup {vocabTerm[t]}
    /\ UNCHANGED <<vocabRoot, vocabCache, vocabPersistent, vocabPhase,
                  vocabTerm, vocabClaim, nextIndex, entryCount, claimedWaste>>
    /\ UNCHANGED CounterVars

VocabMergeSnapshot(t) ==
    /\ t \in Threads
    /\ vocabPersistent' =
        [k \in Keys |->
            IF vocabCache[k] # None
            THEN vocabCache[k]
            ELSE vocabPersistent[k]]
    /\ UNCHANGED <<vocabRoot, vocabCache, vocabPhase, vocabTerm,
                  vocabExpected, vocabClaim, nextIndex, entryCount,
                  claimedWaste, conflicts>>
    /\ UNCHANGED CounterVars

VocabNext ==
    \/ \E t \in Threads, k \in Keys : VocabReturnExisting(t, k)
    \/ \E t \in Threads, k \in Keys : VocabClaim(t, k)
    \/ \E t \in Threads : VocabPublish(t)
    \/ \E t \in Threads : VocabObserveDuplicate(t)
    \/ \E t \in Threads : VocabObserveConflict(t)
    \/ \E t \in Threads : VocabMergeSnapshot(t)

Next ==
    \/ /\ ModelKind = "Counter"
       /\ CounterNext
    \/ /\ ModelKind = "Vocabulary"
       /\ VocabNext

CounterValuesEqualSuccessfulDeltas ==
    ModelKind # "Counter" \/
    \A k \in Keys : counter[k] = incrementSum[k]

CounterCacheOnlyAfterVisible ==
    ModelKind # "Counter" \/ counterCache \subseteq counterVisible

CounterObservedIsSound ==
    ModelKind # "Counter" \/
    \A k \in Keys :
        /\ counterObserved[k] <= counter[k]
        /\ counterObserved[k] > 0 => k \in counterVisible

CounterMergeIsPrefix ==
    ModelKind # "Counter" \/
    \A k \in Keys : persistedCounter[k] <= counter[k]

VocabCacheAgreesWithRoot ==
    ModelKind # "Vocabulary" \/
    \A k \in Keys :
        vocabCache[k] # None => vocabRoot[k] = vocabCache[k]

VocabPersistentAgreesWithRoot ==
    ModelKind # "Vocabulary" \/
    \A k \in Keys :
        vocabPersistent[k] # None => vocabRoot[k] = vocabPersistent[k]

VocabUniqueCommittedIndices ==
    ModelKind # "Vocabulary" \/
    \A k1 \in Keys, k2 \in Keys :
        /\ vocabRoot[k1] # None
        /\ vocabRoot[k2] # None
        /\ vocabRoot[k1] = vocabRoot[k2]
        => k1 = k2

VocabEntryCountMatchesRoot ==
    ModelKind # "Vocabulary" \/
    entryCount = Cardinality(VocabDomain(vocabRoot))

VocabCommittedIndicesWereClaimed ==
    ModelKind # "Vocabulary" \/
    \A k \in Keys :
        vocabRoot[k] # None => vocabRoot[k] < nextIndex

VocabActiveClaimsAreUnique ==
    ModelKind # "Vocabulary" \/
    Cardinality(ActiveClaimSet) = Cardinality(ActiveThreads)

VocabNoIndexReuseAcrossCommittedWastedOrActive ==
    ModelKind # "Vocabulary" \/
    Cardinality(AllUsedIndexes) =
        Cardinality(IndexImage(vocabRoot)) +
        Cardinality(claimedWaste) +
        Cardinality(ActiveClaimSet)

VocabNextIndexAccountsForSparseClaims ==
    ModelKind # "Vocabulary" \/
    Cardinality(AllUsedIndexes) = nextIndex

VocabIdleThreadsHaveNoClaim ==
    ModelKind # "Vocabulary" \/
    \A t \in Threads :
        vocabPhase[t] = "Idle" =>
            /\ vocabTerm[t] = None
            /\ vocabClaim[t] = None

Spec == Init /\ [][Next]_Vars

=============================================================================
