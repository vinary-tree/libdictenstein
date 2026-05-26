------------------- MODULE LockFreeCounterMergeAtomicity -------------------
(****************************************************************************)
(* Bounded model for checked lock-free counter increments and atomic merge.   *)
(*                                                                          *)
(* The Rust overlay stores nonnegative u64 counters, but the persistent WAL   *)
(* represents merge deltas as i64.  The modeled counter domain is therefore   *)
(* a bounded natural range. Checked increments and checked merge preflight     *)
(* reject overflow without changing the overlay, persistent map, or WAL.      *)
(****************************************************************************)

EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS Threads, Keys, MaxCounter, Deltas

ASSUME MaxCounter \in Nat \ {0}
ASSUME Deltas \subseteq Nat
ASSUME Keys # {}
ASSUME Threads # {}

VARIABLES overlay, visible, persistent, wal, incrementFailureSeen, mergeFailureSeen

Vars == <<overlay, visible, persistent, wal, incrementFailureSeen, mergeFailureSeen>>

CounterMaps == [Keys -> 0..MaxCounter]
Zero == [k \in Keys |-> 0]

Snapshot ==
    [k \in Keys |-> IF k \in visible THEN overlay[k] ELSE 0]

SnapshotFits(map, snapshot) ==
    \A k \in Keys : map[k] + snapshot[k] <= MaxCounter

ApplySnapshot(map, snapshot) ==
    [k \in Keys |-> map[k] + snapshot[k]]

RECURSIVE FoldWal(_)

FoldWal(records) ==
    IF records = <<>>
    THEN Zero
    ELSE ApplySnapshot(
        FoldWal(SubSeq(records, 1, Len(records) - 1)),
        records[Len(records)])

Init ==
    /\ overlay = Zero
    /\ visible = {}
    /\ persistent = Zero
    /\ wal = <<>>
    /\ incrementFailureSeen = FALSE
    /\ mergeFailureSeen = FALSE

IncrementOk(t, k, d) ==
    /\ t \in Threads
    /\ k \in Keys
    /\ d \in Deltas
    /\ overlay[k] + d <= MaxCounter
    /\ overlay' = [overlay EXCEPT ![k] = @ + d]
    /\ visible' = visible \cup {k}
    /\ UNCHANGED <<persistent, wal, incrementFailureSeen, mergeFailureSeen>>

IncrementOverflow(t, k, d) ==
    /\ t \in Threads
    /\ k \in Keys
    /\ d \in Deltas
    /\ overlay[k] + d > MaxCounter
    /\ incrementFailureSeen' = TRUE
    /\ UNCHANGED <<overlay, visible, persistent, wal, mergeFailureSeen>>

MergeOk(t) ==
    /\ t \in Threads
    /\ visible # {}
    /\ SnapshotFits(persistent, Snapshot)
    /\ persistent' = ApplySnapshot(persistent, Snapshot)
    /\ wal' = Append(wal, Snapshot)
    /\ overlay' = Zero
    /\ visible' = {}
    /\ UNCHANGED <<incrementFailureSeen, mergeFailureSeen>>

MergeOverflow(t) ==
    /\ t \in Threads
    /\ visible # {}
    /\ ~SnapshotFits(persistent, Snapshot)
    /\ mergeFailureSeen' = TRUE
    /\ UNCHANGED <<overlay, visible, persistent, wal, incrementFailureSeen>>

Next ==
    \/ \E t \in Threads, k \in Keys, d \in Deltas : IncrementOk(t, k, d)
    \/ \E t \in Threads, k \in Keys, d \in Deltas : IncrementOverflow(t, k, d)
    \/ \E t \in Threads : MergeOk(t)
    \/ \E t \in Threads : MergeOverflow(t)

TypeInvariant ==
    /\ overlay \in CounterMaps
    /\ visible \subseteq Keys
    /\ persistent \in CounterMaps
    /\ wal \in Seq(CounterMaps)
    /\ incrementFailureSeen \in BOOLEAN
    /\ mergeFailureSeen \in BOOLEAN

OverlayOnlyVisibleKeysHaveValues ==
    \A k \in Keys : k \notin visible => overlay[k] = 0

PersistentEqualsWalReplay ==
    persistent = FoldWal(wal)

WalRecordsWerePreflighted ==
    \A i \in 1..Len(wal) :
        SnapshotFits(FoldWal(SubSeq(wal, 1, i - 1)), wal[i])

FailureFlagsAreBoolean ==
    /\ incrementFailureSeen \in BOOLEAN
    /\ mergeFailureSeen \in BOOLEAN

Spec == Init /\ [][Next]_Vars

=============================================================================
