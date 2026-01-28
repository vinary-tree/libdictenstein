(** * ArenaManager: Memory Arena Management Model

    This module defines the ArenaManager state and its invariants.
    The ArenaManager allocates node storage across multiple arenas,
    with automatic overflow to new arenas when capacity is exceeded.

    Key invariant: The arenas list is NEVER empty when operations are allowed.

    This invariant prevents the panic:
      "index out of bounds: the len is 0 but the index is 0"
    which occurs in next_slot() when clear_for_loading() empties arenas
    but subsequent load_arena() fails.
*)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Import ListNotations.

(** ** Basic Types *)

(** Arena slot identifier - uniquely identifies a node's location *)
Record ArenaSlot := mkArenaSlot {
  arena_id : nat;
  slot_id : nat
}.

(** Single arena - simplified model focusing on allocation state *)
Record Arena := mkArena {
  a_id : nat;           (** Arena identifier *)
  node_count : nat;     (** Number of allocated nodes *)
  capacity : nat;       (** Maximum capacity *)
  block_id : option nat (** Disk block ID (None = not persisted) *)
}.

(** Arena constructor - creates fresh arena *)
Definition new_arena (id : nat) (cap : nat) : Arena := {|
  a_id := id;
  node_count := 0;
  capacity := cap;
  block_id := None
|}.

(** ** ArenaManager State *)

(** The ArenaManager manages multiple arenas for node allocation *)
Record ArenaManager := mkArenaManager {
  arenas : list Arena;     (** List of arenas *)
  active_arena : nat;      (** Index of active arena for new allocations *)
  arena_size : nat         (** Size for new arena creation *)
}.

(** ** Core Invariant *)

(** The fundamental safety invariant:
    1. arenas list is never empty
    2. active_arena is a valid index into arenas

    When this invariant holds, next_slot() and other operations
    that access arenas[active_arena] will never panic. *)
Definition arena_manager_valid (mgr : ArenaManager) : Prop :=
  length (arenas mgr) > 0 /\
  active_arena mgr < length (arenas mgr).

(** Boolean version for computational use *)
Definition arena_manager_valid_b (mgr : ArenaManager) : bool :=
  (0 <? length (arenas mgr)) && (active_arena mgr <? length (arenas mgr)).

(** Equivalence of boolean and propositional versions *)
Lemma arena_manager_valid_reflect : forall mgr,
  arena_manager_valid_b mgr = true <-> arena_manager_valid mgr.
Proof.
  intros mgr.
  unfold arena_manager_valid_b, arena_manager_valid.
  rewrite andb_true_iff.
  rewrite 2 Nat.ltb_lt.
  reflexivity.
Qed.

(** ** State Transitions *)

(** Initial state - always satisfies invariant *)
Definition new_manager (arena_cap : nat) : ArenaManager := {|
  arenas := [new_arena 0 arena_cap];
  active_arena := 0;
  arena_size := arena_cap
|}.

(** Clear for normal reset - maintains invariant *)
Definition clear (mgr : ArenaManager) : ArenaManager := {|
  arenas := [new_arena 0 (arena_size mgr)];
  active_arena := 0;
  arena_size := arena_size mgr
|}.

(** Clear for loading - FIXED version that maintains invariant
    Keeps a fallback arena so that if load_arena() fails,
    operations like next_slot() won't panic. *)
Definition clear_for_loading_FIXED (mgr : ArenaManager) : ArenaManager := {|
  arenas := [new_arena 0 (arena_size mgr)];  (* Keep one fallback arena *)
  active_arena := 0;
  arena_size := arena_size mgr
|}.

(** The BUGGY original implementation - empties arenas completely *)
Definition clear_for_loading_BUGGY (mgr : ArenaManager) : ArenaManager := {|
  arenas := [];           (* EMPTY! This is the bug. *)
  active_arena := 0;      (* Points to invalid index! *)
  arena_size := arena_size mgr
|}.

(** Load an arena from disk - appends to arenas list *)
Definition load_arena (mgr : ArenaManager) (a : Arena) : ArenaManager := {|
  arenas := arenas mgr ++ [a];
  active_arena := active_arena mgr;  (* Don't change active yet *)
  arena_size := arena_size mgr
|}.

(** Set active arena after loading - bounds-checked *)
Definition set_active_arena (mgr : ArenaManager) (idx : nat) : ArenaManager :=
  if idx <? length (arenas mgr) then
    {| arenas := arenas mgr;
       active_arena := idx;
       arena_size := arena_size mgr |}
  else if 0 <? length (arenas mgr) then
    {| arenas := arenas mgr;
       active_arena := length (arenas mgr) - 1;
       arena_size := arena_size mgr |}
  else
    mgr.  (* No change if somehow empty - shouldn't happen with invariant *)

(** Ensure valid - recovery function that establishes invariant from any state *)
Definition ensure_valid (mgr : ArenaManager) : ArenaManager :=
  if 0 <? length (arenas mgr) then
    if active_arena mgr <? length (arenas mgr) then
      mgr  (* Already valid *)
    else
      {| arenas := arenas mgr;
         active_arena := length (arenas mgr) - 1;
         arena_size := arena_size mgr |}
  else
    new_manager (arena_size mgr).  (* Completely reset *)

(** ** Operations that require invariant *)

(** Get next slot - REQUIRES invariant to not fail *)
Definition next_slot (mgr : ArenaManager) : option ArenaSlot :=
  match nth_error (arenas mgr) (active_arena mgr) with
  | Some arena => Some (mkArenaSlot (active_arena mgr) (node_count arena))
  | None => None
  end.

(** Defensive next_slot - never fails, returns fallback on invalid state *)
Definition next_slot_defensive (mgr : ArenaManager) : ArenaSlot :=
  match nth_error (arenas mgr) (active_arena mgr) with
  | Some arena => mkArenaSlot (active_arena mgr) (node_count arena)
  | None => mkArenaSlot 0 0  (* Fallback for invalid state *)
  end.

(** ** Invariant Preservation Theorems *)

(** Theorem: new_manager establishes invariant *)
Theorem new_manager_valid : forall cap,
  cap > 0 ->
  arena_manager_valid (new_manager cap).
Proof.
  intros cap Hcap.
  unfold arena_manager_valid, new_manager. simpl.
  split; lia.
Qed.

(** Theorem: clear preserves invariant *)
Theorem clear_preserves_valid : forall mgr,
  arena_manager_valid mgr ->
  arena_manager_valid (clear mgr).
Proof.
  intros mgr [Hlen Hactive].
  unfold arena_manager_valid, clear. simpl.
  split; lia.
Qed.

(** Theorem: clear_for_loading_FIXED preserves invariant *)
Theorem clear_for_loading_fixed_valid : forall mgr,
  arena_manager_valid mgr ->
  arena_manager_valid (clear_for_loading_FIXED mgr).
Proof.
  intros mgr [Hlen Hactive].
  unfold arena_manager_valid, clear_for_loading_FIXED. simpl.
  split; lia.
Qed.

(** Theorem: load_arena preserves invariant *)
Theorem load_arena_preserves_valid : forall mgr a,
  arena_manager_valid mgr ->
  arena_manager_valid (load_arena mgr a).
Proof.
  intros mgr a [Hlen Hactive].
  unfold arena_manager_valid, load_arena. simpl.
  rewrite length_app. simpl.
  split; lia.
Qed.

(** Theorem: set_active_arena preserves invariant *)
Theorem set_active_arena_preserves_valid : forall mgr idx,
  arena_manager_valid mgr ->
  arena_manager_valid (set_active_arena mgr idx).
Proof.
  intros mgr idx [Hlen Hactive].
  unfold arena_manager_valid, set_active_arena.
  destruct (idx <? length (arenas mgr)) eqn:E1.
  - simpl. apply Nat.ltb_lt in E1. split; lia.
  - destruct (0 <? length (arenas mgr)) eqn:E2.
    + simpl. apply Nat.ltb_lt in E2. split; lia.
    + simpl. split; assumption.
Qed.

(** Theorem: ensure_valid establishes invariant from any state *)
Theorem ensure_valid_establishes_invariant : forall mgr,
  arena_size mgr > 0 ->
  arena_manager_valid (ensure_valid mgr).
Proof.
  intros mgr Hsize.
  unfold ensure_valid.
  destruct (0 <? length (arenas mgr)) eqn:E1.
  - apply Nat.ltb_lt in E1.
    destruct (active_arena mgr <? length (arenas mgr)) eqn:E2.
    + apply Nat.ltb_lt in E2.
      unfold arena_manager_valid. split; lia.
    + apply Nat.ltb_ge in E2.
      unfold arena_manager_valid. simpl. split; lia.
  - apply Nat.ltb_ge in E1.
    apply new_manager_valid. assumption.
Qed.

(** ** Safety Theorems *)

(** Theorem: next_slot never fails when invariant holds *)
Theorem next_slot_safe : forall mgr,
  arena_manager_valid mgr ->
  exists slot, next_slot mgr = Some slot.
Proof.
  intros mgr [Hlen Hactive].
  unfold next_slot.
  (* We know active_arena < length arenas *)
  (* Use nth_error_nth' to get the element *)
  assert (Hnth: nth_error (arenas mgr) (active_arena mgr) <> None).
  { apply nth_error_Some. lia. }
  destruct (nth_error (arenas mgr) (active_arena mgr)) as [a|] eqn:Ha.
  - exists (mkArenaSlot (active_arena mgr) (node_count a)).
    reflexivity.
  - contradiction.
Qed.

(** Theorem: defensive next_slot is total - always returns a value *)
Theorem next_slot_defensive_total : forall mgr,
  exists slot, next_slot_defensive mgr = slot.
Proof.
  intros mgr.
  unfold next_slot_defensive.
  destruct (nth_error (arenas mgr) (active_arena mgr)); eauto.
Qed.

(** Theorem: defensive and normal agree when invariant holds *)
Theorem defensive_matches_when_valid : forall mgr,
  arena_manager_valid mgr ->
  next_slot mgr = Some (next_slot_defensive mgr).
Proof.
  intros mgr Hvalid.
  destruct Hvalid as [Hlen Hactive].
  unfold next_slot, next_slot_defensive.
  destruct (nth_error (arenas mgr) (active_arena mgr)) eqn:E.
  - reflexivity.
  - (* Contradiction: invariant says index is valid *)
    exfalso.
    apply nth_error_None in E.
    lia.
Qed.

(** ** Bug Demonstration *)

(** Theorem: buggy clear_for_loading breaks invariant *)
Theorem buggy_clear_breaks_invariant : forall mgr,
  ~arena_manager_valid (clear_for_loading_BUGGY mgr).
Proof.
  intros mgr [Hlen Hactive].
  unfold clear_for_loading_BUGGY in *. simpl in *.
  lia.  (* 0 > 0 is false *)
Qed.

(** Corollary: next_slot fails after buggy clear *)
Corollary next_slot_fails_after_buggy_clear : forall mgr,
  next_slot (clear_for_loading_BUGGY mgr) = None.
Proof.
  intros mgr.
  unfold next_slot, clear_for_loading_BUGGY. simpl.
  reflexivity.
Qed.

(** ** Composite Theorem *)

(** All safe operations preserve the invariant *)
Theorem all_safe_operations_preserve_valid :
  (forall cap, cap > 0 -> arena_manager_valid (new_manager cap)) /\
  (forall mgr, arena_manager_valid mgr -> arena_manager_valid (clear mgr)) /\
  (forall mgr, arena_manager_valid mgr -> arena_manager_valid (clear_for_loading_FIXED mgr)) /\
  (forall mgr a, arena_manager_valid mgr -> arena_manager_valid (load_arena mgr a)) /\
  (forall mgr idx, arena_manager_valid mgr -> arena_manager_valid (set_active_arena mgr idx)) /\
  (forall mgr, arena_size mgr > 0 -> arena_manager_valid (ensure_valid mgr)).
Proof.
  split; [apply new_manager_valid |].
  split; [apply clear_preserves_valid |].
  split; [apply clear_for_loading_fixed_valid |].
  split; [apply load_arena_preserves_valid |].
  split; [apply set_active_arena_preserves_valid |].
  apply ensure_valid_establishes_invariant.
Qed.

(** ** Additional Properties *)

(** Idempotence: ensure_valid on valid manager is identity *)
Lemma ensure_valid_idempotent : forall mgr,
  arena_manager_valid mgr ->
  ensure_valid mgr = mgr.
Proof.
  intros mgr [Hlen Hactive].
  unfold ensure_valid.
  destruct (0 <? length (arenas mgr)) eqn:E1.
  - destruct (active_arena mgr <? length (arenas mgr)) eqn:E2.
    + reflexivity.
    + apply Nat.ltb_ge in E2. lia.
  - apply Nat.ltb_ge in E1. lia.
Qed.

(** ensure_valid is idempotent (general case) *)
Lemma ensure_valid_twice : forall mgr,
  arena_size mgr > 0 ->
  ensure_valid (ensure_valid mgr) = ensure_valid mgr.
Proof.
  intros mgr Hsize.
  pose proof (ensure_valid_establishes_invariant mgr Hsize) as Hvalid.
  apply ensure_valid_idempotent. exact Hvalid.
Qed.

(** Arena count is preserved by set_active_arena *)
Lemma set_active_preserves_arena_count : forall mgr idx,
  length (arenas (set_active_arena mgr idx)) = length (arenas mgr).
Proof.
  intros mgr idx.
  unfold set_active_arena.
  destruct (idx <? length (arenas mgr)); simpl; try reflexivity.
  destruct (0 <? length (arenas mgr)); simpl; reflexivity.
Qed.

(** Load arena increases arena count *)
Lemma load_arena_increases_count : forall mgr a,
  length (arenas (load_arena mgr a)) = length (arenas mgr) + 1.
Proof.
  intros mgr a.
  unfold load_arena. simpl.
  rewrite length_app. simpl. lia.
Qed.
