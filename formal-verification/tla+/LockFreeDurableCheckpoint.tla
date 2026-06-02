-------------------- MODULE LockFreeDurableCheckpoint --------------------
(***************************************************************************)
(* Durable checkpoint under LOCK-FREE writers (Path-2 unified MVCC core).    *)
(*                                                                         *)
(* Models the data-loss-critical part of the unified lock-free char ARTrie: *)
(* writers commit with Order A (WAL-append-and-sync, THEN CAS the immutable  *)
(* root → visible), with NO lock excluding the checkpoint. Lock-free CAS     *)
(* lets writes COMMIT OUT OF LSN ORDER (a writer holding LSN 6 may CAS before *)
(* the writer holding LSN 5). A checkpoint reads the atomic root → an         *)
(* immutable snapshot writers cannot mutate.                                 *)
(*                                                                         *)
(* The decisive question this spec answers: what `checkpoint_lsn` is safe?   *)
(*   - APPENDED frontier (max appended LSN): UNSAFE — a write appended before *)
(*     capture but committed after it has lsn ≤ checkpoint_lsn, so the WAL    *)
(*     rotate archives it AND it is not in the (pre-commit) snapshot → lost   *)
(*     on a clean reopen. This is the GAP_LEDGER #41 footgun reborn.          *)
(*   - COMMITTED WATERMARK (max L with {1..L} all committed at capture):      *)
(*     SAFE — proven here. Every visible term is either ≤ watermark (in the   *)
(*     snapshot) or > watermark (retained in WAL and replayed).               *)
(*                                                                         *)
(* Set USE_WATERMARK = TRUE to check the correct design (all invariants       *)
(* hold); FALSE to exhibit the appended-frontier data-loss bug (NoLostWrite   *)
(* is violated — TLC produces the losing trace).                             *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS Writers, Lsns, NoLsn, USE_WATERMARK

ASSUME Lsns = 1..Cardinality(Lsns)          \* Lsns is a contiguous 1..N
ASSUME NoLsn \notin Lsns
MaxL == Cardinality(Lsns)

VARIABLES
    nextLsn,        \* next LSN to assign (1..MaxL+1)
    wphase,         \* writer -> "Idle" | "Appended" | "Committed"
    wlsn,           \* writer -> the LSN it reserved (or NoLsn)
    appended,       \* set of LSNs whose WAL record is durable (Order A: durable before commit)
    committed,      \* set of LSNs that have been CAS-published (visible)
    ckptPhase,      \* "Idle" | "Captured" | "Published"
    ckptTarget,     \* the chosen checkpoint_lsn (watermark or appended frontier)
    ckptSnapshot,   \* set of LSNs captured in the immutable snapshot (visible & ≤ target at capture)
    durableCkpt,    \* set of LSNs durably checkpointed (the published snapshot)
    checkpointLsn,  \* durable checkpoint_lsn on disk (block-0)
    walRetainedFrom,\* WAL reclaim frontier: records with lsn ≥ this are retained
    recovered,      \* set of LSNs reconstructed by a crash+reopen
    recoveryFresh

Vars == <<nextLsn, wphase, wlsn, appended, committed, ckptPhase, ckptTarget,
          ckptSnapshot, durableCkpt, checkpointLsn, walRetainedFrom,
          recovered, recoveryFresh>>

MaxNat(a, b) == IF a >= b THEN a ELSE b

\* The committed watermark: the largest L such that every LSN in 1..L is committed.
\* (Contiguous all-committed prefix — robust to out-of-order CAS commits.) The
\* CHOOSE is total: exactly one n is the length of the committed prefix from 1.
Watermark ==
    CHOOSE n \in 0..MaxL :
        /\ \A m \in 1..n : m \in committed
        /\ (n = MaxL \/ (n + 1) \notin committed)

\* The appended frontier: the max appended LSN (the UNSAFE choice).
AppendedFrontier == IF appended = {} THEN 0 ELSE CHOOSE n \in appended : \A m \in appended : m <= n

TypeInvariant ==
    /\ nextLsn \in 1..(MaxL + 1)
    /\ wphase \in [Writers -> {"Idle", "Appended", "Committed"}]
    /\ wlsn \in [Writers -> Lsns \cup {NoLsn}]
    /\ appended \subseteq Lsns
    /\ committed \subseteq Lsns
    /\ committed \subseteq appended            \* Order A: visible ⊆ durable
    /\ ckptPhase \in {"Idle", "Captured", "Published"}
    /\ ckptTarget \in 0..MaxL
    /\ ckptSnapshot \subseteq Lsns
    /\ durableCkpt \subseteq Lsns
    /\ checkpointLsn \in 0..MaxL
    /\ walRetainedFrom \in 1..(MaxL + 1)
    /\ recovered \subseteq Lsns
    /\ recoveryFresh \in BOOLEAN

Init ==
    /\ nextLsn = 1
    /\ wphase = [w \in Writers |-> "Idle"]
    /\ wlsn = [w \in Writers |-> NoLsn]
    /\ appended = {}
    /\ committed = {}
    /\ ckptPhase = "Idle"
    /\ ckptTarget = 0
    /\ ckptSnapshot = {}
    /\ durableCkpt = {}
    /\ checkpointLsn = 0
    /\ walRetainedFrom = 1
    /\ recovered = {}
    /\ recoveryFresh = FALSE

\* Order A step 1: reserve an LSN and make its WAL record durable (append + sync).
\* Writers run with NO checkpoint gate.
Append(w) ==
    /\ wphase[w] = "Idle"
    /\ nextLsn <= MaxL
    /\ wphase' = [wphase EXCEPT ![w] = "Appended"]
    /\ wlsn' = [wlsn EXCEPT ![w] = nextLsn]
    /\ appended' = appended \cup {nextLsn}
    /\ nextLsn' = nextLsn + 1
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<committed, ckptPhase, ckptTarget, ckptSnapshot, durableCkpt,
                  checkpointLsn, walRetainedFrom, recovered>>

\* Order A step 2: CAS-publish the new root → the write becomes visible.
\* May happen in any order relative to other writers' commits (lock-free).
Commit(w) ==
    /\ wphase[w] = "Appended"
    /\ committed' = committed \cup {wlsn[w]}
    /\ wphase' = [wphase EXCEPT ![w] = "Committed"]
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, wlsn, appended, ckptPhase, ckptTarget, ckptSnapshot,
                  durableCkpt, checkpointLsn, walRetainedFrom, recovered>>

\* A committed writer returns to Idle (can do another op).
RetireWriter(w) ==
    /\ wphase[w] = "Committed"
    /\ wphase' = [wphase EXCEPT ![w] = "Idle"]
    /\ wlsn' = [wlsn EXCEPT ![w] = NoLsn]
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, appended, committed, ckptPhase, ckptTarget,
                  ckptSnapshot, durableCkpt, checkpointLsn, walRetainedFrom, recovered>>

\* Capture: read checkpoint_lsn (watermark or appended frontier) BEFORE the root
\* snapshot, then snapshot the immutable root (the committed set restricted to ≤ target).
\* NO writer-exclusion: writers continue throughout.
CaptureCheckpoint ==
    /\ ckptPhase = "Idle"
    /\ ckptTarget' = (IF USE_WATERMARK THEN Watermark ELSE AppendedFrontier)
    /\ ckptSnapshot' = {l \in committed : l <= ckptTarget'}
    /\ ckptPhase' = "Captured"
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, wphase, wlsn, appended, committed, durableCkpt,
                  checkpointLsn, walRetainedFrom, recovered>>

\* Publish: the snapshot becomes the durable checkpoint (block-0 fsync, F4).
PublishCheckpoint ==
    /\ ckptPhase = "Captured"
    /\ durableCkpt' = ckptSnapshot
    /\ checkpointLsn' = MaxNat(checkpointLsn, ckptTarget)
    /\ ckptPhase' = "Published"
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, wphase, wlsn, appended, committed, ckptTarget,
                  ckptSnapshot, walRetainedFrom, recovered>>

\* Reclaim: archive WAL records ≤ checkpoint_lsn, RETAIN > checkpoint_lsn.
ReclaimWal ==
    /\ ckptPhase = "Published"
    /\ walRetainedFrom' = MaxNat(walRetainedFrom, checkpointLsn + 1)
    /\ ckptPhase' = "Idle"
    /\ ckptTarget' = 0
    /\ ckptSnapshot' = {}
    /\ recoveryFresh' = FALSE
    /\ UNCHANGED <<nextLsn, wphase, wlsn, appended, committed, durableCkpt,
                  checkpointLsn, recovered>>

\* Crash + reopen: recovered = durable checkpoint ∪ {WAL records lsn > checkpoint_lsn
\* that are still retained (≥ walRetainedFrom) and durable (appended)}.
RecoveredSet ==
    durableCkpt \cup {l \in appended : l > checkpointLsn /\ l >= walRetainedFrom}

CrashRecover ==
    /\ recovered' = RecoveredSet
    /\ recoveryFresh' = TRUE
    /\ UNCHANGED <<nextLsn, wphase, wlsn, appended, committed, ckptPhase,
                  ckptTarget, ckptSnapshot, durableCkpt, checkpointLsn, walRetainedFrom>>

Next ==
    \/ \E w \in Writers : Append(w)
    \/ \E w \in Writers : Commit(w)
    \/ \E w \in Writers : RetireWriter(w)
    \/ CaptureCheckpoint
    \/ PublishCheckpoint
    \/ ReclaimWal
    \/ CrashRecover

Spec == Init /\ [][Next]_Vars

------------------------------------------------------------------------------
\* Invariants

\* Order A: every visible (committed) write is durable (its WAL record appended+synced).
DurablePrefix == committed \subseteq appended

\* The immutable snapshot cannot contain a write beyond the captured frontier.
ImmutableSnapshotIsClosed ==
    ckptPhase \in {"Captured", "Published"} => \A l \in ckptSnapshot : l <= ckptTarget

\* A durably-checkpointed write is within the published checkpoint_lsn.
CaptureEqualsPublishFrontier ==
    \A l \in durableCkpt : l <= checkpointLsn

\* THE headline: after a crash+reopen, every COMMITTED (acknowledged/visible) write
\* is reconstructed — no lock-free commit concurrent with a checkpoint is ever lost.
NoLostWriteUnderLockFreeCommit ==
    recoveryFresh => (committed \subseteq recovered)

\* Recovery never invents a write that was never durable.
RecoveredNeverInventsState ==
    recovered \subseteq appended

==============================================================================
