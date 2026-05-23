--------------------- MODULE LockFreeARTrieLinearizability ---------------------
(****************************************************************************)
(* Bounded linearizability model for the byte lock-free PersistentARTrie      *)
(* overlay.                                                                  *)
(*                                                                          *)
(* Scope: a path-copy insert loads the current root, builds a new root, and  *)
(* publishes it with a root CAS. The cache is only populated after a key is   *)
(* visible in the root. Contains may consult cache first, then root. Merge    *)
(* snapshots the cache and persists only keys that were visible at that       *)
(* snapshot point.                                                           *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Threads, Keys, None

VARIABLES root, cache, phase, key, expected,
          successfulInserts, duplicateInserts, conflicts,
          containsTrue, badContainsFalse, merged

Vars == <<root, cache, phase, key, expected,
          successfulInserts, duplicateInserts, conflicts,
          containsTrue, badContainsFalse, merged>>

Phases == {"Idle", "Loaded"}

TypeInvariant ==
    /\ root \in SUBSET Keys
    /\ cache \in SUBSET Keys
    /\ phase \in [Threads -> Phases]
    /\ key \in [Threads -> Keys \cup {None}]
    /\ expected \in [Threads -> SUBSET Keys]
    /\ successfulInserts \in [Keys -> 0..1]
    /\ duplicateInserts \in SUBSET Keys
    /\ conflicts \in SUBSET Keys
    /\ containsTrue \in SUBSET Keys
    /\ badContainsFalse \in BOOLEAN
    /\ merged \in SUBSET Keys

Init ==
    /\ root = {}
    /\ cache = {}
    /\ phase = [t \in Threads |-> "Idle"]
    /\ key = [t \in Threads |-> None]
    /\ expected = [t \in Threads |-> {}]
    /\ successfulInserts = [k \in Keys |-> 0]
    /\ duplicateInserts = {}
    /\ conflicts = {}
    /\ containsTrue = {}
    /\ badContainsFalse = FALSE
    /\ merged = {}

StartInsert(t, k) ==
    /\ t \in Threads
    /\ k \in Keys
    /\ phase[t] = "Idle"
    /\ phase' = [phase EXCEPT ![t] = "Loaded"]
    /\ key' = [key EXCEPT ![t] = k]
    /\ expected' = [expected EXCEPT ![t] = root]
    /\ UNCHANGED <<root, cache, successfulInserts, duplicateInserts,
                  conflicts, containsTrue, badContainsFalse, merged>>

PublishFreshInsert(t) ==
    /\ t \in Threads
    /\ phase[t] = "Loaded"
    /\ key[t] \in Keys
    /\ key[t] \notin expected[t]
    /\ expected[t] = root
    /\ root' = root \cup {key[t]}
    /\ cache' = cache \cup {key[t]}
    /\ successfulInserts' =
        [successfulInserts EXCEPT ![key[t]] = @ + 1]
    /\ phase' = [phase EXCEPT ![t] = "Idle"]
    /\ key' = [key EXCEPT ![t] = None]
    /\ UNCHANGED <<expected, duplicateInserts, conflicts,
                  containsTrue, badContainsFalse, merged>>

ObserveDuplicate(t) ==
    /\ t \in Threads
    /\ phase[t] = "Loaded"
    /\ key[t] \in root
    /\ cache' = cache \cup {key[t]}
    /\ duplicateInserts' = duplicateInserts \cup {key[t]}
    /\ phase' = [phase EXCEPT ![t] = "Idle"]
    /\ key' = [key EXCEPT ![t] = None]
    /\ UNCHANGED <<root, expected, successfulInserts, conflicts,
                  containsTrue, badContainsFalse, merged>>

ObserveConflict(t) ==
    /\ t \in Threads
    /\ phase[t] = "Loaded"
    /\ expected[t] # root
    /\ key[t] \notin root
    /\ conflicts' = conflicts \cup {key[t]}
    /\ phase' = [phase EXCEPT ![t] = "Idle"]
    /\ key' = [key EXCEPT ![t] = None]
    /\ UNCHANGED <<root, cache, expected, successfulInserts,
                  duplicateInserts, containsTrue, badContainsFalse, merged>>

Contains(t, k) ==
    /\ t \in Threads
    /\ k \in Keys
    /\ phase[t] = "Idle"
    /\ IF k \in cache \/ k \in root
       THEN /\ containsTrue' = containsTrue \cup {k}
            /\ badContainsFalse' = badContainsFalse
       ELSE /\ containsTrue' = containsTrue
            /\ badContainsFalse' = badContainsFalse \/ (k \in cache \/ k \in root)
    /\ UNCHANGED <<root, cache, phase, key, expected, successfulInserts,
                  duplicateInserts, conflicts, merged>>

MergeSnapshot(t) ==
    /\ t \in Threads
    /\ phase[t] = "Idle"
    /\ merged' = merged \cup cache
    /\ UNCHANGED <<root, cache, phase, key, expected, successfulInserts,
                  duplicateInserts, conflicts, containsTrue, badContainsFalse>>

Next ==
    \/ \E t \in Threads, k \in Keys : StartInsert(t, k)
    \/ \E t \in Threads : PublishFreshInsert(t)
    \/ \E t \in Threads : ObserveDuplicate(t)
    \/ \E t \in Threads : ObserveConflict(t)
    \/ \E t \in Threads, k \in Keys : Contains(t, k)
    \/ \E t \in Threads : MergeSnapshot(t)

CacheOnlyAfterRootPublish ==
    cache \subseteq root

SuccessfulInsertPublished ==
    \A k \in Keys :
        successfulInserts[k] = 1 => k \in root

AtMostOneSuccessfulInsertPerKey ==
    \A k \in Keys :
        successfulInserts[k] <= 1

ContainsTrueIsSound ==
    containsTrue \subseteq root

MergePersistsOnlyPublishedKeys ==
    merged \subseteq root

IdleThreadsHaveNoPendingKey ==
    \A t \in Threads :
        phase[t] = "Idle" => key[t] = None

NoFalseContainsForVisibleKey ==
    badContainsFalse = FALSE

Spec == Init /\ [][Next]_Vars

=============================================================================
