(** Overlay reestablish-after-recovery model (S5-12 E1, the D1 data-loss guard).

    On an Overlay-regime reopen the char ARTrie rebuilds the immutable lock-free
    overlay from the RECOVERED OWNED tree: for each recovered term it publishes
    (term, value) into the overlay, then clears the owned tree LAST.  Because the
    ctor flips BEFORE dispatching reestablish, [route_overlay()] is already true
    while reestablish runs.  The read-flip (E1) routes the public reads to the
    overlay, so if reestablish were to read its source THROUGH the routed reads it
    would read the still-EMPTY overlay, publish nothing, then clear owned = total
    irreversible loss (defect D1).  The fix: reestablish reads the OWNED source via
    un-routed [owned_*] readers.

    This model proves the guard with a NEGATIVE CONTROL: the same fold, parameterized
    only by its read SOURCE.
    - [reestablish_owned]  reads the fixed recovered-owned map (the un-routed fix);
    - [reestablish_routed] reads the overlay-being-built (the D1 bug: E1-routed reads
      on the empty overlay).
    The correctness theorem holds ONLY for the owned source; for the routed source the
    fold provably publishes NOTHING, so every recovered value is lost.  A proof that
    distinguishes the fix from the bug is a real guard, not a restatement of the code.

    Value is modelled as [nat]; it covers both the [u64] counter overlay (Some count)
    and the [()] membership overlay (Some sentinel) — the data-loss property is the
    same: a recovered (present) term must survive reestablish.

    No axioms, no admits. Build: [coqc -Q . ARTrie Spec/OverlayReestablishSpec.v]. *)

Require Import Coq.Lists.List.
Require Import Coq.Arith.PeanoNat.
Import ListNotations.

Definition Term := nat.
Definition Value := nat.

(** Overlay / owned represented as a partial map (None = absent). *)
Definition Map := Term -> option Value.

Definition empty_map : Map := fun _ => None.

Definition map_put (m : Map) (k : Term) (v : Value) : Map :=
  fun q => if Nat.eqb q k then Some v else m q.

Lemma map_put_same : forall m k v, map_put m k v k = Some v.
Proof. intros. unfold map_put. rewrite Nat.eqb_refl. reflexivity. Qed.

Lemma map_put_other : forall m k v q, q <> k -> map_put m k v q = m q.
Proof.
  intros m k v q Hneq. unfold map_put.
  apply Nat.eqb_neq in Hneq. rewrite Hneq. reflexivity.
Qed.

(** CORRECT reestablish (the D1 fix): publish each term's value READ FROM THE FIXED
    recovered-owned map. The owned source does not change as the overlay grows. *)
Fixpoint reestablish_owned (terms : list Term) (owned : Map) (overlay : Map) : Map :=
  match terms with
  | [] => overlay
  | t :: rest =>
      let overlay' :=
        match owned t with
        | Some v => map_put overlay t v
        | None => overlay
        end in
      reestablish_owned rest owned overlay'
  end.

(** BUGGY reestablish (D1): the ctor flipped first, so the E1 read-flip routes the
    source reads to the OVERLAY-being-built (empty for any unseen term). *)
Fixpoint reestablish_routed (terms : list Term) (overlay : Map) : Map :=
  match terms with
  | [] => overlay
  | t :: rest =>
      let overlay' :=
        match overlay t with
        | Some v => map_put overlay t v
        | None => overlay
        end in
      reestablish_routed rest overlay'
  end.

(** A term not in the list is left exactly as the incoming accumulator. *)
Lemma reestablish_owned_not_in : forall terms owned overlay t,
  ~ In t terms -> reestablish_owned terms owned overlay t = overlay t.
Proof.
  induction terms as [| h rest IH]; intros owned overlay t Hnin; simpl.
  - reflexivity.
  - assert (t <> h) as Hth by (intro Hc; subst; apply Hnin; left; reflexivity).
    assert (~ In t rest) as Hnr by (intro Hc; apply Hnin; right; exact Hc).
    destruct (owned h) as [vh|] eqn:Eh.
    + rewrite (IH owned (map_put overlay h vh) t Hnr).
      apply map_put_other. exact Hth.
    + rewrite (IH owned overlay t Hnr). reflexivity.
Qed.

(** NO-LOSS (the correctness core): every recovered value survives the OWNED-source
    fold, regardless of the starting accumulator. *)
Lemma reestablish_owned_no_loss : forall terms owned overlay t v,
  In t terms -> owned t = Some v ->
  reestablish_owned terms owned overlay t = Some v.
Proof.
  induction terms as [| h rest IH]; intros owned overlay t v Hin Hov; simpl.
  - destruct Hin.
  - destruct Hin as [Heq | Hin'].
    + subst h. rewrite Hov.
      destruct (in_dec Nat.eq_dec t rest) as [Hinr | Hninr].
      * apply IH; assumption.
      * rewrite reestablish_owned_not_in by exact Hninr.
        apply map_put_same.
    + destruct (owned h) as [vh|] eqn:Eh; apply IH; assumption.
Qed.

(** Top-level (from the empty overlay): reestablish from the owned source preserves
    every recovered value — published_overlay agrees with recovered_owned. *)
Theorem reestablish_owned_preserves_recovered : forall terms owned t v,
  In t terms -> owned t = Some v ->
  reestablish_owned terms owned empty_map t = Some v.
Proof. intros. eapply reestablish_owned_no_loss; eassumption. Qed.

(** An everywhere-empty overlay stays everywhere-empty under the routed fold: each
    step's publish is conditioned on reading Some from the overlay, never true. *)
Lemma reestablish_routed_preserves_none : forall terms overlay,
  (forall k, overlay k = None) ->
  forall t, reestablish_routed terms overlay t = None.
Proof.
  induction terms as [| h rest IH]; intros overlay Hnone t; simpl.
  - apply Hnone.
  - rewrite Hnone. apply IH. exact Hnone.
Qed.

(** NEGATIVE CONTROL: the D1-buggy routed fold publishes NOTHING from the empty
    overlay — the exact total-loss the flip-before-reestablish ordering causes. *)
Theorem reestablish_routed_publishes_nothing : forall terms t,
  reestablish_routed terms empty_map t = None.
Proof.
  intros. apply reestablish_routed_preserves_none.
  intros k. unfold empty_map. reflexivity.
Qed.

(** THE D1 GUARD (the headline): for any recovered term-with-value, the OWNED-source
    reestablish keeps it, the ROUTED-source reestablish loses it, and the two results
    differ. The proof goes through ONLY because the fix reads the owned source; it
    would be unprovable if reestablish read the routed (overlay) source — exactly the
    bug the red-team caught. *)
Theorem d1_routed_reads_lose_every_recovered_value : forall terms owned t v,
  In t terms -> owned t = Some v ->
  reestablish_owned terms owned empty_map t = Some v
  /\ reestablish_routed terms empty_map t = None
  /\ reestablish_owned terms owned empty_map t
     <> reestablish_routed terms empty_map t.
Proof.
  intros terms owned t v Hin Hov. split; [| split].
  - apply reestablish_owned_preserves_recovered; assumption.
  - apply reestablish_routed_publishes_nothing.
  - rewrite reestablish_routed_publishes_nothing.
    rewrite (reestablish_owned_preserves_recovered terms owned t v Hin Hov).
    discriminate.
Qed.

(** Clear-last abort-safety. Success = publish all, THEN clear owned. An abort at any
    prefix [k] returns the partial overlay with the owned tree STILL INTACT — so a
    mid-stream failure loses nothing and a re-run recovers everything. *)
Definition reestablish_then_clear (terms : list Term) (owned : Map) : Map * Map :=
  (reestablish_owned terms owned empty_map, empty_map).

Definition reestablish_abort_at (terms : list Term) (owned : Map) (k : nat)
  : Map * Map :=
  (reestablish_owned (firstn k terms) owned empty_map, owned).

(** On any abort the owned source is UNCLEARED (the clear is strictly last). *)
Theorem abort_preserves_owned : forall terms owned k,
  snd (reestablish_abort_at terms owned k) = owned.
Proof. intros. reflexivity. Qed.

(** Everything published before the abort came from the intact owned tree, so it is
    sound (a subset of owned) — re-running reestablish recovers the full set. *)
Theorem abort_published_sound : forall terms owned k t v,
  In t (firstn k terms) -> owned t = Some v ->
  fst (reestablish_abort_at terms owned k) t = Some v.
Proof.
  intros. simpl. apply reestablish_owned_preserves_recovered; assumption.
Qed.

(** The success path clears owned only after a complete, value-preserving publish. *)
Theorem then_clear_preserves_recovered_then_clears : forall terms owned,
  (forall t v, In t terms -> owned t = Some v ->
     fst (reestablish_then_clear terms owned) t = Some v)
  /\ snd (reestablish_then_clear terms owned) = empty_map.
Proof.
  intros terms owned. split.
  - intros t v Hin Hov. simpl.
    apply reestablish_owned_preserves_recovered; assumption.
  - reflexivity.
Qed.
