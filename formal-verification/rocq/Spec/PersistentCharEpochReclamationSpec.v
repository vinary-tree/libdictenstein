(** * PersistentCharEpochReclamationSpec: Eviction-vs-Walk EBR Safety

    Models the non-blocking epoch-based reclamation that makes the persistent char
    trie's lock-free [DictionaryNode] walk safe against concurrent eviction. The
    walk hands out raw pointers (node ids here) that escape any lock; eviction
    UNLINKS a subtree (so new readers cannot reach it), RETIRES it, and FREES it
    only after a quiescence drain proves no reader is active. A reader that latched
    a node before the unlink keeps it until it exits; the drain-to-zero gate ensures
    such a reader has drained before its node is freed.

    The checked claim is the headline safety property:

      [no_use_after_free]: in EVERY reachable state, no active reader holds a raw
      pointer to a freed node.

    proved as a state invariant preserved by every protocol action
    (EnterRead / LoadPtr(=fault) / Unlink+retire / Reclaim / ExitRead), where
    Reclaim is GATED on quiescence (no active readers). No [Admitted], no [Axiom],
    no [Parameter]. *)

From Stdlib Require Import Arith.PeanoNat.

Section EpochReclamation.

(** Sets of natural-number ids as characteristic predicates. *)
Definition NSet := nat -> Prop.

Definition empty : NSet := fun _ => False.
Definition add (s : NSet) (x : nat) : NSet := fun n => s n \/ n = x.
Definition remove (s : NSet) (x : nat) : NSet := fun n => s n /\ n <> x.
Definition union (s t : NSet) : NSet := fun n => s n \/ t n.
Definition disjoint (s t : NSet) : Prop := forall n, s n -> t n -> False.

(** Per-reader latched-pointer map update. *)
Definition upd (f : nat -> NSet) (r : nat) (v : NSet) : nat -> NSet :=
  fun rr => if Nat.eq_dec rr r then v else f rr.

(** Protocol state. *)
Record State := mkState {
  reachable : NSet;          (* nodes still linked in the tree (loadable) *)
  retired   : NSet;          (* unlinked + retired, awaiting reclaim *)
  freed     : NSet;          (* boxes already freed *)
  active    : NSet;          (* readers currently pinned in an epoch *)
  latched   : nat -> NSet    (* reader -> nodes it holds a raw pointer to *)
}.

(** Initial state: every node linked; nothing retired/freed; no readers. *)
Definition init (all : NSet) : State :=
  mkState all empty empty empty (fun _ => empty).

(** One protocol step. *)
Inductive step : State -> State -> Prop :=
(* A reader pins the epoch (enters), starting with no latched pointers. *)
| StepEnter : forall s r,
    step s (mkState (reachable s) (retired s) (freed s)
                    (add (active s) r)
                    (upd (latched s) r empty))
(* An active reader loads / FAULTS a pointer to a currently-reachable node. New
   loads can only reach LINKED nodes, so a reader never newly grabs a retired or
   freed node (after an unlink it re-faults a fresh, reachable node). *)
| StepLoad : forall s r n,
    active s r -> reachable s n ->
    step s (mkState (reachable s) (retired s) (freed s) (active s)
                    (upd (latched s) r (add (latched s r) n)))
(* Eviction unlinks a reachable node and retires it (does NOT free). *)
| StepUnlink : forall s n,
    reachable s n ->
    step s (mkState (remove (reachable s) n) (add (retired s) n)
                    (freed s) (active s) (latched s))
(* Reclaim frees all retired nodes — GATED on quiescence (no active readers). *)
| StepReclaim : forall s,
    (forall r, ~ active s r) ->
    step s (mkState (reachable s) empty (union (freed s) (retired s))
                    (active s) (latched s))
(* A reader exits its epoch, dropping its latched pointers. *)
| StepExit : forall s r,
    step s (mkState (reachable s) (retired s) (freed s)
                    (remove (active s) r)
                    (upd (latched s) r empty)).

(** Reachability of states from [init]. *)
Inductive reachableState (s0 : State) : State -> Prop :=
| RS_refl : reachableState s0 s0
| RS_step : forall s s', reachableState s0 s -> step s s' -> reachableState s0 s'.

(** ** Invariants *)

(** Headline: no active reader holds a pointer to a freed node. *)
Definition NoUseAfterFree (s : State) : Prop :=
  forall r n, active s r -> latched s r n -> ~ freed s n.

(** A reachable (linked) node is never freed. *)
Definition ReachableNotFreed (s : State) : Prop :=
  forall n, reachable s n -> ~ freed s n.

(** Reachable and retired are disjoint (a node is linked OR retired, not both). *)
Definition ReachableRetiredDisjoint (s : State) : Prop :=
  disjoint (reachable s) (retired s).

Definition Inv (s : State) : Prop :=
  NoUseAfterFree s /\ ReachableNotFreed s /\ ReachableRetiredDisjoint s.

(** ** The invariant holds initially and is preserved by every step *)

Lemma init_Inv : forall all, Inv (init all).
Proof.
  intros all.
  unfold Inv, NoUseAfterFree, ReachableNotFreed, ReachableRetiredDisjoint,
         disjoint, init, empty; simpl.
  repeat split.
  - intros r n _ Hl. destruct Hl.     (* latched is empty -> False *)
  - intros n _ Hf. destruct Hf.       (* freed is empty -> False *)
  - intros n _ Hret. destruct Hret.   (* retired is empty -> False *)
Qed.

Lemma step_preserves_Inv : forall s s', Inv s -> step s s' -> Inv s'.
Proof.
  intros s s' [Hnuaf [Hrnf Hdisj]] Hstep.
  destruct Hstep; unfold Inv, NoUseAfterFree, ReachableNotFreed,
                         ReachableRetiredDisjoint, disjoint, add, remove, union,
                         upd, empty in *; simpl in *.
  - (* StepEnter *)
    repeat split.
    + intros r0 n Hact Hl.
      destruct (Nat.eq_dec r0 r) as [He | Hne].
      * destruct Hl.                  (* new reader: empty latched *)
      * destruct Hact as [Hact | Heq].
        -- exact (Hnuaf r0 n Hact Hl).
        -- congruence.                (* Heq : r0 = r contradicts Hne : r0 <> r *)
    + exact Hrnf.
    + exact Hdisj.
  - (* StepLoad *)
    repeat split.
    + intros r0 n0 Hact Hl.
      destruct (Nat.eq_dec r0 r) as [He | Hne].
      * destruct Hl as [Hold | Hnew].
        -- subst r0. exact (Hnuaf r n0 Hact Hold).
        -- subst n0. exact (Hrnf n H0).
      * exact (Hnuaf r0 n0 Hact Hl).
    + exact Hrnf.
    + exact Hdisj.
  - (* StepUnlink *)
    repeat split.
    + exact Hnuaf.
    + intros n0 [Hr _]. exact (Hrnf n0 Hr).
    + intros n0 [Hr Hne] [Hret | Heq].
      * exact (Hdisj n0 Hr Hret).
      * exact (Hne Heq).
  - (* StepReclaim *)
    repeat split.
    + intros r0 n0 Hact _. exfalso. exact (H r0 Hact).  (* no active readers *)
    + intros n0 Hr [Hf | Hret].
      * exact (Hrnf n0 Hr Hf).
      * exact (Hdisj n0 Hr Hret).
    + intros n0 _ Hret. exact Hret.   (* retired is empty *)
  - (* StepExit *)
    repeat split.
    + intros r0 n0 [Hact Hne] Hl.
      destruct (Nat.eq_dec r0 r) as [He | Hne'].
      * destruct Hl.                  (* exited reader: empty latched *)
      * exact (Hnuaf r0 n0 Hact Hl).
    + exact Hrnf.
    + exact Hdisj.
Qed.

Theorem reachable_Inv :
  forall all s, reachableState (init all) s -> Inv s.
Proof.
  intros all s Hreach.
  induction Hreach as [| s s' Hreach IH Hstep].
  - apply init_Inv.
  - exact (step_preserves_Inv s s' IH Hstep).
Qed.

(** MAIN: in every reachable state, no active reader holds a pointer to a freed
    node — the eviction-vs-walk use-after-free is impossible under the gated
    unlink -> retire -> drain -> free protocol. *)
Theorem no_use_after_free :
  forall all s, reachableState (init all) s -> NoUseAfterFree s.
Proof.
  intros all s Hreach.
  destruct (reachable_Inv all s Hreach) as [Hnuaf _].
  exact Hnuaf.
Qed.

(** Corollary: reclamation only ever fires at quiescence — the [StepReclaim]
    precondition. Stated as: any step that grows [freed] required no active
    readers. *)
Theorem reclaim_requires_quiescence :
  forall s s' n,
    step s s' -> ~ freed s n -> freed s' n ->
    (forall r, ~ active s r).
Proof.
  intros s s' n Hstep Hnot Hnow.
  destruct Hstep; simpl in *.
  - exfalso. exact (Hnot Hnow).            (* Enter: freed unchanged *)
  - exfalso. exact (Hnot Hnow).            (* Load: freed unchanged *)
  - exfalso. exact (Hnot Hnow).            (* Unlink: freed unchanged *)
  - exact H.                               (* Reclaim: precondition gives it *)
  - exfalso. exact (Hnot Hnow).            (* Exit: freed unchanged *)
Qed.

End EpochReclamation.
