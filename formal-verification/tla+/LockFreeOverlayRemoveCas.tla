------------------------ MODULE LockFreeOverlayRemoveCas ------------------------
(***************************************************************************)
(* PROVEN OVERLAY DELETE (design "R-B") — composite linearizability of      *)
(* {insert, remove} on the lock-free char-ARTrie overlay once finality is    *)
(* no longer monotone.                                                       *)
(*                                                                         *)
(* The Rust components being modelled are `insert_cas_durable` /             *)
(* `try_insert_lockfree_path` (the 0→1 finalize, via the shared node's       *)
(* in-place `try_set_final` = `fetch_or(IS_FINAL)`) and the NEW R-B          *)
(* `remove_cas_durable` / `try_remove_lockfree_path` /                       *)
(* `build_remove_path_recursive` (the 1→0 clear). Both publish through the   *)
(* SINGLE `lockfree_root` atomic CAS (`AtomicNodePtr`, loser-safe per        *)
(* `Arc::ptr_eq`).                                                           *)
(*                                                                         *)
(* The content invariant the insert proofs relied on — finality MONOTONE     *)
(* (0→1 only) — is BROKEN by delete (1→0). This spec replaces monotonicity   *)
(* with LAST-WRITER-WINS under the root-CAS total order and machine-checks   *)
(* that the composite stays linearizable:                                    *)
(*                                                                         *)
(*   1. LastWriterWins — a term is published-present IFF its last committed   *)
(*      writer was an insert (equivalently: NOT a remove). `present` and      *)
(*      `removed` are EXACT complements over the acked terms. This is the     *)
(*      headline obligation.                                                  *)
(*   2. NoResurrection — a removed term is present again ONLY via a later     *)
(*      insert (a remove never spontaneously un-removes; no stale-cache /     *)
(*      in-place-clear race resurrects it).                                   *)
(*   3. NoLostOp — every acked op's effect is reflected in the published      *)
(*      root: an acked insert ⇒ present (unless a LATER remove cleared it);   *)
(*      an acked remove ⇒ absent (unless a LATER insert re-added it). No op   *)
(*      is silently dropped by a racing CAS.                                  *)
(*                                                                         *)
(* USE_FRESH_COPY_CLEAR = TRUE  -> the design (§3.5): the 1→0 clear is        *)
(*   published on a FRESH `as_non_final()` node version ONLY via the root CAS,*)
(*   so `present` and `removed` update ATOMICALLY with one root bump and stay *)
(*   exact complements. ALL invariants hold.                                 *)
(* USE_FRESH_COPY_CLEAR = FALSE -> the `_Unsafe.cfg` NEGATIVE CONTROL: models *)
(*   the REJECTED in-place `fetch_and(!IS_FINAL)` clear (clear `present`      *)
(*   WITHOUT bumping `root`, decoupled from the `removed` bookkeeping). With  *)
(*   no root-CAS serialization, a concurrent in-place `fetch_or` insert can   *)
(*   re-set `present` while `removed` stays set (resurrection), OR a clear    *)
(*   can drop `present` while a stale insert leaves `removed` unset (lost     *)
(*   remove) — reaching a state where `present` and `removed` are NOT         *)
(*   complements. TLC MUST report a violation of `LastWriterWins`. If TLC     *)
(*   unexpectedly PASSES this, the model no longer exhibits the bug it must   *)
(*   catch -> the negative control is broken -> fail the whole gate.         *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Terms,              \* the finite set of terms that may be inserted/removed
    USE_FRESH_COPY_CLEAR, \* TRUE = design (atomic fresh-copy + root CAS);
                          \* FALSE = unsafe negative control (in-place clear, no root bump)
    MaxRoot             \* TLC finiteness cap on the monotone `root` version tag

ASSUME Terms # {}

VARIABLES
    root,       \* abstract published-root "version" id (a Nat, bumped per CAS)
    present,    \* set of terms whose published-root membership is final (a member)
    removed,    \* set of terms currently ABSENT per their last writer (last op was a
                \* remove, OR the term was never inserted) — the intended complement
                \* of `present` over ALL terms
    acked       \* set of terms with at least one acknowledged op (insert or remove)

Vars == <<root, present, removed, acked>>

TypeInvariant ==
    /\ root \in Nat
    /\ present \subseteq Terms
    /\ removed \subseteq Terms
    /\ acked   \subseteq Terms

\* Empty overlay: nothing present; EVERY term is absent (`removed = Terms`, i.e. no
\* term's last writer was an insert), nothing acked. Modelling "never inserted" as
\* "removed" makes `present`/`removed` exact complements from the start, so
\* LastWriterWins is a total biconditional over ALL terms (not just acked ones):
\* at Init `(t \in {}) <=> (t \notin Terms)` = FALSE <=> FALSE = TRUE.
Init ==
    /\ root = 1
    /\ present = {}
    /\ removed = Terms
    /\ acked = {}

(***************************************************************************)
(* SAFE actions (the design). Each is published by the SINGLE root CAS, so   *)
(* it atomically (one root bump) updates BOTH `present` and `removed`,        *)
(* keeping them exact complements over acked terms.                          *)
(***************************************************************************)

\* InsertCas(t): a lock-free writer finalizes `t` (0→1 via try_set_final on the
\* shared node, published by the root CAS). `t` becomes present and leaves the
\* removed set (its last writer is now an insert). The WAL `Insert` was synced
\* durable BEFORE this visibility CAS (Order A); `acked` records that.
InsertCas(t) ==
    /\ t \in Terms
    /\ root' = root + 1
    /\ present' = present \cup {t}
    /\ removed' = removed \ {t}
    /\ acked' = acked \cup {t}

\* RemoveCas(t): the R-B 1→0 clear of a PRESENT term. The design path publishes a
\* FRESH `as_non_final()` leaf via the root CAS, so `present` and `removed` update
\* atomically with one root bump: `t` leaves present and joins removed (its last
\* writer is now a remove). Enabled only when `t` is present (a no-op remove of an
\* absent term is `RemoveAbsentNoop`, which writes no spine).
RemoveCas(t) ==
    /\ t \in Terms
    /\ t \in present
    /\ root' = root + 1
    /\ present' = present \ {t}
    /\ removed' = removed \cup {t}
    /\ acked' = acked \cup {t}

\* RemoveAbsentNoop(t): removing an ABSENT term. The Rust path returns Ok(false)
\* with NO WAL record and NO spine published (no root bump). It still marks the
\* op acknowledged and records that the last op on `t` was a remove (its membership
\* stays absent — `t \notin present`). Models the absent fast-path (design §3 step
\* 3). This keeps `removed` consistent with "last op was a remove" without a CAS.
RemoveAbsentNoop(t) ==
    /\ t \in Terms
    /\ t \notin present
    /\ removed' = removed \cup {t}
    /\ acked' = acked \cup {t}
    /\ UNCHANGED <<root, present>>

(***************************************************************************)
(* UNSAFE actions (the negative control, USE_FRESH_COPY_CLEAR = FALSE):      *)
(* the REJECTED in-place `fetch_and(!IS_FINAL)` clear. It writes `present` and *)
(* `removed` to SEPARATE locations with NO atomic coupling and NO root bump,  *)
(* so the two can diverge under an interleaving — exactly the resurrection /  *)
(* lost-remove race §3.5 warns about.                                        *)
(***************************************************************************)

\* InsertCasInPlace(t): an in-place `fetch_or(IS_FINAL)` on the shared node — sets
\* `present` WITHOUT clearing `removed` and WITHOUT a root bump (no CAS arbitration
\* of the two locations). On its own this is the monotone-safe insert, but with no
\* root CAS it can interleave between an unsafe remove's two sub-writes.
InsertCasInPlace(t) ==
    /\ ~USE_FRESH_COPY_CLEAR
    /\ t \in Terms
    /\ present' = present \cup {t}
    /\ acked' = acked \cup {t}
    /\ UNCHANGED <<root, removed>>

\* RemoveInPlaceClearOnly(t): the in-place clear's FIRST sub-write — clears
\* `present` (the `fetch_and(!IS_FINAL)`) WITHOUT yet marking `removed` and WITHOUT
\* a root bump. A concurrent `InsertCasInPlace(t)` can now re-set `present`; if the
\* second sub-write (mark removed) lands afterward, the state reaches
\* `t \in present \cap removed` (resurrection) — a LastWriterWins violation.
RemoveInPlaceClearOnly(t) ==
    /\ ~USE_FRESH_COPY_CLEAR
    /\ t \in Terms
    /\ present' = present \ {t}
    /\ acked' = acked \cup {t}
    /\ UNCHANGED <<root, removed>>

\* RemoveInPlaceMarkOnly(t): the in-place clear's SECOND sub-write — marks
\* `removed` WITHOUT (re)clearing `present`. If an `InsertCasInPlace(t)` interleaved
\* between the two sub-writes, `present` is set again here while `removed` is set,
\* breaking the complement.
RemoveInPlaceMarkOnly(t) ==
    /\ ~USE_FRESH_COPY_CLEAR
    /\ t \in Terms
    /\ removed' = removed \cup {t}
    /\ acked' = acked \cup {t}
    /\ UNCHANGED <<root, present>>

Next ==
    \/ \E t \in Terms : InsertCas(t)
    \/ \E t \in Terms : RemoveCas(t)
    \/ \E t \in Terms : RemoveAbsentNoop(t)
    \/ \E t \in Terms : InsertCasInPlace(t)
    \/ \E t \in Terms : RemoveInPlaceClearOnly(t)
    \/ \E t \in Terms : RemoveInPlaceMarkOnly(t)

Spec == Init /\ [][Next]_Vars

------------------------------------------------------------------------------
\* State constraint (TLC finiteness): `root` is an unbounded monotone Nat (bumped
\* once per successful CAS, as the Rust `AtomicNodePtr` version increments). No
\* invariant references the numeric value of `root` — the abstract membership state
\* every invariant depends on is the finite set-valued vars over the finite
\* `Terms`. We cap `root` with a TLC CONSTRAINT (the standard idiom for an
\* unbounded monotone counter, mirroring `OverlayEvictionCas`'s RootBound and
\* `LockFreeDurableCheckpoint`'s bounded `nextLsn`). MaxRoot is generous: with 2
\* Terms, inserting + removing + re-inserting both fits well under 6.
RootBound == root <= MaxRoot

------------------------------------------------------------------------------
\* Invariants

\* (1) LastWriterWins — the headline obligation. A term is published-present IFF
\* its last committed writer was an insert (equivalently: it is NOT in `removed`).
\* `present` and `removed` are EXACT complements: no term is both present AND
\* last-removed (resurrection), and no acked term is neither present NOR
\* last-removed (a lost op leaving an inconsistent membership). Under the design
\* (atomic fresh-copy + root CAS) every action preserves the complement. The unsafe
\* in-place clear breaks it (resurrection or lost remove), which TLC must catch.
LastWriterWins == \A t \in Terms : (t \in present) <=> (t \notin removed)

\* (2) NoResurrection — a removed term is present again ONLY via a later insert.
\* Stated as: a term that is currently in `removed` is NOT present (the only way to
\* become present is `InsertCas`/`InsertCasInPlace`, which removes it from
\* `removed`). A stale-cache / in-place-clear race that left a term present while
\* still marked removed violates this. (Implied by LastWriterWins, asserted
\* separately as the sharp resurrection statement the §3.4 cache invalidation and
\* §3.5 fresh-copy choice both target.)
NoResurrection == \A t \in Terms : (t \in removed) => (t \notin present)

\* (3) NoLostOp — every acknowledged op's effect is reflected: an acked term is
\* EITHER present (its last writer was an insert) OR removed (its last writer was a
\* remove) — never silently dropped to a state that is neither. Combined with
\* LastWriterWins this says the published root faithfully reflects the last writer
\* of every acked term (no racing CAS erased an op).
NoLostOp == \A t \in acked : (t \in present) \/ (t \in removed)

==============================================================================
