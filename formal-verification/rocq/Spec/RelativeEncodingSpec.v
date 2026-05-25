(** * RelativeEncodingSpec: Persistent child-pointer encoding laws

    This specification models the child-pointer encoding boundary used by the
    persistent byte and char ART serializers.  Same-arena children may use a
    compact relative delta only when the child slot is not after the parent.
    Otherwise the implementation must use full slot encoding so decoding is
    lossless.

    The checked decoder obligations are fail-closed: malformed or truncated
    byte streams reject instead of panicking, saturating, or fabricating slots.
*)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Import ListNotations.

(** ** Basic model *)

Definition u32_max : nat := Nat.pow 2 32 - 1.
Definition cross_arena_size : nat := 9.

Record ArenaSlot := mkArenaSlot {
  arena_id : nat;
  slot_id : nat
}.

Inductive PointerEncoding : Type :=
| RelativePointer (delta : nat)
| FullPointer (slot : ArenaSlot).

Definition same_arena (parent child : ArenaSlot) : bool :=
  arena_id parent =? arena_id child.

Definition strict_relative_delta
  (parent child : ArenaSlot) : option nat :=
  if same_arena parent child then
    if slot_id child <=? slot_id parent then
      Some (slot_id parent - slot_id child)
    else
      None
  else
    None.

Definition encode_pointer_lossless
  (parent child : ArenaSlot) : PointerEncoding :=
  if same_arena parent child then
    if slot_id child <=? slot_id parent then
      RelativePointer (slot_id parent - slot_id child)
    else
      FullPointer child
  else
    FullPointer child.

Definition decode_pointer
  (parent : ArenaSlot) (enc : PointerEncoding) : option ArenaSlot :=
  match enc with
  | FullPointer slot => Some slot
  | RelativePointer delta =>
      if delta <=? slot_id parent then
        Some (mkArenaSlot (arena_id parent) (slot_id parent - delta))
      else
        None
  end.

Definition encoded_size_model
  (parent child : ArenaSlot) : nat :=
  match encode_pointer_lossless parent child with
  | RelativePointer _ => 1
  | FullPointer _ => cross_arena_size
  end.

(** ** Pointer encoding theorems *)

Theorem strict_relative_roundtrip :
  forall parent child delta,
    strict_relative_delta parent child = Some delta ->
    decode_pointer parent (RelativePointer delta) = Some child.
Proof.
  intros [pa ps] [ca cs] delta Hdelta.
  unfold strict_relative_delta, decode_pointer, same_arena in *.
  simpl in *.
  destruct (pa =? ca) eqn:Harena; try discriminate.
  destruct (cs <=? ps) eqn:Hle; try discriminate.
  inversion Hdelta; subst delta; clear Hdelta.
  apply Nat.eqb_eq in Harena.
  apply Nat.leb_le in Hle.
  subst ca.
  destruct (ps - cs <=? ps) eqn:Hfits.
  - f_equal. f_equal. lia.
  - apply Nat.leb_gt in Hfits. lia.
Qed.

Theorem strict_relative_rejects_forward_same_arena :
  forall arena parent_slot child_slot,
    parent_slot < child_slot ->
    strict_relative_delta
      (mkArenaSlot arena parent_slot)
      (mkArenaSlot arena child_slot) = None.
Proof.
  intros arena parent_slot child_slot Hlt.
  unfold strict_relative_delta, same_arena.
  simpl.
  rewrite Nat.eqb_refl.
  destruct (child_slot <=? parent_slot) eqn:Hle.
  - apply Nat.leb_le in Hle. lia.
  - reflexivity.
Qed.

Theorem decode_relative_rejects_underflow :
  forall parent delta,
    slot_id parent < delta ->
    decode_pointer parent (RelativePointer delta) = None.
Proof.
  intros [pa ps] delta Hlt.
  unfold decode_pointer; simpl.
  simpl in Hlt.
  destruct (delta <=? ps) eqn:Hle.
  - apply Nat.leb_le in Hle. lia.
  - reflexivity.
Qed.

Theorem lossless_pointer_roundtrip :
  forall parent child,
    decode_pointer parent (encode_pointer_lossless parent child) = Some child.
Proof.
  intros [pa ps] [ca cs].
  unfold encode_pointer_lossless, decode_pointer, same_arena.
  simpl.
  destruct (pa =? ca) eqn:Harena.
  - apply Nat.eqb_eq in Harena. subst ca.
    destruct (cs <=? ps) eqn:Hle.
    + apply Nat.leb_le in Hle.
      destruct (ps - cs <=? ps) eqn:Hfits.
      * f_equal. f_equal. lia.
      * apply Nat.leb_gt in Hfits. lia.
    + reflexivity.
  - reflexivity.
Qed.

Theorem forward_same_arena_uses_full_size :
  forall arena parent_slot child_slot,
    parent_slot < child_slot ->
    encoded_size_model
      (mkArenaSlot arena parent_slot)
      (mkArenaSlot arena child_slot) = cross_arena_size.
Proof.
  intros arena parent_slot child_slot Hlt.
  unfold encoded_size_model, encode_pointer_lossless, same_arena.
  simpl.
  rewrite Nat.eqb_refl.
  destruct (child_slot <=? parent_slot) eqn:Hle.
  - apply Nat.leb_le in Hle. lia.
  - reflexivity.
Qed.

(** ** Fail-closed byte-level decode obligations *)

Definition checked_full_decode_consumes (input_len : nat) : option nat :=
  if cross_arena_size <=? input_len then Some cross_arena_size else None.

Definition checked_tagged_decode_consumes
  (input_len first_byte : nat) : option nat :=
  if input_len =? 0 then
    None
  else if first_byte =? 1 then
    checked_full_decode_consumes input_len
  else
    Some 1.

Theorem empty_input_rejects :
  forall first_byte,
    checked_tagged_decode_consumes 0 first_byte = None.
Proof.
  intros first_byte.
  unfold checked_tagged_decode_consumes.
  reflexivity.
Qed.

Theorem truncated_full_pointer_rejects :
  forall input_len,
    0 < input_len ->
    input_len < cross_arena_size ->
    checked_tagged_decode_consumes input_len 1 = None.
Proof.
  intros input_len Hpos Htrunc.
  unfold cross_arena_size in Htrunc.
  unfold checked_tagged_decode_consumes, checked_full_decode_consumes.
  destruct (input_len =? 0) eqn:Hzero.
  - apply Nat.eqb_eq in Hzero. lia.
  - rewrite Nat.eqb_refl.
    destruct (cross_arena_size <=? input_len) eqn:Hle.
    + apply Nat.leb_le in Hle. unfold cross_arena_size in Hle. lia.
    + reflexivity.
Qed.

Theorem full_pointer_consumes_exact_size :
  forall input_len,
    cross_arena_size <= input_len ->
    checked_tagged_decode_consumes input_len 1 = Some cross_arena_size.
Proof.
  intros input_len Hlen.
  unfold cross_arena_size in Hlen.
  unfold checked_tagged_decode_consumes, checked_full_decode_consumes.
  destruct (input_len =? 0) eqn:Hzero.
  - apply Nat.eqb_eq in Hzero. lia.
  - rewrite Nat.eqb_refl.
    destruct (cross_arena_size <=? input_len) eqn:Hle.
    + reflexivity.
    + apply Nat.leb_gt in Hle. unfold cross_arena_size in Hle. lia.
Qed.

(** ** Sequential sibling decode obligations *)

Fixpoint generate_slots (first : nat) (count : nat) : list nat :=
  match count with
  | 0 => []
  | S rest => first :: generate_slots (S first) rest
  end.

Definition checked_sequential_decode
  (first count : nat) : option (list nat) :=
  if count =? 0 then
    Some []
  else
    let last := first + count - 1 in
    if last <=? u32_max then
      Some (generate_slots first count)
    else
      None.

Lemma generate_slots_length :
  forall first count,
    length (generate_slots first count) = count.
Proof.
  intros first count.
  generalize dependent first.
  induction count; intros first; simpl.
  - reflexivity.
  - rewrite IHcount. reflexivity.
Qed.

Lemma generate_slots_range :
  forall first count slot,
    In slot (generate_slots first count) ->
    first <= slot < first + count.
Proof.
  intros first count.
  generalize dependent first.
  induction count; intros first slot Hin.
  - simpl in Hin. contradiction.
  - simpl in Hin. destruct Hin as [Heq | Hin].
    + subst. lia.
    + apply IHcount in Hin. lia.
Qed.

Theorem sequential_zero_count_decodes_empty :
  forall first,
    checked_sequential_decode first 0 = Some [].
Proof.
  reflexivity.
Qed.

Theorem sequential_decode_rejects_overflow :
  forall first count,
    count > 0 ->
    u32_max < first + count - 1 ->
    checked_sequential_decode first count = None.
Proof.
  intros first count Hcount Hover.
  unfold checked_sequential_decode.
  destruct (count =? 0) eqn:Hzero.
  - apply Nat.eqb_eq in Hzero. lia.
  - destruct (first + count - 1 <=? u32_max) eqn:Hle.
    + apply Nat.leb_le in Hle. lia.
    + reflexivity.
Qed.

Theorem sequential_decode_length :
  forall first count slots,
    checked_sequential_decode first count = Some slots ->
    length slots = count.
Proof.
  intros first count slots Hdecode.
  unfold checked_sequential_decode in Hdecode.
  destruct (count =? 0) eqn:Hzero.
  - inversion Hdecode; subst. apply Nat.eqb_eq in Hzero. subst count. reflexivity.
  - destruct (first + count - 1 <=? u32_max) eqn:Hle; try discriminate.
    inversion Hdecode; subst. apply generate_slots_length.
Qed.

Theorem sequential_decode_range :
  forall first count slots slot,
    checked_sequential_decode first count = Some slots ->
    In slot slots ->
    first <= slot < first + count.
Proof.
  intros first count slots slot Hdecode Hin.
  unfold checked_sequential_decode in Hdecode.
  destruct (count =? 0) eqn:Hzero.
  - inversion Hdecode; subst. simpl in Hin. contradiction.
  - destruct (first + count - 1 <=? u32_max) eqn:Hle; try discriminate.
    inversion Hdecode; subst.
    apply generate_slots_range. exact Hin.
Qed.

(** ** Dedup cache obligations *)

Definition Bytes := list nat.

Record CacheEntry := mkCacheEntry {
  cached_bytes : Bytes;
  cached_slot : ArenaSlot
}.

Inductive DedupOutcome : Type :=
| DedupHit (slot : ArenaSlot)
| DedupMiss (fresh : ArenaSlot)
| DedupCollision (fresh : ArenaSlot)
| DedupAssumedHit (slot : ArenaSlot).

Definition bytes_eq_dec : forall (left right : Bytes), {left = right} + {left <> right}.
Proof.
  decide equality.
  apply Nat.eq_dec.
Defined.

Definition allocate_dedup_model
  (verify_on_hit : bool)
  (lookup : nat -> option CacheEntry)
  (hash : nat)
  (bytes : Bytes)
  (fresh : ArenaSlot) : DedupOutcome :=
  match lookup hash with
  | None => DedupMiss fresh
  | Some entry =>
      if verify_on_hit then
        if bytes_eq_dec (cached_bytes entry) bytes then
          DedupHit (cached_slot entry)
        else
          DedupCollision fresh
      else
        DedupAssumedHit (cached_slot entry)
  end.

Definition collision_free_for
  (lookup : nat -> option CacheEntry)
  (hash : nat)
  (bytes : Bytes) : Prop :=
  forall entry,
    lookup hash = Some entry ->
    cached_bytes entry = bytes.

Theorem verified_hit_sound :
  forall lookup hash bytes fresh slot,
    allocate_dedup_model true lookup hash bytes fresh = DedupHit slot ->
    exists entry,
      lookup hash = Some entry /\
      cached_bytes entry = bytes /\
      cached_slot entry = slot.
Proof.
  intros lookup hash bytes fresh slot Halloc.
  unfold allocate_dedup_model in Halloc.
  destruct (lookup hash) as [entry |] eqn:Hlookup; try discriminate.
  destruct (bytes_eq_dec (cached_bytes entry) bytes) as [Heq | Hneq];
    try discriminate.
  inversion Halloc; subst.
  exists entry. repeat split; assumption.
Qed.

Theorem verified_collision_allocates_fresh :
  forall lookup hash cached bytes fresh,
    lookup hash = Some cached ->
    cached_bytes cached <> bytes ->
    allocate_dedup_model true lookup hash bytes fresh =
    DedupCollision fresh.
Proof.
  intros lookup hash cached bytes fresh Hlookup Hneq.
  unfold allocate_dedup_model.
  rewrite Hlookup.
  destruct (bytes_eq_dec (cached_bytes cached) bytes) as [Heq | Hne].
  - contradiction.
  - reflexivity.
Qed.

Theorem verified_miss_allocates_fresh :
  forall lookup hash bytes fresh,
    lookup hash = None ->
    allocate_dedup_model true lookup hash bytes fresh =
    DedupMiss fresh.
Proof.
  intros lookup hash bytes fresh Hlookup.
  unfold allocate_dedup_model.
  rewrite Hlookup.
  reflexivity.
Qed.

Theorem unverified_hit_sound_under_collision_free_assumption :
  forall lookup hash bytes fresh slot,
    collision_free_for lookup hash bytes ->
    allocate_dedup_model false lookup hash bytes fresh =
    DedupAssumedHit slot ->
    exists entry,
      lookup hash = Some entry /\
      cached_bytes entry = bytes /\
      cached_slot entry = slot.
Proof.
  intros lookup hash bytes fresh slot Hfree Halloc.
  unfold allocate_dedup_model in Halloc.
  destruct (lookup hash) as [entry |] eqn:Hlookup; try discriminate.
  inversion Halloc; subst.
  exists entry.
  repeat split; try assumption.
  apply Hfree. exact Hlookup.
Qed.
