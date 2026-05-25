(** * ArenaReservationSpec: arena slot-store and reservation laws

    This specification models the persistence-facing arena-manager obligations
    used by both byte and char persistent ART backends:

    - allocation writes an exact byte payload at a unique slot;
    - updates preserve slot identity and replace the payload exactly;
    - reserved allocations produce one contiguous same-arena slot range;
    - dirty-slot flushing clears dirty state only after successful persistence;
    - loading a persisted slot directory reconstructs the same slot payloads.

    Kernel, mmap, io_uring, and checksum implementations remain trusted below
    this boundary; this model captures the Rust arena-manager contract that the
    relative-encoding and recovery proofs rely on.
*)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Import ListNotations.

(** ** Basic slot-store model *)

Definition Bytes := list nat.

Record ArenaSlot := mkArenaSlot {
  arena_id : nat;
  slot_id : nat
}.

Definition slot_eq_dec :
  forall (left right : ArenaSlot), {left = right} + {left <> right}.
Proof.
  decide equality; apply Nat.eq_dec.
Defined.

Definition SlotStore := ArenaSlot -> option Bytes.

Definition empty_store : SlotStore := fun _ => None.

Definition read_slot (store : SlotStore) (slot : ArenaSlot) : option Bytes :=
  store slot.

Definition write_slot
  (store : SlotStore) (slot : ArenaSlot) (bytes : Bytes) : SlotStore :=
  fun query => if slot_eq_dec slot query then Some bytes else store query.

Definition advance_slot (slot : ArenaSlot) : ArenaSlot :=
  mkArenaSlot (arena_id slot) (S (slot_id slot)).

Record AllocationResult := mkAllocationResult {
  alloc_store : SlotStore;
  alloc_slot : ArenaSlot;
  alloc_next : ArenaSlot
}.

Definition allocate_model
  (store : SlotStore) (next : ArenaSlot) (bytes : Bytes) : AllocationResult :=
  mkAllocationResult (write_slot store next bytes) next (advance_slot next).

Theorem write_slot_read_same :
  forall store slot bytes,
    read_slot (write_slot store slot bytes) slot = Some bytes.
Proof.
  intros store slot bytes.
  unfold read_slot, write_slot.
  destruct (slot_eq_dec slot slot) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Theorem write_slot_read_other :
  forall store slot query bytes,
    slot <> query ->
    read_slot (write_slot store slot bytes) query = read_slot store query.
Proof.
  intros store slot query bytes Hneq.
  unfold read_slot, write_slot.
  destruct (slot_eq_dec slot query) as [Heq | _].
  - contradiction.
  - reflexivity.
Qed.

Theorem allocate_read_roundtrip :
  forall store next bytes result,
    allocate_model store next bytes = result ->
    read_slot (alloc_store result) (alloc_slot result) = Some bytes.
Proof.
  intros store next bytes result Halloc.
  subst result.
  unfold allocate_model. simpl.
  apply write_slot_read_same.
Qed.

Theorem allocate_advances_with_unique_next :
  forall store next bytes result,
    allocate_model store next bytes = result ->
    arena_id (alloc_next result) = arena_id next /\
    slot_id next < slot_id (alloc_next result) /\
    alloc_slot result <> alloc_next result.
Proof.
  intros store [next_arena next_slot] bytes result Halloc.
  subst result.
  unfold allocate_model, advance_slot. simpl.
  repeat split; try lia.
  intros Heq. inversion Heq. lia.
Qed.

Theorem update_replaces_exact_payload :
  forall store slot old_bytes new_bytes,
    read_slot store slot = Some old_bytes ->
    read_slot (write_slot store slot new_bytes) slot = Some new_bytes.
Proof.
  intros store slot old_bytes new_bytes _.
  apply write_slot_read_same.
Qed.

(** ** Reserved contiguous slot ranges *)

Record Reservation := mkReservation {
  res_arena : nat;
  res_first : nat;
  res_count : nat;
  res_next : nat
}.

Definition reserve_model (next : ArenaSlot) (count : nat) : option Reservation :=
  if count =? 0 then
    None
  else
    Some (mkReservation (arena_id next) (slot_id next) count 0).

Definition reserved_remaining (reservation : Reservation) : nat :=
  res_count reservation - res_next reservation.

Definition reservation_complete (reservation : Reservation) : bool :=
  res_count reservation <=? res_next reservation.

Definition current_reserved_slot (reservation : Reservation) : ArenaSlot :=
  mkArenaSlot
    (res_arena reservation)
    (res_first reservation + res_next reservation).

Record ReservedAllocationResult := mkReservedAllocationResult {
  reserved_store : SlotStore;
  reserved_slot : ArenaSlot;
  reserved_next : Reservation
}.

Definition allocate_reserved_model
  (store : SlotStore) (reservation : Reservation) (bytes : Bytes) :
  option ReservedAllocationResult :=
  if res_next reservation <? res_count reservation then
    let slot := current_reserved_slot reservation in
    Some (mkReservedAllocationResult
      (write_slot store slot bytes)
      slot
      (mkReservation
        (res_arena reservation)
        (res_first reservation)
        (res_count reservation)
        (S (res_next reservation))))
  else
    None.

Fixpoint generate_reserved_slots
  (arena first count : nat) : list ArenaSlot :=
  match count with
  | 0 => []
  | S rest =>
      mkArenaSlot arena first :: generate_reserved_slots arena (S first) rest
  end.

Theorem reserve_zero_rejects :
  forall next,
    reserve_model next 0 = None.
Proof.
  reflexivity.
Qed.

Theorem reserve_nonzero_initializes_contiguous_range :
  forall next count reservation,
    count > 0 ->
    reserve_model next count = Some reservation ->
    res_arena reservation = arena_id next /\
    res_first reservation = slot_id next /\
    res_count reservation = count /\
    res_next reservation = 0.
Proof.
  intros next count reservation Hcount Hreserve.
  unfold reserve_model in Hreserve.
  destruct (count =? 0) eqn:Hzero.
  - apply Nat.eqb_eq in Hzero. lia.
  - inversion Hreserve; subst. simpl. repeat split.
Qed.

Theorem allocate_reserved_read_roundtrip :
  forall store reservation bytes result,
    allocate_reserved_model store reservation bytes = Some result ->
    read_slot (reserved_store result) (reserved_slot result) = Some bytes.
Proof.
  intros store reservation bytes result Halloc.
  unfold allocate_reserved_model in Halloc.
  destruct (res_next reservation <? res_count reservation) eqn:Hfits;
    try discriminate.
  inversion Halloc; subst.
  simpl.
  apply write_slot_read_same.
Qed.

Theorem allocate_reserved_returns_expected_contiguous_slot :
  forall store reservation bytes result,
    allocate_reserved_model store reservation bytes = Some result ->
    reserved_slot result =
      mkArenaSlot
        (res_arena reservation)
        (res_first reservation + res_next reservation) /\
    res_next (reserved_next result) = S (res_next reservation) /\
    res_count (reserved_next result) = res_count reservation.
Proof.
  intros store reservation bytes result Halloc.
  unfold allocate_reserved_model, current_reserved_slot in Halloc.
  destruct (res_next reservation <? res_count reservation) eqn:Hfits;
    try discriminate.
  inversion Halloc; subst. simpl.
  repeat split.
Qed.

Theorem allocate_reserved_exhausted_rejects :
  forall store reservation bytes,
    res_count reservation <= res_next reservation ->
    allocate_reserved_model store reservation bytes = None.
Proof.
  intros store reservation bytes Hexhausted.
  unfold allocate_reserved_model.
  destruct (res_next reservation <? res_count reservation) eqn:Hfits.
  - apply Nat.ltb_lt in Hfits. lia.
  - reflexivity.
Qed.

Lemma generated_reserved_slots_range :
  forall arena first count slot,
    In slot (generate_reserved_slots arena first count) ->
    arena_id slot = arena /\
    first <= slot_id slot < first + count.
Proof.
  intros arena first count.
  generalize dependent first.
  induction count; intros first slot Hin.
  - simpl in Hin. contradiction.
  - simpl in Hin. destruct Hin as [Heq | Hin].
    + subst. simpl. split; lia.
    + apply IHcount in Hin. simpl in Hin. lia.
Qed.

Theorem reserved_range_same_arena_and_contiguous :
  forall reservation slot,
    In slot
      (generate_reserved_slots
        (res_arena reservation)
        (res_first reservation)
        (res_count reservation)) ->
    arena_id slot = res_arena reservation /\
    res_first reservation <= slot_id slot <
      res_first reservation + res_count reservation.
Proof.
  intros reservation slot Hin.
  apply generated_reserved_slots_range. exact Hin.
Qed.

Theorem completed_reservation_has_no_remaining_slots :
  forall reservation,
    reservation_complete reservation = true ->
    reserved_remaining reservation = 0.
Proof.
  intros reservation Hcomplete.
  unfold reservation_complete, reserved_remaining in *.
  apply Nat.leb_le in Hcomplete.
  lia.
Qed.

(** ** Dirty-slot flush model *)

Definition slot_in (slot : ArenaSlot) (slots : list ArenaSlot) : bool :=
  if in_dec slot_eq_dec slot slots then true else false.

Record DurableState := mkDurableState {
  volatile_store : SlotStore;
  durable_store : SlotStore;
  dirty_slots : list ArenaSlot
}.

Inductive FlushOutcome : Type :=
| FlushOk (state : DurableState)
| FlushFailed (state : DurableState).

Definition durable_after_flush
  (state : DurableState) : SlotStore :=
  fun slot =>
    if slot_in slot (dirty_slots state) then
      volatile_store state slot
    else
      durable_store state slot.

Definition flush_dirty_model
  (write_succeeds : bool) (state : DurableState) : FlushOutcome :=
  if write_succeeds then
    FlushOk (mkDurableState
      (volatile_store state)
      (durable_after_flush state)
      [])
  else
    FlushFailed state.

Theorem failed_flush_preserves_dirty_state :
  forall state,
    flush_dirty_model false state = FlushFailed state.
Proof.
  reflexivity.
Qed.

Theorem successful_flush_clears_dirty_slots :
  forall state flushed,
    flush_dirty_model true state = FlushOk flushed ->
    dirty_slots flushed = [].
Proof.
  intros state flushed Hflush.
  unfold flush_dirty_model in Hflush.
  inversion Hflush; subst. reflexivity.
Qed.

Theorem successful_flush_makes_dirty_slot_durable :
  forall state flushed slot,
    In slot (dirty_slots state) ->
    flush_dirty_model true state = FlushOk flushed ->
    durable_store flushed slot = volatile_store state slot.
Proof.
  intros state flushed slot Hdirty Hflush.
  unfold flush_dirty_model in Hflush.
  inversion Hflush; subst.
  simpl.
  unfold durable_after_flush, slot_in.
  destruct (in_dec slot_eq_dec slot (dirty_slots state)).
  - reflexivity.
  - contradiction.
Qed.

(** ** Directory load/reopen model *)

Definition Directory := list (ArenaSlot * Bytes).

Fixpoint replay_lookup (directory : Directory) (query : ArenaSlot) :
  option Bytes :=
  match directory with
  | [] => None
  | (slot, bytes) :: rest =>
      match replay_lookup rest query with
      | Some found => Some found
      | None =>
          if slot_eq_dec slot query then Some bytes else None
      end
  end.

Fixpoint load_directory
  (directory : Directory) (store : SlotStore) : SlotStore :=
  match directory with
  | [] => store
  | (slot, bytes) :: rest => load_directory rest (write_slot store slot bytes)
  end.

Lemma load_directory_lookup :
  forall directory store query,
    read_slot (load_directory directory store) query =
    match replay_lookup directory query with
    | Some bytes => Some bytes
    | None => read_slot store query
    end.
Proof.
  intros directory.
  induction directory as [|[slot bytes] rest IH]; intros store query.
  - reflexivity.
  - simpl. rewrite IH.
    destruct (replay_lookup rest query) as [found |].
    + reflexivity.
    + destruct (slot_eq_dec slot query) as [Heq | Hneq].
      * subst. apply write_slot_read_same.
      * apply write_slot_read_other. exact Hneq.
Qed.

Definition directory_entry_valid
  (arena_count max_payload : nat) (entry : ArenaSlot * Bytes) : bool :=
  let slot := fst entry in
  let bytes := snd entry in
  (arena_id slot <? arena_count) && (length bytes <=? max_payload).

Definition checked_load_directory
  (arena_count max_payload : nat) (directory : Directory) : option SlotStore :=
  if forallb (directory_entry_valid arena_count max_payload) directory then
    Some (load_directory directory empty_store)
  else
    None.

Theorem checked_load_directory_reconstructs_payload :
  forall arena_count max_payload directory store slot bytes,
    checked_load_directory arena_count max_payload directory = Some store ->
    replay_lookup directory slot = Some bytes ->
    read_slot store slot = Some bytes.
Proof.
  intros arena_count max_payload directory store slot bytes Hload Hlookup.
  unfold checked_load_directory in Hload.
  destruct (forallb (directory_entry_valid arena_count max_payload) directory);
    try discriminate.
  inversion Hload; subst.
  rewrite load_directory_lookup.
  rewrite Hlookup.
  reflexivity.
Qed.

Theorem checked_load_directory_rejects_invalid_head :
  forall arena_count max_payload slot bytes rest,
    directory_entry_valid arena_count max_payload (slot, bytes) = false ->
    checked_load_directory arena_count max_payload ((slot, bytes) :: rest) =
    None.
Proof.
  intros arena_count max_payload slot bytes rest Hinvalid.
  unfold checked_load_directory. simpl.
  rewrite Hinvalid. reflexivity.
Qed.
