(** * ArenaInvariants: Arena Manager Invariant Proofs

    This module provides additional proofs about ArenaManager invariants,
    including:
    - Inductive invariant properties
    - State transition sequences
    - Recovery guarantees
*)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Require Import ARTrie.Model.ArenaManager.
Import ListNotations.

(** ** Inductive Invariant Property *)

(** The invariant is inductive: preserved by all safe transitions *)
Definition safe_transition (mgr mgr' : ArenaManager) : Prop :=
  (* Clear transition *)
  (mgr' = clear mgr) \/
  (* Clear for loading (fixed) transition *)
  (mgr' = clear_for_loading_FIXED mgr) \/
  (* Load arena transition *)
  (exists a, mgr' = load_arena mgr a) \/
  (* Set active arena transition *)
  (exists idx, mgr' = set_active_arena mgr idx) \/
  (* Ensure valid transition *)
  (mgr' = ensure_valid mgr /\ arena_size mgr > 0).

Theorem safe_transition_preserves_valid : forall mgr mgr',
  arena_manager_valid mgr ->
  safe_transition mgr mgr' ->
  arena_manager_valid mgr'.
Proof.
  intros mgr mgr' Hvalid Htrans.
  destruct Htrans as [Hclear | [Hcfl | [Hload | [Hset | [Hensure Hsize]]]]].
  - (* Clear *)
    subst. apply clear_preserves_valid. exact Hvalid.
  - (* Clear for loading fixed *)
    subst. apply clear_for_loading_fixed_valid. exact Hvalid.
  - (* Load arena *)
    destruct Hload as [a Ha]. subst.
    apply load_arena_preserves_valid. exact Hvalid.
  - (* Set active arena *)
    destruct Hset as [idx Hidx]. subst.
    apply set_active_arena_preserves_valid. exact Hvalid.
  - (* Ensure valid *)
    subst. apply ensure_valid_establishes_invariant. exact Hsize.
Qed.

(** ** Transition Sequences *)

(** Reflexive transitive closure of safe transitions *)
Inductive safe_transition_star : ArenaManager -> ArenaManager -> Prop :=
  | sts_refl : forall mgr, safe_transition_star mgr mgr
  | sts_step : forall mgr1 mgr2 mgr3,
      safe_transition mgr1 mgr2 ->
      safe_transition_star mgr2 mgr3 ->
      safe_transition_star mgr1 mgr3.

(** Invariant preserved across transition sequences *)
Theorem safe_transition_star_preserves_valid : forall mgr mgr',
  arena_manager_valid mgr ->
  safe_transition_star mgr mgr' ->
  arena_manager_valid mgr'.
Proof.
  intros mgr mgr' Hvalid Hstar.
  induction Hstar.
  - (* Reflexive case *)
    exact Hvalid.
  - (* Step case *)
    apply IHHstar.
    apply safe_transition_preserves_valid with (mgr := mgr1); assumption.
Qed.

(** ** Recovery Guarantees *)

(** From any state with positive arena_size, we can recover to valid state *)
Theorem recovery_always_possible : forall mgr,
  arena_size mgr > 0 ->
  arena_manager_valid (ensure_valid mgr).
Proof.
  intros mgr Hsize.
  apply ensure_valid_establishes_invariant. exact Hsize.
Qed.

(** Recovery followed by any safe operation maintains invariant *)
Theorem recovery_then_safe_valid : forall mgr mgr',
  arena_size mgr > 0 ->
  arena_size (ensure_valid mgr) > 0 ->
  safe_transition (ensure_valid mgr) mgr' ->
  arena_manager_valid mgr'.
Proof.
  intros mgr mgr' Hsize1 Hsize2 Htrans.
  apply safe_transition_preserves_valid with (mgr := ensure_valid mgr).
  - apply ensure_valid_establishes_invariant. exact Hsize1.
  - exact Htrans.
Qed.

(** ** Specific Scenario: Load Sequence *)

(** Common pattern: clear_for_loading followed by load_arena calls *)
Definition load_sequence (mgr : ArenaManager) (arenas_to_load : list Arena) : ArenaManager :=
  fold_left load_arena arenas_to_load (clear_for_loading_FIXED mgr).

(** Load sequence preserves invariant *)
Theorem load_sequence_valid : forall mgr arenas_to_load,
  arena_manager_valid mgr ->
  arena_manager_valid (load_sequence mgr arenas_to_load).
Proof.
  intros mgr arenas_to_load Hvalid.
  unfold load_sequence.
  assert (Hcfl: arena_manager_valid (clear_for_loading_FIXED mgr)).
  { apply clear_for_loading_fixed_valid. exact Hvalid. }
  clear Hvalid.
  generalize dependent (clear_for_loading_FIXED mgr).
  induction arenas_to_load as [|a rest IH]; intros mgr' Hvalid.
  - simpl. exact Hvalid.
  - simpl. apply IH.
    apply load_arena_preserves_valid. exact Hvalid.
Qed.

(** Arena count after load sequence: base case *)
Lemma load_sequence_arena_count_base : forall mgr,
  length (arenas (load_sequence mgr [])) = 1.
Proof.
  intros mgr.
  unfold load_sequence. simpl.
  unfold clear_for_loading_FIXED. simpl.
  reflexivity.
Qed.

(** Arena count increases with each loaded arena *)
Lemma load_arena_increases_count : forall mgr a,
  length (arenas (load_arena mgr a)) = length (arenas mgr) + 1.
Proof.
  intros mgr a.
  unfold load_arena. simpl.
  rewrite length_app. simpl. lia.
Qed.

(** ** Defensive Programming Theorems *)

(** next_slot_defensive agrees with next_slot when valid *)
Theorem defensive_agrees_when_valid : forall mgr,
  arena_manager_valid mgr ->
  next_slot mgr = Some (next_slot_defensive mgr).
Proof.
  apply defensive_matches_when_valid.
Qed.

(** next_slot_defensive returns fallback when arenas is empty *)
Lemma next_slot_defensive_fallback : forall active arena_sz,
  next_slot_defensive (mkArenaManager [] active arena_sz) = mkArenaSlot 0 0.
Proof.
  intros active arena_sz.
  unfold next_slot_defensive. simpl.
  (* nth_error [] active = None for any active *)
  destruct active; reflexivity.
Qed.

(** next_slot_defensive returns fallback when active_arena out of bounds *)
Lemma next_slot_defensive_oob : forall mgr,
  active_arena mgr >= length (arenas mgr) ->
  length (arenas mgr) > 0 ->
  next_slot_defensive mgr = mkArenaSlot 0 0.
Proof.
  intros mgr Hoob Hlen.
  unfold next_slot_defensive.
  destruct (nth_error (arenas mgr) (active_arena mgr)) eqn:E.
  - (* Got Some - contradiction *)
    exfalso.
    assert (Hne: nth_error (arenas mgr) (active_arena mgr) <> None).
    { rewrite E. discriminate. }
    apply nth_error_Some in Hne. lia.
  - reflexivity.
Qed.

(** ** Error Recovery Scenarios *)

(** Scenario: After buggy clear, ensure_valid recovers *)
Theorem buggy_clear_recovery : forall mgr,
  arena_size mgr > 0 ->
  arena_manager_valid (ensure_valid (clear_for_loading_BUGGY mgr)).
Proof.
  intros mgr Hsize.
  unfold ensure_valid, clear_for_loading_BUGGY. simpl.
  apply new_manager_valid.
  exact Hsize.
Qed.

(** Scenario: After any corruption, ensure_valid recovers *)
Theorem any_state_recovery : forall arenas active_arena arena_size,
  arena_size > 0 ->
  let mgr := mkArenaManager arenas active_arena arena_size in
  arena_manager_valid (ensure_valid mgr).
Proof.
  intros arenas' active_arena' arena_size' Hsize mgr.
  apply ensure_valid_establishes_invariant.
  simpl. exact Hsize.
Qed.

(** ** Open() Error Recovery Scenario *)

(** Scenario: open() with failed disk loading

    This models the actual code path that caused the ArenaManager panic:
    1. open() creates ArenaManager with 1 arena
    2. load_root_from_disk() calls clear_for_loading() (BUGGY version) → 0 arenas
    3. load_arena() fails (corrupt file, I/O error, etc.)
    4. load_root_from_disk() returns Err
    5. open() catches error, logs it, falls back to WAL replay
    6. **The fix**: ensure_valid() is called to restore invariant
    7. open() returns Ok(inner) with valid arena manager

    This theorem proves that calling ensure_valid() after the failed loading
    sequence correctly restores the invariant. *)
Theorem open_with_failed_loading_recovered : forall mgr,
  arena_size mgr > 0 ->
  arena_manager_valid (ensure_valid (clear_for_loading_BUGGY mgr)).
Proof.
  (* This is exactly buggy_clear_recovery - reuse it *)
  apply buggy_clear_recovery.
Qed.

(** The corrected open() sequence maintains invariant regardless of loading outcome.

    This corollary captures the full open() behavior:
    - If loading succeeds: arena manager is valid (via load_sequence_valid)
    - If loading fails: ensure_valid() restores validity (via buggy_clear_recovery)

    Together, these guarantee that open() always returns a valid arena manager. *)
Corollary open_always_valid : forall mgr arenas_to_load,
  arena_manager_valid mgr ->
  arena_size mgr > 0 ->
  (* Whether loading succeeds or fails, the result is valid *)
  arena_manager_valid (load_sequence mgr arenas_to_load) /\
  arena_manager_valid (ensure_valid (clear_for_loading_BUGGY mgr)).
Proof.
  intros mgr arenas_to_load Hvalid Hsize.
  split.
  - (* Loading succeeds case *)
    apply load_sequence_valid. exact Hvalid.
  - (* Loading fails case - apply recovery *)
    apply buggy_clear_recovery. exact Hsize.
Qed.

(** ** Composition Theorems *)

(** Composition: ensure_valid followed by clear maintains validity *)
Theorem ensure_then_clear_valid : forall mgr,
  arena_size mgr > 0 ->
  arena_manager_valid (clear (ensure_valid mgr)).
Proof.
  intros mgr Hsize.
  apply clear_preserves_valid.
  apply ensure_valid_establishes_invariant.
  exact Hsize.
Qed.

(** Composition: Multiple operations maintain validity *)
Theorem multi_op_valid : forall mgr a idx,
  arena_manager_valid mgr ->
  arena_manager_valid (set_active_arena (load_arena (clear mgr) a) idx).
Proof.
  intros mgr a idx Hvalid.
  apply set_active_arena_preserves_valid.
  apply load_arena_preserves_valid.
  apply clear_preserves_valid.
  exact Hvalid.
Qed.

(** ** Boolean Decision Procedures *)

(** is_valid as computable predicate *)
Definition is_valid (mgr : ArenaManager) : bool :=
  arena_manager_valid_b mgr.

(** is_valid reflects arena_manager_valid *)
Lemma is_valid_correct : forall mgr,
  is_valid mgr = true <-> arena_manager_valid mgr.
Proof.
  intros mgr.
  unfold is_valid.
  apply arena_manager_valid_reflect.
Qed.

(** Computable ensure_valid correctness *)
Lemma ensure_valid_makes_valid : forall mgr,
  arena_size mgr > 0 ->
  is_valid (ensure_valid mgr) = true.
Proof.
  intros mgr Hsize.
  apply is_valid_correct.
  apply ensure_valid_establishes_invariant.
  exact Hsize.
Qed.
