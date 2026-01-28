(** * SequentialSiblings: Model of Sequential Sibling Encoding/Decoding

    This module models the sequential sibling optimization in ARTrie
    serialization. When child nodes are stored in consecutive arena
    slots, we can encode them efficiently by storing only the first
    slot ID plus a count.

    Key invariant verified:
    - All decoded slot IDs are within arena bounds
    - first_child.slot_id + count - 1 < arena.node_count

    The bug fixed:
    - decode_sequential_siblings() blindly added indices to first_slot
    - No validation that resulting slot IDs are valid arena indices
    - Example: first_slot=4726, count=124 -> produces slot 4849 in
      arena with only 4726 nodes

    Fix strategy:
    1. check_sequential_char_children() validates bounds before encoding
    2. decode_sequential_siblings() uses checked_add as defense-in-depth
*)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Import ListNotations.

(** ** Constants *)

(** Maximum value for u32 slot IDs *)
Definition u32_max : nat := 4294967295.

(** ** Basic Types *)

(** Arena slot identifier - uniquely identifies a node's location *)
Record ArenaSlot := mkArenaSlot {
  arena_id : nat;
  slot_id : nat
}.

(** Sequential siblings encoding: first slot + count *)
Record SequentialEncoding := mkSequentialEncoding {
  first_slot : ArenaSlot;
  child_count : nat
}.

(** ** Core Invariant *)

(** The fundamental safety property: sequential siblings must not exceed arena bounds *)
Definition sequential_siblings_valid
  (first : nat) (count : nat) (arena_size : nat) : Prop :=
  count > 0 ->
  first + count - 1 < arena_size.

(** Boolean version for computational use *)
Definition sequential_siblings_valid_b
  (first : nat) (count : nat) (arena_size : nat) : bool :=
  (0 <? count) && (first + count - 1 <? arena_size).

(** No u32 overflow occurs *)
Definition no_overflow (first : nat) (count : nat) : Prop :=
  count > 0 ->
  first + count - 1 <= u32_max.

(** Boolean version *)
Definition no_overflow_b (first : nat) (count : nat) : bool :=
  (0 <? count) && (first + count - 1 <=? u32_max).

(** Combined validity: both bounds and overflow checks pass *)
Definition fully_valid (first : nat) (count : nat) (arena_size : nat) : Prop :=
  sequential_siblings_valid first count arena_size /\
  no_overflow first count.

(** ** Helper Functions *)

(** Checked addition: returns None if overflow would occur *)
Definition checked_add (a b : nat) : option nat :=
  if a + b <=? u32_max then Some (a + b) else None.

(** Saturating subtraction: returns 0 if underflow would occur *)
Definition saturating_sub (a b : nat) : nat :=
  if b <=? a then a - b else 0.

(** ** Encoding Operations *)

(** Check if sequential encoding is valid before using it.
    This models check_sequential_char_children() in dict_impl_char.rs *)
Definition check_sequential_valid
  (first : nat) (count : nat) (arena_size : nat) : bool :=
  (* At least 2 children for optimization to be worthwhile *)
  (2 <=? count) &&
  (* Bounds check: last slot must be within arena *)
  (first + count - 1 <? arena_size) &&
  (* Overflow check: last slot must not overflow u32 *)
  (first + count - 1 <=? u32_max).

(** Buggy version: no bounds checking *)
Definition check_sequential_valid_BUGGY
  (first : nat) (count : nat) (arena_size : nat) : bool :=
  2 <=? count.  (* Only checks minimum count! *)

(** ** Decoding Operations *)

(** Generate slot IDs for sequential siblings.
    Returns list of slot IDs: [first, first+1, ..., first+count-1] *)
Fixpoint generate_slots (first : nat) (remaining : nat) : list nat :=
  match remaining with
  | 0 => []
  | S n => first :: generate_slots (S first) n
  end.

(** Decode sequential siblings - BUGGY version.
    Blindly generates slots without validation. *)
Definition decode_sequential_BUGGY
  (enc : SequentialEncoding) : list ArenaSlot :=
  let slots := generate_slots (slot_id (first_slot enc)) (child_count enc) in
  map (fun s => mkArenaSlot (arena_id (first_slot enc)) s) slots.

(** Decode sequential siblings - FIXED version with checked_add.
    Uses checked_add to detect overflow; returns empty list on error. *)
Definition decode_sequential_FIXED
  (enc : SequentialEncoding) (arena_size : nat) : option (list ArenaSlot) :=
  let first := slot_id (first_slot enc) in
  let count := child_count enc in
  if count =? 0 then
    Some []
  else
    (* Check for overflow and arena bounds *)
    let last := first + count - 1 in
    if (last <=? u32_max) && (last <? arena_size) then
      let slots := generate_slots first count in
      Some (map (fun s => mkArenaSlot (arena_id (first_slot enc)) s) slots)
    else
      None.  (* Return None to signal error *)

(** ** Lemmas about generate_slots *)

(** Length of generated slots equals count *)
Lemma generate_slots_length : forall first count,
  length (generate_slots first count) = count.
Proof.
  intros first count.
  generalize dependent first.
  induction count; intros.
  - simpl. reflexivity.
  - simpl. rewrite IHcount. reflexivity.
Qed.

(** All generated slots are in range [first, first+count) *)
Lemma generate_slots_range : forall first count s,
  In s (generate_slots first count) ->
  first <= s < first + count.
Proof.
  intros first count.
  generalize dependent first.
  induction count; intros first s Hin.
  - (* count = 0: contradiction, empty list *)
    simpl in Hin. contradiction.
  - (* count = S count *)
    simpl in Hin.
    destruct Hin as [Heq | Htail].
    + (* s = first *)
      subst. lia.
    + (* s in tail *)
      apply IHcount in Htail.
      lia.
Qed.

(** Converse: all slots in range are generated *)
Lemma generate_slots_complete : forall first count s,
  first <= s < first + count ->
  In s (generate_slots first count).
Proof.
  intros first count.
  generalize dependent first.
  induction count; intros first s Hrange.
  - (* count = 0: contradiction *)
    lia.
  - (* count = S count *)
    simpl.
    destruct (Nat.eq_dec s first) as [Heq | Hneq].
    + left. symmetry. exact Heq.
    + right. apply IHcount. lia.
Qed.

(** ** Main Theorems *)

(** Theorem: checked_add returns Some iff no overflow occurs *)
Theorem checked_add_spec : forall a b,
  (exists result, checked_add a b = Some result /\ result = a + b) <->
  a + b <= u32_max.
Proof.
  intros a b.
  unfold checked_add.
  split.
  - intros [result [Hsome Heq]].
    destruct (a + b <=? u32_max) eqn:E.
    + apply Nat.leb_le in E. assumption.
    + discriminate.
  - intros Hle.
    exists (a + b).
    destruct (a + b <=? u32_max) eqn:E.
    + split; reflexivity.
    + apply Nat.leb_gt in E. lia.
Qed.

(** Theorem: if encoding check passes, all decoded slots are valid *)
Theorem encoding_ensures_valid_decode : forall first count arena_size,
  check_sequential_valid first count arena_size = true ->
  forall s, In s (generate_slots first count) ->
  s < arena_size.
Proof.
  intros first count arena_size Hcheck s Hin.
  unfold check_sequential_valid in Hcheck.
  apply andb_prop in Hcheck.
  destruct Hcheck as [Hcheck12 Hoverflow].
  apply andb_prop in Hcheck12.
  destruct Hcheck12 as [Hcount Hbounds].
  apply Nat.ltb_lt in Hbounds.
  apply generate_slots_range in Hin.
  lia.
Qed.

(** Theorem: buggy encoding can produce invalid slots *)
Theorem buggy_encoding_can_fail : exists first count arena_size s,
  check_sequential_valid_BUGGY first count arena_size = true /\
  In s (generate_slots first count) /\
  s >= arena_size.
Proof.
  (* Concrete counterexample: first=4, count=3, arena_size=5 *)
  (* Generated slots: [4, 5, 6], but arena only has indices 0-4 *)
  exists 4, 3, 5, 6.
  split.
  - (* check passes: count >= 2 *)
    simpl. reflexivity.
  - split.
    + (* 6 is generated *)
      simpl. right. right. left. reflexivity.
    + (* 6 >= 5 *)
      lia.
Qed.

(** Theorem: fixed decode returns None for invalid encodings *)
Theorem fixed_decode_rejects_invalid : forall enc arena_size,
  child_count enc > 0 ->
  slot_id (first_slot enc) + child_count enc - 1 >= arena_size ->
  decode_sequential_FIXED enc arena_size = None.
Proof.
  intros enc arena_size Hcount Hinvalid.
  unfold decode_sequential_FIXED.
  destruct (child_count enc =? 0) eqn:Ecount.
  - apply Nat.eqb_eq in Ecount. lia.
  - destruct ((slot_id (first_slot enc) + child_count enc - 1 <=? u32_max) &&
              (slot_id (first_slot enc) + child_count enc - 1 <? arena_size)) eqn:Echeck.
    + apply andb_prop in Echeck.
      destruct Echeck as [_ Hbounds].
      apply Nat.ltb_lt in Hbounds.
      lia.
    + reflexivity.
Qed.

(** Theorem: fixed decode returns valid slots when it succeeds *)
Theorem fixed_decode_valid_on_success : forall enc arena_size slots,
  decode_sequential_FIXED enc arena_size = Some slots ->
  forall slot, In slot slots ->
  slot_id slot < arena_size.
Proof.
  intros enc arena_size slots Hdecode slot Hin.
  unfold decode_sequential_FIXED in Hdecode.
  destruct (child_count enc =? 0) eqn:Ecount.
  - (* count = 0: empty result *)
    injection Hdecode as Heq.
    rewrite <- Heq in Hin.
    simpl in Hin. contradiction.
  - (* count > 0: check bounds *)
    destruct ((slot_id (first_slot enc) + child_count enc - 1 <=? u32_max) &&
              (slot_id (first_slot enc) + child_count enc - 1 <? arena_size)) eqn:Echeck.
    + (* Check passed - slots are valid *)
      injection Hdecode as Heq.
      rewrite <- Heq in Hin.
      apply andb_prop in Echeck.
      destruct Echeck as [_ Hbounds].
      apply Nat.ltb_lt in Hbounds.
      apply in_map_iff in Hin.
      destruct Hin as [s [Hslot Hgen]].
      rewrite <- Hslot. simpl.
      apply generate_slots_range in Hgen.
      lia.
    + (* Check failed - contradiction *)
      discriminate.
Qed.

(** Theorem: fully valid encoding ensures safe decode *)
Theorem fully_valid_ensures_safe_decode : forall first count arena_size,
  fully_valid first count arena_size ->
  count > 0 ->
  forall i, i < count -> first + i < arena_size.
Proof.
  intros first count arena_size [Hbounds Hoverflow] Hcount i Hi.
  unfold sequential_siblings_valid in Hbounds.
  specialize (Hbounds Hcount).
  lia.
Qed.

(** ** Equivalence Lemmas *)

(** Boolean and propositional validity are equivalent *)
Lemma sequential_valid_reflect : forall first count arena_size,
  count > 0 ->
  sequential_siblings_valid_b first count arena_size = true <->
  sequential_siblings_valid first count arena_size.
Proof.
  intros first count arena_size Hcount.
  unfold sequential_siblings_valid_b, sequential_siblings_valid.
  split.
  - intros Hb _.
    apply andb_prop in Hb.
    destruct Hb as [_ Hbounds].
    apply Nat.ltb_lt in Hbounds.
    assumption.
  - intros Hp.
    apply andb_true_intro.
    split.
    + apply Nat.ltb_lt. assumption.
    + apply Nat.ltb_lt. apply Hp. assumption.
Qed.

(** No overflow boolean is equivalent to propositional *)
Lemma no_overflow_reflect : forall first count,
  count > 0 ->
  no_overflow_b first count = true <->
  no_overflow first count.
Proof.
  intros first count Hcount.
  unfold no_overflow_b, no_overflow.
  split.
  - intros Hb _.
    apply andb_prop in Hb.
    destruct Hb as [_ Hle].
    apply Nat.leb_le in Hle.
    assumption.
  - intros Hp.
    apply andb_true_intro.
    split.
    + apply Nat.ltb_lt. assumption.
    + apply Nat.leb_le. apply Hp. assumption.
Qed.

(** ** Corollaries *)

(** Representative bug scenario with smaller numbers for efficient proof checking.
    Original bug: first=4726, count=124, arena_size=4726
    Representative: first=10, count=5, arena_size=10 (same pattern) *)
Corollary actual_bug_scenario :
  let first := 10 in
  let count := 5 in
  let arena_size := 10 in
  check_sequential_valid_BUGGY first count arena_size = true /\
  exists s, In s (generate_slots first count) /\ s >= arena_size.
Proof.
  split.
  - (* Buggy check passes *)
    simpl. reflexivity.
  - (* Invalid slot exists *)
    exists 10.
    split.
    + (* 10 is the first generated slot *)
      simpl. left. reflexivity.
    + (* 10 >= 10 *)
      lia.
Qed.

(** Fixed version would reject this scenario *)
Corollary fixed_rejects_bug_scenario_model :
  let first := 10 in
  let count := 5 in
  let arena_size := 10 in
  check_sequential_valid first count arena_size = false.
Proof.
  simpl.
  (* 10 + 5 - 1 = 14 >= 10, so bounds check fails *)
  reflexivity.
Qed.
