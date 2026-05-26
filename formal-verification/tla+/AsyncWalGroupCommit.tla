------------------------- MODULE AsyncWalGroupCommit -------------------------
(****************************************************************************)
(* Bounded ordered async WAL and group-commit model.                        *)
(*                                                                          *)
(* Scope: public writers reserve monotonically increasing LSNs, enqueue     *)
(* those records in the same FIFO order, and group commit flushes a         *)
(* non-empty prefix of that queue. Acknowledgements are published only for  *)
(* records covered by the synced WAL prefix.                                *)
(****************************************************************************)

EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS Threads, MaxLSN

VARIABLES nextLsn, queue, durable, syncedLsn, waiters, acked

Vars == <<nextLsn, queue, durable, syncedLsn, waiters, acked>>

LsnRange == 1..MaxLSN

Prefix(n) == {lsn \in LsnRange : lsn <= n}

QueueSet == {queue[i] : i \in DOMAIN queue}

UnionWaiters == UNION {waiters[t] : t \in Threads}

NoDupSeq(seq) ==
    \A i, j \in DOMAIN seq :
        i # j => seq[i] # seq[j]

StrictlyIncreasing(seq) ==
    \A i, j \in DOMAIN seq :
        i < j => seq[i] < seq[j]

CommittedPrefix(k) ==
    {queue[i] : i \in 1..k}

RemainingAfter(k) ==
    IF k = Len(queue)
    THEN <<>>
    ELSE SubSeq(queue, k + 1, Len(queue))

TypeInvariant ==
    /\ nextLsn \in 1..(MaxLSN + 1)
    /\ syncedLsn \in 0..MaxLSN
    /\ syncedLsn < nextLsn
    /\ queue \in Seq(LsnRange)
    /\ NoDupSeq(queue)
    /\ durable \in SUBSET LsnRange
    /\ waiters \in [Threads -> SUBSET LsnRange]
    /\ acked \in SUBSET LsnRange

Init ==
    /\ nextLsn = 1
    /\ queue = <<>>
    /\ durable = {}
    /\ syncedLsn = 0
    /\ waiters = [t \in Threads |-> {}]
    /\ acked = {}

Submit(t) ==
    /\ t \in Threads
    /\ nextLsn <= MaxLSN
    /\ queue' = Append(queue, nextLsn)
    /\ waiters' = [waiters EXCEPT ![t] = @ \cup {nextLsn}]
    /\ nextLsn' = nextLsn + 1
    /\ UNCHANGED <<durable, syncedLsn, acked>>

FlushPrefix(k) ==
    /\ k \in DOMAIN queue
    /\ LET committed == CommittedPrefix(k) IN
        /\ durable' = durable \cup committed
        /\ syncedLsn' = queue[k]
        /\ queue' = RemainingAfter(k)
        /\ waiters' = [t \in Threads |-> waiters[t] \ committed]
        /\ acked' = acked \cup committed
    /\ UNCHANGED nextLsn

Next ==
    \/ \E t \in Threads : Submit(t)
    \/ \E k \in DOMAIN queue : FlushPrefix(k)

DurablePrefixExact ==
    durable = Prefix(syncedLsn)

DurablePrefixClosed ==
    \A lsn \in LsnRange :
        lsn <= syncedLsn => lsn \in durable

QueueIsLsnOrdered ==
    StrictlyIncreasing(queue)

QueueStartsAfterSynced ==
    Len(queue) = 0 \/ Head(queue) = syncedLsn + 1

QueueIsContiguousAfterSynced ==
    /\ Len(queue) = nextLsn - syncedLsn - 1
    /\ \A i \in DOMAIN queue :
        queue[i] = syncedLsn + i

QueuedRecordsAreSubmittedNotDurable ==
    /\ QueueSet \subseteq Prefix(nextLsn - 1)
    /\ QueueSet \cap durable = {}

WaitersReferenceQueuedRecords ==
    UnionWaiters = QueueSet

AckedRecordsAreDurable ==
    acked \subseteq durable

NoEarlyAcknowledgement ==
    acked \subseteq Prefix(syncedLsn)

Spec == Init /\ [][Next]_Vars

=============================================================================
