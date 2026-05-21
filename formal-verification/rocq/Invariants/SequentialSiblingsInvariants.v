(** * SequentialSiblingsInvariants: Safety Invariants for Sequential Siblings

    This module proves that the sequential siblings encoding/decoding
    preserves the fundamental arena safety invariant:

        All slot IDs must be less than arena.node_count

    This invariant prevents the panic:
        "Invalid slot ID 4849 (arena has 4726 nodes)"

    The proofs establish:
    1. Encoding-time check is necessary and sufficient
    2. Decode-time check is defense-in-depth
    3. Both together provide strongest guarantee
*)

From ARTrie Require Import Model.ArenaManager.
From ARTrie Require Import Model.SequentialSiblings.
From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Import ListNotations.

Opaque u32_max.

(** ** Invariant Definitions *)

(** Arena slot validity: slot_id < node_count *)
Definition slot_valid_for_arena (slot : ArenaSlot) (arena : Arena) : Prop :=
  slot_id slot < node_count arena.

(** All slots in a list are valid for the arena *)
Definition all_slots_valid (slots : list ArenaSlot) (arena : Arena) : Prop :=
  Forall (fun s => slot_valid_for_arena s arena) slots.

(** Sequential encoding is valid for an arena *)
Definition encoding_valid_for_arena (enc : SequentialEncoding) (arena : Arena) : Prop :=
  child_count enc = 0 \/
  (child_count enc > 0 /\
   slot_id (first_slot enc) + child_count enc - 1 < node_count arena).

(** ** Core Invariant Preservation *)

(** Theorem: If encoding is valid, all decoded slots are valid *)
Theorem valid_encoding_produces_valid_slots : forall enc arena slots,
  encoding_valid_for_arena enc arena ->
  decode_sequential_FIXED enc (node_count arena) = Some slots ->
  all_slots_valid slots arena.
Proof.
  intros enc arena slots Henc Hdecode.
  unfold all_slots_valid.
  apply Forall_forall.
  intros slot Hin.
  unfold slot_valid_for_arena.
  eapply fixed_decode_valid_on_success; eauto.
Qed.

(** Theorem: Buggy decode can produce invalid slots *)
Theorem buggy_decode_can_produce_invalid : exists enc arena,
  let slots := decode_sequential_BUGGY enc in
  ~all_slots_valid slots arena.
Proof.
  (* Example: first_slot=4, count=3, arena has 5 nodes *)
  (* Generates slots 4, 5, 6 but arena only has indices 0-4 *)
  exists (mkSequentialEncoding (mkArenaSlot 0 4) 3).
  exists (mkArena 0 5 100 None).
  simpl.
  unfold all_slots_valid, slot_valid_for_arena.
  intro Hvalid.
  apply Forall_forall with (x := mkArenaSlot 0 6) in Hvalid.
  - simpl in Hvalid. lia.
  - simpl. right. right. left. reflexivity.
Qed.

(** ** Encoding Check Correctness *)

(** Theorem: check_sequential_valid correctly validates encoding *)
Theorem check_valid_correct : forall first count arena_size,
  check_sequential_valid first count arena_size = true <->
  (count >= 2 /\ first + count - 1 < arena_size /\ first + count - 1 <= u32_max).
Proof.
  intros first count arena_size.
  unfold check_sequential_valid.
  split.
  - intros H.
    apply andb_prop in H.
    destruct H as [H12 H3].
    apply andb_prop in H12.
    destruct H12 as [H1 H2].
    apply Nat.leb_le in H1.
    apply Nat.ltb_lt in H2.
    apply Nat.leb_le in H3.
    lia.
  - intros [H1 [H2 H3]].
    apply andb_true_intro.
    split.
    + apply andb_true_intro.
      split.
      * apply Nat.leb_le. lia.
      * apply Nat.ltb_lt. lia.
    + apply Nat.leb_le. lia.
Qed.

(** Corollary: check_sequential_valid ensures decode will succeed *)
Corollary check_ensures_decode_success : forall first count arena_size,
  check_sequential_valid first count arena_size = true ->
  count > 0 ->
  exists slots,
    decode_sequential_FIXED
      (mkSequentialEncoding (mkArenaSlot 0 first) count)
      arena_size = Some slots.
Proof.
  intros first count arena_size Hcheck Hcount.
  apply check_valid_correct in Hcheck.
  destruct Hcheck as [_ [Hbounds Hoverflow]].
  unfold decode_sequential_FIXED.
  simpl.
  destruct (count =? 0) eqn:Ecount.
  - apply Nat.eqb_eq in Ecount. lia.
  - destruct ((first + count - 1 <=? u32_max) &&
              (first + count - 1 <? arena_size)) eqn:E.
    + eexists. reflexivity.
    + apply andb_false_iff in E.
      destruct E as [E | E].
      * apply Nat.leb_gt in E. lia.
      * apply Nat.ltb_ge in E. lia.
Qed.

(** ** Defense in Depth *)

(** Even if encoding check is bypassed, decode check catches invalid *)
Theorem decode_check_catches_invalid : forall enc arena_size,
  child_count enc > 0 ->
  slot_id (first_slot enc) + child_count enc - 1 >= arena_size ->
  decode_sequential_FIXED enc arena_size = None.
Proof.
  intros enc arena_size Hcount Hinvalid.
  apply fixed_decode_rejects_invalid; assumption.
Qed.

(** If both checks are bypassed (impossible with fixes), we get invalid slots *)
Theorem no_checks_leads_to_invalid :
  let enc := mkSequentialEncoding (mkArenaSlot 0 10) 5 in
  let arena_size := 12 in  (* Only 12 nodes, but slots go to 14 *)
  let slots := decode_sequential_BUGGY enc in
  exists s, In s slots /\ slot_id s >= arena_size.
Proof.
  simpl.
  exists (mkArenaSlot 0 14).
  split.
  - (* 14 is in generated slots *)
    simpl. right. right. right. right. left. reflexivity.
  - (* 14 >= 12 *)
    simpl. lia.
Qed.

(** ** Combined Invariant Theorems *)

(** Theorem: Fixed system maintains slot validity invariant *)
Theorem fixed_system_maintains_invariant : forall enc arena,
  child_count enc > 0 ->
  (* If encoding check passes... *)
  check_sequential_valid (slot_id (first_slot enc)) (child_count enc) (node_count arena) = true ->
  (* ...then decode succeeds and all slots are valid *)
  exists slots,
    decode_sequential_FIXED enc (node_count arena) = Some slots /\
    all_slots_valid slots arena.
Proof.
  intros enc arena Hcount Hcheck.
  (* Decode will succeed *)
  assert (Hsuccess: exists slots,
    decode_sequential_FIXED enc (node_count arena) = Some slots).
  {
    unfold decode_sequential_FIXED.
    destruct (child_count enc =? 0) eqn:Ec.
    - apply Nat.eqb_eq in Ec. lia.
    - apply check_valid_correct in Hcheck.
      destruct Hcheck as [_ [Hbounds Hoverflow]].
      destruct ((slot_id (first_slot enc) + child_count enc - 1 <=? u32_max) &&
                (slot_id (first_slot enc) + child_count enc - 1 <? node_count arena)) eqn:E.
      + eexists. reflexivity.
      + apply andb_false_iff in E.
        destruct E as [E | E].
        * apply Nat.leb_gt in E. lia.
        * apply Nat.ltb_ge in E. lia.
  }
  destruct Hsuccess as [slots Hdecode].
  exists slots.
  split.
  - assumption.
  - (* All slots are valid *)
    unfold all_slots_valid.
    apply Forall_forall.
    intros slot Hin.
    unfold slot_valid_for_arena.
    eapply fixed_decode_valid_on_success; eauto.
Qed.

(** ** Specific Bug Scenario Proofs *)

(** Representative bug scenario with smaller numbers for efficient proof checking.
    The original bug had first_slot=4726, count=124, arena_size=4726.
    We use first_slot=10, count=5, arena_size=10 which exhibits the same pattern:
    - first_slot (10) equals arena_size (10)
    - Generated slots [10,11,12,13,14] all exceed arena bounds
    This is isomorphic to the original bug. *)
Definition bug_scenario_enc : SequentialEncoding :=
  mkSequentialEncoding (mkArenaSlot 0 10) 5.

Definition bug_scenario_arena : Arena :=
  mkArena 0 10 100 (Some 1).

(** Theorem: Bug scenario violates invariant with buggy code *)
Theorem bug_scenario_violates_invariant :
  let slots := decode_sequential_BUGGY bug_scenario_enc in
  ~all_slots_valid slots bug_scenario_arena.
Proof.
  simpl.
  unfold all_slots_valid, slot_valid_for_arena.
  intro Hvalid.
  (* Slot 10 is generated (first slot) and equals node_count *)
  apply Forall_forall with (x := mkArenaSlot 0 10) in Hvalid.
  - simpl in Hvalid. lia.  (* 10 < 10 is false *)
  - (* 10 is in the generated list *)
    unfold decode_sequential_BUGGY. simpl.
    left. reflexivity.
Qed.

(** Theorem: Fixed code rejects bug scenario *)
Theorem fixed_rejects_bug_scenario :
  check_sequential_valid 10 5 10 = false.
Proof.
  reflexivity.
Qed.

(** Theorem: Fixed decode also rejects bug scenario *)
Theorem fixed_decode_rejects_bug_scenario :
  decode_sequential_FIXED bug_scenario_enc 10 = None.
Proof.
  unfold decode_sequential_FIXED, bug_scenario_enc.
  cbn [child_count first_slot slot_id arena_id].
  change (5 =? 0) with false.
  change (10 + 5 - 1) with 14.
  destruct (14 <=? u32_max); reflexivity.
Qed.

(** ** Summary Theorem *)

(** The key safety property: with the fixes applied, the invariant
    "all decoded slot IDs < arena.node_count" is always maintained. *)
Theorem sequential_siblings_safety :
  (* For any encoding and arena... *)
  forall enc arena,
  (* Either: (1) encoding check fails, preventing sequential encoding *)
  check_sequential_valid (slot_id (first_slot enc)) (child_count enc) (node_count arena) = false
  \/
  (* Or: (2) check passes AND decode succeeds with valid slots *)
  (check_sequential_valid (slot_id (first_slot enc)) (child_count enc) (node_count arena) = true /\
   (child_count enc = 0 \/
    exists slots,
      decode_sequential_FIXED enc (node_count arena) = Some slots /\
      all_slots_valid slots arena)).
Proof.
  intros enc arena.
  destruct (check_sequential_valid (slot_id (first_slot enc))
              (child_count enc) (node_count arena)) eqn:Hcheck.
  - (* Check passes *)
    right. split.
    + reflexivity.
    + destruct (child_count enc) eqn:Hcount.
      * left. reflexivity.
      * right.
        assert (Hgt: child_count enc > 0) by lia.
        eapply fixed_system_maintains_invariant.
        -- exact Hgt.
        -- rewrite Hcount. exact Hcheck.
  - (* Check fails *)
    left. reflexivity.
Qed.
