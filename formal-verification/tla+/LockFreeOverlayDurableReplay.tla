------------------------ MODULE LockFreeOverlayDurableReplay ------------------------
(***************************************************************************)
(* ORDER-A DURABLE REPLAY-ORDER FIX (design "C′",                            *)
(* docs/design/order-a-replay-order-fix.md). Machine-checks that recovering  *)
(* the lock-free char-ARTrie overlay from the WAL reconstructs the EXACT      *)
(* committed-visible membership once a per-term commit generation is recorded *)
(* durably (`WalRecord::CommitRank`).                                         *)
(*                                                                         *)
(* THE BUG (Order-A, §1). A state-changing op writes in TWO UNLINKED steps:   *)
(*   step 1  Append the data record (`Insert`/`Remove`) — assigns an LSN in   *)
(*           append order `≺_LSN` (`next_lsn.fetch_add`).                     *)
(*   step 2  Win the visibility CAS — commits in CAS order `≺_CAS`.           *)
(* Nothing forces `≺_LSN == ≺_CAS`. Recovery replays in LSN/physical order,   *)
(* so a naive "highest-LSN per term" reconcile can pick the WRONG last writer:*)
(*   s019: WAL `Insert@lsnI, Remove@lsnR` with lsnI < lsnR, but the CAS       *)
(*   last-writer is the Insert (it committed LAST) ⇒ committed-visible is     *)
(*   PRESENT, yet lsn-order replay ends with the Remove ⇒ ABSENT = an         *)
(*   acknowledged net-present key is LOST after reopen (data loss).           *)
(*                                                                         *)
(* THE FIX (C′, §3). On the WINNING CAS read the published leaf's node        *)
(* `version` as a per-term commit GENERATION (monotone in `≺_CAS` per term —  *)
(* §3.6) and, BEFORE acking, append+sync `CommitRank{data_lsn, term,          *)
(* generation}`. Recovery reconciles per-term by MAX generation              *)
(* (`generation_of(lsn) = rank[lsn] otherwise lsn`, ties by lsn), so replay   *)
(* order == CAS/visibility order == committed-visible membership.            *)
(*                                                                         *)
(* `USE_COMMIT_RANK = TRUE`  -> the design: recovery orders by generation.    *)
(*   ALL invariants hold (`ReplayEqualsCommittedVisible` headline).          *)
(* `USE_COMMIT_RANK = FALSE` -> the `_Unsafe.cfg` NEGATIVE CONTROL: recovery  *)
(*   orders by LSN (the broken pre-fix scheme). The s019 interleaving makes   *)
(*   `replayed # present` ⇒ TLC MUST report a `ReplayEqualsCommittedVisible`  *)
(*   violation. If TLC unexpectedly PASSES, the control is broken ⇒ fail the  *)
(*   whole gate.                                                              *)
(*                                                                         *)
(* COMPOSES WITH (does not replace) `LockFreeOverlayRemoveCas` (the           *)
(* visibility-CAS last-writer-wins of {insert, remove}) and                  *)
(* `LockFreeDurableCheckpoint` (the committed-watermark `DurablePrefix`). This *)
(* spec adds the missing link those two never modelled: collapsing two LSNs   *)
(* of ONE term through a crash + replay.                                      *)
(***************************************************************************)

EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    Terms,           \* finite set of terms that may be inserted/removed
    USE_COMMIT_RANK, \* TRUE = design (replay by generation); FALSE = unsafe (by lsn)
    MaxOps           \* TLC finiteness cap on the number of committed ops

ASSUME Terms # {}

(***************************************************************************)
(* A committed op record. `kind` is the data record's effect, `lsn` its       *)
(* append-order LSN, `gen` the per-term commit generation assigned at its CAS. *)
(* `rankLsn` is the LSN of the CommitRank marker bound to it (or 0 if the rank *)
(* has not been appended yet — the IoError window §3.5).                      *)
(***************************************************************************)
Kinds == {"Insert", "Remove"}

VARIABLES
    committedOps, \* set of records [t, kind, lsn, gen, rankLsn] whose CAS has won
    present,      \* set of terms currently published-present (visible LWW)
    removed,      \* set of terms currently published-absent (the complement)
    committedLsns,\* set of LSNs marked committed (data AND rank) — DurablePrefix domain
    nextLsn,      \* next LSN to assign (append order)
    nextGen,      \* GLOBAL next commit generation (a Nat, bumped per winning CAS).
                  \* This faithfully abstracts the IMPLEMENTATION's generation
                  \* source: the PUBLISHED-ROOT node `version`, which the spine
                  \* path-copy bumps by +1 on EVERY publication (insert AND remove),
                  \* fixed at the root CAS — so it is a single global counter that
                  \* strictly increases in root-CAS linearization order. (The leaf
                  \* `version` would NOT work: the insert finalize is an in-place
                  \* `try_set_final` that does not bump it, so an insert
                  \* re-finalizing a leaf a remove cleared would TIE the remove's
                  \* generation. The root version avoids that.) A global counter is
                  \* STRICTLY STRONGER than a per-term counter: it is monotone per
                  \* term too, which is all `ReplayEqualsCommittedVisible` needs.
    replayed,     \* set of terms reconstructed PRESENT by the last CrashRecover
    recovered     \* TRUE once a CrashRecover has run (replayed is meaningful)

Vars ==
    <<committedOps, present, removed, committedLsns, nextLsn, nextGen, replayed, recovered>>

(***************************************************************************)
(* Pending data appends (step 1 done, CAS step 2 not yet). Modelled as a      *)
(* separate set so an Append can sit durable in the WAL before its CAS wins —  *)
(* this is the Order-A window the bug lives in.                               *)
(***************************************************************************)
VARIABLE pendingAppends   \* set of records [t, kind, lsn]

AllVars == <<Vars, pendingAppends>>

RecordType ==
    [t: Terms, kind: Kinds, lsn: Nat, gen: Nat, rankLsn: Nat]

PendingType ==
    [t: Terms, kind: Kinds, lsn: Nat]

TypeInvariant ==
    /\ committedOps \subseteq RecordType
    /\ pendingAppends \subseteq PendingType
    /\ present \subseteq Terms
    /\ removed \subseteq Terms
    /\ committedLsns \subseteq Nat
    /\ nextLsn \in Nat
    /\ nextGen \in Nat
    /\ replayed \subseteq Terms
    /\ recovered \in BOOLEAN

(***************************************************************************)
(* Init: empty overlay. Every term is absent (`removed = Terms`) so the        *)
(* present/removed complement and ReplayEqualsCommittedVisible are TOTAL       *)
(* biconditionals from the start (mirrors LockFreeOverlayRemoveCas's Init).    *)
(* `replayed = {}` and `recovered = FALSE` (no crash yet).                    *)
(***************************************************************************)
Init ==
    /\ committedOps = {}
    /\ pendingAppends = {}
    /\ present = {}
    /\ removed = Terms
    /\ committedLsns = {}
    /\ nextLsn = 1
    /\ nextGen = 0
    /\ replayed = {}
    /\ recovered = FALSE

------------------------------------------------------------------------------
(* Cardinality of all ops that have been *appended* (pending + committed) —    *)
(* the TLC finiteness bound.                                                   *)
OpCount == Cardinality(pendingAppends) + Cardinality(committedOps)

(***************************************************************************)
(* Append(t, kind) — Order-A STEP 1. The data record is written durable        *)
(* (LSN = nextLsn), but its visibility CAS has NOT happened, so `present` /     *)
(* `removed` are UNCHANGED. The LSN is marked committed (the data record is     *)
(* durable per policy). This is the window where `≺_LSN` is fixed BEFORE        *)
(* `≺_CAS`, which is exactly how the two orders can disagree.                  *)
(***************************************************************************)
AppendData(t, kind) ==
    /\ OpCount < MaxOps
    /\ pendingAppends' = pendingAppends \cup
           {[t |-> t, kind |-> kind, lsn |-> nextLsn]}
    /\ committedLsns' = committedLsns \cup {nextLsn}
    /\ nextLsn' = nextLsn + 1
    \* Any new activity invalidates a prior recovery observation: `replayed`
    \* reflects the durable state AS OF the last CrashRecover, so the
    \* post-recovery invariants must only constrain the state immediately after a
    \* CrashRecover (when `replayed` is current). A fresh op begins a new pre-crash
    \* run ⇒ clear `recovered` (and `replayed`).
    /\ recovered' = FALSE
    /\ replayed' = {}
    /\ UNCHANGED <<committedOps, present, removed, nextGen>>

(***************************************************************************)
(* CommitAndRank(rec) — Order-A STEPS 2 + 2.5, as the atomic commit point a     *)
(* SINGLE op performs before it acks. A pending data record's visibility CAS     *)
(* WINS, so it:                                                                 *)
(*   (step 2)   is assigned the next GLOBAL commit GENERATION                    *)
(*              (gen = nextGen + 1 — a single global counter, strictly           *)
(*              increasing in CAS order; the implementation's published-root      *)
(*              `version`, bumped by every spine path-copy, §3.6) and updates     *)
(*              `present`/`removed` last-writer-wins; AND                        *)
(*   (step 2.5) appends+syncs its CommitRank marker (a new LSN), marking BOTH    *)
(*              the data LSN and the rank LSN committed.                        *)
(* Both happen before the op acks, so a COMMITTED (acked) op ALWAYS has its rank *)
(* durable with its correct generation. The two steps are merged into one        *)
(* transition because, for ONE op, the generation is fixed at its CAS and the    *)
(* rank append follows before return — another op interleaving in between only   *)
(* changes the OTHER op's (gen, rank), never this one's. The crash-between-CAS-  *)
(* and-rank window (§3.5) yields an UN-acked op (a pending record at crash), so  *)
(* it is outside the quiesced committed history the invariant constrains.       *)
(*                                                                         *)
(* Because AppendData fixed the data LSN earlier, a LOWER-data-LSN op can win    *)
(* its CAS LATER (higher gen) than a HIGHER-data-LSN op — the s019              *)
(* disagreement between `≺_LSN` and `≺_CAS`.                                    *)
(***************************************************************************)
CommitAndRank(rec) ==
    /\ rec \in pendingAppends
    /\ OpCount < MaxOps          \* the rank record also consumes an op slot
    /\ LET g       == nextGen + 1
           rankLsn == nextLsn
       IN /\ nextGen' = g
          /\ committedOps' = committedOps \cup
                 {[t |-> rec.t, kind |-> rec.kind, lsn |-> rec.lsn,
                   gen |-> g, rankLsn |-> rankLsn]}
          /\ pendingAppends' = pendingAppends \ {rec}
          /\ committedLsns' = committedLsns \cup {rankLsn}
          /\ nextLsn' = nextLsn + 1
          /\ present' = IF rec.kind = "Insert"
                          THEN present \cup {rec.t}
                          ELSE present \ {rec.t}
          /\ removed' = IF rec.kind = "Insert"
                          THEN removed \ {rec.t}
                          ELSE removed \cup {rec.t}
    /\ recovered' = FALSE     \* new activity ⇒ invalidate prior recovery
    /\ replayed' = {}

------------------------------------------------------------------------------
(***************************************************************************)
(* RECOVERY reconcile. For each term, among its committed data records whose    *)
(* DATA LSN is committed (durable), pick the WINNER and take its effect         *)
(* (Insert ⇒ present, Remove ⇒ absent).                                        *)
(*                                                                         *)
(* `OrderKey(rec)`:                                                            *)
(*   USE_COMMIT_RANK = TRUE  -> reconcile by (generation, lsn): the design.     *)
(*       generation_of = rec.gen when the rank is durable (rankLsn committed),  *)
(*       else falls back to rec.lsn (the IoError window — correct for the       *)
(*       single uncommitted op, §3.5).                                          *)
(*   USE_COMMIT_RANK = FALSE -> reconcile by (lsn, lsn): the BROKEN pre-fix     *)
(*       scheme that trusts physical/LSN order. The negative control.          *)
(*                                                                         *)
(* A record's generation is "durable" iff its rankLsn is committed; otherwise   *)
(* the reconcile uses the lsn fallback (`generation_of = lsn`).                *)
(***************************************************************************)
DurableData(rec) == rec.lsn \in committedLsns

GenerationOf(rec) ==
    IF (rec.rankLsn # 0) /\ (rec.rankLsn \in committedLsns)
      THEN rec.gen
      ELSE rec.lsn

OrderKey(rec) ==
    IF USE_COMMIT_RANK THEN GenerationOf(rec) ELSE rec.lsn

\* rec1 is STRICTLY EARLIER than rec2 under the active order (ties broken by lsn).
EarlierThan(rec1, rec2) ==
    \/ OrderKey(rec1) < OrderKey(rec2)
    \/ (OrderKey(rec1) = OrderKey(rec2) /\ rec1.lsn < rec2.lsn)

\* The committed data records for term t whose data LSN is durable.
TermRecords(t) ==
    { rec \in committedOps : rec.t = t /\ DurableData(rec) }

\* rec is the WINNER for t iff no other durable record for t is later under the
\* active order. (Distinct records always have distinct lsns ⇒ a unique winner.)
IsWinner(t, rec) ==
    /\ rec \in TermRecords(t)
    /\ \A other \in TermRecords(t) : (other = rec) \/ EarlierThan(other, rec)

\* The reconstructed-present set: terms whose winning durable record is an Insert.
ReplayPresent ==
    { t \in Terms : \E rec \in TermRecords(t) :
                       IsWinner(t, rec) /\ rec.kind = "Insert" }

(***************************************************************************)
(* CrashRecover — drop the volatile overlay and rebuild membership from the     *)
(* durable WAL via the reconcile above. The visible `present`/`removed` are     *)
(* UNCHANGED (they are the committed-visible ground truth we compare against);  *)
(* `replayed` is set to the reconstruction and `recovered` becomes TRUE.        *)
(* Modelling crash+recover as a pure observation of the durable state keeps the *)
(* invariant a clean comparison `replayed` vs `present` at every post-recovery  *)
(* state, for EVERY reachable committed history.                               *)
(***************************************************************************)
CrashRecover ==
    /\ replayed' = ReplayPresent
    /\ recovered' = TRUE
    /\ UNCHANGED <<committedOps, pendingAppends, present, removed,
                   committedLsns, nextLsn, nextGen>>

Next ==
    \/ \E t \in Terms, k \in Kinds : AppendData(t, k)
    \/ \E rec \in pendingAppends : CommitAndRank(rec)
    \/ CrashRecover

Spec == Init /\ [][Next]_AllVars

------------------------------------------------------------------------------
(* State constraint (TLC finiteness): the number of appended ops is bounded.    *)
OpBound == OpCount <= MaxOps

------------------------------------------------------------------------------
(* Invariants. The post-recovery ones are guarded by `recovered` so they only   *)
(* constrain states where a CrashRecover has produced a meaningful `replayed`.  *)

(* (1) HEADLINE — ReplayEqualsCommittedVisible. After recovery, the             *)
(* reconstructed-present set EQUALS the committed-visible present set, for       *)
(* EVERY term. This is exactly "LSN-ordered replay membership == CAS-order       *)
(* committed-visible membership" the bug violates. Under USE_COMMIT_RANK = TRUE  *)
(* the generation reconcile makes it hold; under FALSE the s019 interleaving     *)
(* breaks it (the Insert has the lower lsn but is the CAS last-writer, so        *)
(* lsn-order replay drops it). *)
(* Asserted only in a QUIESCED, just-recovered state (`recovered` ∧ no pending    *)
(* appends): that is the "well-defined committed history" the design reasons over  *)
(* (§1). A pending (un-acked) op at crash time is a torn write whose replay is     *)
(* allowed to go either way, so we do not constrain non-quiesced states; in a      *)
(* quiesced state `committedOps` is the complete durable history.                 *)
Quiesced == pendingAppends = {}

ReplayEqualsCommittedVisible ==
    (recovered /\ Quiesced)
        => (\A t \in Terms : (t \in replayed) <=> (t \in present))

(* (2) NoLostNetWrite — the s019 DIRECTION: a committed-visible (present) term    *)
(* is reconstructed present (never lost). *)
NoLostNetWrite ==
    (recovered /\ Quiesced)
        => (\A t \in Terms : (t \in present) => (t \in replayed))

(* (3) NoResurrectionOnReplay — the dual: a committed-absent term is NOT          *)
(* reconstructed present (no resurrection of a net-removed key). *)
NoResurrectionOnReplay ==
    (recovered /\ Quiesced)
        => (\A t \in Terms : (t \notin present) => (t \notin replayed))

(* (4) DurablePrefix (reused obligation) — every committed op's DATA LSN is in    *)
(* the committed set, i.e. the durable prefix covers it. An Order-A op is durable *)
(* (data LSN committed) before it is ever visible (in committedOps via a won      *)
(* CAS), and the rank append only ADDS its LSN — so a committed op's data is      *)
(* always recoverable. (Composes with LockFreeDurableCheckpoint's watermark; here *)
(* it guards that the reconcile never references a non-durable data record.)      *)
DurablePrefix ==
    \A rec \in committedOps : rec.lsn \in committedLsns

==============================================================================
