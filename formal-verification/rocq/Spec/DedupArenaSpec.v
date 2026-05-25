(** * DedupArenaSpec: persistent arena deduplication soundness

    This specification models the deduplicating arena wrapper used by the
    persistent byte and char ART backends.  The core safety claim is narrow:
    verified cache hits may reuse an arena slot only when the slot still
    stores the requested bytes.  Collisions or stale cache entries must fail
    closed by allocating a fresh slot.  The legacy unverified mode is modeled
    as a trusted hash hit to document the old boundary, and the public
    compatibility setter is modeled as keeping verification enabled.

    Hash quality, xxHash internals, arena locality, and space-saving optimality
    are outside this proof boundary.
*)

From Stdlib Require Import List.
From Stdlib Require Import Arith.
From Stdlib Require Import Lia.
From Stdlib Require Import Bool.
Import ListNotations.

(** ** Basic arena and cache model *)

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

Record CacheEntry := mkCacheEntry {
  cached_bytes : Bytes;
  cached_slot : ArenaSlot
}.

Definition DedupCache := nat -> option CacheEntry.

Definition empty_cache : DedupCache := fun _ => None.

Definition cache_insert
  (cache : DedupCache) (hash : nat) (bytes : Bytes) (slot : ArenaSlot) :
  DedupCache :=
  fun query =>
    if Nat.eq_dec query hash then
      Some (mkCacheEntry bytes slot)
    else
      cache query.

Theorem cache_insert_lookup_same :
  forall cache hash bytes slot,
    cache_insert cache hash bytes slot hash =
    Some (mkCacheEntry bytes slot).
Proof.
  intros cache hash bytes slot.
  unfold cache_insert.
  destruct (Nat.eq_dec hash hash) as [_ | Hneq].
  - reflexivity.
  - exfalso. apply Hneq. reflexivity.
Qed.

Definition bytes_eq_dec :
  forall (left right : Bytes), {left = right} + {left <> right}.
Proof.
  decide equality.
  apply Nat.eq_dec.
Defined.

Definition set_verify_on_hit_model (_requested : bool) : bool := true.

Theorem compatibility_setter_keeps_verified_mode :
  forall requested,
    set_verify_on_hit_model requested = true.
Proof.
  reflexivity.
Qed.

(** ** Deduplicating allocation model *)

Inductive DedupOutcome : Type :=
| DedupHit (slot : ArenaSlot)
| DedupMiss (fresh : ArenaSlot)
| DedupCollision (fresh : ArenaSlot)
| DedupAssumedHit (slot : ArenaSlot).

Record DedupState := mkDedupState {
  dedup_cache : DedupCache;
  dedup_store : SlotStore;
  dedup_hits : nat;
  dedup_misses : nat;
  dedup_collisions : nat
}.

Record DedupResult := mkDedupResult {
  result_state : DedupState;
  result_outcome : DedupOutcome
}.

Definition record_hit (state : DedupState) : DedupState :=
  mkDedupState
    (dedup_cache state)
    (dedup_store state)
    (S (dedup_hits state))
    (dedup_misses state)
    (dedup_collisions state).

Definition allocate_fresh
  (state : DedupState)
  (hash : nat)
  (bytes : Bytes)
  (fresh : ArenaSlot)
  (outcome : DedupOutcome)
  (collision : bool) : DedupResult :=
  mkDedupResult
    (mkDedupState
      (cache_insert (dedup_cache state) hash bytes fresh)
      (write_slot (dedup_store state) fresh bytes)
      (dedup_hits state)
      (S (dedup_misses state))
      (if collision then S (dedup_collisions state)
       else dedup_collisions state))
    outcome.

Definition allocate_dedup_model
  (verify_on_hit : bool)
  (state : DedupState)
  (hash : nat)
  (bytes : Bytes)
  (fresh : ArenaSlot) : DedupResult :=
  match dedup_cache state hash with
  | None => allocate_fresh state hash bytes fresh (DedupMiss fresh) false
  | Some entry =>
      if verify_on_hit then
        match read_slot (dedup_store state) (cached_slot entry) with
        | Some existing =>
            if bytes_eq_dec existing bytes then
              mkDedupResult
                (record_hit state)
                (DedupHit (cached_slot entry))
            else
              allocate_fresh
                state hash bytes fresh (DedupCollision fresh) true
        | None =>
            allocate_fresh
              state hash bytes fresh (DedupCollision fresh) true
        end
      else
        mkDedupResult
          (record_hit state)
          (DedupAssumedHit (cached_slot entry))
  end.

Theorem verified_hit_sound :
  forall state hash bytes fresh slot,
    result_outcome
      (allocate_dedup_model true state hash bytes fresh) = DedupHit slot ->
    exists entry,
      dedup_cache state hash = Some entry /\
      cached_slot entry = slot /\
      read_slot (dedup_store state) slot = Some bytes.
Proof.
  intros state hash bytes fresh slot Halloc.
  unfold allocate_dedup_model in Halloc.
  destruct (dedup_cache state hash) as [entry |] eqn:Hcache;
    try discriminate.
  destruct (read_slot (dedup_store state) (cached_slot entry))
    as [existing |] eqn:Hread; try discriminate.
  destruct (bytes_eq_dec existing bytes) as [Heq | Hneq];
    try discriminate.
  inversion Halloc; subst.
  exists entry. repeat split; try assumption.
Qed.

Theorem verified_miss_allocates_fresh :
  forall state hash bytes fresh,
    dedup_cache state hash = None ->
    result_outcome
      (allocate_dedup_model true state hash bytes fresh) =
      DedupMiss fresh /\
    read_slot
      (dedup_store
        (result_state
          (allocate_dedup_model true state hash bytes fresh)))
      fresh = Some bytes /\
    dedup_cache
      (result_state
        (allocate_dedup_model true state hash bytes fresh))
      hash = Some (mkCacheEntry bytes fresh) /\
    dedup_misses
      (result_state
        (allocate_dedup_model true state hash bytes fresh)) =
      S (dedup_misses state).
Proof.
  intros state hash bytes fresh Hcache.
  unfold allocate_dedup_model.
  rewrite Hcache. simpl.
  repeat split.
  - apply write_slot_read_same.
  - apply cache_insert_lookup_same.
Qed.

Theorem verified_mismatch_allocates_fresh :
  forall state hash bytes fresh entry existing,
    dedup_cache state hash = Some entry ->
    read_slot (dedup_store state) (cached_slot entry) = Some existing ->
    existing <> bytes ->
    result_outcome
      (allocate_dedup_model true state hash bytes fresh) =
      DedupCollision fresh /\
    read_slot
      (dedup_store
        (result_state
          (allocate_dedup_model true state hash bytes fresh)))
      fresh = Some bytes /\
    dedup_cache
      (result_state
        (allocate_dedup_model true state hash bytes fresh))
      hash = Some (mkCacheEntry bytes fresh) /\
    dedup_misses
      (result_state
        (allocate_dedup_model true state hash bytes fresh)) =
      S (dedup_misses state) /\
    dedup_collisions
      (result_state
        (allocate_dedup_model true state hash bytes fresh)) =
      S (dedup_collisions state).
Proof.
  intros state hash bytes fresh entry existing Hcache Hread Hneq.
  unfold allocate_dedup_model.
  rewrite Hcache. rewrite Hread.
  destruct (bytes_eq_dec existing bytes) as [Heq | _].
  - contradiction.
  - simpl. repeat split.
    + apply write_slot_read_same.
    + apply cache_insert_lookup_same.
Qed.

Theorem verified_missing_cached_slot_allocates_fresh :
  forall state hash bytes fresh entry,
    dedup_cache state hash = Some entry ->
    read_slot (dedup_store state) (cached_slot entry) = None ->
    result_outcome
      (allocate_dedup_model true state hash bytes fresh) =
      DedupCollision fresh /\
    read_slot
      (dedup_store
        (result_state
          (allocate_dedup_model true state hash bytes fresh)))
      fresh = Some bytes /\
    dedup_cache
      (result_state
        (allocate_dedup_model true state hash bytes fresh))
      hash = Some (mkCacheEntry bytes fresh).
Proof.
  intros state hash bytes fresh entry Hcache Hread.
  unfold allocate_dedup_model.
  rewrite Hcache. rewrite Hread. simpl.
  repeat split.
  - apply write_slot_read_same.
  - apply cache_insert_lookup_same.
Qed.

Definition collision_free_for
  (state : DedupState) (hash : nat) (bytes : Bytes) : Prop :=
  forall entry,
    dedup_cache state hash = Some entry ->
    cached_bytes entry = bytes /\
    read_slot (dedup_store state) (cached_slot entry) = Some bytes.

Theorem unverified_hit_sound_under_collision_free_assumption :
  forall state hash bytes fresh slot,
    collision_free_for state hash bytes ->
    result_outcome
      (allocate_dedup_model false state hash bytes fresh) =
      DedupAssumedHit slot ->
    exists entry,
      dedup_cache state hash = Some entry /\
      cached_slot entry = slot /\
      cached_bytes entry = bytes /\
      read_slot (dedup_store state) slot = Some bytes.
Proof.
  intros state hash bytes fresh slot Hfree Halloc.
  unfold allocate_dedup_model in Halloc.
  destruct (dedup_cache state hash) as [entry |] eqn:Hcache;
    try discriminate.
  inversion Halloc; subst.
  destruct (Hfree entry Hcache) as [Hbytes Hread].
  exists entry. repeat split; try assumption.
Qed.

(** ** Cache clearing and batch-local caches *)

Definition clear_dedup_state (state : DedupState) : DedupState :=
  mkDedupState empty_cache (dedup_store state) 0 0 0.

Theorem clear_removes_all_cache_entries :
  forall state hash,
    dedup_cache (clear_dedup_state state) hash = None.
Proof.
  reflexivity.
Qed.

Theorem clear_resets_stats :
  forall state,
    dedup_hits (clear_dedup_state state) = 0 /\
    dedup_misses (clear_dedup_state state) = 0 /\
    dedup_collisions (clear_dedup_state state) = 0.
Proof.
  intros state. repeat split.
Qed.

Record BatchState := mkBatchState {
  batch_cache : DedupCache;
  batch_len : nat;
  batch_threshold : nat
}.

Definition batch_insert
  (batch : BatchState) (hash : nat) (bytes : Bytes) (slot : ArenaSlot) :
  BatchState :=
  mkBatchState
    (cache_insert (batch_cache batch) hash bytes slot)
    (S (batch_len batch))
    (batch_threshold batch).

Definition batch_should_merge (batch : BatchState) : bool :=
  batch_threshold batch <=? batch_len batch.

Definition batch_take (batch : BatchState) : DedupCache * BatchState :=
  (batch_cache batch, mkBatchState empty_cache 0 (batch_threshold batch)).

Fixpoint insert_trace
  (cache : DedupCache)
  (entries : list (nat * Bytes * ArenaSlot)) : DedupCache :=
  match entries with
  | [] => cache
  | (hash, bytes, slot) :: rest =>
      insert_trace (cache_insert cache hash bytes slot) rest
  end.

Definition batch_build
  (threshold : nat)
  (entries : list (nat * Bytes * ArenaSlot)) : BatchState :=
  mkBatchState (insert_trace empty_cache entries) (length entries) threshold.

Theorem batch_take_clears_local_cache :
  forall batch hash taken reset,
    batch_take batch = (taken, reset) ->
    batch_cache reset hash = None /\ batch_len reset = 0.
Proof.
  intros batch hash taken reset Htake.
  unfold batch_take in Htake.
  inversion Htake; subst. simpl.
  split; reflexivity.
Qed.

Theorem batch_build_refines_sequential_insert_trace :
  forall threshold entries hash,
    batch_cache (batch_build threshold entries) hash =
    insert_trace empty_cache entries hash.
Proof.
  reflexivity.
Qed.

Theorem batch_should_merge_after_threshold :
  forall batch,
    batch_threshold batch <= batch_len batch ->
    batch_should_merge batch = true.
Proof.
  intros batch Hle.
  unfold batch_should_merge.
  apply Nat.leb_le. exact Hle.
Qed.

(** ** Byte and char managers share this abstract contract *)

Inductive PersistentBackend : Type :=
| ByteBackend
| CharBackend.

Definition backend_allocate
  (_backend : PersistentBackend)
  (verify_on_hit : bool)
  (state : DedupState)
  (hash : nat)
  (bytes : Bytes)
  (fresh : ArenaSlot) : DedupResult :=
  allocate_dedup_model verify_on_hit state hash bytes fresh.

Theorem byte_char_dedup_use_same_model :
  forall verify_on_hit state hash bytes fresh,
    backend_allocate ByteBackend verify_on_hit state hash bytes fresh =
    backend_allocate CharBackend verify_on_hit state hash bytes fresh.
Proof.
  reflexivity.
Qed.
